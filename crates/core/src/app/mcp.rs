//! `gld mcp`：这台机器上装了哪些 MCP server、开哪几个给服务转、试着起一个。
//!
//! 开关存在 `AppSettings::relayed_mcp_servers`。服务每次请求都重新读设置，
//! 所以开关不用重启服务；但客户端（ChatGPT）只在连上时读一次工具表，从"一个
//! 都没开"到"开了一个"时，那三个工具要客户端刷新连接后才看得见。

use serde::{Deserialize, Serialize};

use super::state::App;
use crate::error::{AppError, AppResult};
use crate::machine_mcp::{self, installed, pool};

/// `gld mcp ls` 的一行。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerDto {
    pub name: String,
    /// 从哪个文件读来的（`~/.claude.json` / `~/.codex/config.toml`）。
    pub source: String,
    /// local process | local URL | remote URL
    pub kind: String,
    /// 跑的是什么，不含密钥（见 `installed::Server::target`）。
    pub target: String,
    /// 服务转不转它。
    pub on: bool,
    /// 要人知道的事：缺环境变量、来源里关着、同名被盖掉……
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServersDto {
    pub servers: Vec<McpServerDto>,
    /// 开着、但两份配置里都找不到了的名字。
    pub missing: Vec<String>,
    /// 配置文件本身的问题。
    pub problems: Vec<String>,
    /// 服务的工具集。read-only 时开了也不转。
    pub tool_profile: String,
}

/// `gld mcp test` 的结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpTestDto {
    pub name: String,
    pub kind: String,
    pub target: String,
    pub ok: bool,
    pub startup_ms: u64,
    pub protocol: Option<String>,
    pub server_name: Option<String>,
    pub server_version: Option<String>,
    /// 模型能用的工具。
    pub tools: Vec<String>,
    /// server 有、但来源配置的 `enabled_tools` / `disabled_tools` 不放的。
    pub filtered_tools: Vec<String>,
    pub instructions_bytes: usize,
    pub error: Option<String>,
}

impl App {
    pub fn mcp_servers(&self) -> AppResult<McpServersDto> {
        let settings = self.settings()?;
        Ok(overview(
            &machine_mcp::read_installed(),
            &settings.relayed_mcp_servers,
            &settings.hub.tool_profile,
        ))
    }

    /// 开（`on`）或关一组名字；`all` 且是关时全关。返回改完之后的一览。
    pub fn switch_mcp_servers(
        &self,
        names: Vec<String>,
        on: bool,
        all: bool,
    ) -> AppResult<McpServersDto> {
        let installed = machine_mcp::read_installed();
        self.update_settings(|settings| {
            let list = &mut settings.relayed_mcp_servers;
            if !on && all {
                list.clear();
                return Ok(());
            }
            if names.is_empty() {
                return Err(AppError::Message(
                    "给出要开 / 关的名字（gld mcp ls 看有哪些）".into(),
                ));
            }
            for name in &names {
                if on {
                    if installed.find(name).is_none() {
                        return Err(AppError::Message(not_installed(name, &installed)));
                    }
                    if !list.contains(name) {
                        list.push(name.clone());
                    }
                } else if let Some(at) = list.iter().position(|kept| kept == name) {
                    list.remove(at);
                } else {
                    return Err(AppError::Message(format!(
                        "{name} 本来就没开（开着的：{}）",
                        if list.is_empty() {
                            "无".to_string()
                        } else {
                            list.join("、")
                        }
                    )));
                }
            }
            Ok(())
        })?;
        self.mcp_servers()
    }

    /// 在**这个进程**里起一次（守护进程里跑，用的就是服务起 server 时的
    /// `PATH` 和环境），握手、列工具，然后关掉。
    pub fn test_mcp_server(&self, name: &str) -> AppResult<McpTestDto> {
        let installed = machine_mcp::read_installed();
        let Some(server) = installed.find(name) else {
            return Err(AppError::Message(not_installed(name, &installed)));
        };
        let settings = self.settings()?;
        let mut result = McpTestDto {
            name: server.name.clone(),
            kind: server.kind().as_str().into(),
            target: server.target(),
            ok: false,
            startup_ms: 0,
            protocol: None,
            server_name: None,
            server_version: None,
            tools: Vec::new(),
            filtered_tools: Vec::new(),
            instructions_bytes: 0,
            error: None,
        };
        if let Some(problem) = config_problem(server) {
            result.error = Some(problem);
            return Ok(result);
        }
        let started = std::time::Instant::now();
        match pool::try_once(server, &machine_mcp::launch(&settings)) {
            Ok(live) => {
                result.ok = true;
                result.startup_ms = live.started_in.as_millis() as u64;
                result.protocol = Some(live.client.protocol.clone());
                result.server_name = live.client.server_name.clone();
                result.server_version = live.client.server_version.clone();
                result.instructions_bytes =
                    live.client.instructions.as_ref().map_or(0, String::len);
                for tool in &live.tools {
                    let Some(tool_name) = tool["name"].as_str() else {
                        continue;
                    };
                    if server.allows_tool(tool_name) {
                        result.tools.push(tool_name.into());
                    } else {
                        result.filtered_tools.push(tool_name.into());
                    }
                }
            }
            Err(error) => {
                result.startup_ms = started.elapsed().as_millis() as u64;
                result.error = Some(error.to_string());
            }
        }
        Ok(result)
    }
}

fn overview(installed: &installed::Installed, on: &[String], profile: &str) -> McpServersDto {
    let servers = installed
        .servers
        .iter()
        .map(|server| {
            let mut notes = Vec::new();
            if let Some(problem) = config_problem(server) {
                notes.push(problem);
            }
            if server.off_in_source {
                notes.push(format!(
                    "{} 里把它关了（gld 不看那个开关，只看这里开没开）",
                    server.source.file()
                ));
            }
            if let Some(only) = &server.enabled_tools {
                notes.push(format!("只放这几个工具：{}", only.join(", ")));
            }
            if !server.disabled_tools.is_empty() {
                notes.push(format!("不放：{}", server.disabled_tools.join(", ")));
            }
            // 两边都装了、内容一样的很常见（开发机 15 个同名里 13 个），那不用提；不一样才要说
            // 用的是哪份。
            if let Some(other) = installed
                .shadowed
                .iter()
                .find(|other| other.name == server.name && other.transport != server.transport)
            {
                notes.push(format!(
                    "{} 里还有一份同名但不一样的（{}），用的是 {} 这份",
                    other.source.file(),
                    other.target(),
                    server.source.file()
                ));
            }
            McpServerDto {
                name: server.name.clone(),
                source: server.source.file().into(),
                kind: server.kind().as_str().into(),
                target: server.target(),
                on: on.contains(&server.name),
                notes,
            }
        })
        .collect();
    McpServersDto {
        servers,
        missing: on
            .iter()
            .filter(|name| installed.find(name).is_none())
            .cloned()
            .collect(),
        problems: installed
            .problems
            .iter()
            .map(|problem| match &problem.server {
                Some(name) => format!("{} 里的 {name}：{}", problem.source.file(), problem.message),
                None => format!("{}：{}", problem.source.file(), problem.message),
            })
            .collect(),
        tool_profile: crate::tools::registry::normalize_tool_profile(profile).into(),
    }
}

/// 配置本身决定了起不来的情况，说成人话。
fn config_problem(server: &installed::Server) -> Option<String> {
    if !server.missing_env.is_empty() {
        return Some(format!(
            "配置里用了环境变量 {}，gld 的守护进程里没有（在启动守护进程的那个 shell 里 export，再 gld daemon restart）",
            server.missing_env.join("、")
        ));
    }
    if matches!(server.transport, installed::Transport::Sse { .. }) {
        return Some(
            "用的是老的 HTTP+SSE 传输（type: sse），gld 不支持；换成它的 streamable HTTP 地址（多半以 /mcp 结尾）".into(),
        );
    }
    None
}

fn not_installed(name: &str, installed: &installed::Installed) -> String {
    let close: Vec<&str> = installed
        .servers
        .iter()
        .map(|server| server.name.as_str())
        .filter(|candidate| candidate.eq_ignore_ascii_case(name))
        .collect();
    if let Some(first) = close.first() {
        return format!("没有叫 {name} 的 MCP server；名字区分大小写，是不是 {first}？");
    }
    let all: Vec<&str> = installed
        .servers
        .iter()
        .map(|server| server.name.as_str())
        .collect();
    if all.is_empty() {
        return format!(
            "没有叫 {name} 的 MCP server：~/.claude.json 和 ~/.codex/config.toml 里一个都没找到"
        );
    }
    format!(
        "没有叫 {name} 的 MCP server。装好的有：{}（来自 ~/.claude.json 和 ~/.codex/config.toml）",
        all.join("、")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine_mcp::installed::{Installed, Source};

    fn server(name: &str) -> installed::Server {
        installed::Server {
            name: name.into(),
            source: Source::Claude,
            transport: installed::Transport::Stdio {
                command: "npx".into(),
                args: vec!["-y".into(), format!("@x/{name}")],
                env: Default::default(),
                cwd: None,
            },
            off_in_source: false,
            enabled_tools: None,
            disabled_tools: Vec::new(),
            startup_timeout: None,
            tool_timeout: None,
            missing_env: Vec::new(),
        }
    }

    #[test]
    fn the_overview_says_what_is_on_what_is_gone_and_why_something_cannot_start() {
        let mut keyed = server("github");
        keyed.missing_env = vec!["GITHUB_TOKEN".into()];
        let mut shadow = server("context7");
        shadow.source = Source::Codex;
        let mut differs = server("context7");
        differs.source = Source::Codex;
        differs.transport = installed::Transport::Http {
            url: "https://mcp.context7.com/mcp?key=secret".into(),
            headers: Default::default(),
        };
        let installed = Installed {
            servers: vec![server("context7"), keyed],
            shadowed: vec![shadow],
            problems: Vec::new(),
        };
        let view = overview(
            &installed,
            &["context7".into(), "uninstalled".into()],
            "compact",
        );
        assert!(view.servers[0].on);
        assert_eq!(view.servers[0].target, "npx @x/context7");
        assert!(
            view.servers[0].notes.is_empty(),
            "内容一样的不提：{:?}",
            view.servers[0].notes
        );
        assert!(!view.servers[1].on);
        assert!(view.servers[1].notes[0].contains("GITHUB_TOKEN"));
        assert_eq!(view.missing, ["uninstalled"]);

        let installed = Installed {
            shadowed: vec![differs],
            ..installed
        };
        let view = overview(&installed, &[], "compact");
        let note = &view.servers[0].notes[0];
        assert!(note.contains("https://mcp.context7.com/mcp"), "{note}");
        assert!(!note.contains("secret"), "查询串里的密钥不能出现：{note}");
    }

    #[test]
    fn a_wrong_name_is_answered_with_the_right_one() {
        let installed = Installed {
            servers: vec![server("Context7"), server("deepwiki")],
            ..Installed::default()
        };
        assert!(not_installed("context7", &installed).contains("是不是 Context7"));
        assert!(not_installed("exa", &installed).contains("Context7、deepwiki"));
    }
}

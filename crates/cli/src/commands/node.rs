//! 本机操作员的节点邀请及凭据管理入口。

use std::io::{IsTerminal, Read};
use std::path::Path;

use gld_core::app::HubStatusDto;
use gld_daemon::Request;
use serde_json::Value;

use super::Ctx;
use crate::cli::{AutostartCmd, NodeCmd};
use crate::error::{CliError, CliResult};
use crate::output::Output;

pub async fn run(ctx: &mut Ctx, command: NodeCmd) -> CliResult {
    let (request, note) = match command {
        NodeCmd::Invite { name, hub } => {
            let entry = match hub {
                Some(url) => checked_entry(&url)?,
                None => {
                    let status: HubStatusDto = ctx.backend.call_typed(Request::HubStatus).await?;
                    hub_entry(&status)?
                }
            };
            let mut result: Value = ctx.backend.call_typed(Request::NodeInvite { name }).await?;
            let invitation = result["invitation"].as_str().unwrap_or("").to_string();
            let commands = JoinCommands::new(&entry, &invitation, env!("CARGO_PKG_VERSION"));
            result["hub_url"] = entry.clone().into();
            result["commands"] = serde_json::to_value(&commands).unwrap_or_default();
            if !ctx.out.json_or(&result) {
                ctx.out.line(format!(
                    "Hub 入口：{entry}\n机器名称：{}\n邀请：{invitation}\n有效期：十分钟，只能用一次；用过或过期后留在命令历史里也没用\n",
                    result["name"].as_str().unwrap_or(""),
                ));
                ctx.out.line(format!(
                    "到目标机执行。把示例路径换成要共享的目录，可以写多个 --share；默认只读，允许改文件加 --write <目录>，允许跑命令加 --run <目录>。\n\nmacOS / Linux：\n{}\n{}\n\nWindows PowerShell：\n{}\n{}\n",
                    commands.install_unix, commands.join_unix, commands.install_windows, commands.join_windows,
                ));
                ctx.out.note("已经装了 gld 的机器跳过第一行，第二行开头换成 gld。目标机会显示要共享的项目，确认后才配对。相同名称的新邀请会使旧邀请失效。");
            }
            return Ok(());
        }
        NodeCmd::Register {
            name,
            id,
            credential_stdin,
        } => (
            Request::NodeRegister {
                name,
                id,
                credential: read_credential(credential_stdin)?,
            },
            "已登记专用凭据。把 Hub 公钥原样填进目标机私有配置的 hub_public_key，再运行 gld node run --config；项目默认只读，文件写入须在目标机显式授权。",
        ),
        NodeCmd::Revoke { machine_id } => (
            Request::NodeRevoke { machine_id },
            "已撤销凭据并关闭旧连接，保留机器身份和项目目录。",
        ),
        NodeCmd::RotateCredential {
            machine_id,
            credential_stdin,
        } => (
            Request::NodeRotateCredential {
                machine_id,
                credential: read_credential(credential_stdin)?,
            },
            "新凭据已生效，旧连接已关闭。更新目标机的私有配置后重新运行节点；机器与项目句柄保持。",
        ),
        NodeCmd::Run { .. }
        | NodeCmd::Shares { .. }
        | NodeCmd::Doctor { .. }
        | NodeCmd::Autostart(_) => {
            unreachable!("local node commands are handled before Backend")
        }
    };
    let result: Value = ctx.backend.call_typed(request).await?;
    if !ctx.out.json_or(&result) {
        ctx.out.line(format!(
            "Hub ID：{}\nHub 公钥：{}\n机器 ID：{}\n名称：{}",
            result["hub_id"].as_str().unwrap_or(""),
            result["hub_public_key"].as_str().unwrap_or(""),
            result["machine_id"].as_str().unwrap_or(""),
            result["name"].as_str().unwrap_or("")
        ));
        ctx.out.note(note);
    }
    Ok(())
}

#[derive(Debug, serde::Serialize)]
struct JoinCommands {
    install_unix: String,
    join_unix: String,
    install_windows: String,
    join_windows: String,
}

impl JoinCommands {
    /// 安装版本跟 Hub 走，目标机和 Hub 是同一版。安装脚本放在 Release 里，和包同一个地方、同一个版本。
    fn new(entry: &str, invitation: &str, version: &str) -> Self {
        let release = format!("https://github.com/xwfe/gld/releases/download/v{version}");
        let unix = |text: &str| format!("'{}'", text.replace('\'', "'\\''"));
        let windows = |text: &str| format!("'{}'", text.replace('\'', "''"));
        Self {
            install_unix: format!("curl -fsSL {release}/install.sh | GLD_VERSION=v{version} sh"),
            join_unix: format!(
                "printf %s {} | \"$HOME/.local/bin/gld\" join --hub {} --share /path/to/project --invite-stdin --autostart",
                unix(invitation),
                unix(entry),
            ),
            install_windows: format!(
                "$env:GLD_VERSION='v{version}'; irm {release}/install.ps1 | iex"
            ),
            join_windows: format!(
                "{} | & \"$env:LOCALAPPDATA\\Programs\\gld\\gld.exe\" join --hub {} --share C:\\path\\to\\project --invite-stdin --autostart",
                windows(invitation),
                windows(entry),
            ),
        }
    }
}

fn checked_entry(url: &str) -> Result<String, CliError> {
    gld_core::nodes::config::parse_hub_url(url)
        .map_err(|error| CliError::new(format!("--hub 的地址目标机用不了：{error}")))?;
    Ok(url.trim_end_matches('/').to_string())
}

/// 目标机把这个地址存进配置，之后一直连它。所以必须是固定的，临时地址和 OpenAI 隧道都得在这里拦下：
/// 放过去配对可能还能成，等 Hub 一重启全部节点掉线，而且看不出原因。
fn hub_entry(status: &HubStatusDto) -> Result<String, CliError> {
    const FIXED: &str = "给 Hub 配固定入口：gld share --tunnel cf:<域名>、frp:<配置名> 或 https://<已有反代的地址>；同一内网、VPN 或已有其他入口时，用 --hub <HTTPS 地址> 指定。";
    let config = &status.config;
    if config.tunnel_type == "openai" {
        return Err(CliError::new(format!(
            "这个服务走 OpenAI 隧道，它没有目标机能连的地址。{FIXED}"
        )));
    }
    if config.tunnel_type == "cloudflare" && config.cloudflare_mode != "named" {
        return Err(CliError::new(format!(
            "Cloudflare 临时地址每次重启 Hub 都会变，目标机存的还是旧地址，之后全部掉线。{FIXED}"
        )));
    }
    let entry = status.public_endpoint.trim_end_matches('/');
    let entry = entry.strip_suffix("/mcp").unwrap_or(entry);
    if entry.is_empty() {
        return Err(CliError::new(format!(
            "这个服务还没有公网入口，目标机连不上。{FIXED}"
        )));
    }
    checked_entry(entry)
}

fn read_credential(enabled: bool) -> Result<String, CliError> {
    if !enabled {
        return Err(CliError::new("节点凭据须使用 --credential-stdin。"));
    }
    if std::io::stdin().is_terminal() {
        return Err(CliError::new(
            "--credential-stdin 需要管道或文件输入，不在终端回显凭据。",
        ));
    }
    let mut credential = String::new();
    std::io::stdin()
        .take(259)
        .read_to_string(&mut credential)
        .map_err(|_| CliError::new("无法读取节点凭据。"))?;
    if credential.len() > 258 {
        return Err(CliError::new("节点凭据输入过长。"));
    }
    Ok(credential.trim_end_matches(['\r', '\n']).to_string())
}

pub async fn run_config(path: &Path, service: bool) -> CliResult {
    if service {
        gld_core::nodes::service::detach_console();
        gld_core::nodes::service::log_to(path);
    }
    // 拿不到锁时退出码非 0：前台那个停了之后，服务管理器一分钟内再拉起这个就接上了。
    let storage = gld_core::nodes::local_config::LocalConfig::lock(path).map_err(|error| {
        if service {
            gld_core::nodes::service::report(&error);
        }
        CliError::new(error)
    })?;
    let config = match load_config(&storage) {
        Ok(config) => config,
        // 配置坏了或项目被删，拉起多少次都一样。退出码 0，服务管理器不再反复拉起，原因在日志里。
        Err(error) if service => {
            gld_core::nodes::service::report(&format!("{error}; not starting"));
            return Ok(());
        }
        Err(error) => return Err(CliError::new(error)),
    };
    tokio::task::spawn_blocking(move || {
        let _configuration_lock = storage;
        if service {
            gld_core::nodes::service::run(config)
        } else {
            gld_core::nodes::client::run(config)
        }
    })
    .await
    .map_err(|_| CliError::new("节点进程异常结束。"))?
    .map_err(CliError::new)
}

fn load_config(
    storage: &gld_core::nodes::local_config::LocalConfig,
) -> Result<gld_core::nodes::config::NodeConfig, String> {
    let config = storage.load()?;
    let projects = gld_core::nodes::join::resolve_workspaces(&config.workspaces)?;
    gld_core::nodes::local_config::validate_config_location(
        storage.path(),
        &projects
            .into_iter()
            .map(|project| project.path)
            .collect::<Vec<_>>(),
    )?;
    Ok(config)
}

pub fn autostart(command: &AutostartCmd, out: Output) -> CliResult {
    let find = |config: &Option<std::path::PathBuf>| {
        gld_core::nodes::local_config::find(config.as_deref()).map_err(CliError::new)
    };
    match command {
        AutostartCmd::Enable { config } => {
            let status =
                gld_core::nodes::autostart::enable(&find(config)?).map_err(CliError::new)?;
            print_autostart(&status, out);
        }
        AutostartCmd::Disable { config } => {
            let removed =
                gld_core::nodes::autostart::disable(&find(config)?).map_err(CliError::new)?;
            if !out.json_or(&serde_json::json!({ "removed": removed })) {
                out.line(if removed {
                    "已停止节点并删除自启动配置。节点配置和项目文件没动，要连 Hub 就 gld node run。"
                } else {
                    "这份配置没配过自启动，什么都没动。"
                });
            }
        }
        AutostartCmd::Status { config } => {
            let status =
                gld_core::nodes::autostart::status(&find(config)?).map_err(CliError::new)?;
            print_autostart(&status, out);
        }
    }
    Ok(())
}

pub fn print_autostart(status: &gld_core::nodes::autostart::Status, out: Output) {
    if out.json_or(status) {
        return;
    }
    out.kv(&[
        (
            "自启动",
            if status.installed {
                format!("已配置（{}）", status.manager)
            } else {
                "没配置".to_string()
            },
        ),
        (
            "节点进程",
            if status.running { "在跑" } else { "没跑" }.to_string(),
        ),
        ("服务名", status.label.clone()),
        ("日志", status.log.display().to_string()),
    ]);
    for note in &status.notes {
        out.note(note);
    }
}

pub fn shares(
    path: &Path,
    workspaces: &[String],
    writable_workspaces: &[String],
    runnable_workspaces: &[String],
    out: Output,
) -> CliResult {
    let changed = !workspaces.is_empty();
    let view = if changed {
        gld_core::nodes::sharing::replace(
            path,
            workspaces,
            writable_workspaces,
            runnable_workspaces,
        )
    } else {
        gld_core::nodes::sharing::show(path)
    }
    .map_err(CliError::new)?;
    if !out.json_or(&view) {
        out.kv(&[
            ("Hub", view.hub_url),
            ("Hub ID", view.hub_id),
            ("机器 ID", view.machine_id),
            ("配置", view.config_path.display().to_string()),
        ]);
        let runs_commands = view.workspaces.iter().any(|workspace| workspace.commands);
        let rows: Vec<_> = view
            .workspaces
            .into_iter()
            .map(|workspace| {
                vec![
                    workspace.id,
                    workspace.name.unwrap_or_else(|| "—".into()),
                    workspace.path.unwrap_or_else(|| "—".into()),
                    if workspace.available {
                        "可共享".into()
                    } else {
                        "缺失或未限制读取".into()
                    },
                    if workspace.commands {
                        "文件写入、跑命令".into()
                    } else if workspace.read_only {
                        "只读".into()
                    } else {
                        "文件写入".into()
                    },
                ]
            })
            .collect();
        out.table(&["项目 ID", "名称", "路径", "本机登记", "权限"], &rows);
        if changed {
            out.note("已替换共享名单及权限。重新运行节点并连接 Hub 后生效；管理工具不开放。");
            if runs_commands {
                out.note("能跑命令的项目不是沙箱：命令以当前账号执行，能调用这个 Hub 的人等于拿到这个账号。");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gld_core::settings::HubConfig;

    fn status(tunnel: &str, mode: &str, endpoint: &str) -> HubStatusDto {
        HubStatusDto {
            state: "running".into(),
            detail: String::new(),
            config: HubConfig {
                tunnel_type: tunnel.into(),
                cloudflare_mode: mode.into(),
                ..HubConfig::default()
            },
            local_endpoint: "http://127.0.0.1:1/mcp".into(),
            public_endpoint: endpoint.into(),
            tunnel_label: String::new(),
            tunnel_error: String::new(),
            members: Vec::new(),
        }
    }

    #[test]
    fn a_fixed_entry_is_used_with_its_path_prefix_and_without_mcp() {
        for (tunnel, mode, endpoint, expected) in [
            (
                "none",
                "",
                "https://hub.example.com/mcp",
                "https://hub.example.com",
            ),
            (
                "none",
                "",
                "https://gw.example.com/hub/mcp",
                "https://gw.example.com/hub",
            ),
            (
                "cloudflare",
                "named",
                "https://mcp.example.com/mcp",
                "https://mcp.example.com",
            ),
            (
                "frp",
                "",
                "https://sub.frp.example.com/mcp",
                "https://sub.frp.example.com",
            ),
        ] {
            assert_eq!(
                hub_entry(&status(tunnel, mode, endpoint)).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn entries_a_target_cannot_keep_using_are_refused_with_the_way_out() {
        for (tunnel, mode, endpoint, reason) in [
            (
                "cloudflare",
                "quick",
                "https://a-b.trycloudflare.com/mcp",
                "临时地址",
            ),
            ("openai", "", "", "OpenAI"),
            ("none", "", "", "没有公网入口"),
            ("none", "", "http://hub.example.com/mcp", "用不了"),
        ] {
            let error = hub_entry(&status(tunnel, mode, endpoint))
                .unwrap_err()
                .to_string();
            assert!(error.contains(reason), "{error}");
            if reason != "用不了" {
                assert!(error.contains("--hub"), "{error}");
            }
        }
    }

    #[test]
    fn an_explicit_entry_must_be_usable_by_a_node() {
        assert_eq!(
            checked_entry("https://hub.example.com/hub/").unwrap(),
            "https://hub.example.com/hub"
        );
        assert_eq!(
            checked_entry("http://127.0.0.1:8080").unwrap(),
            "http://127.0.0.1:8080"
        );
        for url in [
            "http://hub.example.com",
            "not a url",
            "https://u:p@hub.example.com",
        ] {
            assert!(checked_entry(url).is_err(), "{url}");
        }
    }

    #[test]
    fn the_printed_commands_quote_values_and_pin_the_version() {
        let commands = JoinCommands::new("https://hub.example.com/it's", "abc.def", "1.2.3");
        assert_eq!(
            commands.install_unix,
            "curl -fsSL https://github.com/xwfe/gld/releases/download/v1.2.3/install.sh | GLD_VERSION=v1.2.3 sh"
        );
        assert!(commands.join_unix.starts_with("printf %s 'abc.def' | "));
        assert!(commands
            .join_unix
            .contains("--hub 'https://hub.example.com/it'\\''s'"));
        assert!(commands.join_unix.contains("--autostart"));
        assert!(commands
            .install_windows
            .contains("$env:GLD_VERSION='v1.2.3'"));
        assert!(commands
            .install_windows
            .contains("/v1.2.3/install.ps1 | iex"));
        assert!(commands.join_windows.starts_with("'abc.def' | & "));
        assert!(commands
            .join_windows
            .contains("--hub 'https://hub.example.com/it''s'"));
    }
}

//! Agent 档案：怎么用命令行调一种 Agent（RFC-0009 4.1）。
//!
//! 档案只描述"拼什么命令、结果从哪儿取、停的时候发什么信号"，不写代码。gld 内置几份，
//! 数据目录里的 `agents.toml` 可以改写或新加：
//!
//! ```toml
//! [agents.codex]
//! command = ["codex", "exec", "--json", "-o", "{result_file}", "-C", "{workdir}", "-"]
//! result = { from = "file" }
//! ```
//!
//! 命令里的占位符：`{workdir}` 是项目根，`{result_file}` 是任务目录里给 Agent 写最后一条消息的文件，
//! `{prompt}` 是提示全文。没有 `{prompt}` 的档案，提示从 stdin 喂进去。
//!
//! 两档放权（项目的 `agent-approval`）：`command` 是"一键批准所有操作"，用这家全放开的参数；`self_command`
//! 是"Agent 自行判断、自行批准"，用这家自己的自动审批模式（分类器或审查模型把关，拦下的它换办法接着干）。
//! 没写 `self_command` 的就是这家没有那一档。

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// 结果从哪儿取。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResultFrom {
    /// `{result_file}`：Agent 自己把最后一条消息写进去（`codex exec -o`）。
    File,
    /// stdout 整个是一个 JSON 对象，取 `field`（`claude -p --output-format json` 的 `result`）。
    Json,
    /// stdout 原样就是最后一条消息（`pi -p`）。
    Stdout,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultSpec {
    pub from: ResultFrom,
    /// `from = "json"` 时取哪个字段。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub field: String,
}

/// 停的时候先发哪个信号，之后照样 TERM、KILL 兜底。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum StopSignal {
    /// Claude Code 收到 SIGINT 才会正常收尾这一轮；SIGTERM 直接退出 143，这一轮什么都不记。
    Int,
    Term,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentProfile {
    #[serde(skip_deserializing, default)]
    pub name: String,
    /// `agent-approval=all`：一键批准所有操作。
    pub command: Vec<String>,
    /// `agent-approval=self`：Agent 自行判断、自行批准。空的就是这家没有这一档，选了会起不来。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub self_command: Vec<String>,
    pub result: ResultSpec,
    /// 名字像密钥、本来会被 gld 去掉的环境变量里，这个 Agent 要哪几个（比如它靠 API key 登录）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_pass: Vec<String>,
    /// 多久没有任何输出算卡住。
    #[serde(default = "default_idle_timeout_secs")]
    pub idle_timeout_secs: u64,
    #[serde(default = "default_stop_signal")]
    pub stop_signal: StopSignal,
    /// 验过的 `--version` 输出（包含就算）。不在里面照样跑，回包标出来。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verified: Vec<String>,
}

fn default_idle_timeout_secs() -> u64 {
    300
}

fn default_stop_signal() -> StopSignal {
    StopSignal::Term
}

impl AgentProfile {
    /// 有没有"自行判断"那一档。
    pub fn supports_self_approval(&self) -> bool {
        !self.self_command.is_empty()
    }

    /// 起这个 Agent 的 argv。`self_approval` 选 `self_command`：没有就是空的，不退回 `command`（那是全放开）。
    /// `prompt_on_stdin` 为真时提示要从 stdin 喂。
    pub fn argv(
        &self,
        self_approval: bool,
        workdir: &Path,
        result_file: &Path,
        prompt: &str,
    ) -> (Vec<String>, bool) {
        let command = if self_approval {
            &self.self_command
        } else {
            &self.command
        };
        let mut prompt_on_stdin = true;
        let argv = command
            .iter()
            .map(|part| {
                if part.contains("{prompt}") {
                    prompt_on_stdin = false;
                }
                part.replace("{workdir}", &workdir.display().to_string())
                    .replace("{result_file}", &result_file.display().to_string())
                    .replace("{prompt}", prompt)
            })
            .collect();
        (argv, prompt_on_stdin)
    }

    fn check(&self) -> Result<(), String> {
        if self
            .command
            .first()
            .is_none_or(|program| program.trim().is_empty())
        {
            return Err(format!("档案 {} 的 command 是空的", self.name));
        }
        if self.supports_self_approval()
            && self
                .self_command
                .first()
                .is_none_or(|program| program.trim().is_empty())
        {
            return Err(format!(
                "档案 {} 的 self_command 第一项得是程序名",
                self.name
            ));
        }
        if self.result.from == ResultFrom::Json && self.result.field.is_empty() {
            return Err(format!(
                "档案 {} 的结果从 JSON 取，要写 field（比如 result = {{ from = \"json\", field = \"result\" }}）",
                self.name
            ));
        }
        for (key, command) in [
            ("command", &self.command),
            ("self_command", &self.self_command),
        ] {
            if self.result.from == ResultFrom::File
                && !command.is_empty()
                && !command.iter().any(|part| part.contains("{result_file}"))
            {
                return Err(format!(
                    "档案 {} 的结果从文件取，{key} 里得有 {{result_file}}，不然 Agent 不知道往哪写",
                    self.name
                ));
            }
        }
        Ok(())
    }
}

/// 跑完才一次性吐结果、中途不出声的 Agent 用：5 分钟没输出就当卡住的规矩对它不成立，交给总时长管。
const SILENT_UNTIL_DONE_SECS: u64 = 2 * 60 * 60;

fn builtin_profile(
    command: &[&str],
    self_command: &[&str],
    result: ResultSpec,
    idle_timeout_secs: u64,
    stop_signal: StopSignal,
) -> AgentProfile {
    AgentProfile {
        name: String::new(),
        command: command.iter().map(|part| part.to_string()).collect(),
        self_command: self_command.iter().map(|part| part.to_string()).collect(),
        result,
        env_pass: Vec::new(),
        idle_timeout_secs,
        stop_signal,
        verified: Vec::new(),
    }
}

fn from(from: ResultFrom, field: &str) -> ResultSpec {
    ResultSpec {
        from,
        field: field.into(),
    }
}

/// 内置档案。`command` 一律用这家"全放开"的参数（用户 2026-10-22 定的：不限制在沙箱、全权可写，做完交回审核，
/// RFC-0009 4.4）；有自动审批模式的另给 `self_command`（2026-10-23 用户要的"由 Agent 自行判断、自行批准"）。
///
/// 参数都在本机 `--help` 或安装包的参数定义里核过：codex 0.163.0（`--approve-for-me`：审查模型自动批或拒，工作区可写沙箱）、
/// claude 2.1.304（`--permission-mode auto` 分类器把关，`--permission-prompts none` 让 `-p` 下没人答的提示直接拒而不是等）、
/// qwen 0.19.12（`--yolo`、`--approval-mode auto` 在顶层帮助里不显示，参数定义里有）、agy 1.1.28、pi 0.78.1、kimi 0.29.1。
/// **真跑过的只有 codex**（审查 §43–§57）；其余没花额度跑过，见 concepts.md 的表。
fn builtin() -> BTreeMap<String, AgentProfile> {
    let codex_head = [
        "codex",
        "exec",
        "--json",
        "-o",
        "{result_file}",
        "-C",
        "{workdir}",
        "--skip-git-repo-check",
    ];
    // stream-json：一边跑一边出事件，不会因为长时间没输出被当成卡住（json 要跑完才一次性吐）；最后一行是
    // `type: result`，`result` 字段就是最后一条消息。`-p` 下 stream-json 要带 `--verbose`。
    let claude_head = [
        "claude",
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
    ];
    let profiles = [
        (
            "codex",
            builtin_profile(
                &[
                    &codex_head[..],
                    &["--dangerously-bypass-approvals-and-sandbox", "-"],
                ]
                .concat(),
                &[&codex_head[..], &["--approve-for-me", "-"]].concat(),
                from(ResultFrom::File, ""),
                default_idle_timeout_secs(),
                StopSignal::Term,
            ),
        ),
        (
            "claude",
            builtin_profile(
                &[&claude_head[..], &["--dangerously-skip-permissions"]].concat(),
                &[
                    &claude_head[..],
                    &["--permission-mode", "auto", "--permission-prompts", "none"],
                ]
                .concat(),
                from(ResultFrom::Json, "result"),
                default_idle_timeout_secs(),
                StopSignal::Int,
            ),
        ),
        (
            "qwen",
            builtin_profile(
                &["qwen", "-p", "{prompt}", "--yolo"],
                &["qwen", "-p", "{prompt}", "--approval-mode", "auto"],
                from(ResultFrom::Stdout, ""),
                default_idle_timeout_secs(),
                StopSignal::Term,
            ),
        ),
        (
            // Antigravity。提示只能放参数（纯文本 stdin 不读）；它自己的 print 模式默认 5 分钟就不等了，放到 2 小时。
            // 1.1.28 没有自动审批模式（`--mode` 只有 accept-edits、plan）。json 跑完才吐，按"中途不出声"算。
            // 已知 bug：打印完结果进程不退（官方 issue #1313），会拖到总时长被停、记成 timeout，结果照样取得到。
            "agy",
            builtin_profile(
                &[
                    "agy",
                    "-p",
                    "{prompt}",
                    "--output-format",
                    "json",
                    "--print-timeout",
                    "120m",
                    "--dangerously-skip-permissions",
                ],
                &[],
                from(ResultFrom::Json, "response"),
                SILENT_UNTIL_DONE_SECS,
                StopSignal::Term,
            ),
        ),
        (
            // Pi 没有审批：本来就全权，也就没有"自行判断"那一档。text 模式只在最后打出最后一条消息。
            "pi",
            builtin_profile(
                &["pi", "-p", "--no-session"],
                &[],
                from(ResultFrom::Stdout, ""),
                SILENT_UNTIL_DONE_SECS,
                StopSignal::Term,
            ),
        ),
        (
            // Kimi 0.29.1 的 `-p` 本身就是全放开，`--yolo` / `--auto` 只能在交互模式用，加了反而报错。
            "kimi",
            builtin_profile(
                &["kimi", "-p", "{prompt}"],
                &[],
                from(ResultFrom::Stdout, ""),
                default_idle_timeout_secs(),
                StopSignal::Term,
            ),
        ),
    ];
    profiles
        .into_iter()
        .map(|(name, mut profile)| {
            profile.name = name.into();
            (name.to_string(), profile)
        })
        .collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentsFile {
    #[serde(default)]
    agents: BTreeMap<String, AgentProfile>,
}

/// 内置档案加上 `agents_toml` 里的（同名的整条换掉）。文件不存在就只有内置的。
///
/// 写坏了不悄悄退回内置：报错带文件路径和哪一行，免得用户以为改的档案生效了。
pub fn load(agents_toml: &Path) -> Result<BTreeMap<String, AgentProfile>, String> {
    let mut profiles = builtin();
    let text = match std::fs::read_to_string(agents_toml) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(profiles),
        Err(error) => return Err(format!("读不了 {}：{error}", agents_toml.display())),
    };
    let file: AgentsFile = toml::from_str(&text)
        .map_err(|error| format!("{} 写得不对：{error}", agents_toml.display()))?;
    for (name, mut profile) in file.agents {
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(format!(
                "{} 里的档案名「{name}」只能用字母、数字、- 和 _",
                agents_toml.display()
            ));
        }
        profile.name = name.clone();
        profile.check()?;
        profiles.insert(name, profile);
    }
    Ok(profiles)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每份内置档案的 `command` 是这家全放开的参数；有自动审批模式的三家给 `self_command`，
    /// 里面不能混进全放开的参数（那就不是"自行判断"了）。
    #[test]
    fn the_builtin_profiles_give_full_access_and_self_approval_where_the_agent_has_it() {
        let profiles = builtin();
        for profile in profiles.values() {
            profile.check().expect("内置档案自己得过检查");
        }
        let full = |name: &str, flag: &str| {
            assert!(
                profiles[name].command.contains(&flag.to_string()),
                "{name} 的 command 要带 {flag}"
            );
        };
        full("codex", "--dangerously-bypass-approvals-and-sandbox");
        full("claude", "--dangerously-skip-permissions");
        full("qwen", "--yolo");
        full("agy", "--dangerously-skip-permissions");

        let own = |name: &str, flags: &[&str]| {
            let command = &profiles[name].self_command;
            for flag in flags {
                assert!(
                    command.contains(&flag.to_string()),
                    "{name} 的 self_command 要带 {flag}"
                );
            }
            for loud in [
                "--dangerously-bypass-approvals-and-sandbox",
                "--dangerously-skip-permissions",
                "--yolo",
            ] {
                assert!(
                    !command.contains(&loud.to_string()),
                    "{name} 的 self_command 混进了 {loud}"
                );
            }
        };
        own("codex", &["--approve-for-me"]);
        own(
            "claude",
            &["--permission-mode", "auto", "--permission-prompts", "none"],
        );
        own("qwen", &["--approval-mode", "auto"]);
        for name in ["agy", "pi", "kimi"] {
            assert!(
                !profiles[name].supports_self_approval(),
                "{name} 没有自行判断那一档"
            );
        }
        assert_eq!(profiles["claude"].stop_signal, StopSignal::Int);
        // json 要跑完才一次性吐，长任务会被"5 分钟没输出"当成卡住停掉；stream-json 边跑边出。
        // `-p` 下 stream-json 不带 --verbose 会直接报错（claude 2.1.304 的二进制里有这句报错）。
        for command in [
            &profiles["claude"].command,
            &profiles["claude"].self_command,
        ] {
            assert!(
                command
                    .windows(2)
                    .any(|w| w == ["--output-format", "stream-json"])
                    && command.contains(&"--verbose".to_string()),
                "{command:?}"
            );
        }
        // 跑完才吐结果的，不能按"5 分钟没输出就是卡住"停掉。
        assert!(profiles["pi"].idle_timeout_secs >= 3600);
        assert!(profiles["agy"].idle_timeout_secs >= 3600);
    }

    #[test]
    fn placeholders_are_filled_and_the_prompt_goes_to_stdin_unless_named() {
        let codex = &builtin()["codex"];
        let (argv, on_stdin) = codex.argv(false, Path::new("/w"), Path::new("/t/result.txt"), "hi");
        assert!(on_stdin);
        assert!(argv.contains(&"/w".to_string()) && argv.contains(&"/t/result.txt".to_string()));
        assert!(argv.contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
        let (argv, _) = codex.argv(true, Path::new("/w"), Path::new("/t/result.txt"), "hi");
        assert!(argv.contains(&"--approve-for-me".to_string()), "{argv:?}");

        let inline = &builtin()["kimi"];
        let (argv, on_stdin) = inline.argv(false, Path::new("/w"), Path::new("/r"), "fix it");
        assert!(!on_stdin);
        assert_eq!(argv, vec!["kimi", "-p", "fix it"]);
        // 没有自行判断那一档：给空的，绝不退回全放开的 command。
        assert!(inline
            .argv(true, Path::new("/w"), Path::new("/r"), "fix it")
            .0
            .is_empty());
    }

    #[test]
    fn agents_toml_adds_and_replaces_profiles() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("agents.toml");
        assert_eq!(load(&path).expect("没有文件").len(), builtin().len());
        std::fs::write(
            &path,
            r#"
[agents.opencode]
command = ["opencode", "run", "--auto"]
result = { from = "stdout" }

[agents.qoder]
command = ["qodercli", "-p", "{prompt}", "--yolo"]
self_command = ["qodercli", "-p", "{prompt}", "--permission-mode", "auto"]
result = { from = "stdout" }

[agents.claude]
command = ["claude", "-p", "--output-format", "json"]
result = { from = "json", field = "result" }
idle_timeout_secs = 60
"#,
        )
        .expect("write");
        let profiles = load(&path).expect("读得进来");
        assert_eq!(profiles["opencode"].result.from, ResultFrom::Stdout);
        assert!(!profiles["opencode"].supports_self_approval());
        assert!(profiles["qoder"].supports_self_approval());
        assert_eq!(profiles["claude"].idle_timeout_secs, 60);
        assert!(!profiles["claude"]
            .command
            .contains(&"--dangerously-skip-permissions".to_string()));
        assert!(
            !profiles["claude"].supports_self_approval(),
            "同名整条换掉，内置的 self_command 不留"
        );
        assert!(profiles.contains_key("codex"));
    }

    #[test]
    fn a_broken_agents_toml_is_reported_instead_of_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("agents.toml");
        for text in [
            "[agents.x]\ncommand = []\nresult = { from = \"stdout\" }\n",
            "[agents.x]\ncommand = [\"x\"]\nresult = { from = \"json\" }\n",
            "[agents.x]\ncommand = [\"x\"]\nresult = { from = \"file\" }\n",
            "[agents.x]\ncommand = [\"x\"]\nresult = { from = \"stdout\" }\nbogus = 1\n",
            "[agents.\"a b\"]\ncommand = [\"x\"]\nresult = { from = \"stdout\" }\n",
            "[agents.x]\ncommand = [\"x\"]\nself_command = [\"\"]\nresult = { from = \"stdout\" }\n",
            "[agents.x]\ncommand = [\"x\", \"{result_file}\"]\nself_command = [\"x\"]\nresult = { from = \"file\" }\n",
        ] {
            std::fs::write(&path, text).expect("write");
            let error = load(&path).expect_err(text);
            assert!(
                error.contains("agents.toml") || error.contains("档案"),
                "{error}"
            );
        }
    }
}

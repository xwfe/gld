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
    pub command: Vec<String>,
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
    /// 起这个 Agent 的 argv。`prompt_on_stdin` 为真时提示要从 stdin 喂。
    pub fn argv(&self, workdir: &Path, result_file: &Path, prompt: &str) -> (Vec<String>, bool) {
        let mut prompt_on_stdin = true;
        let argv = self
            .command
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
        if self.result.from == ResultFrom::Json && self.result.field.is_empty() {
            return Err(format!(
                "档案 {} 的结果从 JSON 取，要写 field（比如 result = {{ from = \"json\", field = \"result\" }}）",
                self.name
            ));
        }
        if self.result.from == ResultFrom::File
            && !self
                .command
                .iter()
                .any(|part| part.contains("{result_file}"))
        {
            return Err(format!(
                "档案 {} 的结果从文件取，command 里得有 {{result_file}}，不然 Agent 不知道往哪写",
                self.name
            ));
        }
        Ok(())
    }
}

/// 内置档案。全都用这个 Agent 自己"全放开"的参数：用户定的是不限制在沙箱、直接全权可写，
/// 做完交回审核（RFC-0009 4.4）。参数名在本机 codex 0.163.0、claude 2.1.304 的 `--help` 里核过。
fn builtin() -> BTreeMap<String, AgentProfile> {
    let profiles = [
        (
            "codex",
            AgentProfile {
                name: String::new(),
                command: [
                    "codex",
                    "exec",
                    "--json",
                    "-o",
                    "{result_file}",
                    "-C",
                    "{workdir}",
                    "--skip-git-repo-check",
                    "--dangerously-bypass-approvals-and-sandbox",
                    "-",
                ]
                .map(String::from)
                .to_vec(),
                result: ResultSpec {
                    from: ResultFrom::File,
                    field: String::new(),
                },
                env_pass: Vec::new(),
                idle_timeout_secs: default_idle_timeout_secs(),
                stop_signal: StopSignal::Term,
                verified: Vec::new(),
            },
        ),
        (
            "claude",
            AgentProfile {
                name: String::new(),
                command: [
                    "claude",
                    "-p",
                    "--output-format",
                    "json",
                    "--dangerously-skip-permissions",
                ]
                .map(String::from)
                .to_vec(),
                result: ResultSpec {
                    from: ResultFrom::Json,
                    field: "result".into(),
                },
                env_pass: Vec::new(),
                idle_timeout_secs: default_idle_timeout_secs(),
                stop_signal: StopSignal::Int,
                verified: Vec::new(),
            },
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

    #[test]
    fn the_builtin_profiles_are_valid_and_run_with_full_access() {
        let profiles = builtin();
        for profile in profiles.values() {
            profile.check().expect("内置档案自己得过检查");
        }
        let codex = &profiles["codex"];
        assert!(codex
            .command
            .contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
        assert!(profiles["claude"]
            .command
            .contains(&"--dangerously-skip-permissions".to_string()));
        assert_eq!(profiles["claude"].stop_signal, StopSignal::Int);
    }

    #[test]
    fn placeholders_are_filled_and_the_prompt_goes_to_stdin_unless_named() {
        let codex = &builtin()["codex"];
        let (argv, on_stdin) = codex.argv(Path::new("/w"), Path::new("/t/result.txt"), "hi");
        assert!(on_stdin);
        assert!(argv.contains(&"/w".to_string()) && argv.contains(&"/t/result.txt".to_string()));

        let inline = AgentProfile {
            command: vec!["kimi".into(), "-p".into(), "{prompt}".into()],
            ..codex.clone()
        };
        let (argv, on_stdin) = inline.argv(Path::new("/w"), Path::new("/r"), "fix it");
        assert!(!on_stdin);
        assert_eq!(argv, vec!["kimi", "-p", "fix it"]);
    }

    #[test]
    fn agents_toml_adds_and_replaces_profiles() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("agents.toml");
        assert_eq!(load(&path).expect("没有文件").len(), 2);
        std::fs::write(
            &path,
            r#"
[agents.pi]
command = ["pi", "-p", "--no-session"]
result = { from = "stdout" }

[agents.claude]
command = ["claude", "-p", "--output-format", "json"]
result = { from = "json", field = "result" }
idle_timeout_secs = 60
"#,
        )
        .expect("write");
        let profiles = load(&path).expect("读得进来");
        assert_eq!(profiles["pi"].result.from, ResultFrom::Stdout);
        assert_eq!(profiles["claude"].idle_timeout_secs, 60);
        assert!(!profiles["claude"]
            .command
            .contains(&"--dangerously-skip-permissions".to_string()));
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

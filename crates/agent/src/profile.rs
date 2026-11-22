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
    /// 默认模型、默认推理强度。空的就是不传，用这家自己配置里的（比如 `~/.codex/config.toml`）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub effort: String,
    /// 怎么把模型、推理强度传给这家：参数模板，`{model}` / `{effort}` 换成取值，加在命令末尾
    /// （命令最后一项是 `-`、从 stdin 读提示的，加在它前面）。空的就是这家不支持指定。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_args: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effort_args: Vec<String>,
    /// 推理强度能取哪些值，起之前先核对，写错了不白花额度。空的就是不在 gld 这边核对，交给这家自己报错。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub efforts: Vec<String>,
}

/// 这一次怎么跑：哪一档放权、什么模型和推理强度（空串就是不传，用这家自己的配置）。
#[derive(Debug, Clone, Copy, Default)]
pub struct Choice<'a> {
    pub self_approval: bool,
    pub model: &'a str,
    pub effort: &'a str,
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
    /// 给了模型、推理强度就按模板加上（调用方先用 [`Self::resolve`] 核对过）。`prompt_on_stdin` 为真时提示要从 stdin 喂。
    pub fn argv(
        &self,
        choice: Choice<'_>,
        workdir: &Path,
        result_file: &Path,
        prompt: &str,
    ) -> (Vec<String>, bool) {
        let command = if choice.self_approval {
            &self.self_command
        } else {
            &self.command
        };
        let mut prompt_on_stdin = true;
        let mut argv: Vec<String> = command
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
        if argv.is_empty() {
            return (argv, prompt_on_stdin);
        }
        let fill = |template: &[String], key: &str, value: &str| -> Vec<String> {
            if value.is_empty() {
                return Vec::new();
            }
            template
                .iter()
                .map(|part| part.replace(key, value))
                .collect()
        };
        let extra: Vec<String> = fill(&self.model_args, "{model}", choice.model)
            .into_iter()
            .chain(fill(&self.effort_args, "{effort}", choice.effort))
            .collect();
        // 最后一项是 `-`（提示从 stdin 读）的，参数要加在它前面：`-` 是位置参数，放后面有的命令行不认。
        let at = if argv.last().is_some_and(|last| last == "-") {
            argv.len() - 1
        } else {
            argv.len()
        };
        argv.splice(at..at, extra);
        (argv, prompt_on_stdin)
    }

    /// AI 提议的（或放行时改的）模型和推理强度，没给就用档案默认；核对这家传不传得了、取值对不对。
    /// 返回实际要传的两个值（空串就是不传，用这家自己的配置）。
    pub fn resolve(
        &self,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<(String, String), String> {
        let pick = |given: Option<&str>, default: &str| -> String {
            given
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(default)
                .to_string()
        };
        let model = pick(model, &self.model);
        let effort = pick(effort, &self.effort);
        // 取值进的是 argv 的一格，不经过 shell；挡的是以 `-` 开头被当成开关，和明显写错的。
        let plain = |value: &str, extra: &str| {
            value.len() <= 100
                && !value.starts_with('-')
                && value
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
        };
        if !model.is_empty() {
            if self.model_args.is_empty() {
                return Err(format!(
                    "{} 的档案没写怎么指定模型（model_args），不能选模型，用它自己配置里的",
                    self.name
                ));
            }
            if !plain(&model, "._:/@+-") {
                return Err(format!(
                    "模型名「{model}」只能用字母、数字和 . _ : / @ + -，不能以 - 开头"
                ));
            }
        }
        if !effort.is_empty() {
            if self.effort_args.is_empty() {
                return Err(format!(
                    "{} 没有推理强度参数，不能指定 effort（去掉它，或者换一家）",
                    self.name
                ));
            }
            if !plain(&effort, "-_") {
                return Err(format!("推理强度「{effort}」只能用字母、数字、- 和 _"));
            }
            if !self.efforts.is_empty() && !self.efforts.contains(&effort) {
                return Err(format!(
                    "{} 的推理强度只能是 {}，给的是「{effort}」",
                    self.name,
                    self.efforts.join("、")
                ));
            }
        }
        Ok((model, effort))
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
        for (key, template, placeholder) in [
            ("model_args", &self.model_args, "{model}"),
            ("effort_args", &self.effort_args, "{effort}"),
        ] {
            if !template.is_empty() && !template.iter().any(|part| part.contains(placeholder)) {
                return Err(format!(
                    "档案 {} 的 {key} 里得有 {placeholder}，不然取值传不进去",
                    self.name
                ));
            }
        }
        self.resolve(None, None)
            .map_err(|error| format!("档案 {} 的默认模型或推理强度不对：{error}", self.name))?;
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
        model: String::new(),
        effort: String::new(),
        model_args: Vec::new(),
        effort_args: Vec::new(),
        efforts: Vec::new(),
    }
}

/// 这家怎么传模型、推理强度，强度能取哪些值（帮助里没列出取值的给空，交给它自己报错）。
fn choices(
    mut profile: AgentProfile,
    model_args: &[&str],
    effort_args: &[&str],
    efforts: &[&str],
) -> AgentProfile {
    let owned = |parts: &[&str]| parts.iter().map(|part| part.to_string()).collect();
    profile.model_args = owned(model_args);
    profile.effort_args = owned(effort_args);
    profile.efforts = owned(efforts);
    profile
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
        .map(|(name, profile)| {
            // 模型、推理强度怎么传，都在本机 --help 里核过：codex `-m`、`-c model_reasoning_effort=`（取值帮助里没列，
            // 不在 gld 这边核对）；claude `--model`、`--effort`（low…max）；agy `--model`、`--effort`（low|medium|high）；
            // pi `--model`、`--thinking`（off…xhigh）；qwen、kimi 只有 `-m`，没有推理强度参数。
            let mut profile = match name {
                "codex" => choices(
                    profile,
                    &["-m", "{model}"],
                    &["-c", "model_reasoning_effort={effort}"],
                    &[],
                ),
                "claude" => choices(
                    profile,
                    &["--model", "{model}"],
                    &["--effort", "{effort}"],
                    &["low", "medium", "high", "xhigh", "max"],
                ),
                "agy" => choices(
                    profile,
                    &["--model", "{model}"],
                    &["--effort", "{effort}"],
                    &["low", "medium", "high"],
                ),
                "pi" => choices(
                    profile,
                    &["--model", "{model}"],
                    &["--thinking", "{effort}"],
                    &["off", "minimal", "low", "medium", "high", "xhigh"],
                ),
                "qwen" | "kimi" => choices(profile, &["-m", "{model}"], &[], &[]),
                _ => profile,
            };
            profile.name = name.into();
            (name.to_string(), profile)
        })
        .collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentsFile {
    #[serde(default)]
    agents: BTreeMap<String, ProfileEntry>,
}

/// `agents.toml` 里的一条。写了 `command` 就是一整份新档案（同名的内置整条换掉）；没写 `command` 就是在同名内置档案上
/// 改几个字段——最常见的是只改默认模型和推理强度，不用把一长串启动参数抄一遍。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileEntry {
    command: Option<Vec<String>>,
    self_command: Option<Vec<String>>,
    result: Option<ResultSpec>,
    env_pass: Option<Vec<String>>,
    idle_timeout_secs: Option<u64>,
    stop_signal: Option<StopSignal>,
    verified: Option<Vec<String>>,
    model: Option<String>,
    effort: Option<String>,
    model_args: Option<Vec<String>>,
    effort_args: Option<Vec<String>>,
    efforts: Option<Vec<String>>,
}

impl ProfileEntry {
    fn into_profile(
        self,
        name: &str,
        builtins: &BTreeMap<String, AgentProfile>,
    ) -> Result<AgentProfile, String> {
        let mut profile = match self.command {
            Some(command) => AgentProfile {
                name: String::new(),
                command,
                self_command: Vec::new(),
                result: self.result.clone().ok_or_else(|| {
                    format!("档案 {name} 写了 command，也要写 result（结果从哪取）")
                })?,
                env_pass: Vec::new(),
                idle_timeout_secs: default_idle_timeout_secs(),
                stop_signal: default_stop_signal(),
                verified: Vec::new(),
                model: String::new(),
                effort: String::new(),
                model_args: Vec::new(),
                effort_args: Vec::new(),
                efforts: Vec::new(),
            },
            None => builtins.get(name).cloned().ok_or_else(|| {
                format!(
                    "档案 {name} 没写 command，又不是内置的（{}），gld 不知道怎么起它",
                    builtins.keys().cloned().collect::<Vec<_>>().join("、")
                )
            })?,
        };
        macro_rules! take {
            ($($field:ident),*) => {
                $(if let Some(value) = self.$field { profile.$field = value; })*
            };
        }
        take!(
            self_command,
            result,
            env_pass,
            idle_timeout_secs,
            stop_signal,
            verified,
            model,
            effort,
            model_args,
            effort_args,
            efforts
        );
        Ok(profile)
    }
}

/// 内置档案加上 `agents_toml` 里的：写了 `command` 的整条换掉同名内置，没写的在同名内置上改。文件不存在就只有内置的。
///
/// 写坏了不悄悄退回内置：报错带文件路径和哪一行，免得用户以为改的档案生效了。
pub fn load(agents_toml: &Path) -> Result<BTreeMap<String, AgentProfile>, String> {
    let builtins = builtin();
    let mut profiles = builtins.clone();
    let text = match std::fs::read_to_string(agents_toml) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(profiles),
        Err(error) => return Err(format!("读不了 {}：{error}", agents_toml.display())),
    };
    let file: AgentsFile = toml::from_str(&text)
        .map_err(|error| format!("{} 写得不对：{error}", agents_toml.display()))?;
    for (name, entry) in file.agents {
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
        let mut profile = entry
            .into_profile(&name, &builtins)
            .map_err(|error| format!("{}：{error}", agents_toml.display()))?;
        profile.name = name.clone();
        profile.check()?;
        profiles.insert(name, profile);
    }
    Ok(profiles)
}

#[cfg(test)]
mod tests;

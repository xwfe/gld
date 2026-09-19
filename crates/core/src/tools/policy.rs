use std::collections::HashSet;
use std::path::{Component, Path};

use serde_json::Value;

use crate::tools::workspace::Workspace;
use crate::workspace::ActionsConfig;

use super::registry::is_allowed_tool;

static NETWORK_COMMAND_PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
static DANGEROUS_COMMAND_PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
static INTERPRETER_MUTATION_PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();

const BASIC_READ_ONLY_COMMANDS: &[&str] = &[
    "pwd", "ls", "dir", "cat", "head", "tail", "grep", "find", "which", "echo",
];

const DEFAULT_ALLOWED_COMMANDS: &[&str] = &[
    "pytest",
    "python",
    "python3",
    "npm",
    "npx",
    "node",
    "pnpm",
    "yarn",
    "make",
    "mvn",
    "mvnw",
    "gradle",
    "gradlew",
    "cargo",
    "go",
    "ruff",
    "mypy",
    "eslint",
    "tsc",
    "msbuild",
    "dotnet",
    "deno",
    "bun",
    "ruby",
    "java",
    "javac",
    "cmake",
    "clang",
    "gcc",
    "g++",
    "git",
    "cmd",
    "powershell",
    "pwsh",
];

#[derive(Debug, Clone)]
pub struct PolicySettings {
    pub allowed_commands: HashSet<String>,
    pub workspace_local_entries: bool,
    pub workspace_script_extensions: HashSet<String>,
    pub max_patch_bytes: usize,
    pub permission_mode: String,
    /// 读工具是否只许读 Workspace 内。默认 true，见 `Workspace::confine_reads`。
    pub confine_reads: bool,
    /// 白名单是怎么来的：`defaults`（没配）、`defaults_plus_configured`
    /// （配了、是追加）、`only`（配了 `only:`，只允许这些）。
    ///
    /// 合并之后光看 `allowed_commands` 是分不出这三种的，而它们对"我该不该
    /// 请用户改配置"的答案完全不同，预检要把这件事说出来（审查 C02）。
    pub allowlist_mode: &'static str,
    /// 这份策略是从哪儿来的。
    pub config_source: &'static str,
}

impl Default for PolicySettings {
    fn default() -> Self {
        Self {
            allowed_commands: default_allowed_command_set(),
            workspace_local_entries: true,
            workspace_script_extensions: default_workspace_script_extension_set(),
            max_patch_bytes: 200_000,
            permission_mode: "trusted".into(),
            confine_reads: true,
            allowlist_mode: "defaults",
            config_source: "built-in defaults",
        }
    }
}

/// 配置字符串对应哪种白名单模式。见 [`merge_default_allowed_commands`]。
fn allowlist_mode(configured: &str) -> &'static str {
    let trimmed = configured.trim();
    if trimmed.starts_with(ONLY_PREFIX) {
        "only"
    } else if trimmed.is_empty() {
        "defaults"
    } else {
        "defaults_plus_configured"
    }
}

impl PolicySettings {
    pub fn from_runtime(runtime: &crate::workspace::RuntimeConfig) -> Self {
        Self {
            allowed_commands: merge_default_allowed_commands(&runtime.allowed_commands),
            workspace_local_entries: runtime.workspace_local_entries,
            workspace_script_extensions: parse_workspace_script_extensions(
                &runtime.workspace_script_extensions,
            ),
            max_patch_bytes: 200_000,
            permission_mode: runtime.permission_mode.clone(),
            confine_reads: runtime.confine_reads,
            allowlist_mode: allowlist_mode(&runtime.allowed_commands),
            config_source: "workspace config (mcp.allowed-commands)",
        }
    }

    pub fn from_actions_config(actions: &ActionsConfig) -> Self {
        Self {
            allowed_commands: merge_default_allowed_commands(&actions.allowed_commands),
            workspace_local_entries: true,
            workspace_script_extensions: default_workspace_script_extension_set(),
            max_patch_bytes: actions.max_patch_bytes as usize,
            permission_mode: actions.permission_mode.clone(),
            confine_reads: actions.confine_reads,
            allowlist_mode: allowlist_mode(&actions.allowed_commands),
            config_source: "actions config (allowed_commands)",
        }
    }

    pub fn network_allowed(&self) -> bool {
        self.permission_mode == "trusted" || self.permission_mode == "dangerous"
    }

    pub fn skip_permission_gates(&self) -> bool {
        self.permission_mode == "dangerous"
    }
}

/// 策略为什么不让这一步过。
///
/// 拆成枚举，而不是让调用方去错误消息里找关键词：消息是写给人看的，改一个
/// 字就能让 `message.contains("allowlisted")` 这种判断悄悄失效，而它背后是
/// "这次到底是哪条规则拒的"——预检和真实执行必须给出同一个答案（审查 F、C02）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyReason {
    MissingCommand,
    CommandTooLong,
    ExternalExecution,
    WorkdirOutsideWorkspace,
    ShellSyntaxRejected,
    ProtectedRepositoryAsset,
    WorkspacePathProtected,
    ConfirmationRequired,
    NetworkBlocked,
    InvalidSyntax,
    CommandNotAllowlisted,
    EnvironmentNotAllowed,
    TimeoutTooLong,
    PatchMissing,
    PatchTooLarge,
    ToolNotExposed,
}

impl PolicyReason {
    /// 报给客户端的错误码。
    pub fn code(self) -> &'static str {
        match self {
            Self::ExternalExecution => "EXTERNAL_EXECUTION_NOT_ALLOWED",
            Self::ProtectedRepositoryAsset => "PROTECTED_REPOSITORY_ASSET",
            Self::WorkspacePathProtected => "WORKSPACE_PATH_PROTECTED",
            Self::ConfirmationRequired => "DANGEROUS_OPERATION_REQUIRES_CONFIRMATION",
            _ => "POLICY_REJECTED",
        }
    }

    /// 机器可读的原因，放在 `details.reason` 和预检结果的 `rule` 里。
    pub fn slug(self) -> &'static str {
        match self {
            Self::MissingCommand => "missing_command",
            Self::CommandTooLong => "command_too_long",
            Self::ExternalExecution => "external_execution",
            Self::WorkdirOutsideWorkspace => "workdir_outside_workspace",
            Self::ShellSyntaxRejected => "shell_syntax_rejected",
            Self::ProtectedRepositoryAsset => "protected_repository_asset",
            Self::WorkspacePathProtected => "workspace_path_protected",
            Self::ConfirmationRequired => "confirmation_required",
            Self::NetworkBlocked => "network_blocked",
            Self::InvalidSyntax => "invalid_command_syntax",
            Self::CommandNotAllowlisted => "command_not_allowlisted",
            Self::EnvironmentNotAllowed => "environment_not_allowed",
            Self::TimeoutTooLong => "timeout_too_long",
            Self::PatchMissing => "patch_missing",
            Self::PatchTooLarge => "patch_too_large",
            Self::ToolNotExposed => "tool_not_exposed",
        }
    }

    /// 下一步该做什么。不建议"换个解释器再试"这种绕过办法。
    pub fn suggestion(self) -> &'static str {
        match self {
            Self::MissingCommand => "cmd 必须是非空字符串",
            Self::CommandTooLong => "命令太长，拆成几条或写成工作区里的脚本",
            Self::ExternalExecution => "把 filesystem_scope 设为 workspace，在当前 Workspace 内执行",
            Self::WorkdirOutsideWorkspace => "workdir 只能是 Workspace 内的相对路径",
            Self::ShellSyntaxRejected => {
                "移除未加引号的 shell 操作符；引号内的程序参数可以保留。需要管道或重定向时，写成工作区里的脚本再执行"
            }
            Self::ProtectedRepositoryAsset => "不要通过子进程删除或清空 .git/.github",
            Self::WorkspacePathProtected => "子进程只能写 Workspace 内的路径",
            Self::ConfirmationRequired => "让用户确认这次危险操作，再带 confirm=true 重试",
            Self::NetworkBlocked => "safe 模式不放行联网命令；需要联网请用户改 permission-mode",
            Self::InvalidSyntax => "命令的引号没有配对，按 shell 词法修好再发",
            Self::CommandNotAllowlisted => {
                "改用已获准的命令或对应的只读工具；确需放开时，请用户把它加进工作区命令白名单"
            }
            Self::EnvironmentNotAllowed => "环境变量只能由服务端配置，不能随调用传入",
            Self::TimeoutTooLong => "timeout_ms 不能超过 10 分钟；长任务请后台跑再轮询",
            Self::PatchMissing => "patch 必须是非空字符串",
            Self::PatchTooLarge => "补丁超过上限，拆成几批提交",
            Self::ToolNotExposed => "这个工具在当前 profile 里没有暴露",
        }
    }

    /// 是不是"等用户点头就能过"，而不是"这条路走不通"。
    pub fn needs_approval(self) -> bool {
        matches!(self, Self::ConfirmationRequired)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct PolicyError {
    pub reason: PolicyReason,
    pub message: String,
}

impl PolicyError {
    pub fn new(reason: PolicyReason, message: impl Into<String>) -> Self {
        Self {
            reason,
            message: message.into(),
        }
    }
}

/// 配成"只允许这些"的前缀。见 [`merge_default_allowed_commands`]。
pub const ONLY_PREFIX: &str = "only:";

pub fn parse_allowed_commands(configured: &str) -> HashSet<String> {
    let trimmed = configured.trim();
    let only = trimmed.starts_with(ONLY_PREFIX);
    let listed = trimmed.strip_prefix(ONLY_PREFIX).unwrap_or(trimmed).trim();
    // 什么都没配 = 没表态，用默认白名单。写了 `only:` 而列表是空的 = 表了态
    // 「只允许我列的这些」，那就一个都不加——**不能**因为列表空了就回到默认
    // 全集，那是把一个想收紧权限的配置放到最大（审查 C03）。基础诊断命令
    // 两种写法下都保留，所以"收紧到极限"也还能看清楚工作区长什么样。
    if listed.is_empty() && !only {
        return default_allowed_command_set();
    }
    let mut commands: HashSet<String> = listed
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    // 基础诊断命令是工作区可用性的最低保障，不应因 Actions 配置遗漏而失效。
    commands.extend(BASIC_READ_ONLY_COMMANDS.iter().map(|s| s.to_string()));
    commands
}

pub fn parse_workspace_script_extensions(configured: &str) -> HashSet<String> {
    let mut extensions = configured
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            if value.starts_with('.') {
                value.to_ascii_lowercase()
            } else {
                format!(".{}", value.to_ascii_lowercase())
            }
        })
        .collect::<HashSet<_>>();
    if extensions.is_empty() {
        extensions = default_workspace_script_extension_set();
    }
    extensions
}

fn default_allowed_command_set() -> HashSet<String> {
    DEFAULT_ALLOWED_COMMANDS
        .iter()
        .map(|s| s.to_string())
        .chain(BASIC_READ_ONLY_COMMANDS.iter().map(|s| s.to_string()))
        .collect()
}

/// 把配置里的命令并进白名单。
///
/// 默认是**追加**：配 `cargo,git` 得到的是"默认白名单 + cargo + git"。
/// 这么设计是因为默认那批（pytest / npm / make …）是跑测试构建的最低配，
/// 每个工作区都重写一遍太啰嗦。
///
/// 但"追加"意味着这个字段**减不掉任何东西**——配成 `cargo,git` 之后
/// `python` `node` `ruby` 照样能跑，而 `python -c "..."` 等于任意代码执行。
/// 有人为了把服务挂公网而特意收窄白名单，结果一点没收窄，还以为收窄了。
///
/// 所以加了 `only:` 前缀表示"只允许这些"：
///
/// ```text
/// mcp.allowed-commands=cargo,git         默认白名单 + cargo + git
/// mcp.allowed-commands=only:cargo,git    只有 cargo、git（外加基础诊断命令）
/// ```
///
/// 基础诊断命令（pwd / ls / cat / grep …）两种写法下都保留：没有它们
/// 连"这个工作区是什么样"都问不出来，而它们本身不能改东西。
fn merge_default_allowed_commands(configured: &str) -> HashSet<String> {
    if configured.trim().starts_with(ONLY_PREFIX) {
        return parse_allowed_commands(configured);
    }
    let mut commands = default_allowed_command_set();
    commands.extend(parse_allowed_commands(configured));
    commands
}

fn default_workspace_script_extension_set() -> HashSet<String> {
    [".exe", ".bat", ".cmd", ".ps1"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

pub fn validate_tool_arguments(
    tool_name: &str,
    arguments: &Value,
    policy: &PolicySettings,
) -> Result<(), PolicyError> {
    validate_tool_arguments_for_workspace(tool_name, arguments, policy, None)
}

pub fn validate_tool_arguments_for_workspace(
    tool_name: &str,
    arguments: &Value,
    policy: &PolicySettings,
    workspace: Option<&Workspace>,
) -> Result<(), PolicyError> {
    match tool_name {
        "exec_command" => validate_command_for_workspace(arguments, policy, workspace),
        "apply_patch" | "patch_check" => validate_patch(arguments, policy),
        _ => Ok(()),
    }
}

/// Actions OpenAPI 暴露层校验：仅限制「能否调用」，不参与执行逻辑。
pub fn validate_actions_exposure(tool_name: &str) -> Result<(), PolicyError> {
    if is_allowed_tool(tool_name) {
        Ok(())
    } else {
        Err(PolicyError::new(
            PolicyReason::ToolNotExposed,
            format!("Tool is not exposed: {tool_name}"),
        ))
    }
}

pub fn validate_command(arguments: &Value, policy: &PolicySettings) -> Result<(), PolicyError> {
    validate_command_for_workspace(arguments, policy, None)
}

pub fn validate_command_for_workspace(
    arguments: &Value,
    policy: &PolicySettings,
    workspace: Option<&Workspace>,
) -> Result<(), PolicyError> {
    let command = arguments
        .get("cmd")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            PolicyError::new(
                PolicyReason::MissingCommand,
                "exec_command requires a non-empty cmd",
            )
        })?;
    if command.trim().is_empty() {
        return Err(PolicyError::new(
            PolicyReason::MissingCommand,
            "exec_command requires a non-empty cmd",
        ));
    }
    if command.len() > 4_000 {
        return Err(PolicyError::new(
            PolicyReason::CommandTooLong,
            "Command is too long",
        ));
    }
    let filesystem_scope = arguments
        .get("filesystem_scope")
        .and_then(Value::as_str)
        .unwrap_or("workspace");
    if filesystem_scope != "workspace" {
        return Err(PolicyError::new(
            PolicyReason::ExternalExecution,
            "exec_command 只允许在 Workspace 内执行",
        ));
    }
    for key in ["workdir", "cwd"] {
        if let Some(workdir) = arguments.get(key).and_then(Value::as_str) {
            let path = Path::new(workdir);
            if path.is_absolute() || path.components().any(|part| part == Component::ParentDir) {
                return Err(PolicyError::new(
                    PolicyReason::WorkdirOutsideWorkspace,
                    "workdir must stay inside the configured workspace",
                ));
            }
        }
    }
    if has_forbidden_shell_syntax(command) {
        return Err(PolicyError::new(
            PolicyReason::ShellSyntaxRejected,
            "Shell chaining, redirection and expansion are not allowed",
        ));
    }
    if (dangerous_command_pattern().is_match(command)
        || interpreter_mutation_pattern().is_match(command))
        && command_targets_protected_repository_asset(command)
    {
        return Err(PolicyError::new(
            PolicyReason::ProtectedRepositoryAsset,
            "禁止删除或递归清空 .git/.github",
        ));
    }
    if interpreter_mutation_pattern().is_match(command) && command_contains_external_path(command) {
        return Err(PolicyError::new(
            PolicyReason::WorkspacePathProtected,
            "workspace scope 禁止通过子进程写入 Workspace 外部路径",
        ));
    }
    if dangerous_command_pattern().is_match(command)
        && !arguments
            .get("confirm")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        return Err(PolicyError::new(
            PolicyReason::ConfirmationRequired,
            "dangerous command requires confirm=true",
        ));
    }
    if !policy.skip_permission_gates()
        && network_command_pattern().is_match(command)
        && !policy.network_allowed()
    {
        return Err(PolicyError::new(
            PolicyReason::NetworkBlocked,
            "Network-looking commands are blocked in safe permission mode",
        ));
    }

    let parts = shell_words::split(command)
        .map_err(|_| PolicyError::new(PolicyReason::InvalidSyntax, "Invalid command syntax"))?;
    if parts.is_empty() {
        return Err(PolicyError::new(
            PolicyReason::MissingCommand,
            "Empty command",
        ));
    }

    let executable = parts[0].trim_start_matches("./");
    let base_name = executable.rsplit(['/', '\\']).next().unwrap_or(executable);
    let stem = base_name
        .strip_suffix(".exe")
        .or_else(|| base_name.strip_suffix(".cmd"))
        .or_else(|| base_name.strip_suffix(".bat"))
        .unwrap_or(base_name);

    let workspace_entry_candidate = workspace_local_entry_exists(workspace, arguments, executable)
        || executable.contains(['/', '\\'])
        || policy
            .workspace_script_extensions
            .iter()
            .any(|extension| base_name.to_ascii_lowercase().ends_with(extension));
    if !(policy.allowed_commands.contains(stem)
        || (policy.workspace_local_entries && workspace_entry_candidate))
    {
        return Err(PolicyError::new(
            PolicyReason::CommandNotAllowlisted,
            format!("Command is not allowlisted: {stem}"),
        ));
    }

    if arguments.get("env").is_some() {
        return Err(PolicyError::new(
            PolicyReason::EnvironmentNotAllowed,
            "Environment variables cannot be supplied by GPT",
        ));
    }

    if let Some(timeout_ms) = arguments.get("timeout_ms").and_then(Value::as_u64) {
        if timeout_ms > 600_000 {
            return Err(PolicyError::new(
                PolicyReason::TimeoutTooLong,
                "Command timeout exceeds 10 minutes",
            ));
        }
    }

    Ok(())
}

fn workspace_local_entry_exists(
    workspace: Option<&Workspace>,
    arguments: &Value,
    executable: &str,
) -> bool {
    let Some(workspace) = workspace else {
        return false;
    };
    let workdir = arguments
        .get("workdir")
        .or_else(|| arguments.get("cwd"))
        .and_then(Value::as_str)
        .unwrap_or(".");
    let Ok(base) = workspace.resolve_existing(workdir) else {
        return false;
    };
    let candidate = if Path::new(executable).is_absolute() {
        Path::new(executable).to_path_buf()
    } else {
        base.path.join(executable)
    };
    candidate
        .canonicalize()
        .map(|path| path.is_file() && path.starts_with(workspace.root()))
        .unwrap_or(false)
}

pub fn validate_patch(arguments: &Value, policy: &PolicySettings) -> Result<(), PolicyError> {
    // 只改 notebook 的 cell 时没有补丁正文，那不是"参数漏了"。
    let notebook_only = arguments
        .get("notebook_edits")
        .and_then(Value::as_array)
        .is_some_and(|edits| !edits.is_empty());
    let missing = || {
        PolicyError::new(
            PolicyReason::PatchMissing,
            "apply_patch requires a patch (or notebook_edits)",
        )
    };
    let patch = match arguments.get("patch").and_then(Value::as_str) {
        Some(patch) if !patch.trim().is_empty() => patch,
        _ if notebook_only => return Ok(()),
        _ => return Err(missing()),
    };

    if patch.len() > policy.max_patch_bytes {
        return Err(PolicyError::new(
            PolicyReason::PatchTooLarge,
            "Patch is too large",
        ));
    }

    Ok(())
}

fn has_forbidden_shell_syntax(command: &str) -> bool {
    if command.contains(['\r', '\n']) {
        return true;
    }

    let chars: Vec<char> = command.chars().collect();
    let mut quote = None;
    let mut escaped = false;
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if escaped {
            escaped = false;
            index += 1;
            continue;
        }

        match quote {
            Some('\'') => {
                if ch == '\'' {
                    quote = None;
                }
            }
            Some('"') => {
                if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    quote = None;
                }
            }
            Some(_) => {}
            None => {
                if ch == '\\' {
                    escaped = true;
                } else if ch == '\'' || ch == '"' {
                    quote = Some(ch);
                } else if matches!(ch, ';' | '&' | '|' | '>' | '<' | '`')
                    || (ch == '$'
                        && chars
                            .get(index + 1)
                            .is_some_and(|next| *next == '(' || *next == '{'))
                {
                    return true;
                }
            }
        }
        index += 1;
    }
    false
}

fn network_command_pattern() -> &'static regex::Regex {
    NETWORK_COMMAND_PATTERN.get_or_init(|| {
        regex::Regex::new(
            r"(?i)(https?://|urllib\.request|requests\.|http\.client|\bcurl\b|\bwget\b|\bssh\b|\bscp\b|\bftp\b)",
        )
        .expect("valid regex")
    })
}

fn dangerous_command_pattern() -> &'static regex::Regex {
    DANGEROUS_COMMAND_PATTERN.get_or_init(|| {
        regex::Regex::new(
            r"(?i)(git\s+reset\s+--hard|git\s+clean\s+-[^\r\n]*f|git\s+checkout\s+--\s+\.|(^|\s)rm\s+(-[^\r\n]*r[^\r\n]*f|--recursive)|remove-item\s+[^\r\n]*-recurse|(^|\s)(rmdir|del)\s+/s\b)",
        )
        .expect("valid regex")
    })
}

fn interpreter_mutation_pattern() -> &'static regex::Regex {
    INTERPRETER_MUTATION_PATTERN.get_or_init(|| {
        regex::Regex::new(
            r#"(?i)(shutil\.(rmtree|move)|os\.(remove|unlink|rmdir)|pathlib\.[^\s;]+\.(unlink|rename)|write_text|write_bytes|fs\.(writefile|writefilesync|unlink|rm)|set-content|out-file|new-item|files?\.(write|delete)|open\([^)]*['\"]w)"#,
        )
        .expect("valid regex")
    })
}

fn command_contains_external_path(command: &str) -> bool {
    let normalized = command.replace('\\', "/");
    normalized.contains("../")
        || normalized.contains("..\\")
        || regex::Regex::new(r#"(?i)(^|["'\s])/[^"]"#)
            .expect("valid regex")
            .is_match(&normalized)
        || regex::Regex::new(r"(?i)\b[A-Z]:/")
            .expect("valid regex")
            .is_match(&normalized)
}

fn command_targets_protected_repository_asset(command: &str) -> bool {
    let normalized_command = command.to_ascii_lowercase().replace('\\', "/");
    let references_protected_asset =
        normalized_command.contains(".git") || normalized_command.contains(".github");
    if !references_protected_asset {
        return false;
    }

    let mutating_operation = [
        "rm ",
        "remove-item",
        "rmdir",
        "del ",
        "unlink",
        "rmtree",
        "write_text",
        "writefile",
        "rename",
        "move",
        "checkout",
        "clean ",
    ]
    .iter()
    .any(|needle| normalized_command.contains(needle));
    if mutating_operation {
        return true;
    }

    command.split_whitespace().any(|part| {
        let token = part
            .trim_matches(|ch: char| matches!(ch, '\'' | '"' | '`' | ',' | ';'))
            .replace('\\', "/");
        let token = token.strip_prefix("./").unwrap_or(&token);
        token == ".git"
            || token.starts_with(".git/")
            || token == ".github"
            || token.starts_with(".github/")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 不带前缀是**追加**，默认那批仍然在。
    ///
    /// 这个测试以前叫 `..._override_defaults`，名字和断言正好相反——
    /// 它断言的是 pytest 仍然可用，也就是没有 override。
    #[test]
    fn workspace_allowed_commands_are_added_on_top_of_the_defaults() {
        let actions = ActionsConfig {
            allowed_commands: "cargo,go".into(),
            ..ActionsConfig::default()
        };
        let policy = PolicySettings::from_actions_config(&actions);
        assert!(policy.allowed_commands.contains("cargo"));
        assert!(policy.allowed_commands.contains("pytest"));
    }

    /// `only:` 前缀才是真的收窄。
    ///
    /// 没有它的话这个字段减不掉任何东西：配成 cargo,git 之后 python 照样能跑，
    /// 而 `python -c "..."` 就是任意代码执行。为了把服务挂公网而特意收窄白名单
    /// 的人会以为自己收窄了，其实一点没收。
    #[test]
    fn the_only_prefix_actually_restricts() {
        let policy = PolicySettings::from_actions_config(&ActionsConfig {
            allowed_commands: "only:cargo,git".into(),
            ..ActionsConfig::default()
        });

        assert!(policy.allowed_commands.contains("cargo"));
        assert!(policy.allowed_commands.contains("git"));
        for removed in ["python", "python3", "node", "ruby", "powershell"] {
            assert!(
                !policy.allowed_commands.contains(removed),
                "only: 之后不该还有 {removed}"
            );
        }
        // 基础诊断命令保留：没有它们连工作区长什么样都问不出来。
        for kept in ["pwd", "ls", "cat", "grep"] {
            assert!(policy.allowed_commands.contains(kept), "{kept} 该保留");
        }

        // 真的落到执行校验上，不只是集合里少了几个名字。
        assert!(validate_command(&json!({ "cmd": "cargo test" }), &policy).is_ok());
        let denied = validate_command(&json!({ "cmd": "python3 -c \"print(1)\"" }), &policy)
            .expect_err("python 该被拒");
        assert_eq!(
            denied.reason,
            PolicyReason::CommandNotAllowlisted,
            "{denied}"
        );
    }

    /// `only:` 后面写空 = 只剩基础诊断命令，**不是**退回默认白名单。
    ///
    /// 原来是退回默认全集，理由写的是"别让一个手滑的 only: 把工作区变成
    /// 什么都不能跑"。但基础诊断命令（pwd / ls / cat / grep）本来就一直
    /// 保留，所以那个代价并不成立；而一个想收紧权限的配置反而把权限放到
    /// 最大，是权限开关最不该有的方向（审查 C03）。
    #[test]
    fn an_empty_only_list_keeps_only_the_basics() {
        let policy = PolicySettings::from_actions_config(&ActionsConfig {
            allowed_commands: "only:".into(),
            ..ActionsConfig::default()
        });

        for removed in ["cargo", "pytest", "python3", "node"] {
            assert!(
                !policy.allowed_commands.contains(removed),
                "only: 之后不该还有 {removed}"
            );
        }
        for kept in ["pwd", "ls", "cat", "grep"] {
            assert!(policy.allowed_commands.contains(kept), "{kept} 该保留");
        }
        let denied =
            validate_command(&json!({ "cmd": "cargo test" }), &policy).expect_err("cargo 该被拒");
        assert_eq!(
            denied.reason,
            PolicyReason::CommandNotAllowlisted,
            "{denied}"
        );
        assert!(validate_command(&json!({ "cmd": "pwd" }), &policy).is_ok());
    }

    /// 完全没配（空字符串）仍然是默认白名单：那是"没说"，不是"只允许这些"。
    #[test]
    fn no_configuration_at_all_still_means_the_defaults() {
        let policy = PolicySettings::from_actions_config(&ActionsConfig {
            allowed_commands: String::new(),
            ..ActionsConfig::default()
        });
        assert!(policy.allowed_commands.contains("cargo"));
        assert!(policy.allowed_commands.contains("pytest"));
    }

    #[test]
    fn trusted_mode_accepts_any_configured_workspace_script_extension() {
        let policy = PolicySettings {
            workspace_local_entries: true,
            workspace_script_extensions: parse_workspace_script_extensions(".cmd,.launcher"),
            ..PolicySettings::default()
        };
        assert!(
            validate_command(&serde_json::json!({ "cmd": "anything.launcher" }), &policy).is_ok()
        );
        assert!(validate_command(
            &serde_json::json!({ "cmd": "scripts/another-name.cmd" }),
            &policy
        )
        .is_ok());
    }

    #[test]
    fn trusted_mode_accepts_an_extensionless_workspace_entry() {
        let dir = tempfile::tempdir().expect("workspace");
        std::fs::write(dir.path().join("project-entry"), "#!/bin/sh\necho ok\n").expect("entry");
        let workspace = Workspace::new(dir.path().to_path_buf()).expect("workspace");
        assert!(validate_command_for_workspace(
            &serde_json::json!({ "cmd": "project-entry", "workdir": "." }),
            &PolicySettings::default(),
            Some(&workspace),
        )
        .is_ok());
    }

    #[test]
    fn patch_size_uses_workspace_limit() {
        let actions = ActionsConfig {
            max_patch_bytes: 10,
            ..ActionsConfig::default()
        };
        let policy = PolicySettings::from_actions_config(&actions);
        let err = validate_patch(&json!({ "patch": "01234567890" }), &policy).unwrap_err();
        assert_eq!(err.reason, PolicyReason::PatchTooLarge);
    }

    #[test]
    fn basic_diagnostic_commands_are_allowed() {
        let policy = PolicySettings::default();
        for command in BASIC_READ_ONLY_COMMANDS {
            validate_command(&json!({"cmd": command}), &policy)
                .unwrap_or_else(|err| panic!("{command} should be allowed: {err}"));
        }
    }

    #[test]
    fn configured_commands_keep_basic_diagnostics() {
        let actions = ActionsConfig {
            allowed_commands: "cargo,go".into(),
            ..ActionsConfig::default()
        };
        let policy = PolicySettings::from_actions_config(&actions);
        assert!(validate_command(&json!({"cmd": "pwd"}), &policy).is_ok());
        assert!(validate_command(&json!({"cmd": "pytest"}), &policy).is_ok());
    }

    #[test]
    fn quoted_python_code_is_not_treated_as_shell_chaining() {
        let policy = PolicySettings::default();
        assert!(validate_command(
            &json!({"cmd": "python -c \"import os; print(os.getcwd())\""}),
            &policy
        )
        .is_ok());
        assert!(validate_command(
            &json!({"cmd": "python -c \"print(1)\" && echo nope"}),
            &policy
        )
        .is_err());
        assert!(validate_command(&json!({"cmd": "echo hello > output.txt"}), &policy).is_err());
    }
}

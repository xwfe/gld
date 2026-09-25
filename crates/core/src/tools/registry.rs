use serde_json::{json, Value};

pub const TOOL_API_VERSION: &str = "2";

pub const P0_TOOLS: &[(&str, &str, &str, bool, bool, bool)] = &[
    (
        "harness_status",
        "Harness status",
        "Return durable task, workspace, capability, and recovery status.",
        true,
        false,
        false,
    ),
    (
        "operation_log",
        "Operation log",
        "Return Workspace-level operation history independent of Task state.",
        true,
        false,
        false,
    ),
    (
        "server_info",
        "Server info",
        "Return server, workspace, auth, profile, and exposed-tool metadata.",
        true,
        false,
        false,
    ),
    (
        "history_manage",
        "History manager",
        "Stable Tool API v2 entry point for history bootstrap, checkpoint, validation, search, and bounded reads.",
        false,
        false,
        false,
    ),
    (
        "planning_manage",
        "Planning manager",
        "Stable Tool API v2 entry point for Goal and Plan state, lifecycle updates, and review requests.",
        false,
        false,
        false,
    ),
    (
        "task_manage",
        "Task manager",
        "Stable Tool API v2 entry point for durable task state, lifecycle, events, and change summaries.",
        false,
        false,
        false,
    ),
    (
        "history_session_bootstrap",
        "Initialize or restore development session",
        "At the start of every new ChatGPT conversation, call this exactly once before the first response and pass the user's verbatim initial_user_input. It creates or resumes a lossless archive, then returns bounded current state and search/read guidance rather than all history.",
        false,
        false,
        false,
    ),
    (
        "history_session_checkpoint",
        "Save development checkpoint",
        "Append an idempotent, redacted development checkpoint. Pass session_key and expected_path exactly as returned by history_session_bootstrap, plus the user's verbatim raw_user_input; the server cannot read ChatGPT transcripts that were not passed as arguments. Changed content for the same turn_id is preserved as a revision.",
        false,
        false,
        false,
    ),
    (
        "history_session_validate",
        "Validate session archive",
        "Validate history numbering, files, session mappings, and optionally rebuild the derived index without deleting history.",
        false,
        false,
        false,
    ),
    (
        "history_session_search",
        "Search session archive",
        "Search lossless history archives by deterministic keywords and return a bounded page of ranked locations and snippets. Use history_session_read to retrieve exact source text.",
        true,
        false,
        false,
    ),
    (
        "history_session_read",
        "Read session archive",
        "Read one lossless numeric Markdown archive by number or a path returned from history_session_search. Responses are UTF-8-safe pages: max_bytes defaults to 16 KiB and is capped at 64 KiB; follow next_cursor to recover the complete source.",
        true,
        false,
        false,
    ),
    (
        "planning_state",
        "Planning state",
        "Return project-local Goal and Plan state stored inside the configured workspace.",
        true,
        false,
        false,
    ),
    (
        "capability_health_check",
        "Capability health check",
        "Check MCP authentication, workspace access, and exposed tool capability state to distinguish capability mismatch from permission problems.",
        true,
        false,
        false,
    ),
    (
        "create_goal",
        "Create goal",
        "AI conversation workflow: create and focus a durable project Goal when the user's request benefits from an explicit objective, success criteria, and constraints. No pre-approval is required.",
        false,
        false,
        false,
    ),
    (
        "update_goal",
        "Update goal",
        "Update Goal status, focus, constraints, or completed success criteria.",
        false,
        false,
        false,
    ),
    (
        "create_plan",
        "Create plan",
        "AI conversation workflow: create, activate, and focus a durable project Plan, optionally linked to a Goal. No pre-approval is required.",
        false,
        false,
        false,
    ),
    (
        "update_plan",
        "Update plan",
        "Update Plan status, focus, and individual step progress.",
        false,
        false,
        false,
    ),
    (
        "request_goal_review",
        "Request goal review",
        "Submit a completed Goal to the gld CLI for human acceptance. This does not archive the Goal; only the human operator can accept and archive it.",
        false,
        false,
        false,
    ),
    (
        "request_plan_review",
        "Request plan review",
        "Submit a completed Plan to the gld CLI for human acceptance. This does not archive the Plan; only the human operator can accept and archive it.",
        false,
        false,
        false,
    ),
    (
        "project_state",
        "Project state",
        "Return the current project, task, change, and verification state.",
        true,
        false,
        false,
    ),
    (
        "start_task",
        "Start task",
        "Start a durable coding task and capture the workspace baseline.",
        false,
        false,
        false,
    ),
    (
        "update_task",
        "Update task",
        "Update task steps and durable progress.",
        false,
        false,
        false,
    ),
    (
        "pause_task",
        "Pause task",
        "Pause the active coding task.",
        false,
        false,
        false,
    ),
    (
        "resume_task",
        "Resume task",
        "Resume a paused or failed coding task.",
        false,
        false,
        false,
    ),
    (
        "finish_task",
        "Finish task",
        "Finish a task. With evidence_session_ids (exec_command sessions from this task that exited 0 on the current files) it becomes completed; without them it waits in verifying; allow_unverified=true closes it as completed_unverified.",
        false,
        false,
        false,
    ),
    (
        "refresh_baseline",
        "Refresh baseline",
        "After FILE_CHANGED_EXTERNALLY or BASELINE_STALE: list the files that changed since the task last recorded the workspace. Call again with accept_fingerprint and reason to adopt the current workspace as the new baseline.",
        false,
        false,
        false,
    ),
    (
        "task_context",
        "Task context",
        "Return a bounded durable task context for a new conversation.",
        true,
        false,
        false,
    ),
    (
        "list_task_events",
        "List task events",
        "Read task event history with pagination.",
        true,
        false,
        false,
    ),
    (
        "change_summary",
        "Change summary",
        "Explain what changed, why, and what evidence exists.",
        true,
        false,
        false,
    ),
    (
        "check_exec_environment",
        "Check exec environment",
        "Return lightweight exec_command sandbox and environment status known to the server.",
        true,
        false,
        false,
    ),
    (
        "check_command",
        "Check command",
        "Ask whether exec_command would accept this command, without running it: which rule decides, whether the program resolves, and what is allowed instead.",
        true,
        false,
        false,
    ),
    (
        "exec_health_check",
        "Exec health check",
        "Verify the exec worker, session creation, command execution, and stdout/stderr capture.",
        true,
        false,
        false,
    ),
    (
        "get_default_cwd",
        "Get default cwd",
        "Return the current default cwd inside the workspace.",
        true,
        false,
        false,
    ),
    (
        "set_default_cwd",
        "Set default cwd",
        "Set the default cwd for relative tool paths inside the workspace.",
        true,
        false,
        false,
    ),
    (
        "read_notebook",
        "Read notebook",
        "Read a Jupyter notebook as cells: source, outputs and cell ids, with paging. read_file still returns the raw JSON.",
        true,
        false,
        false,
    ),
    (
        "read_file",
        "Read file",
        "Read a UTF-8 text file slice inside the configured workspace.",
        true,
        false,
        false,
    ),
    (
        "list_dir",
        "List directory",
        "List directory entries inside the configured workspace.",
        true,
        false,
        false,
    ),
    (
        "list_files",
        "List files",
        "List workspace files using glob filters.",
        true,
        false,
        false,
    ),
    (
        "search_text",
        "Search text",
        "Search UTF-8 workspace files for text or regex matches.",
        true,
        false,
        false,
    ),
    (
        "grep_text",
        "Grep workspace text",
        "Search workspace text with grep-style regex, glob, context, and bounded results.",
        true,
        false,
        false,
    ),
    (
        "apply_patch",
        "Apply patch",
        "Apply a patch envelope transactionally inside the workspace. On success, warnings lists commands still running here, whose results may describe the code as it was before this patch.",
        false,
        true,
        false,
    ),
    (
        "patch_check",
        "Check patch",
        "Validate a patch without changing the workspace.",
        true,
        false,
        false,
    ),
    (
        "exec_command",
        "Execute command",
        "Run a bounded command in the workspace under runtime policy. Every command takes the workspace write lock before it starts, so it never sees a half-written tree; the lock is held until this call returns (up to yield_time_ms) and released once the command moves to the background, so apply_patch from another session gets WORKSPACE_BUSY only while this call is waiting. A background command is not protected after that: its workspace_writes_since_start says how many times the workspace was written since it started, and anything above 0 means its result may describe older code. The session it returns belongs to this connection: another client cannot read or kill it.",
        false,
        true,
        true,
    ),
    (
        "write_stdin",
        "Write stdin",
        "Write characters to a server-managed running command session. Only sessions started over this connection exist here; any other session_id reports SESSION_NOT_FOUND.",
        false,
        false,
        false,
    ),
    (
        "kill_session",
        "Kill session",
        "Terminate a server-managed running command session. Only sessions started over this connection exist here; any other session_id reports SESSION_NOT_FOUND.",
        false,
        true,
        false,
    ),
    (
        "read_output",
        "Read output",
        "Read retained stdout or stderr by output_ref with per-stream byte offset pagination. Only sessions started over this connection exist here; any other output_ref reports SESSION_NOT_FOUND.",
        true,
        false,
        false,
    ),
    (
        "list_runs",
        "List runs",
        "List this connection's command runs in the workspace, newest first, when you do not have the session_id: after a restart, in a new conversation, or to find commands still running. Returns summaries only (redacted command, termination_reason, exit_code, times, bytes per stream) and output_refs to pass to read_output. Commands started by other connections are never listed or counted. Listing does not stop anything; a run started before the server restarted is still listed but does not count as fresh task evidence (workspace_writes_since_start is null).",
        true,
        false,
        false,
    ),
    (
        "list_skills",
        "List skills",
        "List skills discovered from the enabled IDE and coding-agent providers. Skill bodies are loaded separately on demand. Files that look like skills but could not be taken in are listed under skipped, with the reason. Skills with disableModelInvocation are for the user to start: use one only when the user asks for it by name.",
        true,
        false,
        false,
    ),
    (
        "get_skill",
        "Get skill",
        "Load one discovered SKILL.md by id or unique name, including its full workflow body and the files in its directory. With file, read one of those files instead (scripts, references), including for skills installed outside the workspace.",
        true,
        false,
        false,
    ),
    (
        "git_status",
        "Git status",
        "Return git working tree status for the workspace.",
        true,
        false,
        false,
    ),
    (
        "git_diff",
        "Git diff",
        "Return unified git diff for workspace changes.",
        true,
        false,
        false,
    ),
    (
        "git_log",
        "Git log",
        "Return recent git commits with bounded structured metadata.",
        true,
        false,
        false,
    ),
    (
        "git_show",
        "Git show",
        "Return bounded git show output for a revision.",
        true,
        false,
        false,
    ),
    (
        "git_blame",
        "Git blame",
        "Return bounded git blame metadata for a workspace file.",
        true,
        false,
        false,
    ),
    (
        "request_permissions",
        "Request permissions",
        "Legacy: always answers that no grant can be created. Approval for a dangerous operation is confirm=true on that operation after the user approved it.",
        true,
        false,
        false,
    ),
    (
        "view_image",
        "View image",
        "Return a workspace image as MCP image content.",
        true,
        false,
        false,
    ),
];

pub fn tool_api_descriptor() -> Value {
    json!({
        "version": TOOL_API_VERSION,
        "profile": "stable-aggregate",
        "aggregate_tools": ["history_manage", "planning_manage", "task_manage"],
        "legacy_compatibility": true
    })
}

fn history_manage_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": { "type": "string", "enum": ["bootstrap", "checkpoint", "validate", "search", "read"] },
            "workspace_root": { "type": "string", "minLength": 1 },
            "session_key": { "type": "string", "minLength": 1 },
            "expected_path": { "type": "string", "minLength": 1 },
            "history_dir": { "type": "string", "default": "docs/history-session" },
            "title": { "type": "string" },
            "initial_user_input": { "type": "string" },
            "create_if_missing": { "type": "boolean", "default": true },
            "turn_id": { "type": "string", "minLength": 1 },
            "timestamp": { "type": "string" },
            "user_intent": { "type": "string" },
            "raw_user_input": { "type": "string" },
            "findings": { "type": "array", "items": { "type": "string" } },
            "decisions": { "type": "array", "items": { "type": "string" } },
            "files_changed": { "type": "array", "items": { "type": "string" } },
            "tests": { "type": "array", "items": { "type": "string" } },
            "runtime_state": { "type": "array", "items": { "type": "string" } },
            "remaining_issues": { "type": "array", "items": { "type": "string" } },
            "next_actions": { "type": "array", "items": { "type": "string" } },
            "notes": { "type": "string" },
            "repair": { "type": "boolean", "default": false },
            "query": { "type": "string", "default": "" },
            "cursor": { "type": "integer", "minimum": 0, "default": 0 },
            "limit": { "type": "integer", "minimum": 1, "maximum": 50 },
            "number": { "type": "integer", "minimum": 1 },
            "path": { "type": "string", "minLength": 1 },
            "max_bytes": { "type": "integer", "minimum": 1, "maximum": 65536 },
            "expected_hash": { "type": "string", "minLength": 64, "maxLength": 64 }
        },
        "required": ["action"],
        "additionalProperties": false
    })
}

fn planning_manage_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": { "type": "string", "enum": ["state", "create_goal", "update_goal", "create_plan", "update_plan", "request_goal_review", "request_plan_review"] },
            "goal_id": { "type": "string", "minLength": 1 },
            "plan_id": { "type": "string", "minLength": 1 },
            "title": { "type": "string", "minLength": 1 },
            "objective": { "type": "string", "minLength": 1 },
            "success_criteria": { "type": "array", "items": { "type": "string", "minLength": 1 } },
            "constraints": { "type": "array", "items": { "type": "string", "minLength": 1 } },
            "completed_criteria_ids": { "type": "array", "items": { "type": "string", "minLength": 1 } },
            "status": { "type": "string", "enum": ["draft", "active", "paused", "cancelled"] },
            "focus": { "type": "boolean" },
            "steps": { "type": "array", "items": { "type": "string", "minLength": 1 } },
            "step_updates": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "step_id": { "type": "string", "minLength": 1 },
                        "status": { "type": "string", "enum": ["pending", "in_progress", "completed", "blocked", "skipped"] },
                        "notes": { "type": "string" }
                    },
                    "required": ["step_id", "status"],
                    "additionalProperties": false
                }
            },
            "summary": { "type": "string", "minLength": 1 }
        },
        "required": ["action"],
        "additionalProperties": false
    })
}

/// finish 的验收证据。task_manage 和 finish_task 两处一字不差。
static EVIDENCE_SESSION_IDS_SCHEMA: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| {
    json!({
        "type": "array",
        "items": { "type": "string", "minLength": 1 },
        "description": "finish: session_id of exec_command runs from this task that are the acceptance evidence (tests, build, lint). Each must have exited 0, with no workspace writes during the run and no changes since. All accepted -> completed; any rejected -> error with the reasons, task unchanged."
    })
});

const ACCEPT_FINGERPRINT_DESCRIPTION: &str = "refresh_baseline: current.fingerprint from a previous refresh_baseline call you reviewed. Adopts the current workspace as the task baseline; refused if the workspace changed again since. Requires reason.";

fn task_manage_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": { "type": "string", "enum": ["status", "operation_log", "project_state", "start", "update", "pause", "resume", "finish", "context", "events", "change_summary", "refresh_baseline"] },
            "task_id": { "type": "string", "minLength": 1 },
            "objective": { "type": "string", "minLength": 1 },
            "completed_steps": { "type": "array", "items": { "type": "string" } },
            "pending_steps": { "type": "array", "items": { "type": "string" } },
            "summary": { "type": "string" },
            "allow_unverified": { "type": "boolean", "default": false },
            "evidence_session_ids": EVIDENCE_SESSION_IDS_SCHEMA.clone(),
            "accept_fingerprint": { "type": "string", "minLength": 1, "description": ACCEPT_FINGERPRINT_DESCRIPTION },
            "reason": { "type": "string", "description": "refresh_baseline: who made the reviewed changes and why they belong to this task." },
            "cursor": { "type": "integer", "minimum": 0, "default": 0 },
            "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 50 },
            "max_files": { "type": "integer", "minimum": 1, "maximum": 10000, "default": 200 },
            "max_bytes": { "type": "integer", "minimum": 8192, "maximum": 131072, "default": 32768 },
            "change_id": { "type": "string" }
        },
        "required": ["action"],
        "additionalProperties": false
    })
}

/// Legacy-compatible core surface. It keeps lifecycle-specific tool names while
/// also exposing the Stable Tool API v2 managers so MCP and Actions can migrate
/// without a flag day.
pub const CORE_TOOLS: &[&str] = &[
    "server_info",
    "history_manage",
    "planning_manage",
    "task_manage",
    "history_session_bootstrap",
    "history_session_checkpoint",
    "history_session_validate",
    "history_session_search",
    "history_session_read",
    "planning_state",
    "create_goal",
    "update_goal",
    "create_plan",
    "update_plan",
    "request_goal_review",
    "request_plan_review",
    "capability_health_check",
    "check_exec_environment",
    "check_command",
    "get_default_cwd",
    "set_default_cwd",
    "list_skills",
    "get_skill",
    "read_file",
    "read_notebook",
    "list_dir",
    "list_files",
    "search_text",
    "grep_text",
    "apply_patch",
    "exec_command",
    "write_stdin",
    "kill_session",
    "read_output",
    "list_runs",
    "git_status",
    "git_diff",
    "git_log",
    "git_show",
    "git_blame",
    "request_permissions",
    "view_image",
];

/// Compact default surface: keep ordinary development tools plus the Stable Tool
/// API v2 managers while moving lifecycle-specific compatibility tools, skills,
/// harness internals, and permission helpers to legacy/advanced profiles.
pub const COMPACT_TOOLS: &[&str] = &[
    "server_info",
    "history_manage",
    "planning_manage",
    "task_manage",
    "check_exec_environment",
    "check_command",
    // compact 以前把这两个砍了，等于默认档下 Skill 整体不可用——项目把用法
    // 写进 skill，模型却看不见（RFC-0003 G2）。目录本身有字符预算，见
    // agent_context::COMPACT_SKILL_CATALOG_CHARS。
    "list_skills",
    "get_skill",
    "get_default_cwd",
    "set_default_cwd",
    "read_file",
    "read_notebook",
    "list_dir",
    "list_files",
    "search_text",
    "grep_text",
    "apply_patch",
    "patch_check",
    "exec_command",
    "write_stdin",
    "kill_session",
    "read_output",
    "list_runs",
    "git_status",
    "git_diff",
    "git_log",
    "git_show",
    "git_blame",
    "view_image",
];

pub const CORE_READ_ONLY_TOOLS: &[&str] = &[
    "server_info",
    "planning_state",
    "check_exec_environment",
    "check_command",
    "get_default_cwd",
    "list_skills",
    "get_skill",
    "set_default_cwd",
    "read_file",
    "read_notebook",
    "list_dir",
    "list_files",
    "search_text",
    "grep_text",
    "read_output",
    "list_runs",
    "git_status",
    "git_diff",
    "git_log",
    "git_show",
    "git_blame",
    "request_permissions",
    "view_image",
];

pub const ALLOWED_TOOLS: &[&str] = &[
    "harness_status",
    "operation_log",
    "server_info",
    "history_manage",
    "planning_manage",
    "task_manage",
    "history_session_bootstrap",
    "history_session_checkpoint",
    "history_session_validate",
    "history_session_search",
    "history_session_read",
    "planning_state",
    "create_goal",
    "update_goal",
    "create_plan",
    "update_plan",
    "request_goal_review",
    "request_plan_review",
    "capability_health_check",
    "check_exec_environment",
    "check_command",
    "exec_health_check",
    "get_default_cwd",
    "set_default_cwd",
    "list_skills",
    "get_skill",
    "read_file",
    "read_notebook",
    "list_dir",
    "list_files",
    "search_text",
    "grep_text",
    "grep",
    "apply_patch",
    "patch_check",
    "exec_command",
    "write_stdin",
    "kill_session",
    "read_output",
    "list_runs",
    "git_status",
    "git_diff",
    "git_log",
    "git_show",
    "git_blame",
    "project_state",
    "start_task",
    "update_task",
    "pause_task",
    "resume_task",
    "finish_task",
    "task_context",
    "list_task_events",
    "change_summary",
    "refresh_baseline",
    "request_permissions",
    "view_image",
];

pub const MUTATING_TOOLS: &[&str] = &[
    "history_manage",
    "planning_manage",
    "task_manage",
    "history_session_bootstrap",
    "history_session_checkpoint",
    "history_session_validate",
    "create_goal",
    "update_goal",
    "create_plan",
    "update_plan",
    "request_goal_review",
    "request_plan_review",
    "apply_patch",
    "exec_command",
    "write_stdin",
    "kill_session",
    "set_default_cwd",
    "start_task",
    "update_task",
    "pause_task",
    "resume_task",
    "finish_task",
    // 只看的那一种不改东西，dispatch 按参数区分（manage::action_is_mutating）；
    // 这张表给 Actions 标 isConsequential，按会改的那一种算。
    "refresh_baseline",
];

pub const READ_ONLY_TOOLS: &[&str] = &[
    "harness_status",
    "operation_log",
    "server_info",
    "history_session_search",
    "history_session_read",
    "planning_state",
    "check_exec_environment",
    "check_command",
    "exec_health_check",
    "get_default_cwd",
    "list_skills",
    "get_skill",
    "read_file",
    "read_notebook",
    "list_dir",
    "list_files",
    "search_text",
    "grep_text",
    "grep",
    "read_output",
    "list_runs",
    "git_status",
    "git_diff",
    "git_log",
    "git_show",
    "git_blame",
    "request_permissions",
    "view_image",
    "patch_check",
    "project_state",
    "task_context",
    "list_task_events",
    "change_summary",
];

pub fn is_allowed_tool(name: &str) -> bool {
    ALLOWED_TOOLS.contains(&name)
}

pub fn canonical_tool_name(name: &str) -> &str {
    match name {
        "grep" => "grep_text",
        _ => name,
    }
}

pub fn normalize_tool_profile(profile: &str) -> &'static str {
    match profile {
        "compact" => "compact",
        "advanced" => "advanced",
        "read-only" => "read-only",
        _ => "core",
    }
}

/// 已退役的工具集。它暴露的工具和 advanced 一样多（能写能执行），却把每个都标成
/// `readOnlyHint: true`——客户端据此不再弹确认框，等于替用户把那道真人确认关了
/// （审查 D08）。
pub const RETIRED_COMPAT_PROFILE: &str = "compat-readonly-all";

/// 读配置时把退役的 [`RETIRED_COMPAT_PROFILE`] 换成 advanced：工具一个不少，
/// 标注改回实话。不换的话 [`normalize_tool_profile`] 会把它当成认不出的值、
/// 悄悄降成 core，老配置升级后就少了一半工具。
///
/// 放在反序列化这一层，是因为配置只从这里进来（文件、命令行经 IPC 发回的整份配置），
/// 换过之后内存里就不再有这个值，下次保存也写成 advanced。
pub fn deserialize_tool_profile<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <String as serde::Deserialize>::deserialize(deserializer)?;
    Ok(if value.trim() == RETIRED_COMPAT_PROFILE {
        "advanced".into()
    } else {
        value
    })
}

pub fn exposed_tool_names(tool_profile: &str) -> Vec<&'static str> {
    let names = match normalize_tool_profile(tool_profile) {
        "compact" => COMPACT_TOOLS.to_vec(),
        "read-only" => CORE_READ_ONLY_TOOLS.to_vec(),
        "advanced" => P0_TOOLS.iter().map(|(name, ..)| *name).collect(),
        _ => CORE_TOOLS.to_vec(),
    };

    names
        .into_iter()
        .filter(|name| !CLIENT_HIDDEN_TOOLS.contains(name))
        .collect()
}

/// Legacy compatibility tools that remain callable by name but must not be advertised to
/// MCP clients. `request_permissions` cannot persist a grant for ChatGPT in trusted/safe
/// mode, so advertising it encourages retry loops after policy errors.
const CLIENT_HIDDEN_TOOLS: &[&str] = &["request_permissions"];

pub fn list_tools() -> Vec<Value> {
    list_tools_for_profile("full")
}

fn compact_description<'a>(name: &str, fallback: &'a str) -> &'a str {
    match name {
        "server_info" => "Return compact server and workspace metadata.",
        "history_manage" => "Manage project history through one stable action-based API.",
        "planning_manage" => "Manage Goal and Plan state through one stable action-based API.",
        "task_manage" => "Manage durable task state through one stable action-based API.",
        "history_session_bootstrap" => "Create or resume a history archive when explicitly requested; returns bounded metadata only.",
        "history_session_checkpoint" => "Append a redacted checkpoint when session recording is enabled; session target may be omitted for lazy initialization.",
        "history_session_validate" => "Validate or rebuild history indexes without deleting archives.",
        "history_session_search" => "Search indexed session archives and return bounded matches.",
        "history_session_read" => "Read a bounded UTF-8 page from one selected session archive.",
        "read_file" => "Read a bounded UTF-8 text range from a workspace file.",
        "search_text" | "grep_text" => "Search workspace text with bounded previews.",
        "apply_patch" => "Apply a workspace patch and return a change summary.",
        "exec_command" => "Run an allowed workspace command with bounded output.",
        "write_stdin" => "Write to a running command session with bounded output.",
        "read_output" => "Read a bounded page from a command output session.",
        "git_diff" => "Return bounded Git diff output.",
        _ => fallback,
    }
}

/// 一份 tools/list 的摘要：每个工具收哪些参数、整份的指纹。
///
/// 给"客户端看到的工具表是不是服务端现在给的这份"做对照（审查 D04）。客户端会缓存
/// 工具表，服务端又声明 `listChanged: false`、不会通知它刷新；升级之后客户端还拿着
/// 旧表的话，新工具、新参数它都看不见，而源码和版本号都说"有"。只对工具个数或
/// 版本号查不出来：参数少了个数不变，同一个版本号能对应几十个提交。
///
/// 指纹是整份 tools/list（名字、说明、schema、标注）的 SHA-256 前 16 位，两边
/// 任何一个字不同就不同。
pub fn surface_digest(tools: &[Value]) -> Value {
    use sha2::{Digest, Sha256};

    let bytes = serde_json::to_vec(tools).unwrap_or_default();
    let fingerprint = format!("{:x}", Sha256::digest(&bytes));
    let params: serde_json::Map<String, Value> = tools
        .iter()
        .filter_map(|tool| {
            let name = tool.get("name")?.as_str()?.to_string();
            let mut names: Vec<&String> = tool
                .pointer("/inputSchema/properties")
                .and_then(Value::as_object)
                .map(|properties| properties.keys().collect())
                .unwrap_or_default();
            names.sort();
            Some((name, json!(names)))
        })
        .collect();
    json!({
        "tool_count": tools.len(),
        "tools_fingerprint": &fingerprint[..16],
        "tools": params
    })
}

pub fn list_tools_for_profile(tool_profile: &str) -> Vec<Value> {
    exposed_tool_names(tool_profile)
        .into_iter()
        .filter_map(|name| {
            P0_TOOLS.iter().find(|(n, ..)| *n == name).map(|entry| {
                let (name, title, description, read_only, destructive, open_world) = *entry;
                json!({
                    "name": name,
                    "title": title,
                    "description": if tool_profile == "compact" {
                        compact_description(name, description)
                    } else {
                        description
                    },
                    "inputSchema": input_schema(name),
                    "annotations": {
                        "title": title,
                        "readOnlyHint": read_only,
                        "destructiveHint": destructive,
                        "idempotentHint": read_only,
                        "openWorldHint": open_world
                    }
                })
            })
        })
        .collect()
}

/// `exec_command` / `check_command` / `apply_patch` / `patch_check` 的 `confirm`。
///
/// 这个字段是调用方自己填的，服务端看不到用户有没有点头，只拿它开危险操作那道门。
/// 真人确认靠客户端按工具标注弹的确认框，所以标注必须照实给（审查 D08）。
static CONFIRM_SCHEMA: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| {
    json!({
        "type": "boolean",
        "default": false,
        "description": "Set true only after the user explicitly approved this specific operation. The server cannot see or verify that approval; it only uses this flag to let an operation that needs confirmation through."
    })
});

/// `apply_patch` / `patch_check` 的版本前置条件。两处一字不差，写两遍迟早
/// 会只改一处。
static EXPECTED_VERSIONS_SCHEMA: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| {
    json!({
        "type": "object",
        "additionalProperties": { "type": ["string", "null"] },
        "description": "Optional preconditions: path -> the version read_file or patch_check returned, or null if the path must not exist yet. A file written since then is refused with FILE_VERSION_CONFLICT instead of being overwritten. Paths must be ones this patch touches."
    })
});

/// `apply_patch` / `patch_check` 的 notebook 单元格编辑。
///
/// 预检必须能预检**同一次调用**：`patch_check` 以前只收 `patch`，于是带
/// notebook 编辑或需要 `confirm` 的那次 `apply_patch` 根本没法先试一遍，
/// 预检结果和真跑结果对不上（审查 A19、A01 的补丁那一半）。
static NOTEBOOK_EDITS_SCHEMA: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "path": { "type": "string", "minLength": 1 },
                "cells": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "cell_id": { "type": "string", "minLength": 1 },
                            "new_source": { "type": "string" },
                            "cell_type": { "type": "string", "enum": ["code", "markdown"] },
                            "edit_mode": { "type": "string", "enum": ["replace", "insert", "delete"], "default": "replace" }
                        },
                        "additionalProperties": false
                    }
                }
            },
            "required": ["path", "cells"],
            "additionalProperties": false
        },
        "description": "Cell edits for Jupyter notebooks, applied in the same transaction as patch. cell_id is what read_notebook shows; replace and delete need it, insert puts the new cell after it (or first without one). Replacing a code cell clears its outputs. Either patch or notebook_edits is required."
    })
});

/// `exec_command` / `check_command` 的结构化命令入口。
///
/// 和 `cmd` 二选一。给 `argv` 的时候参数原样送进内核，不经过任何 shell，所以
/// 引号、换行、`|` 都是数据；`cmd` 那一行仍然按 shell 词法拆、仍然禁止未加引号
/// 的操作符（审查 C04）。
static ARGV_SCHEMA: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| {
    json!({
        "type": "array",
        "items": { "type": "string" },
        "minItems": 1,
        "description": "Program plus arguments, one per element: [\"rg\", \"foo|bar\", \"src\"]. Arguments go to the process as-is — no shell, so quotes, newlines and | are data, not operators. Use this instead of cmd whenever an argument contains shell characters. Give cmd or argv, never both."
    })
});

/// glob 的基准是**工作区根**，不是 `path`。这件事以前只在代码里，模型在子目录
/// 里搜 `exec.rs` 得到零结果，看不出是"没有这个文件"还是"glob 基准不对"
/// （审查 F02、复现 E07）。结果里同时回 `glob_base` 和 `search_root`。
const GLOB_BASE_DESCRIPTION: &str = "Glob patterns match the path relative to the WORKSPACE ROOT, not to `path`. Under path=\"crates\", the pattern \"exec.rs\" matches nothing — write \"**/exec.rs\" or the full \"crates/**/exec.rs\". The result echoes glob_base and search_root.";

const COMMAND_CMD_DESCRIPTION: &str = "One command line, split with shell word rules but NOT run through a shell: unquoted ;, &&, |, > and $() are rejected. Use argv when an argument itself contains those characters.";

/// `cwd` 一直被当成 `workdir` 的别名读，但 schema 里没写过，客户端发现不了。
/// 现在参数名对不上会被拒（`tools::args::reject_unknown`），所以这个别名要么
/// 写进 schema，要么就是一段没人能走到的死代码。
const WORKDIR_ALIAS_DESCRIPTION: &str = "Alias for workdir. If both are given, workdir wins.";

pub fn input_schema(name: &str) -> Value {
    match name {
        "history_manage" => history_manage_schema(),
        "planning_manage" => planning_manage_schema(),
        "task_manage" => task_manage_schema(),
        "list_skills" => json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        "get_skill" => json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "minLength": 1 },
                "name": { "type": "string", "minLength": 1 },
                "file": {
                    "type": "string",
                    "minLength": 1,
                    "description": "A path relative to the skill's directory, as listed in files (e.g. scripts/run.sh). Only files inside that directory can be read."
                }
            },
            "description": "Provide either id or name to load a skill. If both are provided, id takes priority.",
            "additionalProperties": false
        }),
        "history_session_bootstrap" => json!({
            "type": "object",
            "properties": {
                "workspace_root": { "type": "string", "minLength": 1 },
                "session_key": { "type": "string", "minLength": 1 },
                "title": { "type": "string" },
                "initial_user_input": { "type": "string" },
                "history_dir": { "type": "string", "default": "docs/history-session" },
                "create_if_missing": { "type": "boolean", "default": true }
            },
            "additionalProperties": false
        }),
        "history_session_checkpoint" => json!({
            "type": "object",
            "properties": {
                "workspace_root": { "type": "string", "minLength": 1 },
                "session_key": { "type": "string", "minLength": 1 },
                "expected_path": { "type": "string", "minLength": 1 },
                "history_dir": { "type": "string", "default": "docs/history-session" },
                "turn_id": { "type": "string", "minLength": 1 },
                "timestamp": { "type": "string" },
                "user_intent": { "type": "string" },
                "raw_user_input": { "type": "string" },
                "findings": { "type": "array", "items": { "type": "string" } },
                "decisions": { "type": "array", "items": { "type": "string" } },
                "files_changed": { "type": "array", "items": { "type": "string" } },
                "tests": { "type": "array", "items": { "type": "string" } },
                "runtime_state": { "type": "array", "items": { "type": "string" } },
                "remaining_issues": { "type": "array", "items": { "type": "string" } },
                "next_actions": { "type": "array", "items": { "type": "string" } },
                "notes": { "type": "string" }
            },
            "additionalProperties": false
        }),
        "history_session_validate" => json!({
            "type": "object",
            "properties": {
                "workspace_root": { "type": "string", "minLength": 1 },
                "history_dir": { "type": "string", "default": "docs/history-session" },
                "repair": { "type": "boolean", "default": false }
            },
            "additionalProperties": false
        }),
        "history_session_search" => json!({
            "type": "object",
            "properties": {
                "workspace_root": { "type": "string", "minLength": 1 },
                "history_dir": { "type": "string", "default": "docs/history-session" },
                "query": { "type": "string", "default": "" },
                "cursor": { "type": "integer", "minimum": 0, "default": 0 },
                "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 5 }
            },
            "additionalProperties": false
        }),
        "history_session_read" => json!({
            "type": "object",
            "properties": {
                "workspace_root": { "type": "string", "minLength": 1 },
                "history_dir": { "type": "string", "default": "docs/history-session" },
                "number": { "type": "integer", "minimum": 1 },
                "path": { "type": "string", "minLength": 1 },
                "cursor": { "type": "integer", "minimum": 0, "default": 0 },
                "max_bytes": { "type": "integer", "minimum": 1, "maximum": 65536, "default": 16384 },
                "expected_hash": { "type": "string", "minLength": 64, "maxLength": 64 }
            },
            "additionalProperties": false
        }),
        "planning_state" => json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        "create_goal" => json!({
            "type": "object",
            "properties": {
                "title": { "type": "string", "minLength": 1 },
                "objective": { "type": "string", "minLength": 1 },
                "success_criteria": { "type": "array", "items": { "type": "string", "minLength": 1 } },
                "constraints": { "type": "array", "items": { "type": "string", "minLength": 1 } }
            },
            "required": ["title", "objective"],
            "additionalProperties": false
        }),
        "update_goal" => json!({
            "type": "object",
            "properties": {
                "goal_id": { "type": "string", "minLength": 1 },
                "title": { "type": "string", "minLength": 1 },
                "objective": { "type": "string", "minLength": 1 },
                "status": { "type": "string", "enum": ["active", "paused", "cancelled"] },
                "constraints": { "type": "array", "items": { "type": "string", "minLength": 1 } },
                "completed_criteria_ids": { "type": "array", "items": { "type": "string", "minLength": 1 } },
                "focus": { "type": "boolean" }
            },
            "required": ["goal_id"],
            "additionalProperties": false
        }),
        "create_plan" => json!({
            "type": "object",
            "properties": {
                "goal_id": { "type": "string", "minLength": 1 },
                "title": { "type": "string", "minLength": 1 },
                "objective": { "type": "string", "minLength": 1 },
                "steps": { "type": "array", "items": { "type": "string", "minLength": 1 } }
            },
            "required": ["title", "objective"],
            "additionalProperties": false
        }),
        "update_plan" => json!({
            "type": "object",
            "properties": {
                "plan_id": { "type": "string", "minLength": 1 },
                "status": { "type": "string", "enum": ["draft", "active", "paused", "cancelled"] },
                "focus": { "type": "boolean" },
                "step_updates": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "step_id": { "type": "string", "minLength": 1 },
                            "status": { "type": "string", "enum": ["pending", "in_progress", "completed", "blocked", "skipped"] },
                            "notes": { "type": "string" }
                        },
                        "required": ["step_id", "status"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["plan_id"],
            "additionalProperties": false
        }),
        "request_goal_review" => json!({
            "type": "object",
            "properties": {
                "goal_id": { "type": "string", "minLength": 1 },
                "summary": { "type": "string", "minLength": 1 }
            },
            "required": ["goal_id", "summary"],
            "additionalProperties": false
        }),
        "request_plan_review" => json!({
            "type": "object",
            "properties": {
                "plan_id": { "type": "string", "minLength": 1 },
                "summary": { "type": "string", "minLength": 1 }
            },
            "required": ["plan_id", "summary"],
            "additionalProperties": false
        }),
        "harness_status" => json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        "exec_health_check" => json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        "operation_log" => json!({
            "type": "object",
            "properties": {
                "cursor": { "type": "integer", "minimum": 0, "default": 0 },
                "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 50 }
            },
            "additionalProperties": false
        }),
        "project_state" => json!({
            "type": "object",
            "properties": {
                "max_files": { "type": "integer", "minimum": 1, "maximum": 10000, "default": 200 }
            },
            "additionalProperties": false
        }),
        "start_task" => json!({
            "type": "object",
            "properties": {
                "objective": { "type": "string", "minLength": 1 }
            },
            "required": ["objective"],
            "additionalProperties": false
        }),
        "update_task" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "minLength": 1 },
                "completed_steps": { "type": "array", "items": { "type": "string" } },
                "pending_steps": { "type": "array", "items": { "type": "string" } }
            },
            "required": ["task_id"],
            "additionalProperties": false
        }),
        "pause_task" | "resume_task" => json!({
            "type": "object",
            "properties": { "task_id": { "type": "string", "minLength": 1 } },
            "required": ["task_id"],
            "additionalProperties": false
        }),
        "finish_task" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "minLength": 1 },
                "summary": { "type": "string" },
                "allow_unverified": { "type": "boolean", "default": false },
                "evidence_session_ids": EVIDENCE_SESSION_IDS_SCHEMA.clone()
            },
            "required": ["task_id"],
            "additionalProperties": false
        }),
        "refresh_baseline" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "minLength": 1 },
                "accept_fingerprint": { "type": "string", "minLength": 1, "description": ACCEPT_FINGERPRINT_DESCRIPTION },
                "reason": { "type": "string", "description": "Who made the reviewed changes and why they belong to this task." }
            },
            "required": ["task_id"],
            "additionalProperties": false
        }),
        "task_context" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string" },
                "max_bytes": { "type": "integer", "minimum": 8192, "maximum": 131072, "default": 32768 }
            },
            "additionalProperties": false
        }),
        "list_task_events" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "minLength": 1 },
                "cursor": { "type": "integer", "minimum": 0, "default": 0 },
                "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 50 }
            },
            "required": ["task_id"],
            "additionalProperties": false
        }),
        "change_summary" => json!({
            "type": "object",
            "properties": { "task_id": { "type": "string" }, "change_id": { "type": "string" } },
            "additionalProperties": false
        }),
        "read_file" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "minLength": 1 },
                "start_line": { "type": "integer", "minimum": 1, "default": 1 },
                "end_line": { "type": "integer", "minimum": 1 },
                "max_bytes": { "type": "integer", "minimum": 1, "maximum": 1048576, "default": 32768 },
                "start_byte": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Read from this absolute byte offset instead of by line. Only needed when a single line is longer than max_bytes: paging by line then skips the rest of that line, and the result says so with skipped_bytes plus the next_start_byte to resume from. Line numbers come back null in this mode — counting them would mean rescanning the file from the start."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
        "list_dir" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "default": "." },
                "recursive": { "type": "boolean", "default": false },
                "max_depth": { "type": "integer", "minimum": 1, "maximum": 20, "default": 1 },
                "max_entries": { "type": "integer", "minimum": 1, "maximum": 10000, "default": 100 },
                "include_hidden": { "type": "boolean", "default": false },
                "include_ignored": { "type": "boolean", "default": false }
            },
            "additionalProperties": false
        }),
        "list_files" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "default": "." },
                "patterns": { "type": "array", "items": { "type": "string" }, "description": GLOB_BASE_DESCRIPTION },
                "glob": { "type": "string", "description": "Alias for a single patterns entry" },
                "exclude_patterns": { "type": "array", "items": { "type": "string" }, "description": GLOB_BASE_DESCRIPTION },
                "include_hidden": { "type": "boolean", "default": false },
                "include_ignored": { "type": "boolean", "default": false },
                "max_results": { "type": "integer", "minimum": 1, "maximum": 50000, "default": 5000 }
            },
            "additionalProperties": false
        }),
        "search_text" | "grep_text" | "grep" => json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "minLength": 1 },
                "path": { "type": "string", "default": "." },
                "glob": { "type": "string", "description": "Alias appended to include_globs" },
                "include_globs": { "type": "array", "items": { "type": "string" }, "description": GLOB_BASE_DESCRIPTION },
                "exclude_globs": { "type": "array", "items": { "type": "string" }, "description": GLOB_BASE_DESCRIPTION },
                "regex": { "type": "boolean", "default": false },
                "case_sensitive": { "type": "boolean", "default": false },
                "context_lines": { "type": "integer", "minimum": 0, "maximum": 20, "default": 0 },
                "max_preview_bytes": { "type": "integer", "minimum": 64, "maximum": 4096, "default": 256 },
                "max_results": { "type": "integer", "minimum": 1, "maximum": 10000, "default": 100 },
                "max_file_bytes": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 67108864,
                    "default": 2097152,
                    "description": "Skip files larger than this many bytes (default 2MiB) to avoid memory spikes"
                },
                "output_mode": {
                    "type": "string",
                    "enum": ["content", "files_with_matches", "count"],
                    "default": "content",
                    "description": "content: matching lines with context (results in matches[]). files_with_matches: only paths (files[]). count: matching lines per file (counts[]). max_results limits matching lines in content mode and files in the other two."
                },
                "multiline": {
                    "type": "boolean",
                    "default": false,
                    "description": "Let one match span lines; . then matches newlines too. The reported line is where the match starts, as in ripgrep -U."
                },
                "type": {
                    "type": "string",
                    "description": "Only search files of this type, e.g. rust, py, ts, md. A subset of ripgrep's --type names; an unknown name is an error that lists the supported ones."
                },
                "include_hidden": {
                    "type": "boolean",
                    "default": false,
                    "description": "Also search dotfiles and dot-directories such as .github. gld's own data directory is never searched."
                },
                "include_ignored": {
                    "type": "boolean",
                    "default": false,
                    "description": "Also search files .gitignore excludes, such as build output. gld's own data directory is never searched."
                }
            },
            "description": "returned_matches is what this call gives back; total_matches is the project-wide total and is null once truncated is true, because the scan stopped early. Directories are walked in file-name order, so the same query twice returns the same cut-short batch — narrow the query or raise max_results to see more.",
            "required": ["query"],
            "additionalProperties": false
        }),
        "read_notebook" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "minLength": 1 },
                "start_cell": { "type": "integer", "minimum": 0, "default": 0 },
                "max_bytes": { "type": "integer", "minimum": 1, "maximum": 1048576, "default": 32768 }
            },
            "description": "Cells come back in content as <cell id=\"…\"> blocks with their outputs. Change them with apply_patch's notebook_edits, addressing cells by the id shown here. Images in outputs are noted but not returned.",
            "required": ["path"],
            "additionalProperties": false
        }),
        "apply_patch" => json!({
            "type": "object",
            "properties": {
                "patch": { "type": "string", "minLength": 1 },
                "notebook_edits": NOTEBOOK_EDITS_SCHEMA.clone(),
                "dry_run": { "type": "boolean", "default": false },
                "confirm": CONFIRM_SCHEMA.clone(),
                "expected_versions": EXPECTED_VERSIONS_SCHEMA.clone(),
                "reason": { "type": "string", "default": "" }
            },
            "additionalProperties": false
        }),
        // 和 apply_patch 同一组参数，除了 `dry_run`——那一条是这个工具自己
        // 定死的 true，收进来只会让人以为可以关掉。
        "patch_check" => json!({
            "type": "object",
            "properties": {
                "patch": { "type": "string", "minLength": 1 },
                "notebook_edits": NOTEBOOK_EDITS_SCHEMA.clone(),
                "confirm": CONFIRM_SCHEMA.clone(),
                "expected_versions": EXPECTED_VERSIONS_SCHEMA.clone()
            },
            "description": "Runs apply_patch without writing. Pass the arguments you intend to apply — including confirm — or the preflight answers a different question than the real call. Either patch or notebook_edits is required.",
            "additionalProperties": false
        }),
        // 和 exec_command 同一组参数：预检要判的就是"这一组参数会不会被放行"，
        // 少一个字段就可能判出不一样的结果。
        "check_command" => json!({
            "type": "object",
            "properties": {
                "cmd": { "type": "string", "minLength": 1, "description": COMMAND_CMD_DESCRIPTION },
                "argv": ARGV_SCHEMA.clone(),
                "workdir": { "type": "string", "default": "." },
                "cwd": { "type": "string", "description": WORKDIR_ALIAS_DESCRIPTION },
                "timeout_ms": { "type": "integer", "minimum": 1, "maximum": 600000 },
                "confirm": CONFIRM_SCHEMA.clone(),
                "filesystem_scope": { "type": "string", "enum": ["workspace"], "default": "workspace" }
            },
            "description": "ok=true 只表示预检做完了；能不能跑看 decision（allow / deny / needs_approval）。不会启动进程、不联网。给 cmd 或 argv，二选一。",
            "additionalProperties": false
        }),
        "exec_command" => json!({
            "type": "object",
            "properties": {
                "cmd": { "type": "string", "minLength": 1, "description": COMMAND_CMD_DESCRIPTION },
                "argv": ARGV_SCHEMA.clone(),
                "workdir": { "type": "string", "default": "." },
                "cwd": { "type": "string", "description": WORKDIR_ALIAS_DESCRIPTION },
                "timeout_ms": { "type": "integer", "minimum": 1, "maximum": 600000, "default": 30000 },
                "max_output_bytes": { "type": "integer", "minimum": 1024, "maximum": 1048576, "default": 32768 },
                "yield_time_ms": { "type": "integer", "minimum": 0, "maximum": 30000, "default": 1000 },
                "tty": { "type": "boolean", "default": false, "description": "Deprecated name for stdin_mode=interactive. It keeps stdin open — it does NOT give the command a terminal: this is a pipe, so programs that require a TTY still will not work." },
                "stdin": { "type": "string", "default": "" },
                "stdin_mode": {
                    "type": "string",
                    "enum": ["close", "once", "interactive"],
                    "description": "close (default when no stdin is given): stdin is closed right away, so a command that reads it gets EOF instead of hanging until timeout. once (default when stdin is given): write it, then close. interactive: write it and keep stdin open for write_stdin. Applied before the call returns, so yield_time_ms=0 keeps the initial input."
                },
                "confirm": CONFIRM_SCHEMA.clone(),
                "filesystem_scope": { "type": "string", "enum": ["workspace"], "default": "workspace" },
                "reason": { "type": "string", "default": "" }
            },
            // 不用 `oneOf` / `required` 表达"cmd 和 argv 二选一"：工具 schema
            // 一直是平的（`core_catalog_...` 那条测试钉着），Actions 那条出口
            // 也吃不下组合关键字。二选一由服务端判，两个都不给报
            // missing_command，两个都给报 conflicting_command_forms。
            "additionalProperties": false
        }),
        "write_stdin" => json!({
            "type": "object",
            "properties": {
                "session_id": { "type": "string", "minLength": 1 },
                "chars": { "type": "string", "default": "" },
                "yield_time_ms": { "type": "integer", "minimum": 0, "maximum": 30000, "default": 1000 },
                "max_output_bytes": { "type": "integer", "minimum": 1, "maximum": 1048576, "default": 32768 }
            },
            "required": ["session_id"],
            "additionalProperties": false
        }),
        "kill_session" => json!({
            "type": "object",
            "properties": {
                "session_id": { "type": "string", "minLength": 1 },
                "signal": { "type": "string", "enum": ["TERM", "KILL", "INT"], "default": "TERM" },
                "wait_ms": { "type": "integer", "minimum": 0, "maximum": 30000, "default": 5000 },
                "max_output_bytes": { "type": "integer", "minimum": 1, "maximum": 1048576, "default": 32768 }
            },
            "required": ["session_id"],
            "additionalProperties": false
        }),
        "read_output" => json!({
            "type": "object",
            "properties": {
                "output_ref": { "type": "string", "minLength": 1 },
                "stream": { "type": "string", "enum": ["stdout", "stderr"] },
                "offset": { "type": "integer", "minimum": 0, "default": 0 },
                "limit": { "type": "integer", "minimum": 1, "maximum": 1048576, "default": 4096 }
            },
            "required": ["output_ref"],
            "additionalProperties": false
        }),
        "list_runs" => json!({
            "type": "object",
            "properties": {
                "status": {
                    "type": "array",
                    "minItems": 1,
                    "items": { "type": "string", "enum": ["running", "exited", "timeout", "killed", "interrupted", "unknown"] },
                    "description": "Only runs whose termination_reason is one of these. \"running\" also matches a run that is being stopped but whose process has not exited yet."
                },
                "started_within_minutes": { "type": "integer", "minimum": 1, "maximum": 10080, "description": "Only runs started in the last N minutes." },
                "limit": { "type": "integer", "minimum": 1, "maximum": 64, "default": 20 },
                "cursor": { "type": "string", "description": "next_cursor from the previous page, passed back unchanged. Keep status and started_within_minutes the same while paging; change them and start again without a cursor." }
            },
            "additionalProperties": false
        }),
        "git_status" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "default": "." },
                "include_untracked": { "type": "boolean", "default": true },
                "max_entries": { "type": "integer", "minimum": 1, "maximum": 10000, "default": 500 }
            },
            "additionalProperties": false
        }),
        "git_diff" => json!({
            "type": "object",
            "properties": {
                "paths": { "type": "array", "items": { "type": "string" }, "default": [] },
                "staged": { "type": "boolean", "default": false },
                "unstaged": { "type": "boolean", "default": true },
                "context_lines": { "type": "integer", "minimum": 0, "maximum": 20, "default": 3 },
                "max_bytes": { "type": "integer", "minimum": 1024, "maximum": 1048576, "default": 65536 }
            },
            "additionalProperties": false
        }),
        "git_log" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "default": "." },
                "ref": { "type": "string", "default": "HEAD" },
                "max_count": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 },
                "skip": { "type": "integer", "minimum": 0, "maximum": 10000, "default": 0 }
            },
            "additionalProperties": false
        }),
        "git_show" => json!({
            "type": "object",
            "properties": {
                "rev": { "type": "string", "default": "HEAD" },
                "path": { "type": "string" },
                "paths": { "type": "array", "items": { "type": "string" } },
                "include_diff": { "type": "boolean", "default": true },
                "context_lines": { "type": "integer", "minimum": 0, "maximum": 20, "default": 3 },
                "max_bytes": { "type": "integer", "minimum": 1, "maximum": 1048576, "default": 65536 }
            },
            "additionalProperties": false
        }),
        "git_blame" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "minLength": 1 },
                "rev": { "type": "string" },
                "start_line": { "type": "integer", "minimum": 1, "default": 1 },
                "end_line": { "type": "integer", "minimum": 1 },
                "max_lines": { "type": "integer", "minimum": 1, "maximum": 1000, "default": 200 }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
        "request_permissions" => json!({
            "type": "object",
            "properties": {
                "tool_name": {
                    "type": "string",
                    "enum": ["exec_command", "apply_patch"]
                },
                "permission": {
                    "type": "string",
                    "enum": [
                        "network",
                        "destructive_command",
                        "long_timeout",
                        "sensitive_env",
                        "shell_expansion",
                        "inline_script",
                        "privileged_executable",
                        "write_generated_or_ignored"
                    ]
                },
                "reason": { "type": "string", "minLength": 1 },
                "arguments": { "type": "object", "additionalProperties": true },
                "scope": {
                    "type": "string",
                    "enum": ["once", "session"],
                    "default": "once"
                },
                "ttl_seconds": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 3600,
                    "default": 300
                }
            },
            "required": ["tool_name", "permission", "reason", "arguments"],
            "additionalProperties": false
        }),
        "set_default_cwd" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "default": "." }
            },
            "additionalProperties": false
        }),
        "view_image" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "minLength": 1 },
                "max_bytes": { "type": "integer", "minimum": 1024, "maximum": 10485760, "default": 5242880 },
                "max_width": { "type": "integer", "minimum": 1, "maximum": 10000, "default": 2000 },
                "max_height": { "type": "integer", "minimum": 1, "maximum": 10000, "default": 2000 },
                "auto_resize": { "type": "boolean", "default": true },
                "output": { "type": "string", "enum": ["mcp_image", "data_url"], "default": "mcp_image" }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
        _ => json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{
        exposed_tool_names, input_schema, list_tools_for_profile, tool_api_descriptor,
        MUTATING_TOOLS, READ_ONLY_TOOLS,
    };

    /// 能写能执行的工具，在任何工具集里都不能标成只读：客户端拿这个标注决定要不要
    /// 让用户确认（审查 D08）。
    #[test]
    fn no_profile_marks_a_writing_tool_read_only() {
        for profile in [
            "compact",
            "core",
            "advanced",
            "read-only",
            "compat-readonly-all",
        ] {
            for tool in list_tools_for_profile(profile) {
                let name = tool["name"].as_str().unwrap_or_default();
                if ["exec_command", "apply_patch", "write_stdin", "kill_session"].contains(&name) {
                    assert_eq!(
                        tool["annotations"]["readOnlyHint"],
                        serde_json::json!(false),
                        "{profile} 把 {name} 标成了只读"
                    );
                }
            }
        }
    }

    /// 老配置里的 compat-readonly-all 读进来是 advanced：工具一个不少，标注改回实话。
    /// 不换的话它会被当成认不出的值降成 core，升级后少一半工具。
    #[test]
    fn the_retired_compat_profile_loads_as_advanced() {
        let runtime: crate::workspace::RuntimeConfig =
            serde_json::from_value(serde_json::json!({ "tool_profile": "compat-readonly-all" }))
                .unwrap();
        assert_eq!(runtime.tool_profile, "advanced");
        let hub: crate::settings::HubConfig =
            serde_json::from_value(serde_json::json!({ "toolProfile": "compat-readonly-all" }))
                .unwrap();
        assert_eq!(hub.tool_profile, "advanced");

        let untouched: crate::workspace::RuntimeConfig =
            serde_json::from_value(serde_json::json!({ "tool_profile": "read-only" })).unwrap();
        assert_eq!(untouched.tool_profile, "read-only");
        let defaulted: crate::workspace::RuntimeConfig =
            serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(defaulted.tool_profile, "compact");
    }

    /// 只读 grant 拿到的就是 read-only 这一份（hub::read_only_allows）。它能列的只有自己起的命令，
    /// 而只读 grant 起不了命令，所以列出来总是空的；放进来是为了四档口径一致，不是给它开口子。
    #[test]
    fn list_runs_is_read_only_and_offered_wherever_read_output_is() {
        for profile in ["core", "compact", "read-only", "advanced"] {
            let names = exposed_tool_names(profile);
            assert_eq!(
                names.contains(&"read_output"),
                names.contains(&"list_runs"),
                "{profile}"
            );
        }
        assert!(READ_ONLY_TOOLS.contains(&"list_runs"));
        assert!(!MUTATING_TOOLS.contains(&"list_runs"));
    }

    #[test]
    fn core_catalog_excludes_non_persistent_permission_tool() {
        let tools = list_tools_for_profile("core");
        let names: Vec<_> = tools
            .iter()
            .map(|tool| tool["name"].as_str().expect("tool name"))
            .collect();
        let unique: HashSet<_> = names.iter().copied().collect();

        assert_eq!(tools.len(), 41);
        assert_eq!(unique.len(), tools.len());
        assert!(names.contains(&"history_manage"));
        assert!(names.contains(&"planning_manage"));
        assert!(names.contains(&"task_manage"));
        assert!(names.contains(&"history_session_bootstrap"));
        assert!(names.contains(&"history_session_checkpoint"));
        assert!(names.contains(&"history_session_validate"));
        assert!(names.contains(&"history_session_search"));
        assert!(names.contains(&"history_session_read"));
        assert!(names.contains(&"planning_state"));
        assert!(names.contains(&"create_goal"));
        assert!(names.contains(&"update_goal"));
        assert!(names.contains(&"create_plan"));
        assert!(names.contains(&"update_plan"));
        assert!(names.contains(&"request_goal_review"));
        assert!(names.contains(&"request_plan_review"));
        assert!(!names.contains(&"set_planning_mode"));
        assert!(names.contains(&"grep_text"));
        assert!(!names.contains(&"grep"));
        assert!(!names.contains(&"request_permissions"));

        for name in names {
            let schema = input_schema(name);
            assert_eq!(schema["type"], "object", "{name} schema type");
            assert!(schema["properties"].is_object(), "{name} properties");
            assert!(schema.get("oneOf").is_none(), "{name} oneOf");
            assert!(schema.get("anyOf").is_none(), "{name} anyOf");
            assert!(schema.get("$ref").is_none(), "{name} ref");
        }
    }

    #[test]
    fn compact_catalog_uses_stable_v2_aggregate_managers() {
        let tools = list_tools_for_profile("compact");
        let names: Vec<_> = tools
            .iter()
            .map(|tool| tool["name"].as_str().expect("tool name"))
            .collect();

        assert!(names.len() < 30);
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"apply_patch"));
        assert!(names.contains(&"exec_command"));
        assert!(names.contains(&"history_manage"));
        assert!(names.contains(&"planning_manage"));
        assert!(names.contains(&"task_manage"));
        assert!(!names.contains(&"history_session_search"));
        assert!(!names.contains(&"history_session_read"));
        assert!(!names.contains(&"planning_state"));
        // compact 以前把 Skill 整个砍掉了：项目把用法写进 skill，默认档下
        // 模型既看不见目录、也没有工具能问（RFC-0003 G2）。目录本身仍有
        // 字符预算，工具只有两个，代价很小。
        assert!(names.contains(&"list_skills"));
        assert!(names.contains(&"get_skill"));
        assert!(!names.contains(&"request_permissions"));
        assert_eq!(tool_api_descriptor()["version"], "2");
    }

    /// 少一个参数，工具个数不变，指纹得变——只对个数查不出旧表。
    #[test]
    fn surface_digest_lists_parameters_and_changes_when_one_goes_missing() {
        let tools = list_tools_for_profile("compact");
        let digest = super::surface_digest(&tools);
        assert_eq!(digest["tool_count"], tools.len());
        let read_file = digest["tools"]["read_file"].as_array().expect("read_file");
        assert!(read_file.contains(&serde_json::json!("start_byte")));

        let mut older = tools.clone();
        let read_file = older
            .iter_mut()
            .find(|tool| tool["name"] == "read_file")
            .expect("read_file");
        read_file["inputSchema"]["properties"]
            .as_object_mut()
            .expect("properties")
            .remove("start_byte");
        let stale = super::surface_digest(&older);
        assert_eq!(stale["tool_count"], digest["tool_count"]);
        assert_ne!(stale["tools_fingerprint"], digest["tools_fingerprint"]);
    }
}

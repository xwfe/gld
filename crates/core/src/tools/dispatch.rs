#![allow(clippy::items_after_test_module)]

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;

use serde_json::{json, Value};
use uuid::Uuid;

use crate::harness::model::{CommandOutcome, WorktreePosition};
use crate::harness::CommandEvidence;
use crate::planning::{
    CommandLedger, ExecutionLedgerUpdate, GoalStatus, PlanStatus, PlanningMode, PlanningService,
    PlanningState, PLANNING_RELATIVE_PATH,
};
use crate::tools::caller::Caller;
use crate::tools::context::ToolContext;
use crate::tools::policy::{validate_tool_arguments_for_workspace, PolicyError};
use crate::tools::workspace::{tool_err, tool_err_code, tool_ok, WorkspaceError};
use crate::tools::{
    exec, file, git, history, image_tool, manage, notebook, outcome, patch, planning, session,
    skill,
};

/// 策略拒绝变成工具响应。
///
/// 码、原因和下一步都由 `PolicyReason` 给，不再从消息里找关键词——预检
/// （`check_command`）走的是同一条判定，两边说的话必须一模一样（审查 F、A01）。
fn policy_tool_err(err: PolicyError) -> Value {
    tool_err(WorkspaceError::ToolDetails {
        code: err.reason.code(),
        message: err.message.clone(),
        category: "policy",
        retryable: false,
        details: json!({
            "stage": "policy",
            "reason": err.reason.slug(),
            "recoverable": !err.reason.needs_approval(),
            "needs_approval": err.reason.needs_approval(),
            "suggestion": err.reason.suggestion(),
            "preflight_tool": "check_command"
        }),
    })
}

/// 被拒的调用也记一笔。
///
/// 审查 F 要的是"策略/规划拒绝也记录脱敏审计事件"：一次没跑成的调用同样是
/// 发生过的事，事后查"模型那半小时到底在干什么"时，只有成功记录是拼不出来的。
///
/// 记的是 `kind="rejected"`，和正常路径的 `started` 分开——账本得能区分
/// "接了没跑"和"跑了"。
///
/// **脱敏**：只记错误码和拒绝阶段，不记命令内容、补丁正文。输入那一格沿用
/// `operation_input`（只有"有没有参数"和调用方自己写的 reason）。
///
/// 范围限定在本来就要记账的工具，加上会改东西的那些：读类工具被拒（几乎只有
/// Planning 那一种）不值得给 append-only 的日志添行。
fn record_rejection(
    ctx: &ToolContext,
    operation_id: &str,
    name: &str,
    args: &Value,
    output: &Value,
) {
    if !(should_log_operation(name) || mutating_tool_call(name, args)) {
        return;
    }
    let error = output.get("error");
    let field = |key: &str| {
        error
            .and_then(|error| error.get(key))
            .cloned()
            .unwrap_or(Value::Null)
    };
    let stage = error
        .and_then(|error| error.get("details"))
        .and_then(|details| details.get("stage"))
        .cloned()
        .unwrap_or(Value::Null);
    // 有没结束的任务就记在它名下：写前检查拒的恰恰是任务期间的调用，按任务翻操作记录
    // 得看得到它。读不出任务（比如任务文件坏了）就不挂，拒绝本身照记。
    let task_id = ctx.harness.active_task().ok().flatten().map(|task| task.id);
    let _ = ctx.harness.record_operation(
        Some(operation_id),
        task_id.as_deref(),
        name,
        "rejected",
        operation_input(args),
        json!({
            "ok": false,
            "code": field("code"),
            "category": field("category"),
            "stage": stage
        }),
    );
}

fn record_execution_ledger(
    ctx: &ToolContext,
    name: &str,
    args: &Value,
    output: &Value,
    tracked_task_id: Option<&str>,
) {
    if !mutating_tool_call(name, args)
        && !matches!(
            name,
            "start_task"
                | "update_task"
                | "pause_task"
                | "resume_task"
                | "finish_task"
                | "refresh_baseline"
        )
    {
        return;
    }
    // 按命令终态记，不按顶层 ok：退出 7 的命令 ok 也是 true（审查 D03）。
    let outcome = outcome::classify(output);
    let task_id = tracked_task_id
        .map(str::to_string)
        .or_else(|| {
            args.get("task_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| {
            output
                .get("task")
                .and_then(|task| task.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    let changed_files = output
        .get("affected_files")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.as_str()
                        .map(str::to_string)
                        .or_else(|| item.get("path").and_then(Value::as_str).map(str::to_string))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let is_checkpoint = name == "history_session_checkpoint"
        || (name == "history_manage"
            && args.get("action").and_then(Value::as_str) == Some("checkpoint"));
    let history_checkpoint_ref = is_checkpoint
        .then(|| {
            output
                .get("path")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .flatten();
    let verification = args
        .get("tests")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let last_tool = args
        .get("action")
        .and_then(Value::as_str)
        .map(|action| format!("{name}:{action}"))
        .unwrap_or_else(|| name.to_string());

    let _ = PlanningService::new(ctx.workspace.root()).record_execution(ExecutionLedgerUpdate {
        task_id,
        last_tool: Some(last_tool),
        state: Some(outcome.state.into()),
        call_ok: Some(outcome.call_ok),
        command: outcome.command.as_ref().map(command_ledger),
        last_error: outcome.error,
        changed_files,
        history_checkpoint_ref,
        verification,
    });
}

fn command_ledger(command: &CommandOutcome) -> CommandLedger {
    CommandLedger {
        session_id: command.session_id.clone(),
        status: command.status.clone(),
        exit_code: command.exit_code,
        command_ok: command.command_ok,
    }
}

/// read_output / write_stdin / kill_session 看到的命令状态补回两本账。
///
/// 命令转了后台，exec_command 那次记下的是 running；它后来怎么结束的，只有这几个
/// 工具看得见。不补的话，Planning 台账永远停在 running，任务也拿不到这条命令的
/// 终态当验收证据（审查 D01、D03）。
///
/// write_stdin / kill_session 是写操作，Planning 那本由 `record_execution_ledger`
/// 照常记；read_output 是读操作不进那里，单独补。
fn observe_command_session(ctx: &ToolContext, name: &str, output: &Value) {
    if !matches!(name, "read_output" | "write_stdin" | "kill_session") {
        return;
    }
    let outcome = outcome::classify(output);
    let Some(command) = outcome.command.as_ref() else {
        return;
    };
    if name == "read_output" {
        let _ = PlanningService::new(ctx.workspace.root()).record_command_observation(
            command_ledger(command),
            outcome.state,
            outcome.error.clone(),
        );
    }
    let _ = ctx.harness.observe_command(name, command, outcome.state);
}

/// 任务里一次写类调用结束：记事件（命令类带上验收证据），把 gld 自己的写入记上账。
///
/// `start` 是写前检查通过时任务记账的位置——检查保证工作区当时就是它。
fn record_tracked_operation(
    ctx: &ToolContext,
    task_id: &str,
    start: WorktreePosition,
    name: &str,
    args: &Value,
    output: &Value,
) {
    let outcome = outcome::classify(output);
    // 调用本身成了（命令哪怕退出非零）就说明是 gld 让它跑的，它写的东西记上账；
    // 调用都没成（参数、策略、起不来）就什么也没写。
    let refreshed = outcome
        .call_ok
        .then(|| ctx.harness.refresh_expected_state(task_id).ok())
        .flatten();
    let evidence = (name == "exec_command")
        .then_some(outcome.command.as_ref())
        .flatten()
        .filter(|command| command.session_id.is_some())
        .map(|command| {
            let finished = !command.is_running();
            let mut text = output
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .chars()
                .take(COMMAND_TEXT_CHARS)
                .collect::<String>();
            crate::tools::history::redact_text(&mut text);
            CommandEvidence {
                command: command.clone(),
                command_text: Some(text),
                start: Some(start),
                end: refreshed
                    .as_ref()
                    .filter(|_| finished)
                    .map(|refresh| refresh.position.clone()),
                changed_during_run: refreshed
                    .filter(|_| finished)
                    .map(|refresh| refresh.changed)
                    .unwrap_or_default(),
            }
        });
    let _ = ctx.harness.record_event(
        task_id,
        "operation_finished",
        Some(name),
        operation_input(args),
        json!({
            "ok": outcome.call_ok,
            "tool": name,
            "state": outcome.state,
            "evidence": evidence
        }),
    );
}

/// 验收证据里命令原文最多留多少字符。
const COMMAND_TEXT_CHARS: usize = 500;

fn capability_health_check(ctx: &ToolContext) -> Value {
    let tools = crate::tools::registry::exposed_tool_names(&ctx.tool_profile);
    let mut hasher = DefaultHasher::new();
    tools.hash(&mut hasher);
    json!({
        "authentication": {
            "status": "available"
        },
        "authorization": {
            "mode": ctx.permission_mode
        },
        "workspace": {
            "path": ctx.workspace.root().display().to_string(),
            "status": "available"
        },
        "capability": {
            "server_tool_count": tools.len(),
            "tool_profile": ctx.tool_profile,
            "tool_fingerprint": format!("{:x}", hasher.finish()),
            "server_version": env!("CARGO_PKG_VERSION"),
            "tool_api": crate::tools::registry::tool_api_descriptor()
        },
        "recommendation": "If client tools are missing while server capability is healthy, refresh MCP tool discovery instead of requesting permissions."
    })
}

fn mutating_tool_call(name: &str, args: &Value) -> bool {
    manage::action_is_mutating(name, args)
        .unwrap_or_else(|| crate::tools::registry::MUTATING_TOOLS.contains(&name))
}

fn planning_protected_tool(name: &str, args: &Value) -> bool {
    const EXEMPT: &[&str] = &[
        "history_session_bootstrap",
        "history_session_checkpoint",
        "history_session_validate",
        "create_goal",
        "update_goal",
        "create_plan",
        "update_plan",
        "kill_session",
        "set_default_cwd",
    ];
    if name == "history_manage" || name == "planning_manage" {
        return false;
    }
    mutating_tool_call(name, args) && !EXEMPT.contains(&name)
}

fn plan_mode_blocks_tool(name: &str, args: &Value) -> bool {
    planning_protected_tool(name, args) || name == "exec_health_check"
}

fn load_planning_state(ctx: &ToolContext) -> Result<PlanningState, Value> {
    PlanningService::new(ctx.workspace.root())
        .state()
        .map_err(|error| {
            tool_err(WorkspaceError::ToolDetails {
                code: "PLANNING_STATE_UNAVAILABLE",
                message: format!("Cannot read project planning state: {error}"),
                category: "storage",
                retryable: false,
                details: json!({
                    "storage_path": PLANNING_RELATIVE_PATH,
                    "fail_closed_for_mutations": true
                }),
            })
        })
}

fn planning_gate(state: &PlanningState, name: &str, args: &Value) -> Option<Value> {
    if !planning_protected_tool(name, args) && state.mode != PlanningMode::Plan {
        return None;
    }
    match state.mode {
        PlanningMode::Direct => None,
        PlanningMode::Plan if plan_mode_blocks_tool(name, args) => {
            Some(tool_err(WorkspaceError::ToolDetails {
                code: "PLAN_MODE_READ_ONLY",
                message: format!("{name} is disabled while this workspace is in Plan mode"),
                category: "permission",
                retryable: false,
                details: json!({
                    "mode": "plan",
                    "revision": state.revision,
                    "suggestion": "Use read/planning tools, or switch the workspace to Goal/Direct mode from the gld CLI."
                }),
            }))
        }
        PlanningMode::Plan => None,
        PlanningMode::Goal => goal_mode_gate(state, name),
    }
}

fn goal_mode_gate(state: &PlanningState, name: &str) -> Option<Value> {
    let Some(goal_id) = state.focus_goal_id.as_deref() else {
        return Some(planning_permission_error(
            "GOAL_CONTEXT_REQUIRED",
            format!("{name} requires an active Goal while Goal mode is enabled"),
            state,
            "Select an active Goal from the gld CLI before modifying the project.",
        ));
    };
    let Some(goal) = state.goals.iter().find(|goal| goal.id == goal_id) else {
        return Some(planning_permission_error(
            "GOAL_CONTEXT_INVALID",
            format!("Focused Goal {goal_id} no longer exists"),
            state,
            "Select another Goal from the gld CLI.",
        ));
    };
    if goal.status != GoalStatus::Active {
        return Some(planning_permission_error(
            "GOAL_NOT_ACTIVE",
            format!("Focused Goal '{}' is {:?}", goal.title, goal.status),
            state,
            "Resume/select an active Goal from the gld CLI before modifying the project.",
        ));
    }
    if let Some(plan_id) = state.focus_plan_id.as_deref() {
        let Some(plan) = state.plans.iter().find(|plan| plan.id == plan_id) else {
            return Some(planning_permission_error(
                "PLAN_CONTEXT_INVALID",
                format!("Focused Plan {plan_id} no longer exists"),
                state,
                "Select another Plan from the gld CLI.",
            ));
        };
        if plan.goal_id.as_deref() != Some(goal_id) {
            return Some(planning_permission_error(
                "PLAN_GOAL_MISMATCH",
                "Focused Plan does not belong to the focused Goal".into(),
                state,
                "Select a Plan linked to the active Goal.",
            ));
        }
        if !matches!(plan.status, PlanStatus::Active | PlanStatus::Draft) {
            return Some(planning_permission_error(
                "PLAN_NOT_EXECUTABLE",
                format!("Focused Plan '{}' is {:?}", plan.title, plan.status),
                state,
                "Activate the Plan or clear the focused Plan before modifying the project.",
            ));
        }
    }
    None
}

fn planning_permission_error(
    code: &'static str,
    message: String,
    state: &PlanningState,
    suggestion: &str,
) -> Value {
    tool_err(WorkspaceError::ToolDetails {
        code,
        message,
        category: "permission",
        retryable: false,
        details: json!({
            "mode": state.mode,
            "revision": state.revision,
            "focus_goal_id": state.focus_goal_id,
            "focus_plan_id": state.focus_plan_id,
            "suggestion": suggestion
        }),
    })
}

/// **唯一工具执行入口**。MCP `tools/call` 与 Actions `POST /actions/{tool}` 必须且只能调用此函数。
/// 策略校验、分发、错误格式在此统一，两路传输层不得另做执行前校验（Actions 仅允许额外的暴露层 `validate_actions_exposure`）。
///
/// `operation_id` 在**进门时**就分配，每一条返回路径都带着它走。以前它是
/// 执行到一半才由 `record_operation` 生成的，于是策略拒绝、Planning 拒绝、
/// 基线拒绝这些提前 return 的响应根本没有 id——出了问题，人拿着模型给的
/// 报错在日志里对不上号（审查 D02、F）。
/// 本机操作员发起的调用：命令行 `gld tool call`、守护进程内部调用、测试。
/// 网络进来的调用必须走 [`call_tool_as`]，把连接的主体带上。
pub fn call_tool(ctx: &ToolContext, name: &str, args: &Value) -> Value {
    call_tool_as(ctx, &Caller::local(), name, args)
}

/// 同上，但由调用方说明这次是**谁**在调。
///
/// 主体现在只影响一件事：拿到哪张命令会话表（`exec_command` 起的命令、
/// `read_output` / `write_stdin` / `kill_session` 认的那些 `session_id`）。
/// 换个主体就是另一张表，互相看不见。为什么要分、分得开哪些，见
/// [`crate::tools::caller`]。
pub fn call_tool_as(ctx: &ToolContext, caller: &Caller, name: &str, args: &Value) -> Value {
    let operation_id = Uuid::new_v4().simple().to_string();
    let mut output = dispatch_tool(ctx, caller, name, args, &operation_id);
    if let Some(object) = output.as_object_mut() {
        // 内层已经写进去的就是这一个（`record_operation` 收的就是它），
        // 这里只负责补上那些没走到记账就返回的路径。
        object
            .entry("operation_id")
            .or_insert_with(|| Value::String(operation_id.clone()));
    }
    output
}

fn dispatch_tool(
    ctx: &ToolContext,
    caller: &Caller,
    name: &str,
    args: &Value,
    operation_id: &str,
) -> Value {
    let effective_args = apply_default_cwd(ctx, name, args);
    let planning_state = match load_planning_state(ctx) {
        Ok(state) => Some(state),
        Err(error)
            if planning_protected_tool(name, &effective_args) || name == "exec_health_check" =>
        {
            record_rejection(ctx, operation_id, name, &effective_args, &error);
            return error;
        }
        Err(_) => None,
    };
    if let Some(state) = planning_state.as_ref() {
        if let Some(error) = planning_gate(state, name, &effective_args) {
            record_rejection(ctx, operation_id, name, &effective_args, &error);
            return attach_planning_context(error, state);
        }
    }
    if let Err(e) = validate_tool_arguments_for_workspace(
        name,
        &effective_args,
        &ctx.policy,
        Some(&ctx.workspace),
    ) {
        let output = policy_tool_err(e);
        record_rejection(ctx, operation_id, name, &effective_args, &output);
        return planning_state
            .as_ref()
            .map(|state| attach_planning_context(output.clone(), state))
            .unwrap_or(output);
    }
    // 名字对不上的参数不能悄悄丢掉（审查 A19）。
    //
    // 放在策略之后：策略对某几个名字有**自己的说法**，比"没有这个参数"有用
    // 得多——`env` 不是拼错了，是服务端不让调用方设环境变量。两道门都过不去
    // 的调用，先报更能指导下一步的那个。
    if let Err(e) = crate::tools::args::reject_unknown(name, &effective_args) {
        let output = tool_err(e);
        record_rejection(ctx, operation_id, name, &effective_args, &output);
        return planning_state
            .as_ref()
            .map(|state| attach_planning_context(output.clone(), state))
            .unwrap_or(output);
    }

    if crate::harness::tools::TOOL_NAMES.contains(&name) {
        let output = match crate::harness::tools::call(ctx, name, args) {
            Ok(value) => value,
            Err(error) => attach_harness_status(ctx, tool_err(error), false),
        };
        record_execution_ledger(ctx, name, args, &output, None);
        return planning_state
            .as_ref()
            .map(|state| attach_planning_context(output.clone(), state))
            .unwrap_or(output);
    }

    let tracked = if requires_write_baseline(name, &effective_args) {
        // 读不出任务就不写。以前读失败当成"没有任务"放行，任务文件一坏，写前检查就
        // 悄悄没了（审查 D11）。
        let checked = ctx.harness.current_task().and_then(|task| {
            if let Some(task) = &task {
                ctx.harness.check_baseline(&task.id)?;
            }
            Ok(task)
        });
        let task = match checked {
            Ok(task) => task,
            Err(error) => {
                let output = attach_harness_status(
                    ctx,
                    tool_err_code(error.code(), error.to_string(), "permission"),
                    false,
                );
                record_rejection(ctx, operation_id, name, &effective_args, &output);
                return output;
            }
        };
        if let Some(task) = task {
            let _ = ctx.harness.record_event(
                &task.id,
                "operation_started",
                Some(name),
                operation_input(args),
                json!({"ok": true, "tracking": "task"}),
            );
            let start = ctx.harness.expected_position(&task);
            Some((task.id, start))
        } else {
            None
        }
    } else {
        None
    };
    let task_id = tracked.as_ref().map(|(id, _)| id.clone());

    let operation = if should_log_operation(name) {
        ctx.harness
            .record_operation(
                Some(operation_id),
                task_id.as_deref(),
                name,
                "started",
                json!({"arguments_present": !args.is_null()}),
                json!({"ok": true}),
            )
            .ok()
    } else {
        None
    };

    let ws = &ctx.workspace;
    let result = match name {
        "history_manage" => manage::history_manage(ctx, &effective_args),
        "planning_manage" => manage::planning_manage(ctx, &effective_args),
        "task_manage" => manage::task_manage(ctx, &effective_args),
        "history_session_bootstrap" => history::bootstrap(ctx, &effective_args),
        "history_session_checkpoint" => history::checkpoint(ctx, &effective_args),
        "history_session_validate" => history::validate(ctx, &effective_args),
        "history_session_search" => history::search(ctx, &effective_args),
        "history_session_read" => history::read(ctx, &effective_args),
        "capability_health_check" => Ok(capability_health_check(ctx)),
        "planning_state" => planning::planning_state(ctx, &effective_args),
        "create_goal" => planning::create_goal(ctx, &effective_args),
        "update_goal" => planning::update_goal(ctx, &effective_args),
        "create_plan" => planning::create_plan(ctx, &effective_args),
        "update_plan" => planning::update_plan(ctx, &effective_args),
        "request_goal_review" => planning::request_goal_review(ctx, &effective_args),
        "request_plan_review" => planning::request_plan_review(ctx, &effective_args),
        "server_info" => server_info(ctx),
        "check_exec_environment" => check_exec_environment(ctx),
        "check_command" => exec::check_command(ctx, &effective_args),
        "exec_health_check" => exec::exec_health_check(ctx, &ctx.runtime.sessions_for(caller)),
        "get_default_cwd" => get_default_cwd(ctx),
        "set_default_cwd" => set_default_cwd(ctx, &effective_args),
        "list_skills" => skill::list_skills(ctx, &effective_args),
        "get_skill" => skill::get_skill(ctx, &effective_args),
        "read_file" => file::read_file(ws, &effective_args),
        "read_notebook" => notebook::read_notebook(ws, &effective_args),
        "list_dir" => file::list_dir(ws, &effective_args),
        "list_files" => file::list_files(ws, &effective_args),
        "search_text" | "grep_text" | "grep" => file::search_text(ws, &effective_args),
        "patch_check" => patch::patch_check(ctx, caller, &effective_args),
        "apply_patch" => patch::apply_patch(ctx, caller, &effective_args),
        // 这四个认 `session_id`，所以都得先问"你是谁"：会话表按目录 + 主体
        // 分，别人的 id 在这张表里查无此人。
        "exec_command" => {
            exec::exec_command(ctx, &ctx.runtime.sessions_for(caller), &effective_args)
        }
        "read_output" => session::read_output(&ctx.runtime.sessions_for(caller), &effective_args),
        // 只列这个主体自己的记录：和另外四个用同一张按目录 + 主体分的表。
        "list_runs" => session::list_runs(&ctx.runtime.sessions_for(caller), &effective_args),
        "write_stdin" => session::write_stdin(&ctx.runtime.sessions_for(caller), &effective_args),
        "kill_session" => session::kill_session(&ctx.runtime.sessions_for(caller), &effective_args),
        "git_status" => git::git_status(ws, &effective_args),
        "git_diff" => git::git_diff(ws, &effective_args),
        "git_log" => git::git_log(ws, &effective_args),
        "git_show" => git::git_show(ws, &effective_args),
        "git_blame" => git::git_blame(ws, &effective_args),
        "view_image" => image_tool::view_image(ws, &effective_args),
        // 不管什么模式都不发授权：服务端没有能记下"用户批准了"的地方。dangerous 模式
        // 以前回 granted、说"需要许可的操作都自动放行"，实际确认门一个都没放（审查 D08）。
        "request_permissions" => Ok(tool_ok(json!({
            "ok": false,
            "status": "unsupported",
            "grant_id": null,
            "expires_at": null,
            "next_actions": [
                "Do not retry request_permissions.",
                "If the original operation returned DANGEROUS_OPERATION_REQUIRES_CONFIRMATION and the user already explicitly authorized it, retry the original tool with confirm=true."
            ],
            "error": {
                "code": "ELICITATION_UNSUPPORTED",
                "message": "Permission elicitation is not available for this client. Do not retry request_permissions; it cannot create a persistent grant.",
                "category": "permission",
                "retryable": false,
                "details": { "requested": effective_args }
            }
        }))),
        _ => {
            let mut output = tool_err_code(
                "INVALID_ARGUMENT",
                format!("Unknown tool: {name}"),
                "validation",
            );
            if let Some(object) = output.as_object_mut() {
                object.insert(
                    "recovery".into(),
                    json!({
                        "type": "capability_discovery_check",
                        "message": "If this tool exists on the server but is missing in the client session, refresh MCP tool discovery instead of requesting permissions.",
                        "next_action": "Call capability_health_check and compare the available tool list before retrying."
                    }),
                );
            }
            return planning_state
                .as_ref()
                .map(|state| attach_planning_context(output.clone(), state))
                .unwrap_or(output);
        }
    };
    let mut output = match result {
        Ok(v) => v,
        Err(e) => tool_err(e),
    };
    if task_id.is_none()
        && standalone_operation(name)
        && output.get("ok") == Some(&Value::Bool(true))
    {
        attach_standalone_metadata(
            &mut output,
            "当前操作已在 standalone 模式完成；如需继续，直接调用下一个开发工具。",
        );
    }
    if let Some(operation) = operation.as_ref() {
        if let Some(object) = output.as_object_mut() {
            object.insert("operation_id".into(), Value::String(operation.id.clone()));
        }
    }
    if output.get("ok").and_then(Value::as_bool) == Some(false) {
        output = attach_harness_status(ctx, output, task_id.is_none());
        output = attach_recovery_guidance(output);
    }
    if let Some((task_id, start)) = tracked {
        record_tracked_operation(ctx, &task_id, start, name, args, &output);
    } else if writes_workspace_archives(name)
        && output.get("ok").and_then(Value::as_bool) == Some(true)
    {
        // History 档案写在项目里（`docs/history-session/`），是用户会提交的内容，
        // 不能像 `.gld/` 那样整体排除出指纹。但它是**工具自己写的**，写完必须记上账，
        // 否则下一次 exec_command / apply_patch 会把它当成外部修改而拒绝执行——
        // 症状是"存了个检查点，然后就什么都干不了了"。
        if let Ok(Some(task)) = ctx.harness.current_task() {
            let _ = ctx.harness.refresh_expected_state(&task.id);
        }
    }
    if let Some(operation) = operation {
        let outcome = outcome::classify(&output);
        let _ = ctx.harness.record_operation(
            Some(&operation.id),
            task_id.as_deref(),
            name,
            outcome.state,
            operation_input(args),
            json!({
                "ok": outcome.call_ok,
                "tool": name,
                "command": outcome.command,
                // 只记码不记消息，和 record_rejection 一样：消息里可能带文件内容。
                "error_code": output.pointer("/error/code"),
                "affected_files": output.get("affected_files")
            }),
        );
    }
    record_execution_ledger(ctx, name, &effective_args, &output, task_id.as_deref());
    observe_command_session(ctx, name, &output);
    if should_attach_planning_context(ctx, name, &output) {
        if let Ok(latest) = PlanningService::new(ctx.workspace.root()).state() {
            output = attach_planning_context(output, &latest);
            if let Some(planning) = output.get("planning_context") {
                ctx.record_context_block("planning_status", planning);
            }
        }
    }
    output
}

fn should_attach_planning_context(ctx: &ToolContext, name: &str, output: &Value) -> bool {
    if ctx.tool_profile != "compact" {
        return true;
    }
    let planning_tool = matches!(
        name,
        "planning_manage"
            | "planning_state"
            | "create_goal"
            | "update_goal"
            | "create_plan"
            | "update_plan"
            | "request_goal_review"
            | "request_plan_review"
    );
    planning_tool || output.get("ok").and_then(Value::as_bool) == Some(false)
}

fn attach_recovery_guidance(mut output: Value) -> Value {
    let Some(error) = output.get("error") else {
        return output;
    };
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let code = error
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or_default();

    let needs_capability_recovery =
        code == "INVALID_ARGUMENT" && message.starts_with("Unknown tool:");

    if needs_capability_recovery {
        if let Some(object) = output.as_object_mut() {
            object.insert(
                "recovery".into(),
                json!({
                    "type": "capability_discovery_mismatch",
                    "automatic_action": "refresh_tool_discovery",
                    "retry_recommended": true,
                    "user_message": "MCP capability is temporarily out of sync. Refreshing available tools is recommended before retrying."
                }),
            );
        }
    }

    output
}

fn attach_planning_context(mut output: Value, state: &PlanningState) -> Value {
    let goal = state
        .focus_goal_id
        .as_deref()
        .and_then(|id| state.goals.iter().find(|goal| goal.id == id))
        .map(|goal| {
            let completed = goal
                .success_criteria
                .iter()
                .filter(|item| item.completed)
                .count();
            json!({
                "id": goal.id,
                "title": goal.title,
                "status": goal.status,
                "criteria_completed": completed,
                "criteria_total": goal.success_criteria.len()
            })
        });
    let plan = state
        .focus_plan_id
        .as_deref()
        .and_then(|id| state.plans.iter().find(|plan| plan.id == id))
        .map(|plan| {
            let completed = plan
                .steps
                .iter()
                .filter(|step| step.status == crate::planning::PlanStepStatus::Completed)
                .count();
            json!({
                "id": plan.id,
                "title": plan.title,
                "status": plan.status,
                "revision": plan.revision,
                "steps_completed": completed,
                "steps_total": plan.steps.len()
            })
        });
    if let Some(object) = output.as_object_mut() {
        object.insert(
            "planning_context".into(),
            json!({
                "mode": state.mode,
                "revision": state.revision,
                "storage_path": PLANNING_RELATIVE_PATH,
                "goal": goal,
                "plan": plan
            }),
        );
    }
    output
}

fn apply_default_cwd(ctx: &ToolContext, name: &str, args: &Value) -> Value {
    let base = if ctx.default_cwd_path() == ctx.workspace.root() {
        ".".to_string()
    } else {
        ctx.default_cwd_display()
    };
    if base == "." {
        return args.clone();
    }

    let mut effective = args.clone();
    match name {
        // 预检必须和真实执行看见同一个 workdir，否则"预检说能跑、真跑被拒"。
        "exec_command" | "check_command"
            if effective.get("workdir").is_none() && effective.get("cwd").is_none() =>
        {
            effective["workdir"] = Value::String(base.clone());
        }
        "list_dir" | "list_files" | "git_status" | "git_log" => {
            let path = effective.get("path").and_then(Value::as_str).unwrap_or(".");
            effective["path"] = Value::String(prefix_relative_path(&base, path));
        }
        "read_file" | "read_notebook" | "search_text" | "grep_text" | "grep" | "git_blame"
        | "view_image" => {
            if let Some(path) = effective.get("path").and_then(Value::as_str) {
                effective["path"] = Value::String(prefix_relative_path(&base, path));
            }
        }
        "git_diff" => {
            if let Some(path) = effective.get("path").and_then(Value::as_str) {
                effective["path"] = Value::String(prefix_relative_path(&base, path));
            }
            if let Some(paths) = effective.get("paths").and_then(Value::as_array).cloned() {
                effective["paths"] = Value::Array(
                    paths
                        .iter()
                        .map(|path| {
                            path.as_str()
                                .map(|value| Value::String(prefix_relative_path(&base, value)))
                                .unwrap_or_else(|| path.clone())
                        })
                        .collect(),
                );
            }
        }
        "apply_patch" | "patch_check" => {
            if let Some(patch) = effective.get("patch").and_then(Value::as_str) {
                effective["patch"] = Value::String(prefix_patch_paths(&base, patch));
            }
        }
        _ => {}
    }
    effective
}

fn prefix_relative_path(base: &str, path: &str) -> String {
    if path == "." || path.is_empty() {
        return base.to_string();
    }
    if Path::new(path).is_absolute() || path.starts_with("..") {
        return path.to_string();
    }
    format!("{base}/{}", path.trim_start_matches("./"))
}

fn prefix_patch_paths(base: &str, patch: &str) -> String {
    patch
        .lines()
        .map(|line| {
            for marker in ["--- a/", "+++ b/"] {
                if let Some(path) = line.strip_prefix(marker) {
                    return format!("{marker}{base}/{path}");
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 这些工具会往工作区里写自己的档案，写完要把指纹记上账。
///
/// 和 `requires_write_baseline` 的区别：那批（exec_command / apply_patch）是替
/// 用户改代码，动手前要先确认工作区没被外部动过；这批是工具自己的记账动作，
/// 不该被基线拦住，但它们确实改了工作区文件，所以事后必须刷新。
///
/// Planning 不在这里：它的状态在 `.gld/` 下，那个目录整个不进指纹
/// （见 `harness::scan::is_skipped_dir`）。
///
/// 代价说清楚：如果用户正好在这次调用之前手工改了别的文件，这次刷新会把那笔
/// 变化一起吸收掉，后面就不再报 FILE_CHANGED_EXTERNALLY 了。相比"存个检查点就
/// 把自己锁死"，这个代价值得。
fn writes_workspace_archives(name: &str) -> bool {
    name.starts_with("history_")
}

fn requires_write_baseline(name: &str, args: &Value) -> bool {
    match name {
        "exec_command" => true,
        "apply_patch" => !args
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        _ => false,
    }
}

fn standalone_operation(name: &str) -> bool {
    matches!(name, "patch_check" | "apply_patch" | "exec_command")
}

fn should_log_operation(name: &str) -> bool {
    standalone_operation(name)
        || matches!(
            name,
            "git_status" | "git_diff" | "git_log" | "git_show" | "git_blame"
        )
}

fn operation_input(args: &Value) -> Value {
    json!({
        "arguments_present": !args.is_null(),
        "reason": args.get("reason")
    })
}

fn attach_harness_status(ctx: &ToolContext, mut output: Value, standalone: bool) -> Value {
    if let Ok(mut status) = ctx.harness.status() {
        if standalone && status.task_id.is_none() {
            status.next_actions.clear();
        }
        status.next_actions = filter_exposed_actions(ctx, status.next_actions);
        if let Some(object) = output.as_object_mut() {
            object.insert(
                "harness".into(),
                serde_json::to_value(status).unwrap_or_else(|_| {
                    json!({
                        "status": "unavailable",
                        "reason": "无法序列化 Harness 状态"
                    })
                }),
            );
            if standalone {
                let hint = standalone_recovery_hint(&output);
                attach_standalone_metadata(&mut output, hint);
            }
        }
    }
    output
}

/// 下一步该干什么，按**这次是怎么失败的**说。
///
/// 原来一律是"请检查 stderr、exit_code 或调整参数后重试"——补丁上下文对不上
/// 时也这么说，而那次根本没有 stderr，也没有 exit_code，更不该"重试"：同一个
/// 补丁再发一遍还是对不上（审查 D01、A18）。
fn standalone_recovery_hint(output: &Value) -> &'static str {
    let code = output
        .get("error")
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    match code {
        "PATCH_FAILED" | "PATCH_AMBIGUOUS" | "NOT_FOUND" => {
            "补丁没有落盘，工作区没有变化；按 error.details.diagnostics 里的 suggested_read_range 重读这些文件，照它们现在的样子重建对不上的那几段再提交。"
        }
        "FILE_VERSION_CONFLICT" => {
            "文件在你读它之后被写过，补丁没有落盘，工作区没有变化；重新 read_file 看现在是什么样，照新内容重建补丁，再把它回的 version 放进 expected_versions。不要原样重发——那正是会盖掉别人改动的那一次。"
        }
        "PATCH_ROLLBACK_INCOMPLETE" => {
            "补丁写到一半失败，而且回滚没做完；先看 error.message 点名的那几个文件现在是什么内容，确认现场之后再决定怎么办，不要直接重发。"
        }
        "POLICY_REJECTED" | "PROTECTED_REPOSITORY_ASSET" | "EXTERNAL_EXECUTION_NOT_ALLOWED"
        | "COMMAND_REJECTED" | "EXECUTABLE_OUTSIDE_WORKSPACE" => {
            "策略拒绝，命令没有启动，也就没有 stderr 和 exit_code；用 check_command 看是哪条规则拒的、有没有已获准的替代做法，需要放开时请用户改配置。"
        }
        "DANGEROUS_OPERATION_REQUIRES_CONFIRMATION" => {
            "这一步需要用户明确授权；拿到授权后带 confirm=true 重试，不要自己绕过。"
        }
        "TIMEOUT" => {
            "命令超时，但它可能已经改了东西；先用 read_output 读已经产生的输出、确认做到哪一步，再决定要不要重跑。"
        }
        _ => "命令未成功；请检查 stderr、exit_code 或调整参数后重试。",
    }
}

fn attach_standalone_metadata(output: &mut Value, recovery_hint: &str) {
    if let Some(object) = output.as_object_mut() {
        object.insert("harness_mode".into(), Value::String("standalone".into()));
        object.insert("task_required".into(), Value::Bool(false));
        object.insert("next_actions".into(), json!([]));
        object.insert(
            "recovery_hint".into(),
            Value::String(recovery_hint.to_string()),
        );
    }
}

/// 只留客户端这一档调得到的下一步。Harness 工具没单独暴露、但有 task_manage 时，
/// 换成 `task_manage:<action>`——以前直接滤掉，compact 档下基线不符时连恢复入口
/// `refresh_baseline` 都不提（审查 D02）。
fn filter_exposed_actions(ctx: &ToolContext, actions: Vec<String>) -> Vec<String> {
    let exposed = crate::tools::registry::exposed_tool_names(&ctx.tool_profile);
    let has_task_manage = exposed.contains(&"task_manage");
    actions
        .into_iter()
        .filter_map(|action| {
            if exposed.contains(&action.as_str()) {
                return Some(action);
            }
            let task_action = manage::task_action_for_tool(&action).filter(|_| has_task_manage)?;
            Some(format!("task_manage:{task_action}"))
        })
        .collect()
}

pub fn server_info(ctx: &ToolContext) -> Result<Value, WorkspaceError> {
    let tools = crate::tools::registry::exposed_tool_names(&ctx.tool_profile);
    let history_context = crate::tools::history::context_snapshot(ctx).ok().flatten();
    let build = exec::server_snapshot();
    Ok(tool_ok(json!({
        "server": ctx.server_name(),
        "title": ctx.server_title(),
        "version": env!("CARGO_PKG_VERSION"),
        // 版本号相同的两次构建可以差几十个提交；核对"跑的是不是这份代码"看这两格。
        // 以前只有 check_command 报它们，而客户端缓存了旧工具表时恰恰看不见
        // check_command（审查 D04）；server_info 哪个版本的客户端都有。
        "build_commit": build["build_commit"],
        "shared_crates": build["shared_crates"],
        "protocol_version": "2025-06-18",
        "workspace": ctx.workspace.root_display(),
        "permission_mode": ctx.permission_mode,
        "default_cwd": ctx.default_cwd_display(),
        "network_allowed": ctx.policy.network_allowed(),
        "tool_profile": ctx.tool_profile,
        "history_recording": ctx.history_recording,
        "history_context_sessions": ctx.history_context_sessions,
        "history_context_revision": history_context
            .as_ref()
            .and_then(|value| value.get("context_revision")),
        "context_audit": ctx.context_audit_snapshot(),
        "auth_enabled": ctx.auth.auth_enabled(),
        "auth_type": ctx.auth.auth_type,
        "endpoint_path": "/mcp",
        "tool_api": crate::tools::registry::tool_api_descriptor(),
        "tools": tools,
        "tool_count": tools.len()
    })))
}

#[cfg(test)]
mod planning_tests {
    use tempfile::tempdir;

    use super::*;

    fn context() -> (tempfile::TempDir, tempfile::TempDir, ToolContext) {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");
        (workspace, harness, ctx)
    }

    #[test]
    fn plan_mode_blocks_project_mutation_but_keeps_kill_session_available() {
        let (_workspace, _harness, ctx) = context();
        PlanningService::new(ctx.workspace.root())
            .set_mode(PlanningMode::Plan)
            .expect("plan mode");
        let state = PlanningService::new(ctx.workspace.root())
            .state()
            .expect("state");

        let blocked = planning_gate(&state, "apply_patch", &json!({})).expect("blocked");
        assert_eq!(blocked["error"]["code"], "PLAN_MODE_READ_ONLY");
        assert!(planning_gate(&state, "exec_command", &json!({})).is_some());
        assert!(planning_gate(&state, "kill_session", &json!({})).is_none());
    }

    #[test]
    fn goal_mode_requires_an_active_focused_goal() {
        let (_workspace, _harness, ctx) = context();
        let service = PlanningService::new(ctx.workspace.root());
        service.set_mode(PlanningMode::Goal).expect("goal mode");
        let state = service.state().expect("state");
        let blocked = planning_gate(&state, "apply_patch", &json!({})).expect("blocked");
        assert_eq!(blocked["error"]["code"], "GOAL_CONTEXT_REQUIRED");

        service
            .create_goal("Goal", "Objective", Vec::new(), Vec::new())
            .expect("goal");
        let state = service.state().expect("state");
        assert!(planning_gate(&state, "apply_patch", &json!({})).is_none());
    }

    #[test]
    fn goal_mode_blocks_mutation_while_goal_waits_for_human_acceptance() {
        let (_workspace, _harness, ctx) = context();
        let service = PlanningService::new(ctx.workspace.root());
        service.set_mode(PlanningMode::Goal).expect("goal mode");
        let goal = service
            .create_goal("Goal", "Objective", Vec::new(), Vec::new())
            .expect("goal");
        service
            .request_goal_review(&goal.id, "Ready for human acceptance")
            .expect("request review");

        let state = service.state().expect("state");
        let blocked = planning_gate(&state, "apply_patch", &json!({})).expect("blocked");
        assert_eq!(blocked["error"]["code"], "GOAL_NOT_ACTIVE");
    }

    #[test]
    fn plan_mode_allows_read_only_v2_actions_and_blocks_task_mutation() {
        let (_workspace, _harness, ctx) = context();
        let service = PlanningService::new(ctx.workspace.root());
        service.set_mode(PlanningMode::Plan).expect("plan mode");
        let state = service.state().expect("state");

        assert!(planning_gate(&state, "planning_manage", &json!({"action":"state"})).is_none());
        assert!(planning_gate(&state, "task_manage", &json!({"action":"context"})).is_none());
        assert!(planning_gate(&state, "task_manage", &json!({"action":"start"})).is_some());
    }

    #[test]
    fn every_normal_tool_response_contains_current_planning_context() {
        let (_workspace, _harness, ctx) = context();
        let service = PlanningService::new(ctx.workspace.root());
        service.set_mode(PlanningMode::Plan).expect("plan mode");

        let output = call_tool(&ctx, "server_info", &json!({}));
        assert_eq!(output["planning_context"]["mode"], "plan");
        assert!(output["planning_context"]["revision"].as_u64().is_some());
    }

    #[test]
    fn compact_normal_tool_response_omits_repeated_planning_context() {
        let (_workspace, _harness, ctx) = context();
        let ctx = ctx.with_tool_profile("compact");
        let output = call_tool(&ctx, "server_info", &json!({}));
        assert!(output.get("planning_context").is_none());
        assert!(output["context_audit"]["blocks"].is_array());
    }
}

/// 这个工作区的执行环境长什么样。
///
/// **权威的那一份策略在 `policy` 里**，和 `check_command` 报的是同一个快照
/// （`exec::policy_snapshot`），不是另算一遍。审查 A 要的就是这个：能力状态
/// 只能有一个来源，两处各写各的迟早说出两套话，而模型没办法知道该信哪个。
///
/// 顶层那些老字段留着不动（有客户端在读），但值全部从同一个快照里取，
/// 有测试钉住它们和 `check_command` 逐字相等。
///
/// 这个工具回答的是"这个工作区整体是什么情况"；要问**某一条具体命令**能不能
/// 跑，用 `check_command`——它走的是真实执行那条判定，能说出是哪条规则、
/// 程序在不在、有什么替代做法。
pub fn check_exec_environment(ctx: &ToolContext) -> Result<Value, WorkspaceError> {
    let policy = crate::tools::exec::policy_snapshot(ctx);
    Ok(tool_ok(json!({
        "workspace": ctx.workspace.root_display(),
        "policy": policy,
        "preflight": {
            "tool": "check_command",
            "note": "要问某一条命令能不能跑，调 check_command：判定和 exec_command 走同一条路径，而且不会启动任何进程"
        },
        "permission_mode": policy["permission_mode"],
        "network_allowed": policy["network_allowed"],
        "landlock_enabled": false,
        "filesystem_sandbox": {
            "available": false,
            "enforced": false,
            "default_scope": "workspace",
            "host_scope_available": false
        },
        // 写入永远只在 Workspace 内：resolve_for_write 一律 join 到工作区根目录
        // 再校验，它根本拿不到 permission_mode。这里以前在 dangerous 模式下报
        // "allowed"，等于告诉模型可以往 /tmp 写——它照做只会拿到
        // ABSOLUTE_PATH_DENIED，然后反复重试。宁可少给能力，也不能给假的。
        "global_tmp_write": "denied",
        "workspace_exec_available": true,
        "workspace_exec_sandbox_enforced": policy["sandbox_enforced"],
        "workspace_exec_boundary": policy["execution_boundary"],
        "system_command_allowlist": policy["allowed_commands"],
        "configured_executable_paths": ctx.executable_paths.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
        "workspace_local_entries": {
            "enabled": policy["workspace_local_entries"],
            "script_extensions": ctx.policy.workspace_script_extensions.iter().cloned().collect::<Vec<_>>(),
            "resolution": "workdir_first"
        },
        // Backward-compatible alias for older MCP clients.
        "allowed_commands": policy["allowed_commands"],
        "warnings": ["Workspace 子进程当前允许执行，但尚未启用操作系统级文件系统沙箱"]
    })))
}

pub fn get_default_cwd(ctx: &ToolContext) -> Result<Value, WorkspaceError> {
    Ok(tool_ok(json!({
        "workspace": ctx.workspace.root_display(),
        "default_cwd": ctx.default_cwd_display(),
        "resolved_cwd": ctx.default_cwd_path().display().to_string()
    })))
}

pub fn set_default_cwd(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let resolved = ctx.workspace.resolve_existing(path)?;
    if !resolved.path.is_dir() {
        return Err(WorkspaceError::not_a_directory(
            "Default cwd must be a directory",
        ));
    }
    ctx.set_default_cwd(resolved.path.clone());
    Ok(tool_ok(json!({
        "workspace": ctx.workspace.root_display(),
        "default_cwd": resolved.display,
        "resolved_cwd": resolved.path.display().to_string()
    })))
}

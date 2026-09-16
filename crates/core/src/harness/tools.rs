use serde_json::{json, Value};

use crate::tools::workspace::{tool_ok, WorkspaceError};
use crate::tools::ToolContext;

use super::model::{TaskSession, TaskStatus};
use super::store::HarnessError;

pub const TOOL_NAMES: &[&str] = &[
    "harness_status",
    "operation_log",
    "project_state",
    "start_task",
    "update_task",
    "pause_task",
    "resume_task",
    "finish_task",
    "task_context",
    "list_task_events",
    "change_summary",
];

pub fn call(ctx: &ToolContext, name: &str, args: &Value) -> Result<Value, WorkspaceError> {
    let value = match name {
        "harness_status" => harness_status(ctx),
        "operation_log" => operation_log(ctx, args),
        "project_state" => project_state(ctx, args),
        "start_task" => start_task(ctx, args),
        "update_task" => update_task(ctx, args),
        "pause_task" => transition(ctx, args, TaskStatus::Paused),
        "resume_task" => transition(ctx, args, TaskStatus::Active),
        "finish_task" => finish_task(ctx, args),
        "task_context" => task_context(ctx, args),
        "list_task_events" => list_task_events(ctx, args),
        "change_summary" => change_summary(ctx, args),
        _ => return Err(tool_error("INVALID_ARGUMENT", "未知 Harness 工具")),
    }?;
    Ok(tool_ok(value))
}

fn harness_status(ctx: &ToolContext) -> Result<Value, WorkspaceError> {
    serde_json::to_value(ctx.harness.status().map_err(map_error)?)
        .map_err(|e| tool_error("SERIALIZE_FAILED", e.to_string()))
}

fn operation_log(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let offset = args.get("cursor").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = crate::tools::args::bounded(args, "operation_log", "limit") as usize;
    let operations = ctx
        .harness
        .list_operations(offset, limit)
        .map_err(map_error)?;
    Ok(json!({
        "operations": operations,
        "next_cursor": offset + operations.len()
    }))
}

fn project_state(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let max_files = crate::tools::args::bounded(args, "project_state", "max_files") as usize;
    let mut state = serde_json::to_value(ctx.harness.project_state(max_files).map_err(map_error)?)
        .map_err(|e| tool_error("SERIALIZE_FAILED", e.to_string()))?;
    if let Some(task) = state.get_mut("task").filter(|task| task.is_object()) {
        *task = without_baseline_entries(task.take());
    }
    Ok(state)
}

fn start_task(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let objective = args
        .get("objective")
        .and_then(Value::as_str)
        .ok_or_else(|| tool_error("INVALID_ARGUMENT", "objective 是必填项"))?;
    let task = ctx.harness.start_task(objective).map_err(map_error)?;
    Ok(json!({"task": task_view(&task)?, "next": ["project_state", "task_context"]}))
}

fn update_task(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let task_id = task_id(args)?;
    let completed_steps = string_list(args.get("completed_steps"))?;
    let pending_steps = string_list(args.get("pending_steps"))?;
    let task = ctx
        .harness
        .update_steps(task_id, completed_steps, pending_steps)
        .map_err(map_error)?;
    Ok(json!({"task": task_view(&task)?}))
}

fn transition(
    ctx: &ToolContext,
    args: &Value,
    status: TaskStatus,
) -> Result<Value, WorkspaceError> {
    let task = ctx
        .harness
        .transition(task_id(args)?, status)
        .map_err(map_error)?;
    Ok(json!({"task": task_view(&task)?}))
}

fn finish_task(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let task_id = task_id(args)?;
    let allow_unverified = args
        .get("allow_unverified")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let status = if allow_unverified {
        TaskStatus::CompletedUnverified
    } else {
        TaskStatus::Verifying
    };
    let task = ctx.harness.transition(task_id, status).map_err(map_error)?;
    let summary = change_summary(ctx, &json!({"task_id": task_id}))?;
    Ok(json!({"task": task_view(&task)?, "change_summary": summary}))
}

fn task_context(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let task = if let Some(task_id) = args.get("task_id").and_then(Value::as_str) {
        Some(ctx.harness.task(task_id).map_err(map_error)?)
    } else {
        ctx.harness.current_task().map_err(map_error)?
    };
    let Some(task) = task else {
        return Ok(json!({"task": null, "message": "当前没有活动任务"}));
    };
    let max_bytes = crate::tools::args::bounded(args, "task_context", "max_bytes") as usize;
    let view = task_view(&task)?;
    // 预算按客户端最终收到的整个对象算，含 tool_ok 补上的 "ok"。truncated 和
    // next_cursor 先占最长的写法，装完填真值只会变短。任务本身不截：目标和步骤
    // 是调用方自己写的，正常离 8 KB 的下限很远。
    let skeleton = json!({
        "ok": true, "task": view, "events": [], "truncated": false, "next_cursor": usize::MAX
    });
    let mut used = json_len(&skeleton)?;
    let mut events = Vec::new();
    let mut truncated = false;
    'fill: loop {
        let page = ctx
            .harness
            .list_events(&task.id, events.len(), EVENT_PAGE)
            .map_err(map_error)?;
        let last_page = page.len() < EVENT_PAGE;
        for event in page {
            let cost = json_len(&event)? + usize::from(!events.is_empty());
            if used + cost > max_bytes {
                truncated = true;
                break 'fill;
            }
            used += cost;
            events.push(event);
        }
        if last_page {
            break;
        }
    }
    let next_cursor = events.len();
    Ok(json!({"task": view, "events": events, "truncated": truncated, "next_cursor": next_cursor}))
}

/// 装事件时一次向存储要几条。只决定读几次文件，装多少由 max_bytes 决定。
const EVENT_PAGE: usize = 100;

/// 回给客户端的任务。凡是把任务交给客户端的工具都走这里。
fn task_view(task: &TaskSession) -> Result<Value, WorkspaceError> {
    serde_json::to_value(task)
        .map(without_baseline_entries)
        .map_err(|e| tool_error("SERIALIZE_FAILED", e.to_string()))
}

/// 基线的逐文件清单（每个文件一条路径加 sha256）只供服务端 check_baseline 比对，
/// 客户端拿到也用不上；它随项目文件数线性涨，2108 个文件的项目约 450 KB，以前
/// 开任务、改步骤、暂停、结束每次都原样回一遍。只留条数，指纹、分支、HEAD 照旧。
fn without_baseline_entries(mut task: Value) -> Value {
    if let Some(baseline) = task.get_mut("baseline").and_then(Value::as_object_mut) {
        let count = baseline
            .remove("entries")
            .and_then(|entries| entries.as_array().map(Vec::len))
            .unwrap_or(0);
        baseline.insert("entry_count".into(), json!(count));
    }
    task
}

fn json_len(value: &impl serde::Serialize) -> Result<usize, WorkspaceError> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(|e| tool_error("SERIALIZE_FAILED", e.to_string()))
}

fn list_task_events(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let task_id = task_id(args)?;
    let offset = args.get("cursor").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = crate::tools::args::bounded(args, "list_task_events", "limit") as usize;
    let events = ctx
        .harness
        .list_events(task_id, offset, limit)
        .map_err(map_error)?;
    Ok(json!({"events": events, "next_cursor": offset + events.len()}))
}

/// 摘要最多列几个改动文件，和改之前一样是 200；超过时看 total_changed_files。
const SUMMARY_FILES: usize = 200;

fn change_summary(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let task = if let Some(task_id) = args.get("task_id").and_then(Value::as_str) {
        ctx.harness.task(task_id).map_err(map_error)?
    } else {
        ctx.harness
            .current_task()
            .map_err(map_error)?
            .ok_or_else(|| tool_error("TASK_STATE_REQUIRED", "没有可总结的活动任务"))?
    };
    // 先对这个任务自己的基线算出全部改动再截，不是先截全部文件再挑改动——
    // 后者在大仓库里会把排在后面的改动整个漏掉。
    let changed = ctx.harness.task_changes(&task);
    let total_changed_files = changed.len();
    let files = changed.into_iter().take(SUMMARY_FILES).collect::<Vec<_>>();
    let events = ctx
        .harness
        .list_events(&task.id, 0, 100)
        .map_err(map_error)?;
    Ok(json!({
        "task_id": task.id,
        "objective": task.objective,
        "why": {"text": task.objective, "source": "task_objective"},
        "files": files,
        "total_changed_files": total_changed_files,
        "evidence": events,
        "verification": [],
        "risks": [],
        "rollback_capability": "not_available_in_foundation"
    }))
}

fn task_id(args: &Value) -> Result<&str, WorkspaceError> {
    args.get("task_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| tool_error("INVALID_ARGUMENT", "task_id 是必填项"))
}

fn string_list(value: Option<&Value>) -> Result<Option<Vec<String>>, WorkspaceError> {
    let Some(value) = value else { return Ok(None) };
    let list = value
        .as_array()
        .ok_or_else(|| tool_error("INVALID_ARGUMENT", "步骤必须是字符串数组"))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| tool_error("INVALID_ARGUMENT", "步骤必须是字符串数组"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(list))
}

fn map_error(error: HarnessError) -> WorkspaceError {
    tool_error(error.code(), error.to_string())
}

fn tool_error(code: &'static str, message: impl Into<String>) -> WorkspaceError {
    WorkspaceError::Tool {
        code,
        message: message.into(),
        category: "permission",
        retryable: matches!(
            code,
            "TASK_ALREADY_ACTIVE" | "FILE_CHANGED_EXTERNALLY" | "BASELINE_STALE"
        ),
    }
}

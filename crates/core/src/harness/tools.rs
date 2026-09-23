use serde_json::{json, Value};

use crate::tools::workspace::{tool_ok, WorkspaceError};
use crate::tools::ToolContext;

use super::model::{TaskSession, TaskStatus};
use super::store::{HarnessError, LogPage};
use super::verify::verification_records;

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
    "refresh_baseline",
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
        "refresh_baseline" => refresh_baseline(ctx, args),
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
    let page = ctx
        .harness
        .list_operations(offset, limit)
        .map_err(map_error)?;
    Ok(log_view("operations", page))
}

/// 一页日志回给客户端的样子。`next_cursor` 按文件行算，坏行也占一行；坏行只在有的时候
/// 列在 `unreadable_lines`，带行号和解析错误。
fn log_view<T: serde::Serialize>(key: &str, page: LogPage<T>) -> Value {
    let mut value = json!({"next_cursor": page.next_offset});
    if !page.unreadable.is_empty() {
        value["unreadable_lines"] = json!(page.unreadable);
    }
    value[key] = json!(page.into_items());
    value
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
    let evidence = string_list(args.get("evidence_session_ids"))?.unwrap_or_default();
    let result = ctx
        .harness
        .finish_task(task_id, &evidence, allow_unverified)
        .map_err(map_error)?;
    if !result.rejected.is_empty() {
        return Err(WorkspaceError::ToolDetails {
            code: "VERIFICATION_REJECTED",
            message: format!(
                "{} 条证据不能接受，任务状态没动（仍是 {:?}）；逐条看 details.rejected",
                result.rejected.len(),
                result.task.status
            ),
            category: "validation",
            retryable: false,
            details: json!({
                "task_id": task_id,
                "task_status": result.task.status,
                "rejected": result.rejected,
                "evidence_candidates": result.candidates
            }),
        });
    }
    let summary = change_summary(ctx, &json!({"task_id": task_id}))?;
    let mut value = json!({"task": task_view(&result.task)?, "change_summary": summary});
    if result.task.status == TaskStatus::Verifying {
        value["verification_required"] = json!(true);
        value["evidence_candidates"] = json!(result.candidates);
        value["next"] = json!(
            "任务等待验收。用 exec_command 跑测试（任务期间起的、退出 0、跑完之后没再改文件），再 finish 带 evidence_session_ids=[它的 session_id]；确认放弃正式验收才用 allow_unverified=true"
        );
    }
    if !result.warnings.is_empty() {
        value["warnings"] = json!(result.warnings);
    }
    Ok(value)
}

fn refresh_baseline(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let review = ctx
        .harness
        .refresh_baseline(
            task_id(args)?,
            args.get("accept_fingerprint").and_then(Value::as_str),
            args.get("reason").and_then(Value::as_str),
        )
        .map_err(map_error)?;
    let mut value =
        serde_json::to_value(&review).map_err(|e| tool_error("SERIALIZE_FAILED", e.to_string()))?;
    if !review.accepted {
        value["next"] = json!(if review.baseline_matches {
            "工作区和任务记账一致，不需要接纳"
        } else {
            "逐个看 changes 里的文件（read_file / git_diff），弄清是谁改的；确认可以算进这个任务时，带 accept_fingerprint=current.fingerprint 和 reason 再调一次。不能算的先恢复原样"
        });
    }
    Ok(value)
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
    // 预算按客户端最终收到的整个对象算，含 tool_ok 补上的 "ok"。truncated、
    // next_cursor、unreadable_line_count 先占最长的写法，装完填真值只会变短。任务本身
    // 不截：目标和步骤是调用方自己写的，正常离 8 KB 的下限很远。坏行只回个数不列明细：
    // 整个文件都坏了时明细能把回包撑爆，要看行号用 action=events（按 limit 分页）。
    let skeleton = json!({
        "ok": true, "task": view, "events": [], "truncated": false, "next_cursor": usize::MAX,
        "unreadable_line_count": usize::MAX
    });
    let mut used = json_len(&skeleton)?;
    let mut events = Vec::new();
    let mut unreadable = 0;
    let mut truncated = false;
    let mut next_cursor = 0;
    'fill: loop {
        let page = ctx
            .harness
            .list_events(&task.id, next_cursor, EVENT_PAGE)
            .map_err(map_error)?;
        for (line, event) in page.records {
            let cost = json_len(&event)? + usize::from(!events.is_empty());
            if used + cost > max_bytes {
                truncated = true;
                next_cursor = line;
                // 坏行的行号从 1 数，截断点 line 从 0 数：之前的坏行是 <= line 的那些。
                unreadable += page
                    .unreadable
                    .iter()
                    .filter(|bad| bad.line.is_some_and(|bad| bad <= line))
                    .count();
                break 'fill;
            }
            used += cost;
            events.push(event);
        }
        unreadable += page.unreadable.len();
        next_cursor = page.next_offset;
        if page.exhausted {
            break;
        }
    }
    let mut value =
        json!({"task": view, "events": events, "truncated": truncated, "next_cursor": next_cursor});
    if unreadable > 0 {
        value["unreadable_line_count"] = json!(unreadable);
    }
    Ok(value)
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
    let page = ctx
        .harness
        .list_events(task_id, offset, limit)
        .map_err(map_error)?;
    Ok(log_view("events", page))
}

/// 摘要最多列几个改动文件，和改之前一样是 200；超过时看 total_changed_files。
const SUMMARY_FILES: usize = 200;

/// 摘要里的 evidence 列前几条事件。验收记录从全部事件里找，不受这个限制。
const SUMMARY_EVENTS: usize = 100;

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
    let changes = ctx.harness.task_changes(&task);
    let total_changed_files = changes.files.len();
    let files = changes
        .files
        .into_iter()
        .take(SUMMARY_FILES)
        .collect::<Vec<_>>();
    let log = ctx
        .harness
        .list_events(&task.id, 0, usize::MAX)
        .map_err(map_error)?;
    let unreadable_lines = log.unreadable.len();
    let mut events = log.into_items();
    let verification = verification_records(&events);
    events.truncate(SUMMARY_EVENTS);
    let mut risks = Vec::new();
    match verification.last() {
        None if task.status == TaskStatus::CompletedUnverified => {
            risks.push("任务以 completed_unverified 收尾：没有被接受的验收证据".to_string())
        }
        None => risks.push("还没有被接受的验收证据".to_string()),
        Some(last) if last.fingerprint != changes.position.fingerprint => {
            risks.push("最后一次验收之后工作区又变了，现在的内容没有被验证".to_string())
        }
        Some(last) if !last.changed_during_run.is_empty() => risks.push(format!(
            "验收命令运行期间改了 {} 个文件，它测的是改之前的内容",
            last.changed_during_run.len()
        )),
        Some(_) => {}
    }
    if !changes.unreadable.is_empty() {
        risks.push(format!(
            "{} 个文件没读到，它们有没有变不知道",
            changes.unreadable.len()
        ));
    }
    if unreadable_lines > 0 {
        risks.push(format!(
            "任务事件里有 {unreadable_lines} 行读不出来，记录不全；行号见 task_manage action=events"
        ));
    }
    Ok(json!({
        "task_id": task.id,
        "objective": task.objective,
        "why": {"text": task.objective, "source": "task_objective"},
        "files": files,
        "total_changed_files": total_changed_files,
        "evidence": events,
        "verification": verification,
        "risks": risks,
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
        // BASELINE_STALE（finish 时工作区有没认领的改动）原样重试没用，得先 refresh_baseline。
        retryable: code == "TASK_ALREADY_ACTIVE",
    }
}

//! 按调用方列出运行记录，并以稳定游标翻页。

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::runs::RunRecord;
use crate::workspace::{tool_ok, WorkspaceError};

use super::output::command_ok;
use super::{ExecSession, SessionStore};

/// `list_runs` 的 `status` 能填的值，和 `termination_reason` 同一套。
const RUN_STATUSES: &[&str] = &[
    "running",
    "exited",
    "timeout",
    "killed",
    "interrupted",
    "unknown",
];

/// 列出这个主体在这个项目里的命令运行记录（审查 D09 的"发现"那一半）。
///
/// 为什么要有：`read_output` 要先知道 `session_id`。它在当初那次对话里、任务事件和 Planning
/// 台账里，可换一个对话、gld 重启过、或者当初根本没转述出来，就只能翻数据目录。这里只回
/// 摘要（脱敏的命令、终态、时间、每个流写了多少），不回输出正文：要看输出，拿 `output_refs`
/// 去 `read_output`，那条路的分页、主体检查一行不变。
///
/// - **只看自己的**：主体过滤在最前面，`total` 也是过滤之后数的，从总数看不出别人跑过什么。
/// - **只读**：不停命令、不发信号、不重放；起它的 gld 不在了的 `running` 照读记录的规矩记成
///   `unknown`（这一步会写回记录，和 `read_output` 一样）。
/// - **列出来不等于还能当证据**：重启前跑完的测试照样列出来，任务验收照旧按
///   `workspace_writes_since_start` 判（换了进程是 `null`，要重跑）。
///
/// 分页按"起跑时间从新到旧、同一毫秒按 id"排；`cursor` 就是上一页最后一条的位置，新起的
/// 命令排在最前面，不会让后面的页错位或重复。
pub fn list_runs(store: &SessionStore, args: &Value) -> Result<Value, WorkspaceError> {
    let limit = gld_protocol::tool_args::bounded(args, "list_runs", "limit") as usize;
    let statuses: Option<Vec<&str>> = match args.get("status") {
        None | Some(Value::Null) => None,
        // 空数组筛掉一切，拿到的空列表会被当成"没跑过"。
        Some(Value::Array(items)) if items.is_empty() => {
            return Err(WorkspaceError::invalid_argument(
                "status must list at least one value; leave it out to list every run",
            ))
        }
        Some(Value::Array(items)) => Some(
            items
                .iter()
                .map(|item| {
                    item.as_str()
                        .filter(|status| RUN_STATUSES.contains(status))
                        .ok_or_else(|| {
                            WorkspaceError::invalid_argument(format!(
                                "status items must be one of: {}",
                                RUN_STATUSES.join(", ")
                            ))
                        })
                })
                .collect::<Result<_, _>>()?,
        ),
        Some(_) => {
            return Err(WorkspaceError::invalid_argument(
                "status must be an array, e.g. [\"running\", \"unknown\"]",
            ))
        }
    };
    let within_ms = match args.get("started_within_minutes") {
        None | Some(Value::Null) => None,
        Some(value) => match value.as_u64() {
            Some(minutes @ 1..=10_080) => Some(minutes * 60_000),
            _ => {
                return Err(WorkspaceError::invalid_argument(
                    "started_within_minutes must be an integer from 1 to 10080 (7 days)",
                ))
            }
        },
    };
    let cursor = match args.get("cursor") {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.as_str().and_then(parse_run_cursor).ok_or_else(|| {
            WorkspaceError::invalid_argument(
                "cursor must be the next_cursor value from a previous list_runs call",
            )
        })?),
    };

    let retention = json!({
        "max_finished_runs": crate::runs::MAX_FINISHED_RUNS,
        "max_age_days": crate::runs::MAX_AGE.as_secs() / 86_400,
    });
    let Some(runs) = store.runs() else {
        // 没有数据目录可写（只有测试和嵌入用法会这样）：什么都没落盘，不是"没跑过命令"。
        return Ok(tool_ok(json!({
            "runs": [],
            "total": 0,
            "returned": 0,
            "next_cursor": null,
            "records_kept": false,
            "retention": retention,
            "warnings": ["run records are not kept here, so there is nothing to list"]
        })));
    };

    let now = crate::runs::now_ms();
    let records = runs.list();
    // 内存表在读完记录之后再取：`insert` 先建记录后放进内存表，这样读到的记录要么已经在
    // 表里，要么是几微秒前刚建的——下面按起跑多久区分后一种。
    let live: HashMap<String, Arc<ExecSession>> =
        store.sessions.lock().expect("sessions lock").clone();
    let mut matched: Vec<Listed> = Vec::new();
    for record in records {
        if within_ms.is_some_and(|within| now.saturating_sub(record.started_at_ms) > within) {
            continue;
        }
        let entry = match live.get(&record.session_id) {
            // 还在本进程内存里的，以内存为准：结束检查每 200 毫秒一次，记录可能还没写；
            // 被 kill 了但进程还没退（拦着 TERM）的，原因已经标成 killed，进程却还在跑。
            Some(session) => {
                session.try_refresh_status();
                let (reason, exit_code, _) = session.termination();
                Listed {
                    running: !session.has_exited(),
                    reason,
                    exit_code,
                    record,
                }
            }
            // 本进程起的、记录还是 running、内存里却没有：结束时写记录失败了，或者会话表被扔掉了，
            // 按 `read_output` 的规矩判 unknown，两边说法一致。刚起几秒的除外，那是正夹在
            // `insert` 两步中间的命令。
            None if record.is_running()
                && record.owner == crate::runs::instance_id()
                && now.saturating_sub(record.started_at_ms) >= DETACHED_AFTER_MS =>
            {
                let record = runs.load_detached(&record.session_id).unwrap_or(record);
                Listed {
                    running: record.is_running(),
                    reason: record.status.clone(),
                    exit_code: record.exit_code,
                    record,
                }
            }
            None => Listed {
                running: record.is_running(),
                reason: record.status.clone(),
                exit_code: record.exit_code,
                record,
            },
        };
        // 按 running 筛看进程在不在，不看原因：正被停下的命令进程还活着，也是"还在跑"。
        if statuses.as_ref().is_some_and(|wanted| {
            !wanted.contains(&entry.reason.as_str())
                && !(entry.running && wanted.contains(&"running"))
        }) {
            continue;
        }
        matched.push(entry);
    }

    let total = matched.len();
    let mut warnings: Vec<String> = Vec::new();
    let start = match &cursor {
        None => 0,
        Some((at, id)) => {
            if !matched
                .iter()
                .any(|entry| entry.record.started_at_ms == *at && entry.record.session_id == *id)
            {
                warnings.push(
                    "the run at the cursor is no longer listed (cleaned up, or it no longer matches the filters); this page continues from its position".into(),
                );
            }
            matched
                .iter()
                .position(|entry| {
                    (entry.record.started_at_ms, &entry.record.session_id) < (*at, id)
                })
                .unwrap_or(matched.len())
        }
    };
    let page: Vec<Value> = matched
        .iter()
        .skip(start)
        .take(limit)
        .map(|entry| {
            let Listed {
                record,
                reason,
                exit_code,
                running,
            } = entry;
            let id = &record.session_id;
            json!({
                "session_id": id,
                "command": record.command,
                "termination_reason": reason,
                "running": running,
                "exit_code": exit_code,
                // 进程还在就不说成败，哪怕原因已经标成 killed / timeout。
                "command_ok": if *running { None } else { command_ok(reason, *exit_code) },
                "started_at_ms": record.started_at_ms,
                "finished_at_ms": record.finished_at_ms,
                "duration_ms": record.finished_at_ms.map(|end| end.saturating_sub(record.started_at_ms)),
                "workspace_writes_since_start": store.writes_since_start(record),
                "output_refs": {
                    "stdout": format!("session:{id}:stdout"),
                    "stderr": format!("session:{id}:stderr"),
                },
                "stdout_bytes": runs.stream_total(record, "stdout"),
                "stderr_bytes": runs.stream_total(record, "stderr"),
                "incomplete_logs": record.incomplete_logs,
                "started_by_this_gld": record.owner == crate::runs::instance_id(),
                "started_by_gld_pid": record.owner_pid,
                "service_port": record.service_port,
            })
        })
        .collect();
    let returned = page.len();
    let next_cursor = (start + returned < total)
        .then(|| matched.get(start + returned - 1))
        .flatten()
        .map(|entry| format!("{}:{}", entry.record.started_at_ms, entry.record.session_id));
    Ok(tool_ok(json!({
        "runs": page,
        "total": total,
        "returned": returned,
        "next_cursor": next_cursor,
        "records_kept": true,
        "retention": retention,
        "warnings": warnings
    })))
}

/// 本进程起的 running 记录、内存表里没有，起跑多久之后才当它脱了钩（判 unknown）。
/// 比 `insert` 两步之间那几微秒长得多，又比"写记录失败"之后有人来看的间隔短得多。
const DETACHED_AFTER_MS: u64 = 5_000;

/// `list_runs` 列出来的一条：记录，加上按内存或记录算出的此刻状态。
struct Listed {
    record: RunRecord,
    reason: String,
    exit_code: Option<i32>,
    running: bool,
}

/// `list_runs` 的游标：`<起跑毫秒>:<session_id>`，id 必须是我们给出去的原样（小写、带连字符）：
/// 大写或去掉连字符的同一个 UUID 排序位置不同，同一毫秒里会翻错页。
fn parse_run_cursor(cursor: &str) -> Option<(u64, String)> {
    let (at, id) = cursor.split_once(':')?;
    let at = at.parse().ok()?;
    let canonical = uuid::Uuid::parse_str(id).ok()?.hyphenated().to_string();
    (canonical == id).then_some((at, canonical))
}

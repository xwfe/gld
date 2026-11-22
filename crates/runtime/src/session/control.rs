//! 标准输入与命令停止；所有等待都沿用调用方的时间预算。

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::process::ChildStdin;

use crate::runs::RunRecord;
use crate::workspace::{tool_ok, WorkspaceError};

use super::output::recorded_snapshot;
use super::process_control::signal_process_tree;
use super::{ExecSession, Found, SessionStore};

/// 一次 `write_stdin` 最多等多久。
///
/// 管道的缓冲是有限的（Linux 64 KiB，macOS 更小）。命令**不读** stdin 时，
/// 写满之后这一写就再也不返回——而它跑在 `block_on` 里，等于把这次工具调用
/// 永久挂住，调用方连"它没在读"都不知道（方案 E：写入阻塞也必须受
/// timeout/cancel 管理，验收 A17）。
pub(super) const STDIN_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// 分段写，超时就停下并**如实说写进去了多少**。
///
/// 不用 `write_all`：它超时之后没法知道已经写了几个字节，而"写了一半"和
/// "一个字节都没写"对调用方是两件事。
pub(super) async fn write_stdin_bounded(
    stdin: &mut ChildStdin,
    bytes: &[u8],
) -> Result<(), WorkspaceError> {
    use tokio::io::AsyncWriteExt;
    let deadline = Instant::now() + STDIN_WRITE_TIMEOUT;
    let mut written = 0usize;
    while written < bytes.len() {
        let left = deadline.saturating_duration_since(Instant::now());
        let stalled = |written: usize| {
            WorkspaceError::ToolDetails {
            code: "STDIN_WRITE_TIMEOUT",
            message: format!(
                "the command is not reading stdin: {written} of {} bytes went in within {}s. It may not read standard input at all, or it is still busy with the previous input.",
                bytes.len(),
                STDIN_WRITE_TIMEOUT.as_secs()
            ),
            category: "runtime",
            retryable: true,
            details: json!({
                "bytes_written": written,
                "bytes_requested": bytes.len(),
                "timeout_seconds": STDIN_WRITE_TIMEOUT.as_secs()
            }),
        }
        };
        if left.is_zero() {
            return Err(stalled(written));
        }
        match tokio::time::timeout(left, stdin.write(&bytes[written..])).await {
            // 写回 0 字节：管道另一头没了。
            Ok(Ok(0)) => {
                return Err(WorkspaceError::Tool {
                    code: "SESSION_CLOSED",
                    message: "Session stdin is closed.".into(),
                    category: "runtime",
                    retryable: false,
                })
            }
            Ok(Ok(n)) => written += n,
            Ok(Err(_)) => {
                return Err(WorkspaceError::Tool {
                    code: "SESSION_CLOSED",
                    message: "Session stdin is closed.".into(),
                    category: "runtime",
                    retryable: false,
                })
            }
            Err(_) => return Err(stalled(written)),
        }
    }
    Ok(())
}

pub fn write_stdin(store: &SessionStore, args: &Value) -> Result<Value, WorkspaceError> {
    let session_id = args
        .get("session_id")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("session_id is required"))?;
    let chars = args.get("chars").and_then(Value::as_str).unwrap_or("");
    let max_output_bytes =
        gld_protocol::tool_args::bounded(args, "write_stdin", "max_output_bytes") as usize;
    let session = match store.lookup(session_id)? {
        Found::Live(session) => session,
        // 只剩运行记录的命令不在这个进程里，它的 stdin 早就接不上了。
        Found::Recorded(record) => {
            if !chars.is_empty() {
                return Err(WorkspaceError::Tool {
                    code: "SESSION_CLOSED",
                    message: format!(
                        "This command is no longer attached to this process ({}); nothing can be written to its stdin.",
                        record.status
                    ),
                    category: "runtime",
                    retryable: false,
                });
            }
            return Ok(tool_ok(recorded_snapshot(store, &record, max_output_bytes)));
        }
    };

    let running = crate::async_rt::block_on(session.is_running());
    if !running {
        if !chars.is_empty() {
            return Err(WorkspaceError::Tool {
                code: "SESSION_CLOSED",
                message: "Session is closed; stdin write blocked.".into(),
                category: "runtime",
                retryable: false,
            });
        }
        return Ok(tool_ok(session.snapshot(max_output_bytes)));
    }

    if !chars.is_empty() {
        let mut stdin_guard = crate::async_rt::block_on(session.stdin.lock());
        let stdin = stdin_guard.as_mut().ok_or_else(|| WorkspaceError::Tool {
            code: "SESSION_CLOSED",
            message: "Session stdin is closed.".into(),
            category: "runtime",
            retryable: false,
        })?;
        use tokio::io::AsyncWriteExt;
        crate::async_rt::block_on(write_stdin_bounded(stdin, chars.as_bytes()))?;
        let _ = crate::async_rt::block_on(stdin.flush());
    }

    let yield_ms = gld_protocol::tool_args::bounded(args, "write_stdin", "yield_time_ms");
    crate::async_rt::block_on(wait_for_exit(
        &session,
        std::time::Duration::from_millis(yield_ms),
    ));
    Ok(tool_ok(session.snapshot(max_output_bytes)))
}

/// 等到命令结束或者到点，先到哪个算哪个。
///
/// 以前不管命令结没结束都睡满 `yield_time_ms`，等 30 秒就真等 30 秒，AI 只好用默认的 1 秒反复问：
/// 2026-10-18 一轮 9 条后台命令轮询了 12 次（审查 §28）。现在结束了就返回，交互式会话照旧等满。
async fn wait_for_exit(session: &ExecSession, limit: std::time::Duration) {
    let deadline = std::time::Instant::now() + limit;
    loop {
        session.refresh_status().await;
        if session.has_exited() {
            session.wait_for_readers().await;
            return;
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return;
        }
        tokio::time::sleep((deadline - now).min(std::time::Duration::from_millis(20))).await;
    }
}

pub fn kill_session(store: &SessionStore, args: &Value) -> Result<Value, WorkspaceError> {
    let session_id = args
        .get("session_id")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("session_id is required"))?;
    let max_output_bytes =
        gld_protocol::tool_args::bounded(args, "kill_session", "max_output_bytes") as usize;
    let wait_ms = gld_protocol::tool_args::bounded(args, "kill_session", "wait_ms");
    let signal = args.get("signal").and_then(Value::as_str).unwrap_or("TERM");
    let session = match store.lookup(session_id)? {
        Found::Live(session) => session,
        Found::Recorded(record) => {
            return Ok(tool_ok(recorded_kill(store, &record, max_output_bytes)));
        }
    };

    let (killed, status, evicted) = stop_session(&session, signal, wait_ms, "killed");

    let mut payload = session.snapshot(max_output_bytes);
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("killed".into(), json!(killed));
        obj.insert("status".into(), json!(status));
        obj.insert("evicted".into(), json!(evicted));
        if status == "terminating" {
            obj.insert(
                "warnings".into(),
                json!(["Process did not exit after kill; session retained for retry"]),
            );
        }
    }

    if evicted {
        store.remove_terminated(session_id);
    }

    Ok(tool_ok(payload))
}

/// 停一条会话：先标上原因，再发信号，最多等 `wait_ms`。返回 (killed, status, evicted)。
///
/// 原因要在发信号之前标：进程一退，[`ExecSession::record_exit_status`] 就把终态写进
/// 运行记录，那时候拿到的必须已经是 `killed` / `interrupted`，而不是默认的 `exited`。
pub(super) fn stop_session(
    session: &ExecSession,
    signal: &str,
    wait_ms: u64,
    reason: &str,
) -> (bool, &'static str, bool) {
    if !crate::async_rt::block_on(session.is_running()) {
        return (false, "exited", true);
    }
    session.mark_termination_reason(reason);
    crate::async_rt::block_on(async {
        let pid = {
            let child = session.child.lock().await;
            child.id()
        };
        if let Some(pid) = pid {
            signal_process_tree(pid, signal);
        } else {
            let mut child = session.child.lock().await;
            let _ = child.start_kill();
        }
        let _ = tokio::time::timeout(std::time::Duration::from_millis(wait_ms), async {
            let mut child = session.child.lock().await;
            let _ = child.wait().await;
        })
        .await;
    });
    crate::async_rt::block_on(session.refresh_status());
    if crate::async_rt::block_on(session.is_running()) {
        return (false, "terminating", false);
    }
    // 判断"还在跑"和标原因之间进程自己结束了：原因没标上，结局是它自己的。
    if session.termination().0 == reason {
        (true, "killed", true)
    } else {
        (false, "exited", true)
    }
}

/// 对只剩运行记录的命令 `kill_session`：这个进程停不了它，如实说清楚它现在怎样。
///
/// - 已经结束的：原样给出结局。
/// - 另一个还活着的 gld 进程起的、还在跑：只有那个进程停得了。
/// - `unknown`：gld 不去发信号——那个 pid 可能已经给了别的进程。`pid_in_use` 说那个进程组
///   现在还在不在，要不要手动停由人判断。
fn recorded_kill(store: &SessionStore, record: &RunRecord, max_output_bytes: usize) -> Value {
    let mut payload = recorded_snapshot(store, record, max_output_bytes);
    let pid = record
        .pid
        .map(|pid| pid.to_string())
        .unwrap_or_else(|| "?".into());
    let warning = match record.status.as_str() {
        "running" => Some(format!(
            "This command was started by another process of this service (pid {}) that is still running; only that process can stop it.",
            record.owner_pid
        )),
        "unknown" => Some(format!(
            "How this command ended was never recorded, and pid {pid} is not signalled: that id may belong to another process by now. pid_in_use says whether a process group with that id exists; look at it (pgrep -l -g {pid}) before stopping anything by hand."
        )),
        _ => None,
    };
    if let Some(object) = payload.as_object_mut() {
        object.insert("killed".into(), json!(false));
        object.insert("evicted".into(), json!(!record.is_running()));
        if let Some(warning) = warning {
            object.insert("warnings".into(), json!([warning]));
        }
    }
    payload
}

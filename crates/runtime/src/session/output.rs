//! 内存会话与落盘记录的统一快照和输出分页。

use serde_json::{json, Value};

use crate::runs::RunRecord;
use crate::workspace::{tool_ok, WorkspaceError};

use super::process_control::process_group_exists;
use super::{ExecSession, Found, SessionStore};

impl ExecSession {
    pub fn snapshot(&self, max_output_bytes: usize) -> Value {
        let stdout = self.stdout.lock().expect("stdout lock").clone();
        let stderr = self.stderr.lock().expect("stderr lock").clone();
        let (reason, exit_code, command_ok) = self.termination();
        let mut value = snapshot_value(
            Snapshot {
                session_id: &self.session_id,
                interactive: self.interactive,
                stdin_open: *self.stdin_open.lock().expect("stdin_open lock"),
                running: !self.has_exited(),
                reason: &reason,
                exit_code,
                command_ok,
                stdout: &stdout,
                stderr: &stderr,
                elapsed_ms: self.started_at.elapsed().as_millis() as u64,
                finished_ms_ago: self.finished_ago().map(|ago| ago.as_millis() as u64),
                workspace_writes_since_start: Some(self.workspace_writes_since_start()),
                kept_on_disk: self.kept_on_disk(),
            },
            max_output_bytes,
        );
        // 每次现查：服务可能刚起来，也可能自己挂了。
        if let (Some(port), Some(object)) = (self.service_port, value.as_object_mut()) {
            object.insert(
                "service".into(),
                crate::service::status(port, !self.has_exited()),
            );
        }
        value
    }
}

/// 按终态算命令成没成。结局不知道（`unknown`）和还在跑一样，不说成也不说败。
pub(super) fn command_ok(reason: &str, exit_code: Option<i32>) -> Option<bool> {
    match reason {
        "exited" => Some(exit_code.is_some_and(|code| code == 0)),
        "running" | "unknown" => None,
        _ => Some(false),
    }
}

/// 一条会话的快照要的那些事实。内存里的会话和运行记录各自填一份，拼出来的字段一样。
struct Snapshot<'a> {
    session_id: &'a str,
    interactive: bool,
    stdin_open: bool,
    running: bool,
    reason: &'a str,
    exit_code: Option<i32>,
    command_ok: Option<bool>,
    stdout: &'a [u8],
    stderr: &'a [u8],
    elapsed_ms: u64,
    finished_ms_ago: Option<u64>,
    workspace_writes_since_start: Option<u64>,
    kept_on_disk: bool,
}

fn snapshot_value(parts: Snapshot<'_>, max_output_bytes: usize) -> Value {
    let stdout = truncate_tail(parts.stdout, max_output_bytes);
    let stderr = truncate_tail(parts.stderr, max_output_bytes);
    let reason = parts.reason;
    json!({
        "session_id": parts.session_id,
        "interactive": parts.interactive,
        "stdin_open": parts.stdin_open,
        // 结局不知道的运行记录不说 exited：进程可能还作为孤儿在跑。
        "status": match (parts.running, reason) {
            (true, _) => "running",
            (false, "unknown") => "unknown",
            (false, _) => "exited",
        },
        "termination_reason": reason,
        "recoverable": matches!(reason, "timeout" | "killed" | "interrupted" | "spawn_failed" | "server_restart"),
        "suggestion": match reason {
            "timeout" => "读取保留输出，调整 timeout_ms 后重试",
            "killed" => "确认终止原因后重新执行命令",
            "interrupted" => "gld 退出时停掉的：先核对它做到了哪一步，再决定要不要重跑",
            "unknown" => "gld 没来得及记下结局：进程可能已经结束，也可能还在跑（看 pid）；先核对现状，别直接重跑",
            "exited" => "检查 exit_code 和 stderr",
            "crashed" => "检查 stderr 后重试或恢复工作区",
            _ if parts.running => "还在跑：要等它结束就调 write_stdin（chars 留空）带 yield_time_ms=30000，结束了立刻返回，最多等 30 秒；service_port 起的服务不会自己结束，看 service.ready",
            _ => "继续读取 session 或等待进程结束",
        },
        "exit_code": parts.exit_code,
        "transport_ok": true,
        "command_ok": parts.command_ok,
        "stdout": stdout.content,
        "stderr": stderr.content,
        "stdout_truncated": stdout.truncated,
        "stderr_truncated": stderr.truncated,
        "elapsed_ms": parts.elapsed_ms,
        // 进程结束多久了。还在跑就是 null。保留期还剩多少由 read_output
        // 回（那里知道 store 的 retention），这里只给事实。
        "finished_ms_ago": parts.finished_ms_ago,
        // 这条命令起来之后，这个工作区落过几次盘（apply_patch 提交一次算
        // 一次）。**不是 0 就说明命令读到的文件和现在的不一样**——它可能
        // 编译了旧代码，也可能中途读到了改了一半的文件树。后台命令期间
        // 没有写互斥，这个数就是事后判断结果可不可信的唯一依据。结局不知道的
        // 运行记录没有这个数（null）。
        "workspace_writes_since_start": parts.workspace_writes_since_start,
        // 输出和结局有没有落进运行记录（审查 D09）。true 时进程结束、gld 重启之后
        // read_output 照样读得到；false 是数据目录写不进去，只剩内存里那份。
        "kept_on_disk": parts.kept_on_disk,
        "output_refs": {
            "stdout": format!("session:{}:stdout", parts.session_id),
            "stderr": format!("session:{}:stderr", parts.session_id)
        }
    })
}

/// 只剩运行记录的会话的快照：和内存里的同一套字段，另加记录里才有的几格。
pub(super) fn recorded_snapshot(
    store: &SessionStore,
    record: &RunRecord,
    max_output_bytes: usize,
) -> Value {
    let runs = store.runs().expect("只有带运行记录的会话表才找得到记录");
    let (stdout, _) = runs.stream_bytes(record, "stdout");
    let (stderr, _) = runs.stream_bytes(record, "stderr");
    let now = crate::runs::now_ms();
    let reason = if record.is_running() {
        "running"
    } else {
        record.status.as_str()
    };
    let mut value = snapshot_value(
        Snapshot {
            session_id: &record.session_id,
            interactive: false,
            stdin_open: false,
            running: record.is_running(),
            reason,
            exit_code: record.exit_code,
            command_ok: command_ok(reason, record.exit_code),
            stdout: &stdout,
            stderr: &stderr,
            elapsed_ms: record
                .finished_at_ms
                .unwrap_or(now)
                .saturating_sub(record.started_at_ms),
            finished_ms_ago: record.finished_at_ms.map(|at| now.saturating_sub(at)),
            workspace_writes_since_start: store.writes_since_start(record),
            kept_on_disk: true,
        },
        max_output_bytes,
    );
    if let Some(object) = value.as_object_mut() {
        for (key, field) in record_fields(record) {
            object.insert(key.into(), field);
        }
        // 只剩记录的命令 gld 已经不管它的进程了，不去探端口：答话的未必是它。
        if let Some(port) = record.service_port {
            object.insert("service".into(), json!({ "port": port, "ready": null }));
        }
    }
    value
}

/// 运行记录才有的几格：从哪读到的、命令的 pid、起它的 gld 进程。
///
/// `unknown` 的另给 `pid_in_use`：那个进程组现在还在不在。在，可能是没收掉的孤儿，也可能
/// 这个号已经给了别的进程——所以只报事实，gld 不去杀它。不在就说明孤儿也没了。
fn record_fields(record: &RunRecord) -> Vec<(&'static str, Value)> {
    let mut fields = vec![
        ("source", json!("run_record")),
        ("pid", json!(record.pid)),
        ("started_by_gld_pid", json!(record.owner_pid)),
    ];
    if record.status == "unknown" {
        fields.push((
            "pid_in_use",
            json!(record.pid.and_then(process_group_exists)),
        ));
    }
    fields
}

struct Truncated {
    content: String,
    truncated: bool,
}

fn truncate_tail(bytes: &[u8], max_bytes: usize) -> Truncated {
    let truncated = bytes.len() > max_bytes;
    let take = bytes.len().min(max_bytes);
    Truncated {
        content: String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(take)..]).into_owned(),
        truncated,
    }
}

pub fn read_output(store: &SessionStore, args: &Value) -> Result<Value, WorkspaceError> {
    // 也收 session_id：exec_command 两个都回，拿 session_id 来读是最自然的写法，以前报
    // schema 错误（D10 实测 ChatGPT 撞上，搭测试环境时这边也撞过一次）。
    let by_session;
    let output_ref = match (
        args.get("output_ref").and_then(Value::as_str),
        args.get("session_id").and_then(Value::as_str),
    ) {
        (Some(output_ref), None) => output_ref,
        (None, Some(session_id)) => {
            by_session = format!("session:{session_id}:full");
            by_session.as_str()
        }
        (Some(_), Some(_)) => {
            return Err(WorkspaceError::invalid_argument(
                "give output_ref or session_id, not both",
            ))
        }
        (None, None) => {
            return Err(WorkspaceError::invalid_argument(
                "output_ref (from exec_command's output_refs) or session_id is required",
            ))
        }
    };
    let parts: Vec<&str> = output_ref.split(':').collect();
    if parts.len() != 3 || parts[0] != "session" {
        return Err(WorkspaceError::invalid_argument(
            "output_ref must look like session:<id>:stdout, session:<id>:stderr, or session:<id>:full",
        ));
    }
    let session_id = parts[1];
    let ref_stream = parts[2];
    if ref_stream != "stdout" && ref_stream != "stderr" && ref_stream != "full" {
        return Err(WorkspaceError::invalid_argument(
            "output_ref stream must be stdout, stderr, or full",
        ));
    }
    let found = store.lookup(session_id)?;

    let requested_stream = args.get("stream").and_then(Value::as_str).unwrap_or("");
    // output_ref 自带流名时再给一个不同的 stream，以前悄悄按 output_ref 读，调用方以为读的是 stderr。
    if (ref_stream == "stdout" || ref_stream == "stderr")
        && !requested_stream.is_empty()
        && requested_stream != ref_stream
    {
        return Err(WorkspaceError::invalid_argument(format!(
            "output_ref is for {ref_stream} but stream says {requested_stream}; use session:{session_id}:{requested_stream} or drop stream"
        )));
    }
    let stream = if ref_stream == "stdout" || ref_stream == "stderr" {
        ref_stream
    } else if requested_stream == "stdout" || requested_stream == "stderr" {
        requested_stream
    } else {
        "stdout"
    };

    // 两个来源给出同一组事实，下面的分页一行不分叉。
    let view = match &found {
        Found::Live(session) => {
            crate::async_rt::block_on(session.refresh_status());
            let (data, total) = session.retained_stream_bytes(stream);
            let (reason, exit_code, command_ok) = session.termination();
            let kept_on_disk = session.kept_on_disk();
            StreamView {
                data,
                total,
                reason,
                exit_code,
                command_ok,
                writes: Some(session.workspace_writes_since_start()),
                running: !session.has_exited(),
                kept_on_disk,
                // 落了盘的输出不会在内存保留期到了之后消失，那个倒计时就不是"还能读多久"了。
                expires_in_ms: (!kept_on_disk)
                    .then(|| session.expires_in(store.retention()))
                    .flatten()
                    .map(|left| left.as_millis() as u64),
                retention_ms: (!kept_on_disk).then(|| store.retention().as_millis() as u64),
                record: None,
            }
        }
        Found::Recorded(record) => {
            let runs = store.runs().expect("只有带运行记录的会话表才找得到记录");
            let (data, total) = runs.stream_bytes(record, stream);
            let reason = if record.is_running() {
                "running".to_string()
            } else {
                record.status.clone()
            };
            StreamView {
                data,
                total,
                command_ok: command_ok(&reason, record.exit_code),
                reason,
                exit_code: record.exit_code,
                writes: store.writes_since_start(record),
                running: record.is_running(),
                kept_on_disk: true,
                expires_in_ms: None,
                retention_ms: None,
                record: Some(record.as_ref()),
            }
        }
    };
    let StreamView {
        data,
        total: total_stream_bytes,
        ..
    } = &view;
    let (data, total_stream_bytes) = (data.as_slice(), *total_stream_bytes);
    let requested_offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = gld_protocol::tool_args::bounded(args, "read_output", "limit") as usize;
    let mut warnings: Vec<String> = Vec::new();
    if ref_stream == "full" {
        warnings.push(
            "legacy full output_ref defaults to stdout; use output_refs for stable stream paging"
                .into(),
        );
    }
    if view
        .record
        .is_some_and(|record| record.incomplete_logs.iter().any(|name| name == stream))
    {
        warnings.push(format!(
            "writing this stream's log failed (disk full?) after {total_stream_bytes} bytes; whatever the command printed after that is not in the run record"
        ));
    }

    // **偏移是整条流里的绝对位置**，不是保留缓冲里的下标。缓冲只留最后
    // SESSION_BUFFER_BYTES 个字节，所以它对应的是 [retained_from, total)
    // 这一段。原来两套坐标混着用：offset 按缓冲算、有没有下一页按累计算，
    // 于是读到缓冲末尾之后每次都回一个空页、next_offset 和传进来的一样，
    // 调用方照着它再读就是死循环（审查 X01）。
    let retained_from = total_stream_bytes.saturating_sub(data.len());
    if requested_offset > total_stream_bytes {
        return Err(WorkspaceError::invalid_argument(format!(
            "offset {requested_offset} is past the end of this stream ({total_stream_bytes} bytes so far)"
        )));
    }
    let start = requested_offset.max(retained_from);
    let dropped = start.saturating_sub(requested_offset);
    if dropped > 0 {
        warnings.push(format!(
            "{dropped} bytes before offset {start} are no longer retained: only the last {} bytes of this stream are kept",
            data.len()
        ));
    }

    let from = start - retained_from;
    let mut take = data.len().saturating_sub(from).min(limit);
    // 别把一个多字节字符劈成两半：截到字符边界，下一页从那里接着读。
    // 劈开的后果是每一页接缝上都多出一个替换字符，而那不是命令输出的内容。
    if from + take < data.len() {
        take = utf8_boundary(&data[from..from + take]);
        if take == 0 {
            // limit 小到装不下一个字符：照原样给出去，至少能前进。
            take = data.len().saturating_sub(from).min(limit);
        }
    }
    let chunk = &data[from..from + take];
    let next = start + chunk.len();
    let running = view.running;
    // 还有留着的字节没给完才有下一页。命令还在跑、但此刻没有新字节时，
    // next_offset 是空——不是"读完了"，而是"现在没有更多"，`complete`
    // 那一格说的才是流有没有结束。
    let next_offset = (next < total_stream_bytes).then_some(next as u64);

    let mut result = json!({
        "session_id": session_id,
        // 命令怎么结束的，和 exec_command 同一套字段。转后台的命令，这是调用方（和台账、
        // 任务验收）知道它退出码的唯一途径；以前这里只说 running 与否（审查 D03）。
        "termination_reason": view.reason,
        "exit_code": view.exit_code,
        "command_ok": view.command_ok,
        "workspace_writes_since_start": view.writes,
        "output_ref": output_ref,
        "stream_output_ref": format!("session:{session_id}:{stream}"),
        "stream": stream,
        "offset": start,
        "requested_offset": requested_offset,
        "dropped_bytes": dropped,
        "retained_from": retained_from,
        "limit": limit,
        "content": String::from_utf8_lossy(chunk),
        "next_offset": next_offset,
        "total_retained_bytes": data.len(),
        "total_stream_bytes": total_stream_bytes,
        "complete": !running && next >= total_stream_bytes,
        "running": running,
        // 这份输出还能读多久。**从进程结束那一刻算起**，和命令的 timeout 无关；
        // 还在跑的会话没有这个数（审查 X02）。到点之后再来读，拿到的是
        // SESSION_EXPIRED 而不是 SESSION_NOT_FOUND。
        //
        // 落了盘（`kept_on_disk`）的两个都是 null：盘上那份按"每个项目最近 64 条、
        // 最长 7 天"清，没有一个固定的倒计时（审查 D09）。
        "expires_in_ms": view.expires_in_ms,
        "retention_ms": view.retention_ms,
        "kept_on_disk": view.kept_on_disk,
        "truncated": next_offset.is_some(),
        "warnings": warnings
    });
    if let (Some(record), Some(object)) = (view.record, result.as_object_mut()) {
        for (key, field) in record_fields(record) {
            object.insert(key.into(), field);
        }
    }
    // 服务模式：起完服务之后 AI 拿 read_output 来问"起来没有"，所以这里也现查一次端口。
    let service = match &found {
        Found::Live(session) => session
            .service_port
            .map(|port| crate::service::status(port, running)),
        Found::Recorded(record) => record
            .service_port
            .map(|port| json!({ "port": port, "ready": null })),
    };
    if let (Some(service), Some(object)) = (service, result.as_object_mut()) {
        object.insert("service".into(), service);
    }
    Ok(tool_ok(result))
}

/// `read_output` 分页要的事实：留着的字节、整条流多长、命令怎样了。
struct StreamView<'a> {
    data: Vec<u8>,
    total: usize,
    reason: String,
    exit_code: Option<i32>,
    command_ok: Option<bool>,
    writes: Option<u64>,
    running: bool,
    kept_on_disk: bool,
    expires_in_ms: Option<u64>,
    retention_ms: Option<u64>,
    /// 从运行记录读到的才有。
    record: Option<&'a RunRecord>,
}

/// 这段字节里，最后一个完整 UTF-8 字符结束的位置。
///
/// 全是完整字符就是它自己的长度；末尾挂着半个字符就退到那个字符之前。
fn utf8_boundary(bytes: &[u8]) -> usize {
    match std::str::from_utf8(bytes) {
        Ok(_) => bytes.len(),
        Err(error) => error.valid_up_to(),
    }
}

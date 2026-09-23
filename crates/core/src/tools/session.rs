use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStdin};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use crate::tools::workspace::{tool_ok, WorkspaceError};
use serde_json::{json, Value};

const SESSION_BUFFER_BYTES: usize = 1_048_576;

/// 进程结束之后，它的输出还能读多久。
///
/// **和命令的 timeout 是两件事**：以前内联跑完的会话 30 秒后回收，转后台那条
/// 路的回收时点却挂在命令自己的 deadline 上——同样是"跑完了"，能读多久取决于
/// 当初怎么调的，而调用方没有任何办法知道还剩多少时间（审查 X02）。现在统一
/// 从进程结束那一刻起算，结果里回 `expires_in_ms`。
const SESSION_RETENTION: Duration = Duration::from_secs(300);

/// 最多留几条**已经结束**的会话。
///
/// 每条会话的两个流各留 1 MiB，纯靠时间回收的话，一个跑了几百条命令的会话表
/// 能占到几百 MiB。还在跑的不算在内——那些不能动。
const MAX_FINISHED_SESSIONS: usize = 32;

/// 最多记几条回收记录。
///
/// 记它是为了让"这个 id 过期了"和"从来没有这个 id"分得开（方案 E：过期、
/// 无效引用要能区分）。只存 id、原因、什么时候回收的，不留输出。
const MAX_TOMBSTONES: usize = 128;

struct Tombstone {
    id: String,
    reason: &'static str,
    at: Instant,
}

pub struct SessionStore {
    sessions: Mutex<HashMap<String, Arc<ExecSession>>>,
    graveyard: Mutex<VecDeque<Tombstone>>,
    retention: Duration,
}

impl Default for SessionStore {
    fn default() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            graveyard: Mutex::new(VecDeque::new()),
            retention: SESSION_RETENTION,
        }
    }
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 自定义保留期。给测试用：把它设成 0 就不用真的等 5 分钟，也不用
    /// sleep（验收 A16 要的"不靠真实 sleep 堆慢测试"）。
    pub fn with_retention(retention: Duration) -> Self {
        Self {
            retention,
            ..Self::default()
        }
    }

    pub fn retention(&self) -> Duration {
        self.retention
    }

    /// 清掉过期的和超出配额的。
    ///
    /// 惰性做：每次 insert / get 顺手扫一遍，不另起定时器。定时器那条路仍然
    /// 存在（`exec` 里的 eviction 任务）用来及时还内存，但**判据只有这一处**。
    fn sweep(&self) {
        let mut sessions = self.sessions.lock().expect("sessions lock");
        let mut expired: Vec<(String, &'static str)> = Vec::new();
        for (id, session) in sessions.iter() {
            if let Some(ago) = session.finished_ago() {
                if ago >= self.retention {
                    expired.push((id.clone(), "expired"));
                }
            }
        }
        // 配额：结束得最早的先走。还在跑的一条都不动。
        let mut finished: Vec<(String, Duration)> = sessions
            .iter()
            .filter(|(id, _)| !expired.iter().any(|(gone, _)| gone == *id))
            .filter_map(|(id, session)| session.finished_ago().map(|ago| (id.clone(), ago)))
            .collect();
        if finished.len() > MAX_FINISHED_SESSIONS {
            // 结束得越久的排在越前面，先淘汰它们。
            finished.sort_by_key(|(_, ago)| std::cmp::Reverse(*ago));
            let over_quota = finished.len() - MAX_FINISHED_SESSIONS;
            for (id, _) in finished.into_iter().take(over_quota) {
                expired.push((id, "evicted_over_quota"));
            }
        }
        for (id, reason) in expired {
            sessions.remove(&id);
            self.bury(&id, reason);
        }
    }

    fn bury(&self, id: &str, reason: &'static str) {
        let mut graveyard = self.graveyard.lock().expect("graveyard lock");
        if graveyard.len() >= MAX_TOMBSTONES {
            graveyard.pop_front();
        }
        graveyard.push_back(Tombstone {
            id: id.to_string(),
            reason,
            at: Instant::now(),
        });
    }

    fn tombstone_error(&self, session_id: &str) -> Option<WorkspaceError> {
        let graveyard = self.graveyard.lock().expect("graveyard lock");
        let stone = graveyard
            .iter()
            .rev()
            .find(|stone| stone.id == session_id)?;
        let seconds = stone.at.elapsed().as_secs();
        let why = match stone.reason {
            "expired" => format!(
                "its output was kept for {}s after the process exited and has been released",
                self.retention.as_secs()
            ),
            "evicted_over_quota" => format!(
                "the workspace keeps at most {MAX_FINISHED_SESSIONS} finished sessions and this was the oldest"
            ),
            "terminated" => {
                "it was stopped (kill_session, switching to plan mode, or the workspace leaving the hub) and its output was released".to_string()
            }
            _ => "it was removed".to_string(),
        };
        Some(WorkspaceError::ToolDetails {
            code: "SESSION_EXPIRED",
            message: format!(
                "Session {session_id} existed but {why} ({seconds}s ago). Re-run the command; the output cannot be recovered."
            ),
            category: "not_found",
            retryable: false,
            details: json!({
                "reason": stone.reason,
                "retention_seconds": self.retention.as_secs(),
                "released_seconds_ago": seconds
            }),
        })
    }

    pub fn insert(&self, session: ExecSession) -> Arc<ExecSession> {
        self.sweep();
        let arc = Arc::new(session);
        self.sessions
            .lock()
            .expect("sessions lock")
            .insert(arc.session_id.clone(), arc.clone());
        arc
    }

    pub fn get(&self, session_id: &str) -> Result<Arc<ExecSession>, WorkspaceError> {
        self.sweep();
        if let Some(session) = self
            .sessions
            .lock()
            .expect("sessions lock")
            .get(session_id)
            .cloned()
        {
            return Ok(session);
        }
        // 这个 id 曾经存在过吗？"过期了"和"编了一个 id"下一步完全不同：前者
        // 重跑命令，后者是自己记错了句柄（方案 E）。
        if let Some(error) = self.tombstone_error(session_id) {
            return Err(error);
        }
        Err(WorkspaceError::Tool {
            code: "SESSION_NOT_FOUND",
            message: format!("Session not found: {session_id}"),
            category: "not_found",
            retryable: false,
        })
    }

    /// 保留期到了，把它从表里去掉。
    pub fn remove(&self, session_id: &str) {
        self.remove_with_reason(session_id, "expired");
    }

    /// 被主动停掉（`kill_session`、切 plan 模式、成员被移出 hub）。
    ///
    /// 和"过期"分开记：拿着句柄回来的人应该知道这条命令是**被停了**，
    /// 而不是自己来晚了。
    pub fn remove_terminated(&self, session_id: &str) {
        self.remove_with_reason(session_id, "terminated");
    }

    fn remove_with_reason(&self, session_id: &str, reason: &'static str) {
        let removed = self
            .sessions
            .lock()
            .expect("sessions lock")
            .remove(session_id);
        if removed.is_some() {
            self.bury(session_id, reason);
        }
    }

    /// 一条会话都没有。
    ///
    /// [`crate::tools::workspace_runtime`] 拿它清理空表：主体按 OAuth
    /// `client_id` 分，跑几天的守护进程会攒下一堆没人再来读的空壳。
    pub fn is_empty(&self) -> bool {
        self.sessions.lock().expect("sessions lock").is_empty()
    }

    /// 表里还在跑的命令。
    ///
    /// **会先问一遍每个子进程死没死**：命令自己跑完时没人通知这边，
    /// `has_exited` 要等有人来读（`read_output`）或者超时监视器到点才更新。
    /// 不问就直接报，一条早就结束的 `cargo test` 会一直被算成"还在跑"，
    /// 那条警告就成了狼来了。
    ///
    /// 内部用 `block_on`，不能在 tokio 异步 worker 线程里调。
    pub fn running(&self) -> Vec<Arc<ExecSession>> {
        let all: Vec<Arc<ExecSession>> = self
            .sessions
            .lock()
            .expect("sessions lock")
            .values()
            .cloned()
            .collect();
        // 锁在这里就放开了：下面要等每个子进程回话，攥着锁会把同一张表上
        // 正要起命令的调用堵住。
        all.into_iter()
            .filter(|session| {
                crate::async_rt::block_on(session.refresh_status());
                !session.has_exited()
            })
            .collect()
    }

    fn session_ids(&self) -> Vec<String> {
        self.sessions
            .lock()
            .expect("sessions lock")
            .keys()
            .cloned()
            .collect()
    }

    /// 结束表里全部会话，返回成功结束的个数。
    ///
    /// 会阻塞等进程退出（每个最多 1.5 秒），内部用 `block_on`，
    /// 不能在 tokio 异步 worker 线程里调。
    pub fn terminate_all(&self) -> usize {
        self.session_ids()
            .into_iter()
            .filter(|session_id| {
                kill_session(
                    self,
                    &json!({
                        "session_id": session_id,
                        "signal": "TERM",
                        "wait_ms": 1500,
                        "max_output_bytes": 1024
                    }),
                )
                .is_ok()
            })
            .count()
    }
}

pub struct ExecSession {
    pub session_id: String,
    /// 跑的是哪条命令。只给起它的那个主体看，见
    /// [`crate::tools::workspace_runtime::WorkspaceRuntime::running_commands`]。
    pub command: String,
    pub(crate) child: AsyncMutex<Child>,
    pub stdin: AsyncMutex<Option<ChildStdin>>,
    stdin_open: Mutex<bool>,
    interactive: bool,
    /// 这个工作区的落盘计数器，和 `WorkspaceRuntime` 共用一个。
    ///
    /// 存计数器本身而不是 `WorkspaceRuntime`：后者持有会话表、会话表持有这里，
    /// 拿 `Arc<WorkspaceRuntime>` 就成环了，谁也放不掉。
    workspace_writes: Arc<AtomicU64>,
    /// 这条命令起来的那一刻，上面那个计数是多少。
    writes_at_start: u64,
    stdout: Mutex<Vec<u8>>,
    stderr: Mutex<Vec<u8>>,
    stdout_total: Mutex<usize>,
    stderr_total: Mutex<usize>,
    pub started_at: Instant,
    pub exit_code: Mutex<Option<i32>>,
    exited: AtomicBool,
    /// 进程结束的那一刻。输出的保留期从这里算起，**和命令的 timeout 无关**
    /// ——一条 10 分钟的命令跑完 3 秒就该和跑完 3 秒的短命令一样开始计时
    /// （审查 X02、方案 E）。
    finished_at: Mutex<Option<Instant>>,
    termination_reason: Mutex<Option<String>>,
    reader_tasks: AsyncMutex<Vec<crate::async_rt::JoinHandle<()>>>,
}

impl ExecSession {
    /// 测试用：没有命令文本，也不跟任何工作区的落盘计数。
    #[cfg(test)]
    pub fn new(child: Child) -> Self {
        Self::new_with_mode(child, false, String::new(), Arc::new(AtomicU64::new(0)))
    }

    pub fn new_with_mode(
        mut child: Child,
        interactive: bool,
        command: String,
        workspace_writes: Arc<AtomicU64>,
    ) -> Self {
        let session_id = Uuid::new_v4().to_string();
        let stdin = child.stdin.take();
        let stdin_open = stdin.is_some();
        let writes_at_start = workspace_writes.load(Ordering::Acquire);
        Self {
            session_id,
            command,
            child: AsyncMutex::new(child),
            stdin: AsyncMutex::new(stdin),
            stdin_open: Mutex::new(stdin_open),
            interactive,
            workspace_writes,
            writes_at_start,
            stdout: Mutex::new(Vec::new()),
            stderr: Mutex::new(Vec::new()),
            stdout_total: Mutex::new(0),
            stderr_total: Mutex::new(0),
            started_at: Instant::now(),
            exit_code: Mutex::new(None),
            exited: AtomicBool::new(false),
            finished_at: Mutex::new(None),
            termination_reason: Mutex::new(None),
            reader_tasks: AsyncMutex::new(Vec::new()),
        }
    }

    pub async fn spawn_readers(self: &Arc<Self>) {
        let stdout = {
            let mut guard = self.child.lock().await;
            guard.stdout.take()
        };
        let stderr = {
            let mut guard = self.child.lock().await;
            guard.stderr.take()
        };
        if let Some(stream) = stdout {
            let session = Arc::clone(self);
            let task = crate::async_rt::spawn(async move {
                session.read_stream(stream, true).await;
            });
            self.reader_tasks.lock().await.push(task);
        }
        if let Some(stream) = stderr {
            let session = Arc::clone(self);
            let task = crate::async_rt::spawn(async move {
                session.read_stream(stream, false).await;
            });
            self.reader_tasks.lock().await.push(task);
        }
    }

    pub async fn wait_for_readers(&self) {
        let mut tasks = self.reader_tasks.lock().await;
        while let Some(task) = tasks.pop() {
            let _ = tokio::time::timeout(std::time::Duration::from_millis(500), task).await;
        }
    }

    async fn read_stream<T>(&self, mut stream: T, is_stdout: bool)
    where
        T: tokio::io::AsyncRead + Unpin,
    {
        let mut buf = [0u8; 4096];
        loop {
            match stream.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = &buf[..n];
                    if is_stdout {
                        let mut data = self.stdout.lock().expect("stdout lock");
                        data.extend_from_slice(chunk);
                        *self.stdout_total.lock().expect("stdout_total lock") += n;
                        trim_buffer(&mut data, SESSION_BUFFER_BYTES);
                    } else {
                        let mut data = self.stderr.lock().expect("stderr lock");
                        data.extend_from_slice(chunk);
                        *self.stderr_total.lock().expect("stderr_total lock") += n;
                        trim_buffer(&mut data, SESSION_BUFFER_BYTES);
                    }
                }
                Err(_) => break,
            }
        }
    }

    pub async fn kill_and_wait(&self) {
        let status = {
            let mut child = self.child.lock().await;
            if let Some(pid) = child.id() {
                signal_process_tree(pid, "KILL");
            }
            let _ = child.start_kill();
            child.wait().await.ok()
        };
        if let Some(status) = status {
            self.record_exit_status(status);
        }
    }

    pub async fn refresh_status(&self) {
        let mut child = self.child.lock().await;
        if let Ok(Some(status)) = child.try_wait() {
            self.record_exit_status(status);
        }
    }

    fn record_exit_status(&self, status: std::process::ExitStatus) {
        *self.exit_code.lock().expect("exit_code lock") = status.code();
        self.exited.store(true, Ordering::Release);
        let mut finished = self.finished_at.lock().expect("finished_at lock");
        if finished.is_none() {
            *finished = Some(Instant::now());
        }
        drop(finished);
        *self.stdin_open.lock().expect("stdin_open lock") = false;
        let mut reason = self.termination_reason.lock().expect("termination lock");
        if reason.is_none() {
            *reason = Some("exited".into());
        }
    }

    pub(crate) fn has_exited(&self) -> bool {
        self.exited.load(Ordering::Acquire)
    }

    /// 进程结束了多久。还在跑就是 `None`。
    pub fn finished_ago(&self) -> Option<Duration> {
        self.finished_at
            .lock()
            .expect("finished_at lock")
            .map(|at| at.elapsed())
    }

    /// 还能读多久。还在跑的会话没有这个数——保留期从进程结束才开始算。
    pub fn expires_in(&self, retention: Duration) -> Option<Duration> {
        self.finished_ago().map(|ago| retention.saturating_sub(ago))
    }

    /// 这条命令起来之后，工作区落过几次盘。
    pub fn workspace_writes_since_start(&self) -> u64 {
        self.workspace_writes
            .load(Ordering::Acquire)
            .saturating_sub(self.writes_at_start)
    }

    pub fn mark_termination_reason(&self, reason: &str) {
        *self.termination_reason.lock().expect("termination lock") = Some(reason.to_string());
    }

    pub(crate) fn mark_stdin_closed(&self) {
        *self.stdin_open.lock().expect("stdin_open lock") = false;
    }

    pub async fn is_running(&self) -> bool {
        self.refresh_status().await;
        !self.has_exited()
    }

    pub fn retained_stream_bytes(&self, stream: &str) -> (Vec<u8>, usize) {
        match stream {
            "stderr" => {
                let data = self.stderr.lock().expect("stderr lock").clone();
                let total = *self.stderr_total.lock().expect("stderr_total lock");
                (data, total)
            }
            _ => {
                let data = self.stdout.lock().expect("stdout lock").clone();
                let total = *self.stdout_total.lock().expect("stdout_total lock");
                (data, total)
            }
        }
    }

    /// 命令此刻怎样了：(termination_reason, exit_code, command_ok)。不碰输出缓冲。
    pub fn termination(&self) -> (String, Option<i32>, Option<bool>) {
        let exit_code = *self.exit_code.lock().expect("exit_code lock");
        let reason = self
            .termination_reason
            .lock()
            .expect("termination lock")
            .clone()
            .unwrap_or_else(|| "running".into());
        let command_ok = match reason.as_str() {
            "exited" => Some(exit_code.is_some_and(|code| code == 0)),
            "running" => None,
            _ => Some(false),
        };
        (reason, exit_code, command_ok)
    }

    pub fn snapshot(&self, max_output_bytes: usize) -> Value {
        let stdout_bytes = self.stdout.lock().expect("stdout lock").clone();
        let stderr_bytes = self.stderr.lock().expect("stderr lock").clone();
        let stdout = truncate_tail(&stdout_bytes, max_output_bytes);
        let stderr = truncate_tail(&stderr_bytes, max_output_bytes);
        let status = if self.has_exited() {
            "exited"
        } else {
            "running"
        };
        let (reason, exit_code, command_ok) = self.termination();
        let reason = reason.as_str();
        json!({
            "session_id": self.session_id,
            "interactive": self.interactive,
            "stdin_open": *self.stdin_open.lock().expect("stdin_open lock"),
            "status": status,
            "termination_reason": reason,
            "recoverable": matches!(reason, "timeout" | "killed" | "spawn_failed" | "server_restart"),
            "suggestion": match reason {
                "timeout" => "读取保留输出，调整 timeout_ms 后重试",
                "killed" => "确认终止原因后重新执行命令",
                "exited" => "检查 exit_code 和 stderr",
                "crashed" => "检查 stderr 后重试或恢复工作区",
                _ => "继续读取 session 或等待进程结束",
            },
            "exit_code": exit_code,
            "transport_ok": true,
            "command_ok": command_ok,
            "stdout": stdout.content,
            "stderr": stderr.content,
            "stdout_truncated": stdout.truncated,
            "stderr_truncated": stderr.truncated,
            "elapsed_ms": self.started_at.elapsed().as_millis(),
            // 进程结束多久了。还在跑就是 null。保留期还剩多少由 read_output
            // 回（那里知道 store 的 retention），这里只给事实。
            "finished_ms_ago": self.finished_ago().map(|ago| ago.as_millis() as u64),
            // 这条命令起来之后，这个工作区落过几次盘（apply_patch 提交一次算
            // 一次）。**不是 0 就说明命令读到的文件和现在的不一样**——它可能
            // 编译了旧代码，也可能中途读到了改了一半的文件树。后台命令期间
            // 没有写互斥，这个数就是事后判断结果可不可信的唯一依据。
            "workspace_writes_since_start": self.workspace_writes_since_start(),
            "output_refs": {
                "stdout": format!("session:{}:stdout", self.session_id),
                "stderr": format!("session:{}:stderr", self.session_id)
            }
        })
    }
}

fn trim_buffer(buf: &mut Vec<u8>, limit: usize) {
    if buf.len() > limit {
        let drop = buf.len() - limit;
        buf.drain(..drop);
    }
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
    let output_ref = args
        .get("output_ref")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("output_ref is required"))?;
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
    let session = store.get(session_id)?;
    crate::async_rt::block_on(session.refresh_status());

    let requested_stream = args.get("stream").and_then(Value::as_str).unwrap_or("");
    let stream = if ref_stream == "stdout" || ref_stream == "stderr" {
        ref_stream
    } else if requested_stream == "stdout" || requested_stream == "stderr" {
        requested_stream
    } else {
        "stdout"
    };

    let (data, total_stream_bytes) = session.retained_stream_bytes(stream);
    let requested_offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = crate::tools::args::bounded(args, "read_output", "limit") as usize;
    let mut warnings: Vec<String> = Vec::new();
    if ref_stream == "full" {
        warnings.push(
            "legacy full output_ref defaults to stdout; use output_refs for stable stream paging"
                .into(),
        );
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
    let running = !session.has_exited();
    // 还有留着的字节没给完才有下一页。命令还在跑、但此刻没有新字节时，
    // next_offset 是空——不是"读完了"，而是"现在没有更多"，`complete`
    // 那一格说的才是流有没有结束。
    let next_offset = (next < total_stream_bytes).then_some(next as u64);
    // 命令怎么结束的，和 exec_command 同一套字段。转后台的命令，这是调用方（和台账、
    // 任务验收）知道它退出码的唯一途径；以前这里只说 running 与否（审查 D03）。
    let (termination_reason, exit_code, command_ok) = session.termination();

    Ok(tool_ok(json!({
        "session_id": session_id,
        "termination_reason": termination_reason,
        "exit_code": exit_code,
        "command_ok": command_ok,
        "workspace_writes_since_start": session.workspace_writes_since_start(),
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
        "expires_in_ms": session
            .expires_in(store.retention())
            .map(|left| left.as_millis() as u64),
        "retention_ms": store.retention().as_millis() as u64,
        "truncated": next_offset.is_some(),
        "warnings": warnings
    })))
}

/// 一次 `write_stdin` 最多等多久。
///
/// 管道的缓冲是有限的（Linux 64 KiB，macOS 更小）。命令**不读** stdin 时，
/// 写满之后这一写就再也不返回——而它跑在 `block_on` 里，等于把这次工具调用
/// 永久挂住，调用方连"它没在读"都不知道（方案 E：写入阻塞也必须受
/// timeout/cancel 管理，验收 A17）。
const STDIN_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// 分段写，超时就停下并**如实说写进去了多少**。
///
/// 不用 `write_all`：它超时之后没法知道已经写了几个字节，而"写了一半"和
/// "一个字节都没写"对调用方是两件事。
async fn write_stdin_bounded(stdin: &mut ChildStdin, bytes: &[u8]) -> Result<(), WorkspaceError> {
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

/// 这段字节里，最后一个完整 UTF-8 字符结束的位置。
///
/// 全是完整字符就是它自己的长度；末尾挂着半个字符就退到那个字符之前。
fn utf8_boundary(bytes: &[u8]) -> usize {
    match std::str::from_utf8(bytes) {
        Ok(_) => bytes.len(),
        Err(error) => error.valid_up_to(),
    }
}

pub fn write_stdin(store: &SessionStore, args: &Value) -> Result<Value, WorkspaceError> {
    let session_id = args
        .get("session_id")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("session_id is required"))?;
    let session = store.get(session_id)?;
    let chars = args.get("chars").and_then(Value::as_str).unwrap_or("");
    let max_output_bytes =
        crate::tools::args::bounded(args, "write_stdin", "max_output_bytes") as usize;

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

    let yield_ms = crate::tools::args::bounded(args, "write_stdin", "yield_time_ms");
    std::thread::sleep(std::time::Duration::from_millis(yield_ms));
    crate::async_rt::block_on(session.refresh_status());
    Ok(tool_ok(session.snapshot(max_output_bytes)))
}

pub fn kill_session(store: &SessionStore, args: &Value) -> Result<Value, WorkspaceError> {
    let session_id = args
        .get("session_id")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("session_id is required"))?;
    let session = store.get(session_id)?;
    let max_output_bytes =
        crate::tools::args::bounded(args, "kill_session", "max_output_bytes") as usize;
    let wait_ms = crate::tools::args::bounded(args, "kill_session", "wait_ms");
    let signal = args.get("signal").and_then(Value::as_str).unwrap_or("TERM");

    let running = crate::async_rt::block_on(session.is_running());
    let mut killed = false;
    let mut status = "exited";
    let mut evicted = true;

    if running {
        session.mark_termination_reason("killed");
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
            status = "terminating";
            evicted = false;
        } else {
            killed = true;
            status = "killed";
        }
    }

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

/// Signal the command and everything it started.
///
/// exec_command puts each command in its own process group whose id is the
/// command's pid, so the group is signalled first. A process that was not
/// started that way has no such group (the kernel does not hand out a pid
/// while a group with that id exists), and gets the signal alone. A child
/// that moved itself to another group or session on purpose is out of reach.
#[cfg(unix)]
fn signal_process_tree(pid: u32, signal: &str) {
    let sig = match signal {
        "KILL" => libc::SIGKILL,
        "INT" => libc::SIGINT,
        _ => libc::SIGTERM,
    };
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return;
    };
    unsafe {
        if libc::kill(-pid, sig) != 0 {
            libc::kill(pid, sig);
        }
    }
}

/// `taskkill /T` walks the child list; TerminateProcess alone would leave
/// the grandchildren. The signal name has no Windows equivalent: it is
/// always a forced stop. Not run on Windows in this change, only compiled
/// by CI.
#[cfg(windows)]
fn signal_process_tree(pid: u32, _signal: &str) {
    use std::os::windows::process::CommandExt;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};

    const CREATE_NO_WINDOW: u32 = 0x08000000;
    let tree = std::process::Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status();
    if tree.is_ok_and(|status| status.success()) {
        return;
    }
    unsafe {
        if let Ok(handle) = OpenProcess(PROCESS_TERMINATE, false, pid) {
            let _ = TerminateProcess(handle, 1);
            let _ = CloseHandle(handle);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::json;

    /// 造一条已经跑完的会话。用 `true` 是因为它立刻退出，不用等、也不用 sleep。
    fn finished_session(store: &SessionStore) -> String {
        let child = crate::async_rt::block_on(async {
            tokio::process::Command::new("true")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("起 true")
        });
        let session = store.insert(ExecSession::new(child));
        // 等它真的退出并记下结束时刻：保留期从这一刻起算。
        crate::async_rt::block_on(async {
            let _ = session.child.lock().await.wait().await;
        });
        crate::async_rt::block_on(session.refresh_status());
        session.session_id.clone()
    }

    /// 过期的句柄和瞎编的句柄不是一回事：前者重跑命令，后者是自己记错了。
    ///
    /// 保留期设成 0，所以不用真的等 5 分钟，也没有 sleep（验收 A16）。
    #[test]
    fn an_expired_handle_is_not_the_same_as_an_unknown_one() {
        let store = SessionStore::with_retention(Duration::ZERO);
        let id = finished_session(&store);

        let Err(expired) = store.get(&id) else {
            panic!("保留期是 0，这条该没了");
        };
        assert_eq!(expired.code(), "SESSION_EXPIRED", "{}", expired.message());
        assert!(
            expired.message().contains("Re-run"),
            "要说清下一步：{}",
            expired.message()
        );

        let Err(unknown) = store.get("00000000-0000-0000-0000-000000000000") else {
            panic!("这个 id 从来没存在过");
        };
        assert_eq!(unknown.code(), "SESSION_NOT_FOUND");
    }

    /// 还在跑的会话不会被保留期扫掉——保留期从**进程结束**才开始算。
    #[test]
    fn a_running_session_is_never_swept() {
        let store = SessionStore::with_retention(Duration::ZERO);
        let child = crate::async_rt::block_on(async {
            tokio::process::Command::new("sleep")
                .arg("30")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("起 sleep")
        });
        let id = store.insert(ExecSession::new(child)).session_id.clone();

        assert!(store.get(&id).is_ok(), "在跑的会话被当成过期的清掉了");
        let session = store.get(&id).expect("还在");
        assert!(session.finished_ago().is_none());
        assert!(session.expires_in(Duration::from_secs(300)).is_none());

        crate::async_rt::block_on(session.kill_and_wait());
    }

    /// 结束的会话超过配额时，**结束得最早的**先走，还在跑的一条都不动。
    #[test]
    fn finished_sessions_are_capped_and_the_oldest_goes_first() {
        let store = SessionStore::with_retention(Duration::from_secs(3600));
        let mut ids = Vec::new();
        for _ in 0..(MAX_FINISHED_SESSIONS + 4) {
            ids.push(finished_session(&store));
        }
        let alive = ids.iter().filter(|id| store.get(id).is_ok()).count();
        assert!(
            alive <= MAX_FINISHED_SESSIONS,
            "配额没生效，还留着 {alive} 条"
        );
        // 最早那几条应当已经被回收，而且报的是"过期"而不是"没见过"。
        let Err(first) = store.get(&ids[0]) else {
            panic!("最早那条该被挤掉");
        };
        assert_eq!(first.code(), "SESSION_EXPIRED", "{}", first.message());
        assert!(
            store.get(ids.last().expect("最后一条")).is_ok(),
            "刚结束的那条不该被挤掉"
        );
    }

    /// 命令不读 stdin 时，写不进去要**有个头**，而且要说清写进去了多少。
    ///
    /// 不封顶的话这次调用永远不返回（它跑在 block_on 里），调用方连"对面没在
    /// 读"都不知道（验收 A17 的输入背压）。这里直接调有界写，传一个比管道
    /// 缓冲大得多的块，对面是个从不读 stdin 的 `sleep`。
    #[test]
    fn writing_to_a_command_that_never_reads_stdin_gives_up_and_says_how_far_it_got() {
        let mut child = crate::async_rt::block_on(async {
            tokio::process::Command::new("sleep")
                .arg("30")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("起 sleep")
        });
        let mut stdin = child.stdin.take().expect("stdin");
        // 8 MiB：任何平台的管道缓冲都装不下，所以一定会卡在中途。
        let payload = vec![b'x'; 8 * 1024 * 1024];
        let started = Instant::now();
        let error = crate::async_rt::block_on(async {
            write_stdin_bounded(&mut stdin, &payload)
                .await
                .expect_err("对面不读，这一写不该成功")
        });
        assert_eq!(error.code(), "STDIN_WRITE_TIMEOUT", "{}", error.message());
        assert!(
            error.message().contains("not reading stdin"),
            "{}",
            error.message()
        );
        assert!(
            started.elapsed() < STDIN_WRITE_TIMEOUT + Duration::from_secs(5),
            "超时没起作用，等了 {:?}",
            started.elapsed()
        );
        crate::async_rt::block_on(async {
            let _ = child.kill().await;
        });
    }

    /// `read_output` 要说清这份输出还能读多久。
    #[test]
    fn read_output_says_how_long_the_output_still_lives() {
        let store = SessionStore::with_retention(Duration::from_secs(300));
        let id = finished_session(&store);
        let out = read_output(
            &store,
            &json!({ "output_ref": format!("session:{id}:stdout") }),
        )
        .expect("read_output");
        assert_eq!(out["retention_ms"], 300_000u64);
        let left = out["expires_in_ms"]
            .as_u64()
            .expect("已结束的会话要给这个数");
        assert!(left > 0 && left <= 300_000, "{out}");
    }
}

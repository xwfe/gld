use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStdin};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use crate::tools::runs::{RunLog, RunRecord, RunWriter};
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
    /// 运行记录（审查 D09）。有它时每条命令在数据目录里留记录和日志，内存里找不到的
    /// 会话从这里读。`None` 是只在内存里的老样子，单元测试用。
    runs: Option<RunLog>,
    /// 这个项目的落盘计数器（和会话里拿的是同一个）。从记录读结果时拿它现算"起跑到现在
    /// gld 写过几次"，见 [`SessionStore::writes_since_start`]。
    workspace_writes: Option<Arc<AtomicU64>>,
}

impl Default for SessionStore {
    fn default() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            graveyard: Mutex::new(VecDeque::new()),
            retention: SESSION_RETENTION,
            runs: None,
            workspace_writes: None,
        }
    }
}

/// 按 `session_id` 找到的一条命令：还在这个进程的内存里，或者只剩运行记录。
pub enum Found {
    Live(Arc<ExecSession>),
    Recorded(Box<RunRecord>),
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 带运行记录的会话表。[`crate::tools::workspace_runtime`] 给每个主体建的都是这种。
    /// `workspace_writes` 是这个项目的落盘计数器。
    pub fn with_runs(runs: RunLog, workspace_writes: Arc<AtomicU64>) -> Self {
        Self {
            runs: Some(runs),
            workspace_writes: Some(workspace_writes),
            ..Self::default()
        }
    }

    /// 从记录读到的命令，起跑之后 gld 往工作区写过几次。算不出来是 `None`。
    ///
    /// **不能用记录里结束那一刻的数**：命令跑完之后 gld 再改源文件，那个数还是 0，
    /// 从记录读到的"退出 0、期间没写过"就会被任务验收当成现在这份代码的证据（独立审查
    /// 发现）。所以按起跑时的计数器现算；那个计数器只在内存里，换了进程（gld 重启过）
    /// 就算不出来，给 `None`——任务验收会拒收，要求重跑。
    fn writes_since_start(&self, record: &RunRecord) -> Option<u64> {
        if record.owner != crate::tools::runs::instance_id() {
            return None;
        }
        let at_start = record.writes_at_start?;
        let now = self.workspace_writes.as_ref()?.load(Ordering::Acquire);
        Some(now.saturating_sub(at_start))
    }

    pub fn runs(&self) -> Option<&RunLog> {
        self.runs.as_ref()
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
        // 有运行记录时走到这里，说明盘上那份也没了：被配额或年龄清掉，或者当初就没写成。
        let why = if self.runs.is_some() {
            format!(
                "{why}, and its run record is gone too (each project keeps its last {} finished commands for at most {} days)",
                crate::tools::runs::MAX_FINISHED_RUNS,
                crate::tools::runs::MAX_AGE.as_secs() / 86_400
            )
        } else {
            why
        };
        // 以前这里说"重跑命令"。输出没了不等于命令没跑：它可能已经改了文件、发了
        // 请求、跑完了迁移。原样重跑有副作用的命令就是做第二遍（审查 D05）。
        Some(WorkspaceError::ToolDetails {
            code: "SESSION_EXPIRED",
            message: format!(
                "Session {session_id} existed but {why} ({seconds}s ago). The command already ran; only its output cannot be recovered. If it changes anything (files, services, data), check the current state before running it again; re-run only commands that are safe to repeat."
            ),
            category: "not_found",
            retryable: false,
            details: json!({
                "reason": stone.reason,
                "retention_seconds": self.retention.as_secs(),
                "released_seconds_ago": seconds,
                "executed": true,
                "output_recoverable": false
            }),
        })
    }

    pub fn insert(&self, mut session: ExecSession) -> Arc<ExecSession> {
        self.sweep();
        if let Some(runs) = &self.runs {
            session.run = runs.start(
                &session.session_id,
                &session.command,
                session.pid,
                session.writes_at_start,
            );
        }
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

    /// 先找内存，找不到再找运行记录（审查 D09）。
    ///
    /// 内存里没有的几种情况都会走到盘上：结束超过保留期、被 `kill_session` 停掉、
    /// 守护进程重启过、直连模式下是上一条命令行起的。别的主体的记录照样找不到，
    /// 报的和从来没有这个 id 一样。
    pub fn lookup(&self, session_id: &str) -> Result<Found, WorkspaceError> {
        match self.get(session_id) {
            Ok(session) => Ok(Found::Live(session)),
            // 内存里没有，那本进程起的也不是真在跑：用 load_detached。
            Err(error) => match self
                .runs
                .as_ref()
                .and_then(|runs| runs.load_detached(session_id))
            {
                Some(record) => Ok(Found::Recorded(Box::new(record))),
                None => Err(error),
            },
        }
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

    /// 结束表里全部会话，返回真正停掉的条数（早就跑完的不算）。`reason` 记进终态：
    /// `killed`（有人要停），或 `interrupted`（gld 自己要退出，见运行记录的说明）。
    ///
    /// 先 TERM 等 1.5 秒，不走再 KILL 等 1 秒：拦着 TERM 的命令不能因此成了孤儿——gld 退出后
    /// 它就没人管了，结局也只能记成 unknown。
    ///
    /// 会阻塞等进程退出，内部用 `block_on`，不能在 tokio 异步 worker 线程里调。
    pub fn terminate_all(&self, reason: &str) -> usize {
        self.session_ids()
            .into_iter()
            .filter(|session_id| {
                let Ok(session) = self.get(session_id) else {
                    return false;
                };
                let (mut killed, mut status, mut evicted) =
                    stop_session(&session, "TERM", 1500, reason);
                if status == "terminating" {
                    (killed, status, evicted) = stop_session(&session, "KILL", 1000, reason);
                }
                if evicted {
                    self.remove_terminated(session_id);
                }
                killed || status == "terminating"
            })
            .count()
    }
}

pub struct ExecSession {
    pub session_id: String,
    /// 跑的是哪条命令。只给起它的那个主体看，见
    /// [`crate::tools::workspace_runtime::WorkspaceRuntime::running_commands`]。
    pub command: String,
    /// 起来时的 pid，进运行记录。
    pid: Option<u32>,
    /// 运行记录的写入端。会话表建着运行记录时由 [`SessionStore::insert`] 装上。
    run: Option<RunWriter>,
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
            pid: child.id(),
            run: None,
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
                    if let Some(run) = &self.run {
                        run.append(is_stdout, chunk);
                    }
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
        // 整段握着 termination 锁：[`ExecSession::mark_termination_reason`] 也拿它，
        // 两边谁先谁后都看得到对方，内存和运行记录里的终态才对得上（独立审查发现：
        // 以前 kill 判断"还在跑"之后进程自己退了，记录写 exited、内存却被改成 killed）。
        let mut reason = self.termination_reason.lock().expect("termination lock");
        *self.exit_code.lock().expect("exit_code lock") = status.code();
        self.exited.store(true, Ordering::Release);
        let mut finished = self.finished_at.lock().expect("finished_at lock");
        let first = finished.is_none();
        if first {
            *finished = Some(Instant::now());
        }
        drop(finished);
        *self.stdin_open.lock().expect("stdin_open lock") = false;
        if reason.is_none() {
            *reason = Some("exited".into());
        }
        let reason = reason.clone().unwrap_or_else(|| "exited".into());
        // 结束的那一刻就落盘，不等有人来读：之后 gld 重启了，这份记录就是唯一知道结局的地方。
        // timeout / killed / interrupted 是停之前先标上的，所以这里拿到的已经是最终原因。
        if first {
            if let Some(run) = &self.run {
                run.finish(&reason, status.code(), self.workspace_writes_since_start());
            }
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

    /// 停它之前标上原因。**已经结束了就不改**：结局已经按实际情况记下（也写进了运行
    /// 记录），再改成 killed 就和记录对不上。
    pub fn mark_termination_reason(&self, reason: &str) {
        let mut current = self.termination_reason.lock().expect("termination lock");
        if !self.has_exited() {
            *current = Some(reason.to_string());
        }
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
        let command_ok = command_ok(&reason, exit_code);
        (reason, exit_code, command_ok)
    }

    /// 这条会话有没有在写运行记录。
    pub fn kept_on_disk(&self) -> bool {
        self.run.is_some()
    }

    pub fn snapshot(&self, max_output_bytes: usize) -> Value {
        let stdout = self.stdout.lock().expect("stdout lock").clone();
        let stderr = self.stderr.lock().expect("stderr lock").clone();
        let (reason, exit_code, command_ok) = self.termination();
        snapshot_value(
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
        )
    }
}

/// 按终态算命令成没成。结局不知道（`unknown`）和还在跑一样，不说成也不说败。
fn command_ok(reason: &str, exit_code: Option<i32>) -> Option<bool> {
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
fn recorded_snapshot(store: &SessionStore, record: &RunRecord, max_output_bytes: usize) -> Value {
    let runs = store.runs().expect("只有带运行记录的会话表才找得到记录");
    let (stdout, _) = runs.stream_bytes(record, "stdout");
    let (stderr, _) = runs.stream_bytes(record, "stderr");
    let now = crate::tools::runs::now_ms();
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

/// 以这个 pid 为组号的进程组现在有没有进程。命令起来时自成一组（组号 = 它的 pid）。
#[cfg(unix)]
fn process_group_exists(pid: u32) -> Option<bool> {
    let pid = libc::pid_t::try_from(pid).ok()?;
    // 发 0 号信号只查不发。EPERM 说明有这个组、只是不归我们管，也算在。
    let alive = unsafe { libc::kill(-pid, 0) } == 0
        || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
    Some(alive)
}

#[cfg(not(unix))]
fn process_group_exists(_pid: u32) -> Option<bool> {
    None
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
    let found = store.lookup(session_id)?;

    let requested_stream = args.get("stream").and_then(Value::as_str).unwrap_or("");
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
    let limit = crate::tools::args::bounded(args, "read_output", "limit") as usize;
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
    Ok(tool_ok(result))
}

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
    let limit = crate::tools::args::bounded(args, "list_runs", "limit") as usize;
    let statuses: Option<Vec<&str>> = match args.get("status") {
        None | Some(Value::Null) => None,
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
        "max_finished_runs": crate::tools::runs::MAX_FINISHED_RUNS,
        "max_age_days": crate::tools::runs::MAX_AGE.as_secs() / 86_400,
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

    let now = crate::tools::runs::now_ms();
    let live: HashMap<String, Arc<ExecSession>> =
        store.sessions.lock().expect("sessions lock").clone();
    let mut matched: Vec<(RunRecord, String, Option<i32>)> = Vec::new();
    for record in runs.list() {
        if within_ms.is_some_and(|within| now.saturating_sub(record.started_at_ms) > within) {
            continue;
        }
        // 还在本进程内存里的，以内存为准：结束检查每 200 毫秒一次，记录可能还没来得及写。
        let (reason, exit_code) = match live.get(&record.session_id) {
            Some(session) => {
                crate::async_rt::block_on(session.refresh_status());
                let (reason, exit_code, _) = session.termination();
                (reason, exit_code)
            }
            None => (record.status.clone(), record.exit_code),
        };
        if statuses
            .as_ref()
            .is_some_and(|wanted| !wanted.contains(&reason.as_str()))
        {
            continue;
        }
        matched.push((record, reason, exit_code));
    }

    let total = matched.len();
    let mut warnings: Vec<String> = Vec::new();
    let start = match &cursor {
        None => 0,
        Some((at, id)) => {
            if !matched
                .iter()
                .any(|(record, ..)| record.started_at_ms == *at && record.session_id == *id)
            {
                warnings.push(
                    "the run at the cursor is no longer listed (cleaned up, or it no longer matches the filters); this page continues from its position".into(),
                );
            }
            matched
                .iter()
                .position(|(record, ..)| (record.started_at_ms, &record.session_id) < (*at, id))
                .unwrap_or(matched.len())
        }
    };
    let page: Vec<Value> = matched
        .iter()
        .skip(start)
        .take(limit)
        .map(|(record, reason, exit_code)| {
            let id = &record.session_id;
            let running = reason == "running";
            json!({
                "session_id": id,
                "command": record.command,
                "termination_reason": reason,
                "running": running,
                "exit_code": exit_code,
                "command_ok": command_ok(reason, *exit_code),
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
                "started_by_this_gld": record.owner == crate::tools::runs::instance_id(),
                "started_by_gld_pid": record.owner_pid,
            })
        })
        .collect();
    let returned = page.len();
    let next_cursor = (start + returned < total)
        .then(|| matched.get(start + returned - 1))
        .flatten()
        .map(|(record, ..)| format!("{}:{}", record.started_at_ms, record.session_id));
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

/// `list_runs` 的游标：`<起跑毫秒>:<session_id>`。
fn parse_run_cursor(cursor: &str) -> Option<(u64, String)> {
    let (at, id) = cursor.split_once(':')?;
    let at = at.parse().ok()?;
    uuid::Uuid::parse_str(id).ok()?;
    Some((at, id.to_string()))
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
    let chars = args.get("chars").and_then(Value::as_str).unwrap_or("");
    let max_output_bytes =
        crate::tools::args::bounded(args, "write_stdin", "max_output_bytes") as usize;
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
    let max_output_bytes =
        crate::tools::args::bounded(args, "kill_session", "max_output_bytes") as usize;
    let wait_ms = crate::tools::args::bounded(args, "kill_session", "wait_ms");
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
fn stop_session(
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

    /// 过期的句柄和瞎编的句柄不是一回事：前者命令确实跑过、只是输出没了，
    /// 后者是自己记错了。
    ///
    /// 过期时不能一句"重跑"了事：命令已经执行过，有副作用的原样重跑就是做第二遍
    /// （审查 D05）。下一步要先核对现状。
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
            expired.message().contains("already ran")
                && expired.message().contains("check the current state"),
            "要说清命令已经跑过、先核对现状再决定重跑：{}",
            expired.message()
        );
        let details = &expired.to_error_value()["details"];
        assert_eq!(details["executed"], true);
        assert_eq!(details["output_recoverable"], false);

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

    /// 从记录读结果时，"起跑之后 gld 写过几次"按计数器现算：同一个进程里算得出（命令结束后
    /// 再写也算进去）；别的进程（gld 重启前）留下的算不出，给 null，不拿结束那一刻的 0 充数。
    #[test]
    fn a_record_from_another_process_does_not_claim_the_workspace_was_untouched() {
        let home = tempfile::tempdir().expect("home");
        let runs = RunLog::new(home.path().join("runs"), "p", "local");
        let counter = Arc::new(AtomicU64::new(5));
        let store = SessionStore::with_runs(runs.clone(), Arc::clone(&counter));
        let id = Uuid::new_v4().to_string();
        let writer = runs.start(&id, "true", None, 5).expect("建记录");
        writer.finish("exited", Some(0), 0);
        counter.fetch_add(2, Ordering::AcqRel);
        let args = json!({ "output_ref": format!("session:{id}:stdout") });

        let same = read_output(&store, &args).expect("read_output");
        assert_eq!(same["source"], "run_record");
        assert_eq!(same["workspace_writes_since_start"], 2, "{same}");

        crate::tools::runs::orphan_record(&runs, &id);
        let other = read_output(&store, &args).expect("read_output");
        assert_eq!(other["termination_reason"], "exited", "{other}");
        assert!(other["workspace_writes_since_start"].is_null(), "{other}");
    }

    /// 本进程起的、记录停在 running、内存里却没有：不是真在跑（结束时没记下来），按 unknown
    /// 说，别说成"另一个进程还在跑它"——那个进程就是自己。
    #[test]
    fn my_own_running_record_that_is_not_in_memory_is_unknown() {
        let home = tempfile::tempdir().expect("home");
        let runs = RunLog::new(home.path().join("runs"), "p", "local");
        let store = SessionStore::with_runs(runs.clone(), Arc::new(AtomicU64::new(0)));
        let id = Uuid::new_v4().to_string();
        let _writer = runs.start(&id, "sleep 30", Some(1), 0).expect("建记录");
        let out = read_output(
            &store,
            &json!({ "output_ref": format!("session:{id}:stdout") }),
        )
        .expect("read_output");
        assert_eq!(out["termination_reason"], "unknown", "{out}");
        assert_eq!(out["running"], false, "{out}");
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

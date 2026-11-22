//! 命令会话的状态、调用方内存表与保留期。
//!
//! 进程、输出、控制和列表在各自模块实现，共享这里的状态；运行记录目录由组合侧传入。

mod control;
mod list;
mod output;
mod process;
mod process_control;

pub use control::{kill_session, write_stdin};
pub use list::list_runs;
pub use output::read_output;

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use tokio::process::{Child, ChildStdin};
use tokio::sync::Mutex as AsyncMutex;

use crate::runs::{RunLog, RunRecord, RunWriter};
use crate::workspace::WorkspaceError;
use control::stop_session;

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
    /// 上面那个计数器是接着哪些进程数下来的，见 [`Self::writes_since_start`]。
    counted_by: Option<Arc<Mutex<HashMap<String, u64>>>>,
    /// 过了保留期、从表里摘掉，但留下的进程还占着输出管道的命令。这一代退出时管道就断了，
    /// 那些进程下次写输出会被 SIGPIPE 带走，所以还得记着，见 [`Self::has_unsettled_output`]。
    left_behind: Mutex<Vec<Arc<ExecSession>>>,
}

impl Default for SessionStore {
    fn default() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            graveyard: Mutex::new(VecDeque::new()),
            retention: SESSION_RETENTION,
            runs: None,
            workspace_writes: None,
            counted_by: None,
            left_behind: Mutex::new(Vec::new()),
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

    /// 带运行记录的会话表。[`crate::workspace_runtime`] 给每个主体建的都是这种。
    /// `workspace_writes` 是这个项目的落盘计数器。
    pub fn with_runs(runs: RunLog, workspace_writes: Arc<AtomicU64>) -> Self {
        Self {
            runs: Some(runs),
            workspace_writes: Some(workspace_writes),
            ..Self::default()
        }
    }

    /// 工作区换代时计数器是接着上一代数的，上一代起的命令也算得出来。
    pub fn counted_by(mut self, processes: Arc<Mutex<HashMap<String, u64>>>) -> Self {
        self.counted_by = Some(processes);
        self
    }

    /// 从记录读到的命令，起跑之后 gld 往工作区写过几次。算不出来是 `None`。
    ///
    /// **不能用记录里结束那一刻的数**：命令跑完之后 gld 再改源文件，那个数还是 0，
    /// 从记录读到的"退出 0、期间没写过"就会被任务验收当成现在这份代码的证据（独立审查
    /// 发现）。所以按起跑时的计数器现算；那个计数器只在内存里，换了进程（gld 重启过）
    /// 就算不出来，给 `None`——任务验收会拒收，要求重跑。
    fn writes_since_start(&self, record: &RunRecord) -> Option<u64> {
        // 上一代的命令只有交接之前起的才算数。之后起的说明这次交接其实没成（等回复超时、
        // 工作区留在了那一代），它期间的写入这个计数器没数到。
        let same_count = record.owner == crate::runs::instance_id()
            || self.counted_by.as_ref().is_some_and(|processes| {
                processes
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .get(&record.owner)
                    .is_some_and(|exported_at| record.started_at_ms <= *exported_at)
            });
        if !same_count {
            return None;
        }
        let at_start = record.writes_at_start?;
        let now = self.workspace_writes.as_ref()?.load(Ordering::Acquire);
        now.checked_sub(at_start)
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
            if let Some(session) = sessions.remove(&id) {
                self.keep_if_left_behind(session);
            }
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
                crate::runs::MAX_FINISHED_RUNS,
                crate::runs::MAX_AGE.as_secs() / 86_400
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
                session.service_port,
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
        if let Some(session) = removed {
            self.keep_if_left_behind(session);
            self.bury(session_id, reason);
        }
    }

    /// 这个连接自己起的、还在跑的、声明了这个 `service_port` 的命令。
    ///
    /// 起新服务前端口被占时用它说出"是你自己那条"，AI 就不会再起一个、也不会去猜是谁。
    pub fn serving(&self, port: u16) -> Option<Arc<ExecSession>> {
        let all: Vec<Arc<ExecSession>> = self
            .sessions
            .lock()
            .expect("sessions lock")
            .values()
            .cloned()
            .collect();
        all.into_iter().find(|session| {
            session.service_port == Some(port) && {
                session.try_refresh_status();
                !session.has_exited()
            }
        })
    }

    /// 一条会话都没有。
    ///
    /// [`crate::workspace_runtime`] 拿它清理空表：主体按 OAuth
    /// `client_id` 分，跑几天的守护进程会攒下一堆没人再来读的空壳。
    pub fn is_empty(&self) -> bool {
        self.sessions.lock().expect("sessions lock").is_empty() && !self.has_left_behind()
    }

    fn keep_if_left_behind(&self, session: Arc<ExecSession>) {
        if session.readers_alive() {
            self.left_behind
                .lock()
                .expect("left_behind lock")
                .push(session);
        }
    }

    fn has_left_behind(&self) -> bool {
        let mut left_behind = self.left_behind.lock().expect("left_behind lock");
        left_behind.retain(|session| session.readers_alive());
        !left_behind.is_empty()
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

    /// 这一代现在退出会不会弄坏什么。
    ///
    /// - 命令退了，它留下的进程还占着输出管道（`npm run dev &`）：这边一退管道就断，那个进程下次
    ///   写输出被 SIGPIPE 带走。过了保留期也一样，所以一直算到它自己退出。重定向走了的
    ///   （`nohup … > log &`）不占管道，读任务已经收尾，不算。
    /// - 没有运行记录（盘满时）：输出只在内存里，换代就读不到了。保留期一过内存里也没了，就不算了。
    pub fn has_unsettled_output(&self) -> bool {
        let sessions: Vec<Arc<ExecSession>> = self
            .sessions
            .lock()
            .expect("sessions lock")
            .values()
            .cloned()
            .collect();
        sessions
            .iter()
            .any(|session| session.run.is_none() || session.readers_alive())
            || self.has_left_behind()
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
    /// [`crate::workspace_runtime::WorkspaceRuntime::running_commands`]。
    pub command: String,
    /// 起来时的 pid，进运行记录。
    pid: Option<u32>,
    /// 服务模式声明的端口（`exec_command` 的 `service_port`），快照里据此现查它答不答话。
    service_port: Option<u16>,
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
    /// 等过一轮还没读完的。不再等它们，但要知道它们还在，见 [`SessionStore::has_unsettled_output`]。
    slow_readers: Mutex<Vec<crate::async_rt::JoinHandle<()>>>,
}

#[cfg(all(test, unix))]
mod tests;

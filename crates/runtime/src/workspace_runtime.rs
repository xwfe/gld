//! 一个工作区目录的执行资源归谁所有。
//!
//! 本层管理同目录上的写锁、命令会话与交接状态。应用层负责选择存储路径、
//! 传入项目标识，并把命令状态与 Agent 等应用资源一起用于换代判断。
//!
//! 为什么要有这么个东西：补丁落盘以前靠 `patch.rs` 里一把 `static` Mutex 串
//! 起来，那是**整个进程一把**——hub 里项目 A 打补丁，项目 B 得排队等它，而
//! 两个项目根本在不同目录。反过来，真正需要排队的场景（同一个目录经 hub、
//! 单项目 listener、CLI 三个入口进来）靠"碰巧在同一个进程里"才成立，没有人
//! 明说过这件事，也就随时会被下一次重构弄没。
//!
//! 所以把它变成一件说得出口的事：**一个目录一个 [`WorkspaceRuntime`]，谁要
//! 写这个目录就得先找它拿锁，谁的命令会话也在它这儿按主体分表存着。**
//!
//! # 管得到什么
//!
//! - **同一个进程里对同一个目录的写**，不管是从 hub、单工作区 listener 还是
//!   CLI 进来的，也不管中间 `ToolContext` 重建过几次。
//! - **另一个 gld 进程**——守护进程和 CLI 同时在跑、开了两个实例。这一层是 OS
//!   文件锁，锁文件在数据目录（`GLD_HOME`，默认 `~/.config/gld`）下的
//!   `write-locks/`，进程死掉时由内核释放，不留要人工清的残留。
//! - **每条命令起来的那一刻**。`exec_command` 先拿到写权才 spawn 进程，所以
//!   命令看到的文件树不会是别人写到一半的——一个补丁改三个文件**不是**原子的
//!   （原子的只有单个文件的替换）。转后台那一路（`yield_time_ms: 0`）占的就
//!   只有这一瞬间。
//! - **`exec_command` 同步等结果的那一段**，也就是从命令起来到这次调用返回。
//!   命令在 `yield_time_ms`（默认 1 秒，上限 30 秒）之内跑完，那就是它的全程；
//!   期间补丁得排队。挡的是"一边跑命令一边改源文件"——那种交叉出来的结果没法
//!   解释，命令读到的是半新半旧的文件。
//! - **命令会话归谁**。`exec_command` 起的命令记在"目录 + 调用方主体"这张表
//!   里（[`gld_policy::CallerId`]）。同一个主体换个入口进来读得到自己
//!   的 `session_id`，换个主体就完全看不见——`read_output` 那边报的是
//!   `SESSION_NOT_FOUND`，和"这个 id 根本不存在"长得一模一样，不给枚举机会。
//!
//! 等写权最多等 [`WRITE_LOCK_WAIT`]，超了就报 `WORKSPACE_BUSY`，不挂着。
//!
//! # 管不到什么（**别当成已经隔离了**）
//!
//! - **转到后台之后的命令**。`yield_time_ms` 到了命令还没跑完，`exec_command`
//!   就带着 `session_id` 先返回，命令继续跑到 `timeout_ms`（上限十分钟）——**那
//!   一段没有写互斥**。
//!
//!   这不是漏了，是刻意的：后台命令没有终点，占着写权等于把目录锁到天亮，
//!   `npm run dev` 起来之后谁也别想改代码了，而那恰恰是日常工作流。
//!
//!   **补的是让两边都看得见**，不是挡住：
//!
//!   - 命令那边——会话快照里的 `workspace_writes_since_start` 说"你起来之后这
//!     个工作区落过几次盘"。不是 0 就意味着它编译/测试的可能是改之前的代码。
//!   - 写的那边——`apply_patch` 的 `warnings` 说"这儿还有几条命令在跑"，自己
//!     起的给 `session_id`（停得掉），别人起的只给数量。
//!
//!   看得见不等于挡得住：谁也不挡谁，要不要停、认不认那条结果，判断留给调用方。
//! - **命令到底写没写文件，这里不猜**。`cargo build`、`npm install` 写文件，
//!   `ls` 不写，靠命令文本判断只会漏判——漏判比不做更糟，它给人"已经协调了"
//!   的错觉。所以按运行形态一刀切：同步等结果的都占，不管它实际写不写，代价
//!   是 `sleep 60` 这种纯等待的命令也占着写权。
//! - **不同数据目录的两个 gld**。锁文件按 `GLD_HOME` 存放，两个进程用不同的
//!   `GLD_HOME` 写同一个目录，就是两个互不知晓的写域。同一台机器上要共用执行
//!   权威，`GLD_HOME` 必须一致。ccnm 那边有同样的约束，两边近期都不打算为此
//!   造一套跨产品的锁服务。
//! - **同一个入口上认不出"两个人"的那些情况**。会话表按主体分，但匿名连接
//!   （`auth_type=noauth`）和共用一条 bearer 令牌的客户端本来就只有一个主体，
//!   它们之间没有隔离可言。哪些分得开、哪些分不开，列在
//!   [`gld_policy::CallerId`]。
//! - **不是 gld 的写者**——编辑器、`git checkout`、另一个 AI 工具。文件锁是
//!   劝告锁（advisory），不参与的人照写不误。那一侧靠的是补丁的版本前置条件
//!   和落盘前复核，两者都不是强 CAS。
//! - **Git 元数据**。同一个仓库的两个 worktree 是两个目录、两个
//!   `WorkspaceRuntime`，但它们共享同一份 `.git`。改文件互不影响，`git
//!   commit` 这类动 common directory 的操作会撞车。ccnm 那边的做法是把资源
//!   键取到 Git common directory，代价是相关 worktree 一起串行；这里先不
//!   跟，因为 gld 目前没有并行 worktree 的编排。要加的时候是"工作树文件"和
//!   "共享 Git 元数据"两级协调，不是把键一换了事。

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use serde::{Deserialize, Serialize};

use crate::session::SessionStore;
use gld_policy::CallerId;

mod locks;

pub use locks::{write_lock_busy, CommitGuard, WRITE_LOCK_WAIT};

/// 应用选择的运行记录位置；项目标识在应用层与任务账本共用同一个算法。
#[derive(Debug, Clone)]
pub struct RunStorage {
    pub root: PathBuf,
    pub project_id: String,
}

/// 创建目录资源时的显式存储位置；缺省时仅保留进程内写锁和内存会话。
///
/// 同一个规范化目录只创建一次资源，之后继续复用首次创建时的路径。
#[derive(Debug, Clone, Default)]
pub struct RuntimePaths {
    pub write_lock_dir: Option<PathBuf>,
    pub runs: Option<RunStorage>,
}

/// 一个工作区目录的执行资源。
///
/// 两件东西：写锁，以及这个目录上的命令会话表。
///
/// **会话表按调用方分，一个 [`CallerId`] 一张。**不跟着 `ToolContext` 走，
/// 所以两件事同时成立：
///
/// - 同一个主体不管经哪个 `ToolContext` 进来，看到的是同一张表。hub 里
///   `context_for` 因为配置指纹变了重建一次上下文，正在跑的后台命令和它的
///   `session_id` 都还在。
/// - 不同主体看到的是不同的表，**互相根本看不见**。A 拿着 B 的 `session_id`
///   去读，得到 `SESSION_NOT_FOUND`，跟"这个 id 不存在"一个样子，不给枚举
///   机会。分得开哪些主体、分不开哪些，写在 [`gld_policy::CallerId`]。
pub struct WorkspaceRuntime {
    commit_lock: Mutex<()>,
    /// 这个目录上的命令会话表，按调用方分。
    sessions: Mutex<HashMap<CallerId, Arc<SessionStore>>>,
    /// 这个目录落过多少次盘（`apply_patch` 提交一次算一次）。
    ///
    /// 只增不减，绕回去要 5.8e11 年。正在跑的命令记下自己起来时的值，差值就是
    /// "我跑的这段时间里工作区被改过几次"——后台命令期间没有写互斥，这个数是
    /// 事后判断命令结果可不可信的唯一依据。
    writes: Arc<AtomicU64>,
    /// 计数器接着哪些进程数下来的：实例 id → 它把计数交出来的时刻（毫秒）。工作区换代时这份
    /// 和 `writes` 一起带过来，那些进程留下的运行记录才算得出"起跑到现在写过几次"，
    /// 见 [`WorkspaceHandoff`] 和 `SessionStore::writes_since_start`。
    counted_by: Arc<Mutex<HashMap<String, u64>>>,
    /// 交接带过来、接过来的 `writes` 值。收到过同一份之后再收一次（上次等回复超时）不算冲突。
    adopted_writes: Mutex<Option<u64>>,
    /// 交接带过来、还没被上下文取走的默认目录，键是入口（`mcp`、`local` 这些）。
    adopted_cwds: Mutex<BTreeMap<String, PathBuf>>,
    /// 跨进程那一层的锁文件。
    ///
    /// `None` 表示建不出来（数据目录不可写之类）。那时只剩进程内互斥，
    /// [`WorkspaceRuntime::lock_commits`] 会记一条 warn——为什么是降级而不是
    /// 拒绝写，说明在那里。
    lock_file: Option<PathBuf>,
    /// 应用传入的记录位置与项目标识；`None` 时命令只留在内存里。
    runs: Option<RunStorage>,
    /// 每个文件最近一段连续由 gld 写出来的版本，和写它的是谁。见 [`WorkspaceRuntime::wrote_since`]。
    own_writes: Mutex<HashMap<String, OwnWrites>>,
}

/// 同一个调用方经 gld 接连写同一个文件留下的版本，旧的在前，最后一个是现在磁盘上的。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct OwnWrites {
    /// [`CallerId::key`]。只保存稳定身份键；权限快照和凭据不进入执行层或工作区交接状态。
    writer: String,
    versions: Vec<String>,
}

/// 工作区从一代搬到下一代时带走的状态（RFC-0008 §3.4）。
///
/// 只有不带走就会答错的东西才在这里：落盘计数（任务验收靠它判断结果还作不作数）、
/// 自己写过的版本（补丁靠它放行没人动过的旧版本号）、默认目录（丢了命令会跑错地方）。
/// 跑完的命令在运行记录里，任务和计划本来就在盘上，都不用带。
///
/// 两代可能不是同一个版本，**加字段要带 `serde(default)`**；不兼容的改动要加
/// [`gld_protocol::generation::STATE_FORMAT`]，否则新代读不懂就留在旧代，这个工作区再也换不过去。
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct WorkspaceHandoff {
    writes: u64,
    counted_by: HashMap<String, u64>,
    #[serde(default)]
    own_writes: HashMap<String, OwnWrites>,
    #[serde(default)]
    default_cwds: BTreeMap<String, PathBuf>,
}

/// 一个文件最多记多少个版本。够一轮对话里改同一个文件几十次。
const OWN_WRITE_VERSIONS: usize = 32;

/// 排障时想知道的就这两件：跨进程那把锁落在哪个文件，以及这个目录上有几个
/// 主体还挂着会话。**不派生 `Debug`**：里面的 `SessionStore` 一路连着
/// `tokio::process::Child`，整个打出来既没人看又可能把命令输出带进日志。
impl std::fmt::Debug for WorkspaceRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let callers = self
            .sessions
            .lock()
            .map(|sessions| sessions.len())
            .unwrap_or(0);
        f.debug_struct("WorkspaceRuntime")
            .field("lock_file", &self.lock_file)
            .field("session_tables", &callers)
            .finish()
    }
}

/// 一个目录上还在跑的命令，从某个调用方的角度看过去。
#[derive(Debug, Default)]
pub struct RunningCommands {
    /// 这个调用方自己起的。它读得到这些输出，也 kill 得掉。
    pub mine: Vec<RunningCommand>,
    /// 别人起的有几条。**故意只给数量**，理由见
    /// [`WorkspaceRuntime::running_commands`]。
    pub others: usize,
}

impl RunningCommands {
    pub fn total(&self) -> usize {
        self.mine.len() + self.others
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }
}

/// 一条还在跑的命令。
#[derive(Debug, Clone)]
pub struct RunningCommand {
    pub session_id: String,
    pub command: String,
    pub running_ms: u128,
}

impl WorkspaceRuntime {
    /// 锁文件在哪；没有就是跨进程那一层没拿到。测试和排障用。
    pub fn lock_file_path(&self) -> Option<&Path> {
        self.lock_file.as_deref()
    }

    /// 记一次落盘。`apply_patch` 真写进去之后调，`dry_run` 不调。
    ///
    /// 为什么不挂在写锁释放上：`exec_command` 同步跑一条 `ls` 也占写锁，按
    /// 释放计数的话它会被算成一次写，正在跑的命令那边就看到一个虚高的数字，
    /// 于是这个数字就不能用来判断"我的结果还作不作数"了。
    pub fn record_write(&self) {
        self.writes.fetch_add(1, Ordering::AcqRel);
    }

    /// 记下 `caller` 经 gld 把 `path` 从 `before` 写成了 `after`。
    ///
    /// 接得上（上一笔是同一个调用方写的、写出来的正是这次的 `before`）就接在后面；
    /// 接不上（中间有人在 gld 之外改过、换了人写、新建的文件）就从这一笔重新记。
    pub fn record_own_write(
        &self,
        caller: &CallerId,
        path: &str,
        before: Option<&str>,
        after: Option<&str>,
    ) {
        let mut writes = self
            .own_writes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(after) = after else {
            writes.remove(path);
            return;
        };
        let continues = writes.get(path).is_some_and(|known| {
            known.writer == caller.key() && known.versions.last().map(String::as_str) == before
        });
        if !continues {
            writes.insert(
                path.to_string(),
                OwnWrites {
                    writer: caller.key().to_string(),
                    versions: before.into_iter().map(str::to_string).collect(),
                },
            );
        }
        let known = writes.get_mut(path).expect("just inserted");
        known.versions.push(after.to_string());
        if known.versions.len() > OWN_WRITE_VERSIONS {
            let extra = known.versions.len() - OWN_WRITE_VERSIONS;
            known.versions.drain(..extra);
        }
    }

    /// `path` 从版本 `then` 变成现在的 `now`，是不是全是 `caller` 自己经 gld 写的。
    ///
    /// 补丁带的 `expected_versions` 对不上时问这个：AI 改完一个文件，下一个补丁还带着改之前
    /// 的版本号，是最常见的补丁失败——记录里 9 次版本冲突全是这样，gld 几分钟前刚写过同一个
    /// 文件（审查 §31）。这时没人动过它改的东西，拒了只会让它白重读一遍。别人写过、gld 之外
    /// 改过、gld 重启过（这张表只在内存里），都答"不是"，照旧拒。
    pub fn wrote_since(&self, caller: &CallerId, path: &str, then: &str, now: &str) -> bool {
        let writes = self
            .own_writes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        writes.get(path).is_some_and(|known| {
            known.writer == caller.key()
                && known.versions.last().map(String::as_str) == Some(now)
                && known.versions[..known.versions.len() - 1]
                    .iter()
                    .any(|version| version == then)
        })
    }

    /// 落盘计数器本身，交给新起的命令记下"我起来时是多少"。
    ///
    /// 给的是计数器而不是 `Arc<WorkspaceRuntime>`：后者持有会话表、会话表持有
    /// 命令，拿整个 runtime 就成环了。
    pub fn write_counter(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.writes)
    }

    /// 这个目录上还在跑的命令，按"是不是你起的"分开。
    ///
    /// **别人的只给个数。**命令文本是另一个主体的东西，刚按主体把会话分开，
    /// 转头把命令行摊开给所有人看就白分了。数量本身不泄露内容，而它是模型
    /// 判断"我现在改文件安不安全"要的最低信息。
    ///
    /// 内部会挨个问子进程死没死，用 `block_on`，不能在 tokio 异步 worker
    /// 线程里调。
    pub fn running_commands(&self, caller: &CallerId) -> RunningCommands {
        let tables: Vec<(bool, Arc<SessionStore>)> = {
            let sessions = self
                .sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            sessions
                .iter()
                .map(|(owner, store)| (owner == caller, Arc::clone(store)))
                .collect()
        };
        let mut mine = Vec::new();
        let mut others = 0usize;
        for (is_mine, store) in tables {
            for session in store.running() {
                if is_mine {
                    mine.push(RunningCommand {
                        session_id: session.session_id.clone(),
                        command: session.command.clone(),
                        running_ms: session.started_at.elapsed().as_millis(),
                    });
                } else {
                    others += 1;
                }
            }
        }
        RunningCommands { mine, others }
    }

    /// 这个调用方在这个目录上的命令会话表；没有就建一张。
    ///
    /// 同一个 `caller` 拿到的永远是同一张表，跟他经哪个 `ToolContext` 进来
    /// 无关。换一个 `caller` 就是另一张表，两边互相看不见。
    pub fn sessions_for(&self, caller: &CallerId) -> Arc<SessionStore> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // 顺手扔掉没人要的空表。主体按 OAuth client_id 分，守护进程一跑几天
        // 就会攒下一堆再也不会有人来读的空壳。
        //
        // 两个条件缺一不可，**别改成只看 `is_empty()`**：并发请求下，线程 A
        // 刚取走一张新建的空表还没来得及往里塞会话，线程 B 就会把它从这里清
        // 掉；A 随后起的命令落在一张脱了钩的表上，`terminate_all_sessions`
        // 和切 plan 模式再也停不掉它。`strong_count > 1` 说的就是"还有人拿着
        // 它"——取表时的 `Arc::clone` 和这里的清理都在同一把锁里，所以看到的
        // 计数是准的。
        sessions.retain(|_, store| Arc::strong_count(store) > 1 || !store.is_empty());
        Arc::clone(sessions.entry(caller.clone()).or_insert_with(|| {
            Arc::new(match &self.runs {
                Some(storage) => SessionStore::with_runs(
                    crate::runs::RunLog::new(
                        storage.root.clone(),
                        &storage.project_id,
                        caller.key(),
                    ),
                    Arc::clone(&self.writes),
                )
                .counted_by(Arc::clone(&self.counted_by)),
                None => SessionStore::new(),
            })
        }))
    }

    /// 本层哪些资源还不能交接；应用层还需合并 Agent 等其他资源的状态。
    ///
    /// 调的时候宿主已经挡住了发给它的新请求，也确认过没有在途调用（`Router::hand_off`），
    /// 所以这里查到空，之后也不会有新命令或新写入。内部要问子进程，不能在异步 worker 线程里调。
    pub fn handoff_blockers(&self) -> Vec<&'static str> {
        let stores: Vec<Arc<SessionStore>> = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .cloned()
            .collect();
        let mut blockers = Vec::new();
        if stores.iter().any(|store| !store.running().is_empty()) {
            blockers.push("running_commands");
        } else if stores.iter().any(|store| store.has_unsettled_output()) {
            blockers.push("unsettled_output");
        }
        if matches!(
            self.commit_lock.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        ) {
            blockers.push("write_in_progress");
        }
        blockers
    }

    /// 把要带走的状态抄一份。只读：下一代没接住时这个工作区照旧归这一代。
    pub fn handoff_state(&self, default_cwds: BTreeMap<String, PathBuf>) -> WorkspaceHandoff {
        let mut counted_by = self
            .counted_by
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        counted_by.insert(
            crate::runs::instance_id().to_string(),
            crate::runs::now_ms(),
        );
        WorkspaceHandoff {
            writes: self.writes.load(Ordering::Acquire),
            counted_by,
            own_writes: self
                .own_writes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone(),
            default_cwds,
        }
    }

    /// 接住上一代带来的状态。这一代已经在这个目录上干过活就拒绝：它的计数和带来的对不上，
    /// 硬接的话验收会拿错的数判断结果还作不作数。
    pub fn adopt_handoff(&self, state: WorkspaceHandoff) -> Result<(), String> {
        let tables_in_use = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .any(|store| !store.is_empty());
        let mut adopted = self
            .adopted_writes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let expected = adopted.unwrap_or(0);
        if tables_in_use || self.writes.load(Ordering::Acquire) != expected {
            return Err("这一代已经在这个目录上干过活，不能接手".to_string());
        }
        // 上次等回复超时、之后又发了新的，两条到的顺序可能反过来。计数不能倒退。
        if state.writes < expected {
            return Err("这是一份过期的交接".to_string());
        }
        self.writes.store(state.writes, Ordering::Release);
        *adopted = Some(state.writes);
        drop(adopted);
        self.counted_by
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .extend(state.counted_by);
        *self
            .own_writes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = state.own_writes;
        *self
            .adopted_cwds
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = state.default_cwds;
        Ok(())
    }

    /// 这个入口的上下文要不要从交接带来的默认目录开始。取一次就没了：之后以上下文自己的为准。
    pub fn take_adopted_cwd(&self, entry: &str) -> Option<PathBuf> {
        self.adopted_cwds
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(entry)
    }

    /// 还留着的会话表张数（顺手清掉空的）。测试用。
    #[cfg(test)]
    fn session_tables(&self) -> usize {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        sessions.retain(|_, store| !store.is_empty());
        sessions.len()
    }

    /// 停掉这个目录上所有还在跑的命令，返回停掉的条数。
    ///
    /// 切到 plan 模式时用：那时说好了"只看不动手"，还在跑的命令得停——
    /// 不分是谁起的，因为模式是整个目录的。
    pub fn terminate_all_sessions(&self) -> usize {
        self.terminate_sessions(|_| true, "killed")
    }

    /// 只停掉某一个入口在这个目录上起的命令，返回停掉的条数。
    ///
    /// hub 停掉、成员被移出 hub 时用：收回的是"经这个入口访问"的权限，
    /// 命令行和工作区自己的监听器起的命令不该被连累。
    pub fn terminate_sessions_in_scope(&self, scope: &str) -> usize {
        self.terminate_sessions(|caller| caller.scope() == scope, "killed")
    }

    /// `reason` 进命令的终态和运行记录：有人要停是 `killed`，gld 自己要退出是 `interrupted`。
    fn terminate_sessions(&self, matches: impl Fn(&CallerId) -> bool, reason: &str) -> usize {
        let stores: Vec<Arc<SessionStore>> = {
            let sessions = self
                .sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            sessions
                .iter()
                .filter(|(owner, _)| matches(owner))
                .map(|(_, store)| Arc::clone(store))
                .collect()
        };
        // 锁在这里就放开了：停命令要等子进程收尾（每个最多 1.5 秒），攥着锁会
        // 把同目录上正要取表的调用一起堵住。
        stores
            .into_iter()
            .map(|store| store.terminate_all(reason))
            .sum()
    }
}

/// 规范化后的目录路径 → 资源。
///
/// **为什么路径就够当身份用**：`canonicalize` 解掉符号链接和 `..` 是明面上的，
/// 不那么明显的是它还会把拼法纠正成磁盘上的真名——大小写不敏感的文件系统上
/// （macOS 的 APFS、Windows 的 NTFS），`.../CASECHECK` 规范化之后就是
/// `.../casecheck`。所以 `/x/gld` 和 `/x/GLD` 不会变成两把锁。这条是实测出来
/// 的，下面有一条测试钉着它；哪天哪个平台不这么干了，那条会红。
///
/// **为什么不用 Unix 的 `(dev, ino)`**：它对 bind mount 更准，但目录被删了重建
/// （`git clone` 重来、`rm -rf && mkdir`）时 inode 会变，同一个路径就分裂成两
/// 个资源——那正是要避免的第二写者，而删了重建比 bind mount 常见得多。
///
/// **为什么是 `Arc` 而不是 `Weak`**：用 `Weak` 的话，最后一个 `ToolContext` 被
/// 丢掉的瞬间这个目录的锁就没了，下一个进来的重新建一把——两次创建之间要是
/// 还有别人在写，又是两个互不知晓的写者。工作区是用户登记出来的，数量有限，
/// 一把空 Mutex 的内存不值得拿这个风险换。
static RUNTIMES: LazyLock<Mutex<HashMap<PathBuf, Arc<WorkspaceRuntime>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 停掉某个 grant 在所有目录上起的命令，返回停掉的条数（`gld grant rm`，RFC-0007）。
///
/// 撤销之后它的令牌下一次请求就 401，可已经起来的后台命令（`npm run dev` 这种）
/// 不会自己停：没人再能读它、停它，它就一直以你的身份跑着。
pub fn terminate_grant_sessions(grant_id: &str) -> usize {
    terminate_sessions_everywhere(|caller| caller.grant_id() == Some(grant_id), "killed")
}

/// 停掉这个进程里所有目录上还在跑的命令，返回停掉的条数。守护进程退出、直连模式下命令行
/// 退出时用。
///
/// 服务停的时候只收它自己入口起的；`gld tool call` 起的（本机主体）不归任何服务，以前守护
/// 进程退出也没人收，一直以你的身份跑到自己的 timeout——守护进程都没了，谁也读不到、停不掉。
///
/// 终态记 `interrupted`，不是 `killed`：不是谁要停它，是 gld 自己要走了。重启之后读运行
/// 记录的人靠这个分清两者（审查 D09）。
pub fn terminate_all_sessions_everywhere() -> usize {
    terminate_sessions_everywhere(|_| true, "interrupted")
}

fn terminate_sessions_everywhere(
    matches: impl Fn(&CallerId) -> bool + Copy,
    reason: &str,
) -> usize {
    let runtimes: Vec<Arc<WorkspaceRuntime>> = RUNTIMES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .values()
        .cloned()
        .collect();
    runtimes
        .iter()
        .map(|runtime| runtime.terminate_sessions(matches, reason))
        .sum()
}

/// 每个目录上在跑几条命令，没有命令的不列。升级状态报告用：只有数量，不带命令内容。
///
/// 要挨个问子进程死没死（内部 `block_on`），不能在 tokio 异步 worker 线程里调。
pub fn running_session_counts() -> Vec<(PathBuf, usize)> {
    let runtimes: Vec<(PathBuf, Arc<WorkspaceRuntime>)> = RUNTIMES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .map(|(root, runtime)| (root.clone(), Arc::clone(runtime)))
        .collect();
    runtimes
        .into_iter()
        .filter_map(|(root, runtime)| {
            let count = runtime
                .running_commands(&CallerId::local_operator())
                .total();
            (count > 0).then_some((root, count))
        })
        .collect()
}

/// 这个进程里还有没有退出就会弄坏的输出，见 [`SessionStore::has_unsettled_output`]。
pub fn any_unsettled_output() -> bool {
    let runtimes: Vec<Arc<WorkspaceRuntime>> = RUNTIMES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .values()
        .cloned()
        .collect();
    runtimes.iter().any(|runtime| {
        let stores: Vec<Arc<SessionStore>> = runtime
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .cloned()
            .collect();
        stores.iter().any(|store| store.has_unsettled_output())
    })
}

/// 拿这个目录的执行资源；没有就建一个。
///
/// 同一个目录不管经过几个入口、换过几次 `ToolContext`，拿到的都是同一个
/// [`WorkspaceRuntime`]。
///
/// 这里自己 `canonicalize` 一次，不指望调用方传规范化路径：漏掉一个调用点的
/// 后果不是报错，而是那个入口悄悄拿到第二把锁，事后很难发现。规范化失败
/// （目录不在了、权限不够）就按原样当键——宁可多一把锁，也不要把两个不同的
/// 目录认成一个。
pub fn runtime_for(root: &Path, paths: RuntimePaths) -> Arc<WorkspaceRuntime> {
    let key = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut runtimes = RUNTIMES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Arc::clone(runtimes.entry(key).or_insert_with_key(|key| {
        Arc::new(WorkspaceRuntime {
            commit_lock: Mutex::new(()),
            sessions: Mutex::new(HashMap::new()),
            writes: Arc::new(AtomicU64::new(0)),
            counted_by: Arc::new(Mutex::new(HashMap::new())),
            adopted_writes: Mutex::new(None),
            adopted_cwds: Mutex::new(BTreeMap::new()),
            lock_file: locks::lock_path_for(key, paths.write_lock_dir.as_deref()),
            runs: paths.runs,
            own_writes: Mutex::new(HashMap::new()),
        })
    }))
}

#[cfg(test)]
mod tests;

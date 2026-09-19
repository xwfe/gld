//! 一个工作区目录的执行资源归谁所有。
//!
//! 先说清楚它**不是**什么：跟 `crate::runtime`（`RuntimeSupervisor`）没关系，
//! 那个管的是 MCP/Actions 这类服务条目的启停。这里管的是"同一个目录上的
//! 写操作怎么排队"。
//!
//! 为什么要有这么个东西：补丁落盘以前靠 `patch.rs` 里一把 `static` Mutex 串
//! 起来，那是**整个进程一把**——hub 里项目 A 打补丁，项目 B 得排队等它，而
//! 两个项目根本在不同目录。反过来，真正需要排队的场景（同一个目录经 hub、
//! 单项目 listener、CLI 三个入口进来）靠"碰巧在同一个进程里"才成立，没有人
//! 明说过这件事，也就随时会被下一次重构弄没。
//!
//! 所以把它变成一件说得出口的事：**一个目录一个 [`WorkspaceRuntime`]，谁要
//! 写这个目录就得先找它拿锁。**
//!
//! # 管得到什么
//!
//! - **同一个进程里对同一个目录的写**，不管是从 hub、单工作区 listener 还是
//!   CLI 进来的，也不管中间 `ToolContext` 重建过几次。
//! - **另一个 gld 进程**——守护进程和 CLI 同时在跑、开了两个实例。这一层是 OS
//!   文件锁，锁文件在数据目录（`GLD_HOME`，默认 `~/.config/gld`）下的
//!   `write-locks/`，进程死掉时由内核释放，不留要人工清的残留。
//! - **`exec_command` 同步等结果的那一段**，也就是从命令起来到这次调用返回。
//!   命令在 `yield_time_ms`（默认 1 秒，上限 30 秒）之内跑完，那就是它的全程；
//!   期间补丁得排队。挡的是"一边跑命令一边改源文件"——那种交叉出来的结果没法
//!   解释，命令读到的是半新半旧的文件。
//!
//! 等写权最多等 [`WRITE_LOCK_WAIT`]，超了就报 `WORKSPACE_BUSY`，不挂着。
//!
//! # 管不到什么（**别当成已经隔离了**）
//!
//! - **转到后台之后的命令**。`yield_time_ms` 到了命令还没跑完，`exec_command`
//!   就带着 `session_id` 先返回，命令继续跑到 `timeout_ms`（上限十分钟）——**那
//!   一段没有写权保护**。传 `yield_time_ms: 0` 的那一路连锁都不取。
//!
//!   这不是漏了，是刻意的：后台命令没有终点，占着写权等于把目录锁到天亮，
//!   `npm run dev` 起来之后谁也别想改代码了，而那恰恰是日常工作流。现在的效果
//!   是把选择权交给调用方——要保护就同步等，要撒手就让它转后台。代价是"跑一个
//!   五分钟的测试，同时改源文件"这件事拦不住，只有前几秒拦得住。
//! - **命令到底写没写文件，这里不猜**。`cargo build`、`npm install` 写文件，
//!   `ls` 不写，靠命令文本判断只会漏判——漏判比不做更糟，它给人"已经协调了"
//!   的错觉。所以按运行形态一刀切：同步等结果的都占，不管它实际写不写，代价
//!   是 `sleep 60` 这种纯等待的命令也占着写权。
//! - **不同数据目录的两个 gld**。锁文件按 `GLD_HOME` 存放，两个进程用不同的
//!   `GLD_HOME` 写同一个目录，就是两个互不知晓的写域。同一台机器上要共用执行
//!   权威，`GLD_HOME` 必须一致。ccnm 那边有同样的约束，两边近期都不打算为此
//!   造一套跨产品的锁服务。
//! - **不是 gld 的写者**——编辑器、`git checkout`、另一个 AI 工具。文件锁是
//!   劝告锁（advisory），不参与的人照写不误。那一侧靠的是补丁的版本前置条件
//!   和落盘前复核，两者都不是强 CAS。
//! - **Git 元数据**。同一个仓库的两个 worktree 是两个目录、两个
//!   `WorkspaceRuntime`，但它们共享同一份 `.git`。改文件互不影响，`git
//!   commit` 这类动 common directory 的操作会撞车。ccnm 那边的做法是把资源
//!   键取到 Git common directory，代价是相关 worktree 一起串行；这里先不
//!   跟，因为 gld 目前没有并行 worktree 的编排。要加的时候是"工作树文件"和
//!   "共享 Git 元数据"两级协调，不是把键一换了事。

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use fs2::FileExt;

use crate::tools::session::SessionStore;

/// 等写权最多等这么久，超了就告诉调用方"有人正在写"。
///
/// 30 秒是这么来的：补丁落盘是毫秒级的，等它根本用不了这么久；真正会等的是
/// `exec_command` 同步等结果的那一段，而它最长就是 `yield_time_ms` 的上限——
/// 也是 30 秒。两个数对齐，意思是"该等的都等过了还没轮到，那就是真堵着"，
/// 这时候告诉模型一声比继续挂着强：客户端那边先超时断开，这边还占着线程。
pub const WRITE_LOCK_WAIT: Duration = Duration::from_secs(30);

/// 等锁时的轮询间隔。
///
/// 进程内那把锁 std 没给带超时的 `lock`，跨进程那把 `fs2` 也只有非阻塞版，
/// 要有上限就只能轮询。20 毫秒是拍的：补丁之间互相等时最多多花这么点，对
/// 毫秒级的操作不痛不痒，又不至于把 CPU 转起来。
const WRITE_LOCK_POLL: Duration = Duration::from_millis(20);

/// 一个工作区目录的执行资源。
///
/// 两件东西：写锁，以及这个目录上都有哪些命令会话表。
///
/// **会话表是"登记"不是"共享"**：每个 `ToolContext` 还是各有各的
/// `SessionStore`，A 入口起的命令 B 入口看不到。真要共享得先能回答"谁能读谁
/// 的输出"，而 gld 现在没有通用的调用方主体标识——`call_local` 只知道是哪个
/// workspace，不知道是谁在调。那是 L1 里 `AuthContext + workspace grant` 的
/// 活，不能顺手塞进来：共享了却没有主体检查，等于把别人的命令输出摊开给所有
/// 连进来的人。这里登记的用处只有一个——切 plan 模式时把这个目录上的命令全停掉。
#[derive(Debug)]
pub struct WorkspaceRuntime {
    commit_lock: Mutex<()>,
    /// 这个目录上的会话表，弱引用：`ToolContext` 没了就跟着失效，不拖着它。
    session_stores: Mutex<Vec<Weak<SessionStore>>>,
    /// 跨进程那一层的锁文件。
    ///
    /// `None` 表示建不出来（数据目录不可写之类）。那时只剩进程内互斥，
    /// [`WorkspaceRuntime::lock_commits`] 会记一条 warn——为什么是降级而不是
    /// 拒绝写，说明在那里。
    lock_file: Option<PathBuf>,
}

/// 落盘期间握着的写权，丢掉就释放。
///
/// 两层：进程内的 `Mutex` 挡住同一个进程里的其他线程，OS 文件锁挡住别的 gld
/// 进程。文件锁随 `File` 关闭而释放，进程被 kill 掉也一样（内核收 fd 的时候
/// 就放了），所以不会留下要人工清理的残留标记。
#[must_use = "丢掉 guard 就等于放开了写权"]
pub struct CommitGuard<'a> {
    _in_process: MutexGuard<'a, ()>,
    across_processes: Option<File>,
}

impl Drop for CommitGuard<'_> {
    fn drop(&mut self) {
        // 关文件本身就会解锁，这里显式来一次是跟 `history::storage::HistoryLock`
        // 保持一个写法，也免得以后有人把 File 换成别的持有方式时漏掉。
        if let Some(file) = self.across_processes.take() {
            let _ = FileExt::unlock(&file);
        }
    }
}

impl WorkspaceRuntime {
    /// 占住这个目录的写权，直到返回的 guard 被丢掉。
    ///
    /// 谁该调用：真要落盘的写操作。只读的预检（`dry_run`）不要占——预检不该
    /// 让真正的写操作排队。
    ///
    /// **会等，但有上限**：[`WRITE_LOCK_WAIT`] 之内没拿到就返回 `None`，由调
    /// 用方决定怎么报。
    ///
    /// 为什么不无限等：写权现在也会被 `exec_command` 同步等结果的那一段占着，
    /// 最长到 `yield_time_ms` 的上限。无限等的话，赶上连着几条命令，补丁就一
    /// 直挂在这儿——客户端那边先超时断开，这边还占着线程傻等。宁可早点告诉模型
    /// "有人正在写，等会儿再来"。
    pub fn lock_commits(&self) -> Option<CommitGuard<'_>> {
        self.lock_commits_within(WRITE_LOCK_WAIT)
    }

    /// 同上，但自己说等多久。测试用短的，别让一条用例干等半分钟。
    pub(crate) fn lock_commits_within(&self, wait: Duration) -> Option<CommitGuard<'_>> {
        let deadline = Instant::now() + wait;
        loop {
            // 上一个持锁的线程 panic 过：锁里存的是 `()`，没有被弄坏的状态可
            // 言，接着用就是了——在这里 panic 掉反而会把一次正常的写变成失败。
            let in_process = match self.commit_lock.try_lock() {
                Ok(guard) => Some(guard),
                Err(std::sync::TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
                Err(std::sync::TryLockError::WouldBlock) => None,
            };
            if let Some(in_process) = in_process {
                let Some(across) = self.take_file_lock_within(deadline) else {
                    // 进程内的拿到了、跨进程的没拿到：把进程内这把也放开，
                    // 不然本进程的其他写操作会被一把拿不全的锁堵着。
                    return None;
                };
                return Some(CommitGuard {
                    _in_process: in_process,
                    across_processes: across,
                });
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(WRITE_LOCK_POLL);
        }
    }

    /// 跨进程那一层，同样等到 `deadline` 为止。
    ///
    /// 返回 `Some(None)` 表示"这个目录没有锁文件"（建不出来，见
    /// [`lock_path_for`]），那是降级不是失败；`None` 才是"有锁文件但一直被别
    /// 人占着"。
    fn take_file_lock_within(&self, deadline: Instant) -> Option<Option<File>> {
        let Some(path) = self.lock_file.as_deref() else {
            return Some(None);
        };
        match take_file_lock(path, deadline) {
            FileLockOutcome::Held(file) => Some(Some(file)),
            FileLockOutcome::Degraded => Some(None),
            FileLockOutcome::Busy => None,
        }
    }

    /// 锁文件在哪；没有就是跨进程那一层没拿到。测试和排障用。
    pub fn lock_file_path(&self) -> Option<&Path> {
        self.lock_file.as_deref()
    }

    /// 把一个 `ToolContext` 的会话表登记到这个目录名下。
    ///
    /// 重复登记同一个表不会记两遍；已经没人用的旧表顺手清掉。
    pub fn register_session_store(&self, store: &Arc<SessionStore>) {
        let mut stores = self
            .session_stores
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        stores.retain(|entry| entry.strong_count() > 0);
        if !stores
            .iter()
            .filter_map(Weak::upgrade)
            .any(|registered| Arc::ptr_eq(&registered, store))
        {
            stores.push(Arc::downgrade(store));
        }
    }

    /// 还挂着的登记数（顺手清掉已经没人用的）。测试用。
    #[cfg(test)]
    fn registered_stores(&self) -> usize {
        let mut stores = self
            .session_stores
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        stores.retain(|entry| entry.strong_count() > 0);
        stores.len()
    }

    /// 停掉这个目录上所有还在跑的命令，返回停掉的条数。
    ///
    /// 切到 plan 模式时用：那时说好了"只看不动手"，还在跑的命令得停。
    pub fn terminate_all_sessions(&self) -> usize {
        let stores: Vec<Arc<SessionStore>> = {
            let mut stores = self
                .session_stores
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            stores.retain(|entry| entry.strong_count() > 0);
            stores.iter().filter_map(Weak::upgrade).collect()
        };
        // 锁在这里就放开了：停命令要等子进程收尾，攥着锁会把同目录上正要登记
        // 的新上下文一起堵住。
        stores.into_iter().map(|store| store.terminate_all()).sum()
    }
}

/// 拿这个锁文件的独占锁，拿不到就返回 `None`。
///
/// 拿不到时为什么是降级而不是让整次补丁失败：这一层是加固，不是唯一防线——
/// 同进程的互斥还在，补丁自己还有版本前置条件和落盘前复核。数据目录一时不可
/// 写（权限、磁盘满、`GLD_HOME` 指歪了）就让所有改文件的操作全部停摆，代价
/// 比它挡住的风险大。日志会说清楚是哪一步没成。
enum FileLockOutcome {
    /// 拿到了。
    Held(File),
    /// 这台机器上这一层用不了（锁文件打不开之类），只剩进程内互斥。
    Degraded,
    /// 有锁文件，但一直被别的进程占着。
    Busy,
}

fn take_file_lock(path: &Path, deadline: Instant) -> FileLockOutcome {
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
    {
        Ok(file) => file,
        Err(error) => {
            eprintln!(
                "打不开跨进程写锁文件 {}：{error}。这次写只有进程内互斥",
                path.display()
            );
            return FileLockOutcome::Degraded;
        }
    };
    loop {
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => return FileLockOutcome::Held(file),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => {
                // 不是"被占着"而是这一层根本用不了（文件系统不支持锁之类）。
                eprintln!(
                    "拿不到跨进程写锁 {}：{error}。这次写只有进程内互斥",
                    path.display()
                );
                return FileLockOutcome::Degraded;
            }
        }
        if Instant::now() >= deadline {
            return FileLockOutcome::Busy;
        }
        std::thread::sleep(WRITE_LOCK_POLL);
    }
}

/// 锁文件路径：数据目录下 `write-locks/<目录路径的哈希>.lock`。
///
/// 为什么不按目录名而按哈希：路径里什么字符都可能有，直接当文件名会撞上长度
/// 上限和非法字符。哈希撞了的后果是两个目录共用一把锁——多串行一点，不是少
/// 一层保护，所以 64 位够用。
///
/// 为什么锁文件不放在项目里：那是用户的仓库，gld 不往里塞自己的状态文件。
/// 代价写在模块文档里——不同 `GLD_HOME` 的两个进程互相看不见。
fn lock_path_for(root: &Path) -> Option<PathBuf> {
    let dir = crate::home::data_home().ok()?.join("write-locks");
    if let Err(error) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "建不出写锁目录 {}：{error}。跨进程互斥这一层不可用",
            dir.display()
        );
        return None;
    }
    let key = root.to_string_lossy();
    Some(dir.join(format!("{:016x}.lock", fnv1a(key.as_bytes()))))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// 等不到这个工作区的写权。补丁和 `exec_command` 都用它，两边报的是同一件事。
///
/// `retryable: true`：占着的人会放手，重试是对的做法。消息里点明多半是命令在
/// 跑——不说的话，模型看到"写权被占着"第一反应是去猜哪个文件被锁了，白费力气。
pub fn write_lock_busy() -> crate::tools::workspace::WorkspaceError {
    crate::tools::workspace::WorkspaceError::Tool {
        code: "WORKSPACE_BUSY",
        message: format!(
            "Another operation is writing this workspace; waited {}s. \
             A command running under exec_command holds the write lock until it finishes or times out. \
             Retry, or look at what is still running before writing again.",
            WRITE_LOCK_WAIT.as_secs()
        ),
        category: "runtime",
        retryable: true,
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

/// 拿这个目录的执行资源；没有就建一个。
///
/// 同一个目录不管经过几个入口、换过几次 `ToolContext`，拿到的都是同一个
/// [`WorkspaceRuntime`]。
///
/// 这里自己 `canonicalize` 一次，不指望调用方传规范化路径：漏掉一个调用点的
/// 后果不是报错，而是那个入口悄悄拿到第二把锁，事后很难发现。规范化失败
/// （目录不在了、权限不够）就按原样当键——宁可多一把锁，也不要把两个不同的
/// 目录认成一个。
pub fn runtime_for(root: &Path) -> Arc<WorkspaceRuntime> {
    let key = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut runtimes = RUNTIMES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Arc::clone(runtimes.entry(key).or_insert_with_key(|key| {
        Arc::new(WorkspaceRuntime {
            commit_lock: Mutex::new(()),
            session_stores: Mutex::new(Vec::new()),
            lock_file: lock_path_for(key),
        })
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn the_same_directory_hands_out_the_same_runtime() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonical");
        assert!(
            Arc::ptr_eq(&runtime_for(&root), &runtime_for(&root)),
            "同一个目录必须是同一个资源所有者，否则就是两个互不知晓的写者"
        );
    }

    #[test]
    fn different_directories_do_not_share_a_lock() {
        let a = tempdir().expect("a");
        let b = tempdir().expect("b");
        let (a, b) = (
            a.path().canonicalize().expect("canonical a"),
            b.path().canonicalize().expect("canonical b"),
        );
        assert!(
            !Arc::ptr_eq(&runtime_for(&a), &runtime_for(&b)),
            "两个目录共用一把锁，hub 里一个项目打补丁会把另一个项目堵住"
        );
    }

    /// 大小写不敏感的文件系统上，`.../foo` 和 `.../FOO` 打开的是同一个目录。
    ///
    /// 这条钉的是"按规范化路径发锁够不够"：实测 macOS 和 Windows 的
    /// `canonicalize` 会把拼法纠正成磁盘上的真名，所以够。哪天哪个平台原样
    /// 返回大小写了，这条会红——那时就得换成文件系统给的目录身份（Unix 的
    /// `dev`+`ino`），并接受它在目录删了重建时会分裂的代价。
    ///
    /// 大小写敏感的文件系统（多数 Linux）上没有别名可言，自动跳过。
    #[test]
    fn a_different_spelling_of_the_same_directory_is_not_a_second_writer() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonical");
        let lower = root.join("casecheck");
        std::fs::create_dir(&lower).expect("建目录");
        let upper = root.join("CASECHECK");
        if !upper.is_dir() {
            return;
        }

        assert_eq!(
            upper.canonicalize().expect("canonical upper"),
            lower,
            "canonicalize 没把大小写纠正回来，路径当身份就不够用了"
        );
        assert!(
            Arc::ptr_eq(&runtime_for(&lower), &runtime_for(&upper)),
            "同一个目录的两种拼法拿到了两把锁——这正是要避免的隐藏第二写者"
        );
    }

    /// hub 的场景：同一个目录经不同入口进来是不同的 `ToolContext`（配置指纹
    /// 一变 `context_for` 就重建一个），但写操作必须排同一个队。
    #[test]
    fn two_contexts_on_one_directory_share_the_write_lock() {
        use crate::tools::context::ToolContext;

        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let first =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("first");
        let second =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("second");

        assert!(
            Arc::ptr_eq(&first.runtime, &second.runtime),
            "同一个目录的两个上下文各拿一把锁，两个入口就能同时往里写"
        );
    }

    /// 跨进程那一层：拿着写权的时候，别人打开同一个锁文件就该被挡在外面。
    ///
    /// 这里用第二个 `File` 句柄冒充另一个进程——`flock` 认的是打开的文件本身
    /// （open file description），不是进程，所以同一个进程里的第二个句柄一样
    /// 会被挡住，测得出真实行为。
    #[test]
    fn a_second_process_cannot_write_while_the_lock_is_held() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonical");
        let runtime = runtime_for(&root);
        let path = runtime
            .lock_file_path()
            .expect("数据目录在测试里是隔离的临时目录，锁文件应该建得出来")
            .to_path_buf();

        let held = runtime.lock_commits();
        let other = File::options()
            .read(true)
            .write(true)
            .open(&path)
            .expect("打开锁文件");
        assert!(
            FileExt::try_lock_exclusive(&other).is_err(),
            "写权被占着，另一个进程却拿到了锁——两个 gld 会同时往一个目录里写"
        );

        drop(held);
        assert!(
            FileExt::try_lock_exclusive(&other).is_ok(),
            "写权放开了，别的进程还是拿不到锁"
        );
        let _ = FileExt::unlock(&other);
    }

    /// 每个上下文都得把自己的会话表登记上来，不然切 plan 模式时停不掉它起
    /// 的命令。上下文没了，登记跟着失效——弱引用不拖着它。
    #[test]
    fn every_context_on_a_directory_registers_its_session_store() {
        use crate::tools::context::ToolContext;

        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let runtime = runtime_for(&workspace.path().canonicalize().expect("canonical"));
        assert_eq!(runtime.registered_stores(), 0, "还没建上下文就有登记");

        let first =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("first");
        assert_eq!(runtime.registered_stores(), 1, "上下文建好了却没登记");
        let second =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("second");
        assert_eq!(runtime.registered_stores(), 2, "第二个入口的会话表没登记");

        drop(first);
        drop(second);
        assert_eq!(runtime.registered_stores(), 0, "上下文没了，登记还挂着");
    }

    /// 写权被占着的时候，后来的人等到上限就得到 `None`，不是一直挂着。
    ///
    /// 这条是 `exec_command` 占写权之后才要紧的：命令能跑十分钟，补丁挂在那儿
    /// 等的话，客户端早超时断开了，这边还占着线程。
    #[test]
    fn waiting_for_a_busy_write_lock_gives_up_instead_of_hanging() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonical");
        let runtime = runtime_for(&root);

        let held = runtime.lock_commits_within(Duration::from_millis(50));
        assert!(held.is_some(), "没人占着却拿不到写权");

        let started = Instant::now();
        assert!(
            runtime
                .lock_commits_within(Duration::from_millis(50))
                .is_none(),
            "写权明明被占着，第二个人却拿到了"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "等了 {:?} 才放弃，说明根本没按上限来",
            started.elapsed()
        );

        drop(held);
        assert!(
            runtime
                .lock_commits_within(Duration::from_millis(50))
                .is_some(),
            "放开之后还是拿不到"
        );
    }

    /// 两个不同的目录各有各的锁文件，不会互相挡。
    #[test]
    fn two_directories_do_not_block_each_other_across_processes() {
        let a = tempdir().expect("a");
        let b = tempdir().expect("b");
        let a = runtime_for(&a.path().canonicalize().expect("canonical a"));
        let b = runtime_for(&b.path().canonicalize().expect("canonical b"));
        assert_ne!(
            a.lock_file_path(),
            b.lock_file_path(),
            "两个目录共用一个锁文件，一个项目落盘会把另一个项目堵在外面"
        );

        let _held = a.lock_commits();
        // b 的写权照样拿得到——拿不到这里会一直等，测试超时就是答案。
        let _also = b.lock_commits();
    }
}

//! 同目录的进程内写锁和显式目录下的跨进程锁。

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::MutexGuard;
use std::time::{Duration, Instant};

use super::WorkspaceRuntime;
use fs2::FileExt;

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

impl CommitGuard<'_> {
    /// 删除整个托管目录不能沿用普通补丁的跨进程锁降级路径。
    pub fn is_cross_process(&self) -> bool {
        self.across_processes.is_some()
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

    /// 同上，但由调用方提供等待上限；托管目录删除和测试使用更短的预算。
    pub fn lock_commits_within(&self, wait: Duration) -> Option<CommitGuard<'_>> {
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
pub(super) fn lock_path_for(root: &Path, directory: Option<&Path>) -> Option<PathBuf> {
    let dir = directory?;
    if let Err(error) = std::fs::create_dir_all(dir) {
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
pub fn write_lock_busy() -> crate::workspace::WorkspaceError {
    crate::workspace::WorkspaceError::Tool {
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

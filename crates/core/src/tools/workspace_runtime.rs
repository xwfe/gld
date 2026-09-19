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
//! # 管得到什么，管不到什么
//!
//! 管得到：同一个进程里对同一个目录的写操作，不管是从 hub、单工作区
//! listener 还是 CLI 进来的，也不管中间 `ToolContext` 重建过几次。
//!
//! 管不到（**别当成已经隔离了**）：
//!
//! - **别的进程**——另一个 gld 实例、编辑器、`git checkout`。进程内的 `Arc`
//!   和 `Mutex` 对它们一点约束力都没有。那一侧靠的是补丁的版本前置条件和
//!   落盘前复核，两者都不是强 CAS。
//! - **`exec` 跑的命令**。模型完全可以 `sed -i` 一把梭，这里拦不住。要把
//!   命令也纳进来，得先分清只读命令和写命令，否则一个 `npm run dev` 就能
//!   把写锁占到天亮。
//! - **Git 元数据**。同一个仓库的两个 worktree 是两个目录、两个
//!   `WorkspaceRuntime`，但它们共享同一份 `.git`。改文件互不影响，`git
//!   commit` 这类动 common directory 的操作会撞车。ccnm 那边的做法是把资源
//!   键取到 Git common directory，代价是相关 worktree 一起串行；这里先不
//!   跟，因为 gld 目前没有并行 worktree 的编排。要加的时候是"工作树文件"和
//!   "共享 Git 元数据"两级协调，不是把键一换了事。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

/// 一个工作区目录的执行资源。
///
/// 现在只有写锁一件东西。会话表（`SessionStore`）还留在 `ToolContext` 里，
/// 那是下一步：会话共享要连带解决"谁能读谁的输出"，跟授权主体绑在一起，
/// 不能顺手塞进来。
#[derive(Debug)]
pub struct WorkspaceRuntime {
    commit_lock: Mutex<()>,
}

impl WorkspaceRuntime {
    /// 占住这个目录的写权，直到返回的 guard 被丢掉。
    ///
    /// 谁该调用：真要落盘的写操作。只读的预检（`dry_run`）不要占——预检不该
    /// 让真正的写操作排队。
    pub fn lock_commits(&self) -> MutexGuard<'_, ()> {
        // 上一个持锁的线程 panic 过：锁里存的是 `()`，没有被弄坏的状态可言，
        // 接着用就是了——在这里 panic 掉反而会把一次正常的补丁变成失败。
        self.commit_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
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
    Arc::clone(runtimes.entry(key).or_insert_with(|| {
        Arc::new(WorkspaceRuntime {
            commit_lock: Mutex::new(()),
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
}

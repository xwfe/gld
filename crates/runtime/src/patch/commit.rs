//! Staging, baseline rechecks, atomic replacement, and rollback.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use uuid::Uuid;

use crate::workspace::{FileState, Workspace, WorkspaceError};

use super::validation::{patch_failed, version_conflict};

/// 只在测试里用的故障注入：让备份、替换、回滚里的某一步真的失败。
///
/// 存在的理由是"失败之后说的话是不是真的"没法靠真实 I/O 稳定复现——
/// 权限、磁盘满、并发都做不成可重复的单测，而这恰恰是最需要有测试的地方
/// （审查 A10）。线程局部，不影响并行跑的别的测试。
#[cfg(test)]
pub(crate) mod faults {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) enum Fault {
        /// 读原文做备份时失败。
        BackupRead(PathBuf),
        /// 把暂存文件换上去时失败。
        Replace(PathBuf),
        /// 回滚写回原内容时失败。
        Restore(PathBuf),
    }

    thread_local! {
        static INJECTED: RefCell<Vec<Fault>> = const { RefCell::new(Vec::new()) };
    }

    /// 装一组故障，返回的守卫在测试结束时清掉它们。
    pub(crate) struct Injected;

    pub(crate) fn inject(faults: Vec<Fault>) -> Injected {
        INJECTED.with(|slot| *slot.borrow_mut() = faults);
        Injected
    }

    impl Drop for Injected {
        fn drop(&mut self) {
            INJECTED.with(|slot| slot.borrow_mut().clear());
        }
    }

    pub(crate) fn hits(fault: &Fault) -> bool {
        INJECTED.with(|slot| slot.borrow().contains(fault))
    }

    pub(crate) fn backup_read_fails(path: &Path) -> bool {
        hits(&Fault::BackupRead(path.to_path_buf()))
    }

    pub(crate) fn replace_fails(path: &Path) -> bool {
        hits(&Fault::Replace(path.to_path_buf()))
    }

    pub(crate) fn restore_fails(path: &Path) -> bool {
        hits(&Fault::Restore(path.to_path_buf()))
    }

    thread_local! {
        static BEFORE_COMMIT: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
    }

    /// 在"算完补丁"和"开始落盘"之间插一手。
    ///
    /// 专门用来复现那个最难测也最要紧的窗口：补丁算完之后、写下去之前，
    /// 别的进程（编辑器、另一个 gld、git checkout）改了同一个文件。靠真实
    /// 并发碰运气是测不稳的。
    pub(crate) struct CommitHook;

    pub(crate) fn before_commit_do(action: impl Fn() + 'static) -> CommitHook {
        BEFORE_COMMIT.with(|slot| *slot.borrow_mut() = Some(Box::new(action)));
        CommitHook
    }

    impl Drop for CommitHook {
        fn drop(&mut self) {
            BEFORE_COMMIT.with(|slot| *slot.borrow_mut() = None);
        }
    }

    pub(crate) fn run_before_commit() {
        let action = BEFORE_COMMIT.with(|slot| slot.borrow_mut().take());
        if let Some(action) = action {
            action();
            BEFORE_COMMIT.with(|slot| *slot.borrow_mut() = Some(action));
        }
    }
}

#[cfg(not(test))]
mod faults {
    use std::path::Path;
    pub(crate) fn backup_read_fails(_path: &Path) -> bool {
        false
    }
    pub(crate) fn replace_fails(_path: &Path) -> bool {
        false
    }
    pub(crate) fn restore_fails(_path: &Path) -> bool {
        false
    }
    pub(crate) fn run_before_commit() {}
}

/// 算这批补丁的时候，一个文件在磁盘上是什么样。落盘前拿它再核一遍。
#[derive(Debug, Clone)]
pub(super) enum Baseline {
    /// 内容就是这些字节（改文件时原文是现成的，比版本号结实）。
    Content(Vec<u8>),
    /// 只记了版本（删除：没必要为了校验把整个文件读进来）。
    Version(String),
    /// 那时这个路径上什么都没有（新建）。
    Absent,
}

impl Baseline {
    pub(super) fn from_state(state: &FileState) -> Self {
        match state {
            FileState::Present(version) => Self::Version(version.clone()),
            FileState::Absent => Self::Absent,
            // 进不到这儿：状态问不出来的文件在前面就被拒了。
            FileState::Unknown(_) => Self::Absent,
        }
    }

    /// 磁盘上现在这一份，还是算补丁时的那一份吗。
    ///
    /// `on_disk` 是刚读出来的内容；路径上没有文件时是 `None`。
    fn check(
        &self,
        display: &str,
        path: &std::path::Path,
        on_disk: Option<&[u8]>,
    ) -> Result<(), WorkspaceError> {
        match self {
            Self::Content(expected) => match on_disk {
                Some(actual) if actual == expected.as_slice() => Ok(()),
                Some(_) => Err(version_conflict(format!(
                    "{display} changed while this patch was being prepared; nothing was written. Read it again and rebuild the patch"
                ))),
                None => Err(version_conflict(format!(
                    "{display} was removed while this patch was being prepared; nothing was written"
                ))),
            },
            Self::Version(expected) => {
                match crate::workspace::file_state(path) {
                    // 删掉的东西已经不在了：结果一样，不算冲突。
                    FileState::Absent => Ok(()),
                    FileState::Present(actual) if &actual == expected => Ok(()),
                    FileState::Present(actual) => Err(version_conflict(format!(
                        "{display} changed while this patch was being prepared (version {expected} is now {actual}); nothing was written"
                    ))),
                    // 问不出来就不写：说不清的时候动手，正是这道门要拦的。
                    FileState::Unknown(reason) => Err(version_conflict(format!(
                        "cannot tell the state of {display} ({reason}) before writing it; nothing was written"
                    ))),
                }
            }
            Self::Absent => match on_disk {
                None if !path.exists() => Ok(()),
                // 新建的目标在这中间冒出来了：**不覆盖**。模型以为自己在建
                // 一个新文件，实际会盖掉别人刚放进来的东西（审查 A09）。
                _ => Err(version_conflict(format!(
                    "{display} appeared while this patch was being prepared; it would have been overwritten, so nothing was written. Read it and decide whether to update it instead"
                ))),
            },
        }
    }
}

pub(super) fn commit_staged(
    ws: &Workspace,
    staged: &HashMap<String, Option<String>>,
    baselines: &HashMap<String, Baseline>,
) -> Result<HashMap<PathBuf, Option<Vec<u8>>>, WorkspaceError> {
    let staged_bytes = staged
        .iter()
        .map(|(path, content)| {
            (
                path.clone(),
                content.as_ref().map(|value| value.as_bytes().to_vec()),
            )
        })
        .collect::<HashMap<_, _>>();
    commit_staged_bytes(ws, &staged_bytes, baselines)
}

fn commit_staged_bytes(
    ws: &Workspace,
    staged: &HashMap<String, Option<Vec<u8>>>,
    baselines: &HashMap<String, Baseline>,
) -> Result<HashMap<PathBuf, Option<Vec<u8>>>, WorkspaceError> {
    faults::run_before_commit();
    let mut backups: HashMap<PathBuf, Option<Vec<u8>>> = HashMap::new();
    let mut temporary_files = HashMap::new();
    // **按路径排序后再落盘**，不按 HashMap 的随机顺序。一批补丁中途失败时，
    // 哪些文件已经换上去、哪些还没有，得是可复现的——否则同一个补丁失败两次
    // 留下的现场可能不一样，人没法照着查（审查 P05 的"错误后的真实状态"）。
    let mut entries: Vec<(&String, &Option<Vec<u8>>)> = staged.iter().collect();
    entries.sort_by(|left, right| left.0.cmp(right.0));
    for (rel, content) in &entries {
        ws.reject_protected_write_path(rel)?;
        let resolved = if content.is_none() {
            ws.resolve_existing(rel)?
        } else {
            ws.resolve_for_write(rel)?
        };
        let path = resolved.path.clone();
        // 读不出原内容就**不要动这个文件**。原来是 `unwrap_or_default()`：
        // 读失败被当成"原来是空的"，一旦后面要回滚，写回去的就是空文件——
        // 本来只是打补丁失败，结果把人家的文件清空了（审查 P05）。
        let backup = if path.exists() && path.is_file() {
            if faults::backup_read_fails(&path) {
                cleanup_temporary_files(temporary_files.values());
                return Err(patch_failed(
                    "backup_unreadable",
                    format!(
                        "cannot read {} to back it up, so nothing was changed",
                        resolved.display
                    ),
                ));
            }
            match fs::read(&path) {
                Ok(bytes) => Some(bytes),
                Err(err) => {
                    cleanup_temporary_files(temporary_files.values());
                    return Err(patch_failed(
                        "backup_unreadable",
                        format!(
                            "cannot read {} to back it up ({err}), so nothing was changed",
                            resolved.display
                        ),
                    ));
                }
            }
        } else {
            None
        };
        // 落盘前最后一次核对：这个文件还是算补丁时的那一份吗。
        //
        // 备份刚刚把它读上来，比一比几乎不要钱，而它挡住的是最坏的一种失败——
        // 补丁算完之后有人动了文件，照写下去就是把那次改动无声盖掉。这里发现
        // 冲突时**还没有任何文件被换上去**（换上去是下面第二轮的事），所以
        // 一个字节都没改，也没有要回滚的东西（审查 C3、A09）。
        if let Some(baseline) = baselines.get(*rel) {
            if let Err(error) = baseline.check(&resolved.display, &path, backup.as_deref()) {
                cleanup_temporary_files(temporary_files.values());
                return Err(error);
            }
        }
        backups.insert(path.clone(), backup);
        if let Some(bytes) = content {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|err| patch_failed("staging_failed", err.to_string()))?;
            }
            // 原文件的权限，好让打完补丁的脚本还是可执行的。文件不存在（新增）
            // 时是 None，临时文件就用新建文件的默认权限。
            let mode = fs::metadata(&path).ok().map(|meta| meta.permissions());
            let temp = path.with_file_name(format!(
                ".{}.harness-stage-{}",
                path.file_name().and_then(|v| v.to_str()).unwrap_or("file"),
                Uuid::new_v4().simple()
            ));
            // 共用 ccnm 的落盘写（toexec-fs）：内容 fsync 之后才返回。少了这一步，
            // 后面那次 rename 可能先持久化、内容还没有，断电后留下的是一个名字对、
            // 长度错的文件——看起来是成功的。
            if let Err(err) = toexec_fs::write_durable(&temp, bytes, mode.as_ref()) {
                // 还没有任何文件被换上去，所以没有要回滚的东西。
                cleanup_temporary_files(temporary_files.values());
                return Err(patch_failed(
                    "staging_failed",
                    format!("Failed to stage file: {err}. Nothing was changed"),
                ));
            }
            temporary_files.insert(path.clone(), temp);
        }
    }

    // 真正换上去的那些文件。回滚只回滚它们：还没动过的文件不需要"恢复"，
    // 把它们一起写一遍只会制造假的失败，也盖掉别人同时做的改动。
    let mut replaced: Vec<PathBuf> = Vec::new();
    for (rel, content) in &entries {
        let resolved = if content.is_none() {
            ws.resolve_existing(rel)?
        } else {
            ws.resolve_for_write(rel)?
        };
        let path = resolved.path;
        let result = if content.is_some() {
            let temp = temporary_files
                .get(&path)
                .cloned()
                .ok_or_else(|| patch_failed("staging_failed", "Staged file is missing"));
            match temp {
                // 注入失败的办法是把暂存文件换成一个不存在的路径，让
                // `toexec_fs::replace` **真的**失败一次。以前是在调用之前直接
                // 返回 Err，那样共享库根本没被调到——它在 Windows 上曾经先
                // 删目标再 rename、失败就把旧文件弄丢（跨仓评审 X01），这条
                // 路径上的测试却照样是绿的。
                Ok(temp) => {
                    let staged = if faults::replace_fails(&path) {
                        temp.with_file_name("this-staged-file-does-not-exist")
                    } else {
                        temp
                    };
                    toexec_fs::replace(&staged, &path)
                }
                Err(error) => Err(std::io::Error::other(error.to_string())),
            }
        } else if faults::replace_fails(&path) {
            Err(std::io::Error::other("injected replace failure"))
        } else if path.exists() && path.is_file() {
            fs::remove_file(&path)
        } else {
            Ok(())
        };
        match result {
            Ok(()) => replaced.push(path),
            Err(err) => {
                cleanup_temporary_files(temporary_files.values());
                let failed = restore_backups(&backups, &replaced);
                if failed.is_empty() {
                    return Err(patch_failed(
                        "write_failed_rolled_back",
                        format!(
                            "Failed to write file: {err}. Everything this patch had changed was rolled back"
                        ),
                    ));
                }
                // 回滚也失败了：工作区现在是半新半旧，**不能**说已经回滚。
                // 人得知道去看哪几个文件。
                let names = failed
                    .iter()
                    .map(|path| crate::workspace::relative_display(ws.root(), path))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(rollback_incomplete(format!(
                    "Failed to write file: {err}, and rolling back did not finish: {names} may now hold this patch's content instead of the original. Check those files before retrying"
                )));
            }
        }
    }
    cleanup_temporary_files(temporary_files.values());
    Ok(backups)
}

/// 把已经换上去的文件恢复成原样，返回**没能恢复的**那些。
///
/// 原来所有错误都被 `let _ =` 吞掉，于是"回滚失败"和"回滚成功"给调用方的
/// 消息一模一样——而这两种情况下工作区完全不同（审查 P05）。
fn restore_backups(
    backups: &HashMap<PathBuf, Option<Vec<u8>>>,
    replaced: &[PathBuf],
) -> Vec<PathBuf> {
    let mut failed = Vec::new();
    for path in replaced {
        let Some(data) = backups.get(path) else {
            continue;
        };
        let result = match data {
            // 原来没有这个文件：回滚就是把新建的删掉。
            None => fs::remove_file(path).or_else(|err| {
                if err.kind() == std::io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(err)
                }
            }),
            Some(bytes) => {
                if faults::restore_fails(path) {
                    Err(std::io::Error::other("injected restore failure"))
                } else {
                    if let Some(parent) = path.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    fs::write(path, bytes)
                }
            }
        };
        if result.is_err() {
            failed.push(path.clone());
        }
    }
    failed
}

fn cleanup_temporary_files<'a>(paths: impl Iterator<Item = &'a PathBuf>) {
    for path in paths {
        let _ = fs::remove_file(path);
    }
}

/// 打补丁失败**而且**回滚没做干净：工作区现在半新半旧。和 `PATCH_FAILED`
/// 分开，因为下一步完全不同——那个是"改一改再来"，这个是"先去看文件"。
fn rollback_incomplete(message: impl Into<String>) -> WorkspaceError {
    WorkspaceError::Tool {
        code: "PATCH_ROLLBACK_INCOMPLETE",
        message: message.into(),
        category: "runtime",
        retryable: false,
    }
}

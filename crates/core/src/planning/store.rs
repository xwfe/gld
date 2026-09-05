use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::error::AppResult;

use super::{PlanningState, PLANNING_RELATIVE_PATH};

/// 按文件路径分的写锁。
///
/// Planning 的每次改动都是"读整个 JSON → 改 → 写回整个 JSON"。MCP 那侧没有
/// 任何串行化（Actions 那侧有 write_lock，两边一直不对称），而 ChatGPT / Claude
/// 会并行发工具调用，于是同一个文件被多个线程同时读改写。
///
/// 实测 12 个并发 create_goal：只剩 1 个落盘，其余报 `goal not found`
/// （自己刚写的状态被别人整体覆盖）或 `EOF while parsing a string`
/// （读到了另一个线程正写到一半的文件）。
///
/// 锁按路径分，不同工作区之间互不影响。
fn lock_for(path: &Path) -> MutexGuard<'static, ()> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, &'static Mutex<()>>>> = OnceLock::new();
    let registry = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mutex = {
        let mut map = registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *map.entry(path.to_path_buf())
            // 锁要和进程同寿：guard 会跨 map 的生命周期存活，所以泄漏一个
            // Mutex（每个工作区一个，数量有限）比引用计数简单也更难出错。
            .or_insert_with(|| Box::leak(Box::new(Mutex::new(()))))
    };
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Debug, Clone)]
pub struct PlanningStore {
    path: PathBuf,
}

impl PlanningStore {
    pub fn new(workspace_root: &Path) -> Self {
        Self {
            path: workspace_root.join(PLANNING_RELATIVE_PATH),
        }
    }

    pub fn load(&self) -> AppResult<PlanningState> {
        let _guard = lock_for(&self.path);
        self.load_unlocked()
    }

    /// 读 → 改 → 写，整段持锁。
    ///
    /// 只让 `save` 加锁是不够的：两个调用各自读到同一份旧状态，
    /// 后写的那个会把先写的改动整个盖掉，而两边都报成功。
    pub fn update<R>(
        &self,
        mutate: impl FnOnce(&mut PlanningState) -> AppResult<R>,
    ) -> AppResult<R> {
        let _guard = lock_for(&self.path);
        let mut state = self.load_unlocked()?;
        state.revision = state.revision.saturating_add(1);
        let result = mutate(&mut state)?;
        self.save_unlocked(&state)?;
        Ok(result)
    }

    /// 在 `.gld/` 里放一个只忽略它自己的 `.gitignore`。
    ///
    /// 这个目录是 gld 在项目里的落脚点，默认不该进版本库——不然每次 AI 动一下
    /// Planning 状态，用户的 `git status` 里就多一条改动。
    ///
    /// 为什么不去改项目根的 `.gitignore`：那是用户的文件（gld 自己把它列为
    /// "关键文件"，连 AI 用 patch 改都要 confirm），工具不该偷偷往里加行；
    /// 而且没有 `.gitignore` 的项目还得凭空建一个。放在自己目录里，效果一样，
    /// 谁都不碰。
    ///
    /// 想让团队共享 Goal / Plan 的话，把这个文件删掉即可：它只在 `.gld/`
    /// 被创建的那一刻写一次，之后不会再补回来。
    fn ignore_self_in_git(gld_dir: &std::path::Path) {
        let marker = gld_dir.join(".gitignore");
        if marker.exists() {
            return;
        }
        // 失败不影响正事：状态照样存，只是没被忽略。
        let _ = fs::write(&marker, "# gld 在这个项目里的本地状态，默认不进版本库\n*\n");
    }

    fn load_unlocked(&self) -> AppResult<PlanningState> {
        if !self.path.exists() {
            return Ok(PlanningState::default());
        }
        let raw = fs::read_to_string(&self.path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    fn save_unlocked(&self, state: &PlanningState) -> AppResult<()> {
        if let Some(parent) = self.path.parent() {
            // 只在第一次建出 `.gld/` 时放忽略文件。目录已经在了就不碰——
            // 用户可能是有意删掉它来让 Goal / Plan 进版本库的，每次都补回去
            // 等于不让人改主意。
            let gld_dir = parent.parent().unwrap_or(parent);
            let first_time = !gld_dir.exists();
            fs::create_dir_all(parent)?;
            if first_time {
                Self::ignore_self_in_git(gld_dir);
            }
        }
        let raw = serde_json::to_string_pretty(state)?;
        // 先写临时文件再改名。直接 fs::write 会先把原文件截断成 0 字节，
        // 此时进程被杀（或另一个进程正在读）就只剩半个 JSON——
        // 这个文件在用户的项目目录里，坏了等于 Goal / Plan 全丢。
        let tmp = self
            .path
            .with_extension(format!("json.tmp.{}", std::process::id()));
        fs::write(&tmp, format!("{raw}\n"))?;
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 并发读改写不能丢更新，也不能读到写了一半的文件。
    #[test]
    fn concurrent_updates_do_not_lose_writes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PlanningStore::new(dir.path());
        const WRITERS: u64 = 16;

        std::thread::scope(|scope| {
            for _ in 0..WRITERS {
                let store = store.clone();
                scope.spawn(move || {
                    store
                        .update(|state| {
                            // revision 由 update 自己递增，这里只要确认每次
                            // 改动都基于上一次的结果，没有被整体覆盖。
                            Ok(state.revision)
                        })
                        .expect("并发 update 不该失败");
                });
            }
        });

        assert_eq!(
            store.load().expect("load").revision,
            WRITERS,
            "每次 update 都该在上一次的基础上 +1；数值偏小说明有更新被覆盖了"
        );
    }

    /// `.gld/` 默认不进版本库：项目里不该因为 AI 动了下 Planning 就多出改动。
    #[test]
    fn the_state_directory_ignores_itself_in_git() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PlanningStore::new(dir.path());

        store.update(|_| Ok(())).expect("写状态");

        let marker = dir.path().join(".gld/.gitignore");
        let content = fs::read_to_string(&marker).expect("应当写了忽略文件");
        assert!(content.contains('*'), "忽略规则不对：{content}");
    }

    /// 删掉它就是"我要把 Goal / Plan 提交上去"，不能每次写状态又给补回来。
    #[test]
    fn removing_the_ignore_file_is_respected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PlanningStore::new(dir.path());
        store.update(|_| Ok(())).expect("第一次写");
        let marker = dir.path().join(".gld/.gitignore");
        fs::remove_file(&marker).expect("删掉忽略文件");

        store.update(|_| Ok(())).expect("再写一次");

        assert!(!marker.exists(), "被删掉的忽略文件又被补回来了");
    }

    /// 落盘要么是完整的旧内容，要么是完整的新内容，不存在中间态。
    #[test]
    fn a_reader_never_sees_a_half_written_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PlanningStore::new(dir.path());
        store.update(|_| Ok(())).expect("seed");
        let path = dir.path().join(PLANNING_RELATIVE_PATH);

        std::thread::scope(|scope| {
            scope.spawn(|| {
                for _ in 0..200 {
                    store.update(|_| Ok(())).expect("writer");
                }
            });
            scope.spawn(|| {
                for _ in 0..200 {
                    // 绕开锁直接读文件，模拟另一个进程在读。
                    if let Ok(raw) = fs::read_to_string(&path) {
                        serde_json::from_str::<PlanningState>(&raw).expect("读到的必须是完整 JSON");
                    }
                }
            });
        });
    }
}

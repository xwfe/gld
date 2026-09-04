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

    fn load_unlocked(&self) -> AppResult<PlanningState> {
        if !self.path.exists() {
            return Ok(PlanningState::default());
        }
        let raw = fs::read_to_string(&self.path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    fn save_unlocked(&self, state: &PlanningState) -> AppResult<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
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

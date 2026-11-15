//! 宿主显式持有的监听状态；不加载应用数据，也不选择工具执行位置。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use super::RuntimeSupervisor;
use crate::error::AppResult;
use crate::generation::ServiceGeneration;

/// 同一个宿主的监听表、工作区重启锁和启动恢复标记必须作为一份资源传递。
#[derive(Default)]
pub struct ListenerManager {
    runtime: Mutex<RuntimeSupervisor>,
    restart_gate: tokio::sync::Mutex<()>,
    startup_restore_attempted: AtomicBool,
}

impl ListenerManager {
    pub fn with_generation(generation: Arc<dyn ServiceGeneration>) -> Self {
        Self {
            runtime: Mutex::new(RuntimeSupervisor::with_generation(generation)),
            ..Self::default()
        }
    }

    pub(crate) fn generation(&self) -> Option<Arc<dyn ServiceGeneration>> {
        self.runtime().generation.clone()
    }

    fn runtime(&self) -> MutexGuard<'_, RuntimeSupervisor> {
        self.runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn with_runtime<R>(
        &self,
        f: impl FnOnce(&mut RuntimeSupervisor) -> AppResult<R>,
    ) -> AppResult<R> {
        f(&mut self.runtime())
    }

    pub(crate) async fn lock_restart(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.restart_gate.lock().await
    }

    /// 先记下已尝试再读配置，失败也不在同一次宿主启动里重复恢复。
    pub(crate) fn begin_startup_restore(&self) -> bool {
        !self.startup_restore_attempted.swap(true, Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_restore_is_claimed_once_per_manager() {
        let manager = Arc::new(ListenerManager::default());
        let claimed = std::thread::scope(|scope| {
            let attempts: Vec<_> = (0..12)
                .map(|_| {
                    let manager = manager.clone();
                    scope.spawn(move || usize::from(manager.begin_startup_restore()))
                })
                .collect();
            attempts
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .sum::<usize>()
        });
        assert_eq!(claimed, 1);
        assert!(!manager.begin_startup_restore());
        assert!(ListenerManager::default().begin_startup_restore());
    }

    #[tokio::test]
    async fn restart_gate_does_not_hold_the_listener_table_lock() {
        let manager = ListenerManager::default();
        let gate = manager.lock_restart().await;
        assert!(manager.restart_gate.try_lock().is_err());
        assert_eq!(manager.with_runtime(|_| Ok(7)).unwrap(), 7);
        drop(gate);
        assert!(manager.restart_gate.try_lock().is_ok());
    }
}

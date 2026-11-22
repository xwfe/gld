//! 宿主显式持有的监听状态；不加载应用数据，也不选择工具执行位置。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::{ListenerHost, WorkspaceListeners};
use crate::error::AppResult;

/// 同一个宿主的监听表、工作区重启锁和启动恢复标记必须作为一份资源传递。
pub struct ListenerManager {
    runtime: Mutex<Box<dyn WorkspaceListeners>>,
    host: Arc<dyn ListenerHost>,
    restart_gate: tokio::sync::Mutex<()>,
    startup_restore_attempted: AtomicBool,
}

impl ListenerManager {
    pub fn new(host: Arc<dyn ListenerHost>, runtime: Box<dyn WorkspaceListeners>) -> Self {
        Self {
            runtime: Mutex::new(runtime),
            host,
            restart_gate: tokio::sync::Mutex::new(()),
            startup_restore_attempted: AtomicBool::new(false),
        }
    }

    pub(crate) fn host(&self) -> &dyn ListenerHost {
        &*self.host
    }

    pub(crate) fn with_runtime<R>(
        &self,
        f: impl FnOnce(&mut dyn WorkspaceListeners) -> AppResult<R>,
    ) -> AppResult<R> {
        let mut guard = self
            .runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(guard.as_mut())
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
pub(crate) use tests::test_manager as test_listener_manager;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::async_rt::JoinHandle;
    use crate::hub::runtime::{HubState, TunnelOutcome, TunnelSnapshot};
    use crate::hub::HubSecrets;
    use crate::runtime::ServiceKind;
    use crate::settings::HubConfig;
    use crate::tunnel::TunnelServiceKind;
    use crate::usage::ServiceUsageStats;
    use crate::workspace::{RuntimeStatusDto, WorkspaceProfile};
    use futures_util::future::BoxFuture;
    use std::collections::HashSet;

    struct UnusedListeners;

    impl ListenerHost for UnusedListeners {
        fn ensure_gateway(&self) -> futures_util::future::BoxFuture<'_, AppResult<()>> {
            unreachable!("test does not start a gateway")
        }

        fn start_hub<'a>(
            &'a self,
            _: &'a HubConfig,
            _: TunnelOutcome,
            _: HubSecrets,
        ) -> BoxFuture<'a, AppResult<()>> {
            unreachable!("test does not start services")
        }
        fn stop_hub(&self) -> BoxFuture<'_, ()> {
            unreachable!()
        }
        fn attach_hub_tunnel(&self, _: TunnelOutcome) -> BoxFuture<'_, ()> {
            unreachable!()
        }
        fn hub_state(&self) -> BoxFuture<'_, HubState> {
            unreachable!()
        }
        fn hub_public_base(&self) -> BoxFuture<'_, Option<(String, String)>> {
            unreachable!()
        }
        fn hub_tunnel_snapshot(&self) -> BoxFuture<'_, Option<TunnelSnapshot>> {
            unreachable!()
        }
    }

    impl WorkspaceListeners for UnusedListeners {
        fn status(&self, _: &WorkspaceProfile, _: ServiceKind) -> RuntimeStatusDto {
            unreachable!()
        }
        fn start(&mut self, _: &WorkspaceProfile, _: ServiceKind) -> AppResult<RuntimeStatusDto> {
            unreachable!()
        }
        fn refresh(&mut self, _: &WorkspaceProfile, _: ServiceKind) {
            unreachable!()
        }
        fn running_workspace_ids(&self, _: ServiceKind) -> Vec<String> {
            unreachable!()
        }
        fn usage_stats(&self, _: &str, _: ServiceKind) -> ServiceUsageStats {
            unreachable!()
        }
        fn is_running(&self, _: &str, _: ServiceKind) -> bool {
            false
        }
        fn is_active(&self, _: &str, _: ServiceKind) -> bool {
            unreachable!()
        }
        fn drop_workspace(&mut self, _: &WorkspaceProfile) {
            unreachable!()
        }
        fn active_tunnel_service_keys(&self) -> HashSet<(String, TunnelServiceKind)> {
            unreachable!()
        }
        fn begin_stop(&mut self, _: &str, _: ServiceKind) -> Option<JoinHandle<()>> {
            unreachable!()
        }
        fn finish_stop(&mut self, _: &str, _: ServiceKind) {
            unreachable!()
        }
    }

    pub(crate) fn test_manager() -> ListenerManager {
        ListenerManager::new(Arc::new(UnusedListeners), Box::new(UnusedListeners))
    }

    #[test]
    fn startup_restore_is_claimed_once_per_manager() {
        let manager = Arc::new(test_manager());
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
        assert!(test_manager().begin_startup_restore());
    }

    #[tokio::test]
    async fn restart_gate_does_not_hold_the_listener_table_lock() {
        let manager = test_manager();
        let gate = manager.lock_restart().await;
        assert!(manager.restart_gate.try_lock().is_err());
        assert_eq!(manager.with_runtime(|_| Ok(7)).unwrap(), 7);
        drop(gate);
        assert!(manager.restart_gate.try_lock().is_ok());
    }
}

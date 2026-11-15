//! 宿主显式持有的监听状态；不加载应用数据，也不选择工具执行位置。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use super::{ListenerHost, RuntimeSupervisor};
use crate::error::AppResult;

/// 同一个宿主的监听表、工作区重启锁和启动恢复标记必须作为一份资源传递。
pub struct ListenerManager {
    runtime: Mutex<RuntimeSupervisor>,
    host: Arc<dyn ListenerHost>,
    restart_gate: tokio::sync::Mutex<()>,
    startup_restore_attempted: AtomicBool,
}

impl ListenerManager {
    pub fn new(host: Arc<dyn ListenerHost>) -> Self {
        Self {
            runtime: Mutex::new(RuntimeSupervisor::new(host.clone())),
            host,
            restart_gate: tokio::sync::Mutex::new(()),
            startup_restore_attempted: AtomicBool::new(false),
        }
    }

    pub(crate) fn host(&self) -> &dyn ListenerHost {
        &*self.host
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
    use crate::hub::runtime::{HubState, TunnelOutcome, TunnelSnapshot};
    use crate::hub::HubSecrets;
    use crate::runtime::{BoundListener, ServiceKind};
    use crate::settings::HubConfig;
    use crate::usage::ServiceUsage;
    use crate::workspace::WorkspaceProfile;
    use futures_util::future::BoxFuture;

    struct UnusedHost;

    impl ListenerHost for UnusedHost {
        fn ensure_gateway(&self) -> futures_util::future::BoxFuture<'_, AppResult<()>> {
            unreachable!("test does not start a gateway")
        }

        fn bind_workspace(
            &self,
            _: &WorkspaceProfile,
            _: ServiceKind,
            _: u16,
            _: Arc<ServiceUsage>,
        ) -> AppResult<Result<BoundListener, String>> {
            unreachable!("test does not start services")
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

    fn test_manager() -> ListenerManager {
        ListenerManager::new(Arc::new(UnusedHost))
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

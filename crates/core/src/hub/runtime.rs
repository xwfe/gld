//! 聚合入口在守护进程里的运行状态：起、停、现在是不是在跑。
//!
//! 和全局入口一样是进程级单例（一个守护进程里只有一个 hub），所以用静态量，
//! 没有挂到 `RuntimeSupervisor` 上——那边的键是（工作区 id, 服务）。
//! 配置校验、凭据、恢复标记这些业务规则在 `App`（app/hub.rs），这里只管进程内的监听器。

use std::sync::{Arc, LazyLock};

use tokio::sync::Mutex;

use super::{Hub, HubSecrets};
use crate::async_rt::JoinHandle;
use crate::error::{AppError, AppResult};
use crate::mcp::ShutdownSender;
use crate::runtime::await_listener_shutdown;
use crate::settings::HubConfig;

struct HubRuntime {
    port: u16,
    shutdown: Option<ShutdownSender>,
    handle: JoinHandle<()>,
    hub: Arc<Hub>,
}

static RUNTIME: LazyLock<Mutex<Option<HubRuntime>>> = LazyLock::new(|| Mutex::new(None));

/// 此刻的监听器状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HubState {
    Stopped,
    Running {
        port: u16,
    },
    /// 开过，但监听任务已经自己退了（运行中出错）。原因在 hub 的 stderr.log 里。
    Exited {
        port: u16,
    },
}

/// 按给定配置起监听器。调用方要先 [`stop`] 并确认端口空出来。
pub async fn start(
    config: &HubConfig,
    public_base_url: String,
    secrets: HubSecrets,
) -> AppResult<()> {
    let mut guard = RUNTIME.lock().await;
    if let Some(previous) = guard.take() {
        stop_runtime(previous).await;
    }
    let hub = Arc::new(Hub::new(config));
    let (shutdown, handle) = crate::mcp::spawn_hub_listener(
        config.local_port,
        hub.clone(),
        &config.auth_type,
        public_base_url,
        secrets,
    )
    .map_err(AppError::Message)?;
    *guard = Some(HubRuntime {
        port: config.local_port,
        shutdown: Some(shutdown),
        handle,
        hub,
    });
    Ok(())
}

pub async fn stop() {
    if let Some(runtime) = RUNTIME.lock().await.take() {
        stop_runtime(runtime).await;
    }
}

pub async fn state() -> HubState {
    match RUNTIME.lock().await.as_ref() {
        None => HubState::Stopped,
        Some(runtime) if runtime.handle.is_finished() => HubState::Exited { port: runtime.port },
        Some(runtime) => HubState::Running { port: runtime.port },
    }
}

async fn stop_runtime(mut runtime: HubRuntime) {
    if let Some(shutdown) = runtime.shutdown.take() {
        let _ = shutdown.send(());
    }
    await_listener_shutdown(Some(runtime.handle), runtime.port).await;
    // 成员里经 hub 起的命令，hub 停了就没人能再读它们的输出或杀掉它们。
    // 结束进程要阻塞等待，不能占着异步 worker。
    let hub = runtime.hub;
    let _ = tokio::task::spawn_blocking(move || hub.shutdown()).await;
}

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
use crate::tunnel::standalone::StandaloneTunnel;

struct HubRuntime {
    port: u16,
    shutdown: Option<ShutdownSender>,
    handle: JoinHandle<()>,
    hub: Arc<Hub>,
    /// 服务自己的隧道进程，跟着监听器一起停。
    tunnel: Option<StandaloneTunnel>,
    /// 这次起来实际用的公网基地址。Cloudflare 临时地址每次都不一样，配置里推不出来。
    public_base: String,
    /// 隧道没起来的原因。服务照样在本地跑（本机客户端不受影响），`gld ls` 把它报出来。
    tunnel_error: String,
}

/// 这次起隧道的结果，由调用方（`App`）起好交进来。
pub struct TunnelOutcome {
    pub public_base: String,
    pub tunnel: Option<StandaloneTunnel>,
    pub error: String,
}

static RUNTIME: LazyLock<Mutex<Option<HubRuntime>>> = LazyLock::new(|| Mutex::new(None));

/// 监听器起过、还没停。给**同步**上下文用：守护进程回 `daemon status` 的那段
/// 代码不是异步的，拿不到上面那把 tokio 锁，而它要是数不到这个服务，
/// `gld daemon status` 就会在服务跑着的时候说"运行中的服务 0"。
///
/// 只反映"起了没停"：监听任务自己退了（[`HubState::Exited`]）这里看不出来，
/// 那种情况 `gld ls` 和 `gld doctor` 会说。
static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn started_hint() -> bool {
    STARTED.load(std::sync::atomic::Ordering::Relaxed)
}

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
    outcome: TunnelOutcome,
    secrets: HubSecrets,
) -> AppResult<()> {
    let mut guard = RUNTIME.lock().await;
    if let Some(previous) = guard.take() {
        stop_runtime(previous).await;
    }
    let hub = Arc::new(Hub::new(config));
    let spawned = crate::mcp::spawn_hub_listener(
        config.local_port,
        hub.clone(),
        &config.auth_type,
        outcome.public_base.clone(),
        secrets,
    );
    let (shutdown, handle) = match spawned {
        Ok(listener) => listener,
        Err(error) => {
            // 监听器起不来，隧道留着就成了一条转到没人听的端口的孤儿进程。
            if let Some(tunnel) = outcome.tunnel {
                tunnel.stop().await;
            }
            return Err(AppError::Message(error));
        }
    };
    STARTED.store(true, std::sync::atomic::Ordering::Relaxed);
    *guard = Some(HubRuntime {
        port: config.local_port,
        shutdown: Some(shutdown),
        handle,
        hub,
        tunnel: outcome.tunnel,
        public_base: outcome.public_base,
        tunnel_error: outcome.error,
    });
    Ok(())
}

pub async fn stop() {
    if let Some(runtime) = RUNTIME.lock().await.take() {
        stop_runtime(runtime).await;
    }
}

/// 跑着的服务此刻对客户端 tools/list 给出的工具表；没在跑是 `None`。
///
/// 和监听器用的是同一个 `Hub`，所以就是客户端拿到的那一份——不是按当前 CLI 的
/// 代码重新算的（审查 D04：客户端看不到的工具，要分清是服务没给还是客户端缓存了旧表）。
pub async fn served_tools() -> Option<Vec<serde_json::Value>> {
    let hub = RUNTIME.lock().await.as_ref()?.hub.clone();
    // 列远端成员要读配置，别在 async worker 上做同步 IO。
    tokio::task::spawn_blocking(move || hub.list_tools())
        .await
        .ok()
}

/// 跑着的服务实际用的公网基地址和隧道报错；没在跑是 `None`。
pub async fn public_base() -> Option<(String, String)> {
    RUNTIME
        .lock()
        .await
        .as_ref()
        .map(|runtime| (runtime.public_base.clone(), runtime.tunnel_error.clone()))
}

/// 此刻公网那一头是什么样，给体检用；服务没在跑是 `None`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelSnapshot {
    /// 这次起来实际用的公网基地址（Cloudflare 临时地址只有这里有）。
    pub public_base: String,
    /// 起隧道时的报错，空串表示没有。
    pub error: String,
    /// gld 自己起着一个隧道进程（cloudflared / frpc）吗。自建反代、
    /// 手填地址、没有公网入口时都是 false。
    pub managed: bool,
    pub pid: Option<u32>,
    /// 那个进程还在不在；不是 gld 起的隧道时是 `None`。
    pub alive: Option<bool>,
}

pub async fn tunnel_snapshot() -> Option<TunnelSnapshot> {
    let mut guard = RUNTIME.lock().await;
    let runtime = guard.as_mut()?;
    let (managed, pid, alive) = match runtime.tunnel.as_mut() {
        Some(tunnel) => (true, tunnel.pid(), Some(tunnel.still_running())),
        None => (false, None, None),
    };
    Some(TunnelSnapshot {
        public_base: runtime.public_base.clone(),
        error: runtime.tunnel_error.clone(),
        managed,
        pid,
        alive,
    })
}

/// 服务这次运行期间的请求统计（所有项目合计）。没在跑是 `None`。
pub async fn usage() -> Option<crate::usage::ServiceUsageStats> {
    RUNTIME
        .lock()
        .await
        .as_ref()
        .map(|runtime| runtime.hub.usage().snapshot(super::HUB_SCOPE, "mcp"))
}

pub async fn state() -> HubState {
    match RUNTIME.lock().await.as_ref() {
        None => HubState::Stopped,
        Some(runtime) if runtime.handle.is_finished() => HubState::Exited { port: runtime.port },
        Some(runtime) => HubState::Running { port: runtime.port },
    }
}

async fn stop_runtime(mut runtime: HubRuntime) {
    STARTED.store(false, std::sync::atomic::Ordering::Relaxed);
    if let Some(shutdown) = runtime.shutdown.take() {
        let _ = shutdown.send(());
    }
    await_listener_shutdown(Some(runtime.handle), runtime.port).await;
    if let Some(tunnel) = runtime.tunnel.take() {
        tunnel.stop().await;
    }
    // 成员里经 hub 起的命令，hub 停了就没人能再读它们的输出或杀掉它们。
    // 结束进程要阻塞等待，不能占着异步 worker。
    let hub = runtime.hub;
    let _ = tokio::task::spawn_blocking(move || hub.shutdown()).await;
}

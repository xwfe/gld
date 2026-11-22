//! 应用配置和宿主监听管理共用的 Hub 状态与输入。

pub use crate::managed_workspaces::startup::StartupSweep;
use crate::tunnel::standalone::StandaloneTunnel;

/// 这次起隧道的结果，由调用方（`App`）起好交进来。
pub struct TunnelOutcome {
    pub public_base: String,
    pub tunnel: Option<StandaloneTunnel>,
    pub error: String,
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

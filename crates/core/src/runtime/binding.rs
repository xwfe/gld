//! 监听管理只记住实际监听任务和停止信号；宿主负责读启动凭据和绑定端口。

use std::sync::Arc;

use futures_util::future::BoxFuture;

use super::ServiceKind;
use crate::hub::runtime::{HubState, TunnelOutcome, TunnelSnapshot};
use crate::hub::HubSecrets;
use crate::settings::HubConfig;
use crate::usage::ServiceUsage;
use crate::workspace::WorkspaceProfile;
use crate::AppResult;

pub type ShutdownSender = tokio::sync::oneshot::Sender<()>;
pub type BoundListener = (ShutdownSender, crate::async_rt::JoinHandle<()>);

pub trait ListenerHost: Send + Sync {
    /// 启动选择经全局入口暴露的监听器前，先确保入口在跑。
    fn ensure_gateway(&self) -> BoxFuture<'_, AppResult<()>>;

    /// 凭据读取出错直接返回 AppError；绑定/监听失败由 Supervisor 记为服务失败。
    fn bind_workspace(
        &self,
        profile: &WorkspaceProfile,
        kind: ServiceKind,
        port: u16,
        usage: Arc<ServiceUsage>,
    ) -> AppResult<Result<BoundListener, String>>;

    fn start_hub<'a>(
        &'a self,
        config: &'a HubConfig,
        outcome: TunnelOutcome,
        secrets: HubSecrets,
    ) -> BoxFuture<'a, AppResult<()>>;
    fn stop_hub(&self) -> BoxFuture<'_, ()>;
    fn attach_hub_tunnel(&self, outcome: TunnelOutcome) -> BoxFuture<'_, ()>;
    fn hub_state(&self) -> BoxFuture<'_, HubState>;
    fn hub_public_base(&self) -> BoxFuture<'_, Option<(String, String)>>;
    fn hub_tunnel_snapshot(&self) -> BoxFuture<'_, Option<TunnelSnapshot>>;
}

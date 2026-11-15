//! 监听管理只记住实际监听任务和停止信号；宿主负责读启动凭据和绑定端口。

use std::sync::Arc;

use futures_util::future::BoxFuture;

use super::ServiceKind;
use crate::hub::{Hub, HubSecrets};
use crate::nodes::registry::Nodes;
use crate::settings::HubConfig;
use crate::usage::ServiceUsage;
use crate::workspace::WorkspaceProfile;
use crate::AppResult;

pub type ShutdownSender = tokio::sync::oneshot::Sender<()>;
pub type BoundListener = (ShutdownSender, crate::async_rt::JoinHandle<()>);

pub trait ListenerBinder: Send + Sync {
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

    fn bind_hub(
        &self,
        config: &HubConfig,
        hub: Arc<Hub>,
        nodes: Arc<Nodes>,
        public_base: String,
        secrets: HubSecrets,
    ) -> Result<BoundListener, String>;
}

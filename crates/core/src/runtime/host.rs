//! 应用侧组合 Hub/网关时需要的宿主操作。

use futures_util::future::BoxFuture;

use crate::hub::runtime::{HubState, TunnelOutcome, TunnelSnapshot};
use crate::hub::HubSecrets;
use crate::settings::HubConfig;
use crate::AppResult;

pub trait ListenerHost: Send + Sync {
    /// 启动选择经全局入口暴露的监听器前，先确保入口在跑。
    fn ensure_gateway(&self) -> BoxFuture<'_, AppResult<()>>;

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

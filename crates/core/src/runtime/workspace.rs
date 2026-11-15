//! 应用流程需要的工作区监听状态，不包含端口或任务的实现。

use std::collections::HashSet;

use super::ServiceKind;
use crate::async_rt::JoinHandle;
use crate::tunnel::TunnelServiceKind;
use crate::usage::ServiceUsageStats;
use crate::workspace::{RuntimeStatusDto, WorkspaceProfile};
use crate::AppResult;

/// 宿主管理实际表；应用在原同步锁内取快照或推进一次状态转换。
/// 发出停止信号后，应用放锁再等任务退出，最后摘表与同步隧道路由。
pub trait WorkspaceListeners: Send {
    fn status(&self, profile: &WorkspaceProfile, kind: ServiceKind) -> RuntimeStatusDto;
    fn start(
        &mut self,
        profile: &WorkspaceProfile,
        kind: ServiceKind,
    ) -> AppResult<RuntimeStatusDto>;
    fn refresh(&mut self, profile: &WorkspaceProfile, kind: ServiceKind);
    fn running_workspace_ids(&self, kind: ServiceKind) -> Vec<String>;
    fn usage_stats(&self, workspace_id: &str, kind: ServiceKind) -> ServiceUsageStats;
    fn is_running(&self, workspace_id: &str, kind: ServiceKind) -> bool;
    fn is_active(&self, workspace_id: &str, kind: ServiceKind) -> bool;
    fn drop_workspace(&mut self, profile: &WorkspaceProfile);
    fn active_tunnel_service_keys(&self) -> HashSet<(String, TunnelServiceKind)>;
    fn begin_stop(&mut self, workspace_id: &str, kind: ServiceKind) -> Option<JoinHandle<()>>;
    fn finish_stop(&mut self, workspace_id: &str, kind: ServiceKind);
}

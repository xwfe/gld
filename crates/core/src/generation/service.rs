//! 仍由 core 组合的监听器所需的具体执行代操作；不负责 CLI、Agent 或子进程管理。

use std::path::Path;
use std::sync::Arc;

use gld_protocol::generation::ExecutionRoute;
use serde_json::Value;

use crate::auth::AuthContext;
use crate::hub::NodeBridge;
use crate::settings::HubConfig;

/// 转发失败的三种情况，调用方靠它决定告诉客户端什么。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    /// 请求没发到执行代，确定没执行。
    Unavailable(String),
    /// 发出去之后执行代没了：结果未知，可能已经产生副作用，绝不能自动重放。
    Lost,
    /// 控制类操作等超时。
    TimedOut,
    /// 执行代回了错误。
    Refused(String),
}

impl CallError {
    /// 确定没有执行，客户端可以放心重试。其余情况工具可能已经跑过（`Lost`，或者跑完了回包发不出来、
    /// panic 的 `Refused`），重试有可能把副作用做两遍，只能让调用方自己查。
    pub fn surely_not_executed(&self) -> bool {
        matches!(self, Self::Unavailable(_))
    }
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(reason) => write!(f, "执行代不可用：{reason}"),
            Self::Lost => write!(f, "执行代在处理这次调用时退出了，结果未知"),
            Self::TimedOut => write!(f, "等执行代回复超时"),
            Self::Refused(reason) => write!(f, "{reason}"),
        }
    }
}

/// 一次 MCP 请求的结果，监听器拿它写日志和回包。
pub struct McpReply {
    pub response: Value,
    pub member: Option<String>,
    pub audit: Value,
}

/// 宿主为监听器提供的执行代转发与 Hub 生命周期。
/// 只有未装配时才走本地执行，转发错误不允许回退本地。
pub trait ServiceGeneration: Send + Sync {
    fn hub_mcp(
        &self,
        route: ExecutionRoute,
        auth: &AuthContext,
        body: &Value,
    ) -> Result<McpReply, CallError>;
    fn workspace_mcp(
        &self,
        workspace_id: &str,
        root: &Path,
        auth: &AuthContext,
        body: &Value,
    ) -> Result<McpReply, CallError>;
    fn actions_call(
        &self,
        workspace_id: &str,
        root: &Path,
        auth: &AuthContext,
        tool: &str,
        arguments: &Value,
    ) -> Result<Value, CallError>;
    fn hub_started(&self, config: &HubConfig, nodes: Arc<dyn NodeBridge>);
    fn hub_stopping(&self);
    fn served_tools(&self) -> Result<Value, CallError>;
}

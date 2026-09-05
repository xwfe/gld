//! 命令行与守护进程之间的请求 / 响应类型。
//!
//! 编码：每条消息是一行 JSON（末尾 `\n`），一个连接只处理一个请求。
//! 这样用 `nc -U ~/.config/gld/daemon.sock` 就能手工调试，也不需要任何帧格式。
//!
//! 兼容性：`Request` 用 `op` 字段区分变体。升级二进制后旧守护进程仍可能
//! 在跑，命令行会先比较 [`DaemonInfo::protocol`] 与 [`PROTOCOL_VERSION`]，
//! 不一致就提示重启守护进程，而不是发一个对方不认识的请求。

use std::path::PathBuf;

use gld_core::app::{
    GlobalRuntimeSettingsDto, PlanStepUpdate, WorkspaceCreateOptions, WorkspaceTarget,
};
use gld_core::planning::{GoalStatus, PlanStatus, PlanningMode};
use gld_core::runtime::ServiceKind;
use gld_core::settings::{FrpProfile, GlobalGatewayConfig, ProxyConfig};
use gld_core::tunnel::TunnelServiceKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 协议版本。请求 / 响应形状有不兼容改动时递增。
///
/// 2：`set_workspace_fields` 的响应从裸的 WorkspaceProfile 变成
///    `WorkspaceUpdate`（多了重启结果），旧命令行解析不了新守护进程的回包。
pub const PROTOCOL_VERSION: u32 = 2;

/// 守护进程自述，用于 `gld daemon status` 与版本核对。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonInfo {
    pub pid: u32,
    pub version: String,
    pub protocol: u32,
    pub started_at_unix: u64,
    pub uptime_secs: u64,
    pub data_home: PathBuf,
    pub socket: PathBuf,
    pub log_file: PathBuf,
    pub running_services: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    // ---- 守护进程本身 ----
    Ping,
    DaemonInfo,
    Shutdown,

    // ---- 工作区 ----
    ListWorkspaces,
    ResolveWorkspace {
        target: WorkspaceTarget,
    },
    CreateWorkspace {
        path: PathBuf,
        options: WorkspaceCreateOptions,
    },
    /// 按目录取工作区，没登记过就当场登记（`gld start <目录>` 用）。
    EnsureWorkspace {
        path: PathBuf,
        options: WorkspaceCreateOptions,
    },
    SetWorkspaceFields {
        target: WorkspaceTarget,
        assignments: Vec<(String, String)>,
    },
    DeleteWorkspace {
        target: WorkspaceTarget,
    },
    UseWorkspace {
        target: WorkspaceTarget,
    },

    // ---- MCP / Actions 服务 ----
    StartService {
        target: WorkspaceTarget,
        kind: ServiceKind,
    },
    StopService {
        target: WorkspaceTarget,
        kind: ServiceKind,
    },
    RestartService {
        target: WorkspaceTarget,
        kind: ServiceKind,
    },
    ServiceStatus {
        target: WorkspaceTarget,
        kind: ServiceKind,
    },
    Overview,

    // ---- 隧道 ----
    TunnelStart {
        target: WorkspaceTarget,
        kind: TunnelServiceKind,
    },
    TunnelStop {
        target: WorkspaceTarget,
        kind: TunnelServiceKind,
    },
    TunnelRestart {
        target: WorkspaceTarget,
        kind: TunnelServiceKind,
    },
    TunnelTest {
        target: WorkspaceTarget,
        kind: TunnelServiceKind,
    },
    TunnelStatus {
        target: WorkspaceTarget,
        kind: TunnelServiceKind,
    },
    FrpSnippet {
        target: WorkspaceTarget,
        kind: TunnelServiceKind,
        /// 输出真实的 frps token，而不是占位符。
        #[serde(default)]
        reveal: bool,
    },

    // ---- 全局共享入口 ----
    GatewayConfig,
    SetGatewayConfig {
        config: GlobalGatewayConfig,
    },
    GatewayStart,
    GatewayStop,
    GatewayStatus,
    GatewayHealth,

    // ---- 密钥 ----
    WorkspaceSecret {
        target: WorkspaceTarget,
        key: String,
    },
    SetWorkspaceSecret {
        target: WorkspaceTarget,
        key: String,
        value: String,
    },
    RegenerateWorkspaceSecret {
        target: WorkspaceTarget,
        key: String,
    },
    SharedSecret {
        key: String,
    },
    SetSharedSecret {
        key: String,
        value: String,
    },
    RegenerateSharedSecret {
        key: String,
    },

    // ---- 设置 ----
    Proxy,
    SetProxy {
        proxy: ProxyConfig,
    },
    RuntimeSettings,
    SetRuntimeSettings {
        runtime: GlobalRuntimeSettingsDto,
    },
    ListFrpProfiles,
    SaveFrpProfile {
        profile: FrpProfile,
        token: Option<String>,
    },
    DeleteFrpProfile {
        id: String,
        /// 还有工作区引用它时也照删（会留下悬空引用）。
        #[serde(default)]
        force: bool,
    },

    // ---- 日志 ----
    Logs {
        target: WorkspaceTarget,
        kind: ServiceKind,
        max_bytes: usize,
    },
    LogDir {
        target: WorkspaceTarget,
    },

    // ---- Planning ----
    PlanningState {
        target: WorkspaceTarget,
    },
    SetPlanningMode {
        target: WorkspaceTarget,
        mode: PlanningMode,
    },
    CreateGoal {
        target: WorkspaceTarget,
        title: String,
        objective: String,
        success_criteria: Vec<String>,
        constraints: Vec<String>,
    },
    UpdateGoal {
        target: WorkspaceTarget,
        goal_id: String,
        title: Option<String>,
        objective: Option<String>,
        status: Option<GoalStatus>,
        constraints: Option<Vec<String>>,
        completed_criteria_ids: Option<Vec<String>>,
        focus: Option<bool>,
    },
    CreatePlan {
        target: WorkspaceTarget,
        goal_id: Option<String>,
        title: String,
        objective: String,
        steps: Vec<String>,
    },
    UpdatePlan {
        target: WorkspaceTarget,
        plan_id: String,
        status: Option<PlanStatus>,
        step_updates: Vec<PlanStepUpdate>,
        focus: Option<bool>,
    },
    AcceptGoalReview {
        target: WorkspaceTarget,
        goal_id: String,
    },
    RejectGoalReview {
        target: WorkspaceTarget,
        goal_id: String,
        feedback: Option<String>,
    },
    AcceptPlanReview {
        target: WorkspaceTarget,
        plan_id: String,
    },
    RejectPlanReview {
        target: WorkspaceTarget,
        plan_id: String,
        feedback: Option<String>,
    },

    // ---- 工具内核 ----
    ListTools {
        target: WorkspaceTarget,
    },
    CallTool {
        target: WorkspaceTarget,
        name: String,
        args: Value,
    },

    // ---- 观察 ----
    Doctor,
    Health {
        target: WorkspaceTarget,
    },
    HistorySessions {
        target: WorkspaceTarget,
    },
    Usage {
        target: WorkspaceTarget,
    },
    AgentContext {
        target: WorkspaceTarget,
    },
    GlobalAgentContext,
}

impl Request {
    /// 这个请求是否只有守护进程能正确执行。
    ///
    /// 启动 / 停止服务、隧道、全局入口都会改变守护进程内存里的运行状态，
    /// 命令行直连执行只会得到一个随进程退出而消失的服务。其余请求
    /// （改配置、看日志、跑健康检查）在守护进程不在时直连执行是安全的。
    pub fn needs_daemon(&self) -> bool {
        matches!(
            self,
            Request::Ping
                | Request::DaemonInfo
                | Request::Shutdown
                | Request::StartService { .. }
                | Request::StopService { .. }
                | Request::RestartService { .. }
                | Request::TunnelStart { .. }
                | Request::TunnelStop { .. }
                | Request::TunnelRestart { .. }
                | Request::TunnelTest { .. }
                | Request::GatewayStart
                | Request::GatewayStop
        )
    }

    /// 可能耗时较久的请求（启动隧道要等公网地址、跑构建的工具调用）。
    pub fn is_slow(&self) -> bool {
        matches!(
            self,
            Request::StartService { .. }
                | Request::RestartService { .. }
                // 改配置会顺带重启受影响的服务，连带重连隧道，和 restart 一样慢。
                | Request::SetWorkspaceFields { .. }
                | Request::TunnelStart { .. }
                | Request::TunnelRestart { .. }
                | Request::TunnelTest { .. }
                | Request::GatewayStart
                | Request::Health { .. }
                | Request::GatewayHealth
                // 工具调用可能跑测试或构建，几分钟都算正常。
                | Request::CallTool { .. }
        )
    }

    /// 日志里用的短名字，不带参数（参数里可能有密钥）。
    pub fn op_name(&self) -> String {
        serde_json::to_value(self)
            .ok()
            .and_then(|value| value.get("op").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_else(|| "unknown".into())
    }
}

/// 出错时的结构化信息。`code` 用来区分“业务拒绝”和“协议 / 内部错误”。
#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct RpcError {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// 业务层拒绝（工作区不存在、端口冲突……），消息可直接给用户看。
    App,
    /// 请求无法解析或形状不对。
    Protocol,
    /// 守护进程内部异常（任务 panic 等）。
    Internal,
}

impl RpcError {
    pub fn app(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::App,
            message: message.into(),
        }
    }

    pub fn protocol(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::Protocol,
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::Internal,
            message: message.into(),
        }
    }
}

impl From<gld_core::AppError> for RpcError {
    fn from(error: gld_core::AppError) -> Self {
        Self::app(error.to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Ok { result: Value },
    Error { error: RpcError },
}

impl Response {
    pub fn into_result(self) -> Result<Value, RpcError> {
        match self {
            Response::Ok { result } => Ok(result),
            Response::Error { error } => Err(error),
        }
    }
}

impl From<Result<Value, RpcError>> for Response {
    fn from(result: Result<Value, RpcError>) -> Self {
        match result {
            Ok(result) => Response::Ok { result },
            Err(error) => Response::Error { error },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip_through_json_with_an_op_tag() {
        let request = Request::StartService {
            target: WorkspaceTarget::selector("api"),
            kind: ServiceKind::Mcp,
        };
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"op\":\"start_service\""));
        assert!(json.contains("\"kind\":\"mcp\""));
        let parsed: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.op_name(), "start_service");
        assert!(parsed.needs_daemon());
        assert!(!Request::ListWorkspaces.needs_daemon());
    }

    #[test]
    fn responses_round_trip() {
        let ok: Response = Ok(serde_json::json!({"a": 1})).into();
        let text = serde_json::to_string(&ok).unwrap();
        let parsed: Response = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed.into_result().unwrap()["a"], 1);

        let err: Response = Err(RpcError::app("nope")).into();
        let text = serde_json::to_string(&err).unwrap();
        let parsed: Response = serde_json::from_str(&text).unwrap();
        let error = parsed.into_result().unwrap_err();
        assert_eq!(error.code, ErrorCode::App);
        assert_eq!(error.message, "nope");
    }
}

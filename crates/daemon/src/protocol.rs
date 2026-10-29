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
    BrowserGrantSpec, CcnmMemberSpec, GlobalRuntimeSettingsDto, GrantSpec, PlanStepUpdate,
    PruneReport, WorkspaceCreateOptions, WorkspaceTarget,
};
use gld_core::planning::{GoalStatus, PlanStatus, PlanningMode};
use gld_core::runtime::ServiceKind;
use gld_core::settings::{FrpProfile, GlobalGatewayConfig, HubConfig, ProxyConfig};
use gld_core::tunnel::TunnelServiceKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 协议版本。请求 / 响应形状有不兼容改动时递增。
///
/// 2：`set_workspace_fields` 的响应从裸的 WorkspaceProfile 变成
///    `WorkspaceUpdate`（多了重启结果），旧命令行解析不了新守护进程的回包。
/// 3：只留多项目模式（RFC-0004），命令行改用 `hub_ensure_started`、`set_hub_secret`、
///    `hub_join_all`、`hub_health`、`hub_usage`、`hub_logs`。旧守护进程不认这几个，
///    不递增的话升级二进制后第一条 `gld start` 报的是"看不懂的请求"，而不是
///    "请 gld daemon restart"。
///    同一版（还没发布过）里又加了 `mcp_servers`、`switch_mcp_servers`、
///    `test_mcp_server`（RFC-0006），没再递增。
/// 4：加了 `served_tools`（`gld tool list --served`）。当时漏了递增，2026-09-23 真机
///    升级时新命令行连着旧守护进程，`--served` 报的是 `unknown variant`，不是"请重启"。
/// 5：加了 `list_grants`、`add_grant`、`remove_grant`（`gld grant`，RFC-0007）。
/// 6：加了 `add_browser_grant`，与普通永久 grant 分开，旧守护进程不能静默降级。
/// 7：加了 `list_agent_tasks`、`show_agent_task`、`approve_agent_task`、`deny_agent_task`、
///    `stop_agent_task`（`gld agent`，RFC-0009）。
/// 8：`approve_agent_task` 多了 `agent`、`model`、`effort`（放行时换 Agent、模型、推理强度）。旧守护进程不认这几个字段，
///    会照 AI 的提议跑、悄悄丢掉用户的修改，所以递增，让命令行先报"请重启"。
/// 9：`remove_grant` 的回包多了 `stoppedAgentTasks`，委派任务多了 `grant`（`--write` grant 能委派了）。旧守护进程
///    撤销 grant 时不停它委派的 Agent，新命令行却会当成"停了 0 个"，所以递增。
/// 10：服务配置多了 `openaiTunnelId`、隧道类型多了 `openai`。旧守护进程收到 `--tunnel openai:…` 会报
///    "看不懂的隧道类型"，还会把 ID 字段丢掉，所以递增，让命令行先报"请重启"。
///    同一版（还没换装过）里又加了 `prune`（`gld prune`），没再递增。
/// 11：服务配置增加 workSessions 开关；旧守护进程会忽略它，不能把开关已生效报给用户。
/// 12：workSessionLimits 加入服务配置，旧守护进程不能静默忽略容量约束。
/// 13：新增 node_register 节点凭据登记，旧守护进程需要升级后才能处理。
/// 14：新增节点凭据撤销/轮换，成功返回前收回失效连接。
/// 15：新增 node_invite，一次性邀请由本机操作员经配置写入口生成。
pub const PROTOCOL_VERSION: u32 = 15;

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

    NodeInvite {
        name: String,
    },
    NodeRegister {
        name: String,
        id: Option<String>,
        credential: String,
    },
    NodeRevoke {
        machine_id: String,
    },
    NodeRotateCredential {
        machine_id: String,
        credential: String,
    },

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
    /// `gld prune`：`confirmed` 为空只列清单；给了就删其中这次还扫得出来的。
    Prune {
        confirmed: Option<PruneReport>,
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

    // ---- 聚合入口 ----
    HubStatus,
    SetHubConfig {
        config: HubConfig,
    },
    HubAddMembers {
        targets: Vec<WorkspaceTarget>,
    },
    HubRemoveMembers {
        targets: Vec<WorkspaceTarget>,
    },
    /// 登记一个远端 ccnm workspace 并加进 hub。
    HubAddRemote {
        spec: CcnmMemberSpec,
    },
    /// 删掉一个远端成员（按 id、名字或 id 前缀）。
    HubRemoveRemote {
        selector: String,
    },
    HubStart,
    HubStop,
    HubSecret {
        key: String,
    },
    RegenerateHubSecret {
        key: String,
    },
    /// 自己定一项服务凭据（授权口令、Cloudflare Tunnel Token）。
    SetHubSecret {
        key: String,
        value: String,
    },
    /// 把登记了但还不在服务里的项目都加进来（RFC-0004 之前的老数据）。
    HubJoinAll,
    /// 服务的本地 / 公网 / OAuth 元数据检查。
    HubHealth,
    /// 没在跑就起；跑得好好的什么都不做（`gld start`）。`HubStart` 是按当前配置重启。
    HubEnsureStarted,
    /// 服务这次运行期间的请求统计；没在跑是 null。
    HubUsage,
    /// 服务自己的日志尾部。
    HubLogs {
        max_bytes: usize,
    },

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

    // ---- 本机装好的 MCP server（RFC-0006）----
    /// 装了哪些、开了哪些。
    McpServers,
    /// 开 / 关一组；`all` 只在关的时候有意义：全关。
    SwitchMcpServers {
        names: Vec<String>,
        on: bool,
        #[serde(default)]
        all: bool,
    },
    /// 在守护进程里起一次、握手、列工具，看它起不起得来。
    TestMcpServer {
        name: String,
    },

    // ---- 只开部分项目的凭据（RFC-0007）----
    ListGrants,
    AddGrant {
        spec: GrantSpec,
    },
    AddBrowserGrant {
        spec: BrowserGrantSpec,
    },
    /// 作废一把，并停掉它起的命令：必须在守护进程里做，命令会话在这里。
    RemoveGrant {
        selector: String,
    },

    // ---- 委派给本机 Agent（RFC-0009）----
    ListAgentTasks,
    ShowAgentTask {
        task_id: String,
    },
    /// 起 Agent：进程和"在跑的表"都得在守护进程里，不然命令行一退就没人管了。
    ApproveAgentTask {
        task_id: String,
        /// 放行时换成别的 Agent、模型、推理强度；不给就照 AI 的提议。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effort: Option<String>,
    },
    DenyAgentTask {
        task_id: String,
    },
    /// 停在跑的：只有起它的守护进程能停。
    StopAgentTask {
        task_id: String,
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
    /// 正在跑的服务给客户端的 tools/list（服务级，和项目无关）。
    ServedTools,
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
                | Request::HubStart
                | Request::HubEnsureStarted
                | Request::HubStop
                // 要的是服务起 server 时的那个环境（PATH、环境变量），不是这个
                // 终端的：两边不一样正是最常见的起不来的原因。
                | Request::TestMcpServer { .. }
                | Request::ApproveAgentTask { .. }
                | Request::StopAgentTask { .. }
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
                // 这三个在 hub 正跑着时会重启它，走全局入口的还要等入口拿到公网地址。
                | Request::HubStart
                | Request::HubEnsureStarted
                | Request::SetHubConfig { .. }
                | Request::RegenerateHubSecret { .. }
                | Request::SetHubSecret { .. }
                | Request::Health { .. }
                | Request::HubHealth
                | Request::GatewayHealth
                // 工具调用可能跑测试或构建，几分钟都算正常。
                | Request::CallTool { .. }
                // `npx -y` 第一次要下载包，握手最多等 30 秒。
                | Request::TestMcpServer { .. }
                // 要停它起的命令，每条最多等 1.5 秒收尾。
                | Request::RemoveGrant { .. }
                // 持久变更后等该节点已有的有界 RPC 收尾，再关闭实际连接。
                | Request::NodeRevoke { .. }
                | Request::NodeRotateCredential { .. }
                // 要算目录大小、删整棵目录，日志攒久了有几百 MB。
                | Request::Prune { .. }
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

    #[test]
    fn node_credential_changes_allow_time_for_in_flight_calls_to_finish() {
        assert!(Request::NodeRevoke {
            machine_id: "a".repeat(32)
        }
        .is_slow());
        assert!(Request::NodeRotateCredential {
            machine_id: "a".repeat(32),
            credential: "synthetic-node-credential".into()
        }
        .is_slow());
        assert!(!Request::NodeRegister {
            name: "node".into(),
            id: None,
            credential: "synthetic-node-credential".into()
        }
        .is_slow());
    }
}

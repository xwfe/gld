//! RFC-0016 D19-A1 的临时能力契约样例：纯数据、选择器和确定性校验。
//!
//! 这里的能力 ID 是 RFC §4.1 的设计分组样例，不是已发布的产品能力；
//! 不得进入 CLI 帮助、MCP 工具表或 concepts.md。`authorized` 由 gld-core 按
//! D18 `AuthContext` 对每次请求计算，本模块只承载节点探测到的前四项状态。
//!
//! 新鲜度属于接收方：接收方在自己的单调时钟上记录收到快照的时间，再把经过的
//! 毫秒数交给评估；`observed_at_ms` 只是签发方单调时钟上的观察戳，只在同一
//! `module_generation` 内有顺序意义，不能拿两台机器的时钟相减。节点重启必须
//! 递增 `module_generation`，否则旧观察可能被当成新节点的状态。

use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;

pub const CAPABILITY_CONTRACT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityId {
    FileCommand,
    DesktopObserve,
    AppOpen,
    DesktopInput,
    Clipboard,
    SessionProvider,
    ProjectAutonomy,
    MachineProvisioner,
}

impl CapabilityId {
    pub const ALL: [Self; 8] = [
        Self::FileCommand,
        Self::DesktopObserve,
        Self::AppOpen,
        Self::DesktopInput,
        Self::Clipboard,
        Self::SessionProvider,
        Self::ProjectAutonomy,
        Self::MachineProvisioner,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::FileCommand => "file_command",
            Self::DesktopObserve => "desktop_observe",
            Self::AppOpen => "app_open",
            Self::DesktopInput => "desktop_input",
            Self::Clipboard => "clipboard",
            Self::SessionProvider => "session_provider",
            Self::ProjectAutonomy => "project_autonomy",
            Self::MachineProvisioner => "machine_provisioner",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|id| id.as_str() == value)
    }
}

impl fmt::Display for CapabilityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 节点身份只复用 D18 已有的 hub/machine 标识；显示名、IP、主机名不参与绑定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeIdentity {
    pub hub_id: String,
    pub machine_id: String,
}

impl NodeIdentity {
    fn validate(&self) -> Result<(), CapabilityContractError> {
        if self.hub_id.trim().is_empty() {
            return Err(CapabilityContractError::InvalidSnapshot("hub_id"));
        }
        if self.machine_id.trim().is_empty() {
            return Err(CapabilityContractError::InvalidSnapshot("machine_id"));
        }
        Ok(())
    }
}

/// workspace 与 machine 路由二选一；同时给两个或都不给都是确定错误，不做猜测。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilitySelector {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
}

impl CapabilitySelector {
    pub fn workspace(workspace_id: impl Into<String>) -> Self {
        Self {
            workspace_id: Some(workspace_id.into()),
            machine_id: None,
        }
    }

    pub fn machine(machine_id: impl Into<String>) -> Self {
        Self {
            workspace_id: None,
            machine_id: Some(machine_id.into()),
        }
    }

    pub fn route(&self) -> Result<CapabilityRoute, CapabilityContractError> {
        let workspace = non_empty(self.workspace_id.as_deref(), "workspace_id")?;
        let machine = non_empty(self.machine_id.as_deref(), "machine_id")?;
        match (workspace, machine) {
            (Some(workspace), None) => Ok(CapabilityRoute::Workspace(workspace)),
            (None, Some(machine)) => Ok(CapabilityRoute::Machine(machine)),
            (Some(_), Some(_)) => Err(CapabilityContractError::AmbiguousTarget),
            (None, None) => Err(CapabilityContractError::MissingTarget),
        }
    }
}

/// 能力查询 = 目标路由 + 可选能力过滤。空 `capability_ids` 表示该目标的全部能力；
/// 非空列表按字符串进入，先校验再过滤，未知 ID 返回稳定错误而不是静默丢弃。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityQuery {
    pub selector: CapabilitySelector,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capability_ids: Vec<String>,
}

impl CapabilityQuery {
    pub fn all(selector: CapabilitySelector) -> Self {
        Self {
            selector,
            capability_ids: Vec::new(),
        }
    }

    pub fn only(selector: CapabilitySelector, capability_ids: Vec<String>) -> Self {
        Self {
            selector,
            capability_ids,
        }
    }

    pub fn route(&self) -> Result<CapabilityRoute, CapabilityContractError> {
        self.selector.route()
    }

    pub fn validate(&self) -> Result<(), CapabilityContractError> {
        self.selector.route()?;
        for id in &self.capability_ids {
            if CapabilityId::parse(id).is_none() {
                return Err(CapabilityContractError::UnknownCapability(id.clone()));
            }
        }
        Ok(())
    }

    pub fn includes(&self, id: CapabilityId) -> bool {
        self.capability_ids.is_empty()
            || self
                .capability_ids
                .iter()
                .any(|requested| CapabilityId::parse(requested) == Some(id))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityRoute {
    Workspace(String),
    Machine(String),
}

fn non_empty(
    value: Option<&str>,
    field: &'static str,
) -> Result<Option<String>, CapabilityContractError> {
    match value {
        None => Ok(None),
        Some(value) if value.trim().is_empty() => {
            Err(CapabilityContractError::EmptyIdentifier(field))
        }
        Some(value) => Ok(Some(value.to_string())),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityAvailabilityState {
    Ready,
    Unavailable,
    Unknown,
}

/// 稳定原因码。新版本出现的未知码必须让旧消费者默认拒绝，而不是猜测含义。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CapabilityReason(pub String);

impl CapabilityReason {
    pub const UNSUPPORTED_PLATFORM: &'static str = "unsupported_platform";
    pub const COMPONENT_NOT_INSTALLED: &'static str = "component_not_installed";
    pub const MODULE_DISABLED: &'static str = "module_disabled";
    pub const SESSION_UNAVAILABLE: &'static str = "session_unavailable";
    pub const RESOURCE_UNAVAILABLE: &'static str = "resource_unavailable";
    pub const CALLER_NOT_AUTHORIZED: &'static str = "caller_not_authorized";
    pub const PROJECT_NOT_ALLOWED: &'static str = "project_not_allowed";
    pub const CAPABILITY_NOT_GRANTED: &'static str = "capability_not_granted";
    pub const STALE_SNAPSHOT: &'static str = "stale_snapshot";

    pub fn is_known(&self) -> bool {
        matches!(
            self.0.as_str(),
            Self::UNSUPPORTED_PLATFORM
                | Self::COMPONENT_NOT_INSTALLED
                | Self::MODULE_DISABLED
                | Self::SESSION_UNAVAILABLE
                | Self::RESOURCE_UNAVAILABLE
                | Self::CALLER_NOT_AUTHORIZED
                | Self::PROJECT_NOT_ALLOWED
                | Self::CAPABILITY_NOT_GRANTED
                | Self::STALE_SNAPSHOT
        )
    }
}

/// `observed_at_ms` 是签发方单调时钟的观察戳，只在同一 `module_generation` 内有
/// 顺序意义；消费者不得用自己的时钟去减它。有效期由接收方用自己的单调时钟判定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityAvailability {
    pub state: CapabilityAvailabilityState,
    pub observed_at_ms: u64,
    pub validity_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<CapabilityReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityProbe {
    pub id: CapabilityId,
    pub supported: bool,
    pub installed: bool,
    pub enabled: bool,
    pub available: CapabilityAvailability,
}

impl CapabilityProbe {
    fn validate(&self) -> Result<(), CapabilityContractError> {
        if !self.supported {
            if self.installed {
                return Err(CapabilityContractError::InvalidProbe("installed"));
            }
            if self.enabled {
                return Err(CapabilityContractError::InvalidProbe("enabled"));
            }
        }
        if self.available.state == CapabilityAvailabilityState::Ready
            && !(self.supported && self.installed && self.enabled)
        {
            return Err(CapabilityContractError::InvalidProbe("available"));
        }
        self.available.validate()
    }
}

/// 线格式解析的唯一入口：先读 `contract_version`，再丢掉本构建不认识的能力 ID。
/// 未知能力按“不可用”处理，快照其余部分仍可用；未知原因码仍由 validate 拒绝。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CapabilitySnapshot {
    pub contract_version: u32,
    pub node: NodeIdentity,
    pub module_generation: u64,
    pub capabilities: Vec<CapabilityProbe>,
}

impl CapabilitySnapshot {
    pub fn from_json(json: &str) -> Result<Self, CapabilityContractError> {
        #[derive(Deserialize)]
        struct VersionProbe {
            contract_version: u32,
        }
        let version: VersionProbe = serde_json::from_str(json)
            .map_err(|_| CapabilityContractError::InvalidSnapshot("json"))?;
        if version.contract_version != CAPABILITY_CONTRACT_VERSION {
            return Err(CapabilityContractError::UnsupportedContractVersion {
                found: version.contract_version,
            });
        }

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct SnapshotWire {
            contract_version: u32,
            node: NodeIdentity,
            module_generation: u64,
            capabilities: Vec<ProbeWire>,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct ProbeWire {
            id: String,
            supported: bool,
            installed: bool,
            enabled: bool,
            available: CapabilityAvailability,
        }

        let wire: SnapshotWire = serde_json::from_str(json)
            .map_err(|_| CapabilityContractError::InvalidSnapshot("json"))?;
        let capabilities = wire
            .capabilities
            .into_iter()
            .filter_map(|probe| {
                let id = CapabilityId::parse(&probe.id)?;
                Some(CapabilityProbe {
                    id,
                    supported: probe.supported,
                    installed: probe.installed,
                    enabled: probe.enabled,
                    available: probe.available,
                })
            })
            .collect();
        let snapshot = Self {
            contract_version: wire.contract_version,
            node: wire.node,
            module_generation: wire.module_generation,
            capabilities,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub fn validate(&self) -> Result<(), CapabilityContractError> {
        if self.contract_version != CAPABILITY_CONTRACT_VERSION {
            return Err(CapabilityContractError::UnsupportedContractVersion {
                found: self.contract_version,
            });
        }
        self.node.validate()?;
        let mut seen = Vec::with_capacity(self.capabilities.len());
        for probe in &self.capabilities {
            if seen.contains(&probe.id) {
                return Err(CapabilityContractError::DuplicateCapability(probe.id));
            }
            seen.push(probe.id);
            probe.validate()?;
        }
        Ok(())
    }
}

impl CapabilityAvailability {
    fn validate(&self) -> Result<(), CapabilityContractError> {
        match self.state {
            CapabilityAvailabilityState::Ready if self.reason.is_some() => {
                Err(CapabilityContractError::InvalidAvailability("ready reason"))
            }
            CapabilityAvailabilityState::Ready => Ok(()),
            CapabilityAvailabilityState::Unavailable | CapabilityAvailabilityState::Unknown => {
                let Some(reason) = &self.reason else {
                    return Err(CapabilityContractError::InvalidAvailability("reason"));
                };
                if !reason.is_known() {
                    return Err(CapabilityContractError::UnknownReason(reason.0.clone()));
                }
                Ok(())
            }
        }
    }

    /// `elapsed_ms` 是接收方单调时钟上“收到快照到现在”的毫秒数；有效期只按它判。
    pub fn fresh_within(&self, elapsed_ms: u64) -> bool {
        elapsed_ms <= self.validity_ms
    }
}

/// 结构、版本和代次一次判完。单项有效期不在这里失败：过期能力由调用方按
/// 接收方单调时钟逐个标成 unknown，其余能力继续可用。
pub fn check_snapshot(
    snapshot: &CapabilitySnapshot,
    current_generation: u64,
) -> Result<(), CapabilityContractError> {
    snapshot.validate()?;
    match snapshot.module_generation.cmp(&current_generation) {
        std::cmp::Ordering::Less => {
            return Err(CapabilityContractError::OlderModuleGeneration {
                snapshot: snapshot.module_generation,
                current: current_generation,
            })
        }
        std::cmp::Ordering::Greater => {
            return Err(CapabilityContractError::NewerModuleGeneration {
                snapshot: snapshot.module_generation,
                current: current_generation,
            })
        }
        std::cmp::Ordering::Equal => {}
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityContractError {
    UnsupportedContractVersion { found: u32 },
    EmptyIdentifier(&'static str),
    AmbiguousTarget,
    MissingTarget,
    TargetMismatch,
    TargetDenied,
    UnknownCapability(String),
    UnknownReason(String),
    DuplicateCapability(CapabilityId),
    InvalidSnapshot(&'static str),
    InvalidProbe(&'static str),
    InvalidAvailability(&'static str),
    OlderModuleGeneration { snapshot: u64, current: u64 },
    NewerModuleGeneration { snapshot: u64, current: u64 },
}

impl CapabilityContractError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedContractVersion { .. } => "CAPABILITY_PROTOCOL_UNSUPPORTED",
            Self::EmptyIdentifier(_) => "CAPABILITY_TARGET_INVALID",
            Self::AmbiguousTarget | Self::MissingTarget | Self::TargetMismatch => {
                "CAPABILITY_TARGET_INVALID"
            }
            Self::TargetDenied => "CAPABILITY_TARGET_DENIED",
            Self::UnknownCapability(_) => "CAPABILITY_UNKNOWN",
            Self::UnknownReason(_) => "CAPABILITY_REASON_UNKNOWN",
            Self::DuplicateCapability(_)
            | Self::InvalidSnapshot(_)
            | Self::InvalidProbe(_)
            | Self::InvalidAvailability(_) => "CAPABILITY_SNAPSHOT_INVALID",
            Self::OlderModuleGeneration { .. } | Self::NewerModuleGeneration { .. } => {
                "CAPABILITY_SNAPSHOT_STALE"
            }
        }
    }
}

impl fmt::Display for CapabilityContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedContractVersion { found } => write!(
                f,
                "capability contract version {found} is not supported; expected {CAPABILITY_CONTRACT_VERSION}"
            ),
            Self::EmptyIdentifier(field) => write!(f, "{field} must not be empty"),
            Self::AmbiguousTarget => f.write_str("workspace_id and machine_id are mutually exclusive"),
            Self::MissingTarget => f.write_str("either workspace_id or machine_id is required"),
            Self::TargetMismatch => f.write_str("capability target does not match the snapshot node"),
            Self::TargetDenied => f.write_str("capability target is not authorized for this caller"),
            Self::UnknownCapability(id) => write!(f, "unknown capability id: {id}"),
            Self::UnknownReason(reason) => write!(f, "unknown capability reason code: {reason}"),
            Self::DuplicateCapability(id) => write!(f, "duplicate capability id: {id}"),
            Self::InvalidSnapshot(field) => write!(f, "invalid capability snapshot: {field}"),
            Self::InvalidProbe(field) => write!(f, "invalid capability probe: {field}"),
            Self::InvalidAvailability(field) => write!(f, "invalid capability availability: {field}"),
            Self::OlderModuleGeneration { snapshot, current } => write!(
                f,
                "capability snapshot generation {snapshot} is older than current generation {current}"
            ),
            Self::NewerModuleGeneration { snapshot, current } => write!(
                f,
                "capability snapshot generation {snapshot} is newer than current generation {current}"
            ),
        }
    }
}

impl Error for CapabilityContractError {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn availability(
        state: CapabilityAvailabilityState,
        reason: Option<&str>,
    ) -> CapabilityAvailability {
        CapabilityAvailability {
            state,
            observed_at_ms: 1_000,
            validity_ms: 500,
            reason: reason.map(|reason| CapabilityReason(reason.into())),
        }
    }

    fn probe(id: CapabilityId) -> CapabilityProbe {
        CapabilityProbe {
            id,
            supported: true,
            installed: true,
            enabled: true,
            available: availability(CapabilityAvailabilityState::Ready, None),
        }
    }

    fn snapshot() -> CapabilitySnapshot {
        CapabilitySnapshot {
            contract_version: CAPABILITY_CONTRACT_VERSION,
            node: NodeIdentity {
                hub_id: "hub-a".into(),
                machine_id: "machine-a".into(),
            },
            module_generation: 7,
            capabilities: CapabilityId::ALL.into_iter().map(probe).collect(),
        }
    }

    #[test]
    fn selectors_never_mix_workspace_and_machine() {
        assert_eq!(
            CapabilitySelector::workspace("ws").route().unwrap(),
            CapabilityRoute::Workspace("ws".into())
        );
        assert_eq!(
            CapabilitySelector::machine("machine").route().unwrap(),
            CapabilityRoute::Machine("machine".into())
        );
        let both = CapabilitySelector {
            workspace_id: Some("ws".into()),
            machine_id: Some("machine".into()),
        };
        assert_eq!(
            both.route().unwrap_err(),
            CapabilityContractError::AmbiguousTarget
        );
        let neither = CapabilitySelector {
            workspace_id: None,
            machine_id: None,
        };
        assert_eq!(
            neither.route().unwrap_err(),
            CapabilityContractError::MissingTarget
        );
        let empty = CapabilitySelector::workspace("  ");
        assert_eq!(
            empty.route().unwrap_err(),
            CapabilityContractError::EmptyIdentifier("workspace_id")
        );
    }

    #[test]
    fn capability_queries_filter_by_id_and_reject_unknown_ids() {
        let all = CapabilityQuery::all(CapabilitySelector::workspace("ws"));
        assert!(all.includes(CapabilityId::FileCommand));
        assert!(all.includes(CapabilityId::DesktopObserve));

        let only_file = CapabilityQuery::only(
            CapabilitySelector::workspace("ws"),
            vec![CapabilityId::FileCommand.as_str().into()],
        );
        assert!(only_file.includes(CapabilityId::FileCommand));
        assert!(!only_file.includes(CapabilityId::DesktopObserve));
        only_file.validate().unwrap();

        let unknown = CapabilityQuery::only(
            CapabilitySelector::machine("machine-a"),
            vec!["capability_from_the_future".into()],
        );
        assert_eq!(
            unknown.validate().unwrap_err(),
            CapabilityContractError::UnknownCapability("capability_from_the_future".into())
        );
        assert_eq!(unknown.validate().unwrap_err().code(), "CAPABILITY_UNKNOWN");
    }

    #[test]
    fn incoherent_probes_are_snapshot_invalid() {
        let mut unsupported_installed = snapshot();
        unsupported_installed.capabilities[0].supported = false;
        unsupported_installed.capabilities[0].installed = true;
        assert_eq!(
            unsupported_installed.validate().unwrap_err(),
            CapabilityContractError::InvalidProbe("installed")
        );
        assert_eq!(
            unsupported_installed.validate().unwrap_err().code(),
            "CAPABILITY_SNAPSHOT_INVALID"
        );

        let mut unsupported_enabled = snapshot();
        unsupported_enabled.capabilities[0].supported = false;
        unsupported_enabled.capabilities[0].installed = false;
        unsupported_enabled.capabilities[0].enabled = true;
        assert_eq!(
            unsupported_enabled.validate().unwrap_err(),
            CapabilityContractError::InvalidProbe("enabled")
        );

        let mut ready_disabled = snapshot();
        ready_disabled.capabilities[0].enabled = false;
        assert_eq!(
            ready_disabled.validate().unwrap_err(),
            CapabilityContractError::InvalidProbe("available")
        );

        let mut ready_not_installed = snapshot();
        ready_not_installed.capabilities[0].installed = false;
        assert_eq!(
            ready_not_installed.validate().unwrap_err(),
            CapabilityContractError::InvalidProbe("available")
        );
    }

    #[test]
    fn generation_version_and_identity_errors_are_stable() {
        let mut old = snapshot();
        old.module_generation = 6;
        assert_eq!(
            check_snapshot(&old, 7).unwrap_err(),
            CapabilityContractError::OlderModuleGeneration {
                snapshot: 6,
                current: 7
            }
        );
        let mut new = snapshot();
        new.module_generation = 8;
        assert_eq!(
            check_snapshot(&new, 7).unwrap_err(),
            CapabilityContractError::NewerModuleGeneration {
                snapshot: 8,
                current: 7
            }
        );

        let mut future = snapshot();
        future.contract_version = CAPABILITY_CONTRACT_VERSION + 1;
        assert_eq!(
            future.validate().unwrap_err(),
            CapabilityContractError::UnsupportedContractVersion { found: 2 }
        );

        let mut empty_node = snapshot();
        empty_node.node.hub_id = "  ".into();
        assert_eq!(
            empty_node.validate().unwrap_err(),
            CapabilityContractError::InvalidSnapshot("hub_id")
        );
        assert_eq!(
            empty_node.validate().unwrap_err().code(),
            "CAPABILITY_SNAPSHOT_INVALID"
        );
    }

    #[test]
    fn freshness_is_receiver_relative_and_per_capability() {
        let availability = availability(CapabilityAvailabilityState::Ready, None);
        assert!(availability.fresh_within(500));
        assert!(!availability.fresh_within(501));

        let mut one_stale = snapshot();
        one_stale.capabilities[0].available.validity_ms = 100;
        assert!(check_snapshot(&one_stale, 7).is_ok());
        assert!(!one_stale.capabilities[0].available.fresh_within(101));
        assert!(one_stale.capabilities[1].available.fresh_within(101));
    }

    #[test]
    fn from_json_reads_version_before_fields_and_drops_unknown_capabilities() {
        let mut future_version = serde_json::to_value(snapshot()).unwrap();
        future_version["contract_version"] = json!(2);
        future_version["future_field"] = json!(true);
        assert_eq!(
            CapabilitySnapshot::from_json(&future_version.to_string()).unwrap_err(),
            CapabilityContractError::UnsupportedContractVersion { found: 2 }
        );

        let mut unknown_capability = serde_json::to_value(snapshot()).unwrap();
        unknown_capability["capabilities"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "id": "capability_from_the_future",
                "supported": true,
                "installed": true,
                "enabled": true,
                "available": {
                    "state": "ready",
                    "observed_at_ms": 1_000,
                    "validity_ms": 500
                }
            }));
        let parsed = CapabilitySnapshot::from_json(&unknown_capability.to_string()).unwrap();
        assert_eq!(parsed.capabilities.len(), CapabilityId::ALL.len());
        assert!(parsed
            .capabilities
            .iter()
            .all(|probe| CapabilityId::parse(probe.id.as_str()).is_some()));

        let mut unknown_reason = serde_json::to_value(snapshot()).unwrap();
        unknown_reason["capabilities"][1]["available"] = json!({
            "state": "unavailable",
            "observed_at_ms": 1_000,
            "validity_ms": 500,
            "reason": "reason_from_the_future"
        });
        assert_eq!(
            CapabilitySnapshot::from_json(&unknown_reason.to_string()).unwrap_err(),
            CapabilityContractError::UnknownReason("reason_from_the_future".into())
        );

        assert_eq!(
            CapabilitySnapshot::from_json("not json").unwrap_err(),
            CapabilityContractError::InvalidSnapshot("json")
        );
    }

    #[test]
    fn unavailable_states_require_known_reasons_and_ready_has_none() {
        let mut bad_ready = snapshot();
        bad_ready.capabilities[0].available.reason =
            Some(CapabilityReason(CapabilityReason::MODULE_DISABLED.into()));
        assert_eq!(
            bad_ready.validate().unwrap_err(),
            CapabilityContractError::InvalidAvailability("ready reason")
        );

        let mut missing_reason = snapshot();
        missing_reason.capabilities[1].available.state = CapabilityAvailabilityState::Unavailable;
        assert_eq!(
            missing_reason.validate().unwrap_err(),
            CapabilityContractError::InvalidAvailability("reason")
        );

        let mut unknown = snapshot();
        unknown.capabilities[1].available =
            availability(CapabilityAvailabilityState::Unknown, Some("surprise"));
        assert_eq!(
            unknown.validate().unwrap_err(),
            CapabilityContractError::UnknownReason("surprise".into())
        );
    }
}

//! RFC-0016 D19-A1 的临时能力契约样例：纯数据、选择器和确定性校验。
//!
//! 这里的能力 ID 是 RFC §4.1 的设计分组样例，不是已发布的产品能力；
//! 不得进入 CLI 帮助、MCP 工具表或 concepts.md。`authorized` 由 gld-core 按
//! D18 `AuthContext` 对每次请求计算，本模块只承载节点探测到的前四项状态。

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
            return Err(CapabilityContractError::EmptyIdentifier("hub_id"));
        }
        if self.machine_id.trim().is_empty() {
            return Err(CapabilityContractError::EmptyIdentifier("machine_id"));
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
        )
    }
}

/// `observed_at_ms` 是签发方单调时钟的毫秒值，不是墙上时间；消费者不得用
/// 自己机器的时钟去减它。新鲜度只由调用方显式传入的 `now_ms` 判定。
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilitySnapshot {
    pub contract_version: u32,
    pub node: NodeIdentity,
    pub module_generation: u64,
    pub capabilities: Vec<CapabilityProbe>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityContractError {
    UnsupportedContractVersion { found: u32 },
    EmptyIdentifier(&'static str),
    AmbiguousTarget,
    MissingTarget,
    UnknownCapability(String),
    UnknownReason(String),
    DuplicateCapability(CapabilityId),
    InvalidAvailability(&'static str),
    StaleSnapshot,
    OlderModuleGeneration { snapshot: u64, current: u64 },
    NewerModuleGeneration { snapshot: u64, current: u64 },
}

impl CapabilityContractError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedContractVersion { .. } => "CAPABILITY_PROTOCOL_UNSUPPORTED",
            Self::EmptyIdentifier(_) => "CAPABILITY_TARGET_INVALID",
            Self::AmbiguousTarget | Self::MissingTarget => "CAPABILITY_TARGET_INVALID",
            Self::UnknownCapability(_) => "CAPABILITY_UNKNOWN",
            Self::UnknownReason(_) => "CAPABILITY_REASON_UNKNOWN",
            Self::DuplicateCapability(_) => "CAPABILITY_SNAPSHOT_INVALID",
            Self::InvalidAvailability(_) => "CAPABILITY_SNAPSHOT_INVALID",
            Self::StaleSnapshot => "CAPABILITY_SNAPSHOT_STALE",
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
            Self::UnknownCapability(id) => write!(f, "unknown capability id: {id}"),
            Self::UnknownReason(reason) => write!(f, "unknown capability reason code: {reason}"),
            Self::DuplicateCapability(id) => write!(f, "duplicate capability id: {id}"),
            Self::InvalidAvailability(field) => write!(f, "invalid capability availability: {field}"),
            Self::StaleSnapshot => f.write_str("capability availability window has expired"),
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

impl CapabilitySnapshot {
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
            probe.available.validate()?;
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

    pub fn fresh_at(&self, now_ms: u64) -> Result<(), CapabilityContractError> {
        if now_ms < self.observed_at_ms {
            return Err(CapabilityContractError::StaleSnapshot);
        }
        if now_ms - self.observed_at_ms > self.validity_ms {
            return Err(CapabilityContractError::StaleSnapshot);
        }
        Ok(())
    }
}

/// 结构、版本、代次和有效期一次判完；`now_ms` 与 `current_generation` 必须由调用方显式给。
pub fn check_snapshot(
    snapshot: &CapabilitySnapshot,
    now_ms: u64,
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
    for probe in &snapshot.capabilities {
        probe.available.fresh_at(now_ms)?;
    }
    Ok(())
}

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
    fn stale_unknown_and_mixed_inputs_have_deterministic_errors() {
        let mut stale = snapshot();
        stale.capabilities[0].available.observed_at_ms = 1_000;
        stale.capabilities[0].available.validity_ms = 100;
        assert_eq!(
            check_snapshot(&stale, 1_101, 7).unwrap_err(),
            CapabilityContractError::StaleSnapshot
        );

        let mut old = snapshot();
        old.module_generation = 6;
        assert_eq!(
            check_snapshot(&old, 1_200, 7).unwrap_err(),
            CapabilityContractError::OlderModuleGeneration {
                snapshot: 6,
                current: 7
            }
        );
        let mut new = snapshot();
        new.module_generation = 8;
        assert_eq!(
            check_snapshot(&new, 1_200, 7).unwrap_err(),
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

        let mut unknown_reason = snapshot();
        unknown_reason.capabilities[1].available = availability(
            CapabilityAvailabilityState::Unavailable,
            Some("reason_from_the_future"),
        );
        assert_eq!(
            unknown_reason.validate().unwrap_err(),
            CapabilityContractError::UnknownReason("reason_from_the_future".into())
        );

        assert_eq!(CapabilityId::parse("not_a_capability"), None);
        assert_eq!(CapabilityId::FileCommand.as_str(), "file_command");
        assert_eq!(
            serde_json::to_value(CapabilityId::DesktopInput).unwrap(),
            json!("desktop_input")
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

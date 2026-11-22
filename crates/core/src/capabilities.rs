//! D19-A1 的调用方可见能力评估：协议只给探测样例，这里按 D18 主体裁剪。
//!
//! 这是契约样例，不接工具表、CLI 或守护进程。桌面 / 会话等可选能力在 D19-A4
//! 给出显式能力授权前一律不给网络主体；`file_command` 只跟随现有项目范围。

use gld_policy::AuthContext;
use gld_protocol::capabilities::{
    check_snapshot, CapabilityAvailability, CapabilityContractError, CapabilityId, CapabilityProbe,
    CapabilityReason, CapabilityRoute, CapabilitySelector, CapabilitySnapshot, NodeIdentity,
};

#[derive(Debug, Clone, PartialEq)]
pub struct CapabilityView {
    pub id: CapabilityId,
    pub supported: bool,
    pub installed: bool,
    pub enabled: bool,
    pub available: CapabilityAvailability,
    pub authorized: bool,
    pub reason: Option<CapabilityReason>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CapabilityEvaluation {
    pub node: NodeIdentity,
    pub module_generation: u64,
    pub selector: CapabilitySelector,
    pub capabilities: Vec<CapabilityView>,
}

impl CapabilityEvaluation {
    pub fn visible_ids(&self) -> Vec<CapabilityId> {
        self.capabilities
            .iter()
            .filter(|view| view.authorized)
            .map(|view| view.id)
            .collect()
    }
}

pub fn evaluate_capabilities(
    snapshot: &CapabilitySnapshot,
    auth: &AuthContext,
    selector: &CapabilitySelector,
    now_ms: u64,
    current_generation: u64,
) -> Result<CapabilityEvaluation, CapabilityContractError> {
    let route = selector.route()?;
    if let CapabilityRoute::Machine(machine) = &route {
        if machine != &snapshot.node.machine_id {
            return Err(CapabilityContractError::TargetMismatch);
        }
    }
    check_snapshot(snapshot, now_ms, current_generation)?;

    let capabilities = snapshot
        .capabilities
        .iter()
        .map(|probe| view_for(auth, probe, &route))
        .collect();
    Ok(CapabilityEvaluation {
        node: snapshot.node.clone(),
        module_generation: snapshot.module_generation,
        selector: selector.clone(),
        capabilities,
    })
}

fn view_for(
    auth: &AuthContext,
    probe: &CapabilityProbe,
    route: &CapabilityRoute,
) -> CapabilityView {
    let (authorized, reason) = authorize(auth, probe.id, route);
    CapabilityView {
        id: probe.id,
        supported: probe.supported,
        installed: probe.installed,
        enabled: probe.enabled,
        available: probe.available.clone(),
        authorized,
        reason,
    }
}

fn authorize(
    auth: &AuthContext,
    id: CapabilityId,
    route: &CapabilityRoute,
) -> (bool, Option<CapabilityReason>) {
    let target_allowed = match route {
        CapabilityRoute::Workspace(workspace) => auth.allows_project(workspace),
        CapabilityRoute::Machine(_) => auth.is_local_operator(),
    };
    if !target_allowed {
        let reason = match route {
            CapabilityRoute::Workspace(_) => CapabilityReason::PROJECT_NOT_ALLOWED,
            CapabilityRoute::Machine(_) => CapabilityReason::CALLER_NOT_AUTHORIZED,
        };
        return (false, Some(CapabilityReason(reason.into())));
    }
    if auth.is_local_operator() || matches!(id, CapabilityId::FileCommand) {
        return (true, None);
    }
    (
        false,
        Some(CapabilityReason(
            CapabilityReason::CAPABILITY_NOT_GRANTED.into(),
        )),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gld_policy::{
        CredentialKind, Grant, GrantPermissions, GrantStatus, McpSubject, ProjectScope,
        ServiceCapabilities,
    };
    use gld_protocol::capabilities::{CapabilityAvailabilityState, CAPABILITY_CONTRACT_VERSION};

    fn ready(id: CapabilityId) -> CapabilityProbe {
        CapabilityProbe {
            id,
            supported: true,
            installed: true,
            enabled: true,
            available: CapabilityAvailability {
                state: CapabilityAvailabilityState::Ready,
                observed_at_ms: 1_000,
                validity_ms: 500,
                reason: None,
            },
        }
    }

    fn snapshot() -> CapabilitySnapshot {
        CapabilitySnapshot {
            contract_version: CAPABILITY_CONTRACT_VERSION,
            node: NodeIdentity {
                hub_id: "hub-a".into(),
                machine_id: "machine-a".into(),
            },
            module_generation: 3,
            capabilities: [
                CapabilityId::FileCommand,
                CapabilityId::DesktopObserve,
                CapabilityId::Clipboard,
                CapabilityId::SessionProvider,
            ]
            .into_iter()
            .map(ready)
            .collect(),
        }
    }

    fn service() -> ServiceCapabilities {
        ServiceCapabilities {
            user_prompts: true,
            machine_mcp: true,
            managed_workspaces: true,
        }
    }

    fn grant(id: &str, scope: ProjectScope, service: ServiceCapabilities) -> AuthContext {
        AuthContext::verified_mcp(
            "hub",
            Grant {
                id: id.into(),
                name: id.into(),
                scope,
                permissions: GrantPermissions {
                    write: true,
                    service,
                },
                status: GrantStatus::Active,
            },
            CredentialKind::Token,
            McpSubject::Token,
        )
        .unwrap()
    }

    fn reason(view: &CapabilityView) -> &str {
        view.reason.as_ref().unwrap().0.as_str()
    }

    #[test]
    fn same_node_returns_different_visible_sets_for_different_subjects() {
        let snapshot = snapshot();
        let selector = CapabilitySelector::workspace("project");
        let local = evaluate_capabilities(
            &snapshot,
            &AuthContext::local_operator(),
            &selector,
            1_200,
            3,
        )
        .unwrap();
        assert_eq!(
            local.visible_ids(),
            vec![
                CapabilityId::FileCommand,
                CapabilityId::DesktopObserve,
                CapabilityId::Clipboard,
                CapabilityId::SessionProvider,
            ]
        );

        let selected = grant(
            "selected",
            ProjectScope::Selected(vec!["project".into()]),
            ServiceCapabilities::default(),
        );
        let selected_eval =
            evaluate_capabilities(&snapshot, &selected, &selector, 1_200, 3).unwrap();
        assert_eq!(selected_eval.visible_ids(), vec![CapabilityId::FileCommand]);
        let desktop = selected_eval
            .capabilities
            .iter()
            .find(|view| view.id == CapabilityId::DesktopObserve)
            .unwrap();
        assert!(desktop.supported && desktop.installed && desktop.enabled);
        assert_eq!(desktop.available.state, CapabilityAvailabilityState::Ready);
        assert!(!desktop.authorized);
        assert_eq!(reason(desktop), CapabilityReason::CAPABILITY_NOT_GRANTED);

        let other = grant(
            "other",
            ProjectScope::Selected(vec!["other".into()]),
            ServiceCapabilities::default(),
        );
        let other_eval = evaluate_capabilities(&snapshot, &other, &selector, 1_200, 3).unwrap();
        assert!(other_eval.visible_ids().is_empty());
        assert_eq!(
            reason(&other_eval.capabilities[0]),
            CapabilityReason::PROJECT_NOT_ALLOWED
        );

        let all_projects = grant("all", ProjectScope::AllProjects, service());
        let all_eval =
            evaluate_capabilities(&snapshot, &all_projects, &selector, 1_200, 3).unwrap();
        assert_eq!(all_eval.visible_ids(), vec![CapabilityId::FileCommand]);
        for id in [
            CapabilityId::DesktopObserve,
            CapabilityId::Clipboard,
            CapabilityId::SessionProvider,
        ] {
            let view = all_eval
                .capabilities
                .iter()
                .find(|view| view.id == id)
                .unwrap();
            assert!(
                !view.authorized,
                "{id} must not be authorized by project/service access"
            );
            assert_eq!(reason(view), CapabilityReason::CAPABILITY_NOT_GRANTED);
        }
    }

    #[test]
    fn machine_targets_are_local_only_and_must_match_the_snapshot_node() {
        let snapshot = snapshot();
        let selector = CapabilitySelector::machine("machine-a");
        let local = evaluate_capabilities(
            &snapshot,
            &AuthContext::local_operator(),
            &selector,
            1_200,
            3,
        )
        .unwrap();
        assert_eq!(local.visible_ids().len(), snapshot.capabilities.len());

        let network = grant(
            "network",
            ProjectScope::AllProjects,
            ServiceCapabilities::default(),
        );
        let denied = evaluate_capabilities(&snapshot, &network, &selector, 1_200, 3).unwrap();
        assert!(denied.visible_ids().is_empty());
        assert_eq!(
            reason(&denied.capabilities[0]),
            CapabilityReason::CALLER_NOT_AUTHORIZED
        );

        let wrong = CapabilitySelector::machine("machine-b");
        assert_eq!(
            evaluate_capabilities(&snapshot, &AuthContext::local_operator(), &wrong, 1_200, 3)
                .unwrap_err(),
            CapabilityContractError::TargetMismatch
        );
    }
}

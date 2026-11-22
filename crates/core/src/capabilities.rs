//! D19-A1 的调用方可见能力评估：协议只给探测样例，这里按 D18 主体裁剪。
//!
//! 这是契约样例，不接工具表、CLI 或守护进程。目标被拒绝时返回稳定错误且不带
//! 探测数据；目标允许但能力未授权时只回能力 ID、`authorized=false` 和原因，
//! 不回 supported / installed / enabled / available，避免 AllProjects 之类的
//! token 学到机器上装了什么。workspace 路由必须由调用方先解析成
//! hub/machine 绑定传进来，本模块不查 NativeMember。桌面 / 会话等可选能力在
//! D19-A4 给出显式能力授权前一律不给网络主体。`file_command` 只跟随现有项目
//! 范围且要求写权限：这里的 authorized 只是“能看到这个能力探测成立”，不等于
//! 允许写文件或执行命令，真正的命令执行仍走既有 grant / tool 策略。

use gld_policy::AuthContext;
use gld_protocol::capabilities::{
    check_snapshot, CapabilityAvailability, CapabilityAvailabilityState, CapabilityContractError,
    CapabilityId, CapabilityProbe, CapabilityQuery, CapabilityReason, CapabilityRoute,
    CapabilitySelector, CapabilitySnapshot, NodeIdentity,
};

#[derive(Debug, Clone, PartialEq)]
pub struct CapabilityProbeView {
    pub supported: bool,
    pub installed: bool,
    pub enabled: bool,
    pub available: CapabilityAvailability,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CapabilityView {
    pub id: CapabilityId,
    pub authorized: bool,
    pub reason: Option<CapabilityReason>,
    /// 探测字段只在调用方对该能力 authorized 时出现；未授权能力不泄露节点状态。
    pub probe: Option<CapabilityProbeView>,
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

/// 评估所运行的 Hub，以及 workspace -> 节点的已解析绑定。workspace 绑定由调用方
/// 从现有 NativeMember 路由解析后传入；machine 路由只需要 hub 归属。
#[derive(Debug, Clone, Copy)]
pub struct CapabilityTargetBinding<'a> {
    hub_id: &'a str,
    workspace_id: Option<&'a str>,
    workspace_node: Option<&'a NodeIdentity>,
}

impl<'a> CapabilityTargetBinding<'a> {
    pub fn hub(hub_id: &'a str) -> Self {
        Self {
            hub_id,
            workspace_id: None,
            workspace_node: None,
        }
    }

    pub fn workspace(mut self, workspace_id: &'a str, node: &'a NodeIdentity) -> Self {
        self.workspace_id = Some(workspace_id);
        self.workspace_node = Some(node);
        self
    }

    fn node_for_workspace(
        &self,
        workspace: &str,
    ) -> Result<&'a NodeIdentity, CapabilityContractError> {
        if self.workspace_id != Some(workspace) {
            return Err(CapabilityContractError::TargetMismatch);
        }
        self.workspace_node
            .ok_or(CapabilityContractError::TargetMismatch)
    }
}

pub fn evaluate_capabilities(
    snapshot: &CapabilitySnapshot,
    auth: &AuthContext,
    query: &CapabilityQuery,
    target: &CapabilityTargetBinding<'_>,
    age_ms: u64,
    current_generation: u64,
) -> Result<CapabilityEvaluation, CapabilityContractError> {
    query.validate()?;
    let route = query.selector.route()?;
    if !target_allowed(auth, &route) {
        return Err(CapabilityContractError::TargetDenied);
    }
    if snapshot.node.hub_id != target.hub_id {
        return Err(CapabilityContractError::TargetMismatch);
    }
    match &route {
        CapabilityRoute::Machine(machine) => {
            if machine != &snapshot.node.machine_id {
                return Err(CapabilityContractError::TargetMismatch);
            }
        }
        CapabilityRoute::Workspace(workspace) => {
            let node = target.node_for_workspace(workspace)?;
            if node != &snapshot.node {
                return Err(CapabilityContractError::TargetMismatch);
            }
        }
    }
    check_snapshot(snapshot, current_generation)?;

    let capabilities = snapshot
        .capabilities
        .iter()
        .filter(|probe| query.includes(probe.id))
        .map(|probe| view_for(auth, probe, age_ms))
        .collect();
    Ok(CapabilityEvaluation {
        node: snapshot.node.clone(),
        module_generation: snapshot.module_generation,
        selector: query.selector.clone(),
        capabilities,
    })
}

fn target_allowed(auth: &AuthContext, route: &CapabilityRoute) -> bool {
    match route {
        CapabilityRoute::Workspace(workspace) => auth.allows_project(workspace),
        CapabilityRoute::Machine(_) => auth.is_local_operator(),
    }
}

fn view_for(auth: &AuthContext, probe: &CapabilityProbe, age_ms: u64) -> CapabilityView {
    let (authorized, reason) = authorize(auth, probe.id);
    CapabilityView {
        id: probe.id,
        authorized,
        reason,
        probe: authorized.then(|| probe_view_for(probe, age_ms)),
    }
}

fn probe_view_for(probe: &CapabilityProbe, age_ms: u64) -> CapabilityProbeView {
    let available = if probe.available.fresh_within(age_ms) {
        probe.available.clone()
    } else {
        CapabilityAvailability {
            state: CapabilityAvailabilityState::Unknown,
            observed_at_ms: probe.available.observed_at_ms,
            validity_ms: probe.available.validity_ms,
            reason: Some(CapabilityReason(CapabilityReason::STALE_SNAPSHOT.into())),
        }
    };
    CapabilityProbeView {
        supported: probe.supported,
        installed: probe.installed,
        enabled: probe.enabled,
        available,
    }
}

fn authorize(auth: &AuthContext, id: CapabilityId) -> (bool, Option<CapabilityReason>) {
    if auth.is_local_operator() {
        return (true, None);
    }
    if matches!(id, CapabilityId::FileCommand) && auth.may_write_project() {
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
    use gld_protocol::capabilities::CAPABILITY_CONTRACT_VERSION;

    fn ready(id: CapabilityId, validity_ms: u64) -> CapabilityProbe {
        CapabilityProbe {
            id,
            supported: true,
            installed: true,
            enabled: true,
            available: CapabilityAvailability {
                state: CapabilityAvailabilityState::Ready,
                observed_at_ms: 1_000,
                validity_ms,
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
            .map(|id| ready(id, 500))
            .collect(),
        }
    }

    fn target_for(snapshot: &CapabilitySnapshot) -> CapabilityTargetBinding<'_> {
        CapabilityTargetBinding::hub("hub-a").workspace("project", &snapshot.node)
    }

    fn service() -> ServiceCapabilities {
        ServiceCapabilities {
            user_prompts: true,
            machine_mcp: true,
            managed_workspaces: true,
        }
    }

    fn grant(
        id: &str,
        scope: ProjectScope,
        service: ServiceCapabilities,
        write: bool,
    ) -> AuthContext {
        AuthContext::verified_mcp(
            "hub",
            Grant {
                id: id.into(),
                name: id.into(),
                scope,
                permissions: GrantPermissions { write, service },
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

    fn query(selector: &CapabilitySelector) -> CapabilityQuery {
        CapabilityQuery::all(selector.clone())
    }

    #[test]
    fn capability_queries_filter_ids_and_fail_closed_on_unknown() {
        let snapshot = snapshot();
        let selector = CapabilitySelector::workspace("project");
        let local = AuthContext::local_operator();
        let target = target_for(&snapshot);

        let all =
            evaluate_capabilities(&snapshot, &local, &query(&selector), &target, 200, 3).unwrap();
        assert_eq!(all.capabilities.len(), snapshot.capabilities.len());

        let only_clipboard = CapabilityQuery::only(
            selector.clone(),
            vec![CapabilityId::Clipboard.as_str().into()],
        );
        let filtered =
            evaluate_capabilities(&snapshot, &local, &only_clipboard, &target, 200, 3).unwrap();
        assert_eq!(filtered.capabilities.len(), 1);
        assert_eq!(filtered.capabilities[0].id, CapabilityId::Clipboard);
        assert_eq!(filtered.visible_ids(), vec![CapabilityId::Clipboard]);

        let unknown = CapabilityQuery::only(selector, vec!["capability_from_the_future".into()]);
        assert_eq!(
            evaluate_capabilities(&snapshot, &local, &unknown, &target, 200, 3).unwrap_err(),
            CapabilityContractError::UnknownCapability("capability_from_the_future".into())
        );
    }

    #[test]
    fn same_node_returns_different_visible_sets_for_different_subjects() {
        let snapshot = snapshot();
        let selector = CapabilitySelector::workspace("project");
        let target = target_for(&snapshot);
        let local = evaluate_capabilities(
            &snapshot,
            &AuthContext::local_operator(),
            &query(&selector),
            &target,
            200,
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
        assert!(local.capabilities.iter().all(|view| view.probe.is_some()));

        let selected = grant(
            "selected",
            ProjectScope::Selected(vec!["project".into()]),
            ServiceCapabilities::default(),
            true,
        );
        let selected_eval =
            evaluate_capabilities(&snapshot, &selected, &query(&selector), &target, 200, 3)
                .unwrap();
        assert_eq!(selected_eval.visible_ids(), vec![CapabilityId::FileCommand]);
        let desktop = selected_eval
            .capabilities
            .iter()
            .find(|view| view.id == CapabilityId::DesktopObserve)
            .unwrap();
        assert!(!desktop.authorized);
        assert_eq!(reason(desktop), CapabilityReason::CAPABILITY_NOT_GRANTED);
        assert!(desktop.probe.is_none());

        let other = grant(
            "other",
            ProjectScope::Selected(vec!["other".into()]),
            ServiceCapabilities::default(),
            true,
        );
        let denied = evaluate_capabilities(&snapshot, &other, &query(&selector), &target, 200, 3)
            .unwrap_err();
        assert_eq!(denied, CapabilityContractError::TargetDenied);
        assert_eq!(denied.code(), "CAPABILITY_TARGET_DENIED");

        let all_projects = grant("all", ProjectScope::AllProjects, service(), true);
        let all_eval =
            evaluate_capabilities(&snapshot, &all_projects, &query(&selector), &target, 200, 3)
                .unwrap();
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
            assert!(view.probe.is_none());
        }
    }

    #[test]
    fn file_command_visibility_requires_write_authority() {
        let snapshot = snapshot();
        let selector = CapabilitySelector::workspace("project");
        let target = target_for(&snapshot);
        let read_only = grant(
            "read-only",
            ProjectScope::Selected(vec!["project".into()]),
            ServiceCapabilities::default(),
            false,
        );
        let eval = evaluate_capabilities(&snapshot, &read_only, &query(&selector), &target, 200, 3)
            .unwrap();
        assert!(eval.visible_ids().is_empty());
        let file = eval
            .capabilities
            .iter()
            .find(|view| view.id == CapabilityId::FileCommand)
            .unwrap();
        assert!(!file.authorized);
        assert_eq!(reason(file), CapabilityReason::CAPABILITY_NOT_GRANTED);
        assert!(file.probe.is_none());
    }

    #[test]
    fn machine_targets_are_local_only_and_must_match_the_snapshot_node() {
        let snapshot = snapshot();
        let selector = CapabilitySelector::machine("machine-a");
        let target = CapabilityTargetBinding::hub("hub-a");
        let local = evaluate_capabilities(
            &snapshot,
            &AuthContext::local_operator(),
            &query(&selector),
            &target,
            200,
            3,
        )
        .unwrap();
        assert_eq!(local.visible_ids().len(), snapshot.capabilities.len());

        let network = grant(
            "network",
            ProjectScope::AllProjects,
            ServiceCapabilities::default(),
            true,
        );
        let denied = evaluate_capabilities(&snapshot, &network, &query(&selector), &target, 200, 3)
            .unwrap_err();
        assert_eq!(denied, CapabilityContractError::TargetDenied);

        let wrong = CapabilitySelector::machine("machine-b");
        assert_eq!(
            evaluate_capabilities(
                &snapshot,
                &AuthContext::local_operator(),
                &query(&wrong),
                &target,
                200,
                3
            )
            .unwrap_err(),
            CapabilityContractError::TargetMismatch
        );

        let wrong_hub = CapabilityTargetBinding::hub("hub-b");
        assert_eq!(
            evaluate_capabilities(
                &snapshot,
                &AuthContext::local_operator(),
                &query(&selector),
                &wrong_hub,
                200,
                3
            )
            .unwrap_err(),
            CapabilityContractError::TargetMismatch
        );
    }

    #[test]
    fn workspace_targets_require_the_resolved_node_binding() {
        let snapshot = snapshot();
        let selector = CapabilitySelector::workspace("project");
        let local = AuthContext::local_operator();

        let missing = CapabilityTargetBinding::hub("hub-a");
        assert_eq!(
            evaluate_capabilities(&snapshot, &local, &query(&selector), &missing, 200, 3)
                .unwrap_err(),
            CapabilityContractError::TargetMismatch
        );

        let wrong_workspace =
            CapabilityTargetBinding::hub("hub-a").workspace("other", &snapshot.node);
        assert_eq!(
            evaluate_capabilities(
                &snapshot,
                &local,
                &query(&selector),
                &wrong_workspace,
                200,
                3
            )
            .unwrap_err(),
            CapabilityContractError::TargetMismatch
        );

        let wrong_node = NodeIdentity {
            hub_id: "hub-a".into(),
            machine_id: "machine-b".into(),
        };
        let wrong_node = CapabilityTargetBinding::hub("hub-a").workspace("project", &wrong_node);
        assert_eq!(
            evaluate_capabilities(&snapshot, &local, &query(&selector), &wrong_node, 200, 3)
                .unwrap_err(),
            CapabilityContractError::TargetMismatch
        );

        let wrong_hub = CapabilityTargetBinding::hub("hub-b").workspace("project", &snapshot.node);
        assert_eq!(
            evaluate_capabilities(&snapshot, &local, &query(&selector), &wrong_hub, 200, 3)
                .unwrap_err(),
            CapabilityContractError::TargetMismatch
        );
    }

    #[test]
    fn stale_capabilities_fail_independently() {
        let snapshot = CapabilitySnapshot {
            capabilities: vec![
                ready(CapabilityId::FileCommand, 500),
                ready(CapabilityId::DesktopObserve, 100),
            ],
            ..snapshot()
        };
        assert!(check_snapshot(&snapshot, 3).is_ok());
        let eval = evaluate_capabilities(
            &snapshot,
            &AuthContext::local_operator(),
            &query(&CapabilitySelector::workspace("project")),
            &target_for(&snapshot),
            200,
            3,
        )
        .unwrap();
        let file = eval
            .capabilities
            .iter()
            .find(|view| view.id == CapabilityId::FileCommand)
            .unwrap();
        let desktop = eval
            .capabilities
            .iter()
            .find(|view| view.id == CapabilityId::DesktopObserve)
            .unwrap();
        assert_eq!(
            file.probe.as_ref().unwrap().available.state,
            CapabilityAvailabilityState::Ready
        );
        assert!(file.authorized);
        let desktop_available = &desktop.probe.as_ref().unwrap().available;
        assert_eq!(
            desktop_available.state,
            CapabilityAvailabilityState::Unknown
        );
        assert_eq!(
            desktop_available.reason.as_ref().unwrap().0.as_str(),
            CapabilityReason::STALE_SNAPSHOT
        );
        assert!(desktop.authorized);
    }
}

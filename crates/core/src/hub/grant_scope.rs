//! 创建时解析范围；运行时仍按当前成员及权限判断。

use crate::auth::ProjectScope;
use crate::error::{AppError, AppResult};

use super::{resolve_member, Hub, Member};

impl Hub {
    pub fn grant_scope(&self, projects: &[String], all: bool) -> AppResult<ProjectScope> {
        if all && !projects.is_empty() {
            return Err(AppError::Message(
                "INVALID_GRANT_SCOPE: --all 与指定项目不能同时使用。".into(),
            ));
        }
        // All 是动态范围；创建不依赖成员可用性，也不扫描或打开项目目录。
        if projects.is_empty() {
            return Ok(ProjectScope::AllProjects);
        }
        let (members, _) = self.snapshot().map_err(AppError::Message)?;
        selected_scope(&members, projects)
    }
}

fn selected_scope(members: &[Member], projects: &[String]) -> AppResult<ProjectScope> {
    let mut ids = Vec::new();
    for selector in projects {
        let member = resolve_member(members, selector.trim()).map_err(|error| {
            let code = error["error"]["code"]
                .as_str()
                .unwrap_or("INVALID_GRANT_SCOPE");
            let message = error["error"]["message"]
                .as_str()
                .unwrap_or("无法解析指定项目。");
            AppError::Message(format!("{code}: {message}"))
        })?;
        if let Member::Local(profile) = member {
            if !profile.runtime.confine_reads {
                return Err(AppError::Message(format!(
                    "GRANT_NEEDS_CONFINED_READS: 项目 {} 必须先设置 confine-reads=true。",
                    profile.name
                )));
            }
        }
        if !ids.iter().any(|id| id == member.id()) {
            ids.push(member.id().to_string());
        }
    }
    let scope = ProjectScope::Selected(ids);
    scope
        .validate()
        .map_err(|error| AppError::Message(error.to_string()))?;
    Ok(scope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::member::{CcnmMember, Mode};
    use crate::nodes::NativeMember;
    use crate::settings::{AppSettings, HubConfig};
    use crate::workspace::WorkspaceProfile;

    fn profile(id: &str, name: &str) -> WorkspaceProfile {
        let mut profile = WorkspaceProfile::new(
            format!("/nonexistent-grant-scope/{name}"),
            Some(name.into()),
        );
        profile.id = id.into();
        profile.runtime.confine_reads = true;
        profile
    }

    fn hub(profiles: Vec<WorkspaceProfile>) -> Hub {
        let mut settings = AppSettings::default();
        settings.hub.members = profiles.iter().map(|profile| profile.id.clone()).collect();
        Hub::fixed(
            &HubConfig::default(),
            super::super::Fixed {
                profiles,
                remotes: Vec::new(),
                settings,
            },
        )
    }

    fn selectors(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).into()).collect()
    }

    #[test]
    fn all_does_not_read_even_a_broken_member_snapshot() {
        let hub = hub(Vec::new());
        assert_eq!(
            hub.grant_scope(&[], false).unwrap(),
            ProjectScope::AllProjects
        );
        let super::super::Members::Fixed(fixed) = &hub.members else {
            unreachable!()
        };
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = fixed.lock().unwrap();
            panic!("poison the fixture snapshot");
        }));
        assert!(fixed.is_poisoned());
        assert_eq!(
            hub.grant_scope(&[], true).unwrap(),
            ProjectScope::AllProjects
        );
        assert_eq!(
            hub.grant_scope(&[], false).unwrap(),
            ProjectScope::AllProjects
        );
        assert!(hub.grant_scope(&selectors(&["api"]), true).is_err());
    }

    #[test]
    fn selected_resolves_names_ids_and_prefixes_and_deduplicates() {
        let hub = hub(vec![profile("abcd1111", "API"), profile("efgh2222", "Web")]);
        assert_eq!(
            hub.grant_scope(&selectors(&["api", "abcd1111", "abcd", " Web "]), false)
                .unwrap(),
            ProjectScope::Selected(vec!["abcd1111".into(), "efgh2222".into()])
        );
    }

    #[test]
    fn selected_preserves_resolution_errors_and_rejects_unconfined_projects() {
        let mut unconfined = profile("unsafe-id", "unsafe");
        unconfined.runtime.confine_reads = false;
        let hub = hub(vec![
            profile("abcd1111", "API"),
            profile("abcd2222", "API"),
            unconfined,
        ]);
        for selector in ["API", "abcd"] {
            assert!(hub
                .grant_scope(&selectors(&[selector]), false)
                .unwrap_err()
                .to_string()
                .contains("WORKSPACE_AMBIGUOUS"));
        }
        for selector in ["missing", "abc", "", "/nonexistent-grant-scope/API"] {
            assert!(hub
                .grant_scope(&selectors(&[selector]), false)
                .unwrap_err()
                .to_string()
                .contains("WORKSPACE_NOT_IN_HUB"));
        }
        assert!(hub
            .grant_scope(&selectors(&["unsafe"]), false)
            .unwrap_err()
            .to_string()
            .contains("GRANT_NEEDS_CONFINED_READS"));
        assert_eq!(
            hub.grant_scope(&[], false).unwrap(),
            ProjectScope::AllProjects
        );
    }

    #[test]
    fn selected_keeps_original_id_when_a_name_is_reused() {
        let old = hub(vec![profile("old-project", "API")])
            .grant_scope(&selectors(&["API"]), false)
            .unwrap();
        let new = hub(vec![profile("new-project", "API")])
            .grant_scope(&selectors(&["API"]), false)
            .unwrap();
        assert!(old.allows("old-project"));
        assert!(!old.allows("new-project"));
        assert!(new.allows("new-project"));
    }

    #[test]
    fn selected_uses_runtime_priority_and_excludes_registered_nonmembers() {
        let hub = hub(vec![
            profile("exact-id", "API"),
            profile("other-id", "exact-id"),
            profile("outside-id", "outside"),
        ]);
        let super::super::Members::Fixed(fixed) = &hub.members else {
            unreachable!()
        };
        fixed
            .lock()
            .unwrap()
            .settings
            .hub
            .members
            .retain(|id| id != "outside-id");
        assert_eq!(
            hub.grant_scope(&selectors(&["exact-id"]), false).unwrap(),
            ProjectScope::Selected(vec!["exact-id".into()])
        );
        for selector in ["outside", "outside-id"] {
            assert!(hub
                .grant_scope(&selectors(&[selector]), false)
                .unwrap_err()
                .to_string()
                .contains("WORKSPACE_NOT_IN_HUB"));
        }
    }

    #[test]
    fn selected_records_remote_and_native_ids_without_opening_connections() {
        let members = vec![
            Member::Remote(CcnmMember {
                id: "remote-stable".into(),
                name: "remote".into(),
                ccnm_bin: "ccnm".into(),
                node: "test-node".into(),
                workspace: "test-workspace".into(),
                max_mode: Mode::Read,
            }),
            Member::Native(NativeMember {
                id: "native-stable".into(),
                name: "native".into(),
                machine_id: "test-machine".into(),
                remote_workspace_id: "test-project".into(),
                online: false,
                revoked: false,
                tool_profile: "read-only".into(),
            }),
        ];
        assert_eq!(
            selected_scope(&members, &selectors(&["remote", "native"])).unwrap(),
            ProjectScope::Selected(vec!["remote-stable".into(), "native-stable".into()])
        );
    }
}

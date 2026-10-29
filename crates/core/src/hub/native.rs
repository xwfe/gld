//! 节点模式只复用项目分发，不启动第二个 Hub 监听器。

use super::*;

impl Hub {
    pub fn for_node(shared_ids: Vec<String>) -> Result<Self, String> {
        if shared_ids.is_empty() {
            return Err("节点必须明确指定至少一个本机项目 ID。".into());
        }
        let hub = Self::with_members(
            &HubConfig {
                tool_profile: "read-only".into(),
                ..HubConfig::default()
            },
            Members::Node(shared_ids.clone()),
            Connections::new(),
        );
        let (members, _) = hub.snapshot()?;
        if members.len() != shared_ids.len()
            || members.iter().any(|member| match member {
                Member::Local(profile) => !profile.runtime.confine_reads,
                _ => true,
            })
        {
            return Err("节点共享项目必须是已登记的本机项目，且全部开启 confine-reads。".into());
        }
        Ok(hub)
    }

    pub fn nodes(&self) -> Arc<Nodes> {
        self.nodes.clone()
    }

    pub(super) fn node_snapshot(
        &self,
        shared_ids: &[String],
    ) -> Result<(Vec<WorkspaceProfile>, Vec<CcnmMember>, AppSettings), String> {
        DataStore::read_file(|data| {
            let mut settings = AppSettings::from_data(data);
            settings.hub.members = shared_ids.to_vec();
            settings.hub.work_sessions = false;
            settings.relayed_mcp_servers.clear();
            settings.global_ai_instructions.clear();
            settings.global_custom_instruction_paths.clear();
            settings.global_custom_skill_paths.clear();
            settings.global_instruction_sources = vec!["custom".into()];
            settings.global_skill_sources = vec!["custom".into()];
            Ok((data.profiles.clone(), Vec::new(), settings))
        })
        .map_err(|error| error.to_string())
    }
}

/// 不自动读取节点账号的全局说明/Skills。项目根说明沿用受限且有界的 read_file。
pub(super) fn project_instructions(context: &SharedToolContext) -> String {
    let mut parts = vec![context.ai_instructions.clone()];
    let primary = if context
        .workspace
        .resolve_read_path("AGENTS.override.md")
        .is_ok()
    {
        "AGENTS.override.md"
    } else {
        "AGENTS.md"
    };
    for path in [primary, "CLAUDE.md", "GEMINI.md"] {
        if let Ok(result) = crate::tools::file::read_file(
            &context.workspace,
            &json!({"path":path,"max_bytes":16384}),
        ) {
            if let Some(content) = result["content"].as_str() {
                parts.push(format!("## {path}\n{content}"));
                if result["truncated"] == true {
                    parts.push(format!(
                        "{path} was truncated; use read_file to continue before editing."
                    ));
                }
            }
        }
    }
    parts
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Principal;

    fn native() -> Member {
        Member::Native(NativeMember {
            id: "n_example".into(),
            name: "machine/api".into(),
            machine_id: "machine".into(),
            remote_workspace_id: "project".into(),
            online: false,
            tool_profile: "read-only".into(),
        })
    }

    #[test]
    fn native_members_require_full_auth_even_if_a_grant_names_their_id() {
        assert!(visible_for(vec![native()], &AuthContext::anonymous("hub")).is_empty());
        let full = AuthContext::new(Principal::SharedSecret, "hub");
        assert_eq!(visible_for(vec![native()], &full).len(), 1);
        let grant = Grant {
            id: "grant".into(),
            name: "grant".into(),
            workspaces: vec!["n_example".into()],
            read_only: false,
            browser: false,
        };
        assert!(visible_for(vec![native()], &full.with_grant(Some(grant))).is_empty());
    }

    #[test]
    fn node_context_reads_only_bounded_project_instructions() {
        let root = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("AGENTS.md"), "old instruction").unwrap();
        std::fs::write(
            root.path().join("AGENTS.override.md"),
            "selected project instruction",
        )
        .unwrap();
        std::fs::write(root.path().join("CLAUDE.md"), "x".repeat(20000)).unwrap();
        let context = Arc::new(
            crate::tools::ToolContext::for_test(
                root.path().to_path_buf(),
                state.path().to_path_buf(),
            )
            .unwrap(),
        );
        let instructions = project_instructions(&context);
        assert!(instructions.contains("selected project instruction"));
        assert!(!instructions.contains("old instruction"));
        assert!(instructions.contains("was truncated"));
        assert!(instructions.len() < 18000);
    }

    #[cfg(unix)]
    #[test]
    fn node_context_never_follows_instructions_outside_the_project() {
        let root = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(state.path().join("private"), "outside-secret-marker").unwrap();
        std::os::unix::fs::symlink(state.path().join("private"), root.path().join("AGENTS.md"))
            .unwrap();
        let context = Arc::new(
            crate::tools::ToolContext::for_test(
                root.path().to_path_buf(),
                state.path().to_path_buf(),
            )
            .unwrap(),
        );
        assert!(!project_instructions(&context).contains("outside-secret-marker"));
    }
}

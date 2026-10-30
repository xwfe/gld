//! 节点模式只复用项目分发，不启动第二个 Hub 监听器。

use super::*;

pub(super) use crate::nodes::config::runs_commands;

#[cfg(test)]
#[path = "native_file_tests.rs"]
mod file_tests;

#[cfg(all(test, unix))]
#[path = "native_command_tests.rs"]
mod command_tests;

impl Hub {
    pub fn for_node(access: NodeAccess) -> Result<Self, String> {
        access.validate()?;
        let shared_count = access.shared.len();
        let hub = Self::with_members(
            &HubConfig {
                // 从完整定义取 schema，再用节点上限筛选，保留原只读工具而不开放命令。
                tool_profile: "advanced".into(),
                ..HubConfig::default()
            },
            Members::Node(access),
            Connections::new(),
        );
        let (members, _) = hub.snapshot()?;
        if members.len() != shared_count
            || members.iter().any(|member| match member {
                Member::Local(profile) => !profile.runtime.confine_reads,
                _ => true,
            })
        {
            return Err("节点共享项目必须是已登记的本机项目，且全部开启 confine-reads。".into());
        }
        Ok(hub)
    }

    /// 只给节点接收层的测试用：跳过本机项目登记检查，不读数据目录。
    #[cfg(test)]
    pub(crate) fn for_node_tests(shared_ids: Vec<String>) -> Self {
        Self::with_members(
            &HubConfig {
                tool_profile: "advanced".into(),
                ..HubConfig::default()
            },
            Members::Node(NodeAccess {
                shared: shared_ids,
                ..NodeAccess::default()
            }),
            Connections::new(),
        )
    }

    pub fn nodes(&self) -> Arc<Nodes> {
        self.nodes.clone()
    }

    /// 节点复用真实的只读工具表，再单独允许文件补丁和跑命令；策略分类表还含会执行命令的检查。
    /// 这里只按工具名判断整台节点开不开，具体项目在 `call_local` 里再判一次。
    pub(super) fn node_tool_allowed(&self, name: &str) -> bool {
        let Members::Node(access) = &self.members else {
            return true;
        };
        matches!(name, LIST_WORKSPACES | WORKSPACE_CONTEXT)
            || read_only_allows(name)
            || (!access.writable.is_empty() && matches!(name, "apply_patch" | "patch_check"))
            || (!access.runnable.is_empty() && runs_commands(name))
    }

    fn node_project_read_only(&self, member: &WorkspaceProfile) -> bool {
        let Members::Node(access) = &self.members else {
            return true;
        };
        !access.writable.contains(&member.id)
            || !member.runtime.confine_reads
            || !exposed_tool_names(&member.runtime.tool_profile).contains(&"apply_patch")
    }

    /// 不是节点时恒为真：本机服务的命令权限走项目工具档和 grant，不走这里。
    pub(super) fn node_project_runs_commands(&self, member: &WorkspaceProfile) -> bool {
        let Members::Node(access) = &self.members else {
            return true;
        };
        access.runnable.contains(&member.id)
            && !self.node_project_read_only(member)
            && exposed_tool_names(&member.runtime.tool_profile).contains(&"exec_command")
    }

    fn node_profile(&self, member: &WorkspaceProfile) -> &'static str {
        if self.node_project_read_only(member) {
            "read-only"
        } else if self.node_project_runs_commands(member) {
            "commands"
        } else {
            "file-write"
        }
    }

    pub(super) fn node_catalog(&self, members: &[Member]) -> Value {
        let mut listed = list_workspaces(members);
        if matches!(self.members, Members::Node(_)) {
            if let Some(items) = listed["workspaces"].as_array_mut() {
                for (member, item) in members.iter().zip(items) {
                    if let Member::Local(member) = member {
                        // 发现调用没有 workspace，不能拿它的只读 grant 代替目标机实际权限。
                        item["read_only"] = json!(self.node_project_read_only(member));
                        item["commands"] = json!(self.node_project_runs_commands(member));
                        item["tool_profile"] = json!(self.node_profile(member));
                    }
                }
            }
        }
        listed
    }

    /// 项目能力展示与硬发调用共用同一个上限，避免借写授权宣告命令或管理工具。
    pub(super) fn restrict_node_surface(
        &self,
        info: &mut Value,
        member: &WorkspaceProfile,
        grant: Option<&Grant>,
    ) {
        if !matches!(self.members, Members::Node(_)) {
            return;
        }
        let Some(tools) = info.get_mut("tools").and_then(Value::as_array_mut) else {
            return;
        };
        let runs = self.node_project_runs_commands(member);
        let shown = |name: &str| self.exposes(name, grant) && (runs || !runs_commands(name));
        tools.retain(|name| name.as_str().is_some_and(shown));
        let read_only = !tools.iter().any(|name| name == "apply_patch");
        let count = tools.len();
        if info.get("tool_count").is_some() {
            info["tool_count"] = json!(count);
        }
        info["read_only"] = json!(read_only);
        info["commands"] = json!(runs);
        info["tool_profile"] = json!(self.node_profile(member));
        if let Some(aggregate) = info
            .get_mut("tool_api")
            .and_then(|api| api.get_mut("aggregate_tools"))
            .and_then(Value::as_array_mut)
        {
            aggregate.retain(|name| name.as_str().is_some_and(shown));
        }
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

pub(super) fn commands_not_allowed(tool: &str, member: &WorkspaceProfile) -> Value {
    tool_err(WorkspaceError::ToolDetails {
        code: "NODE_COMMANDS_NOT_ALLOWED",
        message: format!(
            "{tool} is not allowed in {}: the operator of this machine has not enabled commands for this project. Nothing ran.",
            member.name
        ),
        category: "permission",
        retryable: false,
        details: json!({ "workspace": local_ref(member) }),
    })
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
            revoked: false,
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

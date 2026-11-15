//! 工作区执行资源的应用入口：选择数据目录并注入项目标识。
//!
//! 锁、会话和交接状态由 gld-runtime 持有；这里与 Harness 共用项目 ID 算法。

use std::path::Path;
use std::sync::Arc;

pub use gld_runtime::workspace_runtime::{
    any_unsettled_output, running_session_counts, terminate_all_sessions_everywhere,
    terminate_grant_sessions, write_lock_busy, CommitGuard, RunningCommand, RunningCommands,
    WorkspaceHandoff, WorkspaceRuntime, WRITE_LOCK_WAIT,
};
use gld_runtime::workspace_runtime::{RunStorage, RuntimePaths};

pub fn runtime_for(root: &Path) -> Arc<WorkspaceRuntime> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let paths = crate::home::data_home()
        .ok()
        .map(|home| RuntimePaths {
            write_lock_dir: Some(home.join("write-locks")),
            runs: Some(RunStorage {
                root: home.join("runs"),
                project_id: crate::tools::runs::project_id(&root),
            }),
        })
        .unwrap_or_default();
    gld_runtime::workspace_runtime::runtime_for(&root, paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gld_policy::{
        AuthContext, CallerId, CredentialKind, Grant, GrantPermissions, GrantStatus, McpSubject,
        ProjectScope,
    };
    use tempfile::tempdir;

    /// hub 的场景：同一个目录经不同入口进来是不同的 `ToolContext`（配置指纹
    /// 一变 `context_for` 就重建一个），但写操作必须排同一个队。
    #[test]
    fn two_contexts_on_one_directory_share_the_write_lock() {
        use crate::tools::context::ToolContext;

        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let first =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("first");
        let second =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("second");

        assert!(
            Arc::ptr_eq(&first.runtime, &second.runtime),
            "同一个目录的两个上下文各拿一把锁，两个入口就能同时往里写"
        );
    }

    /// hub 的场景：同一个目录上两个上下文，同一个主体看到的是同一张表。
    /// 这是 `context_for` 因为配置指纹变了重建上下文时，正在跑的命令不丢的
    /// 依据。
    #[test]
    fn two_contexts_on_one_directory_share_one_callers_sessions() {
        use crate::tools::context::ToolContext;

        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let first =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("first");
        let second =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("second");
        let who = caller("alpha");

        assert!(
            Arc::ptr_eq(
                &first.runtime.sessions_for(&who),
                &second.runtime.sessions_for(&who)
            ),
            "换了上下文就换了会话表——旧上下文里起的命令，客户端再也读不到了"
        );
    }

    #[test]
    fn application_and_executor_share_the_canonical_directory_registry() {
        let dir = tempdir().expect("workspace");
        let canonical = dir.path().canonicalize().expect("canonical");
        let from_application = runtime_for(&canonical);
        let from_executor = gld_runtime::workspace_runtime::runtime_for(
            &dir.path().join("."),
            RuntimePaths::default(),
        );
        assert!(Arc::ptr_eq(&from_application, &from_executor));
        let caller = caller("alpha");
        assert!(Arc::ptr_eq(
            &from_application.sessions_for(&caller),
            &from_executor.sessions_for(&caller),
        ));
    }

    fn caller(client_id: &str) -> CallerId {
        AuthContext::verified_mcp(
            "hub",
            Grant {
                id: "grant-a".into(),
                name: "test".into(),
                scope: ProjectScope::AllProjects,
                permissions: GrantPermissions::default(),
                status: GrantStatus::Active,
            },
            CredentialKind::OAuth,
            McpSubject::OAuth {
                client_id: client_id.into(),
            },
        )
        .expect("verified test identity")
        .id()
        .clone()
    }
}

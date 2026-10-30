use super::*;

use crate::auth::Principal;
use crate::nodes::identity::{auth_from_meta, forwarded_meta};
use crate::planning::{PlanningMode, PlanningService};

const HUB_ID: &str = "11111111111111111111111111111111";

#[test]
fn node_file_ceiling_rejects_execution_and_management_even_with_full_auth() {
    let id = "a".repeat(32);
    let hub = Hub::with_members(
        &HubConfig {
            tool_profile: "advanced".into(),
            ..HubConfig::default()
        },
        Members::Node {
            shared: vec![id.clone()],
            writable: vec![id.clone()],
        },
        Connections::new(),
    );
    let auth = AuthContext::new(Principal::SharedSecret, "node-test");
    for name in [
        "exec_command",
        "exec_health_check",
        "write_stdin",
        "kill_session",
        "agent_manage",
        "history_manage",
        "planning_manage",
        "task_manage",
        "work_session",
        "list_mcp_tools",
        "call_mcp_tool",
        "read_mcp_result",
        "remote_read_file",
        "remote_coding_begin",
        "set_default_cwd",
        "get_default_cwd",
    ] {
        let result = hub
            .handle_request(
                &auth,
                &json!({"jsonrpc":"2.0","id":1,
            "method":"tools/call","params":{"name":name,"arguments":{"workspace":id}}}),
            )
            .0;
        assert_eq!(
            result["error"]["data"]["reason"], "unknown_tool",
            "{name}: {result}"
        );
    }
    assert!(hub.exposes("apply_patch", None));
    assert!(hub.exposes("patch_check", None));
    assert!(
        hub.exposes("planning_state", None),
        "retain the pre-existing read surface"
    );
}

#[test]
fn node_file_surfaces_and_local_policy_remain_in_the_same_scope() {
    const CHILD: &str = "GLD_TEST_NODE_FILE_POLICY_CHILD";
    if std::env::var_os(CHILD).is_some() {
        exercise_local_policy();
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "hub::native::file_tests::node_file_surfaces_and_local_policy_remain_in_the_same_scope",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env("GLD_HOME", home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
}

fn exercise_local_policy() {
    let root = tempfile::tempdir().unwrap();
    let api_path = root.path().join("api");
    let web_path = root.path().join("web");
    std::fs::create_dir_all(&api_path).unwrap();
    std::fs::create_dir_all(&web_path).unwrap();
    std::fs::write(api_path.join("README.md"), "original\n").unwrap();
    let mut api =
        WorkspaceProfile::new(api_path.to_string_lossy().into_owned(), Some("api".into()));
    let web = WorkspaceProfile::new(web_path.to_string_lossy().into_owned(), Some("web".into()));
    api.runtime.tool_profile = "advanced".into();
    DataStore::update_file(|data| {
        data.profiles = vec![api.clone(), web.clone()];
        Ok(())
    })
    .unwrap();
    let shared = vec![api.id.clone(), web.id.clone()];
    let writable = vec![api.id.clone()];
    let hub = Hub::for_node(shared.clone(), writable.clone()).unwrap();
    let origin = AuthContext::new(
        Principal::OAuthClient {
            client_id: "alice".into(),
        },
        "hub",
    );
    let call = |name: &str, args: Value| {
        let params =
            json!({"name":name,"arguments":args,"_meta":forwarded_meta(&origin,HUB_ID).unwrap()});
        let auth = auth_from_meta(&params, HUB_ID, &shared, &writable).unwrap();
        hub.handle_request(
            &auth,
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":params}),
        )
        .0
    };
    let inventory = call("list_workspaces", json!({}));
    let projects = inventory["result"]["structuredContent"]["workspaces"]
        .as_array()
        .unwrap();
    assert_eq!(
        projects.iter().find(|p| p["id"] == api.id).unwrap()["read_only"],
        false
    );
    assert_eq!(
        projects.iter().find(|p| p["id"] == web.id).unwrap()["read_only"],
        true
    );
    let control = origin.clone().with_grant(Some(Grant {
        id: "node-control".into(),
        name: "inventory".into(),
        workspaces: shared.clone(),
        read_only: false,
        browser: false,
    }));
    let catalog = hub
        .handle_request(
            &control,
            &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        )
        .0;
    let tools = catalog["result"]["tools"].as_array().unwrap();
    let patch = tools
        .iter()
        .find(|tool| tool["name"] == "apply_patch")
        .unwrap();
    assert_eq!(patch["annotations"]["readOnlyHint"], false);
    assert!(tools.iter().any(|tool| tool["name"] == "planning_state"));
    assert!(tools.iter().all(|tool| ![
        "exec_command",
        "exec_health_check",
        "agent_manage",
        "task_manage"
    ]
    .contains(&tool["name"].as_str().unwrap())));
    for name in ["workspace_context", "server_info"] {
        let result = call(name, json!({"workspace":api.id}));
        let info = &result["result"]["structuredContent"];
        assert_eq!(info["ok"], true, "{result}");
        assert_eq!(info["tool_profile"], "file-write");
        assert_eq!(info["read_only"], false);
        let tools = info["tools"].as_array().unwrap();
        assert!(tools.contains(&json!("apply_patch")) && tools.contains(&json!("patch_check")));
        assert!(tools.iter().all(|tool| ![
            "exec_command",
            "exec_health_check",
            "agent_manage",
            "task_manage",
            "planning_manage",
            "history_manage"
        ]
        .contains(&tool.as_str().unwrap())));
        if name == "server_info" {
            assert_eq!(info["tool_count"], tools.len());
            assert_eq!(info["tool_api"]["aggregate_tools"], json!([]));
        }
    }
    let reader = call("workspace_context", json!({"workspace":web.id}));
    assert_eq!(reader["result"]["structuredContent"]["read_only"], true);
    assert!(!reader["result"]["structuredContent"]["tools"]
        .as_array()
        .unwrap()
        .contains(&json!("apply_patch")));
    let patch = json!({"workspace":api.id,"patch":"*** Begin Patch\n*** Update File: README.md\n@@\n-original\n+changed\n*** End Patch"});
    let planning = PlanningService::new(&api_path);
    planning.set_mode(PlanningMode::Plan).unwrap();
    let refused = call("apply_patch", patch.clone());
    assert_eq!(
        refused["result"]["structuredContent"]["error"]["code"],
        "PLAN_MODE_READ_ONLY"
    );
    planning.set_mode(PlanningMode::Direct).unwrap();
    DataStore::update_file(|data| {
        data.profiles[0].runtime.tool_profile = "read-only".into();
        Ok(())
    })
    .unwrap();
    let refused = call("apply_patch", patch.clone());
    assert_eq!(refused["result"]["structuredContent"]["ok"], false);
    let restricted = call("workspace_context", json!({"workspace":api.id}));
    assert_eq!(restricted["result"]["structuredContent"]["read_only"], true);
    DataStore::update_file(|data| {
        data.profiles[0].runtime.tool_profile = "advanced".into();
        data.profiles[0].runtime.confine_reads = false;
        Ok(())
    })
    .unwrap();
    let refused = call("apply_patch", patch);
    assert_eq!(
        refused["result"]["structuredContent"]["error"]["code"],
        "GRANT_NEEDS_CONFINED_READS"
    );
    assert_eq!(
        std::fs::read_to_string(api_path.join("README.md")).unwrap(),
        "original\n"
    );
}

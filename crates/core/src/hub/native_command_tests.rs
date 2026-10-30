use super::*;

use crate::auth::Principal;
use crate::nodes::identity::{auth_from_meta, forwarded_meta};

const HUB_ID: &str = "11111111111111111111111111111111";

#[test]
fn node_commands_stay_in_the_allowed_project_and_with_the_caller_who_started_them() {
    const CHILD: &str = "GLD_TEST_NODE_COMMANDS_CHILD";
    if std::env::var_os(CHILD).is_some() {
        exercise_commands();
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "hub::native::command_tests::node_commands_stay_in_the_allowed_project_and_with_the_caller_who_started_them",
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

fn exercise_commands() {
    let root = tempfile::tempdir().unwrap();
    let project = |name: &str| {
        let path = root.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        let mut profile =
            WorkspaceProfile::new(path.to_string_lossy().into_owned(), Some(name.into()));
        profile.runtime.tool_profile = "advanced".into();
        // 这里验权限和主体隔离，不验白名单；白名单在真实链路测试里验。
        profile.runtime.command_policy = "all".into();
        profile
    };
    let (api, web, docs) = (project("api"), project("web"), project("docs"));
    DataStore::update_file(|data| {
        data.profiles = vec![api.clone(), web.clone(), docs.clone()];
        Ok(())
    })
    .unwrap();
    let access = NodeAccess {
        shared: vec![api.id.clone(), web.id.clone(), docs.id.clone()],
        writable: vec![api.id.clone(), web.id.clone()],
        runnable: vec![api.id.clone()],
    };
    let hub = Hub::for_node(access.clone()).unwrap();
    let call_as = |who: &str, name: &str, args: Value| {
        let origin = AuthContext::new(
            Principal::OAuthClient {
                client_id: who.into(),
            },
            "hub",
        );
        let params =
            json!({"name":name,"arguments":args,"_meta":forwarded_meta(&origin,HUB_ID).unwrap()});
        let auth = auth_from_meta(&params, HUB_ID, &access.shared, &access.writable).unwrap();
        hub.handle_request(
            &auth,
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":params}),
        )
        .0
    };
    let structured = |reply: Value| reply["result"]["structuredContent"].clone();

    // 只写的项目和只读的项目都不能跑，连名字带前缀也绕不过去。
    let refused = structured(call_as(
        "alice",
        "exec_command",
        json!({"workspace":web.id,"cmd":"pwd"}),
    ));
    assert_eq!(refused["error"]["code"], "NODE_COMMANDS_NOT_ALLOWED");
    let reader = call_as(
        "alice",
        "exec_command",
        json!({"workspace":docs.id,"cmd":"pwd"}),
    );
    assert_eq!(
        reader["error"]["data"]["reason"], "unknown_tool",
        "{reader}"
    );
    for selector in ["api", &api.id[..8]] {
        let by_name = call_as(
            "alice",
            "exec_command",
            json!({"workspace":selector,"cmd":"pwd"}),
        );
        assert_eq!(
            by_name["error"]["data"]["reason"], "unknown_tool",
            "only the exact ID the Hub sends carries the permission: {by_name}"
        );
    }

    // 项目自己看得到的工具表跟实际一致。
    let tools_of = |id: &str| {
        let context = structured(call_as(
            "alice",
            "workspace_context",
            json!({"workspace":id}),
        ));
        assert_eq!(context["ok"], true, "{context}");
        context
    };
    let context = tools_of(&api.id);
    assert_eq!(context["tool_profile"], "commands");
    assert_eq!(context["commands"], true);
    assert!(context["tools"]
        .as_array()
        .unwrap()
        .contains(&json!("exec_command")));
    for id in [&web.id, &docs.id] {
        let context = tools_of(id);
        assert_eq!(context["commands"], false);
        assert!(!context["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool == "exec_command" || tool == "kill_session"));
    }

    // alice 的命令，bob 读不到、停不了、列不出，连“存在”都不知道。
    let started = structured(call_as(
        "alice",
        "exec_command",
        json!({"workspace":api.id,"cmd":"sleep 60","yield_time_ms":0,"timeout_ms":60_000}),
    ));
    assert_eq!(started["ok"], true, "{started}");
    let session = started["session_id"].as_str().unwrap().to_string();
    for (tool, args) in [
        (
            "read_output",
            json!({"workspace":api.id,"session_id":session}),
        ),
        (
            "write_stdin",
            json!({"workspace":api.id,"session_id":session,"chars":"x"}),
        ),
        (
            "kill_session",
            json!({"workspace":api.id,"session_id":session}),
        ),
    ] {
        let stolen = structured(call_as("bob", tool, args));
        assert_eq!(
            stolen["error"]["code"], "SESSION_NOT_FOUND",
            "{tool}: {stolen}"
        );
    }
    let bobs = structured(call_as("bob", "list_runs", json!({"workspace":api.id})));
    assert_eq!(bobs["runs"], json!([]), "{bobs}");

    // 新连接带同一个主体来（节点重连后就是这样），还能找回自己的命令并停掉。
    let alices = structured(call_as("alice", "list_runs", json!({"workspace":api.id})));
    assert_eq!(alices["runs"][0]["session_id"], session, "{alices}");
    let killed = structured(call_as(
        "alice",
        "kill_session",
        json!({"workspace":api.id,"session_id":session}),
    ));
    assert_eq!(killed["ok"], true, "{killed}");

    // 本机把工具档收紧后立刻生效，不用等节点重启。
    DataStore::update_file(|data| {
        data.profiles[0].runtime.tool_profile = "read-only".into();
        Ok(())
    })
    .unwrap();
    let narrowed = structured(call_as(
        "alice",
        "exec_command",
        json!({"workspace":api.id,"cmd":"pwd"}),
    ));
    assert_eq!(narrowed["error"]["code"], "NODE_COMMANDS_NOT_ALLOWED");
}

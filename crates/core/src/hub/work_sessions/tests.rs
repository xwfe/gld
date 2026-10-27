use super::*;
use crate::auth::Principal;

struct Fixture {
    hub: Hub,
    _home: tempfile::TempDir,
}

impl Fixture {
    fn new(enabled: bool) -> Self {
        let home = tempfile::tempdir().unwrap();
        let mut settings = AppSettings::default();
        settings.hub.work_sessions = enabled;
        settings.global_instruction_sources = vec!["disabled".into()];
        settings.global_skill_sources = vec!["disabled".into()];
        let mut hub = Hub::fixed(
            &settings.hub.clone(),
            Fixed {
                profiles: Vec::new(),
                remotes: Vec::new(),
                settings,
            },
        );
        hub.managed_home = Some(home.path().into());
        Self { hub, _home: home }
    }

    fn call(&self, auth: &AuthContext, tool: &str, args: Value) -> Value {
        let (response, _) = self.hub.handle_request(
            auth,
            &json!({
                "jsonrpc":"2.0", "id":1, "method":"tools/call",
                "params":{"name":tool,"arguments":args}
            }),
        );
        if !response["error"].is_null() {
            return response;
        }
        response["result"]["structuredContent"].clone()
    }

    fn start(&self, auth: &AuthContext, request: &str) -> String {
        let result = self.call(
            auth,
            TOOL,
            json!({"action":"start", "client_request_id":request}),
        );
        assert_eq!(result["ok"], true, "{result}");
        result["workspace"].as_str().unwrap().to_string()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.hub.shutdown();
    }
}

fn owner(name: &str) -> AuthContext {
    AuthContext::new(
        Principal::OAuthClient {
            client_id: name.into(),
        },
        HUB_SCOPE,
    )
}

#[test]
fn zero_projects_can_create_a_work_directory_and_use_existing_file_tools() {
    let fixture = Fixture::new(true);
    let auth = owner("a");
    let id = fixture.start(&auth, "convert-1");
    let listed = fixture.call(&auth, LIST_WORKSPACES, json!({}));
    assert_eq!(
        listed["count"], 0,
        "managed workspaces are not registered projects"
    );
    assert_eq!(listed["work_sessions"]["tool"], TOOL);
    let context = fixture.call(&auth, WORKSPACE_CONTEXT, json!({"workspace":id}));
    assert_eq!(context["command_policy"], "guarded");
    assert!(context["instructions"]
        .as_str()
        .unwrap()
        .contains("non-project"));
    let root = PathBuf::from(context["path"].as_str().unwrap());
    assert!(!root.join(".git").exists());
    let patch = fixture.call(
        &auth,
        "apply_patch",
        json!({"workspace":id,
        "patch":"*** Begin Patch\n*** Add File: output.txt\n+hello\n*** End Patch\n"}),
    );
    assert_eq!(patch["ok"], true, "{patch}");
    let read = fixture.call(
        &auth,
        "read_file",
        json!({"workspace":id,"path":"output.txt"}),
    );
    assert_eq!(read["content"], "hello\n");
    let missing = fixture.call(&auth, "read_file", json!({"path":"output.txt"}));
    assert_eq!(missing["error"]["code"], "WORKSPACE_REQUIRED");
    let outside = fixture.call(
        &auth,
        "read_file",
        json!({"workspace":id,"path":"../other.txt"}),
    );
    assert_eq!(outside["ok"], false);
    assert!(!root.parent().unwrap().join(".git").exists());
}

#[test]
fn ownership_applies_to_listing_status_and_every_workspace_tool() {
    let fixture = Fixture::new(true);
    let a = owner("a");
    let b = owner("b");
    let id = fixture.start(&a, "same");
    assert_eq!(fixture.start(&a, "same"), id);
    let other = fixture.start(&b, "same");
    assert_ne!(id, other);
    let hidden = fixture.call(&b, "read_file", json!({"workspace":id,"path":"output.txt"}));
    assert_eq!(hidden["error"]["code"], "WORK_SESSION_NOT_FOUND");
    let hidden = fixture.call(&b, TOOL, json!({"action":"status","workspace":id}));
    assert_eq!(hidden["error"]["code"], "WORK_SESSION_NOT_FOUND");
    let page = fixture.call(&b, TOOL, json!({"action":"list"}));
    assert_eq!(page["count"], 1);
    assert_eq!(page["sessions"][0]["workspace"], other);
    assert!(!serde_json::to_string(&page).unwrap().contains(&id));
    fixture.start(&a, "second");
    let page = fixture.call(&a, TOOL, json!({"action":"list","limit":1}));
    let next = fixture.call(
        &a,
        TOOL,
        json!({"action":"list","limit":1,"cursor":page["next_cursor"]}),
    );
    assert_eq!(next["count"], 1);
    assert_ne!(
        page["sessions"][0]["workspace"],
        next["sessions"][0]["workspace"]
    );
    assert!(next["next_cursor"].is_null());
}

#[test]
fn disabled_read_only_anonymous_and_scoped_grants_cannot_create_or_access() {
    let disabled = Fixture::new(false);
    assert!(!disabled.hub.list_tools().iter().any(|t| t["name"] == TOOL));
    assert_eq!(
        disabled.call(
            &owner("a"),
            TOOL,
            json!({"action":"start","client_request_id":"one"})
        )["error"]["code"],
        "WORK_SESSIONS_DISABLED"
    );
    let mut fixture = Fixture::new(true);
    let auth = owner("a");
    let id = fixture.start(&auth, "one");
    for read_only in [true, false] {
        let grant = auth.clone().with_grant(Some(Grant {
            id: "grant".into(),
            name: "limited".into(),
            workspaces: vec![id.clone()],
            read_only,
            browser: false,
        }));
        assert!(!fixture
            .hub
            .list_tools_for(grant.grant())
            .iter()
            .any(|t| t["name"] == TOOL));
        let denied = fixture.call(&grant, WORKSPACE_CONTEXT, json!({"workspace":id}));
        assert_eq!(denied["error"]["code"], "WORK_SESSION_REQUIRES_FULL_AUTH");
    }
    fixture.hub.tool_profile = "read-only".into();
    assert!(!fixture.hub.list_tools().iter().any(|t| t["name"] == TOOL));
    assert_eq!(
        fixture.call(&auth, "read_file", json!({"workspace":id,"path":"x"}))["error"]["code"],
        "WORK_SESSIONS_DISABLED"
    );
    fixture.hub.tool_profile = "compact".into();
    fixture.hub.auth.auth_type = "noauth".into();
    let anonymous = AuthContext::anonymous(HUB_SCOPE);
    assert!(!fixture.hub.list_tools().iter().any(|t| t["name"] == TOOL));
    assert_eq!(
        fixture.call(
            &anonymous,
            TOOL,
            json!({"action":"start","client_request_id":"one"})
        )["error"]["code"],
        "WORK_SESSION_REQUIRES_FULL_AUTH"
    );
}

#[test]
fn unrelated_calls_do_not_stop_the_owners_managed_command() {
    let fixture = Fixture::new(true);
    let auth = owner("background");
    let id = fixture.start(&auth, "long-command");
    let command = fixture.call(&auth, "exec_command", json!({"workspace":id,
        "argv":["python3","-c","import time; print('ready', flush=True); time.sleep(1); print('done')"],
        "yield_time_ms":1,"timeout_ms":5000}));
    assert_eq!(command["ok"], true, "{command}");
    fixture.call(&owner("someone-else"), LIST_WORKSPACES, json!({}));
    fixture.call(&owner("someone-else"), TOOL, json!({"action":"list"}));
    let result = fixture.call(
        &auth,
        "write_stdin",
        json!({"workspace":id,
        "session_id":command["session_id"], "chars":"", "yield_time_ms":5000}),
    );
    assert_eq!(result["exit_code"], 0, "{result}");
    assert!(result["stdout"].as_str().unwrap().contains("done"));
}

#[test]
fn reloaded_hub_keeps_ids_and_rejects_unknown_action_fields() {
    let fixture = Fixture::new(true);
    let auth = owner("persist");
    let id = fixture.start(&auth, "one");
    let (_, settings) = fixture.hub.snapshot().unwrap();
    let mut new_hub = Hub::fixed(
        &settings.hub.clone(),
        Fixed {
            profiles: vec![],
            remotes: vec![],
            settings,
        },
    );
    new_hub.managed_home = fixture.hub.managed_home.clone();
    let (response, _) = new_hub.handle_request(
        &auth,
        &json!({"id":1,"method":"tools/call","params":{
            "name":TOOL,"arguments":{"action":"status","workspace":id}
        }}),
    );
    assert_eq!(
        response["result"]["structuredContent"]["session"]["workspace"],
        id
    );
    for args in [
        json!({"action":"start","client_request_id":"two","root":"/tmp"}),
        json!({"action":"list","limit":0}),
        json!({"action":"finish","workspace":id}),
    ] {
        assert_eq!(
            fixture.call(&auth, TOOL, args)["error"]["code"],
            "INVALID_ARGUMENT"
        );
    }
    new_hub.shutdown();
}

#[test]
fn retain_and_artifact_route_only_owned_results() {
    let fixture = Fixture::new(true);
    let a = owner("a");
    let id = fixture.start(&a, "one");
    let ctx = fixture.call(&a, WORKSPACE_CONTEXT, json!({"workspace":id}));
    fs_write(ctx["path"].as_str().unwrap(), "result.txt", "saved result");
    let saved = fixture.call(
        &a,
        TOOL,
        json!({"action":"retain","workspace":id,"keep_paths":["result.txt"]}),
    );
    assert_eq!(saved["ok"], true, "{saved}");
    let artifact = &saved["artifacts"][0]["id"];
    let read = fixture.call(
        &a,
        TOOL,
        json!({"action":"artifact","artifact_id":artifact,"include_content":true}),
    );
    assert_eq!(read["content"], "saved result");
    let other = fixture.call(
        &owner("b"),
        TOOL,
        json!({"action":"artifact","artifact_id":artifact}),
    );
    assert_eq!(other["error"]["code"], "WORK_SESSION_NOT_FOUND");
}

fn fs_write(root: &str, path: &str, content: &str) {
    std::fs::write(PathBuf::from(root).join(path), content).unwrap();
}

#[test]
fn running_command_blocks_finish_then_output_survives_closure() {
    let fixture = Fixture::new(true);
    let auth = owner("finish");
    let id = fixture.start(&auth, "one");
    let command = fixture.call(&auth, "exec_command", json!({"workspace":id,
        "argv":["python3","-c","import sys; print('ready',flush=True); sys.stdin.read(); open('out.txt','w').write('done')"],
        "stdin_mode":"interactive", "yield_time_ms":1,"timeout_ms":10000}));
    assert_eq!(command["ok"], true, "{command}");
    let blocked = fixture.call(
        &auth,
        TOOL,
        json!({"action":"finish","workspace":id,"keep_paths":["out.txt"]}),
    );
    assert_eq!(
        blocked["error"]["code"], "WORK_SESSION_RESOURCES_BUSY",
        "{blocked}"
    );
    // 主动停止，不靠短 sleep 猜测进程已退出；另写最终文件。
    let stop = fixture.call(
        &auth,
        "kill_session",
        json!({"workspace":id,"session_id":command["session_id"],"wait_ms":5000}),
    );
    assert_eq!(stop["ok"], true, "{stop}");
    let patch = fixture.call(&auth,"apply_patch",json!({"workspace":id,"patch":"*** Begin Patch\n*** Add File: out.txt\n+done\n*** End Patch\n"}));
    assert_eq!(patch["ok"], true, "{patch}");
    let done = fixture.call(
        &auth,
        TOOL,
        json!({"action":"finish","workspace":id,"keep_paths":["out.txt"]}),
    );
    assert_eq!(done["closed"], true, "{done}");
    let status = fixture.call(&auth, TOOL, json!({"action":"status","workspace":id}));
    assert_eq!(status["session"]["state"], "closed", "{status}");
    let read = fixture.call(&auth,TOOL,json!({"action":"artifact","artifact_id":done["artifacts"][0]["id"],"include_content":true}));
    assert_eq!(read["content"], "done\n");
    let write = fixture.call(&auth,"apply_patch",json!({"workspace":id,"patch":"*** Begin Patch\n*** Add File: late.txt\n+late\n*** End Patch\n"}));
    assert_eq!(write["error"]["code"], "WORK_SESSION_NOT_ACTIVE");
}

#[test]
fn a_rejected_command_does_not_leave_a_false_resource_blocker() {
    let fixture = Fixture::new(true);
    let auth = owner("rejection");
    let id = fixture.start(&auth, "one");
    fixture.call(
        &auth,
        "exec_command",
        json!({"workspace":id,"argv":["not-an-allowed-program"]}),
    );
    let done = fixture.call(
        &auth,
        TOOL,
        json!({"action":"finish","workspace":id,"keep_paths":[]}),
    );
    assert_eq!(done["closed"], true, "{done}");
}

#[test]
fn failure_retention_and_explicit_pinning_do_not_claim_the_task_closed() {
    let fixture = Fixture::new(true);
    let auth = owner("retention");
    let id = fixture.start(&auth, "one");
    let failed = fixture.call(
        &auth,
        TOOL,
        json!({"action":"finish","workspace":id,"keep_paths":[],"outcome":"failed"}),
    );
    assert_eq!(failed["closed"], false, "{failed}");
    assert_eq!(failed["session"]["idle_retention_ms"], 24 * 60 * 60 * 1000);
    let pinned = fixture.call(
        &auth,
        TOOL,
        json!({"action":"retain","workspace":id,"retain_forever":true}),
    );
    assert_eq!(pinned["ok"], true, "{pinned}");
    assert!(pinned["session"]["expires_at_ms"].is_null());
    for args in [
        json!({"action":"retain","workspace":id,"retention_hours":0}),
        json!({"action":"retain","workspace":id,"retention_hours":24,"retain_forever":true}),
        json!({"action":"finish","workspace":id,"keep_paths":[],"outcome":"typo"}),
        json!({"action":"sweep","limit":100}),
    ] {
        assert_eq!(
            fixture.call(&auth, TOOL, args)["error"]["code"],
            "INVALID_ARGUMENT"
        );
    }
}

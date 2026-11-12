//! 固定 JSON 样例用于验证 Rust 类型迁移没有一起改动收发两侧的线格式。

use gld_protocol::daemon::{
    Request, Response, RpcError, FIRST_PROTOCOL_WITH_GENERATIONS, PROTOCOL_VERSION,
};
use serde_json::{json, Value};

fn canonical_request(input: Value, expected: Value) {
    let request: Request = serde_json::from_value(input).unwrap();
    assert_eq!(serde_json::to_value(request).unwrap(), expected);
}

#[test]
fn application_inputs_keep_legacy_defaults_and_field_names() {
    canonical_request(
        json!({"op":"resolve_workspace","target":{}}),
        json!({"op":"resolve_workspace","target":{"selector":null,"cwd":null}}),
    );
    canonical_request(
        json!({"op":"create_workspace","path":"project","options":{}}),
        json!({"op":"create_workspace","path":"project","options":{
            "name":null,"mcp_port":null,"actions_port":null
        }}),
    );
    canonical_request(
        json!({"op":"add_grant","spec":{"name":"reader","workspaces":["workspace"]}}),
        json!({"op":"add_grant","spec":{
            "name":"reader","workspaces":["workspace"],"writable":false
        }}),
    );
    canonical_request(
        json!({"op":"add_browser_grant","spec":{"name":"browser","workspaces":["workspace"]}}),
        json!({"op":"add_browser_grant","spec":{
            "name":"browser","workspaces":["workspace"],"writable":false,
            "ttlSeconds":28800,"allowedOrigins":[]
        }}),
    );
    canonical_request(
        json!({"op":"hub_add_remote","spec":{"name":"","node":"","workspace":"example"}}),
        json!({"op":"hub_add_remote","spec":{
            "name":"","node":"","workspace":"example","ccnmBin":"","mode":""
        }}),
    );
    canonical_request(
        json!({"op":"set_runtime_settings","runtime":{}}),
        json!({"op":"set_runtime_settings","runtime":{
            "executablePaths":"","aiInstructions":"","instructionSources":[],
            "skillSources":[],"customInstructionPaths":"","customSkillPaths":"",
            "hiddenSkills":[],"allowLanAccess":false,"restoreRuntimeStateOnLaunch":false
        }}),
    );
}

#[test]
fn planning_and_prune_keep_existing_state_spellings() {
    canonical_request(
        json!({"op":"update_plan","target":{"selector":"workspace"},"plan_id":"plan",
            "status":"awaiting_acceptance","step_updates":[{"step_id":"step","status":"in_progress"}],
            "focus":null}),
        json!({"op":"update_plan","target":{"selector":"workspace","cwd":null},"plan_id":"plan",
            "status":"awaiting_acceptance","step_updates":[{
                "step_id":"step","status":"in_progress","notes":null
            }],"focus":null}),
    );
    canonical_request(
        json!({"op":"prune","confirmed":{"missingProjects":[],"leftovers":[{
            "kind":"runs","path":"runs/example","bytes":9
        }]}}),
        json!({"op":"prune","confirmed":{"missingProjects":[],"leftovers":[{
            "kind":"runs","path":"runs/example","bytes":9,"project":""
        }],"busy":[],"applied":false,"failures":[]}}),
    );
}

#[test]
fn optional_overrides_and_flags_keep_omission_rules() {
    canonical_request(
        json!({"op":"approve_agent_task","task_id":"task"}),
        json!({"op":"approve_agent_task","task_id":"task"}),
    );
    canonical_request(
        json!({"op":"approve_agent_task","task_id":"task","agent":"codex","model":"model","effort":"high"}),
        json!({"op":"approve_agent_task","task_id":"task","agent":"codex","model":"model","effort":"high"}),
    );
    canonical_request(
        json!({"op":"desktop_probe"}),
        json!({"op":"desktop_probe","only_if_used":false}),
    );
    canonical_request(
        json!({"op":"switch_mcp_servers","names":["example"],"on":false}),
        json!({"op":"switch_mcp_servers","names":["example"],"on":false,"all":false}),
    );
    canonical_request(
        json!({"op":"frp_snippet","target":{},"kind":"actions"}),
        json!({"op":"frp_snippet","target":{"selector":null,"cwd":null},"kind":"actions","reveal":false}),
    );
    canonical_request(
        json!({"op":"delete_frp_profile","id":"profile"}),
        json!({"op":"delete_frp_profile","id":"profile","force":false}),
    );
}

#[test]
fn response_envelopes_and_error_display_keep_the_wire_contract() {
    let ok: Response = Ok(json!({"value":7})).into();
    assert_eq!(
        serde_json::to_value(ok).unwrap(),
        json!({"status":"ok","result":{"value":7}})
    );
    for (code, error) in [
        ("app", RpcError::app("message")),
        ("protocol", RpcError::protocol("message")),
        ("internal", RpcError::internal("message")),
    ] {
        assert_eq!(error.to_string(), "message");
        let response: Response = Err(error).into();
        let fixture = json!({"status":"error","error":{"code":code,"message":"message"}});
        assert_eq!(serde_json::to_value(response).unwrap(), fixture);
        let decoded: Response = serde_json::from_value(fixture).unwrap();
        assert_eq!(decoded.into_result().unwrap_err().to_string(), "message");
    }
    assert_eq!(PROTOCOL_VERSION, 18);
    assert_eq!(FIRST_PROTOCOL_WITH_GENERATIONS, 16);
}

use super::*;

#[test]
fn request_routes_follow_visible_state_without_executing_the_tool() {
    let f = fixture();
    let request = |workspace: &str| {
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "read_file", "arguments": {
                "workspace": workspace, "path": "only-api.txt"
            } }
        })
    };
    let auth = caller();
    for (body, expected) in [
        (json!({"method": "tools/list"}), ExecutionRoute::Discovery),
        (
            json!({"method": "tools/call", "params": {}}),
            ExecutionRoute::Unbound,
        ),
        (
            json!({"method": "tools/call", "params": {"name": "list_workspaces"}}),
            ExecutionRoute::Unbound,
        ),
        (request("api"), ExecutionRoute::Owned(f.api.path.clone())),
        (request("not-a-member"), ExecutionRoute::Unbound),
        (request("outsider"), ExecutionRoute::Unbound),
    ] {
        assert_eq!(f.hub.execution_route(&auth, &body), expected, "{body}");
    }
    let limited = auth.with_grant(Some(Grant {
        id: "grant".into(),
        name: "only-web".into(),
        workspaces: vec![f.web.id.clone()],
        read_only: true,
        browser: false,
    }));
    assert_eq!(
        f.hub.execution_route(&limited, &request("api")),
        ExecutionRoute::Unbound
    );
    assert_eq!(
        f.hub.execution_route(&limited, &request("web")),
        ExecutionRoute::Owned(f.web.path.clone()),
    );
    assert!(
        f.hub.contexts.lock().unwrap().is_empty(),
        "路由不应建立执行上下文"
    );
}

#[test]
fn member_routes_follow_the_location_of_their_execution_state() {
    let f = fixture();
    for (member, expected) in [
        (
            Member::Local(f.api.clone()),
            ExecutionRoute::Owned(f.api.path.clone()),
        ),
        (
            Member::Remote(CcnmMember {
                id: "remote-id".into(),
                name: "remote".into(),
                ccnm_bin: "ccnm".into(),
                node: "test-node".into(),
                workspace: "remote-project".into(),
                max_mode: Mode::Read,
            }),
            ExecutionRoute::Owned("remote-id".into()),
        ),
        (
            Member::Native(NativeMember {
                id: "native-id".into(),
                name: "native".into(),
                machine_id: "test-machine".into(),
                remote_workspace_id: "native-project".into(),
                online: true,
                revoked: false,
                tool_profile: "read-only".into(),
            }),
            ExecutionRoute::Unbound,
        ),
    ] {
        assert_eq!(member_route(&member), expected, "{}", member.id());
    }
}

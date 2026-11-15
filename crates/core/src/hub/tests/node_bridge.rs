use super::*;

#[derive(Default)]
struct Bridge {
    fail_members: std::sync::atomic::AtomicBool,
    calls: Mutex<Vec<(AuthContext, String, String, Value)>>,
}

impl NodeBridge for Bridge {
    fn members(&self) -> Result<Vec<NativeMember>, String> {
        if self.fail_members.load(Ordering::SeqCst) {
            return Err("node inventory unavailable".into());
        }
        Ok(vec![NativeMember {
            id: "node-project".into(),
            name: "machine/project".into(),
            machine_id: "machine".into(),
            remote_workspace_id: "project".into(),
            online: false,
            revoked: false,
            tool_profile: "read-only".into(),
        }])
    }

    fn call(&self, auth: &AuthContext, member: &NativeMember, tool: &str, args: Value) -> Value {
        self.calls
            .lock()
            .unwrap()
            .push((auth.clone(), member.id.clone(), tool.to_string(), args));
        json!({"bridge_result": true})
    }
}

#[test]
fn application_hubs_share_the_injected_bridge_without_owning_its_lifetime() {
    let bridge = Arc::new(Bridge::default());
    let first = Hub::new(&HubConfig::default(), bridge.clone());
    let second = Hub::new(&HubConfig::default(), bridge.clone());
    let member = first.node_members().unwrap().pop().unwrap();
    assert!(!member.online, "离线项目也必须保留在桥的结果里");
    let auth = oauth_client("client");
    let args = json!({"path": "README.md"});
    assert_eq!(
        first.node_call(&auth, &member, "read_file", args.clone()),
        json!({"bridge_result": true})
    );
    assert_eq!(
        *bridge.calls.lock().unwrap(),
        vec![(auth, member.id.clone(), "read_file".to_string(), args)]
    );
    first.shutdown();
    drop(first);
    assert_eq!(second.node_members().unwrap()[0].id, member.id);
    bridge.fail_members.store(true, Ordering::SeqCst);
    assert_eq!(
        second.node_members().err().as_deref(),
        Some("node inventory unavailable")
    );
}

#[test]
fn host_node_cleanup_stays_between_workspace_connections_and_machine_mcp() {
    let (mut hub, opened, _dir) = relay_hub(&["context7"], "compact");
    let spy = RemoteSpy::new();
    let remote = remote_fixture(spy.clone());
    hub.connections = Connections::with_opener(Box::new(spy.clone()));
    call(&hub, "list_dir", json!({"workspace": "api", "path": "."}));
    assert_eq!(hub.contexts.lock().unwrap().len(), 1);
    let args = json!({"server": "context7", "tool": "echo", "arguments": {"q": 1}});
    assert_eq!(
        relay_call(&hub, &caller(), "call_mcp_tool", args.clone())["isError"],
        false
    );
    hub.connections
        .call(
            "test",
            &remote.remote,
            "read_file",
            json!({"path": "README.md"}),
        )
        .unwrap();
    assert_eq!(hub.connections.open_count(), 1);
    assert_eq!(opened.load(Ordering::SeqCst), 1);

    let mut cleaned = false;
    hub.shutdown_with(|| {
        assert!(hub.contexts.lock().unwrap().is_empty());
        assert_eq!(hub.connections.open_count(), 0);
        assert_eq!(spy.closes(), 1);
        // 这里若已关本机 MCP，再调用就会开第二条；旧顺序应继续复用第一条。
        assert_eq!(
            relay_call(&hub, &caller(), "call_mcp_tool", args.clone())["isError"],
            false
        );
        assert_eq!(opened.load(Ordering::SeqCst), 1);
        cleaned = true;
    });
    assert!(cleaned);
    assert_eq!(
        relay_call(&hub, &caller(), "call_mcp_tool", args)["isError"],
        false
    );
    assert_eq!(
        opened.load(Ordering::SeqCst),
        2,
        "外部节点清理之后必须关闭本机 MCP"
    );
    hub.shutdown();
}

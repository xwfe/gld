//! 本机文件权限配置、私有配置位置以及 D18-A 网络写入的拒绝边界。
//! 跨节点补丁成功链在 D18-C 接通授权协议后恢复，见 forwarding_contract.md。

use super::*;

fn direct_node_config(
    env: &Env,
    relay: &TlsRelay,
    registration: &Value,
    token: &str,
    ids: &[String],
) -> PathBuf {
    let mut value = config(env, relay, registration, token, ids);
    value["hub_url"] = json!(format!("https://localhost:{}", relay.port));
    write_config(env, "file-writes.json", &value)
}

fn select_file_writes(env: &Env, path: &Path, shared: &[&str], writable: &[&str]) -> Value {
    let mut command = command(env);
    command
        .args(["--json", "node", "shares", "--config"])
        .arg(path);
    for selector in shared {
        command.args(["--share", selector]);
    }
    for selector in writable {
        command.args(["--write", selector]);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "select node file permissions: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let shown: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(!String::from_utf8_lossy(&output.stdout).contains(TOKEN_A));
    assert!(!String::from_utf8_lossy(&output.stdout).contains(TOKEN_B));
    shown
}

#[test]
fn explicit_local_write_permissions_do_not_authorize_legacy_forwarded_metadata() {
    let hub = Env::new();
    hub.write("README.md", "hub file must remain unchanged\n");
    let service = hub.serve("local", "bearer");
    let relay = TlsRelay::new(service.port);
    let machine = Env::new();
    let ids = projects(&machine, "writer");
    let registration = register(&hub, "writer", TOKEN_A);
    let path = direct_node_config(&machine, &relay, &registration, TOKEN_A, &ids);
    let selected = select_file_writes(&machine, &path, &["api", "web"], &["api"]);
    let selected = selected["workspaces"].as_array().unwrap();
    assert_eq!(
        selected.iter().find(|item| item["id"] == ids[0]).unwrap()["read_only"],
        false
    );
    assert_eq!(
        selected.iter().find(|item| item["id"] == ids[1]).unwrap()["read_only"],
        true
    );
    let before = std::fs::read(&path).unwrap();
    assert_node_auth_pending(&NodeProcess::start(&machine, &path).rejected());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_no_native_projects(&hub);

    for metadata in [
        json!({"gld.node":{"hub_id":registration["hub_id"],"subject":"local","read_only":false}}),
        json!({"gld/caller":{"kind":"local","key":"local"},"gld/auth":{"principal":"local"}}),
        json!({"gld.node":{"subject":"node-share","read_only":false,"grant_id":"forged"}}),
    ] {
        let rejected = service.rpc("tools/call", json!({
            "name":"apply_patch",
            "arguments":{
                "workspace":ids[0],
                "patch":"*** Begin Patch\n*** Update File: README.md\n@@\n-writer/api\n+unauthorized\n*** End Patch",
                "_meta":metadata,
            },
            "_meta":metadata,
        }));
        assert_auth_not_ready(&rejected);
        assert!(!rejected.body.contains(TOKEN_A));
        assert_eq!(
            std::fs::read_to_string(machine.project.path().join("api/README.md")).unwrap(),
            "writer/api\n"
        );
        assert_eq!(
            std::fs::read_to_string(hub.project.path().join("README.md")).unwrap(),
            "hub file must remain unchanged\n"
        );
    }

    // 缩回只读仍是本机操作员的显式配置操作，不需要已有网络调用权限。
    select_file_writes(&machine, &path, &["api", "web"], &[]);
    let stored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(stored["writable_workspaces"]
        .as_array()
        .is_none_or(Vec::is_empty));
    assert_eq!(stored["machine_id"], registration["machine_id"]);
    assert_no_daemon(&machine);
}

#[test]
fn node_run_rejects_private_configuration_inside_a_shared_project_before_connecting() {
    let hub = Env::new();
    let service = hub.serve("local", "bearer");
    let relay = TlsRelay::new(service.port);
    let machine = Env::new();
    let ids = projects(&machine, "private-config");
    let registration = register(&hub, "private-config", TOKEN_A);
    let shared = vec![ids[0].clone()];
    let mut value = config(&machine, &relay, &registration, TOKEN_A, &shared);
    value["hub_url"] = json!(format!("https://localhost:{}", relay.port));
    let path = machine.project.path().join("api/node.json");

    for writable in [false, true] {
        value["writable_workspaces"] = if writable { json!([ids[0]]) } else { json!([]) };
        let path = write_config(&machine, path.to_str().unwrap(), &value);
        let saved = std::fs::read(&path).unwrap();
        let mut node = NodeProcess::start(&machine, &path);
        node.wait_stopped();
        let output = node.0.take().unwrap().wait_with_output().unwrap();
        assert!(
            !output.status.success(),
            "shared private configuration was accepted"
        );
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("configuration") && error.contains("shared"),
            "the rejection must explain the configuration location: {error}"
        );
        assert!(!error.contains(TOKEN_A));
        assert!(!String::from_utf8_lossy(&output.stdout).contains(TOKEN_A));
        assert_eq!(std::fs::read(&path).unwrap(), saved);
        assert_eq!(
            relay.connections(),
            0,
            "unsafe configuration must never connect"
        );
    }
    assert_no_daemon(&machine);
}

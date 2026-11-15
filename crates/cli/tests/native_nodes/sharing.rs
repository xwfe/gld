//! 正式配置的共享范围只在停止后修改，运行中的离线节点仍持有配置锁。
//! D18-A 只验证本地配置和机器登记；网络转发在授权协议接通前拒绝。

use super::*;

fn shares(env: &Env, path: &Path, selected: &[&str]) -> Output {
    let mut command = command(env);
    command
        .args(["--json", "node", "shares", "--config"])
        .arg(path);
    for selector in selected {
        command.args(["--share", selector]);
    }
    command.output().unwrap()
}

fn private_output(output: &Output) {
    assert!(!String::from_utf8_lossy(&output.stdout).contains(TOKEN_A));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(TOKEN_A));
}

#[test]
fn cli_sharing_changes_require_a_stopped_node_and_preserve_its_identity() {
    let hub = Env::new();
    let service = hub.serve("local", "bearer");
    let relay = TlsRelay::new(service.port);
    let machine = Env::new();
    let ids = projects(&machine, "share-node");
    let registration = register(&hub, "share-node", TOKEN_A);
    let mut value = config(&machine, &relay, &registration, TOKEN_A, &ids);
    value["hub_url"] = json!(format!("https://127.0.0.1:{}", free_port()));
    let path = write_config(&machine, "sharing.json", &value);
    let mut node = NodeProcess::start(&machine, &path);
    wait_running(&path);
    let saved = std::fs::read(&path).unwrap();

    let shown = shares(&machine, &path, &[]);
    assert!(
        shown.status.success(),
        "show while running failed: {}",
        String::from_utf8_lossy(&shown.stderr)
    );
    private_output(&shown);
    let listed: Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(listed["workspaces"].as_array().unwrap().len(), 2);
    assert!(listed["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .all(|workspace| workspace["read_only"] == true));

    let refused = shares(&machine, &path, &["web"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("in use"));
    private_output(&refused);
    assert_eq!(std::fs::read(&path).unwrap(), saved);
    assert_eq!(
        relay.connections(),
        0,
        "local lock checks do not need a Hub connection"
    );

    node.stop();
    let changed = shares(&machine, &path, &[ids[1].as_str()]);
    assert!(
        changed.status.success(),
        "change after stop failed: {}",
        String::from_utf8_lossy(&changed.stderr)
    );
    private_output(&changed);
    let changed_view: Value = serde_json::from_slice(&changed.stdout).unwrap();
    assert_eq!(changed_view["workspaces"].as_array().unwrap().len(), 1);
    assert_eq!(changed_view["workspaces"][0]["id"], ids[1]);
    let mut stored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(stored["workspaces"], json!([ids[1]]));
    stored.as_object_mut().unwrap().remove("workspaces");
    value.as_object_mut().unwrap().remove("workspaces");
    assert_eq!(
        stored, value,
        "sharing does not replace identity, credentials or CA"
    );

    let mut reconnect: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    reconnect["hub_url"] = json!(format!("https://localhost:{}", relay.port));
    write_config(&machine, "sharing.json", &reconnect);
    assert_node_auth_pending(&NodeProcess::start(&machine, &path).rejected());
    assert_no_native_projects(&hub);
    assert_no_daemon(&machine);
}

#[test]
fn leaving_removes_only_the_local_node_state_and_hub_keeps_the_machine_until_revoked() {
    let hub = Env::new();
    let service = hub.serve("local", "bearer");
    let relay = TlsRelay::new(service.port);
    let machine = Env::new();
    let ids = projects(&machine, "leaving");
    let registration = register(&hub, "leaving", TOKEN_A);
    let mut value = config(&machine, &relay, &registration, TOKEN_A, &ids);
    value["hub_url"] = json!(format!("https://localhost:{}", free_port()));
    let path = write_config(&machine, "leaving.json", &value);
    let readme = machine.project.path().join("api/README.md");
    let leave = |extra: &[&str]| {
        command(&machine)
            .args(["--json", "leave", "--config"])
            .arg(&path)
            .args(extra)
            .output()
            .unwrap()
    };

    let mut node = NodeProcess::start(&machine, &path);
    wait_running(&path);
    assert!(path.exists());
    let refused = leave(&[]);
    assert!(!refused.status.success(), "needs confirmation");
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--yes"));
    let running = leave(&["--yes"]);
    assert!(!running.status.success(), "the node still holds the lock");
    assert!(String::from_utf8_lossy(&running.stderr).contains("in use"));
    assert!(path.exists());
    node.stop();

    let left = leave(&["--yes"]);
    assert!(
        left.status.success(),
        "{}",
        String::from_utf8_lossy(&left.stderr)
    );
    let output: Value = serde_json::from_slice(&left.stdout).unwrap();
    assert_eq!(output["machine_id"], registration["machine_id"]);
    assert!(!String::from_utf8_lossy(&left.stdout).contains(TOKEN_A));
    assert!(!path.exists());
    assert_eq!(
        std::fs::read_to_string(&readme).unwrap(),
        "leaving/api\n",
        "project files stay"
    );
    assert!(
        !leave(&["--yes"]).status.success(),
        "nothing left to remove"
    );

    // 本机离开只删除本机配置；Hub 的机器登记留给 Hub 操作员明确撤销。
    let machines = stored_machines(&hub);
    assert_eq!(machines.len(), 1);
    assert_eq!(machines[0]["id"], registration["machine_id"]);
    assert_eq!(machines[0]["revoked"], false);
    assert_no_native_projects(&hub);
    let machine_id = registration["machine_id"].as_str().unwrap();
    assert_eq!(
        hub.json(&["--json", "node", "revoke", machine_id])["revoked"],
        true
    );
}

#[test]
fn leaving_refuses_a_file_that_is_not_a_node_configuration() {
    let machine = Env::new();
    let path = machine.home.path().join("not-a-node.json");
    std::fs::write(&path, "{\"important\":true}\n").unwrap();
    let output = command(&machine)
        .args(["leave", "--yes", "--config"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(path.exists());
}

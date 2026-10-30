//! 正式配置的共享范围在停止后修改；运行时查看可用，权限收缩在再次连接时生效。

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
fn cli_sharing_changes_require_a_stopped_node_and_preserve_remaining_project_handles() {
    let hub = Env::new();
    let service = hub.serve("local", "bearer");
    let relay = TlsRelay::new(service.port);
    let machine = Env::new();
    let ids = projects(&machine, "share-node");
    let registration = register(&hub, "share-node", TOKEN_A);
    let mut value = config(&machine, &relay, &registration, TOKEN_A, &ids);
    value["hub_url"] = json!(format!("https://127.0.0.1:{}", relay.port));
    let path = write_config(&machine, "sharing.json", &value);
    let mut node = NodeProcess::start(&machine, &path);
    let original = wait_inventory(&service, 2, 2);
    let api_handle = find(&original, "share-node/api");
    let web_handle = find(&original, "share-node/web");
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
    assert_eq!(read(&service, &api_handle)["content"], "share-node/api\n");

    node.stop();
    wait_inventory(&service, 0, 2);
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

    let _node = NodeProcess::start(&machine, &path);
    let current = wait_inventory(&service, 1, 1);
    assert_eq!(find(&current, "share-node/web"), web_handle);
    assert!(!current
        .iter()
        .any(|workspace| workspace["name"] == "share-node/api"
            || workspace["name"] == "share-node/unshared"));
    assert_eq!(read(&service, &web_handle)["content"], "share-node/web\n");
    assert_eq!(read(&service, &api_handle)["ok"], false);
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
    value["hub_url"] = json!(format!("https://localhost:{}", relay.port));
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
    wait_inventory(&service, 2, 2);
    assert!(path.exists());
    let refused = leave(&[]);
    assert!(!refused.status.success(), "needs confirmation");
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--yes"));
    let running = leave(&["--yes"]);
    assert!(!running.status.success(), "the node still holds the lock");
    assert!(String::from_utf8_lossy(&running.stderr).contains("in use"));
    assert!(path.exists());
    node.stop();
    wait_inventory(&service, 0, 2);

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

    // 登记还在，操作员撤销之前显示离线，不会悄悄消失。
    let items = wait_inventory(&service, 0, 2);
    assert_eq!(
        items.iter().filter(|item| item["kind"] == "node").count(),
        2
    );
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

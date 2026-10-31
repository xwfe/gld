//! 新机器接入：不用先学 `gld add`，不用记配置放哪，之后的命令也不用再写 `--config`。

use super::*;

fn joining(machine: &Env, url: &str, ca: &Path, extra: &[&str], invitation: &str) -> Output {
    let mut join = command(machine);
    join.args(["--json", "join", "--hub", url, "--invite-stdin", "--yes"])
        .args(extra)
        .arg("--ca-certificate")
        .arg(ca)
        .stdin(Stdio::piped());
    let mut child = join.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(invitation.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn fresh_machine() -> (Env, PathBuf, PathBuf) {
    let machine = Env::new();
    machine.write("api/README.md", "fresh/api\n");
    let dir = machine.project.path().join("api");
    let ca = machine.home.path().join("ca.der");
    std::fs::write(&ca, decode(CA)).unwrap();
    (machine, dir, ca)
}

#[test]
fn a_fresh_machine_joins_by_directory_and_later_commands_find_the_config_themselves() {
    let hub = Env::new();
    let service = hub.serve("local", "bearer");
    let relay = TlsRelay::new(service.port);
    let (machine, dir, ca) = fresh_machine();
    let url = format!("https://localhost:{}", relay.port);
    let invitation = hub.json(&["--json", "node", "invite", "fresh"]);
    let token = invitation["invitation"].as_str().unwrap();
    let dir_text = dir.to_str().unwrap();

    let output = joining(
        &machine,
        &url,
        &ca,
        &["--share", dir_text, "--write", dir_text],
        token,
    );
    assert!(
        output.status.success(),
        "join failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let joined: Value = serde_json::from_slice(&output.stdout).unwrap();
    let config = PathBuf::from(joined["config_path"].as_str().unwrap());
    assert_eq!(
        config.file_name().unwrap().to_str().unwrap(),
        format!("{}.json", invitation["hub_id"].as_str().unwrap())
    );
    assert_eq!(config.parent().unwrap().file_name().unwrap(), "nodes");
    assert_eq!(joined["workspaces"][0]["read_only"], false);
    assert_no_daemon(&machine);

    let child = command(&machine).args(["node", "run"]).spawn().unwrap();
    let mut node = NodeProcess(Some(child));
    let items = wait_inventory(&service, 1, 1);
    let api = find(&items, "fresh/api");
    assert_eq!(read(&service, &api)["content"], "fresh/api\n");
    node.stop();

    let shown = command(&machine)
        .args(["--json", "node", "shares"])
        .output()
        .unwrap();
    assert!(shown.status.success());
    let shown: Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(shown["workspaces"][0]["read_only"], false);

    let left = command(&machine)
        .args(["--json", "leave", "--yes"])
        .output()
        .unwrap();
    assert!(left.status.success());
    assert!(!config.exists());
    let again = command(&machine).args(["node", "run"]).output().unwrap();
    assert!(!again.status.success());
    assert!(String::from_utf8_lossy(&again.stderr).contains("gld join"));
}

#[test]
fn a_mistyped_directory_is_reported_without_consuming_the_invitation() {
    let hub = Env::new();
    let service = hub.serve("local", "bearer");
    let relay = TlsRelay::new(service.port);
    let (machine, _dir, ca) = fresh_machine();
    let url = format!("https://localhost:{}", relay.port);
    let invitation = hub.json(&["--json", "node", "invite", "typo"]);
    let token = invitation["invitation"].as_str().unwrap();
    let missing = machine.project.path().join("no-such-dir");

    let output = joining(
        &machine,
        &url,
        &ca,
        &["--share", missing.to_str().unwrap()],
        token,
    );
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains(token));
    assert_eq!(relay.connections(), 0, "nothing should reach the Hub");
}

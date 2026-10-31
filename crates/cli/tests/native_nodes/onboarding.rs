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
    let invitation = hub.json(&["--json", "node", "invite", "fresh", "--hub", &url]);
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
    let invitation = hub.json(&["--json", "node", "invite", "typo", "--hub", &url]);
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
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("目录不存在"), "{error}");
    assert!(!error.contains(token));
    assert_eq!(relay.connections(), 0, "nothing should reach the Hub");
}

fn doctor(env: &Env, config: &Path) -> (bool, Vec<Value>, String) {
    let output = command(env)
        .args(["--json", "node", "doctor", "--config"])
        .arg(config)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let parsed: Value = serde_json::from_str(&text).unwrap();
    (
        output.status.success(),
        parsed["checks"].as_array().unwrap().clone(),
        text,
    )
}

fn check<'a>(checks: &'a [Value], label: &str) -> &'a Value {
    checks
        .iter()
        .find(|check| check["label"] == label)
        .unwrap_or_else(|| panic!("no {label}: {checks:?}"))
}

#[test]
fn doctor_tells_apart_stopped_online_revoked_untrusted_and_unreachable() {
    let hub = Env::new();
    let service = hub.serve("local", "bearer");
    let relay = TlsRelay::new(service.port);
    let machine = Env::new();
    let ids = projects(&machine, "checked");
    let registration = register(&hub, "checked", TOKEN_A);
    let mut value = config(&machine, &relay, &registration, TOKEN_A, &ids);
    value["hub_url"] = json!(format!("https://localhost:{}", relay.port));
    let path = write_config(&machine, "checked.json", &value);

    // 配置好、节点没跑：能握手，提醒没在跑，不算失败。
    let (healthy, checks, text) = doctor(&machine, &path);
    assert!(healthy, "{text}");
    assert_eq!(check(&checks, "Hub 握手")["level"], "ok", "{text}");
    assert_eq!(check(&checks, "节点进程")["level"], "warn", "{text}");
    assert_eq!(check(&checks, "代理")["detail"], "直连", "{text}");
    assert!(!text.contains(TOKEN_A), "doctor output must be shareable");
    // 探测用完就断，节点随后能正常接上。
    let mut node = NodeProcess::start(&machine, &path);
    wait_inventory(&service, 2, 2);

    let (healthy, checks, text) = doctor(&machine, &path);
    assert!(healthy, "{text}");
    assert!(
        check(&checks, "Hub 握手")["detail"]
            .as_str()
            .unwrap()
            .contains("已在线"),
        "{text}"
    );
    node.stop();
    wait_inventory(&service, 0, 2);

    // 证书不受信：不配 CA。
    let mut untrusted = value.clone();
    untrusted.as_object_mut().unwrap().remove("ca_certificate");
    let untrusted = write_config(&machine, "untrusted.json", &untrusted);
    let (healthy, checks, text) = doctor(&machine, &untrusted);
    assert!(!healthy, "{text}");
    assert!(
        check(&checks, "Hub 握手")["fix"]
            .as_str()
            .unwrap()
            .contains("--ca-certificate"),
        "{text}"
    );

    // 端口上没有东西在听。
    let mut closed = value.clone();
    closed["hub_url"] = json!(format!("https://localhost:{}", free_port()));
    let closed = write_config(&machine, "closed.json", &closed);
    let (healthy, checks, text) = doctor(&machine, &closed);
    assert!(!healthy, "{text}");
    assert_eq!(check(&checks, "网络")["level"], "fail", "{text}");

    // Hub 撤销凭据。
    hub.ok(&[
        "node",
        "revoke",
        registration["machine_id"].as_str().unwrap(),
    ]);
    let (healthy, checks, text) = doctor(&machine, &path);
    assert!(!healthy, "{text}");
    let handshake = check(&checks, "Hub 握手");
    assert!(
        handshake["detail"].as_str().unwrap().contains("401"),
        "{text}"
    );
    assert!(
        handshake["fix"].as_str().unwrap().contains("gld leave"),
        "{text}"
    );
}

#[test]
fn doctor_without_a_config_says_how_to_join() {
    let machine = Env::new();
    let output = command(&machine).args(["node", "doctor"]).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("gld join"));
}

#[test]
fn an_invitation_needs_an_entry_the_target_can_keep_using() {
    let hub = Env::new();
    let _service = hub.serve("local", "bearer");
    let refused = command(&hub)
        .args(["node", "invite", "nowhere"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    let error = String::from_utf8_lossy(&refused.stderr);
    assert!(error.contains("--hub"), "{error}");
    let saved: Value =
        serde_json::from_slice(&std::fs::read(hub.home.path().join("data/profiles.json")).unwrap())
            .unwrap();
    assert!(
        saved["node_invitations"]
            .as_array()
            .is_none_or(|invitations| invitations.is_empty()),
        "a refused invitation must not be stored"
    );

    let plain = command(&hub)
        .args(["node", "invite", "plain", "--hub", "http://hub.example.com"])
        .output()
        .unwrap();
    assert!(!plain.status.success());

    let invited = hub.json(&[
        "--json",
        "node",
        "invite",
        "somewhere",
        "--hub",
        "https://hub.example.com/hub/",
    ]);
    assert_eq!(invited["hub_url"], "https://hub.example.com/hub");
    let token = invited["invitation"].as_str().unwrap();
    let join = invited["commands"]["join_unix"].as_str().unwrap();
    assert!(join.contains(token) && join.contains("https://hub.example.com/hub"));
    let version = env!("CARGO_PKG_VERSION");
    assert!(invited["commands"]["install_unix"]
        .as_str()
        .unwrap()
        .contains(&format!("/v{version}/install.sh")));

    let text = command(&hub)
        .args([
            "node",
            "invite",
            "readable",
            "--hub",
            "https://hub.example.com",
        ])
        .output()
        .unwrap();
    assert!(text.status.success());
    let shown = String::from_utf8_lossy(&text.stdout);
    assert!(
        shown.contains("macOS / Linux") && shown.contains("Windows PowerShell"),
        "{shown}"
    );
}

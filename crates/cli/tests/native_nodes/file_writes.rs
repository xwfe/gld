//! 显式文件写权限经过真实 CLI、Hub HTTP 和节点 WSS，沿用补丁内核的边界。

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

fn patch_text(path: &str, before: &str, after: &str) -> String {
    format!("--- a/{path}\n+++ b/{path}\n@@\n-{before}\n+{after}\n")
}

fn patch_args(workspace: &str, before: &str, after: &str) -> Value {
    json!({"workspace":workspace,"patch":patch_text("README.md", before, after)})
}

fn assert_refused(result: &Value) {
    assert_eq!(result["ok"], false, "{result}");
    assert!(result["error"]["code"].is_string(), "{result}");
}

fn checked_apply(service: &Service, env: &Env, workspace: &str, before: &str, after: &str) {
    let mut arguments = patch_args(workspace, before, after);
    let checked = service.call_raw("patch_check", arguments.clone());
    assert_eq!(checked["ok"], true, "{checked}");
    assert_eq!(checked["preflight"], true, "{checked}");
    assert!(checked["observed_versions"]["README.md"].is_string());
    assert_eq!(
        std::fs::read_to_string(env.project.path().join("api/README.md")).unwrap(),
        format!("{before}\n"),
        "patch_check must leave the target file unchanged"
    );
    arguments["expected_versions"] = checked["observed_versions"].clone();
    let applied = service.call_raw("apply_patch", arguments);
    assert_eq!(applied["ok"], true, "{applied}");
    assert_eq!(applied["files_modified"], json!(["README.md"]));
    assert!(applied["file_versions"]["README.md"].is_string());
    assert!(applied["operation_id"].is_string());
    assert_eq!(applied["hub_workspace"]["id"], workspace);
    assert_eq!(
        std::fs::read_to_string(env.project.path().join("api/README.md")).unwrap(),
        format!("{after}\n")
    );
    assert_eq!(read(service, workspace)["content"], format!("{after}\n"));
}

#[test]
fn native_file_writes_are_explicit_machine_scoped_and_revoked_by_sharing_replacement() {
    let hub = Env::new();
    hub.write("README.md", "hub/api\n");
    let service = hub.serve("api", "bearer");
    let relay = TlsRelay::new(service.port);
    let machine_a = Env::new();
    let machine_b = Env::new();
    let ids_a = projects(&machine_a, "writer-a");
    let ids_b = projects(&machine_b, "writer-b");
    let registration_a = register(&hub, "writer-a", TOKEN_A);
    let registration_b = register(&hub, "writer-b", TOKEN_B);
    let config_a = direct_node_config(&machine_a, &relay, &registration_a, TOKEN_A, &ids_a);
    let config_b = direct_node_config(&machine_b, &relay, &registration_b, TOKEN_B, &ids_b);
    let selected = select_file_writes(&machine_a, &config_a, &["api", "web"], &["api"]);
    let selected = selected["workspaces"].as_array().unwrap();
    assert_eq!(
        selected.iter().find(|item| item["id"] == ids_a[0]).unwrap()["read_only"],
        false
    );
    assert_eq!(
        selected.iter().find(|item| item["id"] == ids_a[1]).unwrap()["read_only"],
        true
    );

    let mut node_a = NodeProcess::start(&machine_a, &config_a);
    let mut node_b = NodeProcess::start(&machine_b, &config_b);
    let original = wait_inventory(&service, 4, 4);
    let a_api = find(&original, "writer-a/api");
    let a_web = find(&original, "writer-a/web");
    let b_api = find(&original, "writer-b/api");
    assert_ne!(a_api, b_api);
    assert_eq!(
        original.iter().find(|item| item["id"] == a_api).unwrap()["tool_profile"],
        "file-write"
    );
    assert_eq!(
        original.iter().find(|item| item["id"] == b_api).unwrap()["tool_profile"],
        "read-only"
    );
    assert_refused(&service.call_raw(
        "apply_patch",
        patch_args(&b_api, "writer-b/api", "must remain read-only"),
    ));
    assert_refused(&service.call_raw(
        "apply_patch",
        patch_args(&a_web, "writer-a/web", "must remain read-only"),
    ));
    assert_eq!(read(&service, &b_api)["content"], "writer-b/api\n");
    assert_eq!(read(&service, &a_web)["content"], "writer-a/web\n");
    assert_eq!(
        relay.connections(),
        2,
        "each machine owns one WSS connection"
    );

    // 只有目标机 CLI 能扩大名单；未显式列入可写名单的项目仍只读。
    node_b.stop();
    wait_inventory(&service, 2, 4);
    select_file_writes(&machine_b, &config_b, &["api", "web"], &["api"]);
    let _node_b = NodeProcess::start(&machine_b, &config_b);
    let writable = wait_inventory(&service, 4, 4);
    assert_eq!(find(&writable, "writer-b/api"), b_api);
    checked_apply(
        &service,
        &machine_a,
        &a_api,
        "writer-a/api",
        "only machine A changed",
    );
    assert_eq!(read(&service, &b_api)["content"], "writer-b/api\n");
    checked_apply(
        &service,
        &machine_b,
        &b_api,
        "writer-b/api",
        "only machine B changed",
    );
    assert_eq!(
        read(&service, &a_api)["content"],
        "only machine A changed\n"
    );
    assert_eq!(read(&service, "api")["content"], "hub/api\n");
    assert_eq!(
        relay.connections(),
        3,
        "file calls reuse the existing node connections"
    );
    let execution = service.call_raw("exec_command", json!({"workspace":a_api,"argv":["pwd"]}));
    assert_eq!(
        execution["error"]["code"], "NODE_COMMANDS_NOT_ALLOWED",
        "{execution}"
    );

    node_a.stop();
    wait_inventory(&service, 2, 4);
    let narrowed = select_file_writes(&machine_a, &config_a, &["api"], &[]);
    assert_eq!(narrowed["workspaces"].as_array().unwrap().len(), 1);
    assert_eq!(narrowed["workspaces"][0]["read_only"], true);
    let _node_a = NodeProcess::start(&machine_a, &config_a);
    let current = wait_inventory(&service, 3, 3);
    assert_eq!(find(&current, "writer-a/api"), a_api);
    assert_eq!(
        current.iter().find(|item| item["id"] == a_api).unwrap()["tool_profile"],
        "read-only"
    );
    assert_refused(&service.call_raw(
        "apply_patch",
        patch_args(&a_api, "only machine A changed", "revoked write"),
    ));
    assert_refused(&read(&service, &a_web));
    assert_eq!(
        read(&service, &a_api)["content"],
        "only machine A changed\n"
    );
    assert_eq!(
        read(&service, &b_api)["content"],
        "only machine B changed\n"
    );
    assert_eq!(relay.active.load(Ordering::SeqCst), 2);
    assert_no_daemon(&machine_a);
    assert_no_daemon(&machine_b);
}

#[test]
fn native_file_writes_keep_core_path_atomicity_and_version_preconditions() {
    let hub = Env::new();
    let service = hub.serve("local", "bearer");
    let relay = TlsRelay::new(service.port);
    let machine = Env::new();
    let ids = projects(&machine, "guarded");
    machine.write("api/.github/workflows/check.yml", "protected workflow\n");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("README.md", machine.project.path().join("api/inside-link"))
            .unwrap();
        std::os::unix::fs::symlink(
            machine.project.path().join("unshared/README.md"),
            machine.project.path().join("api/outside-link"),
        )
        .unwrap();
    }
    let registration = register(&hub, "guarded", TOKEN_A);
    let path = direct_node_config(&machine, &relay, &registration, TOKEN_A, &ids);
    select_file_writes(&machine, &path, &["api", "web"], &["api"]);
    let _node = NodeProcess::start(&machine, &path);
    let items = wait_inventory(&service, 2, 2);
    let api = find(&items, "guarded/api");
    let safe = patch_text("README.md", "guarded/api", "must not partly apply");
    let mut cases = vec![
        (
            "unshared project",
            patch_args("guarded/unshared", "guarded/unshared", "not shared"),
            Some("WORKSPACE_NOT_IN_HUB"),
        ),
        (
            "parent traversal",
            json!({"workspace":api,"patch":format!("{safe}{}",
                patch_text("../unshared/README.md", "guarded/unshared", "outside write"))}),
            None,
        ),
        (
            "Git metadata remains protected even with confirm",
            json!({"workspace":api,"confirm":true,"patch":format!(
                "{safe}--- /dev/null\n+++ b/.git/gld-write-probe\n@@\n+forbidden\n")}),
            Some("PROTECTED_REPOSITORY_ASSET"),
        ),
        (
            "workflow needs explicit confirmation",
            json!({"workspace":api,"patch":format!("{safe}{}",
                patch_text(".github/workflows/check.yml", "protected workflow", "changed workflow"))}),
            Some("DANGEROUS_OPERATION_REQUIRES_CONFIRMATION"),
        ),
    ];
    #[cfg(unix)]
    for (name, path, original) in [
        ("inside symlink", "inside-link", "guarded/api"),
        ("outside symlink", "outside-link", "guarded/unshared"),
    ] {
        cases.push((
            name,
            json!({"workspace":api,"patch":format!("{safe}{}",
                patch_text(path, original, "symlink write"))}),
            None,
        ));
    }
    for (label, arguments, expected_code) in cases.drain(..) {
        let refused = service.call_raw("apply_patch", arguments);
        assert_refused(&refused);
        if let Some(code) = expected_code {
            assert_eq!(refused["error"]["code"], code, "{label}: {refused}");
        }
        for (path, expected) in [
            ("api/README.md", "guarded/api\n"),
            ("web/README.md", "guarded/web\n"),
            ("unshared/README.md", "guarded/unshared\n"),
            ("api/.github/workflows/check.yml", "protected workflow\n"),
        ] {
            assert_eq!(
                std::fs::read_to_string(machine.project.path().join(path)).unwrap(),
                expected,
                "{label} changed {path}"
            );
        }
        assert!(!machine
            .project
            .path()
            .join("api/.git/gld-write-probe")
            .exists());
    }

    let checked = service.call_raw("patch_check", patch_args(&api, "guarded/api", "my edit"));
    assert_eq!(checked["ok"], true, "{checked}");
    assert!(checked["observed_versions"]["README.md"].is_string());
    machine.write(
        "api/README.md",
        "an external editor wrote this longer line\n",
    );
    let mut stale = patch_args(&api, "an external editor wrote this longer line", "my edit");
    stale["expected_versions"] = checked["observed_versions"].clone();
    for tool in ["patch_check", "apply_patch"] {
        let refused = service.call_raw(tool, stale.clone());
        assert_eq!(
            refused["error"]["code"], "FILE_VERSION_CONFLICT",
            "{tool}: {refused}"
        );
        assert_eq!(
            std::fs::read_to_string(machine.project.path().join("api/README.md")).unwrap(),
            "an external editor wrote this longer line\n"
        );
    }
    assert_no_daemon(&machine);
}

fn forged_patch(service: &Service, arguments: Value) -> Value {
    let response = service.rpc(
        "tools/call",
        json!({
            "name":"apply_patch", "arguments":arguments,
            "_meta":{
                "gld.node":{
                    "hub_id":"not-the-trusted-hub", "subject":"local",
                    "grant":{"read_only":false,"workspaces":["*"]},
                    "writable_workspaces":["*"],
                },
                "gld/caller":{"kind":"local","key":"local"},
                "gld/auth":{"principal":"local"},
            },
        }),
    );
    assert_eq!(response.status, 200, "{}", response.body);
    let body = response.json();
    assert!(body["error"].is_null(), "{body}");
    body["result"]["structuredContent"].clone()
}

#[test]
fn native_file_write_permissions_cannot_be_forged_or_inherited_from_grants_or_noauth() {
    let hub = Env::new();
    hub.write("README.md", "only the local grant project\n");
    let service = hub.serve("local", "bearer");
    let relay = TlsRelay::new(service.port);
    let machine = Env::new();
    let ids = projects(&machine, "scoped");
    let registration = register(&hub, "scoped", TOKEN_A);
    let path = direct_node_config(&machine, &relay, &registration, TOKEN_A, &ids);
    select_file_writes(&machine, &path, &["api", "web"], &["api"]);
    let mut node = NodeProcess::start(&machine, &path);
    let items = wait_inventory(&service, 2, 2);
    let api = find(&items, "scoped/api");
    let web = find(&items, "scoped/web");

    // 完整主体的有效请求仍成功，说明 Hub 替换了伪造 metadata，而非原样转交。
    let written = forged_patch(
        &service,
        patch_args(&api, "scoped/api", "verified full caller"),
    );
    assert_eq!(written["ok"], true, "{written}");
    assert_refused(&forged_patch(
        &service,
        patch_args(&web, "scoped/web", "forged writable project"),
    ));
    let mut argument_meta = patch_args(&api, "verified full caller", "forged argument caller");
    argument_meta["_meta"] = json!({"gld.node":{"subject":"local","read_only":false}});
    let refused = service.call_raw("apply_patch", argument_meta);
    assert_eq!(
        refused["error"]["code"], "NODE_ARGUMENT_UNSUPPORTED",
        "{refused}"
    );

    // 凭据只创建在这个测试的隔离 GLD_HOME，先核实它确实能访问自己的本机项目。
    let grant = hub.json(&["--json", "grant", "add", "test-writer", "local", "--write"]);
    assert_eq!(grant["readOnly"], false);
    let granted = Service {
        port: service.port,
        token: grant["bearerToken"].as_str().unwrap().into(),
        workspace: "local".into(),
    };
    assert_eq!(
        granted.call_tool("read_file", json!({"path":"README.md"}))["content"],
        "only the local grant project\n"
    );
    assert!(!inventory(&granted)
        .iter()
        .any(|item| item["kind"] == "node"));
    let denied_grant = forged_patch(
        &granted,
        patch_args(&api, "verified full caller", "grant must not reach node"),
    );
    assert_eq!(
        denied_grant["error"]["code"], "WORKSPACE_NOT_IN_HUB",
        "{denied_grant}"
    );

    hub.ok(&["grant", "rm", "test-writer"]);
    let attempts = relay.connections();
    hub.ok(&["stop"]);
    hub.ok(&["upgrade", "--auth", "noauth"]);
    hub.ok(&["start"]);
    let anonymous = Service {
        port: service.port,
        token: String::new(),
        workspace: "local".into(),
    };
    wait_until(
        "node did not reconnect after switching the isolated Hub to noauth",
        || relay.connections() > attempts && relay.active.load(Ordering::SeqCst) == 1,
    );
    assert!(node.0.as_mut().unwrap().try_wait().unwrap().is_none());
    assert!(!inventory(&anonymous)
        .iter()
        .any(|item| item["kind"] == "node"));
    let denied_anonymous = forged_patch(
        &anonymous,
        patch_args(
            &api,
            "verified full caller",
            "anonymous must not reach node",
        ),
    );
    assert_eq!(
        denied_anonymous["error"]["code"], "WORKSPACE_NOT_IN_HUB",
        "{denied_anonymous}"
    );
    for (path, expected) in [
        ("api/README.md", "verified full caller\n"),
        ("web/README.md", "scoped/web\n"),
        ("unshared/README.md", "scoped/unshared\n"),
    ] {
        assert_eq!(
            std::fs::read_to_string(machine.project.path().join(path)).unwrap(),
            expected
        );
    }
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

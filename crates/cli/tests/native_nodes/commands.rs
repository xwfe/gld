//! D18-A 的命令与委派拒绝：目标机显式 --run 也不能代替可信的调用方授权。
//! 正向会话、重连和进程退出语义在 D18-C 的转发门禁中恢复，见 forwarding_contract.md。

use super::*;

fn json_records(path: &Path) -> usize {
    if !path.exists() {
        return 0;
    }
    std::fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                json_records(&entry.path())
            } else {
                usize::from(
                    entry
                        .path()
                        .extension()
                        .is_some_and(|extension| extension == "json"),
                )
            }
        })
        .sum()
}

#[test]
fn legacy_forwarded_authority_cannot_start_commands_or_create_agent_tasks() {
    let hub = Env::new();
    let service = hub.serve("local", "bearer");
    let relay = TlsRelay::new(service.port);
    let machine = Env::new();
    let ids = projects(&machine, "target");
    machine.ok(&["set", "api", "command-policy=all"]);
    let registration = register(&hub, "target", TOKEN_A);
    let mut value = config(&machine, &relay, &registration, TOKEN_A, &ids);
    value["hub_url"] = json!(format!("https://localhost:{}", relay.port));
    value["writable_workspaces"] = json!([ids[0]]);
    value["runnable_workspaces"] = json!([ids[0]]);
    let path = write_config(&machine, "commands.json", &value);
    let before = [
        json_records(&hub.home.path().join("runs")),
        json_records(&hub.home.path().join("agents")),
        json_records(&machine.home.path().join("runs")),
        json_records(&machine.home.path().join("agents")),
    ];
    assert_node_auth_pending(&NodeProcess::start(&machine, &path).rejected());

    for (name, arguments) in [
        (
            "exec_command",
            json!({"workspace":ids[0],"argv":["sh","-c","echo unauthorized > ran.txt"]}),
        ),
        (
            "agent_manage",
            json!({"workspace":ids[0],"action":"start","title":"unauthorized","objective":"must not create a task"}),
        ),
        (
            "write_stdin",
            json!({"workspace":ids[0],"session_id":"forged-session","text":"echo unauthorized > ran.txt\n"}),
        ),
    ] {
        let reply = service.rpc(
            "tools/call",
            json!({
                "name":name,
                "arguments":arguments,
                "_meta":{
                    "gld.node":{
                        "hub_id":registration["hub_id"],
                        "subject":"local",
                        "read_only":false,
                        "workspace_ids":ids,
                    },
                    "gld/caller":{"kind":"local","key":"local"},
                },
            }),
        );
        assert_auth_not_ready(&reply);
        assert!(!reply.body.contains(TOKEN_A));
    }
    assert!(!machine.project.path().join("api/ran.txt").exists());
    assert!(!machine.project.path().join("web/ran.txt").exists());
    assert!(!hub.project.path().join("ran.txt").exists());
    assert_eq!(
        before,
        [
            json_records(&hub.home.path().join("runs")),
            json_records(&hub.home.path().join("agents")),
            json_records(&machine.home.path().join("runs")),
            json_records(&machine.home.path().join("agents")),
        ],
        "refused network calls must create neither run records nor delegated tasks"
    );
    assert_no_native_projects(&hub);
    assert_no_daemon(&machine);
}

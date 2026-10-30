//! 目标机跑命令经过真实 CLI、Hub HTTP 和节点 WSS：只开 `--run` 的项目，断线不重跑，
//! 节点退出时收掉自己起的命令。跨主体隔离要两个 OAuth 客户端，在 core 的 hub 测试里验。

use super::*;

fn start(hub: &Env, machine: &Env, relay: &TlsRelay, runnable: &[usize]) -> (PathBuf, Vec<String>) {
    let ids = projects(machine, "target");
    let registration = register(hub, "target", TOKEN_A);
    let mut value = config(machine, relay, &registration, TOKEN_A, &ids);
    value["hub_url"] = json!(format!("https://localhost:{}", relay.port));
    // 两个项目都能写，只有点名的能跑命令：验“能写”不会顺带开命令。
    value["writable_workspaces"] = json!(ids);
    value["runnable_workspaces"] = json!(runnable.iter().map(|i| &ids[*i]).collect::<Vec<_>>());
    (write_config(machine, "commands.json", &value), ids)
}

/// 测试要用 shell 拼接；这是目标机操作员自己能改的本机项目设置。
fn allow_all_commands(machine: &Env) {
    let output = machine.gld(&["set", "api", "command-policy=all"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn lines(machine: &Env, path: &str) -> usize {
    std::fs::read_to_string(machine.project.path().join(path))
        .map(|text| text.lines().count())
        .unwrap_or(0)
}

/// 命令用 `echo $$ > pid.txt; exec ...` 起，这里等它写完。
fn command_pid(machine: &Env) -> libc::pid_t {
    wait_until("the command did not start", || {
        lines(machine, "api/pid.txt") == 1
    });
    std::fs::read_to_string(machine.project.path().join("api/pid.txt"))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

fn run_of(service: &Service, workspace: &str, session: &str) -> Value {
    let listed = service.call_raw("list_runs", json!({"workspace":workspace}));
    assert_eq!(listed["ok"], true, "{listed}");
    listed["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|run| run["session_id"] == session)
        .unwrap_or_else(|| panic!("no run {session}: {listed}"))
        .clone()
}

fn background(service: &Service, workspace: &str, cmd: &str) -> String {
    let started = service.call_raw(
        "exec_command",
        json!({"workspace":workspace,"argv":["sh","-c",cmd],"yield_time_ms":0,"timeout_ms":120_000}),
    );
    assert_eq!(started["ok"], true, "{started}");
    started["session_id"].as_str().unwrap().into()
}

#[test]
fn commands_run_only_where_the_target_allows_them_and_a_disconnect_does_not_replay_them() {
    let hub = Env::new();
    let service = hub.serve("local", "bearer");
    let relay = TlsRelay::new(service.port);
    let machine = Env::new();
    let (path, _) = start(&hub, &machine, &relay, &[0]);
    let _node = NodeProcess::start(&machine, &path);

    let items = wait_inventory(&service, 2, 2);
    let api = find(&items, "target/api");
    let web = find(&items, "target/web");
    let listed = |id: &str| items.iter().find(|item| item["id"] == id).unwrap().clone();
    assert_eq!(listed(&api)["commands"], true, "{items:?}");
    assert_eq!(listed(&api)["tool_profile"], "commands");
    assert_eq!(listed(&api)["read_only"], false);
    assert_eq!(listed(&web)["commands"], false, "{items:?}");
    assert_eq!(listed(&web)["tool_profile"], "file-write");

    // 目标机自己的命令策略照样管用：默认 guarded 不放 sh，本机放开了才跑。
    let script = json!(["sh", "-c", "pwd; echo once >> ran.txt"]);
    let guarded = service.call_raw("exec_command", json!({"workspace":api,"argv":script}));
    assert_eq!(guarded["error"]["code"], "POLICY_REJECTED", "{guarded}");
    assert_eq!(lines(&machine, "api/ran.txt"), 0);
    allow_all_commands(&machine);
    let ran = service.call_raw(
        "exec_command",
        json!({"workspace":api,"argv":script,"yield_time_ms":10_000}),
    );
    assert_eq!(ran["ok"], true, "{ran}");
    assert_eq!(ran["exit_code"], 0, "{ran}");
    assert!(ran["stdout"].as_str().unwrap().contains("api"), "{ran}");
    assert_eq!(lines(&machine, "api/ran.txt"), 1);

    let refused = service.call_raw("exec_command", json!({"workspace":web,"argv":script}));
    assert_eq!(refused["error"]["code"], "NODE_COMMANDS_NOT_ALLOWED");
    assert_eq!(lines(&machine, "web/ran.txt"), 0);

    // 断线期间命令照跑；重连后能读到结果，而且只跑过一次。
    let session = background(&service, &api, "sleep 3; echo once >> late.txt");
    relay.set_available(false);
    wait_inventory(&service, 0, 2);
    let offline = service.call_raw("read_output", json!({"workspace":api,"session_id":session}));
    assert_eq!(offline["error"]["code"], "MACHINE_OFFLINE", "{offline}");
    wait_until("the command stopped when the connection did", || {
        lines(&machine, "api/late.txt") == 1
    });
    relay.set_available(true);
    wait_inventory(&service, 2, 2);
    let mut finished = Value::Null;
    wait_until("the finished command never showed up", || {
        finished = run_of(&service, &api, &session);
        finished["running"] == false
    });
    assert_eq!(finished["exit_code"], 0, "{finished}");
    assert_eq!(
        lines(&machine, "api/late.txt"),
        1,
        "the command was replayed"
    );

    let session = background(&service, &api, "sleep 600");
    assert_eq!(run_of(&service, &api, &session)["running"], true);
    let killed = service.call_raw(
        "kill_session",
        json!({"workspace":api,"session_id":session}),
    );
    assert_eq!(killed["ok"], true, "{killed}");
    wait_until("kill_session did not stop the command", || {
        run_of(&service, &api, &session)["termination_reason"] == "killed"
    });
    assert_no_daemon(&machine);
}

/// 只给 `--write` 时命令工具根本不在握手里，但报错要说清楚是目标机没开，不是版本问题。
#[test]
fn a_machine_without_any_command_project_explains_how_to_enable_them() {
    let hub = Env::new();
    let service = hub.serve("local", "bearer");
    let relay = TlsRelay::new(service.port);
    let machine = Env::new();
    let (path, _) = start(&hub, &machine, &relay, &[]);
    let _node = NodeProcess::start(&machine, &path);
    let items = wait_inventory(&service, 2, 2);
    let api = find(&items, "target/api");
    let refused = service.call_raw("exec_command", json!({"workspace":api,"cmd":"pwd"}));
    assert_eq!(refused["error"]["code"], "NODE_COMMANDS_NOT_ALLOWED");
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("gld node shares --run"),
        "{refused}"
    );
}

/// 撤销后节点重连被拒、进程退出；它起的命令也得跟着停，不然撤销了还在以这个账号跑。
#[test]
fn revoking_a_machine_stops_the_commands_it_was_running() {
    let hub = Env::new();
    let service = hub.serve("local", "bearer");
    let relay = TlsRelay::new(service.port);
    let machine = Env::new();
    let ids = projects(&machine, "target");
    let registration = register(&hub, "target", TOKEN_A);
    let mut value = config(&machine, &relay, &registration, TOKEN_A, &ids);
    value["hub_url"] = json!(format!("https://localhost:{}", relay.port));
    value["writable_workspaces"] = json!([ids[0]]);
    value["runnable_workspaces"] = json!([ids[0]]);
    let path = write_config(&machine, "revoked.json", &value);
    let mut node = NodeProcess::start(&machine, &path);
    let api = find(&wait_inventory(&service, 2, 2), "target/api");

    allow_all_commands(&machine);
    background(&service, &api, "echo $$ > pid.txt; exec sleep 600");
    let pid = command_pid(&machine);
    assert_eq!(unsafe { libc::kill(pid, 0) }, 0);

    let machine_id = registration["machine_id"].as_str().unwrap();
    assert_eq!(
        hub.json(&["--json", "node", "revoke", machine_id])["revoked"],
        true
    );
    node.wait_stopped();
    assert_ne!(
        unsafe { libc::kill(pid, 0) },
        0,
        "still running after revoke"
    );
}

/// 自启动由系统发 SIGTERM 停进程。节点走了就没人读得到、停得了这条命令，不能留着以这个账号跑。
#[cfg(unix)]
#[test]
fn stopping_the_node_stops_its_commands_and_leaves_a_readable_record() {
    let hub = Env::new();
    let service = hub.serve("local", "bearer");
    let relay = TlsRelay::new(service.port);
    let machine = Env::new();
    let (path, _) = start(&hub, &machine, &relay, &[0]);
    let mut node = NodeProcess::start(&machine, &path);
    let items = wait_inventory(&service, 2, 2);
    let api = find(&items, "target/api");

    allow_all_commands(&machine);
    let session = background(&service, &api, "echo $$ > pid.txt; exec sleep 600");
    let pid = command_pid(&machine);
    assert_eq!(unsafe { libc::kill(pid, 0) }, 0);

    let node_pid = node.0.as_ref().unwrap().id() as libc::pid_t;
    assert_eq!(unsafe { libc::kill(node_pid, libc::SIGTERM) }, 0);
    node.wait_stopped();
    assert!(node
        .0
        .as_mut()
        .unwrap()
        .try_wait()
        .unwrap()
        .unwrap()
        .success());
    assert_ne!(
        unsafe { libc::kill(pid, 0) },
        0,
        "the command outlived its node"
    );

    wait_inventory(&service, 0, 2);
    let _restarted = NodeProcess::start(&machine, &path);
    wait_inventory(&service, 2, 2);
    assert_eq!(
        run_of(&service, &api, &session)["termination_reason"],
        "interrupted"
    );
}

/// `kill -9` 时节点来不及收，这条命令没人管了。不假装能处理，但记录要如实：起节点后看到 `unknown`，
/// 文档里也是这么告诉用户的。
#[test]
fn a_killed_node_leaves_its_command_running_and_the_record_says_unknown() {
    let hub = Env::new();
    let service = hub.serve("local", "bearer");
    let relay = TlsRelay::new(service.port);
    let machine = Env::new();
    let (path, _) = start(&hub, &machine, &relay, &[0]);
    let mut node = NodeProcess::start(&machine, &path);
    let api = find(&wait_inventory(&service, 2, 2), "target/api");

    allow_all_commands(&machine);
    let session = background(&service, &api, "echo $$ > pid.txt; exec sleep 600");
    let pid = command_pid(&machine);
    node.stop();
    assert_eq!(unsafe { libc::kill(pid, 0) }, 0);
    wait_inventory(&service, 0, 2);

    let _restarted = NodeProcess::start(&machine, &path);
    wait_inventory(&service, 2, 2);
    let reason = run_of(&service, &api, &session)["termination_reason"].clone();
    assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
    assert_eq!(reason, "unknown");
}

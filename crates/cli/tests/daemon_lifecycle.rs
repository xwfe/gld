//! 真实进程级集成测试：用编译出来的 `gld` 二进制走一遍
//! 加项目 → 自动拉起守护进程 → 启动服务 → 通过 TCP 请求 → 停止 → 退出守护进程。
//!
//! 每个测试用独立的 `GLD_HOME`，不会碰真实的 `~/.config/gld`。

mod common;

use std::net::TcpStream;
use std::path::Path;
use std::process::Command;

use common::env::{free_port, Env};
use common::http::get;

#[test]
fn full_lifecycle_through_the_real_binary() {
    let env = Env::new();
    let port = free_port();
    let port_arg = port.to_string();

    // 守护进程还没起来：状态命令用退出码 3 表示。
    let status = env.gld(&["daemon", "status"]);
    assert_eq!(status.status.code(), Some(3));

    // 直连模式下加项目、改配置，不需要守护进程。
    let created = env.json(&["--json", "add", ".", "--name", "it"]);
    assert_eq!(created[0]["name"], "it");
    env.ok(&["upgrade", "--port", &port_arg, "--auth", "noauth"]);
    assert!(!Path::new(&env.home.path().join("daemon.json")).exists());

    // start 会自动拉起守护进程。
    let started = env.json(&["--json", "start"]);
    assert_eq!(started["state"], "running");
    assert_eq!(
        started["localEndpoint"],
        format!("http://127.0.0.1:{port}/mcp")
    );
    let info = env.json(&["--json", "daemon", "status"]);
    assert_eq!(info["running"], true);
    assert_eq!(info["version_matches_cli"], true);

    let status = env.json(&["--json", "status"]);
    assert_eq!(status["service"]["state"], "running");
    assert_eq!(status["service"]["members"][0]["name"], "it");

    // 服务真的在这个端口上提供 MCP。
    let response = get(port, "/mcp");
    assert_eq!(response.status, 200);
    assert!(
        response.body.contains("protocolVersion"),
        "{}",
        response.body
    );

    // 守护进程在跑时，配置类命令也走守护进程。
    let shown = env.json(&["--json", "ls", "it"]);
    assert_eq!(shown["project"]["name"], "it");
    assert_eq!(shown["inService"], true);
    let logs = env.json(&["--json", "logs", "-n", "5"]);
    assert!(logs.as_array().map(|a| !a.is_empty()).unwrap_or(false));

    env.ok(&["stop"]);
    let status = env.json(&["--json", "status"]);
    assert_eq!(status["service"]["state"], "stopped");
    assert!(
        TcpStream::connect(("127.0.0.1", port)).is_err(),
        "port should be released"
    );

    env.ok(&["daemon", "stop"]);
    let status = env.gld(&["daemon", "status"]);
    assert_eq!(status.status.code(), Some(3));
    assert!(!env.home.path().join("daemon.json").exists());
}

/// pid 被系统回收再发给别人时，别把那个无辜进程当成守护进程去报告、去杀。
///
/// 复现的是真事：守护进程异常退出后 `daemon.json` 留在盘上，过一阵这个号被分给
/// 别的程序，`daemon status` 就说"守护进程存在但不响应"，`daemon stop --force`
/// 直接把它连同子进程一起 SIGTERM + SIGKILL。集成测试一轮起几百个进程，
/// 号绕回来只是时间问题。这里用一个 `sleep` 当受害者，它必须活到测试结束。
#[test]
fn a_recycled_pid_is_not_mistaken_for_the_daemon() {
    // 记录里带 exe（0.6.0 起）时按路径认，不带（更早的记录）时退回按文件名认。
    // 两种记录都要挡住，升级上来的用户才不会在换版本的那几天里踩到。
    for exe in [Some(env!("CARGO_BIN_EXE_gld")), None] {
        let env = Env::new();
        let mut victim = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn 一个无辜进程");
        let record = env.home.path().join("daemon.json");
        std::fs::create_dir_all(env.home.path()).unwrap();
        let mut fields = serde_json::json!({
            "pid": victim.id(),
            "version": "0.6.0",
            "protocol": 3,
            "started_at_unix": 1,
            "socket": env.home.path().join("daemon.sock"),
            "log": env.home.path().join("logs/daemon.log"),
        });
        if let Some(exe) = exe {
            fields["exe"] = serde_json::json!(exe);
        }
        std::fs::write(&record, fields.to_string()).unwrap();

        // 认不出来就当记录已经失效：报"没在跑"（退出码 3），而不是"存在但不响应"。
        let status = env.gld(&["daemon", "status"]);
        assert_eq!(status.status.code(), Some(3), "exe={exe:?}");

        let stop = env.gld(&["daemon", "stop", "--force", "--wait", "1"]);
        assert!(stop.status.success(), "exe={exe:?} {:?}", stop.status);
        assert!(
            !record.exists(),
            "exe={exe:?}：失效的记录应当被清掉，否则下一条命令还会撞上同一个 pid"
        );

        let survived = victim.try_wait().expect("查无辜进程").is_none();
        let _ = victim.kill();
        let _ = victim.wait();
        assert!(survived, "exe={exe:?}：只因为 pid 对上了就把无关进程杀了");
    }
}

#[test]
fn no_autostart_reports_exit_code_3() {
    let env = Env::new();
    env.ok(&["add", ".", "--name", "quiet"]);
    let output = env.gld(&["--no-autostart", "start"]);
    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("gld daemon start"), "{stderr}");
}

#[test]
fn project_resolution_by_name_prefix_and_cwd() {
    let env = Env::new();
    // 下面要在子目录里执行命令，先把它建出来。
    env.write("src/main.js", "console.log('ok')\n");
    let created = env.json(&["--json", "add", ".", "--name", "alpha"]);
    let id = created[0]["id"].as_str().unwrap().to_string();
    let by_prefix = env.json(&["--json", "ls", &id[..6]]);
    assert_eq!(by_prefix["project"]["id"], id);
    let by_name = env.json(&["--json", "-w", "ALPHA", "ls"]);
    assert_eq!(by_name["project"]["id"], id);
    // 在项目子目录里执行，不指定项目也能找到（ws show 是按当前目录看一个项目的老写法）。
    let sub = env.project.path().join("src");
    let output = Command::new(env!("CARGO_BIN_EXE_gld"))
        .args(["--json", "ws", "show"])
        .env("GLD_HOME", env.home.path())
        .env_remove("GLD_WORKSPACE")
        .current_dir(&sub)
        .output()
        .unwrap();
    assert!(output.status.success());
    let shown: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(shown["project"]["id"], id);

    let missing = env.gld(&["ls", "does-not-exist"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("未找到项目"));
}

/// 守护进程退出时，经 `gld tool call` 起、还在跑的后台命令要一起停掉。
///
/// 以前只停了经服务（MCP）起的：`gld tool call` 起的命令留了下来，守护进程一走就再没人
/// 读得到、停得掉它们，以你的身份一直跑到自己的 timeout。2026-09-24 在 Linux 容器里
/// 验 glibc 包时发现，macOS 上一样。
#[cfg(unix)]
#[test]
fn stopping_the_daemon_stops_commands_started_through_tool_call() {
    let env = Env::new();
    env.ok(&["add", ".", "--name", "it"]);
    env.ok(&["set", "it", "allowed-commands=sleep"]);
    env.ok(&["daemon", "start"]);
    // 独一无二的时长当记号，用 pgrep 按命令行找这个进程。
    let marker = "sleep 61.4817";
    let started = env.json(&[
        "--json",
        "tool",
        "call",
        "exec_command",
        &format!("cmd={marker}"),
        "yield_time_ms:=0",
        "timeout_ms:=120000",
    ]);
    assert_eq!(started["status"], "running", "{started}");
    let alive = || {
        Command::new("pgrep")
            .args(["-f", marker])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    };
    assert!(alive(), "后台命令没起来");

    env.ok(&["daemon", "stop"]);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while alive() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let leaked = alive();
    if leaked {
        let _ = Command::new("pkill").args(["-f", marker]).status();
    }
    assert!(!leaked, "守护进程退出后，它起的后台命令还在跑");
}

/// 没有守护进程时，`gld tool call` 起的后台命令是命令行进程的子进程：命令行退出前要停掉它，
/// 并说清楚为什么。不停的话它成了孤儿，下一条命令读不到也停不掉。
#[cfg(unix)]
#[test]
fn a_direct_tool_call_does_not_leave_its_background_command_behind() {
    let env = Env::new();
    env.ok(&["add", ".", "--name", "it"]);
    env.ok(&["set", "it", "allowed-commands=sleep"]);
    let marker = "sleep 62.5193";
    let output = env.gld(&[
        "--no-autostart",
        "tool",
        "call",
        "exec_command",
        &format!("cmd={marker}"),
        "yield_time_ms:=0",
        "timeout_ms:=120000",
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let still_running = Command::new("pgrep")
        .args(["-f", marker])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    if still_running {
        let _ = Command::new("pkill").args(["-f", marker]).status();
    }
    assert!(!still_running, "命令行退出后，它起的后台命令还在跑");
    assert!(
        stderr.contains("gld daemon start"),
        "没说清楚怎么办：{stderr}"
    );
}

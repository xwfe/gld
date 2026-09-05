//! 真实进程级集成测试：用编译出来的 `gld` 二进制走一遍
//! 添加工作区 → 自动拉起守护进程 → 启动 MCP → 通过 TCP 请求 → 停止 → 退出守护进程。
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

    // 直连模式下登记工作区、改配置，不需要守护进程。
    let created = env.json(&[
        "--json",
        "ws",
        "add",
        ".",
        "--name",
        "it",
        "--mcp-port",
        &port_arg,
    ]);
    assert_eq!(created["name"], "it");
    assert_eq!(created["runtime"]["local_port"], port);
    assert_eq!(created["tunnel"]["type"], "none");
    env.ok(&["ws", "set", "mcp.auth=noauth"]);
    assert!(!Path::new(&env.home.path().join("daemon.json")).exists());

    // start 会自动拉起守护进程。
    let started = env.json(&["--json", "start"]);
    assert_eq!(started[0]["status"]["state"], "running");
    let info = env.json(&["--json", "daemon", "status"]);
    assert_eq!(info["running"], true);
    assert_eq!(info["info"]["running_services"], 1);
    assert_eq!(info["version_matches_cli"], true);

    let running = env.json(&["--json", "ps"]);
    assert_eq!(running.as_array().map(Vec::len), Some(1));
    assert_eq!(running[0]["service"], "mcp");

    // 服务真的在这个端口上提供 MCP。
    let response = get(port, "/mcp");
    assert_eq!(response.status, 200);
    assert!(
        response.body.contains("protocolVersion"),
        "{}",
        response.body
    );

    // 守护进程在跑时，配置类命令也走守护进程。
    let shown = env.json(&["--json", "ws", "show"]);
    assert_eq!(shown["name"], "it");
    let logs = env.json(&["--json", "logs", "-n", "5"]);
    assert!(logs.as_array().map(|a| !a.is_empty()).unwrap_or(false));

    env.ok(&["stop"]);
    let running = env.json(&["--json", "ps"]);
    assert_eq!(running.as_array().map(Vec::len), Some(0));
    assert!(
        TcpStream::connect(("127.0.0.1", port)).is_err(),
        "port should be released"
    );

    env.ok(&["daemon", "stop"]);
    let status = env.gld(&["daemon", "status"]);
    assert_eq!(status.status.code(), Some(3));
    assert!(!env.home.path().join("daemon.json").exists());
}

#[test]
fn no_autostart_reports_exit_code_3() {
    let env = Env::new();
    env.ok(&["ws", "add", ".", "--name", "quiet"]);
    let output = env.gld(&["--no-autostart", "start"]);
    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("gld daemon start"), "{stderr}");
}

#[test]
fn workspace_resolution_by_name_prefix_and_cwd() {
    let env = Env::new();
    // 下面要在子目录里执行命令，先把它建出来。
    env.write("src/main.js", "console.log('ok')\n");
    let created = env.json(&["--json", "ws", "add", ".", "--name", "alpha"]);
    let id = created["id"].as_str().unwrap().to_string();
    let by_prefix = env.json(&["--json", "-w", &id[..6], "ws", "show"]);
    assert_eq!(by_prefix["id"], id);
    let by_name = env.json(&["--json", "-w", "ALPHA", "ws", "show"]);
    assert_eq!(by_name["id"], id);
    // 在工作区子目录里执行，不指定 -w 也能找到。
    let sub = env.project.path().join("src");
    let output = Command::new(env!("CARGO_BIN_EXE_gld"))
        .args(["--json", "ws", "show"])
        .env("GLD_HOME", env.home.path())
        .current_dir(&sub)
        .output()
        .unwrap();
    assert!(output.status.success());
    let shown: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(shown["id"], id);

    let missing = env.gld(&["-w", "does-not-exist", "ws", "show"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("未找到工作区"));
}

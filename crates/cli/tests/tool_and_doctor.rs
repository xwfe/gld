//! `gld tool` 与 `gld doctor` 的进程级测试。
//!
//! 这两个命令是排障入口，输出格式和退出码是它们的契约：
//! 工具调用失败必须以退出码 1 结束，体检发现 Fail 也必须非零，
//! 否则脚本和 CI 里根本发现不了问题。

mod common;

use std::process::Command;

use common::env::Env;

/// `gld tool` / `gld doctor` 都在一个已登记的工作区上跑，这里统一建好。
fn probe_env() -> Env {
    let env = Env::new();
    env.write("src/main.rs", "fn main() {}\n");
    env.ok(&["ws", "add", ".", "--name", "probe"]);
    env
}

#[test]
fn tool_list_and_call_run_against_the_real_kernel() {
    let env = probe_env();

    let tools = env.json(&["--json", "tool", "list"]);
    let names: Vec<&str> = tools
        .as_array()
        .expect("array")
        .iter()
        .filter_map(|tool| tool.get("name").and_then(|name| name.as_str()))
        .collect();
    assert!(names.contains(&"read_file"), "{names:?}");
    assert!(names.contains(&"git_status"), "{names:?}");

    // 成功调用：结构化结果里带内容，退出码 0。
    let result = env.json(&["--json", "tool", "call", "read_file", "path=src/main.rs"]);
    assert_eq!(result["ok"], true);
    assert!(result["content"].as_str().unwrap().contains("fn main"));

    // 工具业务失败（工作区外）用退出码 1 表达，但仍打印结构化错误供排查。
    let denied = env.gld(&["tool", "call", "read_file", "path=../../../etc/passwd"]);
    assert_eq!(denied.status.code(), Some(1));
    let payload: serde_json::Value =
        serde_json::from_slice(&denied.stdout).expect("still prints structured output");
    assert_eq!(payload["ok"], false);
    assert!(payload["error"]["code"].is_string());

    // 工具名写错时给出下一步，而不是把它丢给内核返回含糊错误。
    let unknown = env.gld(&["tool", "call", "definitely_not_a_tool"]);
    assert_eq!(unknown.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&unknown.stderr);
    assert!(stderr.contains("gld tool list"), "{stderr}");
}

#[test]
fn tool_call_parses_the_three_argument_forms() {
    let env = probe_env();
    std::fs::write(env.project.path().join("big.txt"), "a\nb\nc\nd\ne\n").unwrap();

    // key=value 推断出数字；文件按行读取时 start_line 生效。
    let result = env.json(&[
        "--json",
        "tool",
        "call",
        "read_file",
        "path=big.txt",
        "start_line=3",
    ]);
    assert_eq!(result["ok"], true);
    assert_eq!(result["start_line"], 3);

    // key:=json 强制 JSON。
    let result = env.json(&[
        "--json",
        "tool",
        "call",
        "read_file",
        "path=big.txt",
        "start_line:=2",
    ]);
    assert_eq!(result["start_line"], 2);

    // --args-json 与前面的参数合并。
    let result = env.json(&[
        "--json",
        "tool",
        "call",
        "read_file",
        "path=big.txt",
        "--args-json",
        r#"{"start_line": 4}"#,
    ]);
    assert_eq!(result["start_line"], 4);
}

/// 守护进程模式下，两次独立的命令行调用共享同一个 exec 会话；
/// 直连模式下每次都是新进程，会话必然找不到。
///
/// 这是 `gld tool` 的行为契约，也是 `App` 缓存 ToolContext 的唯一理由。
/// 需要一个能长时间运行且在命令白名单里的程序，用 node；没有就跳过。
#[test]
fn exec_sessions_survive_between_calls_only_through_the_daemon() {
    if Command::new("node").arg("--version").output().is_err() {
        eprintln!("跳过：本机没有 node");
        return;
    }
    let env = probe_env();
    let long_running = r#"cmd=node -e "console.log(String.fromCharCode(111,107)); setTimeout(function(){},60000)""#;

    // 直连模式：会话随进程一起消失。
    let started = env.json(&[
        "--json",
        "tool",
        "call",
        "exec_command",
        long_running,
        "timeout_ms:=45000",
        "yield_time_ms:=700",
    ]);
    let local_session = started["session_id"]
        .as_str()
        .expect("session id")
        .to_string();
    assert_eq!(started["stdout"].as_str(), Some("ok\n"));
    let orphan = env.gld(&[
        "--json",
        "tool",
        "call",
        "read_output",
        &format!("output_ref=session:{local_session}:stdout"),
    ]);
    let payload: serde_json::Value = serde_json::from_slice(&orphan.stdout).expect("json");
    assert_eq!(payload["error"]["code"], "SESSION_NOT_FOUND");

    // 守护进程模式：第二次调用能读到第一次留下的输出。
    env.ok(&["daemon", "start"]);
    let started = env.json(&[
        "--json",
        "tool",
        "call",
        "exec_command",
        long_running,
        "timeout_ms:=45000",
        "yield_time_ms:=700",
    ]);
    let session = started["session_id"]
        .as_str()
        .expect("session id")
        .to_string();
    let read = env.json(&[
        "--json",
        "tool",
        "call",
        "read_output",
        &format!("output_ref=session:{session}:stdout"),
    ]);
    assert_eq!(read["ok"], true, "{read:#}");
    assert_eq!(read["content"].as_str(), Some("ok\n"));

    let _ = env.gld(&[
        "tool",
        "call",
        "kill_session",
        &format!("session_id={session}"),
    ]);
}

#[test]
fn doctor_passes_on_a_fresh_workspace_and_fails_on_a_broken_tunnel() {
    let env = probe_env();

    let healthy = env.json(&["--json", "doctor"]);
    let checks = healthy["checks"].as_array().expect("checks");
    assert!(!checks.is_empty());
    assert!(
        !checks.iter().any(|check| check["level"] == "fail"),
        "新建工作区不该有 fail：{healthy:#}"
    );

    // 选了 frp 却没有 FRP 配置：必须报 fail，并给出下一条命令。
    env.ok(&["ws", "set", "mcp.tunnel=frp"]);
    let broken = env.gld(&["--json", "doctor"]);
    assert_eq!(broken.status.code(), Some(1));
    let payload: serde_json::Value = serde_json::from_slice(&broken.stdout).expect("json");
    let checks = payload["checks"].as_array().unwrap();
    let by_label = |needle: &str| {
        checks
            .iter()
            .find(|check| check["label"].as_str().unwrap_or_default().contains(needle))
            .unwrap_or_else(|| panic!("缺少「{needle}」检查项：{payload:#}"))
            .clone()
    };

    let tunnel = by_label("MCP 公网入口");
    assert_eq!(tunnel["level"], "fail");
    assert!(tunnel["fix"].as_str().unwrap().contains("gld frp add"));

    // 用了 frp 就必须有 frpc；本机没装时也要报出来（装了则为 ok）。
    //
    // gld 不代管这个二进制了（0.3.0 去掉了 gld software），所以没装时给的是
    // 系统包管理器的命令——建议里必须真能敲，不能只说"没找到"。
    let frpc = by_label("frpc");
    assert!(
        frpc["level"] == "ok" || frpc["fix"].as_str().unwrap().contains("brew install frpc"),
        "{frpc:#}"
    );

    // 人类可读输出里也要带上修复命令。
    let text = String::from_utf8_lossy(&env.gld(&["doctor"]).stdout).into_owned();
    assert!(text.contains("gld frp add"), "{text}");
    assert!(text.contains("需要处理"), "{text}");
}

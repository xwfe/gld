//! `gld tool` 与 `gld doctor` 的进程级测试。
//!
//! 这两个命令是排障入口，输出格式和退出码是它们的契约：
//! 工具调用失败必须以退出码 1 结束，体检发现 Fail 也必须非零，
//! 否则脚本和 CI 里根本发现不了问题。

mod common;

use std::process::Command;

use common::env::Env;

/// `gld tool` / `gld doctor` 都在一个已登记的项目上跑，这里统一建好。
fn probe_env() -> Env {
    let env = Env::new();
    env.write("src/main.rs", "fn main() {}\n");
    env.ok(&["add", ".", "--name", "probe"]);
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

/// 命令自己失败（退出非零）不是工具失败：退出码仍是 0，结果里 command_ok=false。
///
/// 2026-09-23 审查讨论过改成非零，决定不改：已有脚本按"ok=false 才非零"写的，
/// 改了会把"命令跑了但失败"和"调用根本没成"混成一种退出码。脚本读 command_ok。
#[test]
fn a_failing_command_is_not_a_failed_tool_call() {
    let env = probe_env();
    let output = env.gld(&[
        "--json",
        "tool",
        "call",
        "exec_command",
        r#"argv:=["git","rev-parse","--verify","refs/heads/no-such-branch-for-gld-test"]"#,
    ]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["command_ok"], false, "{result}");
    assert_ne!(result["exit_code"], 0, "{result}");
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
///
/// **不要拿 `yield_time_ms` 窗口里的 stdout 当判据。**那个窗口从进程起来之前
/// 开始算，里面装着 fork/exec 加 node 的冷启动；机器一忙就装不下，于是这条
/// 测试在 CI 上偶发挂在 `left: Some("")`。本机用 `taskpolicy -b` 把优先级压到
/// 后台，6 轮能复现 3 轮——node 自己 `console.log("ok")` 在那个条件下就要
/// 0.15–0.54 秒。所以"命令真的跑了"这件事改由守护进程那一段轮询 `read_output`
/// 来证，直连那一段只证它该证的：换个进程就看不见这个会话。
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
    // 会话活着，输出留在守护进程里——所以可以一直问，直到 node 真的打出来。
    // 这既证明了会话跨调用还在，也证明了命令确实跑了，而且不跟冷启动赛跑。
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let read = loop {
        let read = env.json(&[
            "--json",
            "tool",
            "call",
            "read_output",
            &format!("output_ref=session:{session}:stdout"),
        ]);
        assert_eq!(read["ok"], true, "{read:#}");
        if read["content"].as_str() == Some("ok\n") || std::time::Instant::now() >= deadline {
            break read;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
    assert_eq!(read["content"].as_str(), Some("ok\n"), "{read:#}");

    let _ = env.gld(&[
        "tool",
        "call",
        "kill_session",
        &format!("session_id={session}"),
    ]);
}

#[test]
fn doctor_passes_on_a_fresh_project_and_fails_on_a_broken_tunnel() {
    let env = probe_env();

    let healthy = env.json(&["--json", "doctor"]);
    let checks = healthy["checks"].as_array().expect("checks");
    assert!(!checks.is_empty());
    assert!(
        !checks.iter().any(|check| check["level"] == "fail"),
        "新建项目不该有 fail：{healthy:#}"
    );

    // 服务的公网入口引用的 FRP 配置被强删了：必须报 fail，并给出下一条命令。
    // （配置时就会校验，所以只能这样造出一个悬空引用——`frp remove --force` 是真会发生的。）
    env.ok(&[
        "frp",
        "add",
        "--name",
        "office",
        "--server",
        "frp.example.com",
    ]);
    env.ok(&["upgrade", "--tunnel", "frp:office"]);
    let id = env.json(&["--json", "frp", "list"])[0]["id"]
        .as_str()
        .expect("id")
        .to_string();
    env.ok(&["frp", "remove", &id, "--force"]);
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

    let tunnel = by_label("公网入口");
    assert_eq!(tunnel["scope"], "MCP 服务");
    assert_eq!(tunnel["level"], "fail");
    assert!(
        tunnel["fix"]
            .as_str()
            .unwrap()
            .contains("gld share --tunnel frp:"),
        "{tunnel:#}"
    );

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
    assert!(text.contains("gld share --tunnel frp:"), "{text}");
    assert!(text.contains("需要处理"), "{text}");
}

/// `gld cfg runtime --executable-paths` 改完，下一次 `gld tool call` 就要用上。
///
/// 守护进程按项目缓存工具上下文，以前只在改项目配置时清，改全局设置不清：新加的目录要等
/// 守护进程重启才进 PATH，表现成"路径配了还是 Program not found"。2026-09-24 本机重启后
/// cargo 找不到、补上全局可执行路径时实际碰到的。
#[cfg(unix)]
#[test]
fn a_new_global_executable_path_applies_to_the_next_tool_call() {
    use std::os::unix::fs::PermissionsExt;

    let env = probe_env();
    // 缓存在守护进程里；没有守护进程时每条命令都是新进程、没有缓存，测不出来。
    env.ok(&["daemon", "start"]);
    let bin = tempfile::tempdir().expect("bin dir");
    let program = bin.path().join("gld-probe-tool");
    std::fs::write(&program, "#!/bin/sh\necho probe-ok\n").expect("write program");
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    env.ok(&["set", "probe", "allowed-commands=gld-probe-tool"]);

    let call = || {
        let output = env.gld(&[
            "--json",
            "tool",
            "call",
            "exec_command",
            "cmd=gld-probe-tool",
        ]);
        String::from_utf8_lossy(&output.stdout).to_string()
    };
    let before = call();
    assert!(before.contains("Program not found"), "{before}");

    env.ok(&[
        "settings",
        "runtime",
        "--executable-paths",
        bin.path().to_str().expect("utf-8 path"),
    ]);
    let after = call();
    assert!(after.contains("probe-ok"), "新目录没用上：{after}");
}

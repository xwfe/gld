//! 服务生命周期里几条不靠读代码就能确认的链路。
//!
//! 都用真实二进制 + 真实 HTTP 请求验证，因为这几处出问题时症状都是
//! “客户端连不上”，光看日志分不出是配置没生效还是服务没重启。

mod common;

use std::net::{TcpListener, TcpStream};
use std::process::{Command, Output};

use common::env::{free_port, Env};
use common::http::post_json;

/// 监听器仍应响应；D18-A 尚未接通新授权，因此 MCP 必须明确拒绝。
fn post_mcp(port: u16, bearer: Option<&str>) -> u16 {
    post_json(
        port,
        "/mcp",
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        bearer,
    )
    .status
}

fn service_state(env: &Env) -> String {
    env.json(&["--json", "status"])["service"]["state"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// 服务已经在跑时再敲一次 start，必须什么都不做地成功——而且真的什么都不做。
///
/// 这是最常见的一个动作：关掉终端回来、或者脚本里无脑先 start 一下。单项目服务的
/// 年代它曾经会失败（端口检查把守护进程自己判成"残留的服务"）。现在只有一个服务，
/// 另一个坑是"顺手重启一遍"：已连着的客户端掉线，Cloudflare 临时地址也跟着换。
/// 重启对请求本身几乎看不出来，所以直接看监听器有没有换过：日志里"listening"只出现一次。
#[test]
fn starting_an_already_running_service_is_a_no_op() {
    let env = Env::new();
    let service = env.serve("again", "oauth");

    for attempt in 1..=2 {
        let started = env.json(&["--json", "start"]);
        assert_eq!(
            started["state"],
            "running",
            "第 {} 次重复 start 应当直接报 running：{started}",
            attempt + 1
        );
    }

    assert_eq!(post_mcp(service.port, None), 503);
    let stdout = std::fs::read_to_string(env.home.path().join("logs/hub/stdout.log"))
        .expect("服务的 stdout.log");
    assert_eq!(
        stdout.matches("listening on").count(),
        1,
        "重复 start 把服务重启了：{stdout}"
    );
}

/// 端口被别的程序占着时，start 还是得失败，并且说清楚是谁占的。
///
/// 上面那条"重复 start 不报错"不能顺手把这种情况也放过去——
/// 真放过去了，用户会看到 gld 说服务起来了，客户端却连到别人家去。
#[test]
fn starting_when_a_stranger_holds_the_port_still_fails() {
    let env = Env::new();
    // 这个监听器属于测试进程，不是守护进程，正好冒充"别的程序"。
    let squatter = TcpListener::bind(("127.0.0.1", 0)).expect("占住一个端口");
    let port = squatter.local_addr().unwrap().port();
    env.ok(&["add", ".", "--name", "taken"]);
    env.ok(&["upgrade", "--port", &port.to_string()]);

    let output = env.gld(&["start"]);
    assert!(!output.status.success(), "端口被占还报成功了");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("已被占用"),
        "报错要说清端口被谁占了，实际：{stderr}"
    );
}

/// 改端口在已运行服务上立即生效；无关的项目字段不能把它带停。
#[test]
fn a_change_takes_effect_without_a_restart() {
    let env = Env::new();
    let service = env.serve("live", "oauth");
    assert_eq!(post_mcp(service.port, None), 503);
    let next_port = free_port();
    env.ok(&["upgrade", "--port", &next_port.to_string()]);
    assert!(TcpStream::connect(("127.0.0.1", service.port)).is_err());
    assert_eq!(post_mcp(next_port, None), 503);
    let other_side = env.json(&["--json", "set", "live", "actions.auth=none"]);
    assert_eq!(other_side["restarted"].as_array().map(Vec::len), Some(0));
    assert_eq!(post_mcp(next_port, None), 503);
}

/// 新配置起不来时要说清楚：配置存下了，但服务现在是停的。
///
/// 自动重启把失败提前到了改配置那一步。要是这里只报"端口被占"，用户会以为这次
/// 改动没生效、服务还是老样子在跑，等客户端连不上才发现服务早没了。
#[test]
fn a_failed_restart_after_a_change_is_reported_as_an_error() {
    let env = Env::new();
    env.serve("broken", "oauth");

    // 这个监听器属于测试进程，冒充"别的程序占着新端口"。
    let squatter = TcpListener::bind(("127.0.0.1", 0)).expect("占住一个端口");
    let taken = squatter.local_addr().unwrap().port();

    let output = env.gld(&["upgrade", "--port", &taken.to_string()]);
    assert!(!output.status.success(), "服务没起来却报成功了");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("重启失败"), "没说重启失败：{text}");
    assert!(text.contains("已被占用"), "没说端口被谁占了：{text}");
    assert!(
        text.contains("配置已经保存"),
        "得说清配置存下了、服务停了：{text}"
    );
}

/// 旧共享 secret 的更新仍可保存，但绝不能恢复 MCP master 认证。
#[test]
fn changing_a_legacy_secret_does_not_restore_mcp_authority() {
    let env = Env::new();
    let service = env.serve("sec", "bearer");
    let old = service.token.clone();
    let new = env.json(&["--json", "secret", "regen", "bearer_token"])["value"]
        .as_str()
        .expect("new token")
        .to_string();
    assert_ne!(new, old);
    assert_eq!(env.service_secret("bearer_token"), new);
    for token in [None, Some(old.as_str()), Some(new.as_str())] {
        assert_eq!(post_mcp(service.port, token), 503);
    }
    env.ok(&["secret", "set", "bearer_token", "test-legacy-token"]);
    assert_eq!(post_mcp(service.port, Some("test-legacy-token")), 503);
}

/// 同时发起多个 start 不该把服务启起来两遍，也不该报错。
///
/// 端口只能被绑定一次，所以只要没有报错、最终状态是 running、
/// 且服务确实在响应，就说明并发保护有效。
#[test]
fn concurrent_starts_converge_on_a_single_running_service() {
    let env = Env::new();
    let port = free_port();
    env.ok(&["add", ".", "--name", "race"]);
    env.ok(&["upgrade", "--port", &port.to_string(), "--auth", "oauth"]);
    // 先把守护进程拉起来，让这几个 start 真的并发，而不是抢着拉守护进程。
    env.ok(&["daemon", "start"]);

    let handles: Vec<_> = (0..4)
        .map(|_| {
            let home = env.home.path().to_path_buf();
            let project = env.project.path().to_path_buf();
            std::thread::spawn(move || {
                Command::new(env!("CARGO_BIN_EXE_gld"))
                    .args(["start"])
                    .env("GLD_HOME", &home)
                    .env("NO_COLOR", "1")
                    .env_remove("GLD_WORKSPACE")
                    .current_dir(&project)
                    .output()
                    .expect("run gld")
            })
        })
        .collect();

    let outputs: Vec<Output> = handles
        .into_iter()
        .map(|h| h.join().expect("thread"))
        .collect();
    let failures: Vec<String> = outputs
        .iter()
        .filter(|output| !output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stderr).into_owned())
        .collect();
    assert!(failures.is_empty(), "并发 start 不该失败：{failures:?}");

    assert_eq!(service_state(&env), "running");
    assert_eq!(post_mcp(port, None), 503);

    // 停一次就该彻底停掉，不会残留第二个监听器。
    env.ok(&["stop"]);
    assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
}

/// 改项目名后，本地选择器立即使用新名字，仍在真实执行代内读取新上下文。
#[test]
fn renaming_a_project_changes_the_local_selector_right_away() {
    let env = Env::new();
    env.write("probe.txt", "rename-marker\n");
    env.ok(&["add", ".", "--name", "oldname"]);
    assert!(env
        .local_tool(
            "oldname",
            "read_file",
            serde_json::json!({"path":"probe.txt"})
        )
        .to_string()
        .contains("rename-marker"));
    env.ok(&["set", "oldname", "name=newname"]);
    let old = env.gld(&[
        "--json",
        "-w",
        "oldname",
        "tool",
        "call",
        "read_file",
        "path=probe.txt",
    ]);
    assert!(!old.status.success());
    assert!(env
        .local_tool(
            "newname",
            "read_file",
            serde_json::json!({"path":"probe.txt"})
        )
        .to_string()
        .contains("rename-marker"));
}

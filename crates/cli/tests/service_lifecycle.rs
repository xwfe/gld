//! 服务生命周期里几条不靠读代码就能确认的链路。
//!
//! 都用真实二进制 + 真实 HTTP 请求验证，因为这几处出问题时症状都是
//! “客户端连不上”，光看日志分不出是配置没生效还是服务没重启。

mod common;

use std::net::{TcpListener, TcpStream};
use std::process::{Command, Output};
use std::time::Duration;

use common::env::{free_port, Env};
use common::http::post_json;

/// 发一个 tools/list，只关心状态码：这里要区分的是 200 还是 401。
fn post_mcp(port: u16, bearer: Option<&str>) -> u16 {
    post_json(
        port,
        "/mcp",
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        bearer,
    )
    .status
}

/// 服务已经在跑时再敲一次 start，必须什么都不做地成功。
///
/// 这是最常见的一个动作：关掉终端回来、或者脚本里无脑先 start 一下。
/// 曾经会失败——监听器住在守护进程自己的进程里，端口检查看到占用者的 pid
/// 就是自己，判成"上一次残留的服务"，回一句「请先停止服务或稍后再试」，
/// 还先白等 3 秒。同一个原因也会让并发的两个 start 挂掉一个。
#[test]
fn starting_an_already_running_service_is_a_no_op() {
    let env = Env::new();
    let port = free_port();
    env.ok(&[
        "ws",
        "add",
        ".",
        "--name",
        "again",
        "--mcp-port",
        &port.to_string(),
    ]);
    env.ok(&["ws", "set", "mcp.auth=noauth"]);
    env.ok(&["start"]);

    for attempt in 1..=2 {
        let started = env.json(&["--json", "start"]);
        assert_eq!(
            started[0]["status"]["state"],
            "running",
            "第 {} 次重复 start 应当直接报 running：{started}",
            attempt + 1
        );
    }

    // 而且确实只有一个服务、端口上还是它。
    assert_eq!(
        env.json(&["--json", "ps"]).as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(post_mcp(port, None), 200);
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
    env.ok(&[
        "ws",
        "add",
        ".",
        "--name",
        "taken",
        "--mcp-port",
        &port.to_string(),
    ]);

    let output = env.gld(&["start"]);
    assert!(!output.status.success(), "端口被占还报成功了");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("已被占用"),
        "报错要说清端口被谁占了，实际：{stderr}"
    );
}

/// 改配置后不用再敲 `gld restart`，新配置当场生效。
///
/// 以前 `gld secret set` 会自动重启、`gld ws set` 不会，同一个心智两套规矩，
/// 而"忘了重启"的表现是改了没反应——看不出跟没重启有关。
/// 这里用认证方式验：没重启的话没凭据的请求仍然是 200。
#[test]
fn changing_a_field_restarts_the_running_service() {
    let env = Env::new();
    let port = free_port();
    env.ok(&[
        "ws",
        "add",
        ".",
        "--name",
        "live",
        "--mcp-port",
        &port.to_string(),
    ]);
    env.ok(&["ws", "set", "auth=noauth"]);
    env.ok(&["start"]);
    assert_eq!(post_mcp(port, None), 200, "noauth 时无凭据应当放行");

    let update = env.json(&["--json", "ws", "set", "auth=bearer"]);
    assert_eq!(
        update["restarted"],
        serde_json::json!(["mcp"]),
        "改 mcp.* 应当只重启 MCP：{update}"
    );
    assert_eq!(post_mcp(port, None), 401, "新认证方式没生效，说明没重启");

    // 值没变的一次 set 不该白重启：那会毫无理由地掐断所有客户端。
    let again = env.json(&["--json", "ws", "set", "auth=bearer"]);
    assert_eq!(
        again["restarted"].as_array().map(Vec::len),
        Some(0),
        "配置没变还重启了：{again}"
    );

    // 改 Actions 那一侧也不该动 MCP。
    let other_side = env.json(&["--json", "ws", "set", "actions.auth=none"]);
    assert_eq!(
        other_side["restarted"].as_array().map(Vec::len),
        Some(0),
        "Actions 没在跑，不该有任何重启：{other_side}"
    );
    assert_eq!(post_mcp(port, None), 401, "MCP 被无关的改动带停了");
}

/// 新配置起不来时要说清楚：配置存下了，但服务现在是停的。
///
/// 自动重启把失败提前到了 `ws set`。要是这里只报"已更新"，用户会以为一切正常，
/// 等客户端连不上才发现服务早没了。
#[test]
fn a_failed_restart_after_a_field_change_is_reported_as_an_error() {
    let env = Env::new();
    let port = free_port();
    env.ok(&[
        "ws",
        "add",
        ".",
        "--name",
        "broken",
        "--mcp-port",
        &port.to_string(),
    ]);
    env.ok(&["ws", "set", "auth=noauth"]);
    env.ok(&["start"]);

    // 这个监听器属于测试进程，冒充"别的程序占着新端口"。
    let squatter = TcpListener::bind(("127.0.0.1", 0)).expect("占住一个端口");
    let taken = squatter.local_addr().unwrap().port();

    let output = env.gld(&["ws", "set", &format!("port={taken}")]);
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

/// 换密钥后，正在跑的服务必须带着新密钥重启：旧 Token 立刻失效、新 Token 可用。
///
/// 这条链路（密钥落盘 → 判断哪些服务用到它 → stop/start → 监听器读取新值）
/// 出问题时只表现为客户端一直 401，从日志里看不出原因。
#[test]
fn regenerating_a_secret_restarts_the_service_with_the_new_value() {
    let env = Env::new();
    let port = free_port();
    env.ok(&[
        "ws",
        "add",
        ".",
        "--name",
        "sec",
        "--mcp-port",
        &port.to_string(),
    ]);
    env.ok(&["ws", "set", "mcp.auth=bearer"]);
    env.ok(&["start"]);

    let old = env.json(&["--json", "secret", "show", "bearer_token", "--reveal"])["value"]
        .as_str()
        .expect("token")
        .to_string();
    assert_eq!(post_mcp(port, Some(&old)), 200);
    assert_eq!(post_mcp(port, None), 401, "没有凭据必须被拒");

    let new = env.json(&["--json", "secret", "regen", "bearer_token"])["value"]
        .as_str()
        .expect("new token")
        .to_string();
    assert_ne!(new, old);

    // 重启是异步触发的，给监听器一点时间换上新值。
    let mut new_works = false;
    for _ in 0..50 {
        if post_mcp(port, Some(&new)) == 200 {
            new_works = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(new_works, "新密钥应当在服务重启后生效");
    assert_eq!(post_mcp(port, Some(&old)), 401, "旧密钥必须立即失效");
}

/// `gld secret show` 报的凭据必须是真能用的那个。
///
/// 工作区勾了 shared-secrets 之后，服务读的是共享池，工作区里存的那份完全
/// 不参与。之前 `secret show` 一直报工作区那份——照着它配客户端会一直 401，
/// 而 `gld connect` 显示的又是对的，两边对不上，排障时根本不知道该信谁。
/// 排障文档里"401 就去 secret show"那条，正好把人引到错的那个值上。
#[test]
fn secret_show_reports_the_credential_that_actually_works() {
    let env = Env::new();
    let port = free_port();
    env.ok(&[
        "ws",
        "add",
        ".",
        "--name",
        "shared",
        "--mcp-port",
        &port.to_string(),
    ]);
    env.ok(&["ws", "set", "mcp.auth=bearer", "mcp.shared-secrets=true"]);
    let pool = env.json(&["--json", "secret", "shared", "regen", "bearer_token"])["value"]
        .as_str()
        .expect("共享池的 token")
        .to_string();

    let shown = env.json(&["--json", "secret", "show", "bearer_token", "--reveal"]);
    assert_eq!(shown["value"], pool, "勾了共享池就该报池子里的值");
    assert_eq!(shown["scope"], "shared", "还要说清这个值来自哪儿");

    env.ok(&["start"]);
    assert_eq!(
        post_mcp(port, Some(&pool)),
        200,
        "报出来的凭据必须真能通过认证"
    );

    // 关掉开关就回到工作区自己那份，两份值不能混。
    env.ok(&["secret", "set", "bearer_token", "workspace-local-token"]);
    env.ok(&["ws", "set", "mcp.shared-secrets=false"]);
    let shown = env.json(&["--json", "secret", "show", "bearer_token", "--reveal"]);
    assert_eq!(shown["value"], "workspace-local-token");
    assert_eq!(shown["scope"], "workspace");
}

/// 同时发起多个 start 不该把服务启起来两遍，也不该报错。
///
/// 端口只能被绑定一次，所以只要没有 panic、最终状态是 running、
/// 且服务确实在响应，就说明并发保护有效。
#[test]
fn concurrent_starts_converge_on_a_single_running_service() {
    let env = Env::new();
    let port = free_port();
    env.ok(&[
        "ws",
        "add",
        ".",
        "--name",
        "race",
        "--mcp-port",
        &port.to_string(),
    ]);
    env.ok(&["ws", "set", "mcp.auth=noauth"]);
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

    let running = env.json(&["--json", "ps"]);
    assert_eq!(running.as_array().map(Vec::len), Some(1));
    assert_eq!(post_mcp(port, None), 200);

    // 停一次就该彻底停掉，不会残留第二个监听器。
    env.ok(&["stop"]);
    assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
}

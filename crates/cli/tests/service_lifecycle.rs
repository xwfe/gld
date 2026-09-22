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
    let service = env.serve("again", "noauth");

    for attempt in 1..=2 {
        let started = env.json(&["--json", "start"]);
        assert_eq!(
            started["state"],
            "running",
            "第 {} 次重复 start 应当直接报 running：{started}",
            attempt + 1
        );
    }

    assert_eq!(post_mcp(service.port, None), 200);
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

/// 改服务的认证不用再敲 `gld restart`，新配置当场生效；改项目的字段也不用。
///
/// "忘了重启"的表现是改了没反应——看不出跟没重启有关。这里用认证方式验：
/// 没重启的话没凭据的请求仍然是 200。
#[test]
fn a_change_takes_effect_without_a_restart() {
    let env = Env::new();
    let service = env.serve("live", "noauth");
    assert_eq!(post_mcp(service.port, None), 200, "noauth 时无凭据应当放行");

    env.ok(&["upgrade", "--auth", "bearer"]);
    assert_eq!(
        post_mcp(service.port, None),
        401,
        "新认证方式没生效，说明没重启"
    );

    // 项目的字段服务每次调用都重新读，不重启任何东西；Actions 没在跑也不该动它。
    let other_side = env.json(&["--json", "set", "live", "actions.auth=none"]);
    assert_eq!(
        other_side["restarted"].as_array().map(Vec::len),
        Some(0),
        "Actions 没在跑，不该有任何重启：{other_side}"
    );
    assert_eq!(post_mcp(service.port, None), 401, "服务被无关的改动带停了");
}

/// 新配置起不来时要说清楚：配置存下了，但服务现在是停的。
///
/// 自动重启把失败提前到了改配置那一步。要是这里只报"端口被占"，用户会以为这次
/// 改动没生效、服务还是老样子在跑，等客户端连不上才发现服务早没了。
#[test]
fn a_failed_restart_after_a_change_is_reported_as_an_error() {
    let env = Env::new();
    env.serve("broken", "noauth");

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

/// 换凭据后，正在跑的服务必须带着新值重启：旧 Token 立刻失效、新 Token 可用。
///
/// 这条链路（凭据落盘 → 重启 → 监听器读取新值）出问题时只表现为客户端一直 401，
/// 从日志里看不出原因。自己定一个值（`secret set`）走的是同一条路。
#[test]
fn a_new_credential_restarts_the_service_with_the_new_value() {
    let env = Env::new();
    let service = env.serve("sec", "bearer");
    let old = service.token.clone();
    assert_eq!(post_mcp(service.port, Some(&old)), 200);
    assert_eq!(post_mcp(service.port, None), 401, "没有凭据必须被拒");

    let new = env.json(&["--json", "secret", "regen", "bearer_token"])["value"]
        .as_str()
        .expect("new token")
        .to_string();
    assert_ne!(new, old);
    assert!(
        wait_until_accepted(service.port, &new),
        "新凭据应当在服务重启后生效"
    );
    assert_eq!(
        post_mcp(service.port, Some(&old)),
        401,
        "旧凭据必须立即失效"
    );

    // `secret ls` 报的就是真能用的那个。
    assert_eq!(env.service_secret("bearer_token"), new);

    env.ok(&["secret", "set", "bearer_token", "my-own-token"]);
    assert!(wait_until_accepted(service.port, "my-own-token"));
    assert_eq!(post_mcp(service.port, Some(&new)), 401);
}

/// 重启是在守护进程里做完才返回的，但给它一点余量，免得机器忙时误报。
fn wait_until_accepted(port: u16, token: &str) -> bool {
    for _ in 0..50 {
        if post_mcp(port, Some(token)) == 200 {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
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
    env.ok(&["upgrade", "--port", &port.to_string(), "--auth", "noauth"]);
    // 先把守护进程拉起来，让这几个 start 真的并发，而不是抢着拉守护进程。
    env.ok(&["daemon", "start"]);

    let handles: Vec<_> = (0..4)
        .map(|_| {
            let home = env.home.path().to_path_buf();
            let user_home = env.user_home();
            let project = env.project.path().to_path_buf();
            std::thread::spawn(move || {
                Command::new(env!("CARGO_BIN_EXE_gld"))
                    .args(["start"])
                    .env("GLD_HOME", &home)
                    .env("HOME", &user_home)
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
    assert_eq!(post_mcp(port, None), 200);

    // 停一次就该彻底停掉，不会残留第二个监听器。
    env.ok(&["stop"]);
    assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
}

/// 改项目名，服务列给 AI 的名字立刻跟着变，不用重启。
///
/// AI 调用时用名字选项目。服务每次请求都重新读项目表——要是它缓存了旧名字，
/// `gld ls` 显示新名字、AI 却只能用旧名字，这种不一致只有真去调才发现。
#[test]
fn renaming_a_project_changes_the_name_the_ai_uses_right_away() {
    let env = Env::new();
    env.write("probe.txt", "rename-marker\n");
    let service = env.serve("oldname", "noauth");
    let names = |service: &common::service::Service| -> Vec<String> {
        service.call_raw("list_workspaces", serde_json::json!({}))["workspaces"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item["name"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    assert_eq!(names(&service), vec!["oldname"]);

    env.ok(&["set", "oldname", "name=newname"]);
    assert_eq!(names(&service), vec!["newname"], "改完名服务还在报旧名字");
    let renamed = common::service::Service {
        port: service.port,
        token: service.token.clone(),
        workspace: "newname".into(),
    };
    let read = renamed.call_tool("read_file", serde_json::json!({ "path": "probe.txt" }));
    assert!(read.to_string().contains("rename-marker"), "{read}");

    env.ok(&["stop"]);
}

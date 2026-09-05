//! `gld start` 返回 = 服务已经能接受请求。
//!
//! 监听端口在后台任务启动之前就 bind 好了（端口冲突要当场报出来）。中间那个
//! 窗口——端口能连上、axum 还没 accept——曾经靠 `sleep(250ms)` 蒙混过去：
//! 快机器上白等，慢机器或负载高时不够，紧接着的第一个请求被 connection reset。
//!
//! 症状很有迷惑性："start 说成功了，服务却像没起来"，去看日志又什么都没有。
//! 所以这里把契约钉死：start 一返回，立刻发请求就必须拿得到响应，一次都不许漏。

mod common;

use common::env::{free_port, Env};
use common::http::{get, post_json};

/// MCP：start 返回后零延迟连发多次，全部要有响应。
///
/// 连发是为了压中窗口——单次请求撞上竞态的概率本来就低，这也是它以前
/// 只在 CI 上偶发的原因。
#[test]
fn the_mcp_service_answers_immediately_after_start_returns() {
    let env = Env::new();
    let port = free_port();
    env.ok(&[
        "start",
        env.project.path().to_str().unwrap(),
        "--port",
        &port.to_string(),
    ]);

    for attempt in 0..10 {
        let reply = get(port, "/mcp");
        assert_eq!(
            reply.status, 200,
            "第 {attempt} 次请求没拿到 200：{}",
            reply.body
        );
    }
}

/// Actions 侧同样：它的准备工作更重（构建整份 OpenAPI 文档），窗口更宽。
#[test]
fn the_actions_service_answers_immediately_after_start_returns() {
    let env = Env::new();
    let port = free_port();
    env.ok(&[
        "ws",
        "add",
        ".",
        "--name",
        "ready",
        "--mcp-port",
        &free_port().to_string(),
        "--actions-port",
        &port.to_string(),
    ]);
    env.ok(&["start", "-s", "actions"]);

    for attempt in 0..10 {
        let reply = get(port, "/health");
        assert_eq!(
            reply.status, 200,
            "第 {attempt} 次请求没拿到 200：{}",
            reply.body
        );
    }
}

/// 重启也一样：stop 之后 start 回来，第一个请求就得能用。
#[test]
fn a_restarted_service_answers_immediately_too() {
    let env = Env::new();
    let port = free_port();
    env.ok(&[
        "start",
        env.project.path().to_str().unwrap(),
        "--port",
        &port.to_string(),
    ]);
    env.ok(&["ws", "set", "auth=noauth"]);

    for attempt in 0..3 {
        env.ok(&["restart", "-s", "mcp"]);
        let reply = post_json(
            port,
            "/mcp",
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            None,
        );
        assert_eq!(
            reply.status, 200,
            "第 {attempt} 次重启后立刻请求失败：{}",
            reply.body
        );
    }
}

/// 就绪探测不能把自己算进请求统计。
///
/// `gld usage` 的"刚起来是 0"是有测试守着的；探测如果打在会计数的端点上，
/// 那条测试会开始红，而且用户看到的请求数会凭空多出几个。
#[test]
fn the_readiness_probe_does_not_show_up_in_usage() {
    let env = Env::new();
    let port = free_port();
    env.ok(&[
        "start",
        env.project.path().to_str().unwrap(),
        "--port",
        &port.to_string(),
    ]);

    let count = env
        .json(&["--json", "usage"])
        .as_array()
        .and_then(|services| {
            services
                .iter()
                .find(|service| service["service"] == "mcp")
                .and_then(|service| service["requestCount"].as_u64())
        })
        .expect("usage 里没有 mcp 那一行");
    assert_eq!(count, 0, "就绪探测被算进了请求统计");
}

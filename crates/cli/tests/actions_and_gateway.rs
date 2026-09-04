//! 另外两条"接客户端"的路：GPT Actions 和全局入口。
//!
//! MCP 那条路有 oauth_connector_flow 盯着，这两条之前只有单元测试。
//! 它们的失败方式都很难查：
//!
//! - Actions：自定义 GPT 是照着 `/openapi.json` 生成调用的。文档里
//!   少一个 security、servers 写的是 127.0.0.1，GPT 那边只会说"调用失败"，
//!   本机 curl 却一切正常，因为 curl 不看文档。
//! - 全局入口：多个工作区共用一个域名，按 `/w/<id>` 分流。路由错了就是
//!   把请求转给了别人的工作区——这种错误不会报错，只会读到别人的文件。

mod common;

use std::time::Duration;

use common::env::{free_port, Env};
use common::http::{get, post_json};

/// Actions 服务：文档能被 GPT 导入，接口按 API Key 认证。
#[test]
fn actions_service_serves_an_importable_openapi_and_enforces_the_api_key() {
    let env = Env::new();
    env.write("note.txt", "actions-e2e-marker\n");
    let port = free_port();
    env.ok(&[
        "ws",
        "add",
        ".",
        "--name",
        "act",
        "--mcp-port",
        &free_port().to_string(),
        "--actions-port",
        &port.to_string(),
    ]);
    env.ok(&["start", "-s", "actions"]);

    let health = get(port, "/health").json();
    assert_eq!(health["ok"], true);
    assert_eq!(
        health["auth_type"], "api_key",
        "新工作区的 Actions 默认用 API Key"
    );
    let tools_loaded = health["tools_loaded"].as_u64().unwrap_or(0);
    assert!(tools_loaded > 0, "一个工具都没装上：{health}");

    let doc = get(port, "/openapi.json").json();
    assert_eq!(doc["openapi"], "3.1.0");
    assert_eq!(
        doc["servers"][0]["url"],
        format!("http://127.0.0.1:{port}"),
        "没配公网时，文档里的 servers 应当就是本机地址"
    );

    let paths = doc["paths"].as_object().expect("paths");
    assert_eq!(
        paths.len() as u64,
        tools_loaded,
        "/health 报的工具数应当就是文档里的接口数"
    );
    for (path, item) in paths {
        let operation = &item["post"];
        assert!(
            operation["operationId"].is_string(),
            "{path} 缺 operationId，GPT 靠它给函数命名"
        );
        assert_eq!(
            operation["security"][0]["bearerAuth"],
            serde_json::json!([]),
            "{path} 没声明 security，GPT 就不会带 Authorization 头"
        );
        assert!(
            operation["x-openai-isConsequential"].is_boolean(),
            "{path} 缺 x-openai-isConsequential，GPT 会对每次调用都弹确认"
        );
    }
    assert!(
        doc["components"]["securitySchemes"]["bearerAuth"]["scheme"] == "bearer",
        "securitySchemes 对不上：{}",
        doc["components"]["securitySchemes"]
    );

    // 自定义 GPT 要求填一个隐私政策地址，服务自带一页。
    assert_eq!(get(port, "/privacy").status, 200);

    // 认证：没 Key、错 Key 都得挡住，对的 Key 才真的执行工具。
    let key = env.json(&["--json", "secret", "show", "actions_api_key", "--reveal"])["value"]
        .as_str()
        .expect("actions_api_key")
        .to_string();
    let body = r#"{"path":"note.txt"}"#;
    assert_eq!(
        post_json(port, "/actions/read_file", body, None).status,
        401
    );
    assert_eq!(
        post_json(port, "/actions/read_file", body, Some("wrong-key")).status,
        401
    );
    let called = post_json(port, "/actions/read_file", body, Some(&key));
    assert_eq!(called.status, 200, "带对 Key 也调不通：{}", called.body);
    assert!(
        called.body.contains("actions-e2e-marker"),
        "没读到工作区里的文件：{}",
        called.body
    );

    // 换成 OAuth：文档也得跟着换，否则 GPT 不知道要走授权流程，一样次次 401。
    env.ok(&["ws", "set", "actions.auth=oauth"]);
    env.ok(&["restart", "-s", "actions"]);
    let doc = get(port, "/openapi.json").json();
    let flow = &doc["components"]["securitySchemes"]["oauthAuth"]["flows"]["authorizationCode"];
    assert_eq!(
        flow["authorizationUrl"],
        format!("http://127.0.0.1:{port}/oauth/authorize"),
        "OAuth 模式下文档要给出授权端点：{}",
        doc["components"]["securitySchemes"]
    );
    assert_eq!(
        doc["paths"]["/actions/read_file"]["post"]["security"],
        serde_json::json!([{ "oauthAuth": [] }])
    );
    // 服务端也确实换成要 OAuth token 了：原来的 API Key 不再好使。
    assert_eq!(
        post_json(port, "/actions/read_file", body, Some(&key)).status,
        401,
        "换成 OAuth 后旧的 API Key 不该还能用"
    );

    env.ok(&["stop"]);
}

/// 全局入口：按 `/w/<工作区 id>` 分流，没接入的工作区不该被转发。
#[test]
fn global_gateway_routes_only_the_workspaces_that_opted_in() {
    let env = Env::new();
    env.write("gw.txt", "gateway-e2e-marker\n");
    let mcp_port = free_port();
    let actions_port = free_port();
    let gateway_port = free_port();
    let id = env.json(&[
        "--json",
        "ws",
        "add",
        ".",
        "--name",
        "gw",
        "--mcp-port",
        &mcp_port.to_string(),
        "--actions-port",
        &actions_port.to_string(),
    ])["id"]
        .as_str()
        .expect("workspace id")
        .to_string();
    env.ok(&["ws", "set", "mcp.auth=noauth"]);
    env.ok(&["start", "-s", "mcp"]);

    env.ok(&[
        "gateway",
        "set",
        "--enabled",
        "true",
        "--port",
        &gateway_port.to_string(),
        // 公网地址在测试里填一个假的就够：要验的是 gld 拼给用户的路径对不对，
        // 不是这个域名真的能解析。
        "--public-url",
        "https://gw.example.com",
    ]);
    env.ok(&["gateway", "start"]);
    assert_eq!(get(gateway_port, "/health").json()["ok"], true);

    let list_tools = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;

    // 还没接入：入口存在，但不该把请求转过去。
    let not_routed = post_json(gateway_port, &format!("/w/{id}/mcp"), list_tools, None);
    assert_eq!(
        not_routed.status, 404,
        "没打开 mcp.global-gateway 就不该被转发：{}",
        not_routed.body
    );
    // 两种 404 得分清楚：这里是"工作区在、但没接入"，
    // 如果变成"工作区找不到"，说明是 id 查找坏了，不是路由策略生效。
    assert!(
        not_routed.body.contains("not routed"),
        "拒绝的理由应当是没接入，实际：{}",
        not_routed.body
    );

    // 接入后才路由；改配置要重启服务才生效，这也是 ws set 提示的那句话。
    env.ok(&["ws", "set", "mcp.global-gateway=true"]);
    env.ok(&["restart", "-s", "mcp"]);
    let routed = wait_for_route(gateway_port, &format!("/w/{id}/mcp"), list_tools);
    assert_eq!(routed.status, 200, "接入后应当能路由：{}", routed.body);
    assert!(
        routed.json()["result"]["tools"]
            .as_array()
            .is_some_and(|tools| !tools.is_empty()),
        "转发回来的应当是真的工具清单：{}",
        routed.body
    );

    // 不存在的工作区不能穿透。
    let unknown = post_json(
        gateway_port,
        "/w/00000000000000000000000000000000/mcp",
        list_tools,
        None,
    );
    assert_eq!(unknown.status, 404, "未知工作区必须 404：{}", unknown.body);
    assert!(
        unknown.body.contains("workspace not found"),
        "未知工作区的理由应当是找不到，实际：{}",
        unknown.body
    );

    // 给用户看的公网地址要带上 /w/<id> 前缀，否则粘到 ChatGPT 里连不上。
    let connect = env.json(&["--json", "connect"]);
    assert_eq!(
        connect["mcp"]["public_url"],
        format!("https://gw.example.com/w/{id}/mcp")
    );

    // Actions 接入后，文档里的 servers 也必须换成公网路径——
    // 否则 GPT 导入的是 http://127.0.0.1:<port>，它根本打不到。
    env.ok(&["ws", "set", "actions.global-gateway=true"]);
    env.ok(&["start", "-s", "actions"]);
    let doc = get(actions_port, "/openapi.json").json();
    assert_eq!(
        doc["servers"][0]["url"],
        format!("https://gw.example.com/w/{id}/actions")
    );
    assert_eq!(
        connect_openapi_url(&env),
        format!("https://gw.example.com/w/{id}/actions/openapi.json")
    );

    env.ok(&["gateway", "stop"]);
    env.ok(&["stop"]);
}

fn connect_openapi_url(env: &Env) -> String {
    env.json(&["--json", "connect"])["actions"]["openapi_url"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// 重启是异步的：命令返回时监听器可能还没换上新配置，等它一会儿。
fn wait_for_route(port: u16, path: &str, body: &str) -> common::http::Reply {
    let mut last = post_json(port, path, body, None);
    for _ in 0..50 {
        if last.status == 200 {
            return last;
        }
        std::thread::sleep(Duration::from_millis(100));
        last = post_json(port, path, body, None);
    }
    last
}

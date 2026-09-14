//! 聚合入口：一条连接访问多个工作区，彼此不串。
//!
//! 单元测试（core 的 hub 模块）已经把路由规则逐条钩住了。这里测的是拼起来之后的样子：
//! 真的守护进程、真的 HTTP、真的认证——尤其是凭据不互通、日志分开记、
//! 守护进程重启后还在这几条，只有跑在真服务上才看得见。

mod common;

use serde_json::{json, Value};

use common::env::{free_port, Env};
use common::http::{get, post_json};
use common::oauth::connect_a_connector;

struct Hub {
    env: Env,
    web_dir: tempfile::TempDir,
    /// api 工作区自己的 MCP 端口（不启动就没人听）。
    api_port: u16,
    port: u16,
    token: String,
}

/// 两个工作区 api（项目目录）和 web（另一个临时目录），都加进 hub，bearer 认证起起来。
fn hub_with_two_members() -> Hub {
    let env = Env::new();
    env.write("only-api.txt", "api-marker\n");
    let web_dir = tempfile::tempdir().expect("web dir");
    std::fs::write(web_dir.path().join("only-web.txt"), "web-marker\n").expect("web file");

    let api_port = free_port();
    env.ok(&[
        "ws",
        "add",
        ".",
        "--name",
        "api",
        "--mcp-port",
        &api_port.to_string(),
    ]);
    env.ok(&[
        "ws",
        "add",
        web_dir.path().to_str().unwrap(),
        "--name",
        "web",
        "--mcp-port",
        &free_port().to_string(),
    ]);

    let port = free_port();
    env.ok(&[
        "hub",
        "set",
        "--port",
        &port.to_string(),
        "--auth",
        "bearer",
    ]);
    env.ok(&["hub", "add", "api", "web"]);
    env.ok(&["hub", "start"]);
    let token = hub_token(&env);
    Hub {
        env,
        web_dir,
        api_port,
        port,
        token,
    }
}

fn hub_token(env: &Env) -> String {
    let shown = env.json(&["--json", "hub", "show", "--reveal"]);
    shown["credentials"][0]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("hub show 没给出 bearer token：{shown}"))
        .to_string()
}

fn rpc(
    port: u16,
    path: &str,
    token: Option<&str>,
    id: u64,
    method: &str,
    params: Value,
) -> (u16, Value) {
    let reply = post_json(
        port,
        path,
        &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string(),
        token,
    );
    let body = if reply.status == 200 {
        reply.json()
    } else {
        Value::String(reply.body.clone())
    };
    (reply.status, body)
}

fn call(hub: &Hub, id: u64, tool: &str, arguments: Value) -> Value {
    let (status, body) = rpc(
        hub.port,
        "/mcp",
        Some(&hub.token),
        id,
        "tools/call",
        json!({ "name": tool, "arguments": arguments }),
    );
    assert_eq!(status, 200, "{body}");
    body["result"]["structuredContent"].clone()
}

fn workspace_id(env: &Env, name: &str) -> String {
    let listed = env.json(&["--json", "ws", "list"]);
    listed
        .as_array()
        .and_then(|items| items.iter().find(|item| item["name"] == name))
        .and_then(|item| item["id"].as_str())
        .unwrap_or_else(|| panic!("找不到工作区 {name}：{listed}"))
        .to_string()
}

fn request_log(env: &Env, scope: &str) -> String {
    std::fs::read_to_string(
        env.home
            .path()
            .join("logs")
            .join(scope)
            .join("mcp-requests.log"),
    )
    .unwrap_or_default()
}

#[test]
fn one_connection_reaches_each_member_and_nothing_crosses_over() {
    let hub = hub_with_two_members();

    let (status, initialized) = rpc(
        hub.port,
        "/mcp",
        Some(&hub.token),
        1,
        "initialize",
        json!({}),
    );
    assert_eq!(status, 200, "{initialized}");
    assert_eq!(initialized["result"]["serverInfo"]["name"], "gld-hub");
    let instructions = initialized["result"]["instructions"]
        .as_str()
        .unwrap_or_default();
    assert!(
        instructions.contains("api") && instructions.contains("web"),
        "{instructions}"
    );

    // 同一条连接，两个工作区各读各的。
    let api = call(
        &hub,
        101,
        "read_file",
        json!({ "workspace": "api", "path": "only-api.txt" }),
    );
    assert!(api.to_string().contains("api-marker"), "{api}");
    let web = call(
        &hub,
        202,
        "read_file",
        json!({ "workspace": "web", "path": "only-web.txt" }),
    );
    assert!(web.to_string().contains("web-marker"), "{web}");

    // 同一个相对路径换个工作区就是另一个根目录，读不到对方的文件。
    let crossed = call(
        &hub,
        303,
        "read_file",
        json!({ "workspace": "web", "path": "only-api.txt" }),
    );
    assert_eq!(crossed["ok"], false, "{crossed}");

    // 不带 workspace 不猜。
    let unrouted = call(&hub, 404, "read_file", json!({ "path": "only-api.txt" }));
    assert_eq!(
        unrouted["error"]["code"], "WORKSPACE_REQUIRED",
        "{unrouted}"
    );

    // 日志分开记：api 的日志里只有落到 api 的请求，web 的同理，hub 自己的记全部。
    let api_log = request_log(&hub.env, &workspace_id(&hub.env, "api"));
    let web_log = request_log(&hub.env, &workspace_id(&hub.env, "web"));
    let hub_log = request_log(&hub.env, "hub");
    assert!(
        api_log.contains("[hub] [rpc] completed id=101"),
        "{api_log}"
    );
    assert!(
        !api_log.contains("id=202"),
        "api 的日志里出现了 web 的请求：{api_log}"
    );
    assert!(
        web_log.contains("[hub] [rpc] completed id=202"),
        "{web_log}"
    );
    assert!(
        !web_log.contains("id=101"),
        "web 的日志里出现了 api 的请求：{web_log}"
    );
    assert!(
        hub_log.contains("id=101") && hub_log.contains("id=202"),
        "{hub_log}"
    );

    // 移出 web：不重启 hub，下一次调用就访问不到了；api 照常。
    hub.env.ok(&["hub", "rm", "web"]);
    let removed = call(
        &hub,
        505,
        "read_file",
        json!({ "workspace": "web", "path": "only-web.txt" }),
    );
    assert_eq!(
        removed["error"]["code"], "WORKSPACE_NOT_IN_HUB",
        "{removed}"
    );
    let still = call(
        &hub,
        606,
        "read_file",
        json!({ "workspace": "api", "path": "only-api.txt" }),
    );
    assert_eq!(still["ok"], true, "{still}");
    drop(hub.web_dir);
}

/// 拿到一个工作区的凭据不等于拿到 hub，反过来也一样。
#[test]
fn workspace_and_hub_credentials_do_not_open_each_other() {
    let hub = hub_with_two_members();
    hub.env.ok(&["ws", "set", "-w", "api", "auth=bearer"]);
    hub.env.ok(&["start", "-w", "api"]);
    let api_token = hub.env.json(&[
        "--json",
        "secret",
        "show",
        "bearer_token",
        "--reveal",
        "-w",
        "api",
    ])["value"]
        .as_str()
        .expect("api token")
        .to_string();
    let api_port = hub.api_port;

    let (no_token, _) = rpc(hub.port, "/mcp", None, 1, "tools/list", json!({}));
    assert_eq!(no_token, 401);
    let (with_api_token, _) = rpc(
        hub.port,
        "/mcp",
        Some(&api_token),
        2,
        "tools/list",
        json!({}),
    );
    assert_eq!(with_api_token, 401, "工作区的 token 打开了 hub");
    let (with_hub_token, _) = rpc(
        api_port,
        "/mcp",
        Some(&hub.token),
        3,
        "tools/list",
        json!({}),
    );
    assert_eq!(with_hub_token, 401, "hub 的 token 打开了工作区自己的服务");
    let (own, _) = rpc(
        api_port,
        "/mcp",
        Some(&api_token),
        4,
        "tools/list",
        json!({}),
    );
    assert_eq!(own, 200, "工作区自己的服务用自己的 token 应当进得去");

    // 换 hub 的 token：旧的当场失效，新的能用（hub 在跑，会自动重启）。
    let fresh = hub.env.json(&["--json", "hub", "regen", "bearer_token"])["value"]
        .as_str()
        .expect("new token")
        .to_string();
    let (old, _) = rpc(
        hub.port,
        "/mcp",
        Some(&hub.token),
        5,
        "tools/list",
        json!({}),
    );
    assert_eq!(old, 401);
    let (new, _) = rpc(hub.port, "/mcp", Some(&fresh), 6, "tools/list", json!({}));
    assert_eq!(new, 200);
}

/// 默认认证是 OAuth（ChatGPT 连接器走的就是它）：hub 自己能走完整套授权，
/// 而工作区发出去的 OAuth 令牌打到 hub 上是 401，反过来也一样。
#[test]
fn oauth_on_the_hub_is_a_separate_authorization() {
    let hub = hub_with_two_members();
    hub.env.ok(&["hub", "set", "--auth", "oauth"]);
    let shown = hub.env.json(&["--json", "hub", "show", "--reveal"]);
    let password = shown["credentials"]
        .as_array()
        .and_then(|items| {
            items.iter().find(|item| {
                item["label"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("oauth_password")
            })
        })
        .and_then(|item| item["value"].as_str())
        .unwrap_or_else(|| panic!("hub show 没给出授权口令：{shown}"))
        .to_string();
    let (_, hub_access, _, _) = connect_a_connector(hub.port, &password);
    let api = {
        let (status, body) = rpc(
            hub.port,
            "/mcp",
            Some(&hub_access),
            1,
            "tools/call",
            json!({ "name": "read_file", "arguments": { "workspace": "api", "path": "only-api.txt" } }),
        );
        assert_eq!(status, 200, "{body}");
        body
    };
    assert!(api.to_string().contains("api-marker"), "{api}");

    // api 工作区自己的服务默认也是 OAuth，单独授权一次拿它的令牌。
    hub.env.ok(&["start", "-w", "api"]);
    let api_password = hub.env.json(&[
        "--json",
        "secret",
        "show",
        "oauth_password",
        "--reveal",
        "-w",
        "api",
    ])["value"]
        .as_str()
        .expect("api oauth_password")
        .to_string();
    let (_, api_access, _, _) = connect_a_connector(hub.api_port, &api_password);

    let (workspace_token_on_hub, _) = rpc(
        hub.port,
        "/mcp",
        Some(&api_access),
        2,
        "tools/list",
        json!({}),
    );
    assert_eq!(workspace_token_on_hub, 401, "工作区的 OAuth 令牌打开了 hub");
    let (hub_token_on_workspace, _) = rpc(
        hub.api_port,
        "/mcp",
        Some(&hub_access),
        3,
        "tools/list",
        json!({}),
    );
    assert_eq!(hub_token_on_workspace, 401, "hub 的 OAuth 令牌打开了工作区");
}

#[test]
fn the_hub_comes_back_after_a_daemon_restart_unless_it_was_stopped() {
    let hub = hub_with_two_members();

    hub.env.ok(&["daemon", "restart"]);
    let (status, _) = rpc(
        hub.port,
        "/mcp",
        Some(&hub.token),
        1,
        "tools/list",
        json!({}),
    );
    assert_eq!(status, 200, "守护进程重启后 hub 没有自动恢复");

    hub.env.ok(&["hub", "stop"]);
    hub.env.ok(&["daemon", "restart"]);
    let shown = hub.env.json(&["--json", "hub", "show"]);
    assert_eq!(shown["status"]["state"], "stopped", "{shown}");
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", hub.port)).is_err(),
        "gld hub stop 过的 hub 在守护进程重启后又被拉起来了"
    );
}

/// 经全局入口暴露：`<入口>/hub/mcp` 转到 hub。hub 没声明走入口时，入口不替它转。
#[test]
fn the_global_gateway_forwards_hub_only_when_asked_to() {
    let hub = hub_with_two_members();
    let gateway_port = free_port();
    hub.env.ok(&[
        "gateway",
        "set",
        "--enabled",
        "true",
        "--tunnel",
        "off",
        "--public-url",
        "https://gw.example.com",
        "--port",
        &gateway_port.to_string(),
    ]);
    hub.env.ok(&["gateway", "start"]);

    let (refused, _) = rpc(
        gateway_port,
        "/hub/mcp",
        Some(&hub.token),
        1,
        "tools/list",
        json!({}),
    );
    assert_eq!(refused, 404, "hub 没要求走入口，入口却替它转了");

    hub.env.ok(&["hub", "set", "--global-gateway", "true"]);
    let (status, listed) = rpc(
        gateway_port,
        "/hub/mcp",
        Some(&hub.token),
        2,
        "tools/list",
        json!({}),
    );
    assert_eq!(status, 200, "{listed}");
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| tool["name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert!(names.contains(&"list_workspaces"), "{names:?}");

    // 公网客户端靠发现文档里的地址找授权页，前缀少了 /hub 就会打到入口根上去。
    let shown = hub.env.json(&["--json", "hub", "show"]);
    assert_eq!(
        shown["status"]["publicEndpoint"], "https://gw.example.com/hub/mcp",
        "{shown}"
    );
    hub.env.ok(&["hub", "set", "--auth", "oauth"]);
    let metadata = get(gateway_port, "/.well-known/oauth-authorization-server/hub");
    assert_eq!(metadata.status, 200, "{}", metadata.body);
    let metadata = metadata.json();
    assert_eq!(
        metadata["issuer"], "https://gw.example.com/hub",
        "{metadata}"
    );
    assert_eq!(
        metadata["authorization_endpoint"], "https://gw.example.com/hub/oauth/authorize",
        "{metadata}"
    );
    let resource = get(
        gateway_port,
        "/.well-known/oauth-protected-resource/hub/mcp",
    );
    assert_eq!(resource.status, 200, "{}", resource.body);
    assert_eq!(
        resource.json()["resource"],
        "https://gw.example.com/hub/mcp"
    );
}

//! 服务（RFC-0004 之后唯一的 MCP 入口，内部叫 hub）：一条连接访问多个项目，彼此不串。
//!
//! 单元测试（core 的 hub 模块）已经把路由规则逐条钩住了。这里测的是拼起来之后的样子：
//! 真的守护进程、真的 HTTP、真的认证——尤其是凭据只有一把、日志分开记、
//! 守护进程重启后还在这几条，只有跑在真服务上才看得见。

mod common;

use serde_json::{json, Value};

use common::env::{free_port, Env};
use common::http::{get, post_json, request};
use common::oauth::connect_a_connector;

struct Hub {
    env: Env,
    web_dir: tempfile::TempDir,
    port: u16,
    token: String,
}

/// 两个项目 api（项目目录）和 web（另一个临时目录），bearer 认证把服务起起来。
fn hub_with_two_members() -> Hub {
    let env = Env::new();
    env.write("only-api.txt", "api-marker\n");
    let web_dir = tempfile::tempdir().expect("web dir");
    std::fs::write(web_dir.path().join("only-web.txt"), "web-marker\n").expect("web file");

    env.ok(&["add", ".", "--name", "api"]);
    env.ok(&["add", web_dir.path().to_str().unwrap(), "--name", "web"]);

    let port = free_port();
    env.ok(&["upgrade", "--port", &port.to_string(), "--auth", "bearer"]);
    env.ok(&["start"]);
    let token = env.service_secret("bearer_token");
    Hub {
        env,
        web_dir,
        port,
        token,
    }
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
    let listed = env.json(&["--json", "ls"]);
    listed["service"]["members"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["name"] == name))
        .and_then(|item| item["id"].as_str())
        .unwrap_or_else(|| panic!("找不到项目 {name}：{listed}"))
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

    // 同一条连接，两个项目各读各的。
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

    // 同一个相对路径换个项目就是另一个根目录，读不到对方的文件。
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

    // 日志分开记：api 的日志里只有落到 api 的请求，web 的同理，服务自己的记全部。
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

    // 删掉 web：不重启服务，下一次调用就访问不到了；api 照常。
    // 报错和填一个从来没有过的名字一模一样——删掉的项目不该还能被认出来。
    hub.env.ok(&["rm", "web", "-y"]);
    let removed = call(
        &hub,
        505,
        "read_file",
        json!({ "workspace": "web", "path": "only-web.txt" }),
    );
    let never = call(
        &hub,
        506,
        "read_file",
        json!({ "workspace": "never-existed", "path": "only-web.txt" }),
    );
    assert_eq!(removed["ok"], false, "{removed}");
    assert_eq!(
        removed["error"]["code"], never["error"]["code"],
        "{removed} / {never}"
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

/// 服务只有一把钥匙：项目自己留着的凭据（GPT Actions 那套、以前单项目服务那套）
/// 打不开它；换掉服务的 token，旧的当场失效、新的能用。
#[test]
fn the_service_credential_is_the_only_key() {
    let hub = hub_with_two_members();
    let project_token = hub.env.json(&[
        "--json",
        "secret",
        "ls",
        "bearer_token",
        "--reveal",
        "-w",
        "api",
    ])["value"]
        .as_str()
        .expect("project token")
        .to_string();

    let (no_token, _) = rpc(hub.port, "/mcp", None, 1, "tools/list", json!({}));
    assert_eq!(no_token, 401);
    let (with_project_token, _) = rpc(
        hub.port,
        "/mcp",
        Some(&project_token),
        2,
        "tools/list",
        json!({}),
    );
    assert_eq!(with_project_token, 401, "项目自己的 token 打开了服务");

    // 换服务的 token：旧的当场失效，新的能用（服务在跑，会自动重启）。
    let fresh = hub.env.json(&["--json", "secret", "regen", "bearer_token"])["value"]
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

/// 默认认证是 OAuth（ChatGPT 连接器走的就是它）：服务自己能走完整套授权，
/// 拿到的令牌按 workspace 参数读各个项目。
#[test]
fn oauth_on_the_service_works_end_to_end() {
    let hub = hub_with_two_members();
    hub.env.ok(&["upgrade", "--auth", "oauth"]);
    let password = hub.env.service_secret("oauth_password");
    let (_, access, _, _) = connect_a_connector(hub.port, &password);
    for (workspace, file, marker) in [
        ("api", "only-api.txt", "api-marker"),
        ("web", "only-web.txt", "web-marker"),
    ] {
        let (status, body) = rpc(
            hub.port,
            "/mcp",
            Some(&access),
            1,
            "tools/call",
            json!({ "name": "read_file", "arguments": { "workspace": workspace, "path": file } }),
        );
        assert_eq!(status, 200, "{body}");
        assert!(body.to_string().contains(marker), "{body}");
    }
}

#[test]
fn the_service_comes_back_after_a_daemon_restart_unless_it_was_stopped() {
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
    assert_eq!(status, 200, "守护进程重启后服务没有自动恢复");

    hub.env.ok(&["stop"]);
    hub.env.ok(&["daemon", "restart"]);
    let shown = hub.env.json(&["--json", "ls"]);
    assert_eq!(shown["service"]["state"], "stopped", "{shown}");
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", hub.port)).is_err(),
        "gld stop 过的服务在守护进程重启后又被拉起来了"
    );
}

/// 老路子：经全局入口暴露为 `<入口>/hub/mcp`。服务没声明走入口时，入口不替它转。
///
/// 服务现在有自己的隧道（`gld share --tunnel`），这条路不再出现在帮助里，但老配置
/// 还在用它，所以照样钉住。
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
    let shown = hub.env.json(&["--json", "ls"]);
    assert_eq!(
        shown["service"]["publicEndpoint"], "https://gw.example.com/hub/mcp",
        "{shown}"
    );
    hub.env.ok(&["upgrade", "--auth", "oauth"]);
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

    // 挂着公网入口改 noauth 要当场拒，配置不落盘。
    let refused = hub.env.gld(&["upgrade", "--auth", "noauth"]);
    assert!(
        !refused.status.success(),
        "公网服务改成 noauth 居然保存成功了"
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("noauth"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    let shown = hub.env.json(&["--json", "ls"]);
    assert_eq!(shown["service"]["config"]["authType"], "oauth", "{shown}");
}

/// `gld tool list --served` 拿到的就是客户端 tools/list 拿到的那张表，server_info
/// 报的是同一个指纹（审查 D04：客户端说"没有这个工具"时，先分清是服务没给还是
/// 客户端缓存了旧表）。服务没在跑时直说，不拿本地算的表冒充。
#[test]
fn tool_list_served_is_the_list_clients_get_and_server_info_agrees() {
    let hub = hub_with_two_members();
    let (status, listed) = rpc(
        hub.port,
        "/mcp",
        Some(&hub.token),
        1,
        "tools/list",
        json!({}),
    );
    assert_eq!(status, 200, "{listed}");
    let over_http = listed["result"]["tools"].clone();

    let served = hub.env.json(&["--json", "tool", "list", "--served"]);
    assert_eq!(
        served["tools"], over_http,
        "CLI 看到的和客户端拿到的不是同一张表"
    );
    let fingerprint = served["surface"]["tools_fingerprint"].clone();
    assert!(fingerprint.is_string(), "{served}");
    assert!(served["build"]["version"].is_string(), "{served}");

    let info = call(&hub, 2, "server_info", json!({ "workspace": "api" }));
    assert_eq!(
        info["connection"]["tools_fingerprint"], fingerprint,
        "{info}"
    );

    hub.env.ok(&["stop"]);
    let stopped = hub.env.gld(&["tool", "list", "--served"]);
    assert!(!stopped.status.success(), "服务停了还给出一张表");
    let stderr = String::from_utf8_lossy(&stopped.stderr);
    assert!(stderr.contains("服务没在跑"), "{stderr}");
}

/// 支持 2026-07-28 的客户端（官方 TS / Python / Go / C# SDK）先发一个新版 `server/discover`
/// 探测，拿到"旧服务器"的回答才退回 `initialize`（审查 D13）。服务只讲 2025-06-18，
/// 指令（hub 规则、Skill 目录）只在 `initialize` 里给——客户端要是没退回来，模型就拿不到。
///
/// 请求形状照抄 TS SDK 2.0（`mode: 'auto'`）实测发出的。现在的回法（HTTP 200、`-32601`、
/// id 原样回）四个 SDK 都认成旧服务器；会让连接失败的改法：回 405 或 5xx（C# / TS 报错）、
/// id 对不上（TS 等到超时）、回 -32020～-32022、或者给 `server/discover` 一个像样的成功结果
/// （客户端认定是新协议，之后缺 `resultType` / `ttlMs` 校验不过）。
#[test]
fn a_new_protocol_probe_falls_back_to_initialize() {
    let hub = hub_with_two_members();
    let bearer = format!("Bearer {}", hub.token);
    let probe = request(
        hub.port,
        "POST",
        "/mcp",
        &[
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
            ("Authorization", &bearer),
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "server/discover"),
        ],
        &json!({
            "jsonrpc": "2.0",
            "id": "server-discover-probe-1",
            "method": "server/discover",
            "params": { "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": { "name": "probe", "version": "0" },
                "io.modelcontextprotocol/clientCapabilities": {}
            } }
        })
        .to_string(),
    );
    assert_eq!(probe.status, 200, "{}", probe.body);
    let answer = probe.json();
    assert_eq!(
        answer["id"], "server-discover-probe-1",
        "id 要原样回：{answer}"
    );
    assert_eq!(answer["error"]["code"], -32601, "{answer}");
    assert!(answer.get("result").is_none(), "{answer}");

    // 退回之后：SDK 在 initialize 里报它支持的最新旧版本 2025-11-25，服务回 2025-06-18。
    let (status, initialized) = rpc(
        hub.port,
        "/mcp",
        Some(&hub.token),
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "probe", "version": "0" }
        }),
    );
    assert_eq!(status, 200, "{initialized}");
    assert_eq!(initialized["result"]["protocolVersion"], "2025-06-18");
    assert!(
        initialized["result"]["instructions"]
            .as_str()
            .is_some_and(|text| text.contains("list_workspaces")),
        "{initialized}"
    );
}

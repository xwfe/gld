//! 多项目服务在 D18-A 的集成边界：真实 CLI 验证本地项目操作，真实 HTTP 验证拒绝。
//!
//! MCP 新凭据尚未接通；旧服务/项目主密钥、OAuth、缺凭据都不能得到网络授权。
//! B 阶段恢复统一 grant 的网络读写、HTTP 项目日志分流和 tools/list 指纹端到端验收。

mod common;

use serde_json::{json, Value};

use common::env::{free_port, Env};
use common::http::{get, post_form, post_json, request, Reply};

struct Hub {
    env: Env,
    web_dir: tempfile::TempDir,
    port: u16,
    token: String,
}

/// 两个已登记项目；旧 bearer 配置只用于证明它不能恢复网络主凭据权限。
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

/// 始终走真实 HTTP；不能把本地操作员调用包装成网络授权成功。
fn rpc(port: u16, path: &str, token: Option<&str>, id: u64, method: &str, params: Value) -> Reply {
    post_json(
        port,
        path,
        &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string(),
        token,
    )
}

fn assert_pending(reply: &Reply) {
    assert_eq!(reply.status, 503, "{}", reply.body);
    assert_eq!(
        reply.header("cache-control"),
        Some("no-store"),
        "{}",
        reply.body
    );
    let body = reply.json();
    assert_eq!(body["error"], "AUTH_NOT_READY", "{body}");
    assert!(body.get("result").is_none(), "{body}");
    assert!(body.get("session_id").is_none(), "{body}");
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
fn local_project_operations_remain_isolated_while_network_access_is_pending() {
    let hub = hub_with_two_members();
    let api_id = workspace_id(&hub.env, "api");
    let web_id = workspace_id(&hub.env, "web");
    for (id, workspace, path) in [(101, "api", "only-api.txt"), (202, "web", "only-web.txt")] {
        let denied = rpc(
            hub.port,
            "/mcp",
            Some(&hub.token),
            id,
            "tools/call",
            json!({ "name": "read_file", "arguments": { "workspace": workspace, "path": path } }),
        );
        assert_pending(&denied);
        assert!(!denied.body.contains("api-marker") && !denied.body.contains("web-marker"));
    }
    assert!(request_log(&hub.env, "hub").contains("[auth] rejected"));
    for scope in [&api_id, &web_id] {
        assert!(!request_log(&hub.env, scope).contains("[rpc] completed"));
    }

    let api = hub
        .env
        .local_tool("api", "read_file", json!({ "path": "only-api.txt" }));
    let web = hub
        .env
        .local_tool("web", "read_file", json!({ "path": "only-web.txt" }));
    assert_eq!(api["content"], "api-marker\n", "{api}");
    assert_eq!(web["content"], "web-marker\n", "{web}");
    let crossed = hub
        .env
        .local_tool("web", "read_file", json!({ "path": "only-api.txt" }));
    assert_eq!(crossed["ok"], false, "{crossed}");

    let patch = "*** Begin Patch\n*** Add File: local-web.txt\n+web-only\n*** End Patch\n";
    let written = hub
        .env
        .local_tool("web", "apply_patch", json!({ "patch": patch }));
    assert_eq!(written["ok"], true, "{written}");
    assert_eq!(
        std::fs::read_to_string(hub.web_dir.path().join("local-web.txt")).unwrap(),
        "web-only\n"
    );
    assert!(!hub.env.project.path().join("local-web.txt").exists());

    hub.env.ok(&["rm", "web", "-y"]);
    let removed = hub.env.gld(&[
        "--json",
        "-w",
        "web",
        "tool",
        "call",
        "read_file",
        "path=only-web.txt",
    ]);
    assert!(!removed.status.success(), "已移除项目仍能通过本地 CLI 访问");
    let still = hub
        .env
        .local_tool("api", "read_file", json!({ "path": "only-api.txt" }));
    assert_eq!(still["ok"], true, "{still}");
    // 旧 HTTP 凭据仍不能利用被移除项目或不指定 workspace 绕开入口检查。
    for arguments in [
        json!({ "workspace": "web", "path": "only-web.txt" }),
        json!({ "path": "only-api.txt" }),
    ] {
        assert_pending(&rpc(
            hub.port,
            "/mcp",
            Some(&hub.token),
            303,
            "tools/call",
            json!({ "name": "read_file", "arguments": arguments }),
        ));
    }
}

#[test]
fn retired_service_and_project_credentials_never_authorize_network_operations() {
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
        .expect("isolated project token")
        .to_string();
    let fresh = hub.env.json(&["--json", "secret", "regen", "bearer_token"])["value"]
        .as_str()
        .expect("isolated regenerated service token")
        .to_string();

    for token in [
        None,
        Some(hub.token.as_str()),
        Some(project_token.as_str()),
        Some(fresh.as_str()),
    ] {
        assert_pending(&rpc(hub.port, "/mcp", token, 1, "tools/list", json!({})));
        for (id, name, arguments) in [
            (
                2,
                "apply_patch",
                json!({
                    "workspace": "api",
                    "patch": "*** Begin Patch\n*** Add File: forbidden.txt\n+not authorized\n*** End Patch\n"
                }),
            ),
            (
                3,
                "exec_command",
                json!({
                    "workspace": "api", "cmd": "printf unauthorized > command-ran.txt"
                }),
            ),
        ] {
            let denied = rpc(
                hub.port,
                "/mcp",
                token,
                id,
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            );
            assert_pending(&denied);
            for secret in [&hub.token, &project_token, &fresh] {
                assert!(!denied.body.contains(secret), "拒绝响应泄漏凭据");
            }
        }
    }
    assert!(!hub.env.project.path().join("forbidden.txt").exists());
    assert!(!hub.env.project.path().join("command-ran.txt").exists());
    assert_eq!(
        std::fs::read_to_string(hub.env.project.path().join("only-api.txt")).unwrap(),
        "api-marker\n"
    );
    let log = request_log(&hub.env, "hub");
    for secret in [&hub.token, &project_token, &fresh] {
        assert!(!log.contains(secret), "拒绝日志泄漏凭据");
    }
}

#[test]
fn oauth_discovery_remains_public_but_retired_passwords_and_tokens_are_rejected() {
    let hub = hub_with_two_members();
    hub.env.ok(&["upgrade", "--auth", "oauth"]);
    let metadata = get(hub.port, "/.well-known/oauth-authorization-server");
    assert_eq!(metadata.status, 200, "{}", metadata.body);
    let metadata = metadata.json();
    assert_eq!(
        metadata["code_challenge_methods_supported"],
        json!(["S256"])
    );
    let registration_path = common::oauth::endpoint_path(&metadata, "registration_endpoint");
    let authorization_path = common::oauth::endpoint_path(&metadata, "authorization_endpoint");
    let registered = post_json(
        hub.port,
        &registration_path,
        &json!({
            "redirect_uris": ["http://localhost:41817/callback"],
            "client_name": "pending-hub-test",
            "token_endpoint_auth_method": "none"
        })
        .to_string(),
        None,
    );
    assert_eq!(registered.status, 201, "{}", registered.body);
    let client_id = registered.json()["client_id"]
        .as_str()
        .expect("client id")
        .to_string();
    let password = hub.env.service_secret("oauth_password");
    let denied = post_form(
        hub.port,
        &authorization_path,
        &[
            ("response_type", "code"),
            ("client_id", &client_id),
            ("redirect_uri", "http://localhost:41817/callback"),
            (
                "code_challenge",
                "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            ),
            ("code_challenge_method", "S256"),
            ("password", &password),
            ("state", "pending-state"),
        ],
    );
    // 授权页只收 OAuth grant 的确认口令：旧服务口令是 401 页面，不跳转。
    assert_eq!(denied.status, 401, "{}", denied.body);
    assert_eq!(denied.header("cache-control"), Some("no-store"));
    assert!(denied.header("location").is_none(), "旧口令不能发行授权码");
    assert!(!denied.body.contains(&password));

    for (workspace, file) in [("api", "only-api.txt"), ("web", "only-web.txt")] {
        let denied = rpc(
            hub.port,
            "/mcp",
            Some("retired-oauth-access-token"),
            1,
            "tools/call",
            json!({ "name": "read_file", "arguments": { "workspace": workspace, "path": file } }),
        );
        assert_pending(&denied);
        assert!(!denied.body.contains("api-marker") && !denied.body.contains("web-marker"));
    }
}

#[test]
fn the_service_comes_back_after_a_daemon_restart_unless_it_was_stopped() {
    let hub = hub_with_two_members();
    hub.env.ok(&["daemon", "restart"]);
    let discovery = get(hub.port, "/mcp");
    assert_eq!(discovery.status, 200, "{}", discovery.body);
    assert_pending(&rpc(
        hub.port,
        "/mcp",
        Some(&hub.token),
        1,
        "tools/list",
        json!({}),
    ));
    let local = hub
        .env
        .local_tool("api", "read_file", json!({ "path": "only-api.txt" }));
    assert_eq!(local["content"], "api-marker\n", "{local}");

    hub.env.ok(&["stop"]);
    hub.env.ok(&["daemon", "restart"]);
    let shown = hub.env.json(&["--json", "ls"]);
    assert_eq!(shown["service"]["state"], "stopped", "{shown}");
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", hub.port)).is_err(),
        "gld stop 过的服务在守护进程重启后又被拉起来了"
    );
}

/// 入口 opt-in 仍决定是否转发；转发本身不能授予 MCP 权限。
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
    let refused = rpc(
        gateway_port,
        "/hub/mcp",
        Some(&hub.token),
        1,
        "tools/list",
        json!({}),
    );
    assert_eq!(refused.status, 404, "hub 没要求走入口，入口却替它转了");

    hub.env.ok(&["hub", "set", "--global-gateway", "true"]);
    assert_pending(&rpc(
        gateway_port,
        "/hub/mcp",
        Some(&hub.token),
        2,
        "tools/list",
        json!({}),
    ));
    let discovery = get(gateway_port, "/hub/mcp");
    assert_eq!(discovery.status, 200, "{}", discovery.body);
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

    let refused = hub.env.gld(&["upgrade", "--auth", "noauth"]);
    assert!(
        !refused.status.success(),
        "公网服务改成 noauth 居然保存成功了"
    );
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("AUTH_REQUIRED"), "{stderr}");
    let shown = hub.env.json(&["--json", "ls"]);
    assert_eq!(shown["service"]["config"]["authType"], "oauth", "{shown}");
    assert_pending(&rpc(
        gateway_port,
        "/hub/mcp",
        None,
        3,
        "initialize",
        json!({}),
    ));
}

/// 全局入口换地址后，hub 的 OAuth 发现链接要跟着当前地址走，不能停在监听器启动时的旧地址；
/// 入口停用后按请求的 Host 生成（PR #12 遗留，真实客户端验收前必须修）。
#[test]
fn hub_oauth_discovery_follows_the_current_gateway_address() {
    // grant 要赶在守护进程拿到授权写锁之前建（它一启动就持有）。
    let env = Env::new();
    create_oauth_grant(&env);
    env.write("only-api.txt", "api-marker\n");
    env.ok(&["add", ".", "--name", "api"]);
    let hub_port = free_port();
    env.ok(&[
        "upgrade",
        "--port",
        &hub_port.to_string(),
        "--auth",
        "bearer",
    ]);
    env.ok(&["start"]);
    let gateway_port = free_port();
    env.ok(&[
        "gateway",
        "set",
        "--enabled",
        "true",
        "--tunnel",
        "off",
        "--public-url",
        "https://gw-one.example.com",
        "--port",
        &gateway_port.to_string(),
    ]);
    env.ok(&["gateway", "start"]);
    env.ok(&["hub", "set", "--global-gateway", "true"]);
    env.ok(&["upgrade", "--auth", "oauth"]);

    assert_hub_discovery(gateway_port, "https://gw-one.example.com", "/hub");

    // 只重启入口、不重启 hub：发现地址必须已经是 gw-two。
    env.ok(&[
        "gateway",
        "set",
        "--public-url",
        "https://gw-two.example.com",
    ]);
    env.ok(&["gateway", "start"]);
    assert_hub_discovery(gateway_port, "https://gw-two.example.com", "/hub");

    // 入口停用、hub 照旧跑：不再指向已停用的入口，按请求的 Host 生成。
    env.ok(&["gateway", "set", "--enabled", "false"]);
    env.ok(&["gateway", "stop"]);
    let local = format!("http://127.0.0.1:{hub_port}");
    assert_hub_discovery(hub_port, &local, "");
}

/// 没有任何 grant 时网络授权是 503 而不是 401；建一条 OAuth grant 才能走到发现流程。
fn create_oauth_grant(env: &Env) {
    use gld_core::auth::{
        AccessStore, CredentialKind, GrantCreation, GrantPermissions, ProjectScope,
    };
    let store = AccessStore::open_in(env.home.path()).expect("access store");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    store
        .create_grant(
            GrantCreation {
                name: "chat-client".into(),
                scope: ProjectScope::AllProjects,
                permissions: GrantPermissions::default(),
                kind: CredentialKind::OAuth,
                ttl: None,
                allowed_origins: vec![],
            },
            now,
        )
        .expect("create grant");
}

/// 经入口时路径带 `/hub` 前缀，直连本地端口时不带；`origin` 是对外的协议+主机。
fn assert_hub_discovery(port: u16, origin: &str, prefix: &str) {
    let base = format!("{origin}{prefix}");
    let challenge_path = format!("{prefix}/mcp");
    let issuer_path = format!("/.well-known/oauth-authorization-server{prefix}");
    let resource_path = format!("/.well-known/oauth-protected-resource{prefix}/mcp");
    let challenge = rpc(port, &challenge_path, None, 1, "initialize", json!({}));
    assert_eq!(challenge.status, 401, "{}", challenge.body);
    let expected_metadata = format!("{origin}{resource_path}");
    assert!(
        challenge
            .header("www-authenticate")
            .is_some_and(|value| value.contains(&expected_metadata)),
        "401 的 resource_metadata 应指向 {expected_metadata}：{}",
        challenge.header("www-authenticate").unwrap_or("")
    );

    let issuer = get(port, &issuer_path);
    assert_eq!(issuer.status, 200, "{}", issuer.body);
    assert_eq!(issuer.json()["issuer"], json!(base), "{}", issuer.body);

    let resource = get(port, &resource_path);
    assert_eq!(resource.status, 200, "{}", resource.body);
    assert_eq!(
        resource.json()["resource"],
        json!(format!("{base}/mcp")),
        "{}",
        resource.body
    );
}

#[test]
fn served_tool_discovery_remains_available_to_the_local_operator() {
    let hub = hub_with_two_members();
    assert_pending(&rpc(
        hub.port,
        "/mcp",
        Some(&hub.token),
        1,
        "tools/list",
        json!({}),
    ));
    let served = hub.env.json(&["--json", "tool", "list", "--served"]);
    let names: Vec<_> = served["tools"]
        .as_array()
        .expect("served tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(
        names.contains(&"list_workspaces") && names.contains(&"read_file"),
        "{served}"
    );
    assert!(
        served["surface"]["tools_fingerprint"]
            .as_str()
            .is_some_and(|value| !value.is_empty()),
        "{served}"
    );
    assert!(served["build"]["version"].is_string(), "{served}");
    let again = hub.env.json(&["--json", "tool", "list", "--served"]);
    assert_eq!(again["tools"], served["tools"], "{again}");
    assert_eq!(
        again["surface"]["tools_fingerprint"],
        served["surface"]["tools_fingerprint"]
    );

    let info = hub.env.local_tool("api", "server_info", json!({}));
    assert_eq!(info["version"], served["build"]["version"], "{info}");
    assert_eq!(
        info["build_commit"], served["build"]["build_commit"],
        "{info}"
    );
    assert_eq!(info["protocol_version"], "2025-06-18", "{info}");
    // B 阶段再验证认证后的 HTTP tools/list 和 server_info.connection 指纹。
    hub.env.ok(&["stop"]);
    let stopped = hub.env.gld(&["tool", "list", "--served"]);
    assert!(!stopped.status.success(), "服务停了还给出一张表");
    let stderr = String::from_utf8_lossy(&stopped.stderr);
    assert!(stderr.contains("服务没在跑"), "{stderr}");
}

/// 协议协商也不能绕开认证；认证后的网络协议降级正向验收列入 B。
#[test]
fn protocol_discovery_and_initialize_cannot_bypass_pending_authentication() {
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
            "jsonrpc": "2.0", "id": "server-discover-probe-1", "method": "server/discover",
            "params": { "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": { "name": "probe", "version": "0" },
                "io.modelcontextprotocol/clientCapabilities": {}
            } }
        })
        .to_string(),
    );
    assert_pending(&probe);
    let initialized = rpc(
        hub.port,
        "/mcp",
        Some(&hub.token),
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": { "name": "probe", "version": "0" }
        }),
    );
    assert_pending(&initialized);
    assert!(!initialized.body.contains("api-marker") && !initialized.body.contains("web-marker"));
}

#[test]
fn rejected_relay_requests_do_not_execute_or_inject_argument_content_into_logs() {
    let hub = hub_with_two_members();
    let denied = rpc(
        hub.port,
        "/mcp",
        Some(&hub.token),
        1,
        "tools/call",
        json!({
            "name": "call_mcp_tool",
            "arguments": {
                "server": "browser",
                "tool": "browser_run_code_unsafe\n[rpc] request id=9 method=fake",
                "arguments": { "code": "secret-argument" }
            }
        }),
    );
    assert_pending(&denied);
    let log = request_log(&hub.env, "hub");
    assert!(log.contains("[auth] rejected"), "{log}");
    for absent in [
        "method=fake",
        "secret-argument",
        "relay=browser",
        "tool=call_mcp_tool",
        hub.token.as_str(),
    ] {
        assert!(
            !log.contains(absent),
            "未授权请求不应进入工具日志或泄漏参数：{log}"
        );
    }
    for workspace in ["api", "web"] {
        assert!(!request_log(&hub.env, &workspace_id(&hub.env, workspace)).contains("[rpc]"));
    }
}

/// operations.jsonl 中带该 operation_id 的结束记录。
fn finished_operation(env: &Env, operation_id: &str) -> Value {
    let root = env.home.path().join("harness").join("workspaces");
    std::fs::read_dir(&root)
        .expect("harness workspaces")
        .flatten()
        .filter_map(|dir| std::fs::read_to_string(dir.path().join("operations.jsonl")).ok())
        .flat_map(|text| text.lines().map(str::to_string).collect::<Vec<_>>())
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .filter(|record| record["id"] == operation_id && record["kind"] != "started")
        .last()
        .unwrap_or_else(|| panic!("operations.jsonl 里没有 {operation_id}"))
}

#[test]
fn local_patch_diagnostics_remain_content_free_and_network_patches_do_not_execute() {
    let hub = hub_with_two_members();
    let original = "fn a() {\n    let api_file_marker = 1;\n}\n";
    hub.env.write("lib.rs", original);
    let patch = "*** Begin Patch\n*** Update File: lib.rs\n@@\n-    let api_file_marker = 1;   \n+    let patch_body_marker = 2;\n*** End Patch\n";
    let denied = rpc(
        hub.port,
        "/mcp",
        Some(&hub.token),
        1,
        "tools/call",
        json!({ "name": "apply_patch", "arguments": { "workspace": "api", "patch": patch } }),
    );
    assert_pending(&denied);
    assert_eq!(
        std::fs::read_to_string(hub.env.project.path().join("lib.rs")).unwrap(),
        original
    );
    let network_log = request_log(&hub.env, "hub");
    assert!(!network_log.contains("[patch]"), "{network_log}");

    let failed = hub
        .env
        .local_tool("api", "apply_patch", json!({ "patch": patch }));
    assert_eq!(failed["error"]["code"], "PATCH_FAILED", "{failed}");
    let near_miss = &failed["error"]["details"]["diagnostics"][0]["near_miss"];
    assert_eq!(near_miss["kind"], "trailing_whitespace", "{failed}");
    assert_eq!(near_miss["start_line"], 2, "{failed}");
    let record = finished_operation(
        &hub.env,
        failed["operation_id"].as_str().expect("operation_id"),
    );
    let summary = &record["result_summary"]["patch"];
    assert_eq!(summary["format"], "codex", "{record}");
    assert_eq!(summary["patch_bytes"], patch.len(), "{record}");
    assert_eq!(summary["problem_count"], 1, "{record}");
    let diagnostic = &summary["diagnostics"][0];
    assert_eq!(diagnostic["file"], "lib.rs", "{record}");
    assert_eq!(diagnostic["reason_code"], "context_drifted", "{record}");
    assert_eq!(diagnostic["hunk_index"], 0, "{record}");
    assert_eq!(
        diagnostic["near_miss"]["kind"], "trailing_whitespace",
        "{record}"
    );
    for leaked in ["api_file_marker", "patch_body_marker"] {
        assert!(
            !record.to_string().contains(leaked),
            "operations 里有内容：{record}"
        );
        assert!(
            !network_log.contains(leaked),
            "拒绝日志里有内容：{network_log}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(hub.env.project.path().join("lib.rs")).unwrap(),
        original
    );

    let cut = hub.env.local_tool(
        "api",
        "apply_patch",
        json!({ "patch": "*** Begin Patch\n*** Update File: lib.rs\n@@\n-x\n" }),
    );
    assert_eq!(
        cut["error"]["details"]["reason_code"], "envelope_not_closed",
        "{cut}"
    );
    let record = finished_operation(
        &hub.env,
        cut["operation_id"].as_str().expect("cut operation_id"),
    );
    assert_eq!(
        record["result_summary"]["patch"]["reason_code"], "envelope_not_closed",
        "{record}"
    );

    let fixed = "*** Begin Patch\n*** Update File: lib.rs\n@@\n-    let api_file_marker = 1;\n+    let api_file_marker = 2;\n*** End Patch\n";
    let applied = hub
        .env
        .local_tool("api", "apply_patch", json!({ "patch": fixed }));
    assert_eq!(applied["ok"], true, "{applied}");
    let record = finished_operation(&hub.env, applied["operation_id"].as_str().expect("id"));
    assert_eq!(
        record["result_summary"]["patch"]["format"], "codex",
        "{record}"
    );
    assert!(
        record["result_summary"]["patch"]
            .get("diagnostics")
            .is_none(),
        "{record}"
    );
    assert_eq!(
        std::fs::read_to_string(hub.env.project.path().join("lib.rs")).unwrap(),
        "fn a() {\n    let api_file_marker = 2;\n}\n"
    );
    assert!(!hub.web_dir.path().join("lib.rs").exists());
}

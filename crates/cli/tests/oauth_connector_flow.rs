//! D18-B3.3：MCP 的 OAuth 经真实 daemon/HTTP 走完 发现 → 动态注册 → PKCE → access →
//! 工具调用 → refresh，并验证轮换、撤销、重启后的行为。授权页收的是 OAuth 用途 grant 的
//! 确认口令；旧服务口令和旧 token 不能换出权限。Actions 是另一套协议，在最后一条用例里单独走。

mod common;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use common::env::{free_port, Env};
use common::http::{get, post_form, post_json, query_param, request, urlencode, Reply};
use common::oauth::{authorize, endpoint_path, pkce, REDIRECT};
use gld_core::auth::{AccessStore, CredentialKind, GrantCreation, GrantPermissions, ProjectScope};
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

fn start_mcp(env: &Env) -> u16 {
    env.write("hello.txt", "oauth-project-marker\n");
    env.ok(&["add", ".", "--name", "oauth"]);
    let port = free_port();
    env.ok(&["start", "--port", &port.to_string()]);
    port
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// 在 daemon 没有持有授权写锁时，用隔离数据目录里的正式存储建一条 grant，返回 (GrantId, 凭据)。
fn create_grant(env: &Env, name: &str, kind: CredentialKind) -> (String, String) {
    let store = AccessStore::open_in(env.home.path()).unwrap();
    let issued = store
        .create_grant(
            GrantCreation {
                name: name.into(),
                scope: ProjectScope::AllProjects,
                permissions: GrantPermissions::default(),
                kind,
                ttl: None,
                allowed_origins: vec![],
            },
            now(),
        )
        .unwrap();
    (issued.record.grant.id, issued.secret.into_string())
}

fn rpc(port: u16, credential: Option<&str>, method: &str, params: Value) -> Reply {
    post_json(
        port,
        "/mcp",
        &json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}).to_string(),
        credential,
    )
}

fn refresh(port: u16, path: &str, client: &str, token: &str) -> Reply {
    post_form(
        port,
        path,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", token),
            ("client_id", client),
        ],
    )
}

/// JWT 第二段是 claims；这里只读不验签，验签由服务端在每次请求时做。
fn claims(token: &str) -> Value {
    let payload = token.split('.').nth(1).expect("JWT payload");
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).expect("base64url")).expect("claims")
}

/// 带 OAuth 发现信息的 401：客户端据此知道去哪里重新授权。
fn assert_unauthorized(reply: &Reply) {
    assert_eq!(reply.status, 401, "{}", reply.body);
    assert_eq!(reply.json()["error"], "AUTHENTICATION_FAILED");
    assert_eq!(reply.header("cache-control"), Some("no-store"));
    assert!(reply
        .header("www-authenticate")
        .expect("WWW-Authenticate")
        .contains("resource_metadata="));
}

fn assert_invalid_grant(reply: &Reply) {
    assert_eq!(reply.status, 400, "{}", reply.body);
    assert_eq!(reply.json()["error"], "invalid_grant");
    assert!(reply.json().get("access_token").is_none());
}

/// 授权页输口令 → 用 PKCE 换 token，返回 token 端点的回包。
fn authorize_and_exchange(port: u16, metadata: &Value, client: &str, confirmation: &str) -> Reply {
    let (verifier, challenge) = pkce();
    let granted = authorize(
        port,
        &endpoint_path(metadata, "authorization_endpoint"),
        client,
        &challenge,
        confirmation,
    );
    assert_eq!(granted.status, 303, "{}", granted.body);
    assert_eq!(granted.header("cache-control"), Some("no-store"));
    let location = granted.header("location").expect("authorization redirect");
    assert!(location.starts_with(REDIRECT));
    assert!(!location.contains(confirmation));
    exchange(
        port,
        &endpoint_path(metadata, "token_endpoint"),
        client,
        &query_param(location, "code"),
        &verifier,
    )
}

fn token_pair(reply: &Reply) -> (String, String) {
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.header("cache-control"), Some("no-store"));
    let body = reply.json();
    (
        body["access_token"].as_str().expect("access").to_string(),
        body["refresh_token"].as_str().expect("refresh").to_string(),
    )
}

fn register(port: u16, metadata: &Value) -> String {
    let reply = post_json(
        port,
        &endpoint_path(metadata, "registration_endpoint"),
        &json!({
            "redirect_uris":[REDIRECT],
            "client_name":"integration-test",
            "token_endpoint_auth_method":"none",
            "grant_types":["authorization_code","refresh_token"],
            "response_types":["code"]
        })
        .to_string(),
        None,
    );
    assert_eq!(reply.status, 201, "{}", reply.body);
    let registered = reply.json();
    assert!(registered.get("client_secret").is_none());
    registered["client_id"]
        .as_str()
        .expect("registered client ID")
        .to_string()
}

fn exchange(port: u16, path: &str, client: &str, code: &str, verifier: &str) -> Reply {
    post_form(
        port,
        path,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", REDIRECT),
            ("client_id", client),
            ("code_verifier", verifier),
        ],
    )
}

#[test]
fn mcp_oauth_pkce_tools_refresh_rotation_revocation_and_restart_follow_the_grant() {
    let env = Env::new();
    let (grant_id, confirmation) = create_grant(&env, "chat-client", CredentialKind::OAuth);
    let (_, api_token) = create_grant(&env, "script", CredentialKind::Token);
    let (_, url_secret) = create_grant(&env, "browser", CredentialKind::Url);
    let port = start_mcp(&env);

    // 发现：没带凭据的请求拿到带发现信息的 401，元数据端点照常可读。
    assert_unauthorized(&rpc(port, None, "tools/list", json!({})));
    let protected = get(port, "/.well-known/oauth-protected-resource");
    assert_eq!(protected.status, 200);
    assert!(!protected.json()["authorization_servers"]
        .as_array()
        .expect("authorization servers")
        .is_empty());
    assert_eq!(
        get(port, "/.well-known/oauth-protected-resource/mcp").json(),
        protected.json()
    );
    let metadata = get(port, "/.well-known/oauth-authorization-server").json();
    assert!(metadata["code_challenge_methods_supported"]
        .as_array()
        .unwrap()
        .iter()
        .any(|method| method == "S256"));
    let token_path = endpoint_path(&metadata, "token_endpoint");
    let authorize_path = endpoint_path(&metadata, "authorization_endpoint");

    // 动态注册两个客户端，绑到同一条 grant。
    let client_id = register(port, &metadata);
    let second_client = register(port, &metadata);
    let invalid = post_json(
        port,
        &endpoint_path(&metadata, "registration_endpoint"),
        &json!({"redirect_uris":["not-a-url"]}).to_string(),
        None,
    );
    assert_eq!(invalid.status, 400);
    assert_eq!(invalid.json()["error"], "invalid_redirect_uri");

    // 授权页：口令只走 POST 正文；错口令、别的用途的凭据、旧服务口令是同一个回包，不回显。
    let (verifier, challenge) = pkce();
    let page = get(port, &format!(
        "{authorize_path}?response_type=code&client_id={}&redirect_uri={}&code_challenge={challenge}&code_challenge_method=S256&state=test-state",
        urlencode(&client_id), urlencode(REDIRECT)
    ));
    assert_eq!(page.status, 200, "{}", page.body);
    assert_eq!(page.header("cache-control"), Some("no-store"));
    assert_eq!(page.header("referrer-policy"), Some("no-referrer"));
    let service_password = env.service_secret("oauth_password");
    let (prefix, secret) = confirmation.rsplit_once('.').unwrap();
    let wrong_secret = format!("{prefix}.{}", "A".repeat(secret.len()));
    let unknown_id = format!("gld_oauth_v1.{}.{secret}", "0".repeat(32));
    let mut refusals = Vec::new();
    for offered in [
        wrong_secret.as_str(),
        unknown_id.as_str(),
        api_token.as_str(),
        url_secret.as_str(),
        service_password.as_str(),
    ] {
        let denied = authorize(port, &authorize_path, &client_id, &challenge, offered);
        assert_eq!(denied.status, 401, "{}", denied.body);
        assert_eq!(denied.header("cache-control"), Some("no-store"));
        assert_eq!(denied.header("referrer-policy"), Some("no-referrer"));
        assert!(denied.header("location").is_none());
        assert!(!denied.body.contains(offered));
        refusals.push(denied.body);
    }
    assert!(refusals.iter().all(|body| body == &refusals[0]));

    // PKCE：错 verifier 换不出 token，而且授权码用过即废。
    let granted = authorize(port, &authorize_path, &client_id, &challenge, &confirmation);
    assert_eq!(granted.status, 303, "{}", granted.body);
    let code = query_param(granted.header("location").unwrap(), "code");
    assert_invalid_grant(&exchange(
        port,
        &token_path,
        &client_id,
        &code,
        &"x".repeat(64),
    ));
    assert_invalid_grant(&exchange(port, &token_path, &client_id, &code, &verifier));

    // access / refresh：绑定 grant、client 和修订号 1，期限 30 天 / 90 天。
    let (access, refresh_token) = token_pair(&authorize_and_exchange(
        port,
        &metadata,
        &client_id,
        &confirmation,
    ));
    const DAY: i64 = 60 * 60 * 24;
    for (token, ttl) in [(&access, 30 * DAY), (&refresh_token, 90 * DAY)] {
        let claims = claims(token);
        assert_eq!(claims["grant_id"], grant_id.as_str());
        assert_eq!(claims["client_id"], client_id.as_str());
        assert_eq!(claims["credential_revision"], 1);
        assert_eq!(
            claims["exp"].as_i64().unwrap() - claims["iat"].as_i64().unwrap(),
            ttl
        );
        assert!((claims["iat"].as_i64().unwrap() - now() as i64).abs() < 120);
    }

    // 工具调用：initialize、tools/list、读文件能用；默认权限不含写。
    let initialized = rpc(
        port,
        Some(&access),
        "initialize",
        json!({
            "protocolVersion":"2025-06-18",
            "capabilities":{},
            "clientInfo":{"name":"oauth-test","version":"1"}
        }),
    );
    assert_eq!(initialized.status, 200, "{}", initialized.body);
    assert_eq!(
        initialized.json()["result"]["protocolVersion"],
        "2025-06-18"
    );
    let listed = rpc(port, Some(&access), "tools/list", json!({}));
    assert_eq!(listed.status, 200, "{}", listed.body);
    let tools = listed.json()["result"]["tools"].as_array().unwrap().clone();
    assert!(tools.iter().any(|tool| tool["name"] == "list_workspaces"));
    assert!(!tools.iter().any(|tool| tool["name"] == "apply_patch"));
    let read_marker = |token: &str| {
        rpc(
            port,
            Some(token),
            "tools/call",
            json!({"name":"read_file","arguments":{"workspace":"oauth","path":"hello.txt"}}),
        )
    };
    let read = read_marker(&access);
    assert_eq!(read.status, 200, "{}", read.body);
    assert!(read.body.contains("oauth-project-marker"), "{}", read.body);
    let denied = rpc(
        port,
        Some(&access),
        "tools/call",
        json!({"name":"apply_patch","arguments":{
            "workspace":"oauth",
            "patch":"*** Begin Patch\n*** Add File: forbidden.txt\n+blocked\n*** End Patch\n"
        }}),
    );
    assert!(!env.project.path().join("forbidden.txt").exists());
    let denied_json = denied.json();
    assert!(
        denied_json["result"]["isError"] == true || denied_json["error"].is_object(),
        "{}",
        denied.body
    );

    // 不回落：refresh、确认口令、伪造的旧 token 当 access 用都是 401；access 也不能拿去续签。
    for wrong in [
        refresh_token.as_str(),
        confirmation.as_str(),
        "synthetic-old-oauth-access",
    ] {
        let refused = rpc(port, Some(wrong), "tools/list", json!({}));
        assert_unauthorized(&refused);
        assert!(!refused.body.contains(wrong));
    }
    assert_invalid_grant(&refresh(port, &token_path, &client_id, &access));
    assert_invalid_grant(&refresh(
        port,
        &token_path,
        &client_id,
        "synthetic-old-refresh",
    ));

    // refresh：新的一对仍绑在同一条 grant 和修订号上，新 access 可用。
    let (renewed_access, renewed_refresh) =
        token_pair(&refresh(port, &token_path, &client_id, &refresh_token));
    assert_eq!(claims(&renewed_access)["grant_id"], grant_id.as_str());
    assert_eq!(claims(&renewed_refresh)["credential_revision"], 1);
    assert_eq!(read_marker(&renewed_access).status, 200);

    // 第二个客户端：同一条 grant，自己的 client_id；不能拿别人的 refresh 续签。
    let (second_access, second_refresh) = token_pair(&authorize_and_exchange(
        port,
        &metadata,
        &second_client,
        &confirmation,
    ));
    assert_eq!(claims(&second_access)["grant_id"], grant_id.as_str());
    assert_eq!(claims(&second_access)["client_id"], second_client.as_str());
    assert_eq!(read_marker(&second_access).status, 200);
    let swapped = refresh(port, &token_path, &second_client, &refresh_token);
    assert_eq!(swapped.status, 400, "{}", swapped.body);
    assert_eq!(swapped.json()["error"], "invalid_client");

    // URL 入口只认 URL 用途的秘密，OAuth access 和确认口令放进地址都是 401；
    // 隧道还没接入，仍是 AUTH_NOT_READY。
    let list = json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}).to_string();
    let url_reply = post_json(port, &format!("/mcp/connect/{url_secret}"), &list, None);
    assert_eq!(url_reply.status, 200, "{}", url_reply.body);
    for other in [&second_access, &confirmation] {
        let reply = post_json(port, &format!("/mcp/connect/{other}"), &list, None);
        assert_eq!(reply.status, 401, "{}", reply.body);
        assert!(reply.header("www-authenticate").is_none());
    }
    let tunnel_reply = request(
        port,
        "POST",
        "/mcp",
        &[
            ("Content-Type", "application/json"),
            (gld_core::hub::OPENAI_TUNNEL_HEADER, "synthetic-tunnel-key"),
        ],
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#,
    );
    // 这个服务没配 OpenAI 隧道：隧道头不被接受，也不带 OAuth 发现信息。
    assert_eq!(tunnel_reply.status, 401, "{}", tunnel_reply.body);
    assert_eq!(tunnel_reply.json()["error"], "AUTHENTICATION_FAILED");
    assert!(tunnel_reply.header("www-authenticate").is_none());

    // 重启：注册表和签名密钥都是落盘的那一份，重启前签发的 token 继续可用。
    let registry = env.home.path().join("data/oauth-clients/hub.json");
    let registered = std::fs::read(&registry).expect("isolated registered clients");
    assert!(String::from_utf8_lossy(&registered).contains(&client_id));
    let signing_key = env.service_secret("oauth_token_secret");
    assert!(!signing_key.is_empty());
    env.ok(&["daemon", "stop"]);
    env.ok(&["start"]);
    assert_eq!(std::fs::read(&registry).unwrap(), registered);
    assert_eq!(env.service_secret("oauth_token_secret"), signing_key);
    assert_eq!(
        get(port, "/.well-known/oauth-authorization-server").json(),
        metadata
    );
    assert_eq!(read_marker(&renewed_access).status, 200);
    let (after_restart, _) = token_pair(&refresh(port, &token_path, &client_id, &renewed_refresh));
    assert_eq!(read_marker(&after_restart).status, 200);

    // 轮换（daemon 停着时用正式存储做）：GrantId 不变、修订号加一，旧 access / refresh /
    // 旧口令全部作废；两个客户端都要用新口令重新授权，注册不用重做。
    env.ok(&["daemon", "stop"]);
    let rotated = {
        let store = AccessStore::open_in(env.home.path()).unwrap();
        let issued = store.rotate_grant("chat-client", None, now()).unwrap();
        assert_eq!(issued.record.grant.id, grant_id);
        assert_eq!(issued.record.credential.revision, 2);
        issued.secret.into_string()
    };
    env.ok(&["start"]);
    for stale in [&access, &renewed_access, &after_restart, &second_access] {
        assert_unauthorized(&read_marker(stale));
    }
    for (client, stale) in [
        (&client_id, &refresh_token),
        (&client_id, &renewed_refresh),
        (&second_client, &second_refresh),
    ] {
        assert_invalid_grant(&refresh(port, &token_path, client, stale));
    }
    let (_, challenge) = pkce();
    let old_confirmation = authorize(port, &authorize_path, &client_id, &challenge, &confirmation);
    assert_eq!(old_confirmation.status, 401, "{}", old_confirmation.body);
    let (rotated_access, rotated_refresh) = token_pair(&authorize_and_exchange(
        port, &metadata, &client_id, &rotated,
    ));
    assert_eq!(claims(&rotated_access)["grant_id"], grant_id.as_str());
    assert_eq!(claims(&rotated_access)["credential_revision"], 2);
    assert_eq!(read_marker(&rotated_access).status, 200);

    // 撤销（运行中的 daemon）：下一个请求就是 401，refresh 和重新授权也都不行。
    env.ok(&["grant", "rm", "chat-client"]);
    assert_unauthorized(&read_marker(&rotated_access));
    assert_unauthorized(&rpc(port, Some(&rotated_access), "tools/list", json!({})));
    assert_invalid_grant(&refresh(port, &token_path, &client_id, &rotated_refresh));
    let (_, challenge) = pkce();
    let revoked = authorize(port, &authorize_path, &client_id, &challenge, &rotated);
    assert_eq!(revoked.status, 401, "{}", revoked.body);
    assert!(revoked.header("location").is_none());

    // 撤销只影响这一条 grant：API token 照常，发现和注册也还在。
    assert_eq!(
        rpc(port, Some(&api_token), "tools/list", json!({})).status,
        200
    );
    assert_eq!(
        get(port, "/.well-known/oauth-authorization-server").json(),
        metadata
    );
    assert_eq!(std::fs::read(&registry).unwrap(), registered);
    assert_eq!(
        std::fs::read_to_string(env.project.path().join("hello.txt")).unwrap(),
        "oauth-project-marker\n"
    );
}

#[test]
fn mcp_without_any_grant_keeps_discovery_but_authorizes_nothing() {
    let env = Env::new();
    let port = start_mcp(&env);
    let metadata = get(port, "/.well-known/oauth-authorization-server").json();
    let client_id = register(port, &metadata);
    let authorize_path = endpoint_path(&metadata, "authorization_endpoint");
    let token_path = endpoint_path(&metadata, "token_endpoint");
    let password = env.service_secret("oauth_password");
    let (verifier, challenge) = pkce();
    for offered in [
        password.as_str(),
        "synthetic-old-grant-password",
        "wrong-password",
    ] {
        let response = authorize(port, &authorize_path, &client_id, &challenge, offered);
        assert_eq!(response.status, 401, "{}", response.body);
        assert!(response.header("location").is_none());
        assert!(!response.body.contains(offered));
    }
    assert_invalid_grant(&exchange(
        port,
        &token_path,
        &client_id,
        "synthetic-old-code",
        &verifier,
    ));
    assert_invalid_grant(&refresh(
        port,
        &token_path,
        &client_id,
        "synthetic-old-refresh",
    ));
    // 授权存储还没建过任何 grant：/mcp 对所有来源都是 AUTH_NOT_READY，旧 token 不能恢复权限。
    let rejected = post_json(port, "/mcp", &json!({
        "jsonrpc":"2.0","id":2,"method":"tools/call","params":{
            "name":"apply_patch","arguments":{
                "workspace":"oauth",
                "patch":"*** Begin Patch\n*** Add File: forbidden.txt\n+blocked\n*** End Patch\n"
            }
        }
    }).to_string(), Some("synthetic-old-oauth-access"));
    assert_eq!(rejected.status, 503, "{}", rejected.body);
    assert_eq!(rejected.json()["error"], "AUTH_NOT_READY");
    assert!(!env.project.path().join("forbidden.txt").exists());
}

#[test]
fn actions_oauth_pkce_refresh_and_tool_calls_remain_independent_of_mcp() {
    let env = Env::new();
    create_grant(&env, "chat-client", CredentialKind::OAuth);
    let mcp_port = start_mcp(&env);
    let port = free_port();
    env.ok(&["set", "oauth", &format!("actions.port={port}")]);
    env.ok(&["set", "oauth", "actions.auth=oauth"]);
    env.ok(&["start", "-s", "actions"]);
    let password = env.json(&[
        "--json",
        "secret",
        "ls",
        "actions_oauth_password",
        "--reveal",
        "-w",
        "oauth",
    ])["value"]
        .as_str()
        .expect("isolated Actions password")
        .to_string();
    let metadata = get(port, "/.well-known/oauth-authorization-server").json();
    let client_id = register(port, &metadata);
    let authorize_path = endpoint_path(&metadata, "authorization_endpoint");
    let token_path = endpoint_path(&metadata, "token_endpoint");
    let (verifier, challenge) = pkce();
    let page = get(port, &format!(
        "{authorize_path}?response_type=code&client_id={}&redirect_uri={}&code_challenge={challenge}&code_challenge_method=S256&state=test-state",
        urlencode(&client_id), urlencode(REDIRECT)
    ));
    assert_eq!(page.status, 200);
    assert!(page.body.contains("password"));

    let denied = authorize(
        port,
        &authorize_path,
        &client_id,
        &challenge,
        "wrong-password",
    );
    assert_eq!(denied.status, 401);
    assert!(denied.header("location").is_none());

    let granted = authorize(port, &authorize_path, &client_id, &challenge, &password);
    assert_eq!(granted.status, 303);
    let location = granted.header("location").expect("authorization redirect");
    assert!(location.starts_with(REDIRECT));
    assert_eq!(query_param(location, "state"), "test-state");
    let consumed_code = query_param(location, "code");
    let wrong_verifier = exchange(
        port,
        &token_path,
        &client_id,
        &consumed_code,
        &"x".repeat(64),
    );
    assert_eq!(wrong_verifier.status, 400);
    assert_eq!(wrong_verifier.json()["error"], "invalid_grant");
    assert_eq!(
        exchange(port, &token_path, &client_id, &consumed_code, &verifier).status,
        400
    );

    let fresh = authorize(port, &authorize_path, &client_id, &challenge, &password);
    let code = query_param(fresh.header("location").unwrap(), "code");
    let exchanged = exchange(port, &token_path, &client_id, &code, &verifier);
    assert_eq!(exchanged.status, 200, "{}", exchanged.body);
    let tokens = exchanged.json();
    let access = tokens["access_token"].as_str().unwrap();
    let refresh = tokens["refresh_token"].as_str().unwrap();
    assert_eq!(
        exchange(port, &token_path, &client_id, &code, &verifier).status,
        400
    );
    let called = post_json(
        port,
        "/actions/read_file",
        r#"{"path":"hello.txt"}"#,
        Some(access),
    );
    assert_eq!(called.status, 200, "{}", called.body);
    assert!(called.body.contains("oauth-project-marker"));
    assert_eq!(
        post_json(
            port,
            "/actions/read_file",
            r#"{"path":"hello.txt"}"#,
            Some(refresh)
        )
        .status,
        401
    );
    // Actions 的 access token 不是 MCP 的凭据：MCP 入口已有 OAuth grant 也照样 401。
    assert_unauthorized(&post_json(
        mcp_port,
        "/mcp",
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#,
        Some(access),
    ));

    env.ok(&["restart", "-s", "actions"]);
    let after_restart = post_json(
        port,
        "/actions/read_file",
        r#"{"path":"hello.txt"}"#,
        Some(access),
    );
    assert_eq!(after_restart.status, 200, "{}", after_restart.body);
    let renewed = post_form(
        port,
        &token_path,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh),
            ("client_id", &client_id),
        ],
    );
    assert_eq!(renewed.status, 200, "{}", renewed.body);
    let renewed_body = renewed.json();
    let next_access = renewed_body["access_token"].as_str().unwrap();
    let read = post_json(
        port,
        "/actions/read_file",
        r#"{"path":"hello.txt"}"#,
        Some(next_access),
    );
    assert_eq!(read.status, 200, "{}", read.body);
    assert!(read.body.contains("oauth-project-marker"));
}

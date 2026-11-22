//! MCP 的授权页和 token 端点已接到统一授权：只有 OAuth 用途 grant 的确认口令能换出 token，
//! 旧服务口令、旧授权码和旧 refresh 都被拒绝。`/mcp` 上的 access 验证在下一步接通。
//! Actions remains a separate protocol and exercises the real PKCE/token/tool path below.

mod common;

use common::env::{free_port, Env};
use common::http::{get, post_form, post_json, query_param, urlencode, Reply};
use common::oauth::{authorize, endpoint_path, pkce, REDIRECT};
use serde_json::{json, Value};

fn start_mcp(env: &Env) -> u16 {
    env.write("hello.txt", "oauth-project-marker\n");
    env.ok(&["add", ".", "--name", "oauth"]);
    let port = free_port();
    env.ok(&["start", "--port", &port.to_string()]);
    port
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

fn assert_pending(reply: &Reply) {
    assert_eq!(reply.status, 503, "{}", reply.body);
    assert_eq!(reply.json()["error"], "AUTH_NOT_READY");
    assert_eq!(reply.header("cache-control"), Some("no-store"));
    assert!(reply.header("location").is_none());
    assert!(reply.json().get("access_token").is_none());
    assert!(reply.json().get("refresh_token").is_none());
}

#[test]
fn mcp_discovery_and_durable_registration_survive_a_restart_without_authorizing() {
    let env = Env::new();
    let port = start_mcp(&env);
    let discovery = get(port, "/mcp");
    assert_eq!(discovery.status, 200);
    assert_eq!(discovery.json()["protocolVersion"], "2025-06-18");

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

    let metadata = get(port, "/.well-known/oauth-authorization-server");
    assert_eq!(metadata.status, 200);
    let metadata = metadata.json();
    assert!(metadata["code_challenge_methods_supported"]
        .as_array()
        .unwrap()
        .iter()
        .any(|method| method == "S256"));
    let client_id = register(port, &metadata);
    let invalid = post_json(
        port,
        &endpoint_path(&metadata, "registration_endpoint"),
        &json!({
            "redirect_uris":["not-a-url"]
        })
        .to_string(),
        None,
    );
    assert_eq!(invalid.status, 400);
    assert_eq!(invalid.json()["error"], "invalid_redirect_uri");

    let registry = env.home.path().join("data/oauth-clients/hub.json");
    let before = std::fs::read(&registry).expect("isolated registered client");
    assert!(String::from_utf8_lossy(&before).contains(&client_id));
    env.ok(&["stop"]);
    env.ok(&["start"]);
    assert_eq!(
        std::fs::read(&registry).unwrap(),
        before,
        "restart replaced the registration"
    );
    assert_eq!(
        get(port, "/.well-known/oauth-authorization-server").json(),
        metadata
    );

    let (_, challenge) = pkce();
    let page = get(port, &format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&code_challenge={challenge}&code_challenge_method=S256&state=test-state",
        endpoint_path(&metadata,"authorization_endpoint"), urlencode(&client_id), urlencode(REDIRECT)
    ));
    assert_eq!(page.status, 200, "{}", page.body);
    assert_eq!(page.header("cache-control"), Some("no-store"));
    assert_pending(&post_json(
        port,
        "/mcp",
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        None,
    ));
}

#[test]
fn old_service_password_code_and_tokens_cannot_reenter_mcp_authorization() {
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
        assert_eq!(response.header("cache-control"), Some("no-store"));
        assert!(response.header("location").is_none());
        assert!(!response.body.contains(offered));
    }
    for refused in [
        exchange(
            port,
            &token_path,
            &client_id,
            "synthetic-old-code",
            &verifier,
        ),
        post_form(
            port,
            &token_path,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", "synthetic-old-refresh"),
                ("client_id", &client_id),
            ],
        ),
    ] {
        assert_eq!(refused.status, 400, "{}", refused.body);
        assert_eq!(refused.json()["error"], "invalid_grant");
        assert!(refused.json().get("access_token").is_none());
    }
    let rejected = post_json(port, "/mcp", &json!({
        "jsonrpc":"2.0","id":2,"method":"tools/call","params":{
            "name":"apply_patch","arguments":{
                "workspace":"oauth",
                "patch":"*** Begin Patch\n*** Add File: forbidden.txt\n+blocked\n*** End Patch\n"
            }
        }
    }).to_string(), Some("synthetic-old-oauth-access"));
    assert_pending(&rejected);
    assert!(!env.project.path().join("forbidden.txt").exists());
    assert_eq!(
        std::fs::read_to_string(env.project.path().join("hello.txt")).unwrap(),
        "oauth-project-marker\n"
    );
}

#[test]
fn actions_oauth_pkce_refresh_and_tool_calls_remain_independent_of_mcp() {
    let env = Env::new();
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
    assert_pending(&post_json(
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

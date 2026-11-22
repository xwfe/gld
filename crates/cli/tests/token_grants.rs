//! D18-B3.2: 独立签发的 token 经真实 daemon/HTTP/Hub 进入工具调用；
//! 其他尚未接入的来源仍拒绝，旧全局认证方式不能恢复权限。

mod common;

use common::env::{free_port, Env};
use common::http::post_json;
use gld_core::auth::{AccessStore, CredentialKind, GrantCreation, GrantPermissions, ProjectScope};
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

fn rpc(port: u16, credential: Option<&str>, method: &str, params: Value) -> common::http::Reply {
    post_json(
        port,
        "/mcp",
        &json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}).to_string(),
        credential,
    )
}

#[test]
fn issued_token_reads_through_real_hub_but_cannot_write_or_imitate_oauth() {
    let env = Env::new();
    env.write("visible.txt", "token-read-fixture\n");

    // 在 daemon 持有唯一授权写锁前，用隔离数据目录建立正式格式凭据。
    let store = AccessStore::open_in(env.home.path()).unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let issued = store
        .create_grant(
            GrantCreation {
                name: "Integration".into(),
                scope: ProjectScope::AllProjects,
                permissions: GrantPermissions::default(),
                kind: CredentialKind::Token,
                ttl: None,
                allowed_origins: vec![],
            },
            now,
        )
        .unwrap();
    let secret = issued.secret.into_string();
    let url = GrantCreation {
        name: "URL".into(),
        scope: ProjectScope::AllProjects,
        permissions: GrantPermissions::default(),
        kind: CredentialKind::Url,
        ttl: None,
        allowed_origins: vec![],
    };
    let url_secret = store.create_grant(url, now).unwrap().secret.into_string();
    let oauth = GrantCreation {
        name: "OAuth".into(),
        kind: CredentialKind::OAuth,
        scope: ProjectScope::AllProjects,
        permissions: GrantPermissions::default(),
        ttl: None,
        allowed_origins: vec![],
    };
    let oauth_confirmation = store.create_grant(oauth, now).unwrap().secret.into_string();
    drop(store);

    let port = free_port();
    env.ok(&["add", ".", "--name", "api"]);
    env.ok(&["upgrade", "--port", &port.to_string(), "--auth", "oauth"]);
    env.ok(&["start"]);

    let initialized = rpc(
        port,
        Some(&secret),
        "initialize",
        json!({
            "protocolVersion":"2025-06-18",
            "capabilities":{},
            "clientInfo":{"name":"token-test","version":"1"}
        }),
    );
    assert_eq!(initialized.status, 200, "{}", initialized.body);
    assert_eq!(
        initialized.json()["result"]["protocolVersion"],
        "2025-06-18"
    );

    let discovery = rpc(port, Some(&secret), "tools/list", json!({}));
    assert_eq!(discovery.status, 200, "{}", discovery.body);
    let tools = discovery.json()["result"]["tools"]
        .as_array()
        .unwrap()
        .clone();
    assert!(tools.iter().any(|tool| tool["name"] == "list_workspaces"));
    assert!(!tools.iter().any(|tool| tool["name"] == "apply_patch"));

    let listed = rpc(
        port,
        Some(&secret),
        "tools/call",
        json!({"name":"list_workspaces","arguments":{}}),
    );
    assert_eq!(listed.status, 200, "{}", listed.body);
    assert!(listed.body.contains("api"), "{}", listed.body);
    assert_ne!(listed.json()["result"]["isError"], true);

    let read = rpc(
        port,
        Some(&secret),
        "tools/call",
        json!({"name":"read_file","arguments":{"workspace":"api","path":"visible.txt"}}),
    );
    assert_eq!(read.status, 200, "{}", read.body);
    assert!(read.body.contains("token-read-fixture"), "{}", read.body);

    // AllProjects 在授权创建之后添加的项目也可读，不依赖重签或监听器重启。
    let added = tempfile::tempdir().unwrap();
    std::fs::write(added.path().join("future.txt"), "future-project-fixture\n").unwrap();
    env.ok(&["add", added.path().to_str().unwrap(), "--name", "later"]);
    let read_future = rpc(
        port,
        Some(&secret),
        "tools/call",
        json!({"name":"read_file","arguments":{"workspace":"later","path":"future.txt"}}),
    );
    assert_eq!(read_future.status, 200, "{}", read_future.body);
    assert!(
        read_future.body.contains("future-project-fixture"),
        "{}",
        read_future.body
    );

    let patch =
        "*** Begin Patch\n*** Add File: network-write-denied.txt\n+not authorized\n*** End Patch\n";
    let denied = rpc(
        port,
        Some(&secret),
        "tools/call",
        json!({"name":"apply_patch","arguments":{"workspace":"api","patch":patch}}),
    );
    assert!(!env.project.path().join("network-write-denied.txt").exists());
    let denied_json = denied.json();
    assert!(
        denied_json["result"]["isError"] == true || denied_json["error"].is_object(),
        "{}",
        denied.body
    );

    let invalid = rpc(
        port,
        Some(&format!("{secret}invalid")),
        "tools/list",
        json!({}),
    );
    assert_eq!(invalid.status, 401, "{}", invalid.body);
    assert_eq!(invalid.json()["error"], "AUTHENTICATION_FAILED");
    assert_eq!(invalid.header("cache-control"), Some("no-store"));
    assert_eq!(invalid.header("referrer-policy"), Some("no-referrer"));
    assert!(invalid
        .header("www-authenticate")
        .unwrap()
        .contains("resource_metadata="));
    assert!(!invalid.body.contains(&secret));

    for wrong_purpose in [&url_secret, &oauth_confirmation] {
        let response = rpc(port, Some(wrong_purpose), "tools/list", json!({}));
        assert_eq!(response.status, 401, "{}", response.body);
        assert!(!response.body.contains(wrong_purpose));
    }
    let url_path = format!("/mcp/connect/{url_secret}");
    let url_not_ready = post_json(
        port,
        &url_path,
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}).to_string(),
        None,
    );
    assert_eq!(url_not_ready.status, 503, "{}", url_not_ready.body);
    assert_eq!(url_not_ready.json()["error"], "AUTH_NOT_READY");
    assert!(!url_not_ready.body.contains(&url_secret));

    // 没带凭据，或带着旧服务 token / 旧 OAuth token：按 OAuth Bearer 验证失败，直接 401，
    // 回包带发现信息，不回落到别的凭据。
    for legacy in [None, Some("old-service-token"), Some("old-oauth-access")] {
        let response = rpc(port, legacy, "tools/list", json!({}));
        assert_eq!(response.status, 401, "{}", response.body);
        assert_eq!(response.json()["error"], "AUTHENTICATION_FAILED");
        assert!(response
            .header("www-authenticate")
            .unwrap()
            .contains("resource_metadata="));
    }
}

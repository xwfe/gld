//! D18-A: retired grant creation and legacy MCP credentials fail closed over real HTTP.
//! Positive token/OAuth scope, rotation and revocation flows resume with D18-B's issuance entrypoint.

mod common;

use common::env::{free_port, Env};
use common::http::{post_json, Reply};
use serde_json::{json, Value};

fn start_service(env: &Env, auth: &str) -> u16 {
    let port = free_port();
    env.write("private.txt", "project-private-marker\n");
    env.ok(&["add", ".", "--name", "api"]);
    env.ok(&["upgrade", "--port", &port.to_string(), "--auth", auth]);
    env.ok(&["start"]);
    port
}

fn rpc(port: u16, token: Option<&str>, method: &str, params: Value) -> Reply {
    post_json(
        port,
        "/mcp",
        &json!({"jsonrpc":"2.0", "id":1, "method":method, "params":params}).to_string(),
        token,
    )
}

fn assert_not_ready(reply: &Reply) {
    assert_eq!(reply.status, 503, "{}", reply.body);
    assert_eq!(reply.json()["error"], "AUTH_NOT_READY");
    assert_eq!(reply.header("cache-control"), Some("no-store"));
    assert!(!reply.body.contains("project-private-marker"));
    assert!(reply.json().get("result").is_none());
}

#[test]
fn retired_grant_creation_and_reveal_cannot_write_legacy_credentials() {
    let env = Env::new();
    env.ok(&["add", ".", "--name", "api"]);
    assert_eq!(env.json(&["--json", "grant", "ls"]), json!([]));
    let profiles = env.home.path().join("data/profiles.json");
    let before = std::fs::read(&profiles).expect("isolated profile data");

    for args in [
        vec!["grant", "add", "reader", "api"],
        vec!["--json", "grant", "add", "writer", "api", "--write"],
    ] {
        let output = env.gld(&args);
        assert!(
            !output.status.success(),
            "retired creation succeeded: {args:?}"
        );
        let error = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(error.contains("AUTH_NOT_READY"), "{error}");
        assert!(!error.contains("oauthPassword"));
        assert!(!error.contains("bearerToken"));
    }
    let reveal = env.gld(&["--json", "grant", "ls", "--reveal"]);
    assert!(!reveal.status.success());
    assert!(String::from_utf8_lossy(&reveal.stderr).contains("AUTH_UNSUPPORTED"));
    assert_eq!(env.json(&["--json", "grant", "ls"]), json!([]));
    assert_eq!(
        std::fs::read(profiles).unwrap(),
        before,
        "rejected creation changed project data"
    );
}

#[test]
fn legacy_global_modes_and_bearer_credentials_never_restore_mcp_authority() {
    for mode in ["oauth", "bearer"] {
        let env = Env::new();
        let port = start_service(&env, mode);
        let service_token = env.service_secret("bearer_token");
        for token in [
            None,
            Some(service_token.as_str()),
            Some("synthetic-retired-grant-token"),
        ] {
            assert_not_ready(&rpc(
                port,
                token,
                "initialize",
                json!({
                    "protocolVersion":"2025-06-18",
                    "capabilities":{},
                    "clientInfo":{"name":"authorization-rejection-test","version":"1"}
                }),
            ));
            assert_not_ready(&rpc(port, token, "tools/list", json!({})));
            assert_not_ready(&rpc(
                port,
                token,
                "tools/call",
                json!({
                    "name":"read_file", "arguments":{"workspace":"api","path":"private.txt"}
                }),
            ));
        }
        assert_eq!(env.json(&["--json", "grant", "ls"]), json!([]));
    }
    let env = Env::new();
    let port = start_service(&env, "oauth");
    let before = env.json(&["--json", "ls"])["service"]["config"].clone();
    let noauth = env.gld(&["upgrade", "--auth", "noauth"]);
    assert!(
        !noauth.status.success(),
        "anonymous MCP configuration was accepted"
    );
    assert!(String::from_utf8_lossy(&noauth.stderr).contains("AUTH_REQUIRED"));
    assert_eq!(env.json(&["--json", "ls"])["service"]["config"], before);
    assert_not_ready(&rpc(port, None, "tools/list", json!({})));
}

#[test]
fn rejected_network_writes_have_no_project_side_effects() {
    let env = Env::new();
    let port = start_service(&env, "oauth");
    let patch =
        "*** Begin Patch\n*** Add File: forbidden.txt\n+must not be written\n*** End Patch\n";
    let reply = rpc(
        port,
        Some("synthetic-retired-write-token"),
        "tools/call",
        json!({
            "name":"apply_patch", "arguments":{"workspace":"api","patch":patch}
        }),
    );
    assert_not_ready(&reply);
    assert!(!env.project.path().join("forbidden.txt").exists());
    assert_eq!(
        std::fs::read_to_string(env.project.path().join("private.txt")).unwrap(),
        "project-private-marker\n"
    );

    let command = rpc(
        port,
        None,
        "tools/call",
        json!({
            "name":"exec_command", "arguments":{"workspace":"api","cmd":"echo unauthorized","yield_time_ms":0}
        }),
    );
    assert_not_ready(&command);
    assert!(!command.body.contains("session_id"));
}

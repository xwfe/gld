//! D18-A: retired browser URLs cannot act as aliases for unified URL credentials.
//! Successful URL issuance, Origin policy, TTL and revocation are D18-B acceptance cases.

mod common;

use common::env::{free_port, Env};
use common::http::{post_json, request, Reply};
use serde_json::json;

const EXTENSION_ORIGIN: &str = "chrome-extension://kngiafgkdnlkgmefdafaibkibegkcaef";
const SECRET: &str = "synthetic-url-secret-must-never-be-logged";

fn start_service(env: &Env) -> u16 {
    env.write("private.txt", "url-private-marker\n");
    env.ok(&["add", ".", "--name", "api"]);
    let port = free_port();
    env.ok(&["start", "--port", &port.to_string()]);
    port
}

fn body() -> String {
    json!({"jsonrpc":"2.0","id":7,"method":"tools/list","params":{}}).to_string()
}

fn assert_not_ready(reply: &Reply) {
    assert_eq!(reply.status, 503, "{}", reply.body);
    assert_eq!(reply.json()["error"], "AUTH_NOT_READY");
    assert_eq!(reply.header("cache-control"), Some("no-store"));
    assert!(!reply.body.contains(SECRET));
    assert!(!reply.body.contains("url-private-marker"));
    assert!(reply.header("mcp-session-id").is_none());
}

#[test]
fn retired_browser_creation_cannot_return_or_persist_a_url() {
    let env = Env::new();
    env.ok(&["add", ".", "--name", "api"]);
    assert_eq!(env.json(&["--json", "grant", "ls"]), json!([]));
    let profiles = env.home.path().join("data/profiles.json");
    let before = std::fs::read(&profiles).unwrap();
    for args in [
        vec!["grant", "browser", "reader", "api", "--ttl", "8h"],
        vec![
            "--json", "grant", "browser", "writer", "api", "--ttl", "24h", "--write",
        ],
        vec!["grant", "browser", "all-projects", "--all"],
    ] {
        let output = env.gld(&args);
        assert!(!output.status.success(), "retired URL creation succeeded");
        let message = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(message.contains("AUTH_NOT_READY"), "{message}");
        assert!(!message.contains("/mcp/browser/"));
        assert!(!message.contains("/mcp/connect/"));
    }
    assert_eq!(env.json(&["--json", "grant", "ls"]), json!([]));
    assert_eq!(std::fs::read(profiles).unwrap(), before);
}

#[test]
fn retired_url_route_is_absent_and_new_route_refuses_every_request() {
    let env = Env::new();
    let port = start_service(&env);
    let old_path = format!("/mcp/browser/{SECRET}");
    let new_path = format!("/mcp/connect/{SECRET}");
    for method in ["POST", "GET", "DELETE"] {
        let old = request(
            port,
            method,
            &old_path,
            &[("Content-Type", "application/json")],
            &body(),
        );
        assert_eq!(old.status, 404, "{method}: {}", old.body);
        assert!(!old.body.contains(SECRET));
        let current = request(
            port,
            method,
            &new_path,
            &[
                ("Content-Type", "application/json"),
                ("Origin", EXTENSION_ORIGIN),
                ("Mcp-Protocol-Version", "2025-06-18"),
            ],
            &body(),
        );
        assert_not_ready(&current);
        assert_eq!(current.header("referrer-policy"), Some("no-referrer"));
    }
    for origin in [EXTENSION_ORIGIN, "https://untrusted.example.test", "null"] {
        let denied = request(
            port,
            "POST",
            &new_path,
            &[("Content-Type", "application/json"), ("Origin", origin)],
            &body(),
        );
        assert_not_ready(&denied);
    }
    let patch = json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{
        "name":"apply_patch","arguments":{"workspace":"api","patch":"*** Begin Patch\n*** Add File: forbidden.txt\n+blocked\n*** End Patch\n"}
    }});
    assert_not_ready(&post_json(port, &new_path, &patch.to_string(), None));
    assert!(!env.project.path().join("forbidden.txt").exists());
}

#[test]
fn cors_preflight_does_not_authenticate_or_enable_a_credential_fallback() {
    let env = Env::new();
    let port = start_service(&env);
    let path = format!("/mcp/connect/{SECRET}");
    let preflight = request(port, "OPTIONS", &path, &[
        ("Origin",EXTENSION_ORIGIN),
        ("Access-Control-Request-Method","POST"),
        ("Access-Control-Request-Headers","content-type,accept,authorization,mcp-session-id,mcp-protocol-version,last-event-id"),
    ], "");
    assert!((200..300).contains(&preflight.status), "{}", preflight.body);
    assert_eq!(preflight.header("access-control-allow-origin"), Some("*"));
    assert_ne!(
        preflight.header("access-control-allow-credentials"),
        Some("true")
    );
    let allowed = preflight
        .header("access-control-allow-headers")
        .expect("CORS headers")
        .to_ascii_lowercase();
    for header in [
        "content-type",
        "accept",
        "authorization",
        "mcp-session-id",
        "mcp-protocol-version",
        "last-event-id",
    ] {
        assert!(
            allowed.split(',').any(|item| item.trim() == header),
            "{allowed}"
        );
    }
    assert_not_ready(&post_json(port, &path, &body(), None));
    assert_not_ready(&post_json(port, "/mcp", &body(), Some(SECRET)));
    for query in ["token", "access_token", "api_key"] {
        assert_not_ready(&post_json(
            port,
            &format!("/mcp?{query}={SECRET}"),
            &body(),
            None,
        ));
    }
    let rejected = request(
        port,
        "POST",
        &path,
        &[
            ("Content-Type", "application/json"),
            ("Authorization", "Bearer another-credential"),
            ("Origin", EXTENSION_ORIGIN),
            ("Origin", "https://untrusted.example.test"),
        ],
        &body(),
    );
    assert_eq!(rejected.status, 400, "{}", rejected.body);
    assert_eq!(rejected.json()["error"], "CONFLICTING_CREDENTIAL_SOURCES");
    assert_eq!(rejected.header("cache-control"), Some("no-store"));
    assert_eq!(rejected.header("referrer-policy"), Some("no-referrer"));
    assert!(!rejected.body.contains(SECRET));
    let log = std::fs::read_to_string(env.home.path().join("logs/hub/mcp-requests.log"))
        .expect("rejection log");
    assert!(log.contains("[auth] rejected"));
    assert!(
        !log.contains(SECRET),
        "URL or header credential leaked into the request log"
    );
    assert!(!log.contains("another-credential"));
}

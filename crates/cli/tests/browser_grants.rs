//! 浏览器能力 URL：真实 CLI、隔离 GLD_HOME 和 HTTP 往返。
//!
//! 不借助网关，也不写真实配置。验证客户端握手、每次请求的授权边界以及凭据生命周期。

mod common;

use common::env::{free_port, Env};
use common::http::{post_json, request, Reply};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

const EXTENSION_ORIGIN: &str = "chrome-extension://kngiafgkdnlkgmefdafaibkibegkcaef";
const PROTOCOL: &str = "2025-06-18";

fn two_projects(env: &Env) -> (u16, tempfile::TempDir) {
    env.write("only-api.txt", "api\n");
    let web = tempfile::tempdir().expect("web dir");
    std::fs::write(web.path().join("only-web.txt"), "private web data\n").expect("web file");
    let port = free_port();
    env.ok(&["add", ".", "--name", "api"]);
    env.ok(&[
        "add",
        web.path().to_str().expect("utf-8 path"),
        "--name",
        "web",
    ]);
    env.ok(&["upgrade", "--port", &port.to_string(), "--auth", "oauth"]);
    env.ok(&["start"]);
    (port, web)
}

fn browser_path(created: &Value) -> String {
    let url = created["localUrl"].as_str().expect("localUrl");
    let (_, token) = url
        .split_once("/mcp/browser/")
        .expect("browser capability URL");
    assert!(!token.is_empty(), "URL 缺少能力凭据");
    format!("/mcp/browser/{token}")
}

fn post(
    port: u16,
    path: &str,
    origin: Option<&str>,
    protocol: Option<&str>,
    body: &Value,
) -> Reply {
    let mut headers = vec![
        ("Content-Type", "application/json"),
        ("Accept", "application/json, text/event-stream"),
    ];
    if let Some(origin) = origin {
        headers.push(("Origin", origin));
    }
    if let Some(protocol) = protocol {
        headers.push(("Mcp-Protocol-Version", protocol));
    }
    request(port, "POST", path, &headers, &body.to_string())
}

fn rpc(port: u16, path: &str, origin: Option<&str>, method: &str, params: Value) -> Reply {
    post(
        port,
        path,
        origin,
        Some(PROTOCOL),
        &json!({ "jsonrpc": "2.0", "id": 7, "method": method, "params": params }),
    )
}

fn call(port: u16, path: &str, name: &str, arguments: Value) -> Value {
    let reply = rpc(
        port,
        path,
        Some(EXTENSION_ORIGIN),
        "tools/call",
        json!({ "name": name, "arguments": arguments }),
    );
    assert_eq!(reply.status, 200, "{}", reply.body);
    let packet = reply.json();
    let text = packet["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("没有工具结果：{}", reply.body));
    serde_json::from_str(text).expect("工具结果是 JSON")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_secs()
}

/// 初始化不带协议头或 session；协商后发通知，再列工具、读项目。
/// 只读和项目范围必须在硬发 tools/call 时仍有效。
#[test]
fn browser_url_runs_stateless_mcp_with_scoped_read_only_tools() {
    let env = Env::new();
    let (port, _web) = two_projects(&env);
    let created = env.json(&["--json", "grant", "browser", "reader", "api", "--ttl", "8h"]);
    let path = browser_path(&created);
    assert_eq!(created["grant"]["readOnly"], true);
    assert_eq!(
        created["grant"]["browser"]["allowedOrigins"],
        json!([EXTENSION_ORIGIN])
    );

    let initialized = post(
        port,
        &path,
        Some(EXTENSION_ORIGIN),
        None,
        &json!({
            "jsonrpc": "2.0",
            "id": "browser-handshake",
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "browser-integration-test", "version": "1" }
            }
        }),
    );
    assert_eq!(initialized.status, 200, "{}", initialized.body);
    assert!(
        initialized
            .header("content-type")
            .is_some_and(|value| value.starts_with("application/json")),
        "握手必须支持 JSON 响应"
    );
    assert_eq!(initialized.json()["id"], "browser-handshake");
    assert_eq!(initialized.json()["result"]["protocolVersion"], PROTOCOL);
    assert!(initialized.header("mcp-session-id").is_none());
    assert_eq!(initialized.header("cache-control"), Some("no-store"));
    assert_eq!(initialized.header("referrer-policy"), Some("no-referrer"));

    let notification = post(
        port,
        &path,
        Some(EXTENSION_ORIGIN),
        Some(PROTOCOL),
        &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    );
    assert_eq!(notification.status, 202, "{}", notification.body);
    assert!(notification.body.is_empty(), "通知不能伪造 JSON-RPC 响应");
    assert!(notification.header("mcp-session-id").is_none());

    let listed = rpc(port, &path, Some(EXTENSION_ORIGIN), "tools/list", json!({}));
    assert_eq!(listed.status, 200, "{}", listed.body);
    let listed = listed.json();
    let tools: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(tools.contains(&"read_file"), "{tools:?}");
    assert!(!tools.contains(&"exec_command"), "{tools:?}");
    assert!(!tools.contains(&"apply_patch"), "{tools:?}");

    let workspaces = call(port, &path, "list_workspaces", json!({}));
    let names: Vec<&str> = workspaces["workspaces"]
        .as_array()
        .expect("workspaces")
        .iter()
        .filter_map(|workspace| workspace["name"].as_str())
        .collect();
    assert_eq!(names, vec!["api"], "{workspaces}");
    let read = call(
        port,
        &path,
        "read_file",
        json!({ "workspace": "api", "path": "only-api.txt" }),
    );
    assert_eq!(read["ok"], true, "{read}");
    assert_eq!(read["content"], "api\n", "{read}");

    let outside = call(
        port,
        &path,
        "read_file",
        json!({ "workspace": "web", "path": "only-web.txt" }),
    );
    assert_eq!(
        outside["error"]["code"], "WORKSPACE_NOT_IN_HUB",
        "{outside}"
    );
    assert!(!outside.to_string().contains("private web data"));

    for (name, arguments) in [
        ("exec_command", json!({ "workspace": "api", "cmd": "pwd" })),
        (
            "apply_patch",
            json!({
                "workspace": "api",
                "patch": "*** Begin Patch\n*** Add File: forbidden.txt\n+blocked\n*** End Patch\n"
            }),
        ),
    ] {
        let refused = rpc(
            port,
            &path,
            Some(EXTENSION_ORIGIN),
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        );
        assert_eq!(refused.status, 200, "{}", refused.body);
        assert_eq!(
            refused.json()["error"]["data"]["reason"],
            "unknown_tool",
            "{name}: {}",
            refused.body
        );
    }
    assert!(!env.project.path().join("forbidden.txt").exists());

    for method in ["GET", "DELETE"] {
        let reply = request(port, method, &path, &[("Origin", EXTENSION_ORIGIN)], "");
        assert_eq!(reply.status, 405, "{method}: {}", reply.body);
        let allow = reply.header("allow").expect("Allow");
        assert!(
            allow.split(',').any(|item| item.trim() == "POST"),
            "{allow}"
        );
        assert!(
            allow.split(',').any(|item| item.trim() == "OPTIONS"),
            "{allow}"
        );
    }

    // 新版客户端必须使用协商后的版本；不把任意头值或能力凭据反射到错误里。
    let unsupported = post(
        port,
        &path,
        Some(EXTENSION_ORIGIN),
        Some("2025-11-25"),
        &json!({ "jsonrpc": "2.0", "id": 8, "method": "tools/list", "params": {} }),
    );
    assert_eq!(unsupported.status, 400, "{}", unsupported.body);
    let token = path.strip_prefix("/mcp/browser/").expect("token");
    assert!(!unsupported.body.contains("2025-11-25"));
    assert!(!unsupported.body.contains(token));
    let unsupported_get = request(
        port,
        "GET",
        &path,
        &[
            ("Origin", EXTENSION_ORIGIN),
            ("Mcp-Protocol-Version", "2025-11-25"),
        ],
        "",
    );
    assert_eq!(unsupported_get.status, 400, "协议检查必须先于 405");
}

/// Origin 和路径凭据逐请求校验；预检无需凭据；删掉或到期后旧 URL 不再可用。
/// 浏览器凭据不能作为普通 /mcp 的 bearer 或 query token 使用。
#[test]
fn browser_credentials_enforce_origin_revocation_expiry_and_transport_isolation() {
    let env = Env::new();
    let (port, _web) = two_projects(&env);
    let create = || {
        env.json(&[
            "--json",
            "grant",
            "browser",
            "guest",
            "api",
            "--ttl",
            "8h",
            "--origin",
            "https://tools.example.test",
            "--origin",
            EXTENSION_ORIGIN,
        ])
    };
    let created = create();
    let path = browser_path(&created);
    let token = path.strip_prefix("/mcp/browser/").expect("token");

    for origin in [
        None,
        Some(EXTENSION_ORIGIN),
        Some("https://tools.example.test"),
    ] {
        let reply = rpc(port, &path, origin, "tools/list", json!({}));
        assert_eq!(reply.status, 200, "{origin:?}: {}", reply.body);
    }
    for origin in ["https://evil.example.test", "null"] {
        let reply = rpc(port, &path, Some(origin), "tools/list", json!({}));
        assert_eq!(reply.status, 401, "{origin}: {}", reply.body);
        assert_eq!(reply.header("cache-control"), Some("no-store"));
        assert_eq!(reply.header("referrer-policy"), Some("no-referrer"));
    }
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} });
    for origins in [
        [EXTENSION_ORIGIN, EXTENSION_ORIGIN],
        [EXTENSION_ORIGIN, "https://evil.example.test"],
    ] {
        let duplicate = request(
            port,
            "POST",
            &path,
            &[
                ("Content-Type", "application/json"),
                ("Accept", "application/json, text/event-stream"),
                ("Origin", origins[0]),
                ("Origin", origins[1]),
            ],
            &body.to_string(),
        );
        assert_eq!(
            duplicate.status, 401,
            "重复 Origin 被接受：{}",
            duplicate.body
        );
    }
    for method in ["GET", "DELETE"] {
        let reply = request(port, method, "/mcp/browser/not-a-capability", &[], "");
        assert_eq!(reply.status, 401, "{method}: {}", reply.body);
    }
    assert_eq!(
        rpc(
            port,
            "/mcp/browser/not-a-capability",
            None,
            "tools/list",
            json!({})
        )
        .status,
        401
    );

    let preflight = request(
        port,
        "OPTIONS",
        "/mcp/browser/not-a-capability",
        &[
            ("Origin", EXTENSION_ORIGIN),
            ("Access-Control-Request-Method", "POST"),
            (
                "Access-Control-Request-Headers",
                "content-type,accept,authorization,mcp-session-id,mcp-protocol-version,last-event-id",
            ),
        ],
        "",
    );
    assert!((200..300).contains(&preflight.status), "{}", preflight.body);
    assert_eq!(preflight.header("access-control-allow-origin"), Some("*"));
    assert_ne!(
        preflight.header("access-control-allow-credentials"),
        Some("true")
    );
    let allowed = preflight
        .header("access-control-allow-headers")
        .expect("CORS allow headers")
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
            "{header}: {allowed}"
        );
    }

    assert_eq!(
        post_json(port, "/mcp", &body.to_string(), Some(token)).status,
        401
    );
    for query in ["token", "access_token"] {
        let reply = post_json(
            port,
            &format!("/mcp?{query}={token}"),
            &body.to_string(),
            None,
        );
        assert_eq!(
            reply.status, 401,
            "普通 /mcp 接受了 URL 凭据：{}",
            reply.body
        );
    }

    env.ok(&["grant", "rm", "guest"]);
    assert_eq!(rpc(port, &path, None, "tools/list", json!({})).status, 401);
    let recreated = create();
    let fresh_path = browser_path(&recreated);
    assert_ne!(created["grant"]["id"], recreated["grant"]["id"]);
    assert_ne!(path, fresh_path);
    assert_eq!(rpc(port, &path, None, "tools/list", json!({})).status, 401);
    assert_eq!(
        rpc(port, &fresh_path, None, "tools/list", json!({})).status,
        200
    );

    // 只推进隔离 fixture 的过期时刻，不 sleep，也不改真实 grant。
    let profiles_path = env.home.path().join("data/profiles.json");
    let mut data: Value =
        serde_json::from_slice(&std::fs::read(&profiles_path).expect("isolated profiles"))
            .expect("profiles JSON");
    let record = data["grants"]
        .as_array_mut()
        .expect("grants")
        .iter_mut()
        .find(|record| record["id"] == recreated["grant"]["id"])
        .expect("recreated grant");
    record["browser"]["expires_at_unix_secs"] = json!(0);
    std::fs::write(&profiles_path, serde_json::to_vec_pretty(&data).unwrap())
        .expect("expire fixture");
    assert_eq!(
        rpc(port, &fresh_path, None, "tools/list", json!({})).status,
        401
    );
}

/// TTL 有明确边界，URL 只在创建时出现；持久化和 reveal 不能恢复它。
/// --write 必须显式选择，且真实写入只能落在测试临时目录里。
#[test]
fn browser_grant_cli_bounds_lifetime_hides_secrets_and_requires_write_opt_in() {
    let env = Env::new();
    let (port, _web) = two_projects(&env);

    for ttl in ["0s", "86401s", "18446744073709551616s", "tomorrow"] {
        let output = env.gld(&["grant", "browser", "invalid", "api", "--ttl", ttl]);
        assert!(!output.status.success(), "错误 TTL 被接受：{ttl}");
    }
    assert_eq!(env.json(&["--json", "grant", "ls"]), json!([]));

    for (name, ttl, seconds) in [("one-second", "1s", 1), ("one-day", "24h", 86_400)] {
        let before = now();
        let created = env.json(&["--json", "grant", "browser", name, "api", "--ttl", ttl]);
        let after = now();
        let expires = created["expiresAtUnixSecs"].as_u64().expect("expiry");
        assert!(
            (before + seconds..=after + seconds).contains(&expires),
            "{created}"
        );
        assert_eq!(created["grant"]["readOnly"], true);
    }

    let created = env.json(&[
        "--json", "grant", "browser", "writer", "api", "--ttl", "8h", "--write",
    ]);
    let path = browser_path(&created);
    let token = path.strip_prefix("/mcp/browser/").expect("token");
    assert_eq!(created["grant"]["readOnly"], false);
    assert_eq!(created["grant"]["oauthPassword"], "");
    assert_eq!(created["grant"]["bearerToken"], "");
    assert_eq!(created["publicUrl"], Value::Null);

    let stored_text = std::fs::read_to_string(env.home.path().join("data/profiles.json"))
        .expect("isolated profiles");
    assert!(!stored_text.contains(token), "持久化了原始 URL 凭据");
    let stored: Value = serde_json::from_str(&stored_text).expect("profiles JSON");
    let record = stored["grants"]
        .as_array()
        .expect("grants")
        .iter()
        .find(|record| record["id"] == created["grant"]["id"])
        .expect("writer record");
    assert_eq!(
        record["browser"]["token_sha256"],
        format!("{:x}", Sha256::digest(token.as_bytes()))
    );
    assert_eq!(record["oauth_password"], "");
    assert_eq!(record["bearer_token"], "");

    for args in [
        vec!["--json", "grant", "ls"],
        vec!["--json", "grant", "ls", "--reveal"],
        vec!["grant", "ls", "--reveal"],
    ] {
        let listed = env.ok(&args);
        assert!(!listed.contains(token), "列表泄露原始 URL 凭据");
    }

    let result = call(
        port,
        &path,
        "apply_patch",
        json!({
            "workspace": "api",
            "patch": "*** Begin Patch\n*** Add File: permitted.txt\n+written by explicit grant\n*** End Patch\n"
        }),
    );
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(
        std::fs::read_to_string(env.project.path().join("permitted.txt")).expect("written fixture"),
        "written by explicit grant\n"
    );
}

/// `--all` 展开成服务此刻的全部项目，之后新加的不自动进来；
/// 和点名项目不能同时给，两样都不给也不行。
#[test]
fn browser_grant_all_opens_every_current_project_only() {
    let env = Env::new();
    let (port, _web) = two_projects(&env);

    for args in [
        vec!["grant", "browser", "web-mcp"],
        vec!["grant", "browser", "web-mcp", "api", "--all"],
    ] {
        let output = env.gld(&args);
        assert_eq!(output.status.code(), Some(2), "用法错误应退出 2：{args:?}");
    }
    assert_eq!(env.json(&["--json", "grant", "ls"]), json!([]));

    let created = env.json(&["--json", "grant", "browser", "web-mcp", "--all"]);
    assert_eq!(created["grant"]["readOnly"], true);
    let mut granted: Vec<&str> = created["grant"]["workspaces"]
        .as_array()
        .expect("workspaces")
        .iter()
        .filter_map(|member| member["name"].as_str())
        .collect();
    granted.sort_unstable();
    assert_eq!(granted, vec!["api", "web"], "{created}");

    let later = tempfile::tempdir().expect("later dir");
    env.ok(&[
        "add",
        later.path().to_str().expect("utf-8 path"),
        "--name",
        "later",
    ]);
    // 对照：新项目确实进了服务，现在再建一把 --all 会带上它。
    let again = env.json(&["--json", "grant", "browser", "web-mcp-2", "--all"]);
    assert_eq!(
        again["grant"]["workspaces"]
            .as_array()
            .expect("workspaces")
            .len(),
        3,
        "{again}"
    );

    let path = browser_path(&created);
    let workspaces = call(port, &path, "list_workspaces", json!({}));
    let mut names: Vec<&str> = workspaces["workspaces"]
        .as_array()
        .expect("workspaces")
        .iter()
        .filter_map(|workspace| workspace["name"].as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(names, vec!["api", "web"], "{workspaces}");
    let read = call(
        port,
        &path,
        "read_file",
        json!({ "workspace": "web", "path": "only-web.txt" }),
    );
    assert!(read.to_string().contains("private web data"), "{read}");
}

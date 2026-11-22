//! D18-B3.4: `/mcp/connect/<secret>` 经真实 daemon/HTTP 验证 URL 用途的 grant 和它的 Origin 名单。
//! 旧的 `grant browser` 创建入口和 `/mcp/browser/…` 路径不是新凭据的别名；
//! 创建地址的 CLI（`grant add --via url`）和默认 Origin 属于 D。

mod common;

use common::env::{free_port, Env};
use common::http::{post_json, request, Reply};
use gld_core::auth::{
    AccessStore, CredentialKind, CredentialTtl, GrantCreation, GrantPermissions, ProjectScope,
};
use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const EXTENSION_ORIGIN: &str = "chrome-extension://kngiafgkdnlkgmefdafaibkibegkcaef";
const UNLISTED_ORIGIN: &str = "https://untrusted.example.test";
const SECRET: &str = "synthetic-url-secret-must-never-be-logged";
const CORS_HEADERS: [&str; 4] = [
    "access-control-allow-origin",
    "access-control-allow-methods",
    "access-control-allow-headers",
    "access-control-expose-headers",
];

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
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

struct Spec<'a> {
    name: &'a str,
    kind: CredentialKind,
    scope: ProjectScope,
    write: bool,
    ttl: Option<u64>,
    origins: &'a [&'a str],
}

impl<'a> Spec<'a> {
    fn url(name: &'a str, origins: &'a [&'a str]) -> Self {
        Self {
            name,
            kind: CredentialKind::Url,
            scope: ProjectScope::AllProjects,
            write: false,
            ttl: None,
            origins,
        }
    }
}

/// daemon 运行时持有授权存储的写锁，所以正式格式的凭据都在它停着时建立。
fn create(env: &Env, spec: Spec<'_>) -> String {
    let store = AccessStore::open_in(env.home.path()).unwrap();
    let permissions = GrantPermissions {
        write: spec.write,
        ..GrantPermissions::default()
    };
    store
        .create_grant(
            GrantCreation {
                name: spec.name.into(),
                scope: spec.scope,
                permissions,
                kind: spec.kind,
                ttl: spec.ttl.map(CredentialTtl::Seconds),
                allowed_origins: spec.origins.iter().map(|item| item.to_string()).collect(),
            },
            now(),
        )
        .unwrap()
        .secret
        .into_string()
}

/// 登记 api 和 other 两个项目并停下 daemon，返回 api 的项目 ID。
fn prepare_projects(env: &Env, other: &Path) -> String {
    env.write("visible.txt", "url-read-fixture\n");
    std::fs::write(other.join("other.txt"), "url-other-project-marker\n").unwrap();
    env.ok(&["add", ".", "--name", "api"]);
    env.ok(&["add", other.to_str().unwrap(), "--name", "other"]);
    let id = workspace_id(env, "api");
    let _ = env.gld(&["daemon", "stop"]);
    id
}

fn start(env: &Env) -> u16 {
    let port = free_port();
    env.ok(&["start", "--port", &port.to_string()]);
    port
}

fn body(method: &str, params: Value) -> String {
    json!({"jsonrpc":"2.0","id":7,"method":method,"params":params}).to_string()
}

fn connect(port: u16, secret: &str, origin: Option<&str>, method: &str, params: Value) -> Reply {
    let mut headers = vec![("Content-Type", "application/json")];
    if let Some(origin) = origin {
        headers.push(("Origin", origin));
    }
    request(
        port,
        "POST",
        &format!("/mcp/connect/{secret}"),
        &headers,
        &body(method, params),
    )
}

fn call(port: u16, secret: &str, origin: Option<&str>, name: &str, arguments: Value) -> Reply {
    connect(
        port,
        secret,
        origin,
        "tools/call",
        json!({"name": name, "arguments": arguments}),
    )
}

fn preflight(port: u16, secret: &str, origin: &str) -> Reply {
    request(
        port,
        "OPTIONS",
        &format!("/mcp/connect/{secret}"),
        &[
            ("Origin", origin),
            ("Access-Control-Request-Method", "POST"),
            (
                "Access-Control-Request-Headers",
                "content-type,mcp-session-id,mcp-protocol-version",
            ),
        ],
        "",
    )
}

/// 这条路由的每个响应都禁止缓存、不带 Referer、按 Origin 区分，并且从不回显秘密或允许 cookie。
fn assert_url_reply(reply: &Reply, status: u16, secrets: &[&str]) {
    assert_eq!(reply.status, status, "{}", reply.body);
    assert_eq!(reply.header("cache-control"), Some("no-store"));
    assert_eq!(reply.header("referrer-policy"), Some("no-referrer"));
    assert_eq!(reply.header("vary"), Some("Origin"));
    assert!(reply.header("access-control-allow-credentials").is_none());
    assert!(reply.header("www-authenticate").is_none());
    assert_ne!(reply.header("access-control-allow-origin"), Some("*"));
    for secret in secrets {
        assert!(
            !reply.body.contains(secret),
            "secret echoed: {}",
            reply.body
        );
    }
}

/// 通过核对的请求回显它自己的 Origin，并让跨源脚本读得到会话 ID。
fn assert_cors_for(reply: &Reply, origin: &str) {
    assert_eq!(reply.header("access-control-allow-origin"), Some(origin));
    let exposed = reply
        .header("access-control-expose-headers")
        .expect("expose headers")
        .to_ascii_lowercase();
    for name in ["mcp-session-id", "mcp-protocol-version"] {
        assert!(
            exposed.split(',').any(|item| item.trim() == name),
            "{exposed}"
        );
    }
}

fn assert_no_cors(reply: &Reply) {
    for name in CORS_HEADERS {
        assert!(reply.header(name).is_none(), "{name} on {}", reply.status);
    }
}

fn assert_refused(reply: &Reply, status: u16, error: &str, secrets: &[&str]) {
    assert_url_reply(reply, status, secrets);
    assert_eq!(reply.json()["error"], error, "{}", reply.body);
    assert!(reply.json().get("result").is_none());
    assert_no_cors(reply);
}

fn is_tool_error(reply: &Reply) -> bool {
    let value = reply.json();
    value["result"]["isError"] == true || value["error"].is_object()
}

/// 隔离数据目录下的任何文件（日志、运行记录、授权存储）都不能含有地址里的秘密。
/// 存储只留摘要，所以这里不需要排除任何文件。
fn assert_nothing_on_disk_contains(env: &Env, secrets: &[&str], after: &str) {
    fn visit(dir: &Path, secrets: &[&str], after: &str, seen: &mut usize) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit(&path, secrets, after, seen);
            } else if let Ok(bytes) = std::fs::read(&path) {
                *seen += 1;
                let text = String::from_utf8_lossy(&bytes);
                for secret in secrets {
                    assert!(
                        !text.contains(secret),
                        "{after} 之后 {} 里出现了地址秘密",
                        path.display()
                    );
                }
            }
        }
    }
    let mut seen = 0;
    visit(env.home.path(), secrets, after, &mut seen);
    assert!(
        env.home.path().join("logs/hub/mcp-requests.log").exists(),
        "请求日志不存在，检查没有覆盖到它"
    );
    assert!(seen > 0);
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
fn url_address_reaches_tools_only_with_a_valid_secret_and_a_listed_origin() {
    let env = Env::new();
    let other = tempfile::tempdir().unwrap();
    let api_id = prepare_projects(&env, other.path());
    let origins = [EXTENSION_ORIGIN, "https://app.example.test"];
    let reader = create(
        &env,
        Spec {
            scope: ProjectScope::Selected(vec![api_id]),
            ..Spec::url("reader", &origins)
        },
    );
    let writer = create(
        &env,
        Spec {
            write: true,
            ..Spec::url("writer", &origins[..1])
        },
    );
    let bare = create(&env, Spec::url("bare", &[]));
    let short = create(
        &env,
        Spec {
            ttl: Some(3),
            ..Spec::url("short", &[])
        },
    );
    let token = create(
        &env,
        Spec {
            kind: CredentialKind::Token,
            ..Spec::url("script", &[])
        },
    );
    let oauth = create(
        &env,
        Spec {
            kind: CredentialKind::OAuth,
            ..Spec::url("chat", &[])
        },
    );
    let wrong = format!("{reader}x");
    let secrets = [
        reader.as_str(),
        writer.as_str(),
        bare.as_str(),
        short.as_str(),
        wrong.as_str(),
    ];
    let port = start(&env);
    let checked = |after: &str| assert_nothing_on_disk_contains(&env, &secrets, after);

    // 限期地址先验，免得后面的步骤把 3 秒用完。
    let fresh = connect(port, &short, None, "tools/list", json!({}));
    assert_url_reply(&fresh, 200, &secrets);

    // 成功：带名单内的 Origin 和不带 Origin 都能 initialize、列工具、读文件。
    let initialize = json!({
        "protocolVersion":"2025-06-18",
        "capabilities":{},
        "clientInfo":{"name":"url-test","version":"1"}
    });
    for origin in [
        Some(EXTENSION_ORIGIN),
        Some("https://app.example.test"),
        None,
    ] {
        let initialized = connect(port, &reader, origin, "initialize", initialize.clone());
        assert_url_reply(&initialized, 200, &secrets);
        assert_eq!(
            initialized.json()["result"]["protocolVersion"],
            "2025-06-18"
        );
        let listed = connect(port, &reader, origin, "tools/list", json!({}));
        assert_url_reply(&listed, 200, &secrets);
        let tools = listed.json()["result"]["tools"].as_array().unwrap().clone();
        assert!(tools.iter().any(|tool| tool["name"] == "read_file"));
        assert!(!tools.iter().any(|tool| tool["name"] == "apply_patch"));
        let read = call(
            port,
            &reader,
            origin,
            "read_file",
            json!({"workspace":"api","path":"visible.txt"}),
        );
        assert_url_reply(&read, 200, &secrets);
        assert!(read.body.contains("url-read-fixture"), "{}", read.body);
        for reply in [&initialized, &listed, &read] {
            match origin {
                Some(origin) => assert_cors_for(reply, origin),
                None => assert_no_cors(reply),
            }
        }
    }
    checked("成功的请求");

    // 范围和写权限与别的凭据一样：点名项目之外读不到，只读地址写不了。
    let outside = call(
        port,
        &reader,
        Some(EXTENSION_ORIGIN),
        "read_file",
        json!({"workspace":"other","path":"other.txt"}),
    );
    assert!(is_tool_error(&outside), "{}", outside.body);
    assert!(!outside.body.contains("url-other-project-marker"));
    let patch = |file: &str| {
        json!({"workspace":"api","patch": format!(
            "*** Begin Patch\n*** Add File: {file}\n+written\n*** End Patch\n"
        )})
    };
    let denied = call(
        port,
        &reader,
        Some(EXTENSION_ORIGIN),
        "apply_patch",
        patch("url-readonly.txt"),
    );
    assert!(is_tool_error(&denied), "{}", denied.body);
    assert!(!env.project.path().join("url-readonly.txt").exists());
    let written = call(
        port,
        &writer,
        Some(EXTENSION_ORIGIN),
        "apply_patch",
        patch("url-writer.txt"),
    );
    assert_url_reply(&written, 200, &secrets);
    assert!(!is_tool_error(&written), "{}", written.body);
    assert!(env.project.path().join("url-writer.txt").exists());
    // 可写的 URL 地址也不能委派 Agent。
    let delegated = call(
        port,
        &writer,
        Some(EXTENSION_ORIGIN),
        "agent_manage",
        json!({"workspace":"api","action":"start","agent":"codex","prompt":"noop"}),
    );
    assert!(is_tool_error(&delegated), "{}", delegated.body);
    assert!(
        delegated.body.contains("AGENT_DELEGATION_NOT_ALLOWED"),
        "{}",
        delegated.body
    );

    // 403：秘密有效，Origin 不在这份 grant 的名单里。不做换算，`null` 也不行。
    for (secret, origin) in [
        (&reader, UNLISTED_ORIGIN),
        (&reader, "null"),
        (&reader, "https://app.example.test/"),
        (&reader, "https://APP.example.test"),
        // writer 的名单只有扩展；reader 名单里的网页 Origin 不能借用。
        (&writer, "https://app.example.test"),
        // 名单为空的地址只接受不带 Origin 的请求。
        (&bare, EXTENSION_ORIGIN),
    ] {
        let refused = call(
            port,
            secret,
            Some(origin),
            "read_file",
            json!({"workspace":"api","path":"visible.txt"}),
        );
        assert_refused(&refused, 403, "ORIGIN_NOT_ALLOWED", &secrets);
        assert!(!refused.body.contains("url-read-fixture"));
    }
    assert_url_reply(
        &connect(port, &bare, None, "tools/list", json!({})),
        200,
        &secrets,
    );
    checked("403");

    // 401：秘密不对时，Origin 在不在名单里都是同一个回包；别的用途的秘密也不行。
    for candidate in [wrong.as_str(), SECRET, token.as_str(), oauth.as_str()] {
        for origin in [Some(EXTENSION_ORIGIN), Some(UNLISTED_ORIGIN), None] {
            let refused = connect(port, candidate, origin, "tools/list", json!({}));
            assert_refused(&refused, 401, "AUTHENTICATION_FAILED", &secrets);
        }
    }
    checked("401");

    // 400：Origin 头重复，或路径秘密之外又带了 Authorization。
    let path = format!("/mcp/connect/{reader}");
    let doubled = request(
        port,
        "POST",
        &path,
        &[
            ("Content-Type", "application/json"),
            ("Origin", EXTENSION_ORIGIN),
            ("Origin", EXTENSION_ORIGIN),
        ],
        &body("tools/list", json!({})),
    );
    assert_refused(&doubled, 400, "INVALID_ORIGIN", &secrets);
    let conflict = request(
        port,
        "POST",
        &path,
        &[
            ("Content-Type", "application/json"),
            ("Origin", EXTENSION_ORIGIN),
            ("Authorization", &format!("Bearer {token}")),
        ],
        &body("tools/list", json!({})),
    );
    assert_refused(&conflict, 400, "CONFLICTING_CREDENTIAL_SOURCES", &secrets);
    checked("400");

    // 地址秘密只在自己的路径上有效：当 Bearer 或查询串发到 /mcp 都是 401。
    let list = body("tools/list", json!({}));
    assert_eq!(post_json(port, "/mcp", &list, Some(&reader)).status, 401);
    for query in ["token", "access_token", "api_key"] {
        let reply = post_json(port, &format!("/mcp?{query}={reader}"), &list, None);
        assert_eq!(reply.status, 401, "{}", reply.body);
        assert!(!reply.body.contains(reader.as_str()));
    }
    // 旧路径不是别名。
    let retired = post_json(port, &format!("/mcp/browser/{reader}"), &list, None);
    assert_eq!(retired.status, 404, "{}", retired.body);
    checked("错误入口");

    // GET / DELETE：秘密有效是 405，不开 SSE 流；秘密或 Origin 不对和 POST 一样。
    for method in ["GET", "DELETE"] {
        let allowed = request(port, method, &path, &[("Origin", EXTENSION_ORIGIN)], "");
        assert_url_reply(&allowed, 405, &secrets);
        assert_eq!(allowed.header("allow"), Some("POST, OPTIONS"));
        assert_cors_for(&allowed, EXTENSION_ORIGIN);
        let unlisted = request(port, method, &path, &[("Origin", UNLISTED_ORIGIN)], "");
        assert_refused(&unlisted, 403, "ORIGIN_NOT_ALLOWED", &secrets);
        let unknown = request(
            port,
            method,
            &format!("/mcp/connect/{wrong}"),
            &[("Origin", EXTENSION_ORIGIN)],
            "",
        );
        assert_refused(&unknown, 401, "AUTHENTICATION_FAILED", &secrets);
    }
    checked("405");

    // 预检：只有随后的 POST 会被接受时才回显 Origin 和允许的方法、请求头。
    let allowed = preflight(port, &reader, EXTENSION_ORIGIN);
    assert_url_reply(&allowed, 204, &secrets);
    assert_cors_for(&allowed, EXTENSION_ORIGIN);
    assert_eq!(
        allowed.header("access-control-allow-methods"),
        Some("POST, OPTIONS")
    );
    let request_headers = allowed
        .header("access-control-allow-headers")
        .expect("allowed request headers")
        .to_ascii_lowercase();
    for name in [
        "content-type",
        "accept",
        "mcp-session-id",
        "mcp-protocol-version",
        "last-event-id",
    ] {
        assert!(
            request_headers.split(',').any(|item| item.trim() == name),
            "{request_headers}"
        );
    }
    // Authorization 在这条路由上必然是 400，所以预检不许可它。
    assert!(!request_headers.contains("authorization"));
    for (secret, origin) in [
        (reader.as_str(), UNLISTED_ORIGIN),
        (reader.as_str(), "null"),
        (wrong.as_str(), EXTENSION_ORIGIN),
        (bare.as_str(), EXTENSION_ORIGIN),
        (token.as_str(), EXTENSION_ORIGIN),
    ] {
        let refused = preflight(port, secret, origin);
        assert_url_reply(&refused, 204, &secrets);
        assert_no_cors(&refused);
        assert!(refused.body.is_empty());
    }
    checked("预检");

    // 到期：同一个地址在期限过后就是 401，不会自动续期。
    std::thread::sleep(Duration::from_secs(4));
    let expired = connect(port, &short, None, "tools/list", json!({}));
    assert_refused(&expired, 401, "AUTHENTICATION_FAILED", &secrets);
    checked("到期");

    let log = std::fs::read_to_string(env.home.path().join("logs/hub/mcp-requests.log")).unwrap();
    assert!(
        log.contains("[auth] rejected status=403 entry=url"),
        "{log}"
    );
    assert!(
        log.contains("[auth] rejected status=401 entry=url"),
        "{log}"
    );
    assert!(
        !log.contains("/mcp/connect/"),
        "请求日志不该记这条路由的路径"
    );
}

#[test]
fn rotation_keeps_the_origin_list_and_revocation_blocks_the_next_request() {
    let env = Env::new();
    let other = tempfile::tempdir().unwrap();
    prepare_projects(&env, other.path());
    let first = create(&env, Spec::url("sidebar", &[EXTENSION_ORIGIN]));
    let token = create(
        &env,
        Spec {
            kind: CredentialKind::Token,
            ..Spec::url("script", &[])
        },
    );
    let port = start(&env);
    let read = |secret: &str, origin: Option<&str>| {
        call(
            port,
            secret,
            origin,
            "read_file",
            json!({"workspace":"api","path":"visible.txt"}),
        )
    };
    let before = read(&first, Some(EXTENSION_ORIGIN));
    assert_url_reply(&before, 200, &[&first]);
    assert!(before.body.contains("url-read-fixture"));

    // 轮换（daemon 停着时用正式存储做）：GrantId 和 Origin 名单不变，修订号加一。
    env.ok(&["daemon", "stop"]);
    let second = {
        let store = AccessStore::open_in(env.home.path()).unwrap();
        let issued = store.rotate_grant("sidebar", None, now()).unwrap();
        assert_eq!(issued.record.credential.revision, 2);
        assert_eq!(
            issued.record.credential.allowed_origins,
            vec![EXTENSION_ORIGIN.to_string()]
        );
        issued.secret.into_string()
    };
    env.ok(&["start"]);
    let secrets = [first.as_str(), second.as_str()];
    for origin in [Some(EXTENSION_ORIGIN), Some(UNLISTED_ORIGIN), None] {
        assert_refused(
            &read(&first, origin),
            401,
            "AUTHENTICATION_FAILED",
            &secrets,
        );
    }
    let after = read(&second, Some(EXTENSION_ORIGIN));
    assert_url_reply(&after, 200, &secrets);
    assert_cors_for(&after, EXTENSION_ORIGIN);
    assert!(after.body.contains("url-read-fixture"));
    assert_refused(
        &read(&second, Some(UNLISTED_ORIGIN)),
        403,
        "ORIGIN_NOT_ALLOWED",
        &secrets,
    );
    assert_nothing_on_disk_contains(&env, &secrets, "轮换");

    // 撤销（运行中的 daemon）：下一个请求就是 401，也不再透露 Origin 是否在名单里。
    env.ok(&["grant", "rm", "sidebar"]);
    for origin in [Some(EXTENSION_ORIGIN), Some(UNLISTED_ORIGIN), None] {
        assert_refused(
            &read(&second, origin),
            401,
            "AUTHENTICATION_FAILED",
            &secrets,
        );
    }
    let refused = preflight(port, &second, EXTENSION_ORIGIN);
    assert_url_reply(&refused, 204, &secrets);
    assert_no_cors(&refused);
    // 别的 grant 不受影响。
    let list = body("tools/list", json!({}));
    assert_eq!(post_json(port, "/mcp", &list, Some(&token)).status, 200);
    assert_nothing_on_disk_contains(&env, &secrets, "撤销");
}

#[test]
fn without_any_grant_the_url_entry_stays_closed() {
    let env = Env::new();
    env.write("private.txt", "url-private-marker\n");
    env.ok(&["add", ".", "--name", "api"]);
    let port = start(&env);
    let path = format!("/mcp/connect/{SECRET}");
    let list = body("tools/list", json!({}));
    for method in ["POST", "GET", "DELETE"] {
        let old = request(
            port,
            method,
            &format!("/mcp/browser/{SECRET}"),
            &[("Content-Type", "application/json")],
            &list,
        );
        assert_eq!(old.status, 404, "{method}: {}", old.body);
        assert!(!old.body.contains(SECRET));
        for origin in [EXTENSION_ORIGIN, UNLISTED_ORIGIN, "null"] {
            let reply = request(
                port,
                method,
                &path,
                &[("Content-Type", "application/json"), ("Origin", origin)],
                &list,
            );
            assert_refused(&reply, 503, "AUTH_NOT_READY", &[SECRET]);
            assert!(!reply.body.contains("url-private-marker"));
        }
    }
    let refused = preflight(port, SECRET, EXTENSION_ORIGIN);
    assert_url_reply(&refused, 204, &[SECRET]);
    assert_no_cors(&refused);
    let patch = json!({"name":"apply_patch","arguments":{"workspace":"api","patch":
        "*** Begin Patch\n*** Add File: forbidden.txt\n+blocked\n*** End Patch\n"}});
    let reply = post_json(port, &path, &body("tools/call", patch), None);
    assert_refused(&reply, 503, "AUTH_NOT_READY", &[SECRET]);
    assert!(!env.project.path().join("forbidden.txt").exists());
    let conflict = request(
        port,
        "POST",
        &path,
        &[
            ("Content-Type", "application/json"),
            ("Authorization", "Bearer another-credential"),
            ("Origin", EXTENSION_ORIGIN),
            ("Origin", UNLISTED_ORIGIN),
        ],
        &list,
    );
    assert_refused(&conflict, 400, "CONFLICTING_CREDENTIAL_SOURCES", &[SECRET]);
    assert_nothing_on_disk_contains(&env, &[SECRET, "another-credential"], "未授权的请求");
    let log = std::fs::read_to_string(env.home.path().join("logs/hub/mcp-requests.log")).unwrap();
    assert!(
        log.contains("[auth] rejected status=503 entry=url"),
        "{log}"
    );
}

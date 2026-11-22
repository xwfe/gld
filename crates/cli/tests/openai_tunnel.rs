//! D18-A / B3.5: a tunnel's transport credentials do not authorize MCP requests by themselves.
//! The tunnel key only proves the source; tool access comes from the token grant the tunnel is
//! explicitly bound to in the access store, at the credential revision current when it was bound.
//! One fixture probes GET discovery solely to test process arguments and cleanup.
//! The authorization fixtures perform a real MCP initialize.

#![cfg(unix)]

mod common;

use common::env::{free_port, Env};
use common::http::{get, post_json, request, Reply};
use gld_core::auth::{
    openai_tunnel_name, AccessStore, CredentialKind, GrantCreation, GrantPermissions, ProjectScope,
};
use serde_json::{json, Value};

const TUNNEL_ID: &str = "tunnel_0123456789abcdef0123456789abcdef";

/// `probe_mcp=false` tests listener/process plumbing only; it never reports MCP authentication.
fn fake_tunnel_client(seen: &std::path::Path, probe_mcp: bool) -> String {
    format!(
        r#"#!/usr/bin/env python3
import json, os, sys, http.server, urllib.request, urllib.error
args = sys.argv[1:]
def flag(name):
    return [args[i + 1] for i, a in enumerate(args) if a == name and i + 1 < len(args)]
observed = {{"pid": os.getpid(), "args": args,
            "runtime_key": os.environ.get("GLD_OPENAI_RUNTIME_KEY"),
            "tunnel_key": os.environ.get("GLD_OPENAI_TUNNEL_KEY"),
            "no_proxy": os.environ.get("NO_PROXY", ""),
            "probe_kind": "mcp_initialize" if {probe_mcp} else "listener_discovery"}}
name, value = flag("--mcp.discovery-extra-headers")[0].split(": ", 1)
if value.startswith("env:"):
    value = os.environ.get(value[4:], "")
headers = {{name: value, "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream"}}
if {probe_mcp}:
    body = json.dumps({{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{
        "protocolVersion":"2025-06-18","capabilities":{{}},
        "clientInfo":{{"name":"fake-tunnel","version":"0"}}}}}}).encode()
    probe = urllib.request.Request(flag("--mcp.server-url")[0], data=body, headers=headers)
else:
    probe = urllib.request.Request(flag("--mcp.server-url")[0], headers=headers)
opener = urllib.request.build_opener(urllib.request.ProxyHandler({{}}))
try:
    with opener.open(probe, timeout=5) as response:
        observed["probe_status"] = response.status
        observed["probe_body"] = response.read().decode()
    status = "ready" if {probe_mcp} else "ready (listener discovery only)"
except urllib.error.HTTPError as error:
    observed["probe_status"] = error.code
    observed["probe_body"] = error.read().decode()
    status = "ready (mcp initialize requires auth: %d)" % error.code if error.code == 401 else "mcp probe failed: %d" % error.code
with open({seen:?}, "w") as f:
    json.dump(observed, f)
host, port = flag("--health.listen-addr")[0].rsplit(":", 1)
class Health(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        reply = status.encode()
        self.send_response(200 if status.startswith("ready") else 503)
        self.send_header("Content-Length", str(len(reply)))
        self.end_headers()
        self.wfile.write(reply)
    def log_message(self, *_):
        pass
http.server.HTTPServer((host, int(port)), Health).serve_forever()
"#,
        seen = seen.display().to_string(),
        probe_mcp = if probe_mcp { "True" } else { "False" },
    )
}

fn initialize_with(port: u16, key: &str) -> Reply {
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "test", "version": "0"}}
    })
    .to_string();
    request(
        port,
        "POST",
        "/mcp",
        &[
            ("Content-Type", "application/json"),
            ("X-Gld-Tunnel-Key", key),
        ],
        &body,
    )
}

fn process_alive(pid: u64) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[test]
fn tunnel_process_arguments_and_cleanup_do_not_grant_mcp_authority() {
    let mut env = Env::new();
    let seen = env.home.path().join("tunnel-client-seen.json");
    env.fake_binary("tunnel-client", &fake_tunnel_client(&seen, false));
    env.write("unchanged.txt", "tunnel-private-marker\n");
    env.ok(&["add", ".", "--name", "tunnel"]);
    let port = free_port();
    let tunnel = format!("openai:{TUNNEL_ID}");
    let state = env.json(&[
        "--json",
        "start",
        ".",
        "--port",
        &port.to_string(),
        "--tunnel",
        &tunnel,
        "--token",
        "fixture-runtime-key",
    ]);
    assert_eq!(state["service"]["state"], "running", "{state}");
    assert_eq!(state["service"]["tunnelError"], "", "{state}");
    assert_eq!(state["service"]["publicEndpoint"], "", "{state}");
    assert_eq!(state["service"]["config"]["openaiTunnelId"], TUNNEL_ID);

    let seen_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&seen).expect("假 tunnel-client 没被调起来"))
            .unwrap();
    let args: Vec<&str> = seen_json["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap())
        .collect();
    let joined = args.join(" ");
    assert_eq!(args.first(), Some(&"run"), "{joined}");
    assert!(
        joined.contains(&format!("--control-plane.tunnel-id {TUNNEL_ID}")),
        "{joined}"
    );
    assert!(
        joined.contains(&format!("--mcp.server-url http://127.0.0.1:{port}/mcp")),
        "{joined}"
    );
    // 密钥只经环境变量交过去，argv 里只有 env: 引用。
    assert!(
        joined.contains("--control-plane.api-key env:GLD_OPENAI_RUNTIME_KEY"),
        "{joined}"
    );
    assert!(!joined.contains("fixture-runtime-key"), "{joined}");
    assert_eq!(seen_json["runtime_key"], "fixture-runtime-key");
    assert_eq!(seen_json["probe_kind"], "listener_discovery");
    assert_eq!(seen_json["probe_status"], 200);
    let tunnel_key = env.service_secret("openai_tunnel_key");
    assert!(!tunnel_key.is_empty());
    assert_eq!(seen_json["tunnel_key"], tunnel_key.as_str());
    assert!(!joined.contains(&tunnel_key), "{joined}");
    assert!(
        seen_json["no_proxy"]
            .as_str()
            .unwrap()
            .contains("127.0.0.1"),
        "{seen_json}"
    );

    // A successful listener probe says nothing about MCP authority.
    assert_eq!(state["service"]["config"]["authType"], "oauth");
    // 这台机器还没有任何 grant：密钥对是"未就绪"，密钥不对是不带 OAuth 发现信息的 401。
    for (key, status, error) in [
        (tunnel_key.as_str(), 503, "AUTH_NOT_READY"),
        ("wrong", 401, "AUTHENTICATION_FAILED"),
    ] {
        let refused = initialize_with(port, key);
        assert_eq!(refused.status, status, "{}", refused.body);
        assert_eq!(refused.json()["error"], error);
        assert_eq!(refused.header("cache-control"), Some("no-store"));
        assert_eq!(refused.header("www-authenticate"), None);
        assert!(!refused.body.contains(key));
    }
    let write = request(port, "POST", "/mcp", &[
        ("Content-Type", "application/json"), ("X-Gld-Tunnel-Key", tunnel_key.as_str())
    ], &serde_json::json!({
        "jsonrpc":"2.0","id":2,"method":"tools/call","params":{
            "name":"apply_patch","arguments":{
                "workspace":"tunnel",
                "patch":"*** Begin Patch\n*** Add File: forbidden.txt\n+blocked\n*** End Patch\n"
            }
        }
    }).to_string());
    assert_eq!(write.status, 503, "{}", write.body);
    assert_eq!(write.json()["error"], "AUTH_NOT_READY");
    assert!(!env.project.path().join("forbidden.txt").exists());
    assert_eq!(
        std::fs::read_to_string(env.project.path().join("unchanged.txt")).unwrap(),
        "tunnel-private-marker\n"
    );
    let log = std::fs::read_to_string(env.home.path().join("logs/hub/mcp-requests.log")).unwrap();
    assert!(!log.contains(&tunnel_key));
    assert!(!log.contains("fixture-runtime-key"));

    let listing = env.ok(&["ls"]);
    assert!(
        listing.contains(&format!("OpenAI 隧道 {TUNNEL_ID}")),
        "{listing}"
    );
    assert!(!listing.contains("fixture-runtime-key"));
    assert!(!listing.contains(&tunnel_key));

    // Stored transport settings can relaunch the listener-only fixture; this is not an MCP success.
    env.ok(&["share", "--tunnel", "openai"]);

    let pid = serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&seen).unwrap())
        .unwrap()["pid"]
        .as_u64()
        .unwrap();
    assert!(process_alive(pid));
    env.ok(&["stop"]);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while process_alive(pid) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(!process_alive(pid), "服务停了，tunnel-client 还在");
}

/// A real initialize probe gets 503 even with the configured tunnel header; startup must fail.
#[test]
fn a_tunnel_whose_probe_is_refused_is_reported_and_the_service_keeps_running() {
    let mut env = Env::new();
    let seen = env.home.path().join("tunnel-client-seen.json");
    env.fake_binary("tunnel-client", &fake_tunnel_client(&seen, true));
    let port = free_port();
    let output = env.gld_with_env(
        &[
            "start",
            ".",
            "--port",
            &port.to_string(),
            "--tunnel",
            &format!("openai:{TUNNEL_ID}"),
        ],
        &[("CONTROL_PLANE_API_KEY", "fixture-from-env")],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("503") && stderr.contains("tunnel-client"),
        "{stderr}"
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("fixture-from-env"));
    assert!(!stderr.contains("fixture-from-env"), "凭据泄露：{stderr}");
    // Runtime Key 取自环境变量 CONTROL_PLANE_API_KEY 并存下了。
    assert_eq!(env.service_secret("openai_runtime_key"), "fixture-from-env");
    let state = env.json(&["--json", "ls"]);
    assert_eq!(state["service"]["state"], "running", "{state}");
    let observed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&seen).unwrap()).unwrap();
    assert_eq!(observed["probe_kind"], "mcp_initialize");
    assert_eq!(observed["probe_status"], 503);
    let probe_body: serde_json::Value =
        serde_json::from_str(observed["probe_body"].as_str().unwrap()).unwrap();
    assert_eq!(probe_body["error"], "AUTH_NOT_READY");
    let tunnel_key = env.service_secret("openai_tunnel_key");
    assert!(!stderr.contains(&tunnel_key));
    assert_eq!(
        get(port, "/mcp").status,
        200,
        "the local listener was stopped with the failed tunnel"
    );
    let refused = initialize_with(port, &tunnel_key);
    assert_eq!(refused.status, 503);
    assert_eq!(refused.json()["error"], "AUTH_NOT_READY");
    let pid = observed["pid"].as_u64().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while process_alive(pid) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(!process_alive(pid), "没起好的 tunnel-client 要停掉");
}

#[test]
fn an_openai_tunnel_needs_an_id_and_refuses_a_pasted_key() {
    let env = Env::new();
    let output = env.gld(&["share", "--tunnel", "openai:sk-proj-fixture"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("tunnel_"), "{stderr}");
    // 没配过隧道 ID 时光写 openai 不能往下走。
    let output = env.gld_with_env(
        &["share", "--tunnel", "openai"],
        &[("CONTROL_PLANE_API_KEY", "fixture")],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("隧道 ID"), "{stderr}");
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn tunnel_rpc(port: u16, key: &str, method: &str, params: Value) -> Reply {
    request(
        port,
        "POST",
        "/mcp",
        &[
            ("Content-Type", "application/json"),
            ("X-Gld-Tunnel-Key", key),
        ],
        &json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}).to_string(),
    )
}

fn assert_not_bound(reply: &Reply, key: &str) {
    assert_eq!(reply.status, 403, "{}", reply.body);
    assert_eq!(reply.json()["error"], "TUNNEL_NOT_BOUND");
    assert_eq!(reply.header("cache-control"), Some("no-store"));
    assert_eq!(reply.header("www-authenticate"), None);
    assert!(!reply.body.contains(key));
}

/// 隧道因为没绑定起不来时，用户看到的话要说清差的是绑定，而且本地服务照常在跑。
/// 新起隧道时这句话在命令的报错里；守护进程重启后自己恢复服务时，它在 `gld ls` 的隧道错误里。
fn assert_start_reports_unbound(env: &Env, output: &std::process::Output, port: u16) {
    let state = env.json(&["--json", "ls"]);
    let message = if output.status.success() {
        state["service"]["tunnelError"]
            .as_str()
            .unwrap()
            .to_string()
    } else {
        String::from_utf8_lossy(&output.stderr).into_owned()
    };
    for needle in [
        "还没有绑定到 grant",
        "TUNNEL_NOT_BOUND",
        "绑定命令还没有提供",
    ] {
        assert!(
            message.contains(needle),
            "缺少「{needle}」：{message} / {state}"
        );
    }
    assert_eq!(state["service"]["state"], "running", "{state}");
    assert_eq!(get(port, "/mcp").status, 200);
}

/// 遍历整个隔离数据目录：`nowhere` 里的秘密不许出现在任何文件里；
/// `stored` 里的（隧道密钥、Runtime Key）只许出现在服务凭据文件里。
fn assert_secrets_stay_put(env: &Env, after: &str, nowhere: &[&str], stored: &[&str]) {
    fn visit(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit(&path, files);
            } else {
                files.push(path);
            }
        }
    }
    let mut files = Vec::new();
    visit(env.home.path(), &mut files);
    assert!(files.len() > 3, "{after}：数据目录没扫到文件");
    let mut holders = std::collections::BTreeSet::new();
    for path in &files {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        let name = path.strip_prefix(env.home.path()).unwrap().to_path_buf();
        for secret in nowhere {
            assert!(
                !text.contains(secret),
                "{after} 之后 {} 里出现了 token 秘密",
                name.display()
            );
        }
        if stored.iter().any(|secret| text.contains(secret)) {
            holders.insert(name);
        }
    }
    // 隧道密钥和 Runtime Key 是服务凭据，只存在服务凭据所在的 data/profiles.json 里；
    // 另一处是测试自己的假 tunnel-client 记下的环境变量。日志、授权文件等别处都不许有。
    let expected = ["data/profiles.json", "tunnel-client-seen.json"];
    for holder in &holders {
        assert!(
            expected
                .iter()
                .any(|name| holder == std::path::Path::new(name)),
            "{after} 之后 {} 里出现了隧道密钥或 Runtime Key",
            holder.display()
        );
    }
}

#[test]
fn a_bound_tunnel_gets_exactly_its_grant_until_rotation_or_revocation() {
    let mut env = Env::new();
    let seen = env.home.path().join("tunnel-client-seen.json");
    env.fake_binary("tunnel-client", &fake_tunnel_client(&seen, true));
    env.write("visible.txt", "tunnel-read-fixture\n");
    let other = tempfile::tempdir().unwrap();
    std::fs::write(other.path().join("other.txt"), "tunnel-other-marker\n").unwrap();
    env.ok(&["add", ".", "--name", "api"]);
    env.ok(&["add", other.path().to_str().unwrap(), "--name", "other"]);
    let listed = env.json(&["--json", "ls"]);
    let api_id = listed["service"]["members"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["name"] == "api"))
        .and_then(|item| item["id"].as_str())
        .unwrap_or_else(|| panic!("找不到项目 api：{listed}"))
        .to_string();
    env.ok(&["daemon", "stop"]);

    // 守护进程停着时用正式存储建 grant 并绑定：这一步还没有命令行，绑定只有 core API。
    let tunnel = openai_tunnel_name(TUNNEL_ID).unwrap();
    let (grant_id, first_secret, binding_id) = {
        let store = AccessStore::open_in(env.home.path()).unwrap();
        let issued = store
            .create_grant(
                GrantCreation {
                    name: "chatgpt-tunnel".into(),
                    scope: ProjectScope::Selected(vec![api_id]),
                    permissions: GrantPermissions::default(),
                    kind: CredentialKind::Token,
                    ttl: None,
                    allowed_origins: vec![],
                },
                now(),
            )
            .unwrap();
        let binding = store.bind_tunnel(&tunnel, "chatgpt-tunnel", now()).unwrap();
        assert_eq!(binding.credential_revision, 1);
        (
            issued.record.grant.id.clone(),
            issued.secret.into_string(),
            binding.id,
        )
    };

    let port = free_port();
    let state = env.json(&[
        "--json",
        "start",
        "--port",
        &port.to_string(),
        "--tunnel",
        &format!("openai:{TUNNEL_ID}"),
        "--token",
        "fixture-runtime-key",
    ]);
    assert_eq!(state["service"]["state"], "running", "{state}");
    assert_eq!(state["service"]["tunnelError"], "", "{state}");
    // tunnel-client 的真实 initialize 探测带着隧道头通过了。
    let observed: Value = serde_json::from_str(&std::fs::read_to_string(&seen).unwrap()).unwrap();
    assert_eq!(observed["probe_kind"], "mcp_initialize");
    assert_eq!(observed["probe_status"], 200, "{observed}");
    let tunnel_key = env.service_secret("openai_tunnel_key");
    let key = tunnel_key.as_str();

    let initialized = initialize_with(port, key);
    assert_eq!(initialized.status, 200, "{}", initialized.body);
    assert_eq!(
        initialized.json()["result"]["protocolVersion"],
        "2025-06-18"
    );
    let listed = tunnel_rpc(port, key, "tools/list", json!({}));
    assert_eq!(listed.status, 200, "{}", listed.body);
    let tools = listed.json()["result"]["tools"].as_array().unwrap().clone();
    assert!(tools.iter().any(|tool| tool["name"] == "read_file"));
    // 只读 grant：隧道拿不到写工具。
    assert!(!tools.iter().any(|tool| tool["name"] == "apply_patch"));
    let read = |workspace: &str, path: &str| {
        tunnel_rpc(
            port,
            key,
            "tools/call",
            json!({"name":"read_file","arguments":{"workspace":workspace,"path":path}}),
        )
    };
    let visible = read("api", "visible.txt");
    assert_eq!(visible.status, 200, "{}", visible.body);
    assert!(
        visible.body.contains("tunnel-read-fixture"),
        "{}",
        visible.body
    );
    // grant 只选了 api：别的项目读不到。
    let outside = read("other", "other.txt");
    assert!(
        !outside.body.contains("tunnel-other-marker"),
        "{}",
        outside.body
    );
    let patch = "*** Begin Patch\n*** Add File: tunnel-write.txt\n+written\n*** End Patch\n";
    let write = || {
        tunnel_rpc(
            port,
            key,
            "tools/call",
            json!({"name":"apply_patch","arguments":{"workspace":"api","patch":patch}}),
        )
    };
    let denied = write();
    assert!(
        denied.json()["result"]["isError"] == true || denied.json()["error"].is_object(),
        "{}",
        denied.body
    );
    assert!(!env.project.path().join("tunnel-write.txt").exists());

    // 错的密钥、拿 grant 的 token 当隧道密钥：401，不带 OAuth 发现信息。
    for wrong in ["wrong", first_secret.as_str()] {
        let refused = initialize_with(port, wrong);
        assert_eq!(refused.status, 401, "{}", refused.body);
        assert_eq!(refused.json()["error"], "AUTHENTICATION_FAILED");
        assert_eq!(refused.header("cache-control"), Some("no-store"));
        assert_eq!(refused.header("www-authenticate"), None);
    }
    // 隧道头和 Bearer 不能同时带。
    let both = request(
        port,
        "POST",
        "/mcp",
        &[
            ("Content-Type", "application/json"),
            ("X-Gld-Tunnel-Key", key),
            ("Authorization", &format!("Bearer {first_secret}")),
        ],
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}).to_string(),
    );
    assert_eq!(both.status, 400, "{}", both.body);
    assert_eq!(both.json()["error"], "CONFLICTING_CREDENTIAL_SOURCES");
    assert_secrets_stay_put(
        &env,
        "绑定后调用",
        &[&first_secret],
        &[key, "fixture-runtime-key"],
    );

    // 轮换 grant：旧绑定不跟到新修订。隧道起不来并说明原因；直接用新 token 不受影响。
    env.ok(&["daemon", "stop"]);
    let second_secret = {
        let store = AccessStore::open_in(env.home.path()).unwrap();
        let rotated = store.rotate_grant("chatgpt-tunnel", None, now()).unwrap();
        assert_eq!(rotated.record.credential.revision, 2);
        let snapshot = store.snapshot().unwrap();
        assert_eq!(snapshot.by_tunnel(&tunnel).unwrap().credential_revision, 1);
        rotated.secret.into_string()
    };
    assert_start_reports_unbound(&env, &env.gld(&["start"]), port);
    assert_not_bound(&initialize_with(port, key), key);
    assert_not_bound(&write(), key);
    let list = json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}).to_string();
    assert_eq!(
        post_json(port, "/mcp", &list, Some(&second_secret)).status,
        200
    );
    assert_eq!(
        post_json(port, "/mcp", &list, Some(&first_secret)).status,
        401
    );

    // 重新绑定到当前修订并给 grant 加写权限：隧道恢复，绑定 ID 不变，写权限跟随 grant。
    env.ok(&["daemon", "stop"]);
    {
        let store = AccessStore::open_in(env.home.path()).unwrap();
        let rebound = store.bind_tunnel(&tunnel, &grant_id, now()).unwrap();
        assert_eq!(rebound.id, binding_id);
        assert_eq!(rebound.credential_revision, 2);
        let mut record = store
            .snapshot()
            .unwrap()
            .by_grant_id(&grant_id)
            .unwrap()
            .clone();
        record.grant.permissions.write = true;
        store.update(record).unwrap();
    }
    env.ok(&["start"]);
    assert_eq!(initialize_with(port, key).status, 200);
    let written = write();
    assert_eq!(written.status, 200, "{}", written.body);
    assert_ne!(
        written.json()["result"]["isError"],
        true,
        "{}",
        written.body
    );
    assert_eq!(
        std::fs::read_to_string(env.project.path().join("tunnel-write.txt")).unwrap(),
        "written\n"
    );
    let outside = read("other", "other.txt");
    assert!(
        !outside.body.contains("tunnel-other-marker"),
        "{}",
        outside.body
    );

    // 撤销 grant：绑定记录留着，文件照常加载，隧道请求是 403。
    env.ok(&["daemon", "stop"]);
    {
        let store = AccessStore::open_in(env.home.path()).unwrap();
        store.revoke(&grant_id).unwrap();
        assert_eq!(
            store.snapshot().unwrap().by_tunnel(&tunnel).unwrap().id,
            binding_id
        );
    }
    assert_start_reports_unbound(&env, &env.gld(&["start"]), port);
    assert_not_bound(&initialize_with(port, key), key);
    assert_eq!(
        post_json(port, "/mcp", &list, Some(&second_secret)).status,
        401
    );

    // 拒绝原因只在日志里；秘密不在日志、回包和授权文件里。
    let log = std::fs::read_to_string(env.home.path().join("logs/hub/mcp-requests.log")).unwrap();
    for reason in ["key", "stale_revision", "revoked"] {
        assert!(
            log.contains(&format!("entry=tunnel reason={reason} ")),
            "{reason}: {log}"
        );
    }
    assert_secrets_stay_put(
        &env,
        "轮换、重绑、撤销",
        &[&first_secret, &second_secret],
        &[key, "fixture-runtime-key"],
    );
}

/// 有 grant 但隧道没绑定：tunnel-client 的探测拿到 403，启动报错要告诉用户差的是绑定。
#[test]
fn an_unbound_tunnel_is_refused_and_the_start_error_says_what_is_missing() {
    let mut env = Env::new();
    let seen = env.home.path().join("tunnel-client-seen.json");
    env.fake_binary("tunnel-client", &fake_tunnel_client(&seen, true));
    env.ok(&["add", ".", "--name", "api"]);
    env.ok(&["daemon", "stop"]);
    let secret = {
        let store = AccessStore::open_in(env.home.path()).unwrap();
        let grant = |name: &str, kind| GrantCreation {
            name: name.into(),
            scope: ProjectScope::AllProjects,
            permissions: GrantPermissions::default(),
            kind,
            ttl: None,
            allowed_origins: vec![],
        };
        let issued = store
            .create_grant(grant("script", CredentialKind::Token), now())
            .unwrap();
        // OAuth 用途的 grant 不能绑到隧道上。
        store
            .create_grant(grant("chat", CredentialKind::OAuth), now())
            .unwrap();
        assert!(store
            .bind_tunnel(&openai_tunnel_name(TUNNEL_ID).unwrap(), "chat", now())
            .is_err());
        issued.secret.into_string()
    };
    let port = free_port();
    let started = std::time::Instant::now();
    let output = env.gld(&[
        "start",
        "--port",
        &port.to_string(),
        "--tunnel",
        &format!("openai:{TUNNEL_ID}"),
        "--token",
        "fixture-runtime-key",
    ]);
    assert!(!output.status.success());
    assert_start_reports_unbound(&env, &output, port);
    // 没绑定不会自己好：不等满 30 秒的就绪超时。
    assert!(started.elapsed() < std::time::Duration::from_secs(25));
    let stderr = String::from_utf8_lossy(&output.stderr);
    let tunnel_key = env.service_secret("openai_tunnel_key");
    assert!(!stderr.contains(&tunnel_key) && !stderr.contains("fixture-runtime-key"));
    let observed: Value = serde_json::from_str(&std::fs::read_to_string(&seen).unwrap()).unwrap();
    assert_eq!(observed["probe_status"], 403, "{observed}");
    let probe_body: Value = serde_json::from_str(observed["probe_body"].as_str().unwrap()).unwrap();
    assert_eq!(probe_body["error"], "TUNNEL_NOT_BOUND");

    assert_not_bound(&initialize_with(port, &tunnel_key), &tunnel_key);
    let wrong = initialize_with(port, "wrong");
    assert_eq!(wrong.status, 401, "{}", wrong.body);
    assert_eq!(wrong.header("www-authenticate"), None);
    // 有 grant 不等于隧道能用：同一个 grant 直接带 token 可以，经隧道不行。
    let list = json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}).to_string();
    assert_eq!(post_json(port, "/mcp", &list, Some(&secret)).status, 200);
    let log = std::fs::read_to_string(env.home.path().join("logs/hub/mcp-requests.log")).unwrap();
    assert!(log.contains("entry=tunnel reason=unbound "), "{log}");
    assert_secrets_stay_put(
        &env,
        "未绑定",
        &[&secret],
        &[&tunnel_key, "fixture-runtime-key"],
    );
}

/// 改隧道密钥、隧道 ID 或隧道类型都会按新配置重建 Hub 监听器（守护进程不重启）：
/// 旧密钥立刻失效，换了隧道 ID 要重新绑定，切走 OpenAI 隧道后隧道头不再被接受。
#[test]
fn changing_the_tunnel_key_id_or_type_leaves_no_old_value_valid() {
    let mut env = Env::new();
    let seen = env.home.path().join("tunnel-client-seen.json");
    env.fake_binary("tunnel-client", &fake_tunnel_client(&seen, true));
    env.ok(&["add", ".", "--name", "api"]);
    env.ok(&["daemon", "stop"]);
    {
        let store = AccessStore::open_in(env.home.path()).unwrap();
        store
            .create_grant(
                GrantCreation {
                    name: "chatgpt-tunnel".into(),
                    scope: ProjectScope::AllProjects,
                    permissions: GrantPermissions::default(),
                    kind: CredentialKind::Token,
                    ttl: None,
                    allowed_origins: vec![],
                },
                now(),
            )
            .unwrap();
        store
            .bind_tunnel(
                &openai_tunnel_name(TUNNEL_ID).unwrap(),
                "chatgpt-tunnel",
                now(),
            )
            .unwrap();
    }
    let port = free_port();
    env.ok(&[
        "start",
        "--port",
        &port.to_string(),
        "--tunnel",
        &format!("openai:{TUNNEL_ID}"),
        "--token",
        "fixture-runtime-key",
    ]);
    let daemon_pid = |env: &Env| env.json(&["--json", "daemon", "status"])["info"]["pid"].clone();
    let pid = daemon_pid(&env);
    let old_key = env.service_secret("openai_tunnel_key");
    assert_eq!(initialize_with(port, &old_key).status, 200);

    // 换隧道密钥：旧密钥是 401，新密钥沿用原绑定。
    env.ok(&["secret", "regenerate", "openai_tunnel_key"]);
    let new_key = env.service_secret("openai_tunnel_key");
    assert_ne!(new_key, old_key);
    let stale = initialize_with(port, &old_key);
    assert_eq!(stale.status, 401, "{}", stale.body);
    assert_eq!(stale.header("www-authenticate"), None);
    assert_eq!(initialize_with(port, &new_key).status, 200);

    // 换隧道 ID：绑定不跟过去，密钥对也是 403，直到给新隧道重新绑定。
    let moved = env.gld(&[
        "share",
        "--tunnel",
        "openai:tunnel_fedcba9876543210fedcba9876543210",
    ]);
    assert_start_reports_unbound(&env, &moved, port);
    assert_not_bound(&initialize_with(port, &new_key), &new_key);

    // 切走 OpenAI 隧道：隧道头不再被接受。
    let _ = env.gld(&["share", "--tunnel", "off"]);
    assert_eq!(env.json(&["--json", "ls"])["service"]["state"], "running");
    for key in [&old_key, &new_key] {
        let refused = initialize_with(port, key);
        assert_eq!(refused.status, 401, "{}", refused.body);
        assert_eq!(refused.json()["error"], "AUTHENTICATION_FAILED");
        assert_eq!(refused.header("www-authenticate"), None);
    }
    assert_eq!(daemon_pid(&env), pid, "守护进程不该重启");
    assert!(pid.is_number(), "{pid}");
}

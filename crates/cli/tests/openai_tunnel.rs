//! OpenAI 隧道：gld 托管 `tunnel-client run`，经它进来的请求靠服务凭据 `openai_tunnel_key` 放行。
//!
//! 假 tunnel-client 学真的那几步（对着 openai/tunnel-client v0.0.18 源码）：把 `--mcp.extra-headers`
//! 里 `env:` 引用的值取出来，带着它给服务发一次 `initialize`，再在 `--health.listen-addr` 上答
//! `/readyz`——探测成功是 200 `ready`，拿到 401 时也是 200，正文是 `ready (mcp initialize requires auth: …)`。

#![cfg(unix)]

mod common;

use common::env::{free_port, Env};
use common::http::request;

const TUNNEL_ID: &str = "tunnel_0123456789abcdef0123456789abcdef";

/// `send_key` 为 false 时假装带了一把错的请求头。
fn fake_tunnel_client(seen: &std::path::Path, send_key: bool) -> String {
    format!(
        r#"#!/usr/bin/env python3
import json, os, sys, http.server, urllib.request, urllib.error
args = sys.argv[1:]
def flag(name):
    return [args[i + 1] for i, a in enumerate(args) if a == name and i + 1 < len(args)]
with open({seen:?}, "w") as f:
    json.dump({{"pid": os.getpid(), "args": args,
               "runtime_key": os.environ.get("GLD_OPENAI_RUNTIME_KEY"),
               "tunnel_key": os.environ.get("GLD_OPENAI_TUNNEL_KEY"),
               "no_proxy": os.environ.get("NO_PROXY", "")}}, f)
name, value = flag("--mcp.discovery-extra-headers")[0].split(": ", 1)
if value.startswith("env:"):
    value = os.environ.get(value[4:], "")
if not {send_key}:
    value = "wrong"
body = json.dumps({{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {{
    "protocolVersion": "2025-06-18", "capabilities": {{}},
    "clientInfo": {{"name": "fake-tunnel", "version": "0"}}}}}}).encode()
probe = urllib.request.Request(flag("--mcp.server-url")[0], data=body, headers={{
    name: value, "Content-Type": "application/json",
    "Accept": "application/json, text/event-stream"}})
opener = urllib.request.build_opener(urllib.request.ProxyHandler({{}}))
try:
    opener.open(probe, timeout=5).read()
    status = "ready"
except urllib.error.HTTPError as error:
    status = "ready (mcp initialize requires auth: %d)" % error.code if error.code == 401 else "mcp probe failed: %d" % error.code
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
        send_key = if send_key { "True" } else { "False" },
    )
}

fn initialize_with(port: u16, key: &str) -> u16 {
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
    .status
}

fn process_alive(pid: u64) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[test]
fn openai_tunnel_runs_tunnel_client_and_lets_its_requests_in() {
    let mut env = Env::new();
    let seen = env.home.path().join("tunnel-client-seen.json");
    env.fake_binary("tunnel-client", &fake_tunnel_client(&seen, true));
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

    // 服务还是 oauth：带对请求头的进得来，带错的照样 401。
    assert_eq!(state["service"]["config"]["authType"], "oauth");
    assert_eq!(initialize_with(port, &tunnel_key), 200);
    assert_eq!(initialize_with(port, "wrong"), 401);

    let listing = env.ok(&["ls"]);
    assert!(
        listing.contains(&format!("OpenAI 隧道 {TUNNEL_ID}")),
        "{listing}"
    );
    assert!(listing.contains("Connection 选 Tunnel"), "{listing}");
    assert!(!listing.contains("还没有公网地址"), "{listing}");

    // 隧道 ID 和 Runtime Key 都存下了：不带参数再 share 一次照样起得来。
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

/// 探测拿 401 时真的 tunnel-client 也报 200，gld 得从正文认出来，不能说起好了。
#[test]
fn a_tunnel_whose_probe_is_refused_is_reported_and_the_service_keeps_running() {
    let mut env = Env::new();
    let seen = env.home.path().join("tunnel-client-seen.json");
    env.fake_binary("tunnel-client", &fake_tunnel_client(&seen, false));
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
    assert!(stderr.contains("没认出"), "{stderr}");
    assert!(!stderr.contains("fixture-from-env"), "凭据泄露：{stderr}");
    // Runtime Key 取自环境变量 CONTROL_PLANE_API_KEY 并存下了。
    assert_eq!(env.service_secret("openai_runtime_key"), "fixture-from-env");
    let state = env.json(&["--json", "ls"]);
    assert_eq!(state["service"]["state"], "running", "{state}");
    let pid = serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&seen).unwrap())
        .unwrap()["pid"]
        .as_u64()
        .unwrap();
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

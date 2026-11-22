//! D18-A: a tunnel's transport credentials do not authorize MCP requests.
//! One fixture probes GET discovery solely to test process arguments and cleanup.
//! The authorization fixture performs a real MCP initialize and must fail with 503.

#![cfg(unix)]

mod common;

use common::env::{free_port, Env};
use common::http::{get, request, Reply};

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
    for key in [tunnel_key.as_str(), "wrong"] {
        let refused = initialize_with(port, key);
        assert_eq!(refused.status, 503, "{}", refused.body);
        assert_eq!(refused.json()["error"], "AUTH_NOT_READY");
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

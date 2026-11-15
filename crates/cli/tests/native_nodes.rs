//! D18-A 的真实节点边界：配对、传输身份、机器凭据和本地生命周期可独立验证。
//! 旧控制/转发授权明确拒绝；D18-C 必须恢复的正向场景见 forwarding_contract.md。
//! 所有配置与进程均属于临时 GLD_HOME；TLS 证书仅供隔离测试。

mod common;

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[path = "native_nodes/autostart.rs"]
mod autostart_tests;
#[cfg(unix)]
#[path = "native_nodes/commands.rs"]
mod command_tests;
#[path = "native_nodes/file_writes.rs"]
mod file_write_tests;
#[path = "native_nodes/onboarding.rs"]
mod onboarding_tests;
#[path = "native_nodes/ownership.rs"]
mod ownership_tests;
#[path = "native_nodes/pairing.rs"]
mod pairing_tests;
#[path = "native_nodes/proxy.rs"]
mod proxy_tests;
#[path = "native_nodes/sharing.rs"]
mod sharing_tests;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use common::env::{free_port, Env};
use common::http::post_json;
use common::service::Service;
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, watch};
use tokio_rustls::rustls::{self, pki_types::CertificateDer, pki_types::PrivatePkcs8KeyDer};
use tokio_rustls::TlsAcceptor;

const WAIT: Duration = Duration::from_secs(15);
const TOKEN_A: &str = "native-node-a-isolated-test-credential-1234567890";
const TOKEN_B: &str = "native-node-b-isolated-test-credential-1234567890";
// 与 core/hub/node_transport_tests.rs 相同的测试 CA/localhost 证书，2020–2040 有效。
const CA: &str = "MIIBCjCBvaADAgECAgEBMAUGAytlcDAbMRkwFwYDVQQDDBBnbGQgdGVzdC1vbmx5IENBMB4XDTIwMDEwMTAwMDAwMFoXDTQwMDEwMTAwMDAwMFowGzEZMBcGA1UEAwwQZ2xkIHRlc3Qtb25seSBDQTAqMAUGAytlcAMhAHXUPrR9j/ZkaObE4CXVGDzGo8qs/nWQZsDGtDBq4SeIoyYwJDASBgNVHRMBAf8ECDAGAQH/AgEAMA4GA1UdDwEB/wQEAwIBhjAFBgMrZXADQQBwQqWkHLXIbEpn5g2sYRsTfrEjk7RdiQDC6+roEuIexiO+B0mVXoeq/lgJD5EuqhRw7S0jxtoY3nLy7dL95rYN";
const CERT: &str = "MIIBHjCB0aADAgECAgECMAUGAytlcDAbMRkwFwYDVQQDDBBnbGQgdGVzdC1vbmx5IENBMB4XDTIwMDEwMTAwMDAwMFoXDTQwMDEwMTAwMDAwMFowFDESMBAGA1UEAwwJbG9jYWxob3N0MCowBQYDK2VwAyEAMn7UEwSHwwibLtDgnmhfx3FV0XS6ZIaeWN9u7y7Bj7yjQTA/MAwGA1UdEwEB/wQCMAAwGgYDVR0RBBMwEYIJbG9jYWxob3N0hwR/AAABMBMGA1UdJQQMMAoGCCsGAQUFBwMBMAUGAytlcANBAG2LNzdf1sLA8n89CuHCsL8gz68WoKD7G8s6DmuBh6ESPzmfCQZS9NNsDo0QjZpvTI8jBv5FBQ7anS923FeL6Q0=";
const KEY: &str = "MC4CAQAwBQYDK2VwBCIEIE5CFjy/tFcNFC+1UPZj52Um/IO/1x0t9eJFA5yoRERI";

fn decode(text: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .unwrap()
}

/// 只终止测试 TLS，HTTP 路径和 Upgrade 字节原样转给真实全局入口。
struct TlsRelay {
    port: u16,
    accepted: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    available: watch::Sender<bool>,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

struct ActiveRelay(Arc<AtomicUsize>);

impl Drop for ActiveRelay {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl TlsRelay {
    fn new(upstream: u16) -> Self {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let server = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(decode(CERT))],
            PrivatePkcs8KeyDer::from(decode(KEY)).into(),
        )
        .unwrap();
        let tls = TlsAcceptor::from(Arc::new(server));
        let accepted = Arc::new(AtomicUsize::new(0));
        let count = accepted.clone();
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let active_count = active.clone();
        let peak_count = peak.clone();
        let (available, availability) = watch::channel(true);
        let (stop, mut stopping) = oneshot::channel();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = TcpListener::from_std(listener).unwrap();
                loop {
                    tokio::select! {
                        _ = &mut stopping => break,
                        accepted = listener.accept() => {
                            let Ok((tcp, _)) = accepted else { break };
                            count.fetch_add(1, Ordering::SeqCst);
                            let mut availability = availability.clone();
                            if !*availability.borrow_and_update() { continue; }
                            let tls = tls.clone();
                            let active = active_count.clone();
                            let peak = peak_count.clone();
                            tokio::spawn(async move {
                                tokio::select! {
                                    biased;
                                    _ = availability.changed() => {}
                                    _ = async move {
                                        let Ok(mut client) = tls.accept(tcp).await else { return };
                                        let Ok(mut server) = TcpStream::connect(("127.0.0.1", upstream)).await else { return };
                                        let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                                        peak.fetch_max(current, Ordering::SeqCst);
                                        let _connection = ActiveRelay(active);
                                        let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
                                    } => {}
                                }
                            });
                        }
                    }
                }
            });
        });
        Self {
            port,
            accepted,
            active,
            peak,
            available,
            stop: Some(stop),
            thread: Some(thread),
        }
    }

    fn connections(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }

    fn set_available(&self, available: bool) {
        self.available.send_replace(available);
    }
}

impl Drop for TlsRelay {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn command(env: &Env) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_gld"));
    command
        .env("GLD_HOME", env.home.path())
        .env("NO_COLOR", "1")
        .env_remove("GLD_WORKSPACE")
        .env_remove("TUNNEL_TOKEN")
        .env_remove("CONTROL_PLANE_API_KEY")
        .env_remove("HTTPS_PROXY")
        .env_remove("https_proxy")
        .env_remove("ALL_PROXY")
        .env_remove("all_proxy")
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .current_dir(env.project.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

struct NodeProcess(Option<Child>);

impl NodeProcess {
    fn start(env: &Env, config: &Path) -> Self {
        let child = command(env)
            .args(["node", "run", "--config"])
            .arg(config)
            .spawn()
            .expect("start node process");
        Self(Some(child))
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn rejected(mut self) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            let child = self.0.as_mut().unwrap();
            if let Some(status) = child.try_wait().unwrap() {
                let mut error = String::new();
                child
                    .stderr
                    .as_mut()
                    .unwrap()
                    .read_to_string(&mut error)
                    .unwrap();
                assert!(
                    !status.success(),
                    "invalid node configuration unexpectedly succeeded"
                );
                return error;
            }
            assert!(
                Instant::now() < deadline,
                "invalid node connection did not exit"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn wait_stopped(&mut self) {
        let deadline = Instant::now() + WAIT;
        while self.0.as_mut().unwrap().try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "node process did not stop");
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

fn wait_until(label: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !ready() {
        assert!(Instant::now() < deadline, "{label}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

impl Drop for NodeProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

fn register_output(env: &Env, name: &str, token: &str) -> Output {
    credential_output(
        env,
        &["--json", "node", "register", name, "--credential-stdin"],
        token,
    )
}

fn credential_output(env: &Env, args: &[&str], token: &str) -> Output {
    let mut child = command(env)
        .args(args)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(token.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn register(env: &Env, name: &str, token: &str) -> Value {
    let output = register_output(env, name, token);
    assert!(
        output.status.success(),
        "node register failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["name"], name);
    assert_eq!(value["machine_id"].as_str().unwrap().len(), 32);
    assert!(!String::from_utf8_lossy(&output.stdout).contains(token));
    value
}

fn projects(env: &Env, machine: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for name in ["api", "web", "unshared"] {
        env.write(&format!("{name}/README.md"), &format!("{machine}/{name}\n"));
        let path = env.project.path().join(name);
        let added = env.json(&["--json", "add", path.to_str().unwrap(), "--name", name]);
        if name != "unshared" {
            ids.push(added[0]["id"].as_str().unwrap().into());
        }
    }
    ids
}

fn config(env: &Env, relay: &TlsRelay, registration: &Value, token: &str, ids: &[String]) -> Value {
    let ca = env.home.path().join("node-test-ca.der");
    std::fs::write(&ca, decode(CA)).unwrap();
    json!({
        "hub_url":format!("https://localhost:{}/hub", relay.port),
        "hub_id":registration["hub_id"],
        "hub_public_key":registration["hub_public_key"],
        "machine_id":registration["machine_id"],
        "credential":token,
        "workspaces":ids,
        "ca_certificate":ca,
    })
}

fn write_config(env: &Env, name: &str, value: &Value) -> PathBuf {
    let path = env.home.path().join(name);
    let config: gld_core::nodes::config::NodeConfig =
        serde_json::from_value(value.clone()).unwrap();
    let file = gld_core::nodes::local_config::LocalConfig::lock(&path).unwrap();
    if file.exists().unwrap() {
        file.replace(&config).unwrap();
    } else {
        file.save_pending(&config).unwrap();
        file.finish(&config).unwrap();
    }
    path
}

fn stored_machines(env: &Env) -> Vec<Value> {
    let data: Value =
        serde_json::from_slice(&std::fs::read(env.home.path().join("data/profiles.json")).unwrap())
            .unwrap();
    data["node_machines"].as_array().unwrap().clone()
}

fn assert_auth_not_ready(reply: &common::http::Reply) {
    assert_eq!(reply.status, 503, "{}", reply.body);
    assert_eq!(reply.json()["error"], "AUTH_NOT_READY", "{}", reply.body);
    assert_eq!(reply.header("cache-control"), Some("no-store"));
}

fn assert_node_auth_pending(error: &str) {
    assert!(
        error.contains("node inventory requires scoped authorization; legacy control credentials are not accepted"),
        "a verified transport must reach the explicit authorization refusal: {error}"
    );
    assert!(!error.contains(TOKEN_A));
    assert!(!error.contains(TOKEN_B));
}

fn wait_running(config: &Path) {
    wait_until("node did not acquire the local configuration lock", || {
        gld_core::nodes::local_config::LocalConfig::in_use(config).unwrap()
    });
}

fn assert_no_native_projects(hub: &Env) {
    assert!(
        stored_machines(hub)
            .iter()
            .all(|machine| machine["projects"].as_array().unwrap().is_empty()),
        "transport authentication must not publish an unauthorized project inventory"
    );
}

fn assert_no_daemon(env: &Env) {
    assert_eq!(env.gld(&["daemon", "status"]).status.code(), Some(3));
    assert!(!env.home.path().join("daemon.json").exists());
    #[cfg(unix)]
    assert!(!env.home.path().join("daemon.sock").exists());
}

#[test]
fn tls_gateway_and_machine_credentials_do_not_authorize_legacy_node_control() {
    let hub = Env::new();
    hub.write("README.md", "hub/api\n");
    let service = hub.serve("api", "bearer");
    let gateway_port = free_port();
    hub.ok(&[
        "gateway",
        "set",
        "--enabled",
        "true",
        "--tunnel",
        "off",
        "--public-url",
        "https://gateway.example.com",
        "--port",
        &gateway_port.to_string(),
    ]);
    hub.ok(&["gateway", "start"]);
    let relay = TlsRelay::new(gateway_port);
    let machine_a = Env::new();
    let machine_b = Env::new();
    let ids_a = projects(&machine_a, "machine-a");
    let ids_b = projects(&machine_b, "machine-b");
    assert_no_daemon(&machine_a);
    assert_no_daemon(&machine_b);
    let registration_a = register(&hub, "machine-a", TOKEN_A);
    let registration_b = register(&hub, "machine-b", TOKEN_B);
    assert_eq!(registration_a["hub_id"], registration_b["hub_id"]);
    assert_ne!(registration_a["machine_id"], registration_b["machine_id"]);

    let value_a = config(&machine_a, &relay, &registration_a, TOKEN_A, &ids_a);
    let value_b = config(&machine_b, &relay, &registration_b, TOKEN_B, &ids_b);
    let config_a = write_config(&machine_a, "node-a.json", &value_a);
    let config_b = write_config(&machine_b, "node-b.json", &value_b);

    let disabled = NodeProcess::start(&machine_a, &config_a).rejected();
    assert!(
        disabled.contains("404"),
        "gateway must require explicit Hub opt-in: {disabled}"
    );
    hub.ok(&["hub", "set", "--global-gateway", "true"]);

    // TLS 信任必须显式提供；MCP Bearer 与节点凭据不能互换。
    let mut untrusted = value_a.clone();
    untrusted.as_object_mut().unwrap().remove("ca_certificate");
    let untrusted = write_config(&machine_a, "untrusted.json", &untrusted);
    NodeProcess::start(&machine_a, &untrusted).rejected();
    for token in [
        "wrong-node-credential-with-enough-bytes",
        service.token.as_str(),
    ] {
        let mut wrong = value_a.clone();
        wrong["credential"] = json!(token);
        let wrong = write_config(&machine_a, "wrong-credential.json", &wrong);
        let error = NodeProcess::start(&machine_a, &wrong).rejected();
        assert!(
            error.contains("401"),
            "expected unauthorized handshake: {error}"
        );
        assert!(!error.contains(token));
    }
    // 有域名证书、照抄 hub_id 的冒充者给不出钉住公钥的握手证明（RFC-0013 §7.3）。
    // 反过来模拟：节点钉的不是这个 Hub 的公钥，就得在任何调用之前拒绝并退出。
    let mut other_key = value_a.clone();
    other_key["hub_public_key"] = json!("e".repeat(64));
    let other_key = write_config(&machine_a, "other-hub-key.json", &other_key);
    let error = NodeProcess::start(&machine_a, &other_key).rejected();
    assert!(error.contains("pinned Hub key"), "{error}");
    assert_no_native_projects(&hub);
    let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
        "params":{"name":"list_workspaces","arguments":{}}})
    .to_string();
    for bearer in [None, Some(TOKEN_A)] {
        assert_auth_not_ready(&post_json(service.port, "/mcp", &request, bearer));
    }
    let before = relay.connections();
    for (machine, path) in [(&machine_a, &config_a), (&machine_b, &config_b)] {
        let saved = std::fs::read(path).unwrap();
        let error = NodeProcess::start(machine, path).rejected();
        assert_node_auth_pending(&error);
        assert_eq!(std::fs::read(path).unwrap(), saved);
        assert_no_daemon(machine);
    }
    assert_eq!(
        relay.connections(),
        before + 2,
        "unsupported authorization must terminate without retrying"
    );
    assert_no_native_projects(&hub);
    for (machine, label) in [(&machine_a, "machine-a"), (&machine_b, "machine-b")] {
        for project in ["api", "web", "unshared"] {
            assert_eq!(
                std::fs::read_to_string(
                    machine.project.path().join(format!("{project}/README.md"))
                )
                .unwrap(),
                format!("{label}/{project}\n")
            );
        }
    }
}

#[test]
fn an_initial_network_outage_retries_then_stops_at_the_authorization_boundary() {
    let hub = Env::new();
    let service = hub.serve("local", "bearer");
    let relay = TlsRelay::new(service.port);
    let machine = Env::new();
    let ids = projects(&machine, "recovering");
    let registration = register(&hub, "recovering", TOKEN_A);
    let mut value = config(&machine, &relay, &registration, TOKEN_A, &ids);
    value["hub_url"] = json!(format!("https://localhost:{}", relay.port));
    let path = write_config(&machine, "recovering.json", &value);
    relay.set_available(false);
    let mut node = NodeProcess::start(&machine, &path);
    wait_until("initial connection was never attempted", || {
        relay.connections() > 0
    });
    assert!(node.0.as_mut().unwrap().try_wait().unwrap().is_none());
    relay.set_available(true);
    assert_node_auth_pending(&node.rejected());
    wait_until("refused connection was not released", || {
        relay.active.load(Ordering::SeqCst) == 0
    });
    assert_eq!(relay.peak.load(Ordering::SeqCst), 1);
    assert_no_native_projects(&hub);
    assert_no_daemon(&machine);
}

#[test]
fn node_registration_does_not_truncate_an_overlong_credential() {
    let env = Env::new();
    let token = "x".repeat(300);
    let output = register_output(&env, "overlong", &token);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("过长"), "{error}");
    assert!(!error.contains(&token));
}

#[test]
fn removing_a_revoked_machine_preserves_other_registrations_and_frees_its_name() {
    let hub = Env::new();
    let service = hub.serve("local", "bearer");
    let relay = TlsRelay::new(service.port);
    let machine = Env::new();
    let ids = projects(&machine, "machine-a");
    let a = register(&hub, "machine-a", TOKEN_A);
    let b = register(&hub, "machine-b", TOKEN_B);
    let machine_id = a["machine_id"].as_str().unwrap();
    let mut value = config(&machine, &relay, &a, TOKEN_A, &ids);
    value["hub_url"] = json!(format!("https://localhost:{}", relay.port));
    let path = write_config(&machine, "removed.json", &value);
    let before = stored_machines(&hub);
    let refused = hub.gld(&["node", "remove", "machine-a"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains(machine_id));
    assert_eq!(stored_machines(&hub), before);

    hub.json(&["--json", "node", "revoke", machine_id]);
    let removed = hub.json(&["--json", "node", "remove", "MACHINE-A"]);
    assert_eq!(removed["removed"], true);
    assert_eq!(removed["machine_id"], machine_id);
    assert!(!removed.to_string().contains(TOKEN_A));
    let remaining = stored_machines(&hub);
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0]["id"], b["machine_id"]);
    assert!(NodeProcess::start(&machine, &path)
        .rejected()
        .contains("401"));
    assert!(!hub.gld(&["node", "remove", machine_id]).status.success());
    let again = register(&hub, "machine-a", "synthetic-node-credential-re-registered");
    assert_ne!(again["machine_id"], machine_id);
    assert_eq!(again["hub_id"], a["hub_id"]);
    assert_no_native_projects(&hub);
    assert_no_daemon(&machine);
}

#[test]
fn node_credential_rotation_preserves_identity_but_never_grants_project_authority() {
    let hub = Env::new();
    let service = hub.serve("local", "bearer");
    let relay = TlsRelay::new(service.port);
    let machine = Env::new();
    let ids = projects(&machine, "machine-a");
    let registration = register(&hub, "machine-a", TOKEN_A);
    register(&hub, "machine-b", TOKEN_B);
    let machine_id = registration["machine_id"].as_str().unwrap();
    let mut value = config(&machine, &relay, &registration, TOKEN_A, &ids);
    value["hub_url"] = json!(format!("https://localhost:{}", relay.port));
    let path = write_config(&machine, "rotated.json", &value);
    assert_node_auth_pending(&NodeProcess::start(&machine, &path).rejected());
    let args = [
        "--json",
        "node",
        "rotate-credential",
        machine_id,
        "--credential-stdin",
    ];
    let snapshot = stored_machines(&hub);
    assert!(!credential_output(&hub, &args, TOKEN_B).status.success());
    assert_eq!(stored_machines(&hub), snapshot);

    let revoked = hub.json(&["--json", "node", "revoke", machine_id]);
    assert_eq!(revoked["revoked"], true);
    assert_eq!(hub.json(&["--json", "node", "revoke", machine_id]), revoked);
    assert!(NodeProcess::start(&machine, &path)
        .rejected()
        .contains("401"));
    for token in [TOKEN_A, TOKEN_B, service.token.as_str()] {
        let output = credential_output(&hub, &args, token);
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stderr).contains(token));
        assert!(stored_machines(&hub)
            .iter()
            .any(|machine| machine["id"] == machine_id && machine["revoked"] == true));
    }
    for token in [
        "native-node-new-credential-isolated-test-00000001",
        "native-node-new-credential-isolated-test-00000002",
    ] {
        let output = credential_output(&hub, &args, token);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let rotated: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(rotated["machine_id"], machine_id);
        assert_eq!(rotated["hub_id"], registration["hub_id"]);
        assert_eq!(rotated["revoked"], false);
        assert!(!String::from_utf8_lossy(&output.stdout).contains(token));
        assert!(rotated.get("credential_sha256").is_none());
        assert!(NodeProcess::start(&machine, &path)
            .rejected()
            .contains("401"));
        value["credential"] = json!(token);
        write_config(&machine, "rotated.json", &value);
        let error = NodeProcess::start(&machine, &path).rejected();
        assert_node_auth_pending(&error);
        assert!(!error.contains(token));
        assert_no_native_projects(&hub);
    }
    assert_no_daemon(&machine);
}

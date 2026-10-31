//! 邀请、目标机确认与响应丢失后的同一登记恢复，均经过真实 CLI 和 Hub。

use super::*;
use std::sync::atomic::AtomicBool;

fn join_command(env: &Env, url: &str, config: &Path, ca: Option<&Path>, yes: bool) -> Command {
    let mut command = command(env);
    command
        .args(["--json", "join", "--hub", url, "--config"])
        .arg(config)
        .args(["--share", "api", "--invite-stdin"])
        .stdin(Stdio::piped());
    if yes {
        command.arg("--yes");
    }
    if let Some(ca) = ca {
        command.arg("--ca-certificate").arg(ca);
    }
    command
}

fn with_invitation(mut command: Command, invitation: &str) -> Output {
    let mut child = command.spawn().expect("start join");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(invitation.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn node_data(env: &Env) -> Value {
    serde_json::from_slice(&std::fs::read(env.home.path().join("data/profiles.json")).unwrap())
        .unwrap()
}

fn assert_no_secret(output: &Output, secret: &str) {
    assert!(!String::from_utf8_lossy(&output.stdout).contains(secret));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
}

/// 读到响应头即停止，不让被拒绝的 Upgrade 依赖服务端关闭普通 HTTP keep-alive。
fn rejected_upgrade(port: u16, machine_id: &str, invitation: &str) -> u16 {
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    let request = format!(
        "GET /nodes/v1/connect/{machine_id} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
         Connection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nAuthorization: Bearer {invitation}\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut header = Vec::new();
    let mut chunk = [0_u8; 1024];
    while !header.windows(4).any(|part| part == b"\r\n\r\n") {
        let length = stream.read(&mut chunk).unwrap();
        assert!(
            length > 0 && header.len() + length <= 8192,
            "incomplete Upgrade rejection"
        );
        header.extend_from_slice(&chunk[..length]);
    }
    String::from_utf8(header)
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn cli_join_pairs_only_selected_projects_through_tls_and_preserves_confirmation_boundaries() {
    let hub = Env::new();
    let service = hub.serve("local", "bearer");
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
    hub.ok(&["hub", "set", "--global-gateway", "true"]);
    let relay = TlsRelay::new(gateway_port);
    let machine = Env::new();
    let ids = projects(&machine, "paired-node");
    let ca = machine.home.path().join("pairing-ca.der");
    std::fs::write(&ca, decode(CA)).unwrap();
    let url = format!("https://127.0.0.1:{}/hub", relay.port);
    let config = machine.home.path().join("paired.json");
    let pending = machine.home.path().join("paired.json.pending");
    let invitation = hub.json(&["--json", "node", "invite", "paired-node"]);
    // 全局入口就是这个 Hub 的固定入口，邀请不用再问地址。
    assert_eq!(invitation["hub_url"], "https://gateway.example.com/hub");
    let token = invitation["invitation"].as_str().unwrap();
    assert_eq!(
        node_data(&hub)["node_invitations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let declined = with_invitation(
        join_command(&machine, &url, &config, Some(&ca), false),
        token,
    );
    assert!(!declined.status.success());
    assert!(String::from_utf8_lossy(&declined.stderr).contains("--yes"));
    assert_no_secret(&declined, token);
    assert!(!config.exists());
    assert!(!pending.exists());
    assert!(node_data(&hub)["node_machines"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        node_data(&hub)["node_invitations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // 两类公开入口都不能把邀请或 MCP 服务密钥当作另一种凭据。
    let rpc = json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
        "params":{"name":"list_workspaces","arguments":{}}})
    .to_string();
    assert_eq!(
        post_json(service.port, "/mcp", &rpc, Some(token)).status,
        401
    );
    assert_eq!(rejected_upgrade(service.port, &"e".repeat(32), token), 401);
    let body = json!({"request_id":"f".repeat(32),"credential":TOKEN_B}).to_string();
    assert_eq!(
        post_json(service.port, "/nodes/v1/pair", &body, Some(&service.token)).status,
        401
    );

    let sentinel = "private-node-credential-must-not-be-echoed";
    let malformed =
        json!({"request_id":"f".repeat(32),"credential":{"secret":sentinel}}).to_string();
    let rejected = post_json(service.port, "/nodes/v1/pair", &malformed, Some(token));
    assert_eq!(rejected.status, 400);
    assert!(!rejected.body.contains(sentinel));
    assert!(!rejected.body.contains(token));
    let oversized =
        json!({"request_id":"f".repeat(32),"credential":sentinel.repeat(300)}).to_string();
    let rejected = post_json(service.port, "/nodes/v1/pair", &oversized, Some(token));
    assert_eq!(rejected.status, 400);
    assert!(!rejected.body.contains(sentinel));
    assert!(!rejected.body.contains(token));

    let output = with_invitation(
        join_command(&machine, &url, &config, Some(&ca), true),
        token,
    );
    assert!(
        output.status.success(),
        "join failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let joined: Value = serde_json::from_slice(&output.stdout).unwrap();
    let saved_bytes = std::fs::read(&config).unwrap();
    let saved: Value = serde_json::from_slice(&saved_bytes).unwrap();
    assert_no_secret(&output, token);
    assert_no_secret(&output, saved["credential"].as_str().unwrap());
    assert_eq!(joined["hub_id"], invitation["hub_id"]);
    assert_eq!(joined["machine_id"], saved["machine_id"]);
    assert_eq!(joined["name"], "paired-node");
    assert_eq!(joined["workspaces"].as_array().unwrap().len(), 1);
    assert_eq!(joined["workspaces"][0]["id"], ids[0]);
    assert_eq!(saved["workspaces"], json!([ids[0]]));
    assert!(!pending.exists());
    assert_eq!(
        node_data(&hub)["node_machines"].as_array().unwrap().len(),
        1
    );
    assert!(node_data(&hub)["node_invitations"]
        .as_array()
        .unwrap()
        .is_empty());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&config).unwrap().permissions().mode() & 0o077,
            0
        );
    }

    let duplicate = with_invitation(
        join_command(&machine, &url, &config, Some(&ca), true),
        token,
    );
    assert!(!duplicate.status.success());
    assert_no_secret(&duplicate, token);
    assert_eq!(std::fs::read(&config).unwrap(), saved_bytes);
    assert!(!pending.exists());
    assert_eq!(
        node_data(&hub)["node_machines"].as_array().unwrap().len(),
        1
    );

    assert_no_daemon(&machine);
    let _node = NodeProcess::start(&machine, &config);
    let listed = wait_inventory(&service, 1, 1);
    assert!(!listed
        .iter()
        .any(|item| item["name"] == "paired-node/web" || item["name"] == "paired-node/unshared"));
    let workspace = find(&listed, "paired-node/api");
    assert_eq!(read(&service, &workspace)["content"], "paired-node/api\n");
    assert_no_daemon(&machine);
}

/// 只处理有界 HTTP 配对请求；第一次先收完真实 Hub 的成功响应，再丢弃它。
struct LostFirstReply {
    port: u16,
    completed: Arc<AtomicUsize>,
    stopping: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl LostFirstReply {
    fn new(upstream: u16) -> Self {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let completed = Arc::new(AtomicUsize::new(0));
        let count = completed.clone();
        let stopping = Arc::new(AtomicBool::new(false));
        let stop = stopping.clone();
        let thread = std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                let (mut client, _) = match listener.accept() {
                    Ok(accepted) => accepted,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::park_timeout(Duration::from_millis(10));
                        continue;
                    }
                    Err(error) => panic!("pairing relay accept: {error}"),
                };
                // 监听器用非阻塞 accept 轮询；已接受的连接明确切回阻塞读取。
                client.set_nonblocking(false).unwrap();
                client
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let (headers, body) = read_pair_request(&mut client);
                let mut forwarded = headers
                    .lines()
                    .filter(|line| !line.to_ascii_lowercase().starts_with("connection:"))
                    .map(|line| format!("{line}\r\n"))
                    .collect::<String>();
                forwarded.push_str("Connection: close\r\n\r\n");
                let mut request = forwarded.into_bytes();
                request.extend_from_slice(&body);
                let mut server = std::net::TcpStream::connect(("127.0.0.1", upstream)).unwrap();
                server
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                server.write_all(&request).unwrap();
                let mut response = Vec::new();
                server
                    .take(16 * 1024 + 1)
                    .read_to_end(&mut response)
                    .unwrap();
                assert!(
                    response.len() <= 16 * 1024,
                    "oversized pairing response in test relay"
                );
                assert!(
                    response.starts_with(b"HTTP/1.1 200 "),
                    "upstream pairing did not succeed"
                );
                if count.fetch_add(1, Ordering::SeqCst) != 0 {
                    client.write_all(&response).unwrap();
                }
            }
        });
        Self {
            port,
            completed,
            stopping,
            thread: Some(thread),
        }
    }
}

impl Drop for LostFirstReply {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let result = thread.join();
            if !std::thread::panicking() {
                result.expect("pairing relay failed");
            }
        }
    }
}

fn read_pair_request(stream: &mut std::net::TcpStream) -> (String, Vec<u8>) {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 1024];
    let header_end = loop {
        let read = stream.read(&mut buffer).unwrap();
        assert!(
            read > 0 && bytes.len() + read <= 16 * 1024,
            "invalid pairing request header"
        );
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers = String::from_utf8(bytes[..header_end - 4].to_vec()).unwrap();
    assert!(headers.starts_with("POST /nodes/v1/pair HTTP/1.1\r\n"));
    let length: usize = headers
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .unwrap()
        .1
        .trim()
        .parse()
        .unwrap();
    assert!(length <= 4096);
    while bytes.len() - header_end < length {
        let remaining = length - (bytes.len() - header_end);
        let read = stream.read(&mut buffer[..remaining.min(1024)]).unwrap();
        assert!(read > 0, "incomplete pairing request body");
        bytes.extend_from_slice(&buffer[..read]);
    }
    assert_eq!(bytes.len() - header_end, length);
    (headers, bytes[header_end..].to_vec())
}

#[test]
fn cli_join_recovers_a_lost_success_response_without_changing_the_pending_identity() {
    let hub = Env::new();
    let service = hub.serve("local", "bearer");
    let relay = LostFirstReply::new(service.port);
    let machine = Env::new();
    let ids = projects(&machine, "recovered-node");
    let url = format!("http://127.0.0.1:{}", relay.port);
    let invitation = hub.json(&["--json", "node", "invite", "recovered-node", "--hub", &url]);
    let token = invitation["invitation"].as_str().unwrap();
    let config = machine.home.path().join("recover.json");
    let pending_path = machine.home.path().join("recover.json.pending");
    let attempt = || {
        let mut command = join_command(&machine, &url, &config, None, true);
        for name in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            command.env(name, "http://127.0.0.1:1");
        }
        command.env("NO_PROXY", "").env("no_proxy", "");
        with_invitation(command, token)
    };
    let lost = attempt();
    assert!(!lost.status.success());
    assert_no_secret(&lost, token);
    assert!(!config.exists());
    let pending: Value = serde_json::from_slice(&std::fs::read(&pending_path).unwrap()).unwrap();
    assert_no_secret(&lost, pending["credential"].as_str().unwrap());
    assert_eq!(
        relay.completed.load(Ordering::SeqCst),
        1,
        "Hub succeeded before its response was dropped"
    );
    let first_data = node_data(&hub);
    assert_eq!(first_data["node_machines"].as_array().unwrap().len(), 1);
    assert_eq!(first_data["node_machines"][0]["id"], pending["machine_id"]);
    assert!(first_data["node_invitations"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_no_daemon(&machine);

    let restored = attempt();
    assert!(
        restored.status.success(),
        "pairing retry failed: {}",
        String::from_utf8_lossy(&restored.stderr)
    );
    assert_no_secret(&restored, token);
    assert_no_secret(&restored, pending["credential"].as_str().unwrap());
    let joined: Value = serde_json::from_slice(&restored.stdout).unwrap();
    let saved: Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    assert_eq!(
        saved, pending,
        "retry must preserve every persisted pairing field"
    );
    assert_eq!(joined["machine_id"], pending["machine_id"]);
    assert_eq!(saved["workspaces"], json!([ids[0]]));
    assert!(!pending_path.exists());
    assert_eq!(relay.completed.load(Ordering::SeqCst), 2);
    assert_eq!(
        node_data(&hub)["node_machines"],
        first_data["node_machines"]
    );
    assert_no_daemon(&machine);
}

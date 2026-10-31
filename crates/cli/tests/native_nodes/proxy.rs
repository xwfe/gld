//! 只能经代理出网的目标机：配对和长连接都走 CONNECT 隧道，TLS 和 Hub 身份校验照常在隧道里做。

use super::*;
use std::net::TcpListener as StdListener;
use std::sync::atomic::AtomicBool;

/// 不解 TLS：把 CONNECT 的目标改连到中转，之后原样转发字节，和真实代理一样看不到内容。
struct ConnectProxy {
    port: u16,
    targets: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
}

impl ConnectProxy {
    fn new(upstream: u16) -> Self {
        let listener = StdListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let targets = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (seen, stopping) = (targets.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                let Ok((client, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                };
                let seen = seen.clone();
                std::thread::spawn(move || tunnel(client, upstream, seen));
            }
        });
        Self {
            port,
            targets,
            stop,
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn targets(&self) -> Vec<String> {
        self.targets.lock().unwrap().clone()
    }
}

impl Drop for ConnectProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn tunnel(mut client: std::net::TcpStream, upstream: u16, seen: Arc<Mutex<Vec<String>>>) {
    client.set_nonblocking(false).unwrap();
    let mut header = Vec::new();
    let mut byte = [0_u8; 1];
    while !header.ends_with(b"\r\n\r\n") {
        if client.read_exact(&mut byte).is_err() {
            return;
        }
        header.push(byte[0]);
    }
    let header = String::from_utf8(header).unwrap();
    let target = header.split_whitespace().nth(1).unwrap_or_default();
    seen.lock().unwrap().push(target.to_string());
    let Ok(mut from_server) = std::net::TcpStream::connect(("127.0.0.1", upstream)) else {
        return;
    };
    client
        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
        .unwrap();
    let (mut from_client, mut to_client) = (client.try_clone().unwrap(), client);
    let mut to_server = from_server.try_clone().unwrap();
    let forward = std::thread::spawn(move || {
        let _ = std::io::copy(&mut from_client, &mut to_server);
        let _ = to_server.shutdown(std::net::Shutdown::Both);
    });
    let _ = std::io::copy(&mut from_server, &mut to_client);
    let _ = to_client.shutdown(std::net::Shutdown::Both);
    let _ = forward.join();
}

#[test]
fn a_machine_behind_a_connect_proxy_pairs_and_stays_connected() {
    let hub = Env::new();
    let service = hub.serve("local", "bearer");
    let relay = TlsRelay::new(service.port);
    let proxy = ConnectProxy::new(relay.port);
    let machine = Env::new();
    projects(&machine, "proxied");
    let ca = machine.home.path().join("proxy-ca.der");
    std::fs::write(&ca, decode(CA)).unwrap();
    let config = machine.home.path().join("proxied.json");
    let url = format!("https://localhost:{}", relay.port);
    let invitation = hub.json(&["--json", "node", "invite", "proxied"]);

    let mut join = command(&machine);
    join.env("HTTPS_PROXY", proxy.url())
        .args(["--json", "join", "--hub", &url, "--config"])
        .arg(&config)
        .args(["--share", "api", "--invite-stdin", "--yes"])
        .arg("--ca-certificate")
        .arg(&ca)
        .stdin(Stdio::piped());
    let mut child = join.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(invitation["invitation"].as_str().unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "join through the proxy failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(proxy.targets().len(), 1, "pairing should use the proxy");

    let child = command(&machine)
        .env("HTTPS_PROXY", proxy.url())
        .args(["node", "run", "--config"])
        .arg(&config)
        .spawn()
        .unwrap();
    let mut node = NodeProcess(Some(child));
    let items = wait_inventory(&service, 1, 1);
    let api = find(&items, "proxied/api");
    assert_eq!(read(&service, &api)["content"], "proxied/api\n");
    let targets = proxy.targets();
    assert_eq!(
        targets.len(),
        2,
        "one more CONNECT for the node: {targets:?}"
    );
    assert!(targets
        .iter()
        .all(|target| *target == format!("localhost:{}", relay.port)));
    node.stop();
}

#[test]
fn no_proxy_keeps_the_connection_direct() {
    let hub = Env::new();
    let service = hub.serve("local", "bearer");
    let relay = TlsRelay::new(service.port);
    let proxy = ConnectProxy::new(relay.port);
    let machine = Env::new();
    let ids = projects(&machine, "direct");
    let registration = register(&hub, "direct", TOKEN_A);
    let mut value = config(&machine, &relay, &registration, TOKEN_A, &ids);
    value["hub_url"] = json!(format!("https://localhost:{}", relay.port));
    let path = write_config(&machine, "direct.json", &value);
    let child = command(&machine)
        .env("HTTPS_PROXY", proxy.url())
        .env("NO_PROXY", "localhost")
        .args(["node", "run", "--config"])
        .arg(&path)
        .spawn()
        .unwrap();
    let mut node = NodeProcess(Some(child));
    wait_inventory(&service, 2, 2);
    assert!(proxy.targets().is_empty());
    node.stop();
}

#[test]
fn an_unsupported_proxy_stops_with_a_clear_error_instead_of_timing_out() {
    let hub = Env::new();
    let service = hub.serve("local", "bearer");
    let relay = TlsRelay::new(service.port);
    let machine = Env::new();
    let ids = projects(&machine, "socks");
    let registration = register(&hub, "socks", TOKEN_A);
    let value = config(&machine, &relay, &registration, TOKEN_A, &ids);
    let path = write_config(&machine, "socks.json", &value);
    let child = command(&machine)
        .env("HTTPS_PROXY", "socks5://user:hunter2@127.0.0.1:1080")
        .args(["node", "run", "--config"])
        .arg(&path)
        .spawn()
        .unwrap();
    let error = NodeProcess(Some(child)).rejected();
    assert!(error.contains("HTTP CONNECT"), "{error}");
    assert!(!error.contains("hunter2"), "{error}");
    assert_eq!(relay.connections(), 0);
}

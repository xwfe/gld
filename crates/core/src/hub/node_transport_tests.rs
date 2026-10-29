//! 单机上的两条真实 WSS 节点连接：只验证传输和现有项目分发，不冒充已提供机器配对/产品路由。

use super::*;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::rustls::{self, pki_types::CertificateDer, pki_types::PrivatePkcs8KeyDer};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest, protocol::Message, protocol::Role,
};
use tokio_tungstenite::{Connector, WebSocketStream};

use crate::bridge::peer::{Peer, PeerError, Transport};
use crate::nodes::transport::{socket_config, WebSocketTransport, MAX_MESSAGE_BYTES};

const WAIT: Duration = Duration::from_secs(5);
// 仅测试用的自签 CA/localhost 证书与私钥；2020–2040 有效，不用于发行或连接真实机器。
const CA: &str = "MIIBCjCBvaADAgECAgEBMAUGAytlcDAbMRkwFwYDVQQDDBBnbGQgdGVzdC1vbmx5IENBMB4XDTIwMDEwMTAwMDAwMFoXDTQwMDEwMTAwMDAwMFowGzEZMBcGA1UEAwwQZ2xkIHRlc3Qtb25seSBDQTAqMAUGAytlcAMhAHXUPrR9j/ZkaObE4CXVGDzGo8qs/nWQZsDGtDBq4SeIoyYwJDASBgNVHRMBAf8ECDAGAQH/AgEAMA4GA1UdDwEB/wQEAwIBhjAFBgMrZXADQQBwQqWkHLXIbEpn5g2sYRsTfrEjk7RdiQDC6+roEuIexiO+B0mVXoeq/lgJD5EuqhRw7S0jxtoY3nLy7dL95rYN";
const CERT: &str = "MIIBHjCB0aADAgECAgECMAUGAytlcDAbMRkwFwYDVQQDDBBnbGQgdGVzdC1vbmx5IENBMB4XDTIwMDEwMTAwMDAwMFoXDTQwMDEwMTAwMDAwMFowFDESMBAGA1UEAwwJbG9jYWxob3N0MCowBQYDK2VwAyEAMn7UEwSHwwibLtDgnmhfx3FV0XS6ZIaeWN9u7y7Bj7yjQTA/MAwGA1UdEwEB/wQCMAAwGgYDVR0RBBMwEYIJbG9jYWxob3N0hwR/AAABMBMGA1UdJQQMMAoGCCsGAQUFBwMBMAUGAytlcANBAG2LNzdf1sLA8n89CuHCsL8gz68WoKD7G8s6DmuBh6ESPzmfCQZS9NNsDo0QjZpvTI8jBv5FBQ7anS923FeL6Q0=";
const KEY: &str = "MC4CAQAwBQYDK2VwBCIEIE5CFjy/tFcNFC+1UPZj52Um/IO/1x0t9eJFA5yoRERI";

fn tls(trusted: bool) -> (TlsAcceptor, Connector) {
    let decode = |text| {
        base64::engine::general_purpose::STANDARD
            .decode(text)
            .unwrap()
    };
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let server = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(decode(CERT))],
            PrivatePkcs8KeyDer::from(decode(KEY)).into(),
        )
        .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    if trusted {
        roots.add(CertificateDer::from(decode(CA))).unwrap();
    }
    let client = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    (
        TlsAcceptor::from(Arc::new(server)),
        Connector::Rustls(Arc::new(client)),
    )
}

async fn connect(
    address: std::net::SocketAddr,
    token: &str,
    tls: Connector,
) -> Result<
    WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
    tokio_tungstenite::tungstenite::Error,
> {
    let mut request = format!("wss://{address}/nodes/v1/connect")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    tokio_tungstenite::connect_async_tls_with_config(
        request,
        Some(socket_config()),
        false,
        Some(tls),
    )
    .await
    .map(|(socket, _)| socket)
}

// tungstenite 的握手回调签名要求返回 HTTP Response，不能在调用端改成 Box。
#[allow(clippy::result_large_err)]
async fn accept(
    listener: &TcpListener,
    tls: &TlsAcceptor,
    expected_token: &'static str,
) -> Result<WebSocketStream<tokio_rustls::server::TlsStream<TcpStream>>, String> {
    let (tcp, _) = listener.accept().await.map_err(|e| e.to_string())?;
    let stream = tls.accept(tcp).await.map_err(|e| e.to_string())?;
    tokio_tungstenite::accept_hdr_async_with_config(
        stream,
        move |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
              response: tokio_tungstenite::tungstenite::handshake::server::Response| {
            if request
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                == Some(expected_token)
            {
                Ok(response)
            } else {
                Err(tokio_tungstenite::tungstenite::http::Response::builder()
                    .status(401)
                    .body(Some("unauthorized test node".into()))
                    .unwrap())
            }
        },
        Some(socket_config()),
    )
    .await
    .map_err(|e| e.to_string())
}

fn node(label: &str) -> (Vec<tempfile::TempDir>, Hub, Vec<String>) {
    crate::home::isolate_for_tests();
    let mut dirs = Vec::new();
    let mut profiles = Vec::new();
    for name in ["api", "web"] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), format!("{label}/{name}")).unwrap();
        profiles.push(WorkspaceProfile::new(
            dir.path().canonicalize().unwrap().display().to_string(),
            Some(name.into()),
        ));
        dirs.push(dir);
    }
    let ids = profiles.iter().map(|p| p.id.clone()).collect::<Vec<_>>();
    let mut settings = AppSettings::default();
    settings.hub.tool_profile = "read-only".into();
    settings.hub.members = ids.clone();
    let config = settings.hub.clone();
    let state_dir = tempfile::tempdir().unwrap();
    let mut hub = Hub::fixed(
        &config,
        Fixed {
            profiles,
            remotes: Vec::new(),
            settings,
        },
    );
    // 两台模拟机器各有自己的状态目录，不与其它测试竞争托管任务存储锁。
    hub.managed_home = Some(state_dir.path().to_path_buf());
    dirs.push(state_dir);
    (dirs, hub, ids)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_wss_nodes_each_use_one_peer_for_multiple_real_workspaces() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (server_tls, _) = tls(true);
        let connections = Arc::new(AtomicUsize::new(0));
        let accepted = connections.clone();
        let accepting = tokio::spawn(async move {
            let mut sockets = Vec::new();
            for expected in ["Bearer node-a", "Bearer node-b"] {
                sockets.push(accept(&listener, &server_tls, expected).await.unwrap());
                accepted.fetch_add(1, Ordering::SeqCst);
            }
            sockets
        });
        let mut nodes = Vec::new();
        let mut project_ids = Vec::new();
        for label in ["node-a", "node-b"] {
            let mut socket = connect(address, label, tls(true).1).await.unwrap();
            let (dirs, hub, ids) = node(label);
            project_ids.push(ids);
            nodes.push(tokio::spawn(async move {
                let _dirs = dirs;
                let hub = Arc::new(hub);
                // 测试中由已通过握手的连接绑定主体；不把网络请求降成 Caller::local。
                let auth = AuthContext::new(crate::auth::Principal::SharedSecret, HUB_SCOPE);
                loop {
                    // 等下一条请求是正常空闲，不使用“在途 RPC 超时就断线”的 recv_line。
                    let text = match socket.next().await {
                        Some(Ok(Message::Text(text))) => text,
                        Some(Ok(Message::Ping(_))) => {
                            socket.flush().await.unwrap();
                            continue;
                        }
                        Some(Ok(Message::Pong(_))) => continue,
                        Some(Ok(Message::Close(_))) => {
                            let _ = socket.flush().await;
                            break;
                        }
                        None => break,
                        Some(other) => panic!("unexpected node message: {other:?}"),
                    };
                    let request: Value = serde_json::from_str(text.as_str()).unwrap();
                    let handler = hub.clone();
                    let caller = auth.clone();
                    let response = tokio::task::spawn_blocking(move || {
                        handler.handle_request(&caller, &request).0
                    })
                    .await
                    .unwrap();
                    if !response.is_null() {
                        socket.send(Message::Text(response.to_string().into())).await.unwrap();
                    }
                }
                tokio::task::spawn_blocking(move || hub.shutdown()).await.unwrap();
            }));
        }
        let sockets = accepting.await.unwrap();
        let mut calls = Vec::new();
        // 每台机器仍只有一个串行 Peer；不同机器独立运行，不互相消耗空闲期限。
        for (index, socket) in sockets.into_iter().enumerate() {
            let wrong_project = project_ids[1 - index][0].clone();
            calls.push(tokio::task::spawn_blocking(move || {
                let mut peer = Peer::new(WebSocketTransport::new(socket).unwrap());
                peer.initialize("gld-hub", WAIT).unwrap();
                let tools = peer.list_tools(WAIT).unwrap();
                assert!(tools.iter().any(|tool| tool["name"] == "read_file"));
                let projects = peer.call_tool("list_workspaces", json!({}), WAIT).unwrap();
                assert_eq!(projects["structuredContent"]["count"], 2);
                for name in ["api", "web", "api"] {
                    let read = peer
                        .call_tool("read_file", json!({"workspace":name,"path":"README.md"}), WAIT)
                        .unwrap();
                    let label = if index == 0 { "a" } else { "b" };
                    assert_eq!(read["structuredContent"]["content"], format!("node-{label}/{name}"));
                }
                let wrong_machine = peer.call_tool(
                    "read_file", json!({"workspace":wrong_project,"path":"README.md"}), WAIT,
                ).unwrap();
                assert_eq!(wrong_machine["isError"], true);
                let escaped = peer.call_tool(
                    "read_file", json!({"workspace":"api","path":"../outside.txt"}), WAIT,
                ).unwrap();
                assert_eq!(escaped["isError"], true);
                let write = peer.call_tool(
                    "apply_patch",
                    json!({"workspace":"api","patch":"*** Begin Patch\n*** Add File: x\n+x\n*** End Patch"}),
                    WAIT,
                );
                assert!(matches!(write, Err(PeerError::Remote { code: -32602, .. })));
                peer.shutdown(WAIT);
            }));
        }
        for call in calls {
            call.await.unwrap();
        }
        for node in nodes {
            node.await.unwrap();
        }
        assert_eq!(connections.load(Ordering::SeqCst), 2);
    })
    .await
    .expect("two-node transport fixture did not finish within 60 seconds");
}

#[tokio::test(flavor = "multi_thread")]
async fn wss_rejects_wrong_node_credentials_and_untrusted_server_certificate() {
    for trusted in [true, false] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (server_tls, _) = tls(true);
        let accepting = tokio::spawn(async move {
            accept(&listener, &server_tls, "Bearer expected-node")
                .await
                .is_err()
        });
        let result = connect(address, "wrong-node", tls(trusted).1).await;
        assert!(result.is_err());
        if trusted {
            let tokio_tungstenite::tungstenite::Error::Http(response) = result.unwrap_err() else {
                panic!("expected 401, not a TLS error")
            };
            assert_eq!(response.status(), 401);
        } else {
            assert!(matches!(
                result,
                Err(tokio_tungstenite::tungstenite::Error::Io(_)
                    | tokio_tungstenite::tungstenite::Error::Tls(_))
            ));
        }
        assert!(accepting.await.unwrap());
    }
}

async fn memory_pair(
    limit: usize,
) -> (
    WebSocketTransport<tokio::io::DuplexStream>,
    WebSocketStream<tokio::io::DuplexStream>,
) {
    let (a, b) = tokio::io::duplex(4096);
    let config = socket_config()
        .max_message_size(Some(limit))
        .max_frame_size(Some(limit));
    let a = WebSocketStream::from_raw_socket(a, Role::Client, Some(config)).await;
    let b = WebSocketStream::from_raw_socket(b, Role::Server, Some(socket_config())).await;
    (WebSocketTransport::new(a).unwrap(), b)
}

#[tokio::test(flavor = "multi_thread")]
async fn peer_timeout_closes_the_socket_without_retrying_the_request() {
    let (wire, mut remote) = memory_pair(1024).await;
    let calling = tokio::task::spawn_blocking(move || {
        let mut peer = Peer::new(wire);
        assert!(matches!(
            peer.call_tool("read_file", json!({}), Duration::from_millis(100)),
            Err(PeerError::Timeout { .. })
        ));
        assert!(matches!(peer.ping(WAIT), Err(PeerError::Write(_))));
    });
    let Message::Text(text) = remote.next().await.unwrap().unwrap() else {
        panic!("expected text")
    };
    let message: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(message["jsonrpc"], "2.0");
    assert_eq!(message["method"], "tools/call");
    calling.await.unwrap();
    let next = tokio::time::timeout(WAIT, remote.next()).await.unwrap();
    assert!(!matches!(next, Some(Ok(Message::Text(_)))));
}

#[tokio::test(flavor = "multi_thread")]
async fn binary_and_oversize_messages_close_instead_of_reaching_rpc() {
    for message in [
        Message::Binary(vec![1, 2].into()),
        Message::Text("x".repeat(256).into()),
    ] {
        let (mut wire, mut remote) = memory_pair(128).await;
        remote.send(message).await.unwrap();
        tokio::task::spawn_blocking(move || {
            assert!(wire.recv_line(WAIT).unwrap().is_none());
            assert!(!wire.stderr_tail().is_empty());
            assert_eq!(
                wire.send_line("{}").unwrap_err().kind(),
                std::io::ErrorKind::NotConnected
            );
        })
        .await
        .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn websocket_ping_is_answered_without_changing_json_rpc_messages() {
    let (mut wire, mut remote) = memory_pair(1024).await;
    remote.send(Message::Ping(vec![7].into())).await.unwrap();
    remote
        .send(Message::Text(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}".into(),
        ))
        .await
        .unwrap();
    let receiving = tokio::task::spawn_blocking(move || {
        let response = wire.recv_line(WAIT).unwrap().unwrap();
        assert_eq!(serde_json::from_str::<Value>(&response).unwrap()["id"], 1);
        wire
    });
    let pong = tokio::time::timeout(WAIT, remote.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(pong, Message::Pong(vec![7].into()));
    drop(receiving.await.unwrap());
}

#[tokio::test]
async fn an_unbounded_socket_cannot_be_attached() {
    let (a, _b) = tokio::io::duplex(64);
    let socket = WebSocketStream::from_raw_socket(a, Role::Client, None).await;
    assert!(WebSocketTransport::new(socket).is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn oversize_outgoing_text_is_rejected_before_any_bytes_are_sent() {
    let (mut wire, mut remote) = memory_pair(1024).await;
    let sending = tokio::task::spawn_blocking(move || {
        let error = wire
            .send_line(&"x".repeat(MAX_MESSAGE_BYTES + 1))
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        wire.send_line("{}").unwrap();
        wire
    });
    assert_eq!(
        remote.next().await.unwrap().unwrap(),
        Message::Text("{}".into())
    );
    drop(sending.await.unwrap());
}

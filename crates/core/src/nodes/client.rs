//! 节点角色只建立一条出站连接，再把 JSON-RPC 交给现有 Hub 请求内核。
//! M0 断线即退出；配对和普通退避重连属于后续切片，不重放未知结果的调用。

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, protocol::Message};
use tokio_tungstenite::{Connector, WebSocketStream};

use crate::auth::{AuthContext, Grant, Principal};
use crate::bridge::peer::PROTOCOL_VERSION;
use crate::hub::Hub;

use super::config::NodeConfig;
use super::identity::auth_from_meta;
use super::transport::{socket_config, MAX_MESSAGE_BYTES};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_CA_BYTES: u64 = 64 * 1024;

/// 同步 CLI 入口。不会启动本机 MCP/OAuth/隧道，也不修改本机项目登记。
pub fn run(config: NodeConfig) -> Result<(), String> {
    config.validate()?;
    let hub = Arc::new(Hub::for_node(config.workspaces.clone())?);
    crate::async_rt::block_on(async move {
        let url = config.websocket_url()?;
        let mut request = url
            .into_client_request()
            .map_err(|_| "invalid node WebSocket endpoint".to_string())?;
        let mut credential: axum::http::HeaderValue = format!("Bearer {}", config.credential)
            .parse()
            .map_err(|_| "invalid node credential header".to_string())?;
        // HeaderValue 的 Debug 也不应展示凭据。
        credential.set_sensitive(true);
        request.headers_mut().insert("authorization", credential);
        let connector = ca_connector(&config)?;
        let connected = tokio::time::timeout(
            CONNECT_TIMEOUT,
            tokio_tungstenite::connect_async_tls_with_config(
                request,
                Some(socket_config()),
                false,
                connector,
            ),
        )
        .await
        .map_err(|_| "node TLS/WebSocket connection timed out".to_string())?;
        let (mut socket, response) = connected.map_err(|error| match error {
            tokio_tungstenite::tungstenite::Error::Http(response) => format!(
                "node admission failed with HTTP {}",
                response.status().as_u16()
            ),
            _ => "node TLS/WebSocket connection failed; check the endpoint and trusted CA".into(),
        })?;
        if response
            .headers()
            .get("x-gld-hub-id")
            .and_then(|v| v.to_str().ok())
            != Some(config.hub_id.as_str())
        {
            let _ = tokio::time::timeout(WRITE_TIMEOUT, socket.close(None)).await;
            return Err("node endpoint returned a different Hub identity".into());
        }
        tokio::select! {
            result = serve_socket(socket, hub, config) => result,
            _ = tokio::signal::ctrl_c() => Ok(()),
        }
    })
}

fn ca_connector(config: &NodeConfig) -> Result<Option<Connector>, String> {
    let Some(path) = config.ca_certificate.as_ref() else {
        return Ok(None);
    };
    let file = std::fs::File::open(path)
        .map_err(|_| "cannot read the configured node CA certificate".to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_CA_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read the configured node CA certificate".to_string())?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_CA_BYTES {
        return Err("node CA certificate must be a DER certificate of at most 64 KiB".into());
    }
    use tokio_rustls::rustls;
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(bytes))
        .map_err(|_| "node CA certificate is not a valid DER certificate".to_string())?;
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|_| "node TLS protocol configuration is unavailable".to_string())?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(Some(Connector::Rustls(Arc::new(tls))))
}

fn control_auth(config: &NodeConfig) -> AuthContext {
    AuthContext::new(Principal::SharedSecret, "node-control").with_grant(Some(Grant {
        id: "node-control".into(),
        name: "node project inventory".into(),
        workspaces: config.workspaces.clone(),
        read_only: true,
        browser: false,
    }))
}

async fn send_response<S>(socket: &mut WebSocketStream<S>, response: Value) -> Result<(), String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if response.is_null() {
        return Ok(());
    }
    let text = response.to_string();
    if text.len() > MAX_MESSAGE_BYTES {
        return Err("node response exceeds its message limit".into());
    }
    tokio::time::timeout(WRITE_TIMEOUT, socket.send(Message::Text(text.into())))
        .await
        .map_err(|_| "node response write timed out".to_string())?
        .map_err(|_| "node response write failed".to_string())
}

fn rpc_error(body: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": body.get("id").cloned().unwrap_or(Value::Null),
        "error": { "code": code, "message": message } })
}

/// 已完成 TLS/节点鉴权及 Hub 身份核对后的连接；测试可用合成 TLS 流走同一分发。
pub(crate) async fn serve_socket<S>(
    mut socket: WebSocketStream<S>,
    hub: Arc<Hub>,
    config: NodeConfig,
) -> Result<(), String>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut initialized = false;
    let mut idle_deadline = tokio::time::Instant::now() + IDLE_TIMEOUT;
    loop {
        let message = tokio::time::timeout_at(idle_deadline, socket.next())
            .await
            .map_err(|_| "node Hub heartbeat timed out".to_string())?;
        idle_deadline = tokio::time::Instant::now() + IDLE_TIMEOUT;
        let text = match message {
            Some(Ok(Message::Text(text))) => text,
            Some(Ok(Message::Ping(_))) => {
                tokio::time::timeout(WRITE_TIMEOUT, socket.flush())
                    .await
                    .map_err(|_| "node Pong write timed out".to_string())?
                    .map_err(|_| "node Pong write failed".to_string())?;
                continue;
            }
            Some(Ok(Message::Pong(_))) => continue,
            Some(Ok(Message::Close(_))) | None => return Ok(()),
            Some(Ok(_)) => return Err("node RPC requires WebSocket Text messages".into()),
            Some(Err(_)) => {
                return Err("node WebSocket closed or exceeded its message limit".into())
            }
        };
        let body: Value = match serde_json::from_str(text.as_str()) {
            Ok(body) => body,
            Err(_) => {
                send_response(&mut socket, rpc_error(&Value::Null, -32700, "Invalid JSON")).await?;
                continue;
            }
        };
        if body.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            send_response(
                &mut socket,
                rpc_error(&body, -32600, "JSON-RPC 2.0 required"),
            )
            .await?;
            continue;
        }
        let method = body.get("method").and_then(Value::as_str).unwrap_or("");
        if method == "initialize" {
            if body
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
                != Some(PROTOCOL_VERSION)
            {
                send_response(
                    &mut socket,
                    rpc_error(&body, -32602, "Unsupported protocolVersion"),
                )
                .await?;
                continue;
            }
            initialized = true;
        } else if !initialized {
            send_response(
                &mut socket,
                rpc_error(&body, -32600, "Initialize the node first"),
            )
            .await?;
            continue;
        }
        // Peer 初始化通知后可立即发 tools/list；通知没有响应，也不占 RPC 槽。
        if method == "notifications/initialized" {
            continue;
        }
        let auth = if method == "tools/call" {
            match auth_from_meta(&body["params"], &config.hub_id, &config.workspaces) {
                Ok(auth) => auth,
                Err(_) => {
                    send_response(
                        &mut socket,
                        rpc_error(&body, -32602, "Trusted node caller metadata required"),
                    )
                    .await?;
                    continue;
                }
            }
        } else {
            control_auth(&config)
        };
        let handler = hub.clone();
        let mut response =
            tokio::task::spawn_blocking(move || handler.handle_request(&auth, &body).0);
        // 工具执行时继续处理标准控制帧，不为第二条 RPC 建队列。
        let result = loop {
            tokio::select! {
                result = &mut response => break result.map_err(|_| "node request handler failed".to_string())?,
                incoming = socket.next() => match incoming {
                    Some(Ok(Message::Ping(_))) => {
                        tokio::time::timeout(WRITE_TIMEOUT, socket.flush()).await
                            .map_err(|_| "node Pong write timed out".to_string())?
                            .map_err(|_| "node Pong write failed".to_string())?;
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) | None => return Ok(()),
                    Some(Ok(_)) => return Err("node accepts only one in-flight RPC".into()),
                    Some(Err(_)) => return Err("node WebSocket read failed".into()),
                }
            }
        };
        send_response(&mut socket, result).await?;
        idle_deadline = tokio::time::Instant::now() + IDLE_TIMEOUT;
    }
}

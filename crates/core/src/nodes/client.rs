//! 节点角色只建立一条出站连接，再把 JSON-RPC 交给现有 Hub 请求内核。
//! 短暂断线后重新鉴权、初始化和列项目；旧调用只收尾，不重放。

use std::future::Future;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, protocol::Message};
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};

use crate::auth::{AuthContext, Grant, Principal};
use crate::bridge::peer::PROTOCOL_VERSION;
use crate::hub::Hub;
use crate::tools::workspace::{tool_err_code, wrap_tool_result};

use super::config::NodeConfig;
use super::hub_key::{
    new_nonce, verify_handshake, Binding, MessageVerifier, NONCE_HEADER, PROOF_HEADER,
};
use super::identity::auth_from_meta;
use super::proxy::{self, Proxy, ProxyError};
use super::transport::{socket_config, MAX_MESSAGE_BYTES, MESSAGE_WRITE_TIMEOUT};

#[path = "reconnect.rs"]
mod reconnect;
use reconnect::{Backoff, Failure, Stability};

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_CA_BYTES: u64 = 64 * 1024;

/// 同步 CLI 入口。不会启动本机 MCP/OAuth/隧道，也不修改本机项目登记。
pub fn run(config: NodeConfig) -> Result<(), String> {
    config.validate()?;
    let hub = Arc::new(Hub::for_node(config.access())?);
    let outcome = crate::async_rt::block_on(async move {
        let shutdown = stop_signal();
        tokio::pin!(shutdown);
        run_until(config, hub, &mut shutdown).await
    });
    // 不管是被停还是凭据被撤销退出，这里起的命令都没人能再读、再停了，不能留着以这个账号跑。
    crate::tools::workspace_runtime::terminate_all_sessions_everywhere();
    outcome
}

/// 开机自启时系统用 SIGTERM 停进程，前台运行时关终端或 SSH 断开发 SIGHUP；没接住的话进程直接死，
/// 命令留在后台。`nohup` 的意思就是“别因为 SIGHUP 退出”，那种情况不接：一接就把忽略覆盖掉了。
async fn stop_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            let hangup = async {
                // 先查再接：接了之后查到的就是自己的处理函数了。
                if !hangup_ignored() {
                    if let Ok(mut hup) = signal(SignalKind::hangup()) {
                        hup.recv().await;
                        return;
                    }
                }
                std::future::pending::<()>().await
            };
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
                _ = hangup => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(unix)]
fn hangup_ignored() -> bool {
    let mut current: libc::sigaction = unsafe { std::mem::zeroed() };
    let queried = unsafe { libc::sigaction(libc::SIGHUP, std::ptr::null(), &mut current) };
    queried == 0 && current.sa_sigaction == libc::SIG_IGN
}

async fn run_until<F>(config: NodeConfig, hub: Arc<Hub>, shutdown: &mut F) -> Result<(), String>
where
    F: Future<Output = ()> + Unpin,
{
    let mut request = config
        .websocket_url()?
        .into_client_request()
        .map_err(|_| "invalid node WebSocket endpoint".to_string())?;
    let mut credential: axum::http::HeaderValue =
        format!("Bearer {}", config.credential)
            .parse()
            .map_err(|_| "invalid node credential header".to_string())?;
    credential.set_sensitive(true);
    request.headers_mut().insert("authorization", credential);
    let connector = ca_connector(&config)?;
    let hub_url = reqwest::Url::parse(&config.hub_url).map_err(|_| "Invalid node Hub URL")?;
    let proxy = proxy::for_hub(&hub_url)?;
    let mut backoff = Backoff::default();
    loop {
        let connect_timeout =
            backoff.connection_timeout(std::time::Instant::now(), CONNECT_TIMEOUT)?;
        // 每次连接换一个 nonce，旧连接里的握手证明和消息签名都不能拿来重放。
        let binding = Binding::new(&config.hub_id, &config.machine_id, &new_nonce()?)?;
        let mut attempt = request.clone();
        attempt.headers_mut().insert(
            NONCE_HEADER,
            binding
                .nonce()
                .parse()
                .map_err(|_| "invalid node nonce header".to_string())?,
        );
        let connected = tokio::select! {
            biased;
            _ = &mut *shutdown => return Ok(()),
            result = tokio::time::timeout(connect_timeout,
                open_socket(attempt, proxy.as_ref(), connector.clone())) =>
                result.map_err(|_| Failure::transient("node TLS/WebSocket connection timed out"))
                    .and_then(|result| result),
        };
        let failure = match connected {
            Err(failure) => failure,
            Ok((mut socket, response)) => {
                let header = |name: &str| {
                    response
                        .headers()
                        .get(name)
                        .and_then(|value| value.to_str().ok())
                };
                if header("x-gld-hub-id") != Some(config.hub_id.as_str()) {
                    let _ = tokio::time::timeout(WRITE_TIMEOUT, SinkExt::close(&mut socket)).await;
                    return Err("node endpoint returned a different Hub identity".into());
                }
                if !header(PROOF_HEADER)
                    .is_some_and(|proof| verify_handshake(&config.hub_public_key, &binding, proof))
                {
                    let _ = tokio::time::timeout(WRITE_TIMEOUT, SinkExt::close(&mut socket)).await;
                    return Err("node endpoint could not prove the pinned Hub key; it is not the paired Hub, or the Hub key changed and this machine must join again".into());
                }
                backoff.connected();
                let mut stability = Stability::new();
                let verifier = MessageVerifier::new(&config.hub_public_key, binding);
                let result = serve_until(
                    socket,
                    hub.clone(),
                    &config,
                    verifier,
                    shutdown,
                    &mut stability,
                )
                .await;
                backoff.session_ended(stability.stable);
                match result {
                    Ok(SessionEnd::Stopped) => return Ok(()),
                    Ok(SessionEnd::Disconnected) => {
                        Failure::transient("node Hub connection closed")
                    }
                    Err(failure) => failure,
                }
            }
        };
        let random = uuid::Uuid::new_v4().as_u128() as u64;
        let delay = backoff.next(&failure, std::time::Instant::now(), random)?;
        eprintln!(
            "{}; retrying the connection in {:.1}s without replaying requests",
            failure.message,
            delay.as_secs_f64()
        );
        tokio::select! {
            biased;
            _ = &mut *shutdown => return Ok(()),
            _ = tokio::time::sleep(delay) => {}
        }
    }
}

type ClientSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
type HandshakeResponse = tokio_tungstenite::tungstenite::handshake::client::Response;

async fn open_socket(
    request: tokio_tungstenite::tungstenite::handshake::client::Request,
    proxy: Option<&Proxy>,
    connector: Option<Connector>,
) -> Result<(ClientSocket, HandshakeResponse), Failure> {
    let Some(proxy) = proxy else {
        return tokio_tungstenite::connect_async_tls_with_config(
            request,
            Some(socket_config()),
            false,
            connector,
        )
        .await
        .map_err(Failure::websocket);
    };
    let uri = request.uri();
    let host = uri.host().unwrap_or_default().to_string();
    let port = uri.port_u16().unwrap_or(443);
    let tunnel = proxy
        .connect(&host, port)
        .await
        .map_err(|error| match error {
            ProxyError::Unreachable(message) => Failure::transient(message),
            ProxyError::Rejected(message) => Failure::permanent(message),
        })?;
    tokio_tungstenite::client_async_tls_with_config(
        request,
        tunnel,
        Some(socket_config()),
        connector,
    )
    .await
    .map_err(Failure::websocket)
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
        read_only: config.writable_workspaces.is_empty(),
        browser: false,
    }))
}

/// 节点自己拼的控制和错误回复都很小；超限说明代码出错，按不可重试处理。
async fn send_response<S>(socket: &mut WebSocketStream<S>, response: Value) -> Result<(), Failure>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if response.is_null() {
        return Ok(());
    }
    let text = response.to_string();
    if text.len() > MAX_MESSAGE_BYTES {
        return Err(Failure::permanent(
            "node response exceeds its message limit",
        ));
    }
    write_text(socket, text).await
}

/// `view_image` 的结果里同一张图有三份 base64：图片块、`base64`、`data_url`，回包约为原图 4 倍。
/// 节点消息只有 8 MiB，这里只留客户端要的那一份；图片块模式约为原图 1.33 倍，默认 5 MiB 的图放得下。
/// 只在节点上裁：本机客户端已经在用的字段不动。`data_url` 模式的文本块跟着重写，
/// 它和结构化结果相等，Hub 才认得出来并同步改句柄（`registry.rs::tag_result`）。
fn keep_one_image_copy(response: &mut Value) {
    let Some(result) = response.get_mut("result").and_then(Value::as_object_mut) else {
        return;
    };
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return;
    }
    let image_block = result
        .get("content")
        .and_then(Value::as_array)
        .is_some_and(|blocks| blocks.iter().any(|block| block["type"] == "image"));
    let Some(structured) = result
        .get_mut("structuredContent")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    structured.remove("base64");
    if image_block {
        structured.remove("data_url");
        return;
    }
    let text = Value::Object(structured.clone()).to_string();
    if let Some(block) = result
        .get_mut("content")
        .and_then(|blocks| blocks.get_mut(0))
    {
        block["text"] = json!(text);
    }
}

/// 处理器的结果已经在本机生效（补丁可能已落盘），超限时只能丢弃正文，换一条小错误回给这一次调用。
/// 以前整个节点退出，机器离线到有人手动重启。
async fn send_result<S>(
    socket: &mut WebSocketStream<S>,
    result: Value,
    tool_call: bool,
) -> Result<(), Failure>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if result.is_null() {
        return Ok(());
    }
    let text = result.to_string();
    if text.len() <= MAX_MESSAGE_BYTES {
        return write_text(socket, text).await;
    }
    send_response(socket, oversized_reply(&result, tool_call, text.len())).await
}

fn oversized_reply(result: &Value, tool_call: bool, bytes: usize) -> Value {
    if !tool_call {
        return rpc_error(
            result,
            -32603,
            "Node response exceeds the message limit and was discarded",
        );
    }
    let message = format!(
        "The tool ran on this machine, but its result ({bytes} bytes) exceeds the \
         {MAX_MESSAGE_BYTES}-byte node message limit and was discarded. \
         Ask for less output, such as a smaller max_bytes or a narrower range."
    );
    json!({
        "jsonrpc": "2.0",
        "id": result.get("id").cloned().unwrap_or(Value::Null),
        "result": wrap_tool_result(tool_err_code("NODE_RESPONSE_TOO_LARGE", message, "validation")),
    })
}

async fn write_text<S>(socket: &mut WebSocketStream<S>, text: String) -> Result<(), Failure>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    tokio::time::timeout(
        MESSAGE_WRITE_TIMEOUT,
        socket.send(Message::Text(text.into())),
    )
    .await
    .map_err(|_| Failure::transient("node response write timed out"))?
    .map_err(Failure::websocket)
}

async fn flush_control<S>(socket: &mut WebSocketStream<S>) -> Result<(), Failure>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    tokio::time::timeout(WRITE_TIMEOUT, socket.flush())
        .await
        .map_err(|_| Failure::transient("node Pong write timed out"))?
        .map_err(Failure::websocket)
}

fn rpc_error(body: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": body.get("id").cloned().unwrap_or(Value::Null),
        "error": { "code": code, "message": message } })
}

#[derive(Debug, PartialEq, Eq)]
enum SessionEnd {
    Disconnected,
    Stopped,
}

#[derive(Debug)]
enum CallOutcome {
    Response(Value),
    End(SessionEnd),
}

fn close_outcome(
    frame: Option<tokio_tungstenite::tungstenite::protocol::CloseFrame>,
) -> Result<SessionEnd, Failure> {
    if frame.is_some_and(|frame| matches!(u16::from(frame.code), 1002 | 1003 | 1007..=1010)) {
        Err(Failure::permanent(
            "node Hub rejected the protocol, policy or message limit",
        ))
    } else {
        Ok(SessionEnd::Disconnected)
    }
}

/// 重连前收回正在执行的同步处理器，结果丢弃，不带入下一代连接。
/// 用户停止会退出连接循环；已经开始的同步调用不能强行取消。
async fn wait_response<S, F>(
    socket: &mut WebSocketStream<S>,
    mut response: tokio::task::JoinHandle<Value>,
    shutdown: &mut F,
    stability: &mut Stability,
) -> Result<CallOutcome, Failure>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: Future<Output = ()> + Unpin,
{
    let end = loop {
        tokio::select! {
            biased;
            _ = &mut *shutdown => break Ok(SessionEnd::Stopped),
            result = &mut response => return result.map(CallOutcome::Response)
                .map_err(|_| Failure::permanent("node request handler failed")),
            incoming = tokio::time::timeout(IDLE_TIMEOUT, socket.next()) => match incoming {
                Ok(Some(Ok(Message::Ping(_)))) => {
                    stability.received();
                    if let Err(error) = flush_control(socket).await {
                        break Err(error);
                    }
                }
                Ok(Some(Ok(Message::Pong(_)))) => stability.received(),
                Ok(Some(Ok(Message::Close(frame)))) => break close_outcome(frame),
                Ok(None) => break Ok(SessionEnd::Disconnected),
                Ok(Some(Ok(_))) => break Err(Failure::permanent("node accepts only one in-flight RPC")),
                Ok(Some(Err(error))) => break Err(Failure::websocket(error)),
                Err(_) => break Err(Failure::transient("node Hub heartbeat timed out")),
            }
        }
    };
    // 收到 Close 时已有自动应答排队；Sink 的 close 会刷新它，不能再 send 一个 Close。
    let _ = tokio::time::timeout(WRITE_TIMEOUT, SinkExt::close(socket)).await;
    if matches!(&end, Ok(SessionEnd::Stopped)) {
        return Ok(CallOutcome::End(SessionEnd::Stopped));
    }
    tokio::select! {
        biased;
        _ = &mut *shutdown => Ok(CallOutcome::End(SessionEnd::Stopped)),
        joined = &mut response => {
            joined.map_err(|_| Failure::permanent("node request handler failed"))?;
            end.map(CallOutcome::End)
        }
    }
}

async fn serve_until<S, F>(
    mut socket: WebSocketStream<S>,
    hub: Arc<Hub>,
    config: &NodeConfig,
    mut verifier: MessageVerifier,
    shutdown: &mut F,
    stability: &mut Stability,
) -> Result<SessionEnd, Failure>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    F: Future<Output = ()> + Unpin,
{
    let mut initialized = false;
    loop {
        let message = tokio::select! {
            biased;
            _ = &mut *shutdown => {
                let _ = tokio::time::timeout(WRITE_TIMEOUT, SinkExt::close(&mut socket)).await;
                return Ok(SessionEnd::Stopped);
            }
            message = tokio::time::timeout(IDLE_TIMEOUT, socket.next()) =>
                message.map_err(|_| Failure::transient("node Hub heartbeat timed out"))?,
        };
        let text = match message {
            // 验签在解析之前：被插入、改写或重放的消息连 JSON 都不解析，更不会执行。
            Some(Ok(Message::Text(text))) => {
                stability.received();
                verifier.verify(text.as_str()).map_err(|reason| {
                    Failure::permanent(format!(
                        "{reason}; the connection may pass through an untrusted relay"
                    ))
                })?
            }
            Some(Ok(Message::Ping(_))) => {
                stability.received();
                flush_control(&mut socket).await?;
                continue;
            }
            Some(Ok(Message::Pong(_))) => {
                stability.received();
                continue;
            }
            Some(Ok(Message::Close(frame))) => return close_outcome(frame),
            None => return Ok(SessionEnd::Disconnected),
            Some(Ok(_)) => {
                return Err(Failure::permanent(
                    "node RPC requires WebSocket Text messages",
                ))
            }
            Some(Err(error)) => return Err(Failure::websocket(error)),
        };
        let body: Value = match serde_json::from_str(text.as_str()) {
            Ok(body) => body,
            Err(_) => {
                let _ = send_response(&mut socket, rpc_error(&Value::Null, -32700, "Invalid JSON"))
                    .await;
                return Err(Failure::permanent("node Hub sent invalid JSON"));
            }
        };
        if body.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            let _ = send_response(
                &mut socket,
                rpc_error(&body, -32600, "JSON-RPC 2.0 required"),
            )
            .await;
            return Err(Failure::permanent("node Hub requires JSON-RPC 2.0"));
        }
        let method = body.get("method").and_then(Value::as_str).unwrap_or("");
        if method == "initialize" {
            if body
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
                != Some(PROTOCOL_VERSION)
            {
                let _ = send_response(
                    &mut socket,
                    rpc_error(&body, -32602, "Unsupported protocolVersion"),
                )
                .await;
                return Err(Failure::permanent(
                    "node Hub protocolVersion is unsupported",
                ));
            }
            initialized = true;
        } else if !initialized {
            let _ = send_response(
                &mut socket,
                rpc_error(&body, -32600, "Initialize the node first"),
            )
            .await;
            return Err(Failure::permanent(
                "node Hub did not initialize the connection",
            ));
        }
        // Peer 初始化通知后可立即发 tools/list；通知没有响应，也不占 RPC 槽。
        if method == "notifications/initialized" {
            continue;
        }
        let auth = if method == "tools/call" {
            match auth_from_meta(
                &body["params"],
                &config.hub_id,
                &config.workspaces,
                &config.writable_workspaces,
            ) {
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
            control_auth(config)
        };
        let tool_call = method == "tools/call";
        let handler = hub.clone();
        let response = tokio::task::spawn_blocking(move || {
            let mut response = handler.handle_request(&auth, &body).0;
            if tool_call
                && body.pointer("/params/name").and_then(Value::as_str) == Some("view_image")
            {
                keep_one_image_copy(&mut response);
            }
            response
        });
        match wait_response(&mut socket, response, shutdown, stability).await? {
            CallOutcome::Response(result) => send_result(&mut socket, result, tool_call).await?,
            CallOutcome::End(end) => return Ok(end),
        }
    }
}

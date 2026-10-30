//! 把一条已建立的 WebSocket 接到现有同步 Peer；每条 Text message 就是一条 JSON-RPC。
//! 本模块不鉴权、不解析项目、不重连、不重放。调用者须先验证节点身份，并在阻塞线程中使用。

use std::io;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{Sink, SinkExt, Stream, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::protocol::{Message, WebSocketConfig};
use tokio_tungstenite::WebSocketStream;

use super::hub_key::MessageSigner;
use crate::async_rt;
use crate::bridge::peer::Transport;

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;

pub const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// 一整条 RPC 消息的写超时，8 MiB 约需 280 KB/s（2.2 Mbps）。Ping 这类小控制帧仍用 5 秒：
/// 写不出去说明连接已坏，不必多等。
pub(super) const MESSAGE_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// 握手或接管 HTTP/1.1 Upgrade 时使用；限制应在库接收/拼接 message 之前生效。
pub fn socket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_MESSAGE_BYTES))
}

pub struct SocketTransport<S> {
    socket: Option<S>,
    signer: Option<MessageSigner>,
    cancel: Option<Arc<Notify>>,
    last_error: String,
    last_ping: Instant,
    waiting_pong: Option<Instant>,
}

pub type WebSocketTransport<S> = SocketTransport<WebSocketStream<S>>;

impl<S: AsyncRead + AsyncWrite + Unpin> WebSocketTransport<S> {
    /// 接受普通 WS 或 TLS stream；网络鉴权、TLS 校验和单机唯一连接由接入层负责。
    /// 构造本身不会初始化 MCP、调用工具或再建任何连接。
    pub fn new(socket: WebSocketStream<S>) -> io::Result<Self> {
        let config = socket.get_config();
        if config
            .max_message_size
            .is_none_or(|n| n > MAX_MESSAGE_BYTES)
            || config.max_frame_size.is_none_or(|n| n > MAX_MESSAGE_BYTES)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "node socket must use bounded message and frame sizes",
            ));
        }
        Ok(Self::bounded(socket))
    }
}

impl<S> SocketTransport<S> {
    pub(crate) fn bounded(socket: S) -> Self {
        Self {
            socket: Some(socket),
            signer: None,
            cancel: None,
            last_error: String::new(),
            last_ping: Instant::now(),
            waiting_pong: None,
        }
    }

    /// Hub 的生产连接都要签名；不签的只剩只测传输层的用例。
    pub(crate) fn signed(mut self, signer: MessageSigner) -> Self {
        self.signer = Some(signer);
        self
    }

    /// 发一次通知就切断正在等的收发。调用方持着 Peer 的锁阻塞在这里，别的线程没有别的办法叫停它。
    pub(crate) fn cancellable(mut self, cancel: Arc<Notify>) -> Self {
        self.cancel = Some(cancel);
        self
    }

    pub fn is_open(&self) -> bool {
        self.socket.is_some()
    }

    fn disconnect(&mut self, reason: &str) {
        self.last_error = reason.into();
        self.socket.take();
    }
}

async fn cancelled(cancel: Option<&Arc<Notify>>) {
    match cancel {
        Some(cancel) => cancel.notified().await,
        None => std::future::pending().await,
    }
}

impl<S, E> Transport for SocketTransport<S>
where
    S: Stream<Item = Result<Message, E>> + Sink<Message, Error = E> + Unpin + Send,
{
    fn send_line(&mut self, line: &str) -> io::Result<()> {
        let signed;
        let line = match self.signer.as_mut() {
            Some(signer) => {
                signed = signer
                    .sign(line, MAX_MESSAGE_BYTES)
                    .map_err(|reason| io::Error::new(io::ErrorKind::InvalidInput, reason))?;
                signed.as_str()
            }
            None => line,
        };
        if line.len() > MAX_MESSAGE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "node message exceeds the size limit; it was not sent",
            ));
        }
        let socket = self.socket.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "node connection is closed")
        })?;
        let cancel = self.cancel.clone();
        // 写入也必须有界；不加发送队列，否则超时返回后旧请求还可能被排队发送。
        let outcome = async_rt::block_on(async {
            tokio::select! {
                biased;
                _ = cancelled(cancel.as_ref()) => None,
                result = tokio::time::timeout(
                    MESSAGE_WRITE_TIMEOUT,
                    socket.send(Message::Text(line.into())),
                ) => Some(result),
            }
        });
        let Some(outcome) = outcome else {
            self.disconnect("node connection is closing; request outcome may be unknown");
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                self.last_error.clone(),
            ));
        };
        match outcome {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => {
                self.disconnect("WebSocket write failed; request outcome may be unknown");
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    self.last_error.clone(),
                ))
            }
            Err(_) => {
                self.disconnect("WebSocket write timed out; request outcome may be unknown");
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    self.last_error.clone(),
                ))
            }
        }
    }

    fn recv_line(&mut self, timeout: Duration) -> Result<Option<String>, RecvTimeoutError> {
        let Some(socket) = self.socket.as_mut() else {
            return Ok(None);
        };
        let mut saw_pong = false;
        let cancel = self.cancel.clone();
        let outcome = async_rt::block_on(async {
            let read = tokio::time::timeout(timeout, async {
                loop {
                    match socket.next().await {
                        Some(Ok(Message::Text(text))) => return Ok(Some(text.to_string())),
                        // tungstenite 自动准备 Pong；flush 发送标准响应，不引入业务心跳协议。
                        Some(Ok(Message::Ping(_))) => {
                            socket
                                .flush()
                                .await
                                .map_err(|_| "WebSocket control write failed")?;
                        }
                        Some(Ok(Message::Pong(payload))) => {
                            saw_pong |= payload.as_ref() == b"gld-node";
                        }
                        Some(Ok(Message::Close(_))) => {
                            let _ = socket.flush().await;
                            return Ok(None);
                        }
                        None => return Ok(None),
                        Some(Ok(_)) => return Err("node RPC requires a WebSocket Text message"),
                        Some(Err(_)) => {
                            return Err("WebSocket read failed or message exceeded its limit")
                        }
                    }
                }
            });
            tokio::select! {
                biased;
                _ = cancelled(cancel.as_ref()) => {
                    Ok(Err("node connection is closing; request outcome may be unknown"))
                }
                result = read => result,
            }
        });
        if saw_pong {
            self.waiting_pong = None;
        }
        match outcome {
            Ok(Ok(Some(text))) => Ok(Some(text)),
            Ok(Ok(None)) => {
                self.disconnect("node connection closed");
                Ok(None)
            }
            Ok(Err(reason)) => {
                self.disconnect(reason);
                Ok(None)
            }
            Err(_) => {
                // 整条连接失效，晚到响应不能混入后续请求，也没有任何自动补发。
                self.disconnect("WebSocket response timed out; request outcome may be unknown");
                Err(RecvTimeoutError::Timeout)
            }
        }
    }

    fn stderr_tail(&self) -> String {
        // 沿用 Peer 的传输诊断入口，不把原始请求、凭据或远端数据拼入错误。
        self.last_error.clone()
    }

    fn shutdown(&mut self, grace: Duration) {
        if let Some(mut socket) = self.socket.take() {
            async_rt::block_on(async {
                let _ = tokio::time::timeout(grace, socket.close()).await;
            });
        }
    }
}

impl<S, E> SocketTransport<S>
where
    S: Stream<Item = Result<Message, E>> + Sink<Message, Error = E> + Unpin + Send,
{
    /// 空闲短等不是 RPC 超时；Close、EOF 或标准 Pong 超时才释放机器名额。
    pub fn poll_idle(&mut self, wait: Duration) {
        if self
            .waiting_pong
            .is_some_and(|sent| sent.elapsed() >= Duration::from_secs(20))
        {
            self.disconnect("node WebSocket Pong timed out");
            return;
        }
        if self.waiting_pong.is_none() && self.last_ping.elapsed() >= Duration::from_secs(20) {
            let Some(socket) = self.socket.as_mut() else {
                return;
            };
            let sent = async_rt::block_on(async {
                tokio::time::timeout(
                    WRITE_TIMEOUT,
                    socket.send(Message::Ping(b"gld-node".to_vec().into())),
                )
                .await
            });
            if !matches!(sent, Ok(Ok(()))) {
                self.disconnect("node WebSocket Ping write failed");
                return;
            }
            let now = Instant::now();
            self.last_ping = now;
            self.waiting_pong = Some(now);
        }
        let Some(socket) = self.socket.as_mut() else {
            return;
        };
        let outcome = async_rt::block_on(async {
            tokio::time::timeout(wait, async {
                let message = socket.next().await;
                if matches!(&message, Some(Ok(Message::Ping(_) | Message::Close(_)))) {
                    socket.flush().await.map_err(|_| ())?;
                }
                Ok::<_, ()>(message)
            })
            .await
        });
        match outcome {
            Err(_) => {}
            Ok(Ok(Some(Ok(Message::Ping(_))))) => {}
            Ok(Ok(Some(Ok(Message::Pong(payload))))) => {
                if payload.as_ref() == b"gld-node" {
                    self.waiting_pong = None;
                }
            }
            Ok(Ok(Some(Ok(Message::Close(_)))) | Ok(None)) => {
                self.disconnect("node connection closed")
            }
            Ok(Ok(Some(Ok(_)))) => self.disconnect("unsolicited node RPC message"),
            Ok(Err(_) | Ok(Some(Err(_)))) => {
                self.disconnect("WebSocket read failed or message exceeded its limit")
            }
        }
    }
}

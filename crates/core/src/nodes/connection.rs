//! 一个已认证机器连接上的单一 Peer；空闲控制帧不会占用另一个连接或 RPC 队列。

use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::extract::ws::{self, WebSocket};
use futures_util::{Sink, Stream};
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Message};

use super::hub_key::MessageSigner;
use super::transport::SocketTransport;
use crate::bridge::peer::{Peer, PeerError};

/// 关连接时给在途调用多久自己收完。一次调用最多要等 60 秒，不能全等：`gld daemon stop` 只等 20 秒，
/// 多台机器一起忙时更久。切断后调用方拿到“结果不明”，和断线一样，不会重放。
const CLOSE_GRACE: Duration = Duration::from_secs(2);

/// 连接正忙时最多等多久再报 BUSY。客户端常把几个读请求并行发出来，每个只要几十毫秒，
/// 立刻拒绝的话 8 个里有 6 个失败（真机测过）；真正卡住的调用（等命令 yield、慢网）等完也还是 BUSY，不无限排队。
const BUSY_WAIT: Duration = Duration::from_secs(5);

/// Axum 把同一 WebSocket 消息包装成自己的类型；这里只转换类型，不解析 JSON-RPC。
pub struct AxumSocket(WebSocket);
pub type AxumTransport = SocketTransport<AxumSocket>;

fn socket_error(_: axum::Error) -> io::Error {
    io::Error::other("node WebSocket I/O failed")
}

impl Stream for AxumSocket {
    type Item = Result<Message, io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.get_mut().0).poll_next(cx).map(|item| {
            item.map(|message| {
                message
                    .map(|message| match message {
                        ws::Message::Text(text) => Message::Text(text.to_string().into()),
                        ws::Message::Binary(data) => Message::Binary(data),
                        ws::Message::Ping(data) => Message::Ping(data),
                        ws::Message::Pong(data) => Message::Pong(data),
                        ws::Message::Close(frame) => {
                            Message::Close(frame.map(|frame| CloseFrame {
                                code: frame.code.into(),
                                reason: frame.reason.to_string().into(),
                            }))
                        }
                    })
                    .map_err(socket_error)
            })
        })
    }
}

impl Sink<Message> for AxumSocket {
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.get_mut().0)
            .poll_ready(cx)
            .map_err(socket_error)
    }

    fn start_send(self: Pin<&mut Self>, message: Message) -> Result<(), Self::Error> {
        let message = match message {
            Message::Text(text) => ws::Message::Text(text.to_string().into()),
            Message::Binary(data) => ws::Message::Binary(data),
            Message::Ping(data) => ws::Message::Ping(data),
            Message::Pong(data) => ws::Message::Pong(data),
            Message::Close(frame) => ws::Message::Close(frame.map(|frame| ws::CloseFrame {
                code: frame.code.into(),
                reason: frame.reason.to_string().into(),
            })),
            Message::Frame(_) => return Err(io::Error::other("raw WebSocket frames are not RPC")),
        };
        Pin::new(&mut self.get_mut().0)
            .start_send(message)
            .map_err(socket_error)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.get_mut().0)
            .poll_flush(cx)
            .map_err(socket_error)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.get_mut().0)
            .poll_close(cx)
            .map_err(socket_error)
    }
}

#[derive(Debug)]
pub enum ConnectionError {
    Busy,
    Closed,
    Peer(PeerError),
}

impl std::fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => f.write_str("another RPC is using this machine connection"),
            Self::Closed => f.write_str("machine connection is closed"),
            Self::Peer(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ConnectionError {}

pub struct Connection {
    peer: Mutex<Peer<AxumTransport>>,
    in_flight: AtomicBool,
    closing: AtomicBool,
    closed: AtomicBool,
    cancel: Arc<Notify>,
}

struct InFlight<'a>(&'a AtomicBool);
impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Connection {
    /// 监听器必须在 Upgrade 前限制 message/frame 大小；Axum 升级后不暴露该配置。
    /// 签名器绑定本次握手的 nonce；Hub 发出的每条消息都签，节点验不过就断开。
    pub fn new(socket: WebSocket, signer: MessageSigner) -> Arc<Self> {
        let cancel = Arc::new(Notify::new());
        Arc::new(Self {
            peer: Mutex::new(Peer::new(
                SocketTransport::bounded(AxumSocket(socket))
                    .signed(signer)
                    .cancellable(cancel.clone()),
            )),
            in_flight: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            cancel,
        })
    }

    /// 同一时刻只有一个 RPC。别的调用短等一下，等不到就报 Busy，这时什么都没发。
    /// RPC 可等正在结束的短暂 idle poll，不因心跳误报 Busy。
    pub fn with_peer<R>(
        &self,
        call: impl FnOnce(&mut Peer<AxumTransport>) -> Result<R, PeerError>,
    ) -> Result<R, ConnectionError> {
        if self.closing.load(Ordering::Acquire) || self.is_closed() {
            return Err(ConnectionError::Closed);
        }
        let deadline = Instant::now() + BUSY_WAIT;
        while self
            .in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            if self.closing.load(Ordering::Acquire) || self.is_closed() {
                return Err(ConnectionError::Closed);
            }
            if Instant::now() >= deadline {
                return Err(ConnectionError::Busy);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _in_flight = InFlight(&self.in_flight);
        let mut peer = self.peer.lock().unwrap_or_else(|e| e.into_inner());
        if self.closing.load(Ordering::Acquire) || !peer.transport_mut().is_open() {
            peer.shutdown(Duration::from_millis(50));
            self.closed.store(true, Ordering::Release);
            return Err(ConnectionError::Closed);
        }
        let result = call(&mut peer);
        let fatal = match &result {
            Ok(_) | Err(PeerError::Remote { .. } | PeerError::Unsupported { .. }) => false,
            Err(PeerError::Write(error)) if error.kind() == io::ErrorKind::InvalidInput => false,
            Err(_) => true,
        };
        if fatal || self.closing.load(Ordering::Acquire) {
            peer.shutdown(Duration::from_millis(50));
        }
        if !peer.transport_mut().is_open() {
            self.closed.store(true, Ordering::Release);
        }
        result.map_err(ConnectionError::Peer)
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// 在阻塞线程中调用；在途调用收尾或被切断、实际 socket 关闭后，才释放机器名额。
    pub fn close(&self) {
        self.closing.store(true, Ordering::Release);
        let deadline = Instant::now() + CLOSE_GRACE;
        while self.in_flight.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        self.cancel.notify_one();
        let mut peer = self.peer.lock().unwrap_or_else(|e| e.into_inner());
        peer.shutdown(Duration::from_millis(50));
        self.closed.store(true, Ordering::Release);
    }

    fn poll_idle(&self) {
        if self.in_flight.load(Ordering::Acquire) {
            return;
        }
        let Ok(mut peer) = self.peer.try_lock() else {
            return;
        };
        if self.closing.load(Ordering::Acquire) {
            peer.shutdown(Duration::from_millis(50));
        } else {
            peer.transport_mut().poll_idle(Duration::from_millis(10));
        }
        if !peer.transport_mut().is_open() {
            self.closed.store(true, Ordering::Release);
        }
    }

    /// 等到旧 socket 实际结束后，注册层才可释放唯一连接登记。
    pub async fn monitor(self: Arc<Self>) {
        while !self.is_closed() {
            let connection = self.clone();
            if tokio::task::spawn_blocking(move || connection.poll_idle())
                .await
                .is_err()
            {
                // 出错后仍先关闭实际 socket，再标记结束。
                let connection = self.clone();
                let _ = tokio::task::spawn_blocking(move || connection.close()).await;
            }
            if !self.is_closed() {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    }
}

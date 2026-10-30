use super::*;
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

struct Socket {
    incoming: VecDeque<Message>,
    sent: Arc<Mutex<Vec<Message>>>,
    eof: bool,
}

impl Stream for Socket {
    type Item = Result<Message, io::Error>;
    fn poll_next(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.incoming.pop_front() {
            Some(message) => Poll::Ready(Some(Ok(message))),
            None if self.eof => Poll::Ready(None),
            None => Poll::Pending,
        }
    }
}

impl Sink<Message> for Socket {
    type Error = io::Error;
    fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn start_send(self: Pin<&mut Self>, message: Message) -> io::Result<()> {
        self.sent.lock().unwrap().push(message);
        Ok(())
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

fn fixture(
    messages: Vec<Message>,
    eof: bool,
) -> (SocketTransport<Socket>, Arc<Mutex<Vec<Message>>>) {
    let sent = Arc::new(Mutex::new(Vec::new()));
    (
        SocketTransport::bounded(Socket {
            incoming: messages.into(),
            sent: sent.clone(),
            eof,
        }),
        sent,
    )
}

#[test]
fn idle_poll_does_not_turn_a_quiet_connection_into_a_failed_rpc() {
    let (mut transport, sent) = fixture(vec![], false);
    for _ in 0..3 {
        transport.poll_idle(Duration::from_millis(1));
    }
    assert!(transport.is_open());
    assert!(sent.lock().unwrap().is_empty());
}

#[test]
fn standard_pong_keeps_the_single_connection_and_missing_pong_expires_it() {
    let (mut transport, sent) = fixture(vec![Message::Pong(b"gld-node".to_vec().into())], false);
    transport.last_ping = Instant::now() - Duration::from_secs(21);
    transport.poll_idle(Duration::from_millis(1));
    assert!(transport.is_open());
    assert!(transport.waiting_pong.is_none());
    assert!(matches!(
        sent.lock().unwrap().as_slice(),
        [Message::Ping(_)]
    ));

    transport.waiting_pong = Some(Instant::now() - Duration::from_secs(21));
    transport.poll_idle(Duration::from_millis(1));
    assert!(!transport.is_open());
    assert_eq!(
        sent.lock().unwrap().len(),
        1,
        "expired socket must not queue or repeat a call"
    );
}

#[test]
fn idle_eof_and_unsolicited_rpc_close_without_sending_any_tool_request() {
    for (incoming, eof) in [(vec![], true), (vec![Message::Text("{}".into())], false)] {
        let (mut transport, sent) = fixture(incoming, eof);
        transport.poll_idle(Duration::from_millis(1));
        assert!(!transport.is_open());
        assert!(sent.lock().unwrap().is_empty());
    }
}

/// Hub 这边同理：大补丁发给慢网上的节点时，5 秒写不完也不能算断线。
#[test]
fn a_large_request_on_a_slow_link_is_not_cut_off_after_five_seconds() {
    let (hub, node) = tokio::io::duplex(16 * 1024);
    let (mut hub, node) = async_rt::block_on(async {
        (
            WebSocketTransport::new(
                WebSocketStream::from_raw_socket(
                    hub,
                    tokio_tungstenite::tungstenite::protocol::Role::Server,
                    Some(socket_config()),
                )
                .await,
            )
            .unwrap(),
            WebSocketStream::from_raw_socket(
                node,
                tokio_tungstenite::tungstenite::protocol::Role::Client,
                Some(socket_config()),
            )
            .await,
        )
    });
    let line = "x".repeat(1024 * 1024);
    let drain = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(6));
        async_rt::block_on(async move {
            let mut node = node;
            node.next().await
        })
    });
    hub.send_line(&line).unwrap();
    assert!(matches!(
        drain.join().unwrap(),
        Some(Ok(Message::Text(text))) if text.len() == line.len()
    ));
}

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::DuplexStream;
use tokio_tungstenite::tungstenite::protocol::Role;

async fn sockets() -> (WebSocketStream<DuplexStream>, WebSocketStream<DuplexStream>) {
    let (left, right) = tokio::io::duplex(16 * 1024);
    (
        WebSocketStream::from_raw_socket(left, Role::Client, Some(socket_config())).await,
        WebSocketStream::from_raw_socket(right, Role::Server, Some(socket_config())).await,
    )
}

struct PendingHandler {
    task: tokio::task::JoinHandle<Value>,
    started: tokio::sync::oneshot::Receiver<()>,
    release: std::sync::mpsc::Sender<()>,
    completed: Arc<AtomicUsize>,
    finished: tokio::sync::oneshot::Receiver<()>,
}

fn pending_handler() -> PendingHandler {
    let (ready, started) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let (done, finished) = tokio::sync::oneshot::channel();
    let completed = Arc::new(AtomicUsize::new(0));
    let count = completed.clone();
    let task = tokio::task::spawn_blocking(move || {
        let _ = ready.send(());
        let _ = wait.recv();
        count.fetch_add(1, Ordering::SeqCst);
        let _ = done.send(());
        json!({"jsonrpc":"2.0","id":1,"result":{"must_not_be_replayed":true}})
    });
    PendingHandler {
        task,
        started,
        release,
        completed,
        finished,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnect_joins_the_running_handler_and_discards_its_response() {
    let (mut node, mut hub) = sockets().await;
    let handler = pending_handler();
    let mut operation = tokio::spawn(async move {
        wait_response(
            &mut node,
            handler.task,
            &mut std::future::pending(),
            &mut Stability::new(),
        )
        .await
    });
    handler.started.await.unwrap();
    hub.close(None).await.unwrap();
    let closed = tokio::time::timeout(Duration::from_secs(2), hub.next())
        .await
        .unwrap();
    assert!(matches!(closed, Some(Ok(Message::Close(_)))));
    assert!(
        !operation.is_finished(),
        "the old handler must finish before reconnection is allowed"
    );
    handler.release.send(()).unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(2), &mut operation)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(ended, CallOutcome::End(SessionEnd::Disconnected)));
    assert_eq!(handler.completed.load(Ordering::SeqCst), 1);
    assert!(matches!(hub.next().await, None | Some(Err(_))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_closes_the_socket_and_stops_without_waiting_for_the_handler() {
    let (mut node, mut hub) = sockets().await;
    let handler = pending_handler();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let operation = tokio::spawn(async move {
        let shutdown = async {
            let _ = stopped.await;
        };
        tokio::pin!(shutdown);
        wait_response(
            &mut node,
            handler.task,
            &mut shutdown,
            &mut Stability::new(),
        )
        .await
    });
    handler.started.await.unwrap();
    stop.send(()).unwrap();
    let closed = tokio::time::timeout(Duration::from_secs(2), hub.next())
        .await
        .unwrap();
    assert!(matches!(closed, Some(Ok(Message::Close(_)))));
    let ended = tokio::time::timeout(Duration::from_secs(2), operation)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(ended, CallOutcome::End(SessionEnd::Stopped)));
    assert_eq!(handler.completed.load(Ordering::SeqCst), 0);
    handler.release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), handler.finished)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(handler.completed.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_can_interrupt_draining_a_disconnected_handler() {
    let (mut node, mut hub) = sockets().await;
    let handler = pending_handler();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let operation = tokio::spawn(async move {
        let shutdown = async {
            let _ = stopped.await;
        };
        tokio::pin!(shutdown);
        wait_response(
            &mut node,
            handler.task,
            &mut shutdown,
            &mut Stability::new(),
        )
        .await
    });
    handler.started.await.unwrap();
    hub.close(None).await.unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), hub.next())
            .await
            .unwrap(),
        Some(Ok(Message::Close(_)))
    ));
    assert!(!operation.is_finished());
    stop.send(()).unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(2), operation)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(ended, CallOutcome::End(SessionEnd::Stopped)));
    assert_eq!(handler.completed.load(Ordering::SeqCst), 0);
    handler.release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), handler.finished)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn control_frames_continue_but_a_second_rpc_closes_and_drains_the_first() {
    let (mut node, mut hub) = sockets().await;
    let handler = pending_handler();
    let operation = tokio::spawn(async move {
        wait_response(
            &mut node,
            handler.task,
            &mut std::future::pending(),
            &mut Stability::new(),
        )
        .await
    });
    handler.started.await.unwrap();
    hub.send(Message::Ping(b"probe".to_vec().into()))
        .await
        .unwrap();
    let pong = tokio::time::timeout(Duration::from_secs(2), hub.next())
        .await
        .unwrap();
    assert!(matches!(pong, Some(Ok(Message::Pong(value))) if value.as_ref() == b"probe"));
    hub.send(Message::Text("{\"jsonrpc\":\"2.0\",\"id\":2}".into()))
        .await
        .unwrap();
    let closed = tokio::time::timeout(Duration::from_secs(2), hub.next())
        .await
        .unwrap();
    assert!(matches!(closed, Some(Ok(Message::Close(_)))));
    assert!(!operation.is_finished());
    handler.release.send(()).unwrap();
    let failure = tokio::time::timeout(Duration::from_secs(2), operation)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(failure.retry, reconnect::Retry::Never);
    assert_eq!(handler.completed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn oversized_response_is_terminal_before_any_write() {
    let (mut node, mut hub) = sockets().await;
    let failure = send_response(&mut node, json!("x".repeat(MAX_MESSAGE_BYTES)))
        .await
        .unwrap_err();
    assert_eq!(failure.retry, reconnect::Retry::Never);
    // 在随后合法消息之前没有任何超限正文写入 socket。
    node.send(Message::Text("marker".into())).await.unwrap();
    assert!(matches!(hub.next().await, Some(Ok(Message::Text(value))) if value == "marker"));
}

#[tokio::test]
async fn eof_during_http_upgrade_is_a_transient_connection_failure() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_tungstenite::tungstenite::{error::ProtocolError, Error};
    let (node, mut peer) = tokio::io::duplex(16 * 1024);
    let interrupted = async {
        let mut headers = Vec::new();
        let mut chunk = [0_u8; 1024];
        while !headers.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            let count = peer.read(&mut chunk).await.unwrap();
            assert!(count > 0 && headers.len() < 8192);
            headers.extend_from_slice(&chunk[..count]);
        }
        peer.write_all(b"HTTP/1.1 101 Switching Protocols\r\n")
            .await
            .unwrap();
        peer.shutdown().await.unwrap();
    };
    let connected = tokio_tungstenite::client_async("ws://127.0.0.1:80/nodes", node);
    let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(connected, interrupted)
    })
    .await
    .unwrap();
    let error = result.unwrap_err();
    assert!(matches!(
        &error,
        Error::Protocol(ProtocolError::HandshakeIncomplete)
    ));
    assert_eq!(Failure::websocket(error).retry, reconnect::Retry::Transient);
}

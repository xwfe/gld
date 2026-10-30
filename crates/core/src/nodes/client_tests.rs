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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_relay_cannot_alter_a_hub_message_even_when_it_keeps_the_signature_tag() {
    use super::super::hub_key::{generate_private_key, HubKey, MessageSigner};
    let project = "d".repeat(32);
    let key = Arc::new(HubKey::from_stored(&generate_private_key().unwrap()).unwrap());
    let config = NodeConfig {
        hub_url: "https://example.invalid/hub".into(),
        hub_id: "a".repeat(32),
        hub_public_key: key.public_key(),
        machine_id: "b".repeat(32),
        credential: "c".repeat(64),
        workspaces: vec![project.clone()],
        writable_workspaces: Vec::new(),
        runnable_workspaces: Vec::new(),
        ca_certificate: None,
    };
    let binding = Binding::new(&config.hub_id, &config.machine_id, &new_nonce().unwrap()).unwrap();
    let verifier = MessageVerifier::new(&config.hub_public_key, binding.clone());
    let hub = Arc::new(Hub::for_node_tests(vec![project]));
    let (node, mut relay) = sockets().await;
    let serving = tokio::spawn(async move {
        serve_until(
            node,
            hub,
            &config,
            verifier,
            &mut std::future::pending(),
            &mut Stability::new(),
        )
        .await
    });
    let mut signer = MessageSigner::new(key, binding);
    let initialize = json!({"jsonrpc":"2.0","id":1,"method":"initialize",
        "params":{"protocolVersion":PROTOCOL_VERSION}})
    .to_string();
    relay
        .send(Message::Text(
            signer.sign(&initialize, MAX_MESSAGE_BYTES).unwrap().into(),
        ))
        .await
        .unwrap();
    let answered = tokio::time::timeout(Duration::from_secs(5), relay.next())
        .await
        .unwrap();
    assert!(
        matches!(&answered, Some(Ok(Message::Text(text))) if text.contains("\"result\"")),
        "a signed message is handled normally: {answered:?}"
    );

    // 中间人保留签名段、只改调用内容：节点不解析、不执行、不回复，按不可重试的失败退出。
    let call = json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
        "params":{"name":"list_workspaces","arguments":{}}})
    .to_string();
    let altered = signer
        .sign(&call, MAX_MESSAGE_BYTES)
        .unwrap()
        .replace("list_workspaces", "workspace_context");
    relay.send(Message::Text(altered.into())).await.unwrap();
    let failure = tokio::time::timeout(Duration::from_secs(5), serving)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(failure.retry, reconnect::Retry::Never);
    assert!(failure.message.contains("signature"), "{}", failure.message);
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), relay.next())
            .await
            .unwrap(),
        None | Some(Err(_))
    ));
}

#[tokio::test]
async fn an_oversized_control_reply_is_terminal_before_any_write() {
    let (mut node, mut hub) = sockets().await;
    let failure = send_response(&mut node, json!("x".repeat(MAX_MESSAGE_BYTES)))
        .await
        .unwrap_err();
    assert_eq!(failure.retry, reconnect::Retry::Never);
    // 在随后合法消息之前没有任何超限正文写入 socket。
    node.send(Message::Text("marker".into())).await.unwrap();
    assert!(matches!(hub.next().await, Some(Ok(Message::Text(value))) if value == "marker"));
}

async fn next_json(hub: &mut WebSocketStream<DuplexStream>) -> Value {
    match tokio::time::timeout(Duration::from_secs(2), hub.next())
        .await
        .unwrap()
    {
        Some(Ok(Message::Text(text))) => {
            assert!(text.len() < 4096, "only a small error may be written");
            serde_json::from_str(text.as_str()).unwrap()
        }
        other => panic!("expected one JSON reply, got {other:?}"),
    }
}

#[tokio::test]
async fn an_oversized_tool_result_fails_only_that_call_and_keeps_the_connection() {
    let (mut node, mut hub) = sockets().await;
    let huge = |id: u64| {
        json!({"jsonrpc":"2.0","id":id,"result":{
            "content":[{"type":"text","text":"x".repeat(MAX_MESSAGE_BYTES)}],"isError":false}})
    };
    send_result(&mut node, huge(7), true).await.unwrap();
    let reply = next_json(&mut hub).await;
    assert_eq!(reply["id"], 7);
    assert_eq!(reply["result"]["isError"], true);
    let error = &reply["result"]["structuredContent"]["error"];
    assert_eq!(error["code"], "NODE_RESPONSE_TOO_LARGE");
    assert_eq!(error["retryable"], false);
    assert!(error["message"]
        .as_str()
        .unwrap()
        .contains("ran on this machine"));

    // 非工具调用没有工具结果的形状，回标准 JSON-RPC 错误；两次之后连接仍能照常收发。
    send_result(&mut node, huge(8), false).await.unwrap();
    let reply = next_json(&mut hub).await;
    assert_eq!(reply["id"], 8);
    assert_eq!(reply["error"]["code"], -32603);
    let small = json!({"jsonrpc":"2.0","id":9,"result":{"ok":true}});
    send_result(&mut node, small.clone(), true).await.unwrap();
    assert_eq!(next_json(&mut hub).await, small);
}

fn image_response(output: &str) -> (Value, String) {
    let encoded = "QUJD".repeat(1000);
    let structured = json!({
        "ok": true, "path": "a.png", "mime_type": "image/png", "bytes": 3000,
        "base64": encoded, "data_url": format!("data:image/png;base64,{encoded}"),
    });
    let result = crate::tools::workspace::wrap_mcp_tool_result(
        "view_image",
        &json!({"output": output}),
        structured,
    );
    (json!({"jsonrpc":"2.0","id":1,"result":result}), encoded)
}

#[test]
fn view_image_keeps_only_the_copy_the_client_asked_for() {
    let (mut response, encoded) = image_response("mcp_image");
    assert_eq!(response.to_string().matches(encoded.as_str()).count(), 3);
    keep_one_image_copy(&mut response);
    assert_eq!(response.to_string().matches(encoded.as_str()).count(), 1);
    let result = &response["result"];
    assert_eq!(result["content"][0]["type"], "image");
    assert_eq!(result["content"][0]["data"], json!(encoded));
    assert_eq!(result["structuredContent"]["bytes"], 3000);

    let (mut response, encoded) = image_response("data_url");
    keep_one_image_copy(&mut response);
    let result = &response["result"];
    assert!(result["structuredContent"].get("base64").is_none());
    assert!(result["structuredContent"]["data_url"]
        .as_str()
        .unwrap()
        .ends_with(encoded.as_str()));
    // 文本块要和结构化结果相等，否则 Hub 不会同步改里面的项目句柄。
    let text: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text, result["structuredContent"]);
}

#[test]
fn a_failed_view_image_is_left_alone() {
    let failure = wrap_tool_result(tool_err_code("BINARY_FILE", "not an image", "validation"));
    let mut response = json!({"jsonrpc":"2.0","id":1,"result":failure});
    let before = response.clone();
    keep_one_image_copy(&mut response);
    assert_eq!(response, before);
}

/// 5 Mbps 上行发 4 MB 约 7 秒。写超时以前是 5 秒，慢网下回包发到一半就断线。
#[tokio::test]
async fn a_large_reply_on_a_slow_link_is_not_cut_off_after_five_seconds() {
    let (mut node, mut hub) = sockets().await;
    let reply = json!({"jsonrpc":"2.0","id":1,"result":{"text":"x".repeat(1024 * 1024)}});
    let drain = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(6)).await;
        hub.next().await
    });
    send_result(&mut node, reply.clone(), true).await.unwrap();
    match drain.await.unwrap() {
        Some(Ok(Message::Text(text))) => assert_eq!(text.len(), reply.to_string().len()),
        other => panic!("expected the whole reply, got {other:?}"),
    }
}

/// 这几个数互相牵制，只改一个不会报错，要到线上才发现调用被无故断开。
#[test]
fn the_hub_call_budget_fits_the_longest_tool_wait_and_a_slow_reply() {
    use crate::tools::args::bounded;
    let longest_wait = [
        ("exec_command", "yield_time_ms"),
        ("write_stdin", "yield_time_ms"),
        ("kill_session", "wait_ms"),
    ]
    .iter()
    .map(|(tool, arg)| bounded(&json!({ *arg: u64::MAX }), tool, arg))
    .max()
    .unwrap();
    let call = crate::nodes::registry::CALL;
    assert!(call >= Duration::from_millis(longest_wait) + MESSAGE_WRITE_TIMEOUT);
    assert!(call <= IDLE_TIMEOUT);
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

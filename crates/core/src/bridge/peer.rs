//! 一个 MCP stdio 客户端：gld 用它连 `ccnm mcp bridge`。
//!
//! gld 本来只有 MCP **服务端**（`mcp::listener`），这是第一个客户端方向的
//! 实现。它只做 RFC-0002 第 5 节要的那几件事：initialize、tools/list、
//! tools/call、ping、关闭，**不做**通用 MCP 客户端——没有 sampling、没有
//! roots、不实现服务端反向请求。
//!
//! 传输抽成 [`Transport`]，所以协议逻辑能用内存管道测，不必每个用例都起
//! 一个进程。只有 [`spawn`] 这一步碰真实子进程。
//!
//! 三条来自 RFC 的硬要求，都在这一层：
//!
//! - **调用串行**：`&mut self`，一次一个请求。远端 writer guard 不允许同一
//!   连接上并发 patch 与 exec。
//! - **每次调用都有 deadline**：SSH 可以黑洞掉不报错，读 stdout 必须能超时
//!   放弃，否则 hub 的工作线程就挂在那里。
//! - **stderr 有界收集**：ccnm 的 `CCNM_E_*` 诊断只在 stderr 上，而 Host
//!   通常把它丢掉（ccnm 自己的排障文档记过这个坑）。这里留最后若干字节，
//!   启动失败时能把真正的原因报出来。

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// MCP 协议版本。跟 ccnm 冻结的 `ccnm.workspace-mcp/1` 用的是同一个。
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// stderr 最多留这么多字节。够放下 ccnm 的一行 `CCNM_E_*` 诊断加上下文，
/// 又不至于让一个刷屏的远端把内存吃掉。
const STDERR_KEEP: usize = 8 * 1024;

/// 跟远端说话时出的岔子。
///
/// 故意不实现 `From<io::Error>`：每一处失败都要说清楚是在哪一步，因为
/// 「连不上」「握手不对」「超时」对调用方是完全不同的处理。
#[derive(Debug)]
pub enum PeerError {
    /// 进程都没起来。
    Spawn {
        program: String,
        source: std::io::Error,
    },
    /// 写请求失败，通常是对面已经死了。
    Write(std::io::Error),
    /// 等回复超时。`waited` 是实际等了多久。
    Timeout { method: String, waited: Duration },
    /// 对面把管道关了。`stderr` 是它留下的最后一段话。
    Closed { method: String, stderr: String },
    /// 回了一条不是 JSON，或者不是 JSON-RPC 的东西。
    Malformed { method: String, detail: String },
    /// 对面明确回了一个 JSON-RPC error。
    Remote {
        method: String,
        code: i64,
        message: String,
    },
    /// 握手结果不能接受，比如协议版本对不上。
    Handshake(String),
}

impl std::fmt::Display for PeerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PeerError::Spawn { program, source } => {
                write!(f, "cannot start {program}: {source}")
            }
            PeerError::Write(e) => write!(f, "cannot send to the bridge: {e}"),
            PeerError::Timeout { method, waited } => write!(
                f,
                "the bridge did not answer {method} within {:.1}s",
                waited.as_secs_f64()
            ),
            PeerError::Closed { method, stderr } if stderr.is_empty() => {
                write!(f, "the bridge closed the connection during {method}")
            }
            PeerError::Closed { method, stderr } => write!(
                f,
                "the bridge closed the connection during {method}; it said: {stderr}"
            ),
            PeerError::Malformed { method, detail } => {
                write!(
                    f,
                    "the bridge answered {method} with something unusable: {detail}"
                )
            }
            PeerError::Remote {
                method,
                code,
                message,
            } => {
                write!(f, "the bridge refused {method} ({code}): {message}")
            }
            PeerError::Handshake(detail) => write!(f, "the bridge handshake is unusable: {detail}"),
        }
    }
}

impl std::error::Error for PeerError {}

/// 一条双向的行式通道。真实实现是子进程的 stdin/stdout；测试用内存管道。
pub trait Transport: Send {
    /// 发一行（实现负责补换行并 flush）。
    fn send_line(&mut self, line: &str) -> std::io::Result<()>;
    /// 收一行，最多等 `timeout`。`Ok(None)` 表示对面关了。
    fn recv_line(&mut self, timeout: Duration) -> Result<Option<String>, RecvTimeoutError>;
    /// 对面在 stderr 上留下的最后一段话，没有就是空串。
    fn stderr_tail(&self) -> String {
        String::new()
    }
}

/// 跟一个 MCP 服务端的一次连接。
///
/// 调用是串行的（`&mut self`），符合 RFC-0002 5.4「每个远端会话串行执行
/// 调用」。
pub struct Peer<T: Transport> {
    transport: T,
    next_id: u64,
    /// 握手拿到的东西，`initialize` 之后才有。
    server_info: Option<Value>,
}

impl<T: Transport> Peer<T> {
    pub fn new(transport: T) -> Self {
        Peer {
            transport,
            next_id: 1,
            server_info: None,
        }
    }

    /// 握手。必须在任何别的调用之前做一次。
    ///
    /// 版本对不上就直接失败：ccnm 的 bridge 在协议号不认识时会拒绝启动
    /// 并且 stdout 上一个字都不说（它的冻结契约就是这么定的），所以这里
    /// 也不做「降级试试看」。
    pub fn initialize(&mut self, client_name: &str, timeout: Duration) -> Result<Value, PeerError> {
        let result = self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": client_name, "version": env!("CARGO_PKG_VERSION") }
            }),
            timeout,
        )?;
        let version = result.get("protocolVersion").and_then(Value::as_str);
        match version {
            Some(PROTOCOL_VERSION) => {}
            Some(other) => {
                return Err(PeerError::Handshake(format!(
                    "it speaks {other}, this client speaks {PROTOCOL_VERSION}"
                )));
            }
            None => {
                return Err(PeerError::Handshake(
                    "the initialize result has no protocolVersion".into(),
                ));
            }
        }
        self.server_info = result.get("serverInfo").cloned();
        // 按协议，握手之后要发一条 initialized 通知；没有回复可等。
        self.notify("notifications/initialized", json!({}))?;
        Ok(result)
    }

    /// 握手时对面报的 `serverInfo`，没握过手就是 `None`。
    pub fn server_info(&self) -> Option<&Value> {
        self.server_info.as_ref()
    }

    /// 远端有哪些工具。**不分页**：ccnm 的 bridge 一次全给，七个工具而已。
    pub fn list_tools(&mut self, timeout: Duration) -> Result<Vec<Value>, PeerError> {
        let result = self.request("tools/list", json!({}), timeout)?;
        match result.get("tools") {
            Some(Value::Array(tools)) => Ok(tools.clone()),
            _ => Err(PeerError::Malformed {
                method: "tools/list".into(),
                detail: "no tools array".into(),
            }),
        }
    }

    /// 调一个远端工具。
    ///
    /// **原样返回远端的 result**，包括 `isError: true` 的那种——那是工具
    /// 自己说「这次没成」，不是协议错误，外层不能把它吞掉或改写成成功。
    pub fn call_tool(
        &mut self,
        name: &str,
        arguments: Value,
        timeout: Duration,
    ) -> Result<Value, PeerError> {
        self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
            timeout,
        )
    }

    /// 探活。**不能拿它当租期续期的凭据**——传输层还通不代表 Web 那头还有人。
    pub fn ping(&mut self, timeout: Duration) -> Result<(), PeerError> {
        self.request("ping", json!({}), timeout).map(|_| ())
    }

    /// 发一条请求并等回复。
    fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, PeerError> {
        let id = self.next_id;
        self.next_id += 1;
        let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.transport
            .send_line(&line.to_string())
            .map_err(PeerError::Write)?;

        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(PeerError::Timeout {
                    method: method.into(),
                    waited: timeout,
                });
            }
            let line = match self.transport.recv_line(left) {
                Ok(Some(line)) => line,
                Ok(None) => {
                    return Err(PeerError::Closed {
                        method: method.into(),
                        stderr: self.transport.stderr_tail(),
                    });
                }
                Err(RecvTimeoutError::Timeout) => {
                    return Err(PeerError::Timeout {
                        method: method.into(),
                        waited: timeout,
                    });
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(PeerError::Closed {
                        method: method.into(),
                        stderr: self.transport.stderr_tail(),
                    });
                }
            };
            let message: Value = match serde_json::from_str(&line) {
                Ok(value) => value,
                Err(e) => {
                    return Err(PeerError::Malformed {
                        method: method.into(),
                        detail: format!("{e}; the line was: {}", truncate(&line, 200)),
                    });
                }
            };
            // 服务端自己发的通知没有 id，跳过继续等自己的回复。
            match message.get("id") {
                Some(Value::Number(got)) if got.as_u64() == Some(id) => {}
                _ => continue,
            }
            if let Some(error) = message.get("error") {
                return Err(PeerError::Remote {
                    method: method.into(),
                    code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("no message")
                        .to_string(),
                });
            }
            return match message.get("result") {
                Some(result) => Ok(result.clone()),
                None => Err(PeerError::Malformed {
                    method: method.into(),
                    detail: "neither result nor error".into(),
                }),
            };
        }
    }

    /// 发一条不等回复的通知。
    fn notify(&mut self, method: &str, params: Value) -> Result<(), PeerError> {
        let line = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        self.transport
            .send_line(&line.to_string())
            .map_err(PeerError::Write)
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut cut = max;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &text[..cut])
}

/// 真实子进程的 stdio。
///
/// stdout 和 stderr 各有一个读线程：stdout 按行送进 channel，stderr 只留
/// 最后 [`STDERR_KEEP`] 字节。不用线程的话，读 stdout 就没法带超时，而
/// 一个黑洞掉的 SSH 会把 hub 的工作线程永远挂住。
pub struct ChildTransport {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    stderr: Arc<Mutex<VecDeque<u8>>>,
}

impl ChildTransport {
    /// 起一个子进程。**argv 直接传，不拼 shell**（RFC-0002 5.1）。
    pub fn spawn(program: &str, args: &[String]) -> Result<Self, PeerError> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| PeerError::Spawn {
                program: program.to_string(),
                source,
            })?;
        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");

        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || pump_lines(stdout, tx));
        let kept = Arc::new(Mutex::new(VecDeque::new()));
        let sink = kept.clone();
        std::thread::spawn(move || pump_stderr(stderr, sink));

        Ok(ChildTransport {
            child,
            stdin,
            lines,
            stderr: kept,
        })
    }

    /// 结束这个连接：关掉 stdin 让对面读到 EOF，再等它退出。
    ///
    /// **不是先 kill**。ccnm 的 `mcp-serve` 读到 EOF 会正常收尾并释放
    /// Runtime 那边的写锁；直接杀掉会留下 `held` 标记，要人工恢复（ccnm
    /// 的运维文档写过这个坑）。
    pub fn close(mut self, grace: Duration) -> std::io::Result<Option<std::process::ExitStatus>> {
        drop(self.stdin);
        let deadline = Instant::now() + grace;
        loop {
            match self.child.try_wait()? {
                Some(status) => return Ok(Some(status)),
                None if Instant::now() >= deadline => {
                    // 宽限期过了还不走，才动手。调用方拿到 None 就知道
                    // 「远端是不是收尾干净了」这件事是未知的。
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    return Ok(None);
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    }
}

impl Transport for ChildTransport {
    fn send_line(&mut self, line: &str) -> std::io::Result<()> {
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()
    }

    fn recv_line(&mut self, timeout: Duration) -> Result<Option<String>, RecvTimeoutError> {
        match self.lines.recv_timeout(timeout) {
            Ok(line) => Ok(Some(line)),
            Err(RecvTimeoutError::Disconnected) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn stderr_tail(&self) -> String {
        let kept = self
            .stderr
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        String::from_utf8_lossy(&kept.iter().copied().collect::<Vec<_>>())
            .trim()
            .to_string()
    }
}

fn pump_lines(stdout: ChildStdout, tx: mpsc::Sender<String>) {
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        match line {
            Ok(line) => {
                if tx.send(line).is_err() {
                    return;
                }
            }
            Err(_) => return,
        }
    }
}

fn pump_stderr(stderr: std::process::ChildStderr, sink: Arc<Mutex<VecDeque<u8>>>) {
    let mut reader = BufReader::new(stderr);
    let mut buf = [0u8; 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                let mut kept = sink.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                kept.extend(&buf[..n]);
                while kept.len() > STDERR_KEEP {
                    kept.pop_front();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 内存里的合成 peer：预先排好要回什么，不起进程。
    struct Scripted {
        sent: Vec<String>,
        replies: VecDeque<Reply>,
        stderr: String,
    }

    enum Reply {
        Line(String),
        /// 对面关了管道。
        Closed,
        /// 对面什么都不说，让调用方超时。
        Silent,
    }

    impl Scripted {
        fn new(replies: Vec<Reply>) -> Self {
            Scripted {
                sent: Vec::new(),
                replies: replies.into(),
                stderr: String::new(),
            }
        }
        fn with_stderr(mut self, text: &str) -> Self {
            self.stderr = text.into();
            self
        }
    }

    impl Transport for Scripted {
        fn send_line(&mut self, line: &str) -> std::io::Result<()> {
            self.sent.push(line.to_string());
            Ok(())
        }
        fn recv_line(&mut self, timeout: Duration) -> Result<Option<String>, RecvTimeoutError> {
            match self.replies.pop_front() {
                Some(Reply::Line(line)) => Ok(Some(line)),
                Some(Reply::Closed) | None => Ok(None),
                Some(Reply::Silent) => {
                    std::thread::sleep(timeout.min(Duration::from_millis(30)));
                    Err(RecvTimeoutError::Timeout)
                }
            }
        }
        fn stderr_tail(&self) -> String {
            self.stderr.clone()
        }
    }

    fn ok(id: u64, result: Value) -> Reply {
        Reply::Line(json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string())
    }

    const QUICK: Duration = Duration::from_secs(1);

    #[test]
    fn a_handshake_that_matches_is_accepted_and_followed_by_the_notification() {
        let mut peer = Peer::new(Scripted::new(vec![ok(
            1,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "ccnm", "version": "0.7.0" }
            }),
        )]));
        peer.initialize("gld-test", QUICK).expect("握手");
        assert_eq!(
            peer.server_info().and_then(|info| info.get("name")),
            Some(&json!("ccnm"))
        );
        let sent = &peer.transport.sent;
        assert_eq!(sent.len(), 2, "应该是 initialize 加一条通知：{sent:?}");
        assert!(sent[1].contains("notifications/initialized"), "{}", sent[1]);
    }

    /// 版本对不上就停，不试着降级——ccnm 的 bridge 在协议号不认识时
    /// stdout 一个字都不说，猜不出它能接受什么。
    #[test]
    fn a_handshake_with_another_protocol_version_is_refused() {
        let mut peer = Peer::new(Scripted::new(vec![ok(
            1,
            json!({ "protocolVersion": "1999-01-01" }),
        )]));
        let err = peer.initialize("gld-test", QUICK).expect_err("应该拒绝");
        assert!(matches!(err, PeerError::Handshake(_)), "{err}");
        assert!(err.to_string().contains("1999-01-01"), "{err}");
    }

    #[test]
    fn a_tool_result_comes_back_as_it_is_including_an_error_flag() {
        let mut peer = Peer::new(Scripted::new(vec![ok(
            1,
            json!({ "content": [{ "type": "text", "text": "nope" }], "isError": true }),
        )]));
        let result = peer
            .call_tool("read_file", json!({ "path": "a.txt" }), QUICK)
            .expect("调用本身成功");
        assert_eq!(result["isError"], json!(true), "isError 不能被吞掉");
        assert_eq!(result["content"][0]["text"], json!("nope"));
    }

    /// 服务端主动发的通知没有 id，不能被当成自己那条请求的回复。
    #[test]
    fn a_notification_in_between_does_not_get_mistaken_for_the_reply() {
        let mut peer = Peer::new(Scripted::new(vec![
            Reply::Line(
                json!({ "jsonrpc": "2.0", "method": "notifications/message", "params": {} })
                    .to_string(),
            ),
            ok(1, json!({ "tools": [{ "name": "read_file" }] })),
        ]));
        let tools = peer.list_tools(QUICK).expect("列工具");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], json!("read_file"));
    }

    /// 别人那条请求的回复也不能当成自己的。
    #[test]
    fn a_reply_with_another_id_is_skipped() {
        let mut peer = Peer::new(Scripted::new(vec![
            ok(99, json!({ "tools": [] })),
            ok(1, json!({ "tools": [{ "name": "list_files" }] })),
        ]));
        let tools = peer.list_tools(QUICK).expect("列工具");
        assert_eq!(tools[0]["name"], json!("list_files"));
    }

    #[test]
    fn a_remote_error_says_the_code_and_the_message() {
        let mut peer = Peer::new(Scripted::new(vec![Reply::Line(
            json!({ "jsonrpc": "2.0", "id": 1, "error": { "code": -32601, "message": "Method not found" } })
                .to_string(),
        )]));
        let err = peer.ping(QUICK).expect_err("应该报错");
        match err {
            PeerError::Remote {
                code, ref message, ..
            } => {
                assert_eq!(code, -32601);
                assert_eq!(message, "Method not found");
            }
            other => panic!("错误类型不对：{other}"),
        }
    }

    /// 对面挂了，而且 stderr 上有话——那句话必须带到调用方面前。
    /// ccnm 的 `CCNM_E_*` 诊断只在 stderr 上，Host 通常把它丢了。
    #[test]
    fn a_closed_pipe_carries_what_the_bridge_said_on_stderr() {
        let transport = Scripted::new(vec![Reply::Closed])
            .with_stderr("CCNM_E_POLICY: workspace x is not available to external MCP");
        let mut peer = Peer::new(transport);
        let err = peer.initialize("gld-test", QUICK).expect_err("应该报错");
        assert!(err.to_string().contains("CCNM_E_POLICY"), "{err}");
    }

    /// SSH 可以黑洞掉不报错。读必须能超时放弃，否则工作线程就挂在那儿。
    #[test]
    fn a_silent_bridge_times_out_instead_of_hanging() {
        let mut peer = Peer::new(Scripted::new(vec![Reply::Silent]));
        let err = peer.ping(Duration::from_millis(50)).expect_err("应该超时");
        assert!(matches!(err, PeerError::Timeout { .. }), "{err}");
    }

    #[test]
    fn a_line_that_is_not_json_is_reported_with_what_was_on_it() {
        let mut peer = Peer::new(Scripted::new(vec![Reply::Line("not json at all".into())]));
        let err = peer.ping(QUICK).expect_err("应该报错");
        assert!(err.to_string().contains("not json at all"), "{err}");
    }

    #[test]
    fn a_program_that_does_not_exist_reports_which_one() {
        let Err(err) = ChildTransport::spawn("gld-no-such-program-here", &[]) else {
            panic!("这个程序不该存在，spawn 应该失败");
        };
        assert!(matches!(err, PeerError::Spawn { .. }), "{err}");
        assert!(
            err.to_string().contains("gld-no-such-program-here"),
            "{err}"
        );
    }

    /// 真进程走一遍：起 `cat`，它把收到的行原样回显，正好当一个只会
    /// 复读的 peer。证明 argv 启动、行读写和关闭是接得上的。
    #[cfg(unix)]
    #[test]
    fn a_real_child_process_round_trips_a_line() {
        let mut transport = ChildTransport::spawn("cat", &[]).expect("起 cat");
        transport
            .send_line("{\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{}}")
            .expect("写");
        let line = transport
            .recv_line(Duration::from_secs(5))
            .expect("读")
            .expect("有一行");
        assert!(line.contains("\"id\":7"), "{line}");
        let status = transport.close(Duration::from_secs(5)).expect("关");
        assert!(status.is_some(), "cat 读到 EOF 应该自己退出");
    }
}

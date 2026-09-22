//! Streamable HTTP：MCP 2025-03-26 起的标准远端传输。
//!
//! 每条消息一次 POST。回复要么是一个 JSON，要么是一段 SSE（`text/event-stream`，
//! 实测 deepwiki、exa 都是这种），在 SSE 里等到自己那条回复就停。握手时 server
//! 给的 `Mcp-Session-Id` 之后每次都带上，协议版本放在 `MCP-Protocol-Version`
//! 头里。
//!
//! 三件实测撞到的事：
//!
//! - **不带 User-Agent 会被 Cloudflare 拦掉**：exa 的地址对没有 UA 的请求回
//!   403（Cloudflare 1010），带上任何一个就是 200。这里总是带 `gld/<版本>`，
//!   配置里自己写了就用配置的。
//! - 401 是要登录。Claude Code 对这种 server 走 OAuth，令牌存在它自己那里，
//!   gld 拿不到也不替它登录，报 [`Error::NeedsLogin`]。
//! - 回复大小有上限（[`MAX_MESSAGE_BYTES`]），跟 stdio 同一个数。
//!
//! 老的 HTTP+SSE 传输（`type: "sse"`）不支持：那是另一套握手（先 GET 一条
//! 长连接拿 POST 地址），官方已经废弃。

use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE, USER_AGENT};
use serde_json::Value;

use toexec_mcp::installed::{display_url, is_loopback_url};
use toexec_mcp::transport::{Recv, Transport, MAX_MESSAGE_BYTES};
use toexec_mcp::Error;

/// 连远端地址走不走代理，照 gld 的全局出站代理设置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Proxy {
    None,
    /// 看环境变量 `HTTP(S)_PROXY`（gld 的默认）。
    System,
    Manual(String),
}

impl Proxy {
    pub fn from_settings(proxy: &crate::settings::ProxyConfig) -> Proxy {
        match proxy.mode.as_str() {
            "manual" if !proxy.url.trim().is_empty() => Proxy::Manual(proxy.url.trim().to_string()),
            "none" => Proxy::None,
            _ => Proxy::System,
        }
    }
}

pub struct Http {
    url: String,
    headers: HeaderMap,
    client: reqwest::Client,
    session: Option<String>,
    protocol: Option<String>,
    /// 已经收到、还没被取走的消息。
    inbox: VecDeque<String>,
    closed: bool,
}

impl Http {
    /// `proxy` 是 gld 的全局出站代理（`gld cfg proxy`）。本机地址一律不走代理：
    /// 开着 `HTTP_PROXY` 时连 `127.0.0.1` 也会被送进代理，回一个 502。
    pub fn new(
        url: &str,
        headers: &BTreeMap<String, String>,
        proxy: &Proxy,
    ) -> Result<Http, Error> {
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(Error::Start(format!(
                "the url does not start with http:// or https://: {}",
                display_url(url)
            )));
        }
        let mut map = HeaderMap::new();
        map.insert(
            USER_AGENT,
            HeaderValue::from_static(concat!("gld/", env!("CARGO_PKG_VERSION"))),
        );
        for (name, value) in headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| Error::Start(format!("header {name} is not a valid HTTP header")))?;
            let value = HeaderValue::from_str(value).map_err(|_| {
                Error::Start(format!(
                    "the value of header {name} cannot go in an HTTP header"
                ))
            })?;
            map.insert(name, value);
        }
        let mut builder = reqwest::Client::builder();
        let loopback = is_loopback_url(url);
        builder = match proxy {
            _ if loopback => builder.no_proxy(),
            Proxy::None => builder.no_proxy(),
            // reqwest 默认就读 HTTP(S)_PROXY / NO_PROXY。
            Proxy::System => builder,
            Proxy::Manual(address) => {
                builder.proxy(reqwest::Proxy::all(address).map_err(|error| {
                    Error::Start(format!("gld's proxy setting is not usable: {error}"))
                })?)
            }
        };
        let client = builder
            .build()
            .map_err(|error| Error::Start(format!("cannot set up HTTP: {error}")))?;
        Ok(Http {
            url: url.to_string(),
            headers: map,
            client,
            session: None,
            protocol: None,
            inbox: VecDeque::new(),
            closed: false,
        })
    }

    async fn post(&mut self, line: &str, timeout: Duration) -> Result<(), Error> {
        let waiting_for = serde_json::from_str::<Value>(line)
            .ok()
            .filter(|message| message.get("method").is_some())
            .and_then(|message| message.get("id").cloned());
        let mut request = self
            .client
            .post(&self.url)
            .timeout(timeout)
            .headers(self.headers.clone())
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json, text/event-stream")
            .body(line.to_string());
        if let Some(session) = &self.session {
            request = request.header("Mcp-Session-Id", session);
        }
        if let Some(protocol) = &self.protocol {
            request = request.header("MCP-Protocol-Version", protocol);
        }
        let mut response = request
            .send()
            .await
            .map_err(|error| failed(error, timeout))?;
        let status = response.status();
        if let Some(session) = response
            .headers()
            .get("Mcp-Session-Id")
            .and_then(|value| value.to_str().ok())
        {
            self.session = Some(session.to_string());
        }
        if status.as_u16() == 401 {
            return Err(Error::NeedsLogin { status: 401 });
        }
        // 会话过期：协议要求客户端重新握手。报成"断了"，连接池会丢掉这条、
        // 下次重开。
        if status.as_u16() == 404 && self.session.is_some() {
            return Err(Error::Closed {
                during: String::new(),
                said: "the server no longer knows this session (HTTP 404)".into(),
            });
        }
        if !status.is_success() {
            let body = read_capped(&mut response, 400).await.unwrap_or_default();
            return Err(Error::Http {
                status: status.as_u16(),
                said: String::from_utf8_lossy(&body).trim().to_string(),
            });
        }
        let is_stream = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"));
        if is_stream {
            self.read_stream(&mut response, waiting_for.as_ref(), timeout)
                .await
        } else {
            let body = read_capped(&mut response, MAX_MESSAGE_BYTES)
                .await
                .map_err(|error| failed(error, timeout))?;
            if body.len() > MAX_MESSAGE_BYTES {
                return Err(too_long(body.len()));
            }
            let text = String::from_utf8_lossy(&body);
            if !text.trim().is_empty() {
                self.take(text.trim());
            }
            Ok(())
        }
    }

    /// 读 SSE，一个事件一条消息，等到 `waiting_for` 那条就停。
    async fn read_stream(
        &mut self,
        response: &mut reqwest::Response,
        waiting_for: Option<&Value>,
        timeout: Duration,
    ) -> Result<(), Error> {
        let mut buffer: Vec<u8> = Vec::new();
        let mut total = 0usize;
        loop {
            let chunk = response
                .chunk()
                .await
                .map_err(|error| failed(error, timeout))?;
            let Some(chunk) = chunk else {
                // 流结束了。最后一个事件可能没有空行收尾。
                if let Some(data) = event_data(&buffer) {
                    self.take(&data);
                }
                return Ok(());
            };
            total += chunk.len();
            if total > MAX_MESSAGE_BYTES {
                return Err(too_long(total));
            }
            buffer.extend_from_slice(&chunk);
            while let Some((end, skip)) = event_end(&buffer) {
                let event: Vec<u8> = buffer.drain(..end + skip).take(end).collect();
                let Some(data) = event_data(&event) else {
                    continue;
                };
                let done = waiting_for.is_some_and(|id| answers(&data, id));
                self.take(&data);
                if done {
                    return Ok(());
                }
            }
        }
    }

    /// 收下一条（或一批）消息。握手的回复里有协议版本，记下来以后放进头里。
    fn take(&mut self, text: &str) {
        let Ok(parsed) = serde_json::from_str::<Value>(text) else {
            return;
        };
        let messages = match parsed {
            Value::Array(batch) => batch,
            single => vec![single],
        };
        for message in messages {
            if let Some(version) = message
                .get("result")
                .and_then(|result| result.get("protocolVersion"))
                .and_then(Value::as_str)
            {
                self.protocol = Some(version.to_string());
            }
            self.inbox.push_back(message.to_string());
        }
    }
}

impl Transport for Http {
    fn send(&mut self, line: &str, timeout: Duration) -> Result<(), Error> {
        if self.closed {
            return Err(Error::Closed {
                during: String::new(),
                said: String::new(),
            });
        }
        crate::async_rt::block_on(self.post(line, timeout))
    }

    /// 回复在 `send` 里就收齐了，这里只是取。没有就是 server 回了个空。
    fn recv(&mut self, _timeout: Duration) -> Recv {
        match self.inbox.pop_front() {
            Some(line) => Recv::Line(line),
            None => Recv::Closed {
                said: "the HTTP response ended without a reply".into(),
            },
        }
    }

    /// 有会话的话告诉 server 这个会话不用了（协议里是可选的，失败了不管）。
    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        let Some(session) = self.session.take() else {
            return;
        };
        let request = self
            .client
            .delete(&self.url)
            .timeout(Duration::from_secs(2))
            .headers(self.headers.clone())
            .header("Mcp-Session-Id", session);
        // `send()` 要在运行时里调：它当场就建超时计时器。
        let _ = crate::async_rt::block_on(async move { request.send().await });
    }
}

impl Drop for Http {
    fn drop(&mut self) {
        self.close();
    }
}

fn failed(error: reqwest::Error, timeout: Duration) -> Error {
    if error.is_timeout() {
        Error::Timeout {
            during: String::new(),
            waited: timeout,
        }
    } else if error.is_connect() {
        Error::Start(format!("cannot reach the server: {error}"))
    } else {
        Error::Closed {
            during: String::new(),
            said: error.to_string(),
        }
    }
}

fn too_long(bytes: usize) -> Error {
    Error::Protocol(format!(
        "the server sent more than {MAX_MESSAGE_BYTES} bytes in one reply ({bytes} so far)"
    ))
}

/// 最多读 `max + 1` 字节，多一个字节就知道超了。
async fn read_capped(
    response: &mut reqwest::Response,
    max: usize,
) -> Result<Vec<u8>, reqwest::Error> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        body.extend_from_slice(&chunk);
        if body.len() > max {
            body.truncate(max + 1);
            break;
        }
    }
    Ok(body)
}

/// 缓冲里第一个事件在哪结束：空行（`\n\n` 或 `\r\n\r\n`）。返回事件长度和
/// 分隔符长度。
fn event_end(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer
        .windows(2)
        .position(|w| w == b"\n\n")
        .map(|at| (at, 2));
    let crlf = buffer
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|at| (at, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (one, other) => one.or(other),
    }
}

/// 一个事件里所有 `data:` 行拼起来（SSE 规定多行 data 用换行连接）。
/// 没有 data（注释、只有 `event:` 的心跳）返回 `None`。
fn event_data(event: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(event);
    let lines: Vec<&str> = text
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|data| data.strip_prefix(' ').unwrap_or(data))
        .collect();
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

/// 这段数据是不是 `id` 那条请求的回复（批量回复里有它也算）。
fn answers(data: &str, id: &Value) -> bool {
    let is_reply =
        |message: &Value| message.get("id") == Some(id) && message.get("method").is_none();
    match serde_json::from_str::<Value>(data) {
        Ok(Value::Array(batch)) => batch.iter().any(is_reply),
        Ok(message) => is_reply(&message),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap as Headers, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::post;
    use axum::Router;
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use toexec_mcp::Client;

    /// 一次请求带了哪几个头：User-Agent、会话号、x-api-key。
    struct Heard {
        agent: Option<String>,
        session: Option<String>,
        key: Option<String>,
    }

    #[derive(Default)]
    struct Seen {
        requests: Vec<Heard>,
    }

    /// 起一个本机的 streamable HTTP server：握手发会话号，之后要求带上；
    /// `tools/list` 用 SSE 回，前面垫一个心跳和一条通知。
    fn serve(require_login: bool) -> (String, Arc<Mutex<Seen>>) {
        let seen = Arc::new(Mutex::new(Seen::default()));
        let state = seen.clone();
        let app = Router::new().route(
            "/mcp",
            post(move |headers: Headers, body: String| {
                let state = state.clone();
                async move {
                    let message: Value = serde_json::from_str(&body).unwrap();
                    let header = |name: &str| {
                        headers
                            .get(name)
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_string)
                    };
                    state.lock().unwrap().requests.push(Heard {
                        agent: header("user-agent"),
                        session: header("mcp-session-id"),
                        key: header("x-api-key"),
                    });
                    if require_login {
                        return StatusCode::UNAUTHORIZED.into_response();
                    }
                    let method = message["method"].as_str().unwrap_or("");
                    let reply = |result: Value| {
                        json!({ "jsonrpc": "2.0", "id": message["id"], "result": result })
                    };
                    match method {
                        "initialize" => {
                            let mut response: Response = axum::Json(reply(
                                json!({ "protocolVersion": "2025-06-18", "serverInfo": { "name": "web" } }),
                            ))
                            .into_response();
                            response
                                .headers_mut()
                                .insert("Mcp-Session-Id", "s-123".parse().unwrap());
                            response
                        }
                        _ if header("mcp-session-id").as_deref() != Some("s-123") => {
                            StatusCode::BAD_REQUEST.into_response()
                        }
                        "notifications/initialized" => StatusCode::ACCEPTED.into_response(),
                        "tools/list" => {
                            let events = format!(
                                ": keep-alive\r\n\r\nevent: message\r\ndata: {}\r\n\r\ndata: {}\r\n\r\n",
                                json!({ "jsonrpc": "2.0", "method": "notifications/progress", "params": {} }),
                                reply(json!({ "tools": [{ "name": "search" }] }))
                            );
                            ([(CONTENT_TYPE, "text/event-stream")], events).into_response()
                        }
                        _ => StatusCode::NOT_FOUND.into_response(),
                    }
                }
            }),
        );
        let listener =
            crate::async_rt::block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).expect("bind");
        let address = listener.local_addr().unwrap();
        crate::async_rt::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}/mcp"), seen)
    }

    #[test]
    fn a_session_is_carried_and_an_sse_reply_is_picked_out() {
        let (url, seen) = serve(false);
        let headers = BTreeMap::from([("x-api-key".to_string(), "k1".to_string())]);
        let transport = Http::new(&url, &headers, &Proxy::System).unwrap();
        let mut client = Client::connect(
            Box::new(transport),
            ("gld-test", "0"),
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(client.server_name.as_deref(), Some("web"));
        let tools = client.list_tools(Duration::from_secs(5)).unwrap();
        assert_eq!(tools, [json!({ "name": "search" })]);

        let seen = seen.lock().unwrap();
        assert_eq!(seen.requests.len(), 3);
        for heard in &seen.requests {
            let agent = heard.agent.as_deref().unwrap_or("");
            assert!(agent.starts_with("gld/"), "{agent}");
            assert_eq!(heard.key.as_deref(), Some("k1"));
        }
        assert_eq!(seen.requests[0].session, None, "握手时还没有会话号");
        assert_eq!(seen.requests[2].session.as_deref(), Some("s-123"));
    }

    #[test]
    fn a_login_wall_is_named_as_such() {
        let (url, _) = serve(true);
        let transport = Http::new(&url, &BTreeMap::new(), &Proxy::System).unwrap();
        let Err(error) = Client::connect(
            Box::new(transport),
            ("gld-test", "0"),
            Duration::from_secs(5),
        ) else {
            panic!("should need login");
        };
        assert!(
            matches!(error, Error::NeedsLogin { status: 401 }),
            "{error}"
        );
    }

    #[test]
    fn nobody_listening_is_a_start_failure() {
        // 环境里开着 HTTP_PROXY 也不能把本机地址送进代理（那样回的是代理的 502）。
        let transport =
            Http::new("http://127.0.0.1:9/mcp", &BTreeMap::new(), &Proxy::System).unwrap();
        let Err(error) = Client::connect(
            Box::new(transport),
            ("gld-test", "0"),
            Duration::from_secs(5),
        ) else {
            panic!("nothing listens on port 9");
        };
        assert!(matches!(error, Error::Start(_)), "{error}");
    }

    #[test]
    fn events_split_on_either_line_ending_and_join_their_data_lines() {
        let buffer = b"data: {\"a\":\ndata: 1}\r\n\r\nrest";
        let (end, skip) = event_end(buffer).unwrap();
        assert_eq!(skip, 4);
        assert_eq!(event_data(&buffer[..end]).as_deref(), Some("{\"a\":\n1}"));
        assert_eq!(event_data(b": comment"), None);
    }
}

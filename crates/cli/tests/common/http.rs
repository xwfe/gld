//! 直接用 TCP 说 HTTP 的小客户端。
//!
//! 为什么不用现成的 HTTP 库：沙箱和公司网络里普遍设了 `HTTP_PROXY`，
//! 大部分客户端会连 127.0.0.1 的请求也扔给代理，于是测试变成随机 502，
//! 看起来像服务没起来。自己发字节最省心，顺带还能看到原始响应头。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Reply {
    /// 取响应头。名字按小写比对——HTTP 头大小写不敏感，axum 发出来的是
    /// 小写，按 `Location` 去找会一直取不到，看着像服务端没发这个头。
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.as_str())
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|err| panic!("响应不是 JSON（{err}）：{}", self.body))
    }
}

pub fn request(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Reply {
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\
         Content-Length: {}\r\n",
        body.len()
    );
    for (key, value) in headers {
        head.push_str(&format!("{key}: {value}\r\n"));
    }
    head.push_str("\r\n");

    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    // 头和体一次写完。分两次 write 的话，服务端可能在只读了头之后就决定
    // 拒绝（401 直接返回，根本不读 body），随后带着"还没读走的 body"关闭连接——
    // 这种关闭内核发的是 RST 而不是 FIN，客户端读响应时拿到 ECONNRESET。
    // 这就是这套测试里那个只在 POST 上偶发的 connection reset。
    let mut request = head.into_bytes();
    request.extend_from_slice(body.as_bytes());
    stream.write_all(&request).unwrap();

    let mut raw = Vec::new();
    if let Err(error) = stream.read_to_end(&mut raw) {
        // 已经读到完整响应（含空行分隔的头）时，RST 只是服务端没读完请求体就
        // 关闭连接的副作用，响应本身是有效的——真实 HTTP 客户端也这么处理。
        let text = String::from_utf8_lossy(&raw);
        if !text.contains("\r\n\r\n") {
            panic!("read response 失败：{error}（已收到 {} 字节）", raw.len());
        }
    }
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("响应不完整：{text}"));

    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("状态行读不出来：{head}"));
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();

    let chunked = headers
        .iter()
        .any(|(key, value)| key == "transfer-encoding" && value.contains("chunked"));
    let body = if chunked {
        dechunk(body)
    } else {
        body.to_string()
    };
    Reply {
        status,
        headers,
        body,
    }
}

/// 解 chunked 编码。不解的话 JSON 里会混进十六进制长度前缀，
/// 报出来像是服务端返回了非法 JSON。
fn dechunk(body: &str) -> String {
    let mut rest = body;
    let mut out = String::new();
    while let Some((size_line, tail)) = rest.split_once("\r\n") {
        let size = usize::from_str_radix(size_line.trim().split(';').next().unwrap_or("0"), 16)
            .unwrap_or(0);
        if size == 0 || tail.len() < size {
            out.push_str(&tail[..size.min(tail.len())]);
            break;
        }
        out.push_str(&tail[..size]);
        rest = tail[size..].strip_prefix("\r\n").unwrap_or("");
    }
    out
}

pub fn get(port: u16, path: &str) -> Reply {
    request(port, "GET", path, &[], "")
}

pub fn post_json(port: u16, path: &str, body: &str, bearer: Option<&str>) -> Reply {
    let auth = bearer.map(|token| format!("Bearer {token}"));
    let mut headers = vec![("Content-Type", "application/json")];
    if let Some(value) = auth.as_deref() {
        headers.push(("Authorization", value));
    }
    request(port, "POST", path, &headers, body)
}

pub fn post_form(port: u16, path: &str, fields: &[(&str, &str)]) -> Reply {
    let body = fields
        .iter()
        .map(|(key, value)| format!("{}={}", urlencode(key), urlencode(value)))
        .collect::<Vec<_>>()
        .join("&");
    request(
        port,
        "POST",
        path,
        &[("Content-Type", "application/x-www-form-urlencoded")],
        &body,
    )
}

pub fn urlencode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// 从 URL 的 query 里取一个参数值。
pub fn query_param(url: &str, key: &str) -> String {
    url.split_once('?')
        .map(|(_, query)| query)
        .unwrap_or("")
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(name, _)| *name == key)
        .map(|(_, value)| value.to_string())
        .unwrap_or_default()
}

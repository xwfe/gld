//! 配对和长连接共用一份代理判断。以前配对走 reqwest 的判断、长连接直连，只能经代理出网的机器上
//! `gld join` 成功、`gld node run` 一直超时。这里只认环境变量和 HTTP CONNECT：隧道里 TLS 仍是端到端的，
//! 代理看不到节点凭据；SOCKS、HTTPS 代理和 PAC 明确报错，不落成笼统的连接超时。

use std::fmt;
use std::net::IpAddr;

use base64::Engine;
use percent_encoding::percent_decode_str;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[cfg(test)]
#[path = "proxy_tests.rs"]
mod tests;

const MAX_RESPONSE_HEADER: usize = 8 * 1024;
const PROXY_VARIABLES: [&str; 4] = ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"];
const BYPASS_VARIABLES: [&str; 2] = ["NO_PROXY", "no_proxy"];

/// 拿到这个结果说明：环境变量要求这条连接走代理，且方式受支持。
#[derive(Clone)]
pub struct Proxy {
    address: String,
    credentials: Option<(String, String)>,
}

impl fmt::Debug for Proxy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Proxy")
            .field("address", &self.address)
            .field(
                "credentials",
                &self.credentials.as_ref().map(|_| "[redacted]"),
            )
            .finish()
    }
}

#[derive(Debug)]
pub enum ProxyError {
    /// 代理连不上或中途断开，等一会儿可能自己好。
    Unreachable(String),
    /// 代理明确拒绝了。5xx 算 Unreachable，上游可能只是暂时不通。
    Rejected(String),
}

impl fmt::Display for ProxyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable(message) | Self::Rejected(message) => f.write_str(message),
        }
    }
}

/// 这条连接要不要走代理。错误信息只含变量名，代理地址里可能有密码。
pub fn for_hub(hub: &reqwest::Url) -> Result<Option<Proxy>, String> {
    resolve(hub, |name| std::env::var(name).ok())
}

fn resolve(
    hub: &reqwest::Url,
    env: impl Fn(&str) -> Option<String>,
) -> Result<Option<Proxy>, String> {
    let first = |names: &[&'static str]| {
        names.iter().find_map(|name| {
            env(name)
                .filter(|value| !value.trim().is_empty())
                .map(|value| (*name, value))
        })
    };
    let Some(host) = hub.host_str().map(str::to_ascii_lowercase) else {
        return Ok(None);
    };
    // 明文 HTTP 只允许回环地址，不能让环境里的代理把凭据带出这台机器。
    if host_ip(&host).is_some_and(|ip| ip.is_loopback()) || hub.scheme() == "http" {
        return Ok(None);
    }
    let Some((variable, value)) = first(&PROXY_VARIABLES) else {
        return Ok(None);
    };
    if first(&BYPASS_VARIABLES).is_some_and(|(_, list)| bypassed(&host, &list)) {
        return Ok(None);
    }
    parse(variable, value.trim()).map(Some)
}

fn parse(variable: &str, value: &str) -> Result<Proxy, String> {
    let unusable =
        || format!("{variable} is not a usable proxy address; expected http://host:port");
    // 和 curl 一样，没写协议就按 HTTP 代理算。
    let url = if value.contains("://") {
        reqwest::Url::parse(value)
    } else {
        reqwest::Url::parse(&format!("http://{value}"))
    }
    .map_err(|_| unusable())?;
    if url.scheme() != "http" {
        return Err(format!(
            "{variable} uses {}://, but nodes only support HTTP CONNECT proxies (http://host:port); \
             SOCKS and HTTPS proxies are not supported",
            url.scheme()
        ));
    }
    let host = url.host_str().ok_or_else(unusable)?;
    let port = url.port_or_known_default().ok_or_else(unusable)?;
    let decode = |text: &str| percent_decode_str(text).decode_utf8_lossy().into_owned();
    let credentials = (!url.username().is_empty()).then(|| {
        (
            decode(url.username()),
            decode(url.password().unwrap_or_default()),
        )
    });
    Ok(Proxy {
        address: format!("{host}:{port}"),
        credentials,
    })
}

fn host_ip(host: &str) -> Option<IpAddr> {
    host.trim_matches(['[', ']']).parse().ok()
}

/// `*`、域名（带不带开头的点都算后缀）、IP、CIDR；逗号分隔。写法和 curl 的常见用法一致。
fn bypassed(host: &str, list: &str) -> bool {
    list.split(',')
        .map(|entry| entry.trim().to_ascii_lowercase())
        .filter(|entry| !entry.is_empty())
        .any(|entry| {
            if entry == "*" {
                return true;
            }
            if let Some(ip) = host_ip(host) {
                return in_cidr(ip, &entry) || host_ip(&entry) == Some(ip);
            }
            let suffix = entry.trim_start_matches("*.").trim_start_matches('.');
            host == suffix || host.ends_with(&format!(".{suffix}"))
        })
}

fn in_cidr(ip: IpAddr, entry: &str) -> bool {
    let Some((network, bits)) = entry.split_once('/') else {
        return false;
    };
    let (Ok(network), Ok(bits)) = (network.parse::<IpAddr>(), bits.parse::<u32>()) else {
        return false;
    };
    let masked = |value: u128, width: u32| {
        (bits <= width).then(|| {
            let shift = width - bits;
            value.checked_shr(shift).unwrap_or(0)
        })
    };
    match (ip, network) {
        (IpAddr::V4(a), IpAddr::V4(b)) => masked(u32::from(a).into(), 32)
            .is_some_and(|x| Some(x) == masked(u32::from(b).into(), 32)),
        (IpAddr::V6(a), IpAddr::V6(b)) => {
            masked(u128::from(a), 128).is_some_and(|x| Some(x) == masked(u128::from(b), 128))
        }
        _ => false,
    }
}

impl Proxy {
    /// 给人看的地址，不含账号密码。
    pub fn address(&self) -> &str {
        &self.address
    }

    /// 配对用的 HTTP 客户端走同一个代理。
    pub fn for_reqwest(&self) -> Result<reqwest::Proxy, String> {
        let proxy = reqwest::Proxy::all(format!("http://{}", self.address))
            .map_err(|_| "Cannot configure the node proxy".to_string())?;
        Ok(match &self.credentials {
            Some((user, password)) => proxy.basic_auth(user, password),
            None => proxy,
        })
    }

    /// 建隧道。之后这条 TCP 上的 TLS 和 WebSocket 由调用方照常握手。
    pub async fn connect(&self, host: &str, port: u16) -> Result<TcpStream, ProxyError> {
        let mut stream = TcpStream::connect(&self.address).await.map_err(|_| {
            ProxyError::Unreachable(format!("cannot reach node proxy {}", self.address))
        })?;
        let mut request = format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n");
        if let Some((user, password)) = &self.credentials {
            let token =
                base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
            request.push_str(&format!("Proxy-Authorization: Basic {token}\r\n"));
        }
        request.push_str("\r\n");
        let lost = || {
            ProxyError::Unreachable(format!("node proxy {} closed the connection", self.address))
        };
        stream
            .write_all(request.as_bytes())
            .await
            .map_err(|_| lost())?;
        let status = self.read_status(&mut stream).await?;
        match status {
            200..=299 => Ok(stream),
            407 => Err(ProxyError::Rejected(format!(
                "node proxy {} requires authentication or rejected the credentials in its proxy variable (HTTP 407)",
                self.address
            ))),
            400..=499 => Err(ProxyError::Rejected(format!(
                "node proxy {} refused the connection to the Hub (HTTP {status})",
                self.address
            ))),
            _ => Err(ProxyError::Unreachable(format!(
                "node proxy {} could not reach the Hub (HTTP {status})",
                self.address
            ))),
        }
    }

    /// 一个字节一个字节读：读多了会吃掉隧道里的第一段数据，而响应头很短，几百次读没有代价。
    async fn read_status(&self, stream: &mut TcpStream) -> Result<u16, ProxyError> {
        let lost = || {
            ProxyError::Unreachable(format!("node proxy {} closed the connection", self.address))
        };
        let mut header = Vec::with_capacity(256);
        while !header.ends_with(b"\r\n\r\n") && !header.ends_with(b"\n\n") {
            if header.len() >= MAX_RESPONSE_HEADER {
                return Err(ProxyError::Rejected(format!(
                    "node proxy {} sent an oversized response",
                    self.address
                )));
            }
            let byte = stream.read_u8().await.map_err(|_| lost())?;
            header.push(byte);
        }
        // 只解析状态码：后面的头和原因短语里可能有非 UTF-8 字节，不影响判断。
        header
            .split(|byte| byte.is_ascii_whitespace())
            .filter(|part| !part.is_empty())
            .nth(1)
            .and_then(|status| std::str::from_utf8(status).ok())
            .and_then(|status| status.parse().ok())
            .ok_or_else(|| {
                ProxyError::Rejected(format!(
                    "node proxy {} sent an invalid response",
                    self.address
                ))
            })
    }
}

use std::collections::HashMap;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;

fn hub() -> reqwest::Url {
    reqwest::Url::parse("https://hub.example.com/hub").unwrap()
}

fn lookup<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    let map: HashMap<&str, &str> = pairs.iter().copied().collect();
    move |name| map.get(name).map(|value| value.to_string())
}

#[test]
fn no_variable_means_a_direct_connection() {
    assert!(resolve(&hub(), lookup(&[])).unwrap().is_none());
    assert!(resolve(&hub(), lookup(&[("HTTPS_PROXY", "  ")]))
        .unwrap()
        .is_none());
}

#[test]
fn proxy_variables_apply_in_the_documented_order_and_a_bare_address_is_http() {
    let proxy = resolve(
        &hub(),
        lookup(&[
            ("https_proxy", "proxy-b:2"),
            ("HTTPS_PROXY", "http://proxy-a:1"),
            ("ALL_PROXY", "proxy-c:3"),
        ]),
    )
    .unwrap()
    .unwrap();
    assert_eq!(proxy.address(), "proxy-a:1");
    let proxy = resolve(&hub(), lookup(&[("all_proxy", "proxy-c:3")]))
        .unwrap()
        .unwrap();
    assert_eq!(proxy.address(), "proxy-c:3");
    let proxy = resolve(&hub(), lookup(&[("HTTPS_PROXY", "http://proxy-a")]))
        .unwrap()
        .unwrap();
    assert_eq!(proxy.address(), "proxy-a:80");
}

#[test]
fn loopback_and_plain_http_hubs_never_use_a_proxy() {
    for url in [
        "https://127.0.0.1:8443/",
        "https://[::1]:8443/",
        "http://127.0.0.1:8080/",
    ] {
        let url = reqwest::Url::parse(url).unwrap();
        assert!(resolve(&url, lookup(&[("HTTPS_PROXY", "http://proxy:1")]))
            .unwrap()
            .is_none());
    }
}

#[test]
fn no_proxy_matches_wildcards_suffixes_and_addresses() {
    let bypass = |host: &str, list: &str| {
        let url = reqwest::Url::parse(&format!("https://{host}/")).unwrap();
        resolve(
            &url,
            lookup(&[("HTTPS_PROXY", "http://proxy:1"), ("NO_PROXY", list)]),
        )
        .unwrap()
        .is_none()
    };
    assert!(bypass("hub.example.com", "*"));
    assert!(bypass("hub.example.com", "other.test, example.com"));
    assert!(bypass("hub.example.com", ".example.com"));
    assert!(bypass("HUB.example.com", "*.EXAMPLE.com"));
    assert!(bypass("example.com", "example.com"));
    assert!(bypass("10.1.2.3", "10.0.0.0/8"));
    assert!(bypass("10.1.2.3", "10.1.2.3"));
    assert!(bypass("[fd00::1]", "fd00::/8"));
    assert!(bypass("10.1.2.3", "0.0.0.0/0"));

    assert!(!bypass("hub.example.com", "ample.com"));
    assert!(!bypass("hub.example.com", "example.org"));
    assert!(!bypass("11.1.2.3", "10.0.0.0/8"));
    assert!(!bypass("10.1.2.3", "10.0.0.0/99"));
}

#[test]
fn unsupported_proxies_fail_clearly_without_showing_the_address() {
    for value in [
        "socks5://user:hunter2@proxy:1080",
        "https://user:hunter2@proxy:443",
    ] {
        let error = resolve(&hub(), lookup(&[("HTTPS_PROXY", value)])).unwrap_err();
        assert!(error.contains("HTTPS_PROXY"), "{error}");
        assert!(error.contains("HTTP CONNECT"), "{error}");
        assert!(!error.contains("hunter2"), "{error}");
    }
    let error = resolve(&hub(), lookup(&[("HTTPS_PROXY", "http://[broken")])).unwrap_err();
    assert!(error.contains("HTTPS_PROXY"), "{error}");
}

#[test]
fn debug_output_redacts_proxy_credentials() {
    let proxy = resolve(
        &hub(),
        lookup(&[("HTTPS_PROXY", "http://user:hunter2@proxy:8080")]),
    )
    .unwrap()
    .unwrap();
    assert_eq!(proxy.address(), "proxy:8080");
    assert!(!format!("{proxy:?}").contains("hunter2"));
}

/// 假代理：收完 CONNECT 回一段固定响应，再把请求原文和之后收到的字节交回测试。
async fn fake_proxy(response: &'static [u8]) -> (String, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut seen = Vec::new();
        while !seen.ends_with(b"\r\n\r\n") {
            seen.push(socket.read_u8().await.unwrap());
        }
        socket.write_all(response).await.unwrap();
        String::from_utf8(seen).unwrap()
    });
    (address, task)
}

fn proxy_at(address: &str, userinfo: &str) -> Proxy {
    let value = format!("http://{userinfo}{address}");
    resolve(&hub(), lookup(&[("HTTPS_PROXY", value.as_str())]))
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn connect_keeps_bytes_that_follow_the_proxy_response() {
    let (address, task) =
        fake_proxy(b"HTTP/1.1 200 Connection established\r\nVia: test\r\n\r\ntunnel-bytes").await;
    let mut stream = proxy_at(&address, "")
        .connect("hub.example.com", 443)
        .await
        .unwrap();
    let mut rest = [0_u8; 12];
    stream.read_exact(&mut rest).await.unwrap();
    assert_eq!(&rest, b"tunnel-bytes");
    let request = task.await.unwrap();
    assert!(request.starts_with("CONNECT hub.example.com:443 HTTP/1.1\r\n"));
    assert!(request.contains("Host: hub.example.com:443\r\n"));
    assert!(!request.contains("Proxy-Authorization"));
}

#[tokio::test]
async fn credentials_are_decoded_and_sent_as_basic_auth() {
    let (address, task) = fake_proxy(b"HTTP/1.1 200 OK\r\n\r\n").await;
    proxy_at(&address, "us%40er:p%3Ass@")
        .connect("hub.example.com", 443)
        .await
        .unwrap();
    // base64("us@er:p:ss")
    assert!(task
        .await
        .unwrap()
        .contains("Proxy-Authorization: Basic dXNAZXI6cDpzcw==\r\n"));
}

#[tokio::test]
async fn refusals_and_outages_are_told_apart() {
    let (address, _task) = fake_proxy(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n").await;
    let error = proxy_at(&address, "user:hunter2@")
        .connect("hub.example.com", 443)
        .await
        .unwrap_err();
    assert!(matches!(error, ProxyError::Rejected(_)));
    assert!(error.to_string().contains("407"));
    assert!(!error.to_string().contains("hunter2"));

    let (address, _task) = fake_proxy(b"HTTP/1.1 403 Forbidden\r\n\r\n").await;
    let error = proxy_at(&address, "")
        .connect("hub.example.com", 443)
        .await
        .unwrap_err();
    assert!(matches!(error, ProxyError::Rejected(_)));

    let (address, _task) = fake_proxy(b"HTTP/1.1 502 Bad Gateway\r\n\r\n").await;
    let error = proxy_at(&address, "")
        .connect("hub.example.com", 443)
        .await
        .unwrap_err();
    assert!(matches!(error, ProxyError::Unreachable(_)));

    let (address, _task) = fake_proxy(b"not http at all\r\n\r\n").await;
    let error = proxy_at(&address, "")
        .connect("hub.example.com", 443)
        .await
        .unwrap_err();
    assert!(matches!(error, ProxyError::Rejected(_)));
}

#[tokio::test]
async fn a_dead_proxy_counts_as_unreachable() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    drop(listener);
    let error = proxy_at(&address, "")
        .connect("hub.example.com", 443)
        .await
        .unwrap_err();
    assert!(matches!(error, ProxyError::Unreachable(_)));
}

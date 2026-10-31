//! 连接级退避与错误分类；这里只决定是否再接线，不持有或重放任何 RPC。

use std::io;
use std::time::{Duration, Instant};

use tokio_tungstenite::tungstenite::{error::ProtocolError, Error};

const STABLE_CONNECTION: Duration = Duration::from_secs(60);
const CONFLICT_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::nodes) enum Retry {
    Transient,
    Conflict,
    Never,
    /// HTTP 401：多半是凭据被撤销或换过，但这个响应没有签名，也可能来自反代自己的认证。
    Refused,
}

/// 诊断要分清是哪一层出的错，光看 `retry` 分不出来。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::nodes) enum Layer {
    Http(u16),
    Tls,
    Proxy,
    Other,
}

#[derive(Debug)]
pub(in crate::nodes) struct Failure {
    pub retry: Retry,
    pub message: String,
    pub layer: Layer,
}

impl Failure {
    pub fn transient(message: impl Into<String>) -> Self {
        Self {
            retry: Retry::Transient,
            message: message.into(),
            layer: Layer::Other,
        }
    }

    pub fn permanent(message: impl Into<String>) -> Self {
        Self {
            retry: Retry::Never,
            message: message.into(),
            layer: Layer::Other,
        }
    }

    fn at(mut self, layer: Layer) -> Self {
        self.layer = layer;
        self
    }

    pub fn proxy(mut self) -> Self {
        self.layer = Layer::Proxy;
        self
    }

    pub fn websocket(error: Error) -> Self {
        match error {
            Error::Http(response) => {
                let status = response.status().as_u16();
                let retry = match status {
                    401 => Retry::Refused,
                    409 => Retry::Conflict,
                    429 | 500..=599 => Retry::Transient,
                    _ => Retry::Never,
                };
                Self { retry, message: format!("node admission failed with HTTP {status}"), layer: Layer::Http(status) }
            }
            // tokio-tungstenite 0.29 的 rustls 握手失败包装为 Io；不能只检查 Tls。
            Error::Io(error) if error.get_ref().is_some_and(|inner|
                inner.is::<tokio_rustls::rustls::Error>()) =>
                Self::permanent("node TLS verification failed; check the endpoint and trusted CA").at(Layer::Tls),
            Error::Tls(_) =>
                Self::permanent("node TLS verification failed; check the endpoint and trusted CA").at(Layer::Tls),
            Error::Io(error) if matches!(error.kind(),
                io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput
                | io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported) =>
                Self::permanent("node connection rejected invalid data or local access; check endpoint and TLS trust"),
            Error::Io(_) | Error::ConnectionClosed | Error::AlreadyClosed
            | Error::Protocol(ProtocolError::ResetWithoutClosingHandshake | ProtocolError::HandshakeIncomplete) =>
                Self::transient("node network connection was lost"),
            _ => Self::permanent("node WebSocket protocol or message limit was rejected"),
        }
    }
}

#[derive(Default)]
pub(in crate::nodes) struct Backoff {
    attempt: u32,
    connected_once: bool,
    conflict_since: Option<Instant>,
}

impl Backoff {
    /// 前台第一次连接就遇到 409，多半是开了第二个进程，退出才能让人看见。服务管理器刚重启的进程不一样：
    /// 那多半是 Hub 还没发现上一个进程已经没了，几秒后就好，按断线后的规矩重试。
    pub fn expecting_old_connection() -> Self {
        Self {
            connected_once: true,
            ..Self::default()
        }
    }

    pub fn connected(&mut self) {
        self.connected_once = true;
        self.conflict_since = None;
    }

    pub fn session_ended(&mut self, stable: bool) {
        if stable {
            self.attempt = 0;
        }
    }

    fn remaining_conflict_window(&self, now: Instant) -> Result<Option<Duration>, String> {
        let Some(since) = self.conflict_since else {
            return Ok(None);
        };
        CONFLICT_WINDOW.checked_sub(now.duration_since(since))
            .filter(|remaining| !remaining.is_zero()).map(Some)
            .ok_or_else(|| "node connection conflict persisted for 60 seconds; stop the duplicate node process before retrying".into())
    }

    /// 在下一次握手前检查截止，握手本身也只能占剩余窗口。
    pub fn connection_timeout(&self, now: Instant, normal: Duration) -> Result<Duration, String> {
        Ok(self
            .remaining_conflict_window(now)?
            .map_or(normal, |left| left.min(normal)))
    }

    pub fn next(
        &mut self,
        failure: &Failure,
        now: Instant,
        random: u64,
    ) -> Result<Duration, String> {
        match failure.retry {
            Retry::Never | Retry::Refused => return Err(failure.message.clone()),
            Retry::Conflict if !self.connected_once => {
                return Err(format!(
                    "{}; this machine is already connected",
                    failure.message
                ));
            }
            Retry::Conflict => {
                self.conflict_since.get_or_insert(now);
            }
            Retry::Transient => self.conflict_since = None,
        }
        let base_ms = (1_000_u64 << self.attempt.min(5)).min(30_000);
        // 首次不少于一秒，最高三十秒；抖动避免同一 Hub 的节点同时重试。
        let jitter_ms = base_ms / 5;
        let floor_ms = base_ms.min(30_000 - jitter_ms);
        let delay_ms = floor_ms + random % (jitter_ms + 1);
        self.attempt = self.attempt.saturating_add(1);
        let delay = Duration::from_millis(delay_ms);
        Ok(self
            .remaining_conflict_window(now)?
            .map_or(delay, |left| left.min(delay)))
    }
}

/// 只有在连接上实际收到消息满一分钟才视为稳定；收回阻塞处理器耗时不计入。
pub(super) struct Stability {
    since: Instant,
    pub stable: bool,
}

impl Stability {
    pub fn new() -> Self {
        Self {
            since: Instant::now(),
            stable: false,
        }
    }

    pub fn received(&mut self) {
        self.stable |= self.since.elapsed() >= STABLE_CONNECTION;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::tungstenite::http::Response;

    #[test]
    fn credential_tls_and_protocol_failures_are_terminal_but_network_faults_retry() {
        let http = |status| {
            Error::Http(
                Response::builder()
                    .status(status)
                    .body(None)
                    .unwrap()
                    .into(),
            )
        };
        for status in [400, 403, 404] {
            assert_eq!(Failure::websocket(http(status)).retry, Retry::Never);
        }
        assert_eq!(Failure::websocket(http(401)).retry, Retry::Refused);
        for status in [429, 500, 502, 503, 504] {
            assert_eq!(Failure::websocket(http(status)).retry, Retry::Transient);
        }
        assert_eq!(Failure::websocket(http(409)).retry, Retry::Conflict);
        let tls = tokio_rustls::rustls::Error::General("synthetic TLS rejection".into());
        assert_eq!(
            Failure::websocket(Error::Io(io::Error::other(tls))).retry,
            Retry::Never
        );
        assert_eq!(
            Failure::websocket(Error::Io(io::Error::from(io::ErrorKind::InvalidData))).retry,
            Retry::Never
        );
        assert_eq!(
            Failure::websocket(Error::Io(io::Error::from(io::ErrorKind::ConnectionRefused))).retry,
            Retry::Transient
        );
        assert_eq!(
            Failure::websocket(Error::Protocol(ProtocolError::ResetWithoutClosingHandshake)).retry,
            Retry::Transient
        );
        assert_eq!(
            Failure::websocket(Error::Protocol(ProtocolError::HandshakeIncomplete)).retry,
            Retry::Transient
        );
        assert_eq!(
            Failure::websocket(Error::Protocol(ProtocolError::UnmaskedFrameFromClient)).retry,
            Retry::Never
        );
    }

    #[test]
    fn backoff_is_bounded_and_short_connections_do_not_reset_it() {
        let mut backoff = Backoff::default();
        let failure = Failure::transient("network unavailable");
        let now = Instant::now();
        for expected in [1, 2, 4, 8, 16, 24, 24] {
            backoff.connected();
            backoff.session_ended(false);
            assert_eq!(
                backoff.next(&failure, now, 0).unwrap(),
                Duration::from_secs(expected)
            );
        }
        backoff.session_ended(true);
        assert_eq!(
            backoff.next(&failure, now, 0).unwrap(),
            Duration::from_secs(1)
        );
        for sample in [0, 1, 17, 1024, u64::MAX] {
            let mut jitter = Backoff::default();
            for _ in 0..50 {
                let delay = jitter.next(&failure, now, sample).unwrap();
                assert!((Duration::from_secs(1)..=Duration::from_secs(30)).contains(&delay));
            }
        }
    }

    #[test]
    fn initial_duplicate_exits_and_recovery_conflict_has_a_deadline() {
        let failure = Failure {
            retry: Retry::Conflict,
            layer: Layer::Http(409),
            message: "HTTP 409".into(),
        };
        let now = Instant::now();
        let mut backoff = Backoff::default();
        assert!(backoff.next(&failure, now, 0).is_err());
        backoff.connected();
        assert!(backoff.next(&failure, now, 0).is_ok());
        let near_end = now + Duration::from_secs(59);
        assert!(backoff.next(&failure, near_end, u64::MAX).unwrap() <= Duration::from_secs(1));
        assert_eq!(
            backoff
                .connection_timeout(near_end, Duration::from_secs(10))
                .unwrap(),
            Duration::from_secs(1)
        );
        assert!(backoff
            .connection_timeout(now + CONFLICT_WINDOW, Duration::from_secs(10))
            .is_err());
        assert!(backoff.next(&failure, now + CONFLICT_WINDOW, 0).is_err());
        backoff.connected();
        assert!(backoff.next(&failure, now + CONFLICT_WINDOW, 0).is_ok());
    }

    #[test]
    fn only_a_restarted_service_retries_a_first_conflict() {
        let conflict = || Failure {
            retry: Retry::Conflict,
            layer: Layer::Http(409),
            message: "HTTP 409".into(),
        };
        let now = Instant::now();
        assert!(Backoff::default().next(&conflict(), now, 0).is_err());
        let mut service = Backoff::expecting_old_connection();
        assert!(service.next(&conflict(), now, 0).is_ok());
        // 一分钟还没好就不是旧连接了，交给服务模式的长间隔重试。
        assert!(service.next(&conflict(), now + CONFLICT_WINDOW, 0).is_err());
    }

    #[test]
    fn network_failure_releases_the_conflict_deadline_without_resetting_backoff() {
        let conflict = Failure {
            retry: Retry::Conflict,
            layer: Layer::Http(409),
            message: "HTTP 409".into(),
        };
        let network = Failure::transient("connection refused");
        let now = Instant::now();
        let mut backoff = Backoff::default();
        backoff.connected();
        assert_eq!(
            backoff.next(&conflict, now, 0).unwrap(),
            Duration::from_secs(1)
        );
        assert_eq!(
            backoff
                .next(&network, now + Duration::from_secs(1), 0)
                .unwrap(),
            Duration::from_secs(2)
        );
        let later = now + CONFLICT_WINDOW * 2;
        assert_eq!(
            backoff
                .connection_timeout(later, Duration::from_secs(10))
                .unwrap(),
            Duration::from_secs(10)
        );
        assert_eq!(
            backoff.next(&network, later, 0).unwrap(),
            Duration::from_secs(4)
        );
        assert!(backoff.next(&conflict, later, 0).is_ok());
        assert!(backoff.next(&conflict, later + CONFLICT_WINDOW, 0).is_err());
    }
}

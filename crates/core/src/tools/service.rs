//! `exec_command` 的服务模式（`service_port`）：dev server 这类要一直开着、跨几次调用用的命令。
//!
//! 为什么要有：D10 Web 那轮 AI 后台起 `pnpm dev`，只能翻输出猜它起没起来；`timeout_ms` 最多
//! 10 分钟，转后台照样到点停，改两轮代码服务就没了，最后只好让 Playwright 自己起（审查 §11）。
//!
//! 管三件事：
//! - **起之前端口必须空着。**已经有东西在听，就分不清待会儿答话的是这条命令还是别人（另一个
//!   dev server、gld 自己的端口）。是同一个连接自己起的另一条服务，就把它的 `session_id` 说出来。
//! - **等它答话。**只连本机回环（127.0.0.1、::1），连得上、进程也还活着才算 ready。只连 TCP，
//!   不发 HTTP：连得上说明在听，页面对不对是测试的事。
//! - **说出它听在哪。**只有 ::1 答话的写明（Vite 默认就这样，探 127.0.0.1 的工具会一直等）；
//!   从本机的非回环地址也连得上，说明它听在所有网卡上，同一网络的机器都能访问。gld 不替它改，只说。
//!
//! 不是隔离：gld 管不了 dev server 绑哪个地址、对外发什么请求，它和别的项目命令一样只受策略约束。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, UdpSocket};
use std::time::Duration;

use serde_json::{json, Value};

use crate::tools::workspace::WorkspaceError;

/// 普通命令的 `timeout_ms` 上限：10 分钟。
pub const PLAIN_MAX_TIMEOUT_MS: u64 = 600_000;
/// 带 `service_port` 时 `timeout_ms` 的默认值（30 分钟）和上限（1 小时）。到点照样停：AI 忘了
/// `kill_session` 的服务最多再占一小时端口，不会一直挂着。
pub const DEFAULT_TIMEOUT_MS: u64 = 1_800_000;
pub const MAX_TIMEOUT_MS: u64 = 3_600_000;
/// 没给 `yield_time_ms` 时最多等它答话多久。答话了就提前返回，所以给满 yield 的上限。
pub const DEFAULT_WAIT_MS: u64 = 30_000;

/// 回环上连不上是立刻被拒（RST），这个超时只在包被丢掉时起作用。
const CONNECT_TIMEOUT: Duration = Duration::from_millis(300);

pub fn port_arg(args: &Value) -> Result<Option<u16>, WorkspaceError> {
    match args.get("service_port") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|port| u16::try_from(port).ok())
            .filter(|port| *port > 0)
            .map(Some)
            .ok_or_else(|| {
                WorkspaceError::invalid_argument("service_port must be a TCP port from 1 to 65535")
            }),
    }
}

/// 本机回环上哪几个地址在这个端口答话。
pub fn answering(port: u16) -> Vec<&'static str> {
    [
        ("127.0.0.1", IpAddr::V4(Ipv4Addr::LOCALHOST)),
        ("::1", IpAddr::V6(Ipv6Addr::LOCALHOST)),
    ]
    .into_iter()
    .filter(|(_, ip)| connects(*ip, port))
    .map(|(label, _)| label)
    .collect()
}

fn connects(ip: IpAddr, port: u16) -> bool {
    TcpStream::connect_timeout(&SocketAddr::new(ip, port), CONNECT_TIMEOUT).is_ok()
}

/// 从本机一个非回环地址也连得上，就是听在所有网卡上了。找不到这样的地址（没联网）是 `None`：不知道。
fn reachable_beyond_loopback(port: u16) -> Option<bool> {
    outward_address().map(|ip| connects(ip, port))
}

/// 往外发包会用的那个本机地址。UDP 的 connect 只选路由、不发包；192.0.2.1 是文档专用地址（RFC 5737）。
fn outward_address() -> Option<IpAddr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

/// 回包里的 `service`。`running` 是命令进程还在不在：进程没了，端口上答话的就不是它。
pub fn status(port: u16, running: bool) -> Value {
    if !running {
        return json!({
            "port": port,
            "ready": false,
            "listening_on": [],
            "note": "the command is no longer running, so it is not serving this port"
        });
    }
    let listening = answering(port);
    let ready = !listening.is_empty();
    let exposed = if ready {
        reachable_beyond_loopback(port)
    } else {
        None
    };
    let note = if !ready {
        format!(
            "nothing answers on 127.0.0.1 or ::1 port {port} yet. Call read_output with this session_id to check again; if the output names another port, the server is not using service_port (pass the port to it, e.g. --port {port} --strictPort)"
        )
    } else if exposed == Some(true) {
        format!(
            "port {port} also answers on this machine's network address, so other machines on the same network can reach it (the server listens on all interfaces, e.g. --host or 0.0.0.0). Unless the user asked for that, stop it and start it bound to 127.0.0.1"
        )
    } else if listening == ["::1"] {
        format!(
            "only ::1 answers. Tools that connect to 127.0.0.1 (curl 127.0.0.1, Playwright webServer.url) will not reach it: use http://localhost:{port}, or start it with --host 127.0.0.1"
        )
    } else {
        format!(
            "ready on localhost port {port}. The port was free when this command started, so it is almost certainly this command's server. Stop it with kill_session when you are done; it is stopped anyway when timeout_ms runs out"
        )
    };
    json!({
        "port": port,
        "ready": ready,
        "listening_on": listening,
        "reachable_from_network": exposed,
        "note": note
    })
}

/// 起之前端口已被占用。`own` 是同一个连接自己起的、还在跑的那条服务：(session_id, 命令)。
pub fn port_in_use(
    port: u16,
    listening: Vec<&str>,
    own: Option<(String, String)>,
) -> WorkspaceError {
    let message = match &own {
        Some((session_id, command)) => format!(
            "Port {port} is already served by your own running command {session_id} ({command}). Use that one, or stop it with kill_session before starting another"
        ),
        None => format!(
            "Something is already listening on port {port} ({}), and it was not started through this connection, so a later answer on that port could come from either. Nothing was started. Pick a free port and pass it to the server (e.g. --port <port> --strictPort), or ask the user what is using {port}",
            listening.join(", ")
        ),
    };
    WorkspaceError::ToolDetails {
        code: "PORT_IN_USE",
        message,
        category: "conflict",
        retryable: false,
        details: json!({
            "port": port,
            "listening_on": listening,
            "own_session_id": own.map(|(session_id, _)| session_id),
            "executed": false
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn a_port_is_a_port() {
        assert_eq!(port_arg(&json!({})).unwrap(), None);
        assert_eq!(
            port_arg(&json!({ "service_port": 5173 })).unwrap(),
            Some(5173)
        );
        for bad in [
            json!(0),
            json!(65_536),
            json!(-1),
            json!("5173"),
            json!(51.5),
        ] {
            assert!(port_arg(&json!({ "service_port": bad })).is_err(), "{bad}");
        }
    }

    #[test]
    fn only_loopback_addresses_that_answer_are_listed() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let port = listener.local_addr().expect("addr").port();
        assert_eq!(answering(port), ["127.0.0.1"]);
        let status = status(port, true);
        assert_eq!(status["ready"], true);
        // 只绑了 127.0.0.1：从本机网卡地址连不上。没联网时是 null。
        assert_ne!(status["reachable_from_network"], true, "{status}");
        // 不在 drop 之后断言"没人答话"了：并发的测试 spawn 子进程时可能继承这个监听 socket，
        // 端口会多活一会儿。没起来的情形由 tests/service_mode.rs 用真进程测。
    }

    /// 绑在所有网卡上的要说出来：同一网络的机器能连上。
    #[test]
    fn listening_on_every_interface_is_reported() {
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let status = status(port, true);
        assert_eq!(status["ready"], true);
        if outward_address().is_some() {
            assert_eq!(status["reachable_from_network"], true, "{status}");
            assert!(status["note"].as_str().unwrap().contains("network"));
        }
    }

    #[test]
    fn a_process_that_exited_serves_nothing() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let port = listener.local_addr().expect("addr").port();
        assert_eq!(
            status(port, false)["ready"],
            false,
            "端口有人答话，但不是这条命令"
        );
    }
}

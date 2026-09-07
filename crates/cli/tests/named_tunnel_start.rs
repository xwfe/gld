mod common;

use clap::Parser;
use common::env::{free_port, Env};
use gld::Cli;

#[test]
fn explicit_port_applies_to_new_and_running_workspace() {
    let env = Env::new();
    let first = free_port();
    env.ok(&["start", ".", "--port", &first.to_string()]);
    let second = free_port();
    env.ok(&["start", ".", "--port", &second.to_string()]);
    env.ok(&["start"]);
    let state = env.json(&["--json", "list"]);
    assert_eq!(
        state["mcp"]["local_url"],
        format!("http://127.0.0.1:{second}/mcp")
    );
    assert!(std::net::TcpStream::connect(("127.0.0.1", second)).is_ok());
    assert!(std::net::TcpStream::connect(("127.0.0.1", first)).is_err());
}

#[cfg(unix)]
#[test]
fn named_tunnel_accepts_a_reachable_endpoint_with_new_token_flag() {
    let mut env = Env::new();
    env.fake_binary("cloudflared", "#!/bin/sh\necho 'INF Registered tunnel connection connIndex=0'\nwhile true; do sleep 1; done\n");
    let port = free_port();
    // 用真实本地 MCP 作为入口，隔离测试不访问 Cloudflare。
    let state = env.json(&[
        "--json",
        "start",
        ".",
        "--port",
        &port.to_string(),
        "--tunnel",
        &format!("cf:http://127.0.0.1:{port}/mcp"),
        "--token",
        "fixture-token",
    ]);
    assert_eq!(state["mcp"]["state"], "running");
    env.ok(&["start"]);
}

#[test]
fn token_flags_and_explicit_port_parse() {
    for command in ["start", "share", "upgrade"] {
        for flag in ["--token", "--tunnel-token"] {
            assert!(
                Cli::try_parse_from([
                    "gld",
                    command,
                    "--tunnel",
                    "cf:example.com",
                    flag,
                    "fixture-token"
                ])
                .is_ok(),
                "{command} {flag}"
            );
        }
    }
    assert!(Cli::try_parse_from(["gld", "start", "--port", "28767"]).is_ok());
    for port in ["0", "65536", "invalid"] {
        assert!(Cli::try_parse_from(["gld", "start", "--port", port]).is_err());
    }
    assert!(Cli::try_parse_from(["gld", "start", "--token", "fixture-token"]).is_err());
}

#[cfg(unix)]
#[test]
fn registered_named_tunnel_with_502_must_not_report_success() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop = done.clone();
    let server = std::thread::spawn(move || {
        while !stop.load(std::sync::atomic::Ordering::Relaxed) {
            if let Ok((mut stream, _)) = listener.accept() {
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(1)))
                    .unwrap();
                let _ = stream.read(&mut [0; 2048]);
                let _ = stream.write_all(
                    b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            } else {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    });
    let mut env = Env::new();
    env.fake_binary("cloudflared", "#!/bin/sh\necho 'INF Registered tunnel connection connIndex=0'\nwhile true; do sleep 1; done\n");
    let port = free_port();
    let output = env.gld(&[
        "start",
        ".",
        "--port",
        &port.to_string(),
        "--tunnel",
        &format!("cf:http://{address}/mcp"),
        "--tunnel-token",
        "fixture-token",
    ]);
    // 不再传 --tunnel 也必须检查保存的 named 配置；--json 不能吞掉错误。
    let repeated = env.gld(&["--json", "start"]);
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    server.join().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "连接边缘不代表公网可达");
    assert!(!repeated.status.success(), "重复启动也必须报告公网失败");
    assert!(stderr.contains("502"), "{stderr}");
    assert!(
        stderr.contains(&format!("http://127.0.0.1:{port}")),
        "{stderr}"
    );
    assert!(stderr.contains("回源"), "{stderr}");
    assert!(!stderr.contains("fixture-token"), "凭据泄露");
    let state = env.json(&["--json", "list"]);
    assert_eq!(
        state["mcp"]["state"], "running",
        "失败不能停本地服务：{state}"
    );
}

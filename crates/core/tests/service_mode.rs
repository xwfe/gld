//! `exec_command` 的服务模式（`service_port`）：dev server 起之前端口要空着、答话了才算起来、
//! 能开到 1 小时。D10 Web 那轮后台起的 dev server 到 10 分钟就被停，AI 也只能翻输出猜它起没起来（审查 §11）。
//!
//! 真起一个只会 listen 的 python3 小脚本当服务：python3 在默认白名单里，CI 的 Ubuntu、macOS 都有。
//!
//! **不赌第一次调用就看到它起来。**CI run 37724176474 的 macOS 上，`python3 -m http.server` 跑满 30 秒端口都没人
//! 答话、一行输出也没有（它 bind 之后先 `socket.getfqdn()` 反查主机名，查完才 listen）；本机用 `taskpolicy -b`
//! 压到后台优先级，换成这个小脚本也有一两成跑不进 30 秒。所以第一次没起来的，照产品的用法拿 `read_output` 轮询到
//! 它答话；第一次就起来了的，才检查"答话就返回、没等满 30 秒"。

#![cfg(unix)]

mod common;

use std::fs;
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use gld_core::tools::{call_tool, ToolContext};
use serde_json::{json, Value};

struct Fixture {
    ctx: ToolContext,
    _temp: tempfile::TempDir,
}

fn fixture() -> Fixture {
    common::isolate_data_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace: PathBuf = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    fs::write(workspace.join("index.html"), "<h1>hi</h1>\n").expect("file");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("ctx");
    Fixture { ctx, _temp: temp }
}

/// 一个此刻空着的端口：从 20000 往上按计数取，"连一下"确认没人听。
///
/// 别改成"bind 到 0 再放掉"：macOS 上测试进程里的监听 socket 会被并发 spawn 的子进程继承，
/// 端口就被一个无关的 python 占着。原委见 `crates/cli/tests/common/env.rs` 的 `free_port`。
fn free_port() -> u16 {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let start = std::process::id() % 6_000;
    for _ in 0..6_000 {
        let port = 20_000 + ((start + NEXT.fetch_add(1, Ordering::Relaxed)) % 6_000) as u16;
        if !answers(port) {
            return port;
        }
    }
    panic!("20000–26000 之间没有一个空闲端口");
}

fn answers(port: u16) -> bool {
    TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_ok()
}

/// 只在 127.0.0.1 上 listen、来一个连接就关一个的服务。服务模式只连 TCP，不需要它会说 HTTP。
const LISTENER: &str = "import socket, sys
s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(('127.0.0.1', int(sys.argv[1])))
s.listen()
while True:
    s.accept()[0].close()
";

fn serve(ctx: &ToolContext, port: u16, extra: Value) -> Value {
    let mut args = json!({
        "argv": ["python3", "-c", LISTENER, port.to_string()],
        "service_port": port,
    });
    for (key, value) in extra.as_object().expect("object") {
        args[key] = value.clone();
    }
    call_tool(ctx, "exec_command", &args)
}

/// exec_command 回来时服务还没答话的，轮询 read_output 到它答话（最多 2 分钟），返回最后一次回包。
fn until_ready(ctx: &ToolContext, started: &Value) -> Value {
    if started["service"]["ready"] == true {
        return started.clone();
    }
    let session = started["session_id"].as_str().expect("session");
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let read = call_tool(ctx, "read_output", &json!({ "session_id": session }));
        if read["service"]["ready"] == true || read["running"] == false || Instant::now() > deadline
        {
            return read;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn kill(ctx: &ToolContext, session: &str) {
    let out = call_tool(ctx, "kill_session", &json!({ "session_id": session }));
    assert_eq!(out["ok"], true, "{out}");
}

#[test]
fn a_service_returns_once_its_port_answers_and_keeps_running() {
    let fx = fixture();
    let port = free_port();
    let out = serve(&fx.ctx, port, json!({}));
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(out["status"], "running", "{out}");
    let session = out["session_id"].as_str().expect("session").to_string();
    let summary = out["command_summary"].as_str().unwrap_or_default();
    if out["service"]["ready"] == true {
        // 一答话就返回，不等满默认的 30 秒。去掉提前返回的话，这里总是 30 秒多。
        assert!(
            out["duration_ms"].as_u64().expect("duration") < 30_000,
            "{out}"
        );
        assert!(summary.starts_with("service is up"), "{out}");
    } else {
        assert!(summary.contains("nothing answers"), "{out}");
    }
    let ready = until_ready(&fx.ctx, &out);
    assert_eq!(ready["service"]["ready"], true, "{ready}");
    assert_eq!(
        ready["service"]["listening_on"],
        json!(["127.0.0.1"]),
        "{ready}"
    );
    assert_ne!(ready["service"]["reachable_from_network"], true, "{ready}");
    assert!(answers(port));

    // read_output 现查一次，list_runs 认得出哪条是服务。
    let read = call_tool(&fx.ctx, "read_output", &json!({ "session_id": session }));
    assert_eq!(read["service"]["ready"], true, "{read}");
    let listed = call_tool(&fx.ctx, "list_runs", &json!({ "status": ["running"] }));
    assert_eq!(listed["runs"][0]["service_port"], port, "{listed}");

    kill(&fx.ctx, &session);
    assert!(!answers(port), "kill_session 之后端口该空出来");
}

/// 端口已经有人在听：分不清待会儿答话的是谁，什么都不起。
#[test]
fn a_taken_port_starts_nothing() {
    let fx = fixture();
    let squatter = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
    let port = squatter.local_addr().expect("addr").port();
    let out = serve(&fx.ctx, port, json!({}));
    assert_eq!(out["ok"], false, "{out}");
    assert_eq!(out["error"]["code"], "PORT_IN_USE", "{out}");
    assert_eq!(out["error"]["details"]["executed"], false, "{out}");
    assert!(out["error"]["details"]["own_session_id"].is_null(), "{out}");
    let listed = call_tool(&fx.ctx, "list_runs", &json!({}));
    assert_eq!(listed["total"], 0, "什么都不该起：{listed}");
}

/// 占着端口的是自己起的那条：说出它的 session_id，AI 就不会再起一个、也不用去猜。
#[test]
fn a_second_start_on_my_own_service_names_it() {
    let fx = fixture();
    let port = free_port();
    let first = serve(&fx.ctx, port, json!({}));
    let session = first["session_id"].as_str().expect("session").to_string();
    let ready = until_ready(&fx.ctx, &first);
    assert_eq!(ready["service"]["ready"], true, "{ready}");

    let second = serve(&fx.ctx, port, json!({}));
    assert_eq!(second["error"]["code"], "PORT_IN_USE", "{second}");
    assert_eq!(
        second["error"]["details"]["own_session_id"], session,
        "{second}"
    );
    assert!(
        second["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains(&session),
        "{second}"
    );
    kill(&fx.ctx, &session);
}

/// 进程在跑、端口没人答话（没起来，或者听在别的端口）：照实说，不算起来了。
#[test]
fn a_service_that_does_not_listen_is_not_ready() {
    let fx = fixture();
    let port = free_port();
    let out = call_tool(
        &fx.ctx,
        "exec_command",
        &json!({
            "argv": ["python3", "-c", "import time; time.sleep(30)"],
            "service_port": port,
            "yield_time_ms": 500,
        }),
    );
    assert_eq!(out["status"], "running", "{out}");
    assert_eq!(out["service"]["ready"], false, "{out}");
    assert!(
        out["command_summary"]
            .as_str()
            .unwrap_or_default()
            .contains("nothing answers"),
        "{out}"
    );
    kill(&fx.ctx, out["session_id"].as_str().expect("session"));
}

/// 普通命令最多 10 分钟；带 service_port 的服务能到 1 小时，再长也不行。
#[test]
fn only_a_service_may_run_past_ten_minutes() {
    let fx = fixture();
    let decision = |args: Value| {
        let out = call_tool(&fx.ctx, "check_command", &args);
        (
            out["decision"].as_str().unwrap_or_default().to_string(),
            out["rule"].as_str().unwrap_or_default().to_string(),
        )
    };
    let plain = decision(json!({ "cmd": "python3 -V", "timeout_ms": 1_200_000 }));
    assert_eq!(plain.0, "deny", "{plain:?}");
    let service =
        decision(json!({ "cmd": "python3 -V", "timeout_ms": 1_200_000, "service_port": 8000 }));
    assert_eq!(service.0, "allow", "{service:?}");
    let too_long =
        decision(json!({ "cmd": "python3 -V", "timeout_ms": 3_600_001, "service_port": 8000 }));
    assert_eq!(too_long.0, "deny", "{too_long:?}");
    assert_eq!(too_long.1, plain.1, "同一条规则：{too_long:?}");

    let bad_port = call_tool(
        &fx.ctx,
        "exec_command",
        &json!({ "cmd": "python3 -V", "service_port": 70_000 }),
    );
    assert_eq!(bad_port["ok"], false, "{bad_port}");
}

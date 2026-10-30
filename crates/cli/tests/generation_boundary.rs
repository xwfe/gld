//! 稳定宿主和执行代的边界（RFC-0008 §3.4、§5）。全部用真进程、真 HTTP、隔离的 `GLD_HOME`。
//!
//! 要证明的只有一件事：工具实现出了什么问题、换成哪个版本，对外的端口和认证都不动。
//! 这里没有一条测试说明已经零中断：哪些还没做到写在 RFC 的 §5。
//!
//! 只在 Unix 跑：要 `kill -9`、要用 shell 脚本冒充坏掉的新版本。

#![cfg(unix)]

mod common;

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use common::env::{free_port, Env};
use common::http::{post_form, post_json, query_param, request, request_waiting, Reply};
use common::oauth::{authorize, connect_a_connector, endpoint_path, pkce, REDIRECT};
use serde_json::{json, Value};

struct Fixture {
    env: Env,
    port: u16,
    token: String,
    other: tempfile::TempDir,
}

/// 两个项目 api（测试项目目录）和 web，bearer 认证，命令放开（要跑 sleep、cat）。
fn fixture() -> Fixture {
    let env = Env::new();
    let other = tempfile::tempdir().expect("web dir");
    env.ok(&["add", ".", "--name", "api"]);
    env.ok(&["add", other.path().to_str().unwrap(), "--name", "web"]);
    env.ok(&["set", "api", "command-policy=all"]);
    env.ok(&["set", "web", "command-policy=all"]);
    let port = free_port();
    env.ok(&["upgrade", "--port", &port.to_string(), "--auth", "bearer"]);
    env.ok(&["start"]);
    let token = env.service_secret("bearer_token");
    Fixture {
        env,
        port,
        token,
        other,
    }
}

fn rpc_body(method: &str, params: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }).to_string()
}

fn call_body(name: &str, arguments: Value) -> String {
    rpc_body(
        "tools/call",
        json!({ "name": name, "arguments": arguments }),
    )
}

impl Fixture {
    fn post(&self, body: &str) -> Reply {
        post_json(self.port, "/mcp", body, Some(&self.token))
    }

    fn tool(&self, name: &str, arguments: Value) -> Value {
        let reply = self.post(&call_body(name, arguments));
        assert_eq!(reply.status, 200, "{}", reply.body);
        structured(&reply)
    }

    fn status(&self) -> Value {
        self.env.json(&["--json", "daemon", "upgrade", "status"])
    }

    fn host_pid(&self) -> u32 {
        self.status()["host"]["pid"].as_u64().unwrap() as u32
    }

    /// 这个项目现在归哪一代：跑一条命令，看运行记录里是谁起的。
    fn runner_of(&self, workspace: &str) -> u32 {
        let run = self.tool(
            "exec_command",
            json!({ "workspace": workspace, "cmd": "true", "yield_time_ms": 30000 }),
        );
        assert_eq!(run["ok"], true, "{run}");
        let listed = self.tool("list_runs", json!({ "workspace": workspace, "limit": 1 }));
        listed["runs"][0]["started_by_gld_pid"]
            .as_u64()
            .unwrap_or_else(|| panic!("运行记录没有 pid：{listed}")) as u32
    }

    fn wait_until_answering(&self) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let reply = self.post(&call_body("list_workspaces", json!({})));
            if reply.status == 200 && reply.json().get("error").is_none() {
                return;
            }
            assert!(Instant::now() < deadline, "服务一直没恢复：{}", reply.body);
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn listener_pid(&self) -> Option<u32> {
        gld_core::platform::platform()
            .find_pid_listening_on_port(self.port)
            .expect("查端口")
    }
}

fn structured(reply: &Reply) -> Value {
    let body = reply.json();
    let text = body["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("没有工具结果：{}", reply.body));
    serde_json::from_str(text).expect("工具结果是 JSON")
}

fn generation_pids(status: &Value) -> Vec<u32> {
    status["generations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["pid"].as_u64().unwrap() as u32)
        .collect()
}

fn generation(status: &Value, id: &str) -> Value {
    status["generations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id)
        .cloned()
        .unwrap_or_else(|| panic!("没有 {id}：{status}"))
}

/// 一条这次运行独有的 sleep 命令。`pgrep -f` 看整条命令行，写死的字样会匹配到任何恰好带着
/// 这段字的进程（比如正在跑这批测试的 shell 脚本），然后报一个不存在的孤儿。
fn unique_sleep() -> String {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let nth = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("sleep 90.{}{nth}", std::process::id())
}

fn alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn kill_9(pid: u32) {
    let killed = Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .expect("kill");
    assert!(killed.success());
}

fn wait_gone(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while alive(pid) {
        assert!(Instant::now() < deadline, "pid {pid} 一直没退");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !ok() {
        assert!(Instant::now() < deadline, "等不到：{what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// 工具真的在子进程里跑，对外端口真的在宿主手里。
#[test]
fn tools_run_in_a_child_and_the_port_stays_with_the_host() {
    let fixture = fixture();
    let status = fixture.status();
    let host = status["host"]["pid"].as_u64().unwrap() as u32;
    let pids = generation_pids(&status);
    assert_eq!(pids.len(), 1, "{status}");
    assert_ne!(pids[0], host, "执行代不该是宿主自己：{status}");

    assert_eq!(fixture.listener_pid(), Some(host), "端口应该在宿主手里");
    assert_eq!(fixture.runner_of("api"), pids[0], "命令应该是执行代起的");

    // `gld tool call` 走的也是同一代：两条入口共用一张命令表和写入序号。
    fixture.env.json(&[
        "--json",
        "-w",
        "api",
        "tool",
        "call",
        "exec_command",
        "cmd=sleep 0.1",
        "yield_time_ms:=30000",
    ]);
    let direct = fixture.env.json(&[
        "--json",
        "-w",
        "api",
        "tool",
        "call",
        "list_runs",
        "limit:=1",
    ]);
    assert_eq!(
        direct["runs"][0]["started_by_gld_pid"].as_u64(),
        Some(u64::from(pids[0])),
        "{direct}"
    );
}

/// 项目命令拿不到私有通道：除了 stdio 再没有别的继承下来的描述符。
/// 有的话它就能冒充宿主发消息，或者抢走发给执行代的消息。
#[test]
fn project_commands_inherit_nothing_from_the_private_channel() {
    let fixture = fixture();
    // -L：看描述符指向的东西是什么类型，两个平台都用 p 表示管道。
    let run = fixture.tool(
        "exec_command",
        json!({ "workspace": "api", "argv": ["sh", "-c", "ls -lL /dev/fd"], "yield_time_ms": 30000 }),
    );
    let text = run["stdout"].as_str().unwrap_or_else(|| panic!("{run}"));
    let pipes = text.lines().filter(|line| line.starts_with('p')).count();
    // 就 stdin、stdout、stderr 三根。多出来的那根就是通道。
    assert_eq!(pipes, 3, "项目命令继承了多余的管道：\n{text}");
}

/// 杀掉执行代：在途请求如实失败，端口、认证都不变，新请求接着能用。
#[test]
fn killing_the_generation_keeps_the_port_and_the_auth() {
    let fixture = fixture();
    let before = fixture.status();
    let host = fixture.host_pid();
    let old = generation_pids(&before)[0];

    let sleeper = unique_sleep();
    let in_flight = {
        let (port, token) = (fixture.port, fixture.token.clone());
        let sleeper = sleeper.clone();
        std::thread::spawn(move || {
            let body = call_body(
                "exec_command",
                json!({ "workspace": "api", "cmd": sleeper, "yield_time_ms": 30000 }),
            );
            let auth = format!("Bearer {token}");
            let headers = [
                ("Content-Type", "application/json"),
                ("Authorization", auth.as_str()),
            ];
            request_waiting(
                port,
                "POST",
                "/mcp",
                &headers,
                &body,
                Duration::from_secs(30),
            )
        })
    };
    wait_for("请求进到执行代", || {
        generation(&fixture.status(), "g1")["in_flight"] == 1
    });

    kill_9(old);
    let reply = in_flight.join().expect("请求线程");
    assert_eq!(reply.status, 200, "入口是好的，不该回 5xx：{}", reply.body);
    let error = &reply.json()["error"];
    assert_eq!(error["data"]["reason"], "generation_lost", "{}", reply.body);
    assert_eq!(
        error["data"]["retryable"], false,
        "结果未知时不能叫客户端直接重试：{}",
        reply.body
    );

    // 没有执行代时也不能放松鉴权。
    let wrong = post_json(
        fixture.port,
        "/mcp",
        &call_body("list_workspaces", json!({})),
        Some("wrong-token"),
    );
    assert_eq!(wrong.status, 401, "{}", wrong.body);

    fixture.wait_until_answering();
    assert_eq!(fixture.host_pid(), host, "宿主不该动");
    assert_eq!(fixture.listener_pid(), Some(host), "端口不该换主人");
    let after = fixture.status();
    assert_eq!(after["restarts"], 1, "{after}");
    let new = generation_pids(&after);
    assert_eq!(new.len(), 1, "{after}");
    assert_ne!(new[0], old);
    assert!(
        after["last_failure"].is_null(),
        "恢复之后不该还挂着失败：{after}"
    );
    assert_eq!(fixture.runner_of("api"), new[0]);
    let _ = Command::new("pkill").args(["-f", &sleeper]).status();
}

/// 已有 token、refresh token、client_id、签名密钥，和进行到一半的授权码，都在宿主手里。
#[test]
fn oauth_state_survives_losing_the_generation() {
    let env = Env::new();
    let port = free_port();
    env.ok(&["add", ".", "--name", "api"]);
    env.ok(&["start", "--port", &port.to_string()]);
    let password = env.service_secret("oauth_password");
    let token_secret = env.service_secret("oauth_token_secret");
    let (client_id, access, refresh, token_path) = connect_a_connector(port, &password);

    // 授权进行到一半：码发了还没换。
    let metadata = common::http::get(port, "/.well-known/oauth-authorization-server").json();
    let authorize_path = endpoint_path(&metadata, "authorization_endpoint");
    let (verifier, challenge) = pkce();
    let granted = authorize(port, &authorize_path, &client_id, &challenge, &password);
    let code = query_param(granted.header("location").unwrap_or_default(), "code");

    // 先切一代、再把当前代杀掉：两种换代都不碰认证状态。
    env.ok(&["daemon", "upgrade", "prepare", env!("CARGO_BIN_EXE_gld")]);
    env.ok(&["daemon", "upgrade", "switch"]);
    let status = env.json(&["--json", "daemon", "upgrade", "status"]);
    let current = generation(&status, status["current"].as_str().unwrap());
    kill_9(current["pid"].as_u64().unwrap() as u32);

    let deadline = Instant::now() + Duration::from_secs(20);
    let tools = rpc_body("tools/list", json!({}));
    loop {
        let reply = post_json(port, "/mcp", &tools, Some(&access));
        assert_ne!(reply.status, 401, "已有 token 不该失效：{}", reply.body);
        if reply.status == 200 && reply.json().get("error").is_none() {
            break;
        }
        assert!(Instant::now() < deadline, "没恢复：{}", reply.body);
        std::thread::sleep(Duration::from_millis(100));
    }

    let exchanged = post_form(
        port,
        &token_path,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", REDIRECT),
            ("client_id", &client_id),
            ("code_verifier", &verifier),
        ],
    );
    assert_eq!(exchanged.status, 200, "授权码丢了：{}", exchanged.body);

    let refreshed = post_form(
        port,
        &token_path,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", &refresh),
            ("client_id", &client_id),
        ],
    );
    assert_eq!(refreshed.status, 200, "refresh 失效：{}", refreshed.body);
    assert_eq!(
        env.service_secret("oauth_token_secret"),
        token_secret,
        "签名密钥被重新生成了"
    );
}

/// 隧道进程是宿主的子进程，执行代没了它照跑，公网地址不变。
#[test]
fn the_tunnel_process_and_public_address_stay_when_the_generation_dies() {
    let mut env = Env::new();
    let marker = env.home.path().join("tunnel.pid");
    env.fake_binary(
        "cloudflared",
        &format!(
            "#!/bin/sh\necho $$ > '{}'\necho 'INF Registered tunnel connection connIndex=0'\nwhile true; do sleep 1; done\n",
            marker.display()
        ),
    );
    let port = free_port();
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
    let public = state["service"]["publicEndpoint"].clone();
    let tunnel: u32 = std::fs::read_to_string(&marker)
        .expect("假隧道该记下 pid")
        .trim()
        .parse()
        .unwrap();

    let status = env.json(&["--json", "daemon", "upgrade", "status"]);
    kill_9(generation_pids(&status)[0]);
    wait_for("执行代重启", || {
        env.json(&["--json", "daemon", "upgrade", "status"])["restarts"] == 1
    });

    env.ok(&["daemon", "upgrade", "prepare", env!("CARGO_BIN_EXE_gld")]);
    env.ok(&["daemon", "upgrade", "switch"]);

    assert!(alive(tunnel), "隧道进程不该跟着执行代死");
    let tunnel_after: u32 = std::fs::read_to_string(&marker)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(tunnel_after, tunnel, "隧道被重启了");
    let after = env.json(&["--json", "ls"]);
    assert_eq!(after["service"]["publicEndpoint"], public, "{after}");
}

/// 宿主被 kill -9：执行代读到管道关闭，收好命令自己退，记录写 interrupted，不留孤儿。
#[test]
fn losing_the_host_stops_the_generation_and_its_commands() {
    let fixture = fixture();
    // 用 `gld tool call` 起：重启之后读它的还是同一个主体（本机）。
    let sleeper = unique_sleep();
    let cmd = format!("cmd={sleeper}");
    let started = fixture.env.json(&[
        "--json",
        "-w",
        "api",
        "tool",
        "call",
        "exec_command",
        &cmd,
        "yield_time_ms:=0",
        "timeout_ms:=120000",
    ]);
    let session = started["session_id"].as_str().unwrap().to_string();
    let status = fixture.status();
    let generation_pid = generation_pids(&status)[0];

    kill_9(fixture.host_pid());
    wait_gone(generation_pid);
    let leftover = Command::new("pgrep")
        .args(["-f", &sleeper])
        .stdout(std::process::Stdio::null())
        .status()
        .expect("pgrep");
    assert!(!leftover.success(), "命令成了孤儿");

    fixture.env.ok(&["daemon", "start"]);
    let read = fixture.env.json(&[
        "--json",
        "-w",
        "api",
        "tool",
        "call",
        "read_output",
        &format!("output_ref=session:{session}:stdout"),
    ]);
    assert_eq!(read["termination_reason"], "interrupted", "{read}");
}

/// 假的新版本：一段 shell，按给的方式回应第一条消息。
fn fake_candidate(env: &Env, name: &str, script: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = env.home.path().join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn ready_frame(protocol: u32, state: u32) -> String {
    format!(
        "printf '%s\\n' '{{\"frame\":\"ready\",\"identity\":{{\"version\":\"9.9.9\",\"build_commit\":null,\"generation_protocol\":{protocol},\"state_format\":{state},\"pid\":'\"$$\"',\"exe\":null}}}}'"
    )
}

/// 不管新版本坏成什么样，失败都发生在动路由之前，在线服务一点不受影响。
#[test]
fn a_bad_new_version_fails_before_the_live_service_notices() {
    let fixture = fixture();
    let env = &fixture.env;
    let current = gld_core::generation::GENERATION_PROTOCOL;
    let state = gld_core::generation::STATE_FORMAT;
    fixture.tool("list_workspaces", json!({}));
    let before = fixture.status();
    let g1 = generation_pids(&before)[0];
    let served = fixture.runner_of("api");

    // 只回 ready，之后什么都不做。
    let silent = format!("read hello\n{}\nsleep 300", ready_frame(current, state));
    // 回 ready、ping 也应答，但工具表是空的。
    let no_tools = format!(
        r#"read hello
{}
while read line; do
  id=$(printf '%s' "$line" | sed 's/.*"id":\([0-9]*\).*/\1/')
  case "$line" in
    *'"op":"ping"'*) printf '{{"frame":"reply","id":%s,"ok":true,"value":{{"identity":{{"pid":%s}}}}}}\n' "$id" "$$" ;;
    *) printf '{{"frame":"reply","id":%s,"ok":true,"value":{{"tools":[]}}}}\n' "$id" ;;
  esac
done"#,
        ready_frame(current, state)
    );
    let refuses = "read hello\nprintf '%s\\n' '{\"frame\":\"refused\",\"reason\":\"数据文件读不出来\"}'\nsleep 300".to_string();
    let cases: Vec<(&str, String, &str)> = vec![
        ("exits", "exit 3".into(), "管道关闭"),
        (
            "garbage",
            "echo hello-from-a-shell\nsleep 300".into(),
            "协议",
        ),
        ("hangs", "sleep 300".into(), "没有就绪"),
        ("refuses", refuses, "拒绝"),
        (
            "wrong-protocol",
            format!("read hello\n{}\nsleep 300", ready_frame(current + 1, state)),
            "协议",
        ),
        (
            "wrong-state",
            format!("read hello\n{}\nsleep 300", ready_frame(current, state + 1)),
            "状态格式",
        ),
        ("silent", silent, "不应答"),
        ("no-tools", no_tools, "少了当前代有的工具"),
    ];
    for (name, script, expected) in cases {
        let exe = fake_candidate(env, name, &script);
        let output = env.gld(&[
            "daemon",
            "upgrade",
            "prepare",
            exe.to_str().unwrap(),
            "--timeout",
            "2",
        ]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{name} 不该通过");
        assert!(stderr.contains(expected), "{name}：{stderr}");

        let status = fixture.status();
        assert_eq!(status["current"], "g1", "{name} 动了路由：{status}");
        assert!(status["prepared"].is_null(), "{name}：{status}");
        assert_eq!(generation_pids(&status), vec![g1], "{name}：{status}");
        assert!(
            status["last_failure"].as_str().is_some(),
            "{name} 没留下原因：{status}"
        );
        // 老服务接着干活，还是那一代。
        assert_eq!(fixture.runner_of("api"), served, "{name}");
    }

    // 不存在的路径同样如实报错。
    let missing = env.gld(&["daemon", "upgrade", "prepare", "/nonexistent/gld"]);
    assert!(!missing.status.success());
    assert_eq!(fixture.status()["current"], "g1");
    // 切换和放弃在没有新代时都拒绝，什么都不动。
    assert!(!env.gld(&["daemon", "upgrade", "switch"]).status.success());
    assert!(!env.gld(&["daemon", "upgrade", "abort"]).status.success());
    assert_eq!(fixture.runner_of("api"), served);
}

/// 准备好不等于切过去；放弃之后路由一点没变。
#[test]
fn a_prepared_version_gets_no_traffic_and_can_be_abandoned() {
    let fixture = fixture();
    let env = &fixture.env;
    let g1 = generation_pids(&fixture.status())[0];
    let exe = env!("CARGO_BIN_EXE_gld");
    env.ok(&["daemon", "upgrade", "prepare", exe]);
    let status = fixture.status();
    assert_eq!(status["prepared"], "g2", "{status}");
    let g2 = generation(&status, "g2");
    assert_eq!(g2["role"], "prepared");
    let prepared_pid = g2["pid"].as_u64().unwrap() as u32;
    assert_ne!(prepared_pid, g1);

    // 第二次 prepare 不能把第一个顶掉。
    assert!(!env
        .gld(&["daemon", "upgrade", "prepare", exe])
        .status
        .success());
    // 还没绑过的项目也进老的那一代。
    assert_eq!(fixture.runner_of("web"), g1);
    assert_eq!(
        generation(&fixture.status(), "g2")["in_flight"],
        0,
        "准备好的新代不该收到请求"
    );

    env.ok(&["daemon", "upgrade", "abort"]);
    wait_gone(prepared_pid);
    let status = fixture.status();
    assert!(status["prepared"].is_null(), "{status}");
    assert_eq!(status["current"], "g1", "{status}");
    assert_eq!(fixture.runner_of("api"), g1);
}

/// 切换：跨切换的长请求、命令会话和写入序号都留在原来那一代，
/// 新项目进新代。旧代的命令、stdin 都不受影响。
#[test]
fn switching_keeps_running_work_with_its_generation_and_sends_new_workspaces_to_the_new_one() {
    let fixture = fixture();
    let env = &fixture.env;
    let g1 = fixture.runner_of("api");

    // 交互命令：切换之后接着写 stdin、读输出、拿退出码。
    let interactive = fixture.tool(
        "exec_command",
        json!({
            "workspace": "api",
            "argv": ["sh", "-c", "read line; echo \"got-$line\"; exit 7"],
            "stdin_mode": "interactive",
            "yield_time_ms": 0,
            "timeout_ms": 120000,
        }),
    );
    let session = interactive["session_id"]
        .as_str()
        .unwrap_or_else(|| panic!("{interactive}"))
        .to_string();

    // 长请求：切换时还没返回。
    let long = {
        let (port, token) = (fixture.port, fixture.token.clone());
        std::thread::spawn(move || {
            let body = call_body(
                "exec_command",
                json!({ "workspace": "api", "argv": ["sh", "-c", "sleep 2; echo long-marker"], "yield_time_ms": 30000 }),
            );
            post_json(port, "/mcp", &body, Some(&token))
        })
    };
    wait_for("长请求进到执行代", || {
        generation(&fixture.status(), "g1")["in_flight"] == 1
    });

    let exe = env!("CARGO_BIN_EXE_gld");
    env.ok(&["daemon", "upgrade", "prepare", exe]);
    env.ok(&["daemon", "upgrade", "switch"]);

    let status = fixture.status();
    assert_eq!(status["current"], "g2", "{status}");
    let old = generation(&status, "g1");
    assert_eq!(old["role"], "draining", "{status}");
    assert!(old["blocked_by"]
        .as_array()
        .unwrap()
        .contains(&json!("owned_workspaces")));

    let finished = long.join().expect("长请求线程");
    let finished = structured(&finished);
    assert_eq!(finished["exit_code"], 0, "{finished}");
    assert!(
        finished["stdout"]
            .as_str()
            .is_some_and(|text| text.contains("long-marker")),
        "长请求要拿到自己的原响应：{finished}"
    );

    // 已绑定的项目还归老的，没绑过的进新的。
    let g2 = generation(&fixture.status(), "g2")["pid"].as_u64().unwrap() as u32;
    assert_eq!(fixture.runner_of("api"), g1);
    assert_eq!(fixture.runner_of("web"), g2);

    // 活动命令期间的写入，序号不漏记：写和命令归同一代。
    let patched = fixture.tool(
        "apply_patch",
        json!({
            "workspace": "api",
            "patch": "*** Begin Patch\n*** Add File: while-running.txt\n+hello\n*** End Patch\n",
        }),
    );
    assert_eq!(patched["ok"], true, "{patched}");
    let snapshot = fixture.tool(
        "write_stdin",
        json!({ "workspace": "api", "session_id": session, "chars": "after-switch\n", "yield_time_ms": 10000 }),
    );
    assert_eq!(snapshot["session_id"], session.as_str(), "{snapshot}");
    assert_eq!(snapshot["workspace_writes_since_start"], 1, "{snapshot}");
    assert!(
        snapshot["stdout"]
            .as_str()
            .is_some_and(|text| text.contains("got-after-switch")),
        "{snapshot}"
    );
    assert_eq!(snapshot["exit_code"], 7, "{snapshot}");

    // 老的还留着：它有工作区，这一期没有交接，不能杀。
    let status = fixture.status();
    assert!(alive(g1), "{status}");
    assert_eq!(generation(&status, "g1")["role"], "draining");
}

/// 切换前后一直有请求：同一个端口、同一个 token，没有一条因为切换报错。延迟预算没有验。
#[test]
fn requests_keep_succeeding_across_prepare_and_switch() {
    let fixture = fixture();
    fixture.env.write("hello.txt", "steady\n");
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let client = {
        let (port, token, stop) = (fixture.port, fixture.token.clone(), stop.clone());
        std::thread::spawn(move || {
            let body = call_body(
                "read_file",
                json!({ "workspace": "api", "path": "hello.txt" }),
            );
            let mut count = 0u32;
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                let reply = post_json(port, "/mcp", &body, Some(&token));
                assert_eq!(reply.status, 200, "{}", reply.body);
                assert!(reply.body.contains("steady"), "{}", reply.body);
                count += 1;
            }
            count
        })
    };
    for _ in 0..2 {
        std::thread::sleep(Duration::from_millis(200));
        fixture
            .env
            .ok(&["daemon", "upgrade", "prepare", env!("CARGO_BIN_EXE_gld")]);
        fixture.env.ok(&["daemon", "upgrade", "switch"]);
    }
    std::thread::sleep(Duration::from_millis(200));
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let count = client.join().expect("请求线程没有报错");
    assert!(count > 10, "这段时间只发出去 {count} 条，没覆盖到切换");
}

/// 切换时正在跑的带副作用调用只执行一次，回包对得上；新版本失败也不会让它再跑一遍。
#[test]
fn a_call_with_side_effects_runs_once_across_a_switch_and_a_failed_candidate() {
    let fixture = fixture();
    let env = &fixture.env;
    let call = {
        let (port, token) = (fixture.port, fixture.token.clone());
        std::thread::spawn(move || {
            let body = call_body(
                "exec_command",
                json!({
                    "workspace": "api",
                    "argv": ["sh", "-c", "sleep 2; echo once >> effect.log; echo effect-done"],
                    "yield_time_ms": 30000,
                }),
            );
            post_json(port, "/mcp", &body, Some(&token))
        })
    };
    wait_for("调用进到执行代", || {
        generation(&fixture.status(), "g1")["in_flight"] == 1
    });
    let bad = fake_candidate(env, "bad", "exit 3");
    assert!(!env
        .gld(&["daemon", "upgrade", "prepare", bad.to_str().unwrap()])
        .status
        .success());
    env.ok(&["daemon", "upgrade", "prepare", env!("CARGO_BIN_EXE_gld")]);
    env.ok(&["daemon", "upgrade", "switch"]);

    let reply = structured(&call.join().expect("请求线程"));
    assert!(
        reply["stdout"]
            .as_str()
            .is_some_and(|text| text.contains("effect-done")),
        "{reply}"
    );
    let effect = std::fs::read_to_string(env.project.path().join("effect.log")).unwrap();
    assert_eq!(effect, "once\n", "副作用不止一次");
    std::thread::sleep(Duration::from_millis(300));
    let effect = std::fs::read_to_string(env.project.path().join("effect.log")).unwrap();
    assert_eq!(effect, "once\n", "切换之后又跑了一遍");
}

/// 旧代没有任何工作区：切过去之后它自己退，不留进程。
#[test]
fn an_old_generation_with_nothing_to_finish_retires_after_the_switch() {
    let fixture = fixture();
    let env = &fixture.env;
    let g1 = generation_pids(&fixture.status())[0];
    env.ok(&["daemon", "upgrade", "prepare", env!("CARGO_BIN_EXE_gld")]);
    env.ok(&["daemon", "upgrade", "switch"]);
    wait_gone(g1);
    let status = fixture.status();
    assert_eq!(status["retired"], 1, "{status}");
    assert_eq!(generation_pids(&status).len(), 1, "{status}");
    fixture.wait_until_answering();
}

/// 代数到了上限就拒绝再准备，旧代一个不杀。
#[test]
fn the_generation_limit_refuses_new_ones_and_kills_nothing() {
    let fixture = fixture();
    let env = &fixture.env;
    let exe = env!("CARGO_BIN_EXE_gld");
    let g1 = fixture.runner_of("api");
    env.ok(&["daemon", "upgrade", "prepare", exe]);
    env.ok(&["daemon", "upgrade", "switch"]);
    let g2 = fixture.runner_of("web");
    assert_ne!(g2, g1);
    env.ok(&["daemon", "upgrade", "prepare", exe]);
    env.ok(&["daemon", "upgrade", "switch"]);
    assert_eq!(generation_pids(&fixture.status()).len(), 3);

    let refused = env.gld(&["daemon", "upgrade", "prepare", exe]);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success());
    assert!(stderr.contains("上限"), "{stderr}");
    assert!(alive(g1) && alive(g2));
    assert_eq!(generation_pids(&fixture.status()).len(), 3);
    assert_eq!(fixture.runner_of("api"), g1);
    assert_eq!(fixture.runner_of("web"), g2);
}

/// 报告只有计数和标识。
#[test]
fn the_status_report_carries_no_secret_or_tool_argument() {
    let fixture = fixture();
    let env = &fixture.env;
    let started = fixture.tool(
        "exec_command",
        json!({ "workspace": "api", "cmd": "sleep 33.1 # argument-marker", "yield_time_ms": 0, "timeout_ms": 60000 }),
    );
    let report = fixture.status().to_string();
    assert!(
        report.contains("\"active_commands\":1"),
        "命令数不对：{report}"
    );
    assert!(!report.contains(&fixture.token), "报告里有 token");
    assert!(
        !report.contains(&env.service_secret("oauth_password")),
        "报告里有口令"
    );
    assert!(
        !report.contains("argument-marker"),
        "报告里有工具参数：{report}"
    );
    assert!(
        !report.contains(started["session_id"].as_str().unwrap()),
        "报告里有 session id：{report}"
    );
    // 能力如实标着还没做到的，别让人从“能切换”推出“零中断”。
    let status = fixture.status();
    assert_eq!(status["capability"]["zero_interruption"], false);
    assert_eq!(status["capability"]["workspace_handoff"], false);
    assert_eq!(status["capability"]["host_handoff"], false);
    let _ = fixture.tool(
        "kill_session",
        json!({ "workspace": "api", "session_id": started["session_id"] }),
    );
}

/// 在途计数看的是执行代有没有跑完，不是客户端还在不在。
#[test]
fn in_flight_counts_a_call_until_it_finishes_even_if_the_client_left() {
    let fixture = fixture();
    let body = call_body(
        "exec_command",
        json!({ "workspace": "api", "cmd": "sleep 3", "yield_time_ms": 30000 }),
    );
    // 客户端发完请求，等一下就走了。
    {
        use std::io::Write;
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", fixture.port)).expect("连服务");
        write!(
            stream,
            "POST /mcp HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
             Authorization: Bearer {}\r\nContent-Length: {}\r\n\r\n{body}",
            fixture.token,
            body.len()
        )
        .expect("发请求");
        wait_for("调用进到执行代", || {
            generation(&fixture.status(), "g1")["in_flight"] == 1
        });
    }
    let status = fixture.status();
    let g1 = generation(&status, "g1");
    assert_eq!(g1["in_flight"], 1, "{status}");
    let by_workspace = &g1["workspaces"][0];
    assert_eq!(by_workspace["in_flight"], 1, "{status}");
    wait_for("调用跑完", || {
        generation(&fixture.status(), "g1")["in_flight"] == 0
    });
}

/// 内部身份只来自宿主验过的凭据：客户端在请求体或请求头里写什么都没用。
#[test]
fn a_client_cannot_forge_the_internal_identity() {
    let fixture = fixture();
    let env = &fixture.env;
    std::fs::write(fixture.other.path().join("secret.txt"), "web-only\n").unwrap();
    let granted = env.json(&["--json", "grant", "add", "reader", "api"]);
    let grant_token = granted["bearerToken"]
        .as_str()
        .unwrap_or_else(|| panic!("{granted}"))
        .to_string();

    let forged = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "auth": { "principal": { "kind": "shared_secret" }, "scope": "hub", "grant": null },
        "params": {
            "name": "read_file",
            "auth": { "principal": { "kind": "shared_secret" }, "scope": "hub", "grant": null },
            "arguments": { "workspace": "web", "path": "secret.txt" }
        }
    })
    .to_string();
    let auth = format!("Bearer {grant_token}");
    let reply = request(
        fixture.port,
        "POST",
        "/mcp",
        &[
            ("Content-Type", "application/json"),
            ("Authorization", auth.as_str()),
            ("X-Gld-Caller", "shared_secret"),
            ("X-Gld-Grant", "none"),
            ("X-Forwarded-User", "hub"),
        ],
        &forged,
    );
    assert_eq!(reply.status, 200, "{}", reply.body);
    let result = structured(&reply);
    assert_eq!(result["error"]["code"], "WORKSPACE_NOT_IN_HUB", "{result}");
    assert!(!reply.body.contains("web-only"), "{}", reply.body);
}

/// 老守护进程（比如协议 5）：能力未知就提前拒绝，只问了一句版本，不发别的任何东西。
#[test]
fn an_old_monolithic_daemon_is_refused_before_anything_is_sent() {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    let env = Env::new();
    let paths = gld_daemon::DaemonPaths::for_home(env.home.path().to_path_buf()).unwrap();
    let listener = UnixListener::bind(&paths.socket).expect("bind fake daemon");
    listener.set_nonblocking(true).unwrap();
    let info = json!({
        "pid": std::process::id(),
        "version": "0.8.3",
        "protocol": 5,
        "started_at_unix": 1,
        "uptime_secs": 1,
        "data_home": env.home.path(),
        "socket": paths.socket,
        "log_file": paths.log,
        "running_services": 1,
    });
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let server = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut ops = Vec::new();
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                let Ok((stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                ops.push(request["op"].as_str().unwrap().to_string());
                let mut stream = stream;
                let reply = json!({ "status": "ok", "result": info });
                writeln!(stream, "{reply}").unwrap();
            }
            ops
        })
    };

    for args in [
        vec!["daemon", "upgrade", "status"],
        vec!["daemon", "upgrade", "prepare", env!("CARGO_BIN_EXE_gld")],
        vec!["daemon", "upgrade", "switch"],
        vec!["daemon", "upgrade", "abort"],
    ] {
        let output = env.gld(&args);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{args:?}");
        assert!(stderr.contains("协议 5"), "{args:?}: {stderr}");
    }
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let ops = server.join().expect("fake daemon");
    assert!(
        ops.iter().all(|op| op == "daemon_info"),
        "只该问版本，不该发别的：{ops:?}"
    );
    assert!(!ops.is_empty());
    // Env 的 Drop 会 stop --force：它这里面对的 pid 是测试进程自己，别让它按 pid 动手。
    gld_daemon::lifecycle::cleanup_stale(&paths);
}

/// 换二进制的那个窗口：盘上的二进制已经是新构建了，当前代这时挂掉，按新的（兼容的）起，不卡死。
/// 不兼容的如实报，别带着对不上的状态格式继续写。
#[test]
fn a_replaced_incompatible_binary_is_reported_and_the_host_keeps_answering() {
    let env = Env::new();
    let exe_dir = tempfile::tempdir().expect("exe dir");
    let exe = exe_dir.path().join("gld");
    std::fs::copy(env!("CARGO_BIN_EXE_gld"), &exe).unwrap();
    let port = free_port();
    env.ok(&["add", ".", "--name", "api"]);
    env.ok(&["upgrade", "--port", &port.to_string(), "--auth", "bearer"]);
    let started = Command::new(&exe)
        .args(["start"])
        .env("GLD_HOME", env.home.path())
        .env("NO_COLOR", "1")
        .current_dir(env.project.path())
        .output()
        .expect("start");
    assert!(started.status.success(), "{started:?}");
    let token = env.service_secret("bearer_token");
    let status = env.json(&["--json", "daemon", "upgrade", "status"]);
    let host = status["host"]["pid"].as_u64().unwrap() as u32;

    // 换成一个坏的：就是 mv 换二进制，宿主还开着旧的。
    std::fs::remove_file(&exe).unwrap();
    std::fs::write(&exe, "#!/bin/sh\nexit 1\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    kill_9(generation_pids(&status)[0]);
    wait_for("记下失败原因", || {
        env.json(&["--json", "daemon", "upgrade", "status"])["last_failure"].is_string()
    });

    // 宿主和认证都还在；工具调用如实报不可用，没执行。
    let wrong = post_json(port, "/mcp", &rpc_body("tools/list", json!({})), Some("x"));
    assert_eq!(wrong.status, 401, "{}", wrong.body);
    let auth = format!("Bearer {token}");
    let reply = request_waiting(
        port,
        "POST",
        "/mcp",
        &[
            ("Content-Type", "application/json"),
            ("Authorization", auth.as_str()),
        ],
        &rpc_body("tools/list", json!({})),
        // 等替代的执行代最多 10 秒，然后如实回不可用。
        Duration::from_secs(20),
    );
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(
        reply.json()["error"]["data"]["reason"],
        "generation_unavailable",
        "{}",
        reply.body
    );
    let after = env.json(&["--json", "daemon", "status"]);
    assert_eq!(after["info"]["pid"].as_u64(), Some(u64::from(host)));
}

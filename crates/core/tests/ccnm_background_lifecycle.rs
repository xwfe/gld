//! 真实 gld 连真实 ccnm：远端后台命令什么时候会被连带杀掉。
//!
//! 跨仓评审 X04 是从两边源码推出来的因果链——**hub 的单次调用预算一到就丢连接，
//! 而 ccnm 的后台命令活不过连接**——当时没人跑过。这个文件跑一次，用真实的
//! `ccnm internal mcp-serve` 和真实的 `Connections`，不是合成 peer。
//!
//! **没有 ccnm 二进制就跳过**，CI 上没有。本机这么跑：
//!
//! ```text
//! (cd ../ccnm && cargo build --bin ccnm)
//! CCNM_BIN=../ccnm/target/debug/ccnm cargo test -p gld-core --test ccnm_background_lifecycle
//! ```
//!
//! 同一根管子上还有一条：ccnm P49 把项目 `.mcp.json` 里的 server 转给会话，
//! gld hub 的 `remote_call_mcp_tool` 就走这里（文件末尾）。
//!
//! 为什么不经过 `ccnm mcp bridge`：那条命令一定会 SSH 到另一台机器（它要求
//! node 有 ssh 别名且不是本机），本机测试没有那一跳。所以 `ccnm_bin` 指向一个
//! 包装脚本，把 bridge 的 argv 换成 SSH 那头真正跑的东西——同一个 `mcp-serve`、
//! 同一套工具、同一把写锁。

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use base64::Engine;
use gld_core::bridge::member::{CcnmMember, Mode};
use gld_core::bridge::session::{Connections, Spawn};
use serde_json::{json, Value};

const ANYONE: &str = "test-principal";

/// 哪来的 ccnm。没有就整组跳过。
fn ccnm_binary() -> Option<PathBuf> {
    if let Some(from_env) = std::env::var_os("CCNM_BIN") {
        let path = PathBuf::from(from_env);
        return path.is_file().then_some(path);
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../ccnm");
    ["debug", "release"]
        .iter()
        .map(|profile| repo.join("target").join(profile).join("ccnm"))
        .find(|candidate| candidate.is_file())
}

/// 一台"远端"：ccnm 的配置、一棵项目树，和那个替代 SSH 的包装脚本。
struct Remote {
    _dir: tempfile::TempDir,
    root: PathBuf,
    member: CcnmMember,
}

fn remote(ccnm: &Path) -> Remote {
    let dir = tempfile::tempdir().expect("临时目录");
    // 解析成真实路径：macOS 的 /var 是指向 /private/var 的符号链接，而 ccnm 的
    // 凭据检查对"路径可达性未知"是 fail-closed 的。
    let base = dir.path().canonicalize().expect("真实路径");
    let root = base.join("project");
    for sub in ["project", "home", "state"] {
        std::fs::create_dir_all(base.join(sub)).expect("建目录");
    }
    std::fs::write(root.join("hello.txt"), "one\n").expect("写文件");

    let config = base.join("config.toml");
    std::fs::write(
        &config,
        format!(
            r#"
this = "runtime"

[nodes.runtime]

[workspaces.demo]
root = "{root}"
agent_node = "runtime"
external_mcp = "coding"
allow_unconfined_exec = true
"#,
            root = root.display()
        ),
    )
    .expect("写配置");

    let script = base.join("fake-bridge.sh");
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
# gld 发过来的是 `mcp bridge demo --node runtime --mode <read|coding>`。
# 真 bridge 会 exec 成一条 ssh；本机没有那一跳，所以直接起 SSH 那头的东西。
set -e
case "$*" in
  *coding*) payload="{coding}" ;;
  *)        payload="{read}" ;;
esac
# env -i 把环境清干净：ccnm 的凭据边界检查看的是"执行身份能碰到什么"，
# 从开发机继承一整份环境过去，它会（正确地）拒绝开会话。真部署里 SSH
# 那头本来就是干净环境，这里只是把同一件事补上。
exec env -i PATH="$PATH" HOME="{home}" XDG_STATE_HOME="{state}" \
     CCNM_CONFIG="{config}" "{ccnm}" internal mcp-serve --payload "$payload"
"#,
            config = config.display(),
            home = base.join("home").display(),
            state = base.join("state").display(),
            coding = payload("coding"),
            read = payload("read"),
            ccnm = ccnm.display(),
        ),
    )
    .expect("写脚本");
    std::fs::set_permissions(
        &script,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
    )
    .expect("给脚本执行位");

    Remote {
        _dir: dir,
        root,
        member: CcnmMember {
            id: "remote".into(),
            name: "remote".into(),
            ccnm_bin: script.display().to_string(),
            node: "runtime".into(),
            workspace: "demo".into(),
            max_mode: Mode::Coding,
        },
    }
}

/// ccnm internal 协议 5 的 payload，和它自己的中立客户端测试拼法一样。
fn payload(mode: &str) -> String {
    let body = json!({
        "protocol": 5, "workspace": "demo",
        "session": format!("gld-x04-{mode}"), "mode": mode,
    });
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(body.to_string())
}

fn text_of(result: &Value) -> String {
    result["content"][0]["text"].as_str().unwrap_or("").into()
}

/// 等那个后台命令把自己的 pid 写出来。
fn wait_pid(root: &Path) -> i32 {
    let path = root.join("bg.pid");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(pid) = text.trim().parse() {
                return pid;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("后台命令一直没写出 pid");
}

fn alive(pid: i32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// 它现在的父进程。**留下来的孤儿 ppid 是 1**：起它的 ccnm 已经不在了。
fn ppid(pid: i32) -> String {
    std::process::Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_default()
}

fn gone_within(pid: i32, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// 起一个后台命令，返回它的 pid。
fn start_background(pool: &Connections, remote: &Remote, handle: &str) -> i32 {
    let started = pool
        .call_coding(
            ANYONE,
            &remote.member,
            handle,
            "exec_command",
            json!({ "shell": "echo $$ > bg.pid; exec sleep 60", "run_in_background": true }),
        )
        .expect("起后台命令");
    let text = text_of(&started);
    assert!(
        text.contains("running in the background as output_ref r-"),
        "{text}"
    );
    wait_pid(&remote.root)
}

/// **X04 的因果链，跑出来了**：hub 的单次调用预算一到就丢连接，ccnm 随即停掉
/// 这个会话起的所有后台命令。出事的是前台那条，陪葬的是后台那些。
///
/// 这里把调用预算调到 3 秒、前台命令睡 8 秒来复现；线上是 60 秒对 ccnm 默认的
/// 120 秒——**同一件事**，而且不给 `timeout_ms` 就必然撞上。修在 hub 那一层：
/// 前台命令的期限被压到调用预算以内（`tools::MAX_FOREGROUND_TIMEOUT_MS`），
/// 到点是远端杀掉命令并正常回话，连接和后台命令都还在。
#[test]
fn a_foreground_call_past_the_budget_takes_the_background_commands_with_it() {
    let Some(ccnm) = ccnm_binary() else {
        eprintln!("跳过：没找到 ccnm 二进制（设 CCNM_BIN，或先在 ccnm 仓 cargo build）");
        return;
    };
    let remote = remote(&ccnm);
    let pool = Connections::with_opener(Box::new(Spawn))
        .with_budgets(Duration::from_secs(300), Duration::from_secs(3));
    let handle = pool.begin_coding(ANYONE, &remote.member).expect("开会话");
    let pid = start_background(&pool, &remote, &handle);
    assert!(alive(pid), "后台命令该在跑");

    // 一条跑得比 hub 预算长的前台命令。远端会让它跑满 8 秒，hub 3 秒就放弃。
    let failed = pool
        .call_coding(
            ANYONE,
            &remote.member,
            &handle,
            "exec_command",
            json!({ "shell": "sleep 8", "timeout_ms": 8000 }),
        )
        .expect_err("hub 这边该超时");
    // 连接层只说"没在预算内答话"。这次调用做没做成是未知的，怎么把这件事
    // 讲给模型听是 hub 那一层的事（`REMOTE_OUTCOME_UNKNOWN`）。
    assert!(
        failed.to_string().contains("did not answer"),
        "该是超时：{failed}"
    );

    assert!(
        gone_within(pid, Duration::from_secs(25)),
        "连接被丢之后，远端该把这个会话起的后台命令停掉——pid {pid} 还在，ppid {}",
        ppid(pid)
    );
}

/// 反过来：前台命令在预算之内时，它跑完了后台那条还好好的。
///
/// 证明上面那条不是"起了后台命令就活不长"，而是**只有超预算才会连坐**。
#[test]
fn a_foreground_call_inside_the_budget_leaves_the_background_alone() {
    let Some(ccnm) = ccnm_binary() else {
        eprintln!("跳过：没找到 ccnm 二进制（设 CCNM_BIN，或先在 ccnm 仓 cargo build）");
        return;
    };
    let remote = remote(&ccnm);
    let pool = Connections::with_opener(Box::new(Spawn))
        .with_budgets(Duration::from_secs(300), Duration::from_secs(20));
    let handle = pool.begin_coding(ANYONE, &remote.member).expect("开会话");
    let pid = start_background(&pool, &remote, &handle);

    let done = pool
        .call_coding(
            ANYONE,
            &remote.member,
            &handle,
            "exec_command",
            json!({ "shell": "sleep 1; echo done", "timeout_ms": 5000 }),
        )
        .expect("前台命令该正常回来");
    assert!(text_of(&done).contains("\ndone\n"), "{}", text_of(&done));
    assert!(alive(pid), "后台命令不该受影响");

    // 会话正常结束时才轮到它：ccnm 在连接结束前停掉这条连接起的所有命令。
    pool.end_coding(ANYONE, &remote.member, &handle);
    assert!(
        gone_within(pid, Duration::from_secs(10)),
        "会话关了，后台命令该跟着停——pid {pid} 还在"
    );
}

/// 一个用 sh 写的 stdio MCP server：握手、列一个工具、调用时报出自己的 pid。
const FAKE_MCP_SERVER: &str = r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  [ -z "$id" ] && continue
  case "$line" in
    *'"initialize"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","serverInfo":{"name":"fake"}}}\n' "$id" ;;
    *'"tools/list"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"pid","inputSchema":{"type":"object"}}]}}\n' "$id" ;;
    *'"tools/call"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"%s"}]}}\n' "$id" "$$" ;;
  esac
done
"#;

/// ccnm P49 经真实的管子：项目 `.mcp.json` 里声明的 server 在 coding 会话里调得到，
/// 会话一结束（gld 这边关连接）它就被收掉——它能写工作树，得在写锁放掉之前走。
#[test]
fn a_projects_mcp_server_answers_through_the_real_pipe_and_goes_with_the_session() {
    let Some(ccnm) = ccnm_binary() else {
        eprintln!("跳过：没找到 ccnm 二进制（设 CCNM_BIN，或先在 ccnm 仓 cargo build）");
        return;
    };
    let remote = remote(&ccnm);
    let script = remote.root.join("fake-mcp.sh");
    std::fs::write(&script, FAKE_MCP_SERVER).expect("写 server");
    std::fs::set_permissions(
        &script,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
    )
    .expect("给执行位");
    std::fs::write(
        remote.root.join(".mcp.json"),
        r#"{ "mcpServers": { "fake": { "command": "./fake-mcp.sh" } } }"#,
    )
    .expect("写 .mcp.json");

    let pool = Connections::with_opener(Box::new(Spawn));
    let handle = pool.begin_coding(ANYONE, &remote.member).expect("开会话");
    let answered = pool
        .call_coding(
            ANYONE,
            &remote.member,
            &handle,
            "call_mcp_tool",
            json!({ "server": "fake", "tool": "pid" }),
        )
        .expect("调得通");
    let pid: i32 = text_of(&answered).trim().parse().expect("回的是 pid");
    assert!(alive(pid), "server 该在跑");

    pool.end_coding(ANYONE, &remote.member, &handle);
    assert!(
        gone_within(pid, Duration::from_secs(25)),
        "会话结束，server 该跟着走——pid {pid} 还在，ppid {}",
        ppid(pid)
    );
}

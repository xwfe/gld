//! 命令的运行记录（审查 D09）：输出和结局落在数据目录里，内存里找不到的会话从盘上读。
//!
//! 全走 `call_tool`，也就是客户端那条路。守护进程重启、`kill -9` 这两种要真进程的场景在
//! `crates/cli/tests/daemon_lifecycle.rs`。

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gld_core::tools::{call_tool, Caller, ToolContext};
use serde_json::{json, Value};

struct Fixture {
    ctx: ToolContext,
    workspace: PathBuf,
    _temp: tempfile::TempDir,
}

fn fixture() -> Fixture {
    common::isolate_data_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    fs::write(workspace.join("README.md"), "初始内容\n").expect("file");
    let ctx = ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("ctx");
    Fixture {
        ctx,
        workspace,
        _temp: temp,
    }
}

#[cfg(unix)]
fn script(workspace: &Path, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = workspace.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("script");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
}

fn session_id(output: &Value) -> String {
    output["session_id"]
        .as_str()
        .unwrap_or_else(|| panic!("没有 session_id：{output}"))
        .to_string()
}

fn read(ctx: &ToolContext, session: &str, offset: u64) -> Value {
    call_tool(
        ctx,
        "read_output",
        &json!({"output_ref": format!("session:{session}:stdout"), "offset": offset, "limit": 65536}),
    )
}

/// 这条命令在盘上的 `run.json`。
fn record_path(workspace: &Path, session: &str) -> PathBuf {
    let root = workspace.canonicalize().expect("canonical");
    gld_core::home::data_home()
        .expect("data home")
        .join("runs")
        .join(gld_core::tools::runs::project_id(&root))
        .join(session)
        .join("run.json")
}

/// `kill_session` 之后会话从内存表里拿掉了，以前再读报 `SESSION_EXPIRED`，停之前打的
/// 字也跟着没了。现在从运行记录读：结局是 killed，输出还在。
#[cfg(unix)]
#[test]
fn a_killed_command_is_still_readable_afterwards() {
    let fx = fixture();
    script(&fx.workspace, "tick", "echo 停之前打的字\nexec sleep 30");
    let started = call_tool(
        &fx.ctx,
        "exec_command",
        &json!({"cmd": "./tick", "yield_time_ms": 500, "timeout_ms": 60_000}),
    );
    let session = session_id(&started);
    assert_eq!(started["kept_on_disk"], true, "{started}");
    let killed = call_tool(
        &fx.ctx,
        "kill_session",
        &json!({"session_id": session, "wait_ms": 2000}),
    );
    assert_eq!(killed["killed"], true, "{killed}");

    let read = read(&fx.ctx, &session, 0);
    assert_eq!(read["ok"], true, "{read}");
    assert_eq!(read["source"], "run_record", "{read}");
    assert_eq!(read["termination_reason"], "killed", "{read}");
    assert_eq!(read["command_ok"], false, "{read}");
    assert_eq!(read["running"], false, "{read}");
    assert!(
        read["content"]
            .as_str()
            .is_some_and(|text| text.contains("停之前打的字")),
        "{read}"
    );
    // 已经停了的命令再 kill：如实说它早就结束了，不报错。
    let again = call_tool(&fx.ctx, "kill_session", &json!({"session_id": session}));
    assert_eq!(again["ok"], true, "{again}");
    assert_eq!(again["killed"], false, "{again}");
    assert_eq!(again["termination_reason"], "killed", "{again}");
    // 写 stdin：接不上了。
    let write = call_tool(
        &fx.ctx,
        "write_stdin",
        &json!({"session_id": session, "chars": "hi\n"}),
    );
    assert_eq!(write["error"]["code"], "SESSION_CLOSED", "{write}");
}

/// 转了后台的命令结束那一刻终态就落盘，不等谁来 `read_output`：之后 gld 重启了，
/// 这份记录就是唯一知道结局的地方。以前没人读就没人知道它结束了。
#[cfg(unix)]
#[test]
fn a_background_command_is_recorded_when_it_ends_not_when_it_is_read() {
    let fx = fixture();
    script(&fx.workspace, "quick", "echo done\nsleep 0.3\nexit 3");
    let started = call_tool(
        &fx.ctx,
        "exec_command",
        &json!({"cmd": "./quick", "yield_time_ms": 0, "timeout_ms": 30_000}),
    );
    let session = session_id(&started);
    let path = record_path(&fx.workspace, &session);
    let deadline = Instant::now() + Duration::from_secs(5);
    let record = loop {
        let record: Value = serde_json::from_slice(&fs::read(&path).expect("run.json 该在"))
            .expect("run.json 是 JSON");
        if record["status"] != "running" || Instant::now() >= deadline {
            break record;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(record["status"], "exited", "{record}");
    assert_eq!(record["exit_code"], 3, "{record}");
    assert_eq!(record["command"], "./quick", "{record}");
    assert_eq!(record["workspace_writes_during_run"], 0, "{record}");
}

/// 别的主体拿我的 session_id 去读运行记录，和读内存里的会话一样：查无此人。
#[cfg(unix)]
#[test]
fn another_caller_cannot_read_my_run_record() {
    use gld_core::auth::{AuthContext, Principal};
    use gld_core::tools::call_tool_as;
    let fx = fixture();
    script(&fx.workspace, "tick", "echo 只给我看\nexec sleep 30");
    let started = call_tool(
        &fx.ctx,
        "exec_command",
        &json!({"cmd": "./tick", "yield_time_ms": 300, "timeout_ms": 60_000}),
    );
    let session = session_id(&started);
    call_tool(&fx.ctx, "kill_session", &json!({"session_id": session}));

    let stranger = Caller::from_auth(&AuthContext::new(
        Principal::OAuthClient {
            client_id: "stranger".into(),
        },
        "hub",
    ));
    let theirs = call_tool_as(
        &fx.ctx,
        &stranger,
        "read_output",
        &json!({"output_ref": format!("session:{session}:stdout")}),
    );
    assert_eq!(theirs["error"]["code"], "SESSION_NOT_FOUND", "{theirs}");
    assert!(
        !theirs.to_string().contains("只给我看"),
        "别人的输出漏出去了：{theirs}"
    );
    // 拿路径穿越当 id：也只是查无此人。
    let escape = call_tool(
        &fx.ctx,
        "read_output",
        &json!({"output_ref": "session:../../runs:stdout"}),
    );
    assert_eq!(escape["error"]["code"], "SESSION_NOT_FOUND", "{escape}");
}

/// 输出比盘上留的多（每个流留最后 1～2 MiB）时，从记录读也按整条流的绝对偏移分页，
/// 前面没留下的如实报 `dropped_bytes`，一路读到 `complete`。
#[cfg(unix)]
#[test]
fn a_long_output_pages_from_the_record_by_absolute_offset() {
    let fx = fixture();
    // 约 3.1 MiB：每行 33 字节（`line ` + 27 位数字 + 换行），10 万行。
    script(
        &fx.workspace,
        "flood",
        "i=0\nwhile [ $i -lt 100000 ]; do printf 'line %027d\\n' $i; i=$((i+1)); done",
    );
    let started = call_tool(
        &fx.ctx,
        "exec_command",
        &json!({"cmd": "./flood", "yield_time_ms": 30_000, "timeout_ms": 60_000, "max_output_bytes": 256}),
    );
    assert_eq!(started["termination_reason"], "exited", "{started}");
    let session = session_id(&started);
    // 结束了的会话 kill 一下就从内存表里拿掉了，后面只能从记录读。
    call_tool(&fx.ctx, "kill_session", &json!({"session_id": session}));

    let first = read(&fx.ctx, &session, 0);
    assert_eq!(first["source"], "run_record", "{first}");
    let total = first["total_stream_bytes"].as_u64().expect("total");
    assert_eq!(total, 100_000 * 33);
    let retained_from = first["retained_from"].as_u64().expect("retained_from");
    assert!(retained_from > 0, "3.1 MiB 的输出不该全留着：{first}");
    assert!(
        total - retained_from >= 1 << 20,
        "至少留最后 1 MiB：{first}"
    );
    assert_eq!(first["dropped_bytes"], retained_from);

    let mut offset = retained_from;
    let mut last_line = String::new();
    for _ in 0..100 {
        let page = read(&fx.ctx, &session, offset);
        assert_eq!(page["offset"], offset, "{page}");
        if let Some(text) = page["content"].as_str() {
            if let Some(line) = text.lines().rfind(|line| !line.is_empty()) {
                last_line = line.to_string();
            }
        }
        match page["next_offset"].as_u64() {
            Some(next) => offset = next,
            None => {
                assert_eq!(page["complete"], true, "{page}");
                break;
            }
        }
    }
    assert_eq!(last_line, format!("line {:027}", 99_999));
}

/// 开任务、起一条后台命令、等它结束（结局落盘），返回 (任务 id, session_id)。
fn task_with_a_finished_background_command(fx: &Fixture, cmd: &str) -> (String, String) {
    let started = call_tool(
        &fx.ctx,
        "task_manage",
        &json!({"action": "start", "objective": "验收"}),
    );
    let task_id = started["task"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("没开成任务：{started}"))
        .to_string();
    let ran = call_tool(
        &fx.ctx,
        "exec_command",
        &json!({"cmd": cmd, "yield_time_ms": 0, "timeout_ms": 30_000}),
    );
    let session = session_id(&ran);
    let path = record_path(&fx.workspace, &session);
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let done = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some_and(|record| record["status"] == "exited");
        if done {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    (task_id, session)
}

/// 把会话从内存表里拿掉，**不经过 call_tool**：经 call_tool 的 kill_session 本身就会被任务
/// 那边"看到结局"，之后再从记录读就测不到记录那条路了（独立审查发现原来的写法有这个问题）。
fn forget_in_memory(fx: &Fixture, session: &str) {
    let store = fx.ctx.runtime.sessions_for(&Caller::local());
    gld_core::tools::session::kill_session(&store, &json!({"session_id": session}))
        .expect("从内存拿掉");
}

fn finish(fx: &Fixture, task_id: &str, session: &str) -> Value {
    call_tool(
        &fx.ctx,
        "task_manage",
        &json!({"action": "finish", "task_id": task_id, "evidence_session_ids": [session]}),
    )
}

/// 从记录读到的结局照样能当任务验收证据：读到那一刻补进任务事件，和从内存读一样。
#[test]
fn a_result_read_from_the_record_still_counts_as_task_evidence() {
    let fx = fixture();
    let (task_id, session) = task_with_a_finished_background_command(&fx, "git --version");
    forget_in_memory(&fx, &session);
    let read = read(&fx.ctx, &session, 0);
    assert_eq!(read["source"], "run_record", "{read}");
    assert_eq!(read["command_ok"], true, "{read}");
    assert_eq!(read["workspace_writes_since_start"], 0, "{read}");

    let finished = finish(&fx, &task_id, &session);
    assert_eq!(finished["task"]["status"], "completed", "{finished}");
}

/// 命令跑完之后 gld 又改了源文件，再从记录读到"退出 0"：不能当现在这份代码的证据。
///
/// 独立审查发现的：记录里的写入次数在命令结束那一刻就定死了（0），任务基线又跟着那次改动
/// 更新了，于是"退出 0、期间没写过、结束时的工作区等于现在"三项全过，测旧代码的结果被收成
/// completed。内存那条路每次按当前计数器重算，一直是拒收的。
#[test]
fn a_record_does_not_hide_writes_made_after_the_command_ended() {
    let fx = fixture();
    let (task_id, session) = task_with_a_finished_background_command(&fx, "git --version");
    let patched = call_tool(
        &fx.ctx,
        "apply_patch",
        &json!({"patch": "--- a/README.md\n+++ b/README.md\n@@\n-初始内容\n+改过了\n"}),
    );
    assert_eq!(patched["ok"], true, "{patched}");
    forget_in_memory(&fx, &session);
    let read = read(&fx.ctx, &session, 0);
    assert_eq!(read["source"], "run_record", "{read}");
    assert_eq!(read["workspace_writes_since_start"], 1, "{read}");

    let finished = finish(&fx, &task_id, &session);
    assert_eq!(
        finished["error"]["code"], "VERIFICATION_REJECTED",
        "{finished}"
    );
    assert!(
        finished.to_string().contains("EVIDENCE_STALE"),
        "{finished}"
    );
}

/// 拦着 TERM 的命令，停的时候补一个 KILL：不然 gld 退出后它成了没人管的孤儿，结局也只能
/// 记成 unknown（独立审查发现）。
#[cfg(unix)]
#[test]
fn a_command_that_ignores_term_is_still_stopped() {
    let fx = fixture();
    script(
        &fx.workspace,
        "stubborn",
        "trap '' TERM\necho ready\nwhile :; do sleep 1; done",
    );
    let started = call_tool(
        &fx.ctx,
        "exec_command",
        &json!({"cmd": "./stubborn", "yield_time_ms": 500, "timeout_ms": 60_000}),
    );
    let session = session_id(&started);
    assert_eq!(fx.ctx.runtime.terminate_all_sessions(), 1);
    let read = read(&fx.ctx, &session, 0);
    assert_eq!(read["termination_reason"], "killed", "{read}");
    assert_eq!(read["running"], false, "{read}");
    let record: Value =
        serde_json::from_slice(&fs::read(record_path(&fx.workspace, &session)).expect("run.json"))
            .expect("json");
    assert_eq!(record["status"], "killed", "{record}");
}

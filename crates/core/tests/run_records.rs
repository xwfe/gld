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
    // list_runs 只看日志文件名和大小算总字节数，分过段、前面的段删掉了也得对得上。
    let listed = call_tool(&fx.ctx, "list_runs", &json!({}));
    assert_eq!(listed["runs"][0]["stdout_bytes"], total, "{listed}");

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

// ---- list_runs：不知道 session_id 时找回自己的命令 ----

fn list(ctx: &ToolContext, args: Value) -> Value {
    let listed = call_tool(ctx, "list_runs", &args);
    assert_eq!(listed["ok"], true, "{listed}");
    listed
}

fn listed_ids(listed: &Value) -> Vec<String> {
    listed["runs"]
        .as_array()
        .unwrap_or_else(|| panic!("没有 runs：{listed}"))
        .iter()
        .map(|run| run["session_id"].as_str().expect("id").to_string())
        .collect()
}

/// 跑一条前台命令等它结束，返回 session_id。
fn run_to_end(fx: &Fixture, cmd: &str) -> String {
    let ran = call_tool(
        &fx.ctx,
        "exec_command",
        &json!({"cmd": cmd, "yield_time_ms": 10_000, "timeout_ms": 30_000}),
    );
    assert_eq!(ran["termination_reason"], "exited", "{ran}");
    session_id(&ran)
}

/// 以一条真记录为底，在同一个项目里造一条改过字段的记录。用来摆出正常流程里要靠 `kill -9`、
/// 等 7 天才出得来的状态。
fn forge_record(fx: &Fixture, template: &str, edit: impl FnOnce(&mut Value)) -> String {
    let source = record_path(&fx.workspace, template);
    let mut record: Value =
        serde_json::from_slice(&fs::read(&source).expect("run.json")).expect("json");
    let id = uuid::Uuid::new_v4().to_string();
    record["session_id"] = json!(id);
    edit(&mut record);
    let dir = source.parent().unwrap().parent().unwrap().join(&id);
    fs::create_dir_all(&dir).expect("dir");
    fs::write(dir.join("run.json"), serde_json::to_vec(&record).unwrap()).expect("write");
    id
}

/// 换了对话、忘了 id，也能列出自己跑过的命令：新的在前，给的 `output_refs` 直接能交给
/// `read_output`，每个流写了多少字节和读出来的对得上。
#[cfg(unix)]
#[test]
fn my_runs_are_listed_newest_first_and_their_refs_read_back() {
    let fx = fixture();
    script(&fx.workspace, "ok", "echo 第一条");
    script(
        &fx.workspace,
        "fail",
        "echo 第二条\necho 出错了 >&2\nexit 4",
    );
    let first = run_to_end(&fx, "./ok");
    let second = run_to_end(&fx, "./fail");

    let listed = list(&fx.ctx, json!({}));
    assert_eq!(listed["total"], 2, "{listed}");
    assert_eq!(listed_ids(&listed), vec![second.clone(), first.clone()]);
    assert_eq!(listed["next_cursor"], Value::Null, "{listed}");

    let newest = &listed["runs"][0];
    assert_eq!(newest["command"], "./fail", "{newest}");
    assert_eq!(newest["termination_reason"], "exited", "{newest}");
    assert_eq!(newest["exit_code"], 4, "{newest}");
    assert_eq!(newest["command_ok"], false, "{newest}");
    assert_eq!(newest["running"], false, "{newest}");
    assert_eq!(newest["started_by_this_gld"], true, "{newest}");
    assert_eq!(newest["workspace_writes_since_start"], 0, "{newest}");
    assert_eq!(newest["stderr_bytes"], "出错了\n".len(), "{newest}");
    // 摘要，不是输出：列表里不带输出正文。
    assert!(!listed.to_string().contains("第二条"), "{listed}");

    let stdout_ref = newest["output_refs"]["stdout"].as_str().expect("ref");
    let read = call_tool(&fx.ctx, "read_output", &json!({"output_ref": stdout_ref}));
    assert_eq!(read["content"], "第二条\n", "{read}");
    assert_eq!(read["total_stream_bytes"], newest["stdout_bytes"], "{read}");
}

/// 别的主体一条也看不到，连"有几条"都看不到：`total` 是按主体过滤之后才数的。
#[cfg(unix)]
#[test]
fn another_caller_lists_none_of_my_runs_not_even_a_count() {
    use gld_core::auth::{AuthContext, Principal};
    use gld_core::tools::call_tool_as;
    let fx = fixture();
    script(&fx.workspace, "ok", "echo 只给我看");
    run_to_end(&fx, "./ok");

    let stranger = Caller::from_auth(&AuthContext::new(
        Principal::OAuthClient {
            client_id: "stranger".into(),
        },
        "hub",
    ));
    let theirs = call_tool_as(&fx.ctx, &stranger, "list_runs", &json!({}));
    assert_eq!(theirs["ok"], true, "{theirs}");
    assert_eq!(theirs["total"], 0, "{theirs}");
    assert_eq!(theirs["runs"], json!([]), "{theirs}");
    assert!(!theirs.to_string().contains("./ok"), "{theirs}");
}

/// 翻页中途又起了新命令：新的排在最前面，下一页照旧从游标往后接，不重不漏。
/// 游标指的那条被清掉了也接得上，只是多一条提示。
#[cfg(unix)]
#[test]
fn paging_survives_new_runs_and_a_cleaned_up_cursor() {
    let fx = fixture();
    script(&fx.workspace, "ok", "echo hi");
    let oldest = run_to_end(&fx, "./ok");
    let middle = run_to_end(&fx, "./ok");
    let newest = run_to_end(&fx, "./ok");

    let page1 = list(&fx.ctx, json!({"limit": 2}));
    assert_eq!(listed_ids(&page1), vec![newest.clone(), middle.clone()]);
    assert_eq!(page1["total"], 3, "{page1}");
    let cursor = page1["next_cursor"].as_str().expect("有下一页").to_string();

    let later = run_to_end(&fx, "./ok");
    let page2 = list(&fx.ctx, json!({"limit": 2, "cursor": cursor}));
    assert_eq!(listed_ids(&page2), vec![oldest.clone()], "{page2}");
    assert_eq!(page2["next_cursor"], Value::Null, "{page2}");
    assert_eq!(page2["warnings"], json!([]), "{page2}");
    assert_eq!(listed_ids(&list(&fx.ctx, json!({"limit": 1})))[0], later);

    // 游标那条没了（被配额或年龄清掉）：从它原来的位置接着往后，提示一句。
    fs::remove_dir_all(record_path(&fx.workspace, &middle).parent().unwrap()).expect("rm");
    let page2 = list(&fx.ctx, json!({"limit": 2, "cursor": cursor}));
    assert_eq!(listed_ids(&page2), vec![oldest], "{page2}");
    assert_eq!(
        page2["warnings"].as_array().map(Vec::len),
        Some(1),
        "{page2}"
    );
}

/// 起它的 gld 进程已经不在了、记录还停在 running（`kill -9` 之后就是这样）：列出来是
/// `unknown`，不说成也不说败；列表不发任何信号，那个 pid 上的进程照样活着。
#[cfg(unix)]
#[test]
fn a_run_whose_gld_is_gone_is_listed_as_unknown_and_left_alone() {
    let fx = fixture();
    script(&fx.workspace, "ok", "echo hi");
    let template = run_to_end(&fx, "./ok");
    let mut bystander = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("sleep");
    let pid = bystander.id();
    let orphan = forge_record(&fx, &template, |record| {
        record["status"] = json!("running");
        record["exit_code"] = Value::Null;
        record["finished_at_ms"] = Value::Null;
        record["owner"] = json!("gld-instance-that-is-gone");
        record["pid"] = json!(pid);
    });

    let listed = list(&fx.ctx, json!({"status": ["unknown"]}));
    assert_eq!(listed_ids(&listed), vec![orphan.clone()], "{listed}");
    let run = &listed["runs"][0];
    assert_eq!(run["command_ok"], Value::Null, "{run}");
    assert_eq!(run["started_by_this_gld"], false, "{run}");
    assert_eq!(run["workspace_writes_since_start"], Value::Null, "{run}");
    // 结论写回了记录：下一个读的人看到的是同一个。
    let record: Value =
        serde_json::from_slice(&fs::read(record_path(&fx.workspace, &orphan)).unwrap()).unwrap();
    assert_eq!(record["status"], "unknown", "{record}");
    assert_eq!(
        bystander.try_wait().expect("wait"),
        None,
        "列表把别人的进程停了"
    );
    let _ = bystander.kill();
    let _ = bystander.wait();
}

/// 本进程起的、记录还是 running、内存表里却没有：结束时写记录失败了或者会话表被扔掉了。
/// 起跑超过几秒的判 unknown，和 `read_output` 说法一致；刚起的不动——`insert` 先建记录、
/// 后放进内存表，列表碰巧夹在中间时不能把一条刚起的命令写成 unknown。
#[cfg(unix)]
#[test]
fn this_processs_detached_running_record_is_unknown_unless_just_started() {
    let fx = fixture();
    script(&fx.workspace, "ok", "echo hi");
    let template = run_to_end(&fx, "./ok");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let mine = |started_at: u64| {
        forge_record(&fx, &template, |record| {
            record["status"] = json!("running");
            record["exit_code"] = Value::Null;
            record["finished_at_ms"] = Value::Null;
            record["started_at_ms"] = json!(started_at);
            record["owner"] = json!(gld_core::tools::runs::instance_id());
        })
    };
    let starting = mine(now);
    let detached = mine(now - 60_000);

    let running = list(&fx.ctx, json!({"status": ["running"]}));
    assert_eq!(listed_ids(&running), vec![starting.clone()], "{running}");
    let unknown = list(&fx.ctx, json!({"status": ["unknown"]}));
    assert_eq!(listed_ids(&unknown), vec![detached.clone()], "{unknown}");
    let read = read(&fx.ctx, &detached, 0);
    assert_eq!(read["termination_reason"], "unknown", "{read}");
    let record: Value =
        serde_json::from_slice(&fs::read(record_path(&fx.workspace, &starting)).unwrap()).unwrap();
    assert_eq!(record["status"], "running", "刚起的被写成了 {record}");
}

/// 被 kill 了、进程却还没退（拦着 TERM）：原因已经标成 killed，但它还在跑。列表跟
/// `read_output` 一样说 `running: true`、不说成败，按 running 筛找得到它。
/// 这时 kill_session 正拿着子进程锁等它退出，列表也不能被卡住。
#[cfg(unix)]
#[test]
fn a_run_being_stopped_is_still_running_and_does_not_block_the_list() {
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
    let terminating = call_tool(
        &fx.ctx,
        "kill_session",
        &json!({"session_id": session, "signal": "TERM", "wait_ms": 200}),
    );
    assert_eq!(terminating["status"], "terminating", "{terminating}");

    let listed = list(&fx.ctx, json!({"status": ["running"]}));
    assert_eq!(listed_ids(&listed), vec![session.clone()], "{listed}");
    let run = &listed["runs"][0];
    assert_eq!(run["running"], true, "{run}");
    assert_eq!(run["command_ok"], Value::Null, "{run}");
    // 盘上记录还是 running；"正在被停"这件事只有内存里知道。
    assert_eq!(run["termination_reason"], "killed", "{run}");

    // 另一个线程拿着子进程锁等它退出（最长 3 秒），列表照样马上回来。
    std::thread::scope(|scope| {
        let waiting = scope.spawn(|| {
            call_tool(
                &fx.ctx,
                "kill_session",
                &json!({"session_id": session, "signal": "TERM", "wait_ms": 3000}),
            )
        });
        std::thread::sleep(Duration::from_millis(300));
        let begun = Instant::now();
        list(&fx.ctx, json!({"status": ["exited"]}));
        assert!(
            begun.elapsed() < Duration::from_millis(1500),
            "列表被正在进行的 kill 卡了 {:?}",
            begun.elapsed()
        );
        waiting.join().expect("kill 线程");
    });
    call_tool(
        &fx.ctx,
        "kill_session",
        &json!({"session_id": session, "signal": "KILL", "wait_ms": 2000}),
    );
}

/// 同一毫秒里起了几条（脚本连着跑很容易）：按 id 排次序，一条一条翻也不重不漏。
#[cfg(unix)]
#[test]
fn paging_within_one_millisecond_is_ordered_by_id() {
    let fx = fixture();
    script(&fx.workspace, "ok", "echo hi");
    let template = run_to_end(&fx, "./ok");
    // 一分钟前：比 template 早，又在 7 天保留期里。
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        - 60_000;
    let mut same: Vec<String> = (0..3)
        .map(|_| {
            forge_record(&fx, &template, |record| {
                record["started_at_ms"] = json!(at);
                record["finished_at_ms"] = json!(at + 1);
            })
        })
        .collect();
    same.sort_by(|a, b| b.cmp(a));

    let mut seen = Vec::new();
    let mut cursor = Value::Null;
    loop {
        let page = list(&fx.ctx, json!({"limit": 1, "cursor": cursor}));
        seen.extend(listed_ids(&page));
        cursor = page["next_cursor"].clone();
        if cursor.is_null() {
            break;
        }
    }
    let mut expected = vec![template];
    expected.extend(same);
    assert_eq!(seen, expected);
}

/// 结束超过 7 天、还没轮到清理的不列出来：按保留规矩它已经不在了。时间窗口按起跑时间筛。
#[cfg(unix)]
#[test]
fn old_runs_are_not_listed_and_the_time_window_filters() {
    let fx = fixture();
    script(&fx.workspace, "ok", "echo hi");
    let fresh = run_to_end(&fx, "./ok");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let day = 24 * 60 * 60 * 1000;
    let stale = forge_record(&fx, &fresh, |record| {
        record["started_at_ms"] = json!(now - 8 * day);
        record["finished_at_ms"] = json!(now - 8 * day + 1000);
    });
    let hours_ago = forge_record(&fx, &fresh, |record| {
        record["started_at_ms"] = json!(now - 3 * 60 * 60 * 1000);
        record["finished_at_ms"] = json!(now - 3 * 60 * 60 * 1000 + 1000);
    });

    let all = listed_ids(&list(&fx.ctx, json!({})));
    assert_eq!(all, vec![fresh.clone(), hours_ago], "{all:?}");
    assert!(!all.contains(&stale));
    let recent = listed_ids(&list(&fx.ctx, json!({"started_within_minutes": 60})));
    assert_eq!(recent, vec![fresh]);
}

/// 参数写错直接报错，不悄悄当成"不筛"：筛错了的空列表会被当成"没跑过"。
#[test]
fn bad_list_runs_arguments_are_refused() {
    let fx = fixture();
    for args in [
        json!({"status": ["done"]}),
        json!({"status": "running"}),
        json!({"started_within_minutes": 0}),
        json!({"started_within_minutes": 20_000}),
        json!({"cursor": "not-a-cursor"}),
        json!({"status": []}),
        json!({"cursor": "1:0F8FAD5B-D9CB-469F-A165-70867728950E"}),
        json!({"cursor": "1:0f8fad5bd9cb469fa16570867728950e"}),
        json!({"session_id": "x"}),
    ] {
        let refused = call_tool(&fx.ctx, "list_runs", &args);
        assert_eq!(
            refused["error"]["code"], "INVALID_ARGUMENT",
            "{args} → {refused}"
        );
    }
    let empty = list(&fx.ctx, json!({}));
    assert_eq!(empty["total"], 0, "{empty}");
    assert_eq!(empty["records_kept"], true, "{empty}");
}

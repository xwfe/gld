//! Harness 存储碰上并发、写到一半、文件损坏时的行为（审查 D11）。
//!
//! 全走 `call_tool`，也就是客户端那条路。坏数据是直接往 Harness 数据目录里写出来的
//! （故障注入），不代表线上已经出过这种事。

mod common;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Barrier;
use std::thread;

use gld_core::tools::{call_tool, ToolContext};
use serde_json::{json, Value};

/// 一定退出 0、会真起一个子进程（有 session_id）的命令。
const PASSING: &str = "git --version";

struct Fixture {
    workspace: PathBuf,
    harness_root: PathBuf,
    _temp: tempfile::TempDir,
}

fn fixture() -> Fixture {
    common::isolate_data_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    fs::write(workspace.join("README.md"), "初始内容\n").expect("file");
    Fixture {
        workspace,
        harness_root: temp.path().join("harness"),
        _temp: temp,
    }
}

/// 一个新的上下文。同一个工作区开几个，就是几条客户端连接（或者重启之后的那个）。
fn connect(fx: &Fixture) -> ToolContext {
    ToolContext::for_test(fx.workspace.clone(), fx.harness_root.clone()).expect("ctx")
}

/// 这个工作区在 Harness 数据目录里的那一格。
fn store_dir(fx: &Fixture, ctx: &ToolContext) -> PathBuf {
    fx.harness_root
        .join("workspaces")
        .join(ctx.harness.workspace_id())
}

fn task(ctx: &ToolContext, action: &str, extra: Value) -> Value {
    let mut args = json!({"action": action});
    for (key, value) in extra.as_object().expect("object") {
        args[key] = value.clone();
    }
    call_tool(ctx, "task_manage", &args)
}

fn start(ctx: &ToolContext) -> String {
    let started = task(ctx, "start", json!({"objective": "存储"}));
    assert_eq!(started["ok"], true, "{started}");
    started["task"]["id"].as_str().expect("task id").to_string()
}

fn append_raw(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    file.write_all(bytes).expect("append");
}

fn open_tasks_on_disk(dir: &Path) -> usize {
    fs::read_dir(dir.join("tasks"))
        .expect("tasks dir")
        .filter_map(|entry| {
            let path = entry.expect("entry").path();
            (path.extension().and_then(|s| s.to_str()) == Some("json")).then_some(path)
        })
        .filter(|path| {
            let task: Value = serde_json::from_slice(&fs::read(path).expect("read")).expect("json");
            matches!(
                task["status"].as_str(),
                Some("active" | "paused" | "verifying" | "failed")
            )
        })
        .count()
}

#[test]
fn parallel_starts_leave_exactly_one_open_task() {
    const CLIENTS: usize = 8;
    let fx = fixture();
    let barrier = Barrier::new(CLIENTS);
    let results: Vec<Value> = thread::scope(|scope| {
        let handles: Vec<_> = (0..CLIENTS)
            .map(|i| {
                let (fx, barrier) = (&fx, &barrier);
                scope.spawn(move || {
                    let ctx = connect(fx);
                    barrier.wait();
                    task(&ctx, "start", json!({"objective": format!("并发 {i}")}))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("join"))
            .collect()
    });

    let started = results.iter().filter(|r| r["ok"] == true).count();
    assert_eq!(started, 1, "只能有一个开成：{results:#?}");
    for refused in results.iter().filter(|r| r["ok"] != true) {
        assert_eq!(refused["error"]["code"], "TASK_ALREADY_ACTIVE", "{refused}");
    }
    let ctx = connect(&fx);
    assert_eq!(open_tasks_on_disk(&store_dir(&fx, &ctx)), 1);
}

#[test]
fn parallel_updates_do_not_undo_each_other() {
    let fx = fixture();
    let (left, right) = (connect(&fx), connect(&fx));
    let task_id = start(&left);
    let barrier = Barrier::new(2);
    // 一边只改 completed_steps，一边只改 pending_steps。两次读改写没排队的话，
    // 后写的那个拿的是旧任务，会把对方刚写的那一格改回去。
    for round in 0..30 {
        thread::scope(|scope| {
            scope.spawn(|| {
                barrier.wait();
                let done = task(
                    &left,
                    "update",
                    json!({"task_id": task_id, "completed_steps": [format!("a{round}")]}),
                );
                assert_eq!(done["ok"], true, "{done}");
            });
            scope.spawn(|| {
                barrier.wait();
                let done = task(
                    &right,
                    "update",
                    json!({"task_id": task_id, "pending_steps": [format!("b{round}")]}),
                );
                assert_eq!(done["ok"], true, "{done}");
            });
        });
        let now = task(&left, "context", json!({"task_id": task_id}));
        assert_eq!(
            now["task"]["completed_steps"],
            json!([format!("a{round}")]),
            "第 {round} 轮：{now}"
        );
        assert_eq!(
            now["task"]["pending_steps"],
            json!([format!("b{round}")]),
            "第 {round} 轮：{now}"
        );
    }
}

#[test]
fn a_bad_event_line_hides_only_itself_and_later_evidence_still_counts() {
    let fx = fixture();
    let ctx = connect(&fx);
    let task_id = start(&ctx);
    let events = store_dir(&fx, &ctx)
        .join("events")
        .join(format!("{task_id}.jsonl"));
    // 第 2 行不是 UTF-8，第 3 行是半截 JSON。
    append_raw(&events, b"\xff\xfe not text\n{\"id\": \"cut\n");

    let passed = exec_session(&ctx);
    let done = task(
        &ctx,
        "finish",
        json!({"task_id": task_id, "evidence_session_ids": [passed]}),
    );
    assert_eq!(done["ok"], true, "坏行后面的证据应该还读得到：{done}");
    assert_eq!(done["task"]["status"], "completed");
    let warnings = done["warnings"].to_string();
    assert!(warnings.contains("2 行"), "要说有几行没读出来：{done}");

    let listed = task(&ctx, "events", json!({"task_id": task_id, "limit": 100}));
    assert_eq!(listed["ok"], true, "{listed}");
    let kinds: Vec<&str> = listed["events"]
        .as_array()
        .expect("events")
        .iter()
        .map(|event| event["kind"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(kinds.first(), Some(&"task_started"), "{listed}");
    assert!(kinds.contains(&"verification_recorded"), "{listed}");
    let lines: Vec<u64> = listed["unreadable_lines"]
        .as_array()
        .unwrap_or_else(|| panic!("没有 unreadable_lines：{listed}"))
        .iter()
        .map(|item| item["line"].as_u64().expect("line"))
        .collect();
    assert_eq!(lines, [2, 3], "{listed}");
    let total = kinds.len() as u64 + 2;
    assert_eq!(listed["next_cursor"], total, "坏行也占一行，翻页不能回头");
    let context = task(&ctx, "context", json!({"task_id": task_id}));
    assert_eq!(context["unreadable_line_count"], 2, "{context}");
    assert_eq!(context["next_cursor"], total, "{context}");
    assert!(
        fs::read(&events).expect("read").starts_with(b"{"),
        "原文件还在"
    );
}

#[test]
fn a_line_cut_off_by_a_crash_does_not_swallow_the_next_record() {
    let fx = fixture();
    let ctx = connect(&fx);
    let task_id = start(&ctx);
    let dir = store_dir(&fx, &ctx);
    // 进程在写一行的中途没了：半行、没有换行。
    append_raw(
        &dir.join("events").join(format!("{task_id}.jsonl")),
        b"{\"id\":\"half",
    );
    let updated = task(
        &ctx,
        "update",
        json!({"task_id": task_id, "pending_steps": ["下一步"]}),
    );
    assert_eq!(updated["ok"], true, "{updated}");

    let listed = task(&ctx, "events", json!({"task_id": task_id}));
    let kinds: Vec<&str> = listed["events"]
        .as_array()
        .expect("events")
        .iter()
        .map(|event| event["kind"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(kinds, ["task_started", "task_updated"], "{listed}");
    assert_eq!(listed["unreadable_lines"][0]["line"], 2, "{listed}");

    // 操作日志是同一种文件，同样处理。
    let operations = dir.join("operations.jsonl");
    fs::write(&operations, b"{\"id\":\"half").expect("seed");
    exec_session(&ctx);
    let log = task(&ctx, "operation_log", json!({}));
    assert_eq!(log["ok"], true, "{log}");
    assert!(
        !log["operations"].as_array().expect("operations").is_empty(),
        "{log}"
    );
    assert_eq!(log["unreadable_lines"][0]["line"], 1, "{log}");
}

#[test]
fn a_corrupt_task_file_stops_writes_instead_of_dropping_the_task() {
    let fx = fixture();
    let ctx = connect(&fx);
    let task_id = start(&ctx);
    let file = store_dir(&fx, &ctx)
        .join("tasks")
        .join(format!("{task_id}.json"));
    let broken = b"{\"id\": \"cut".to_vec();
    fs::write(&file, &broken).expect("corrupt");

    // 以前坏文件被悄悄跳过，工作区就成了"没有任务"：写入不再查基线，还能再开一个任务。
    let status = task(&ctx, "status", json!({}));
    assert_eq!(status["error"]["code"], "STORE_CORRUPT", "{status}");
    assert!(
        status["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains(&task_id),
        "{status}"
    );
    let patched = call_tool(
        &ctx,
        "apply_patch",
        &json!({"patch": "--- a/README.md\n+++ b/README.md\n@@\n-初始内容\n+改过\n"}),
    );
    assert_eq!(patched["error"]["code"], "STORE_CORRUPT", "{patched}");
    assert_eq!(
        fs::read_to_string(fx.workspace.join("README.md")).expect("read"),
        "初始内容\n"
    );
    let second = task(&ctx, "start", json!({"objective": "另一个"}));
    assert_eq!(second["error"]["code"], "STORE_CORRUPT", "{second}");
    assert_eq!(fs::read(&file).expect("read"), broken, "坏文件原样留着");

    // 人把它挪开（不是 .json 就不当任务读），工作区回到没有任务的样子。
    fs::rename(&file, file.with_extension("json.corrupt")).expect("move aside");
    let status = task(&ctx, "status", json!({}));
    assert_eq!(status["ok"], true, "{status}");
    assert_eq!(status["task_id"], Value::Null, "{status}");
}

#[test]
fn a_corrupt_finished_task_next_to_an_open_one_is_reported_not_blocking() {
    let fx = fixture();
    let ctx = connect(&fx);
    let task_id = start(&ctx);
    let tasks = store_dir(&fx, &ctx).join("tasks");
    fs::write(tasks.join("0123456789abcdef.json"), b"not json").expect("corrupt");

    let status = task(&ctx, "status", json!({}));
    assert_eq!(status["ok"], true, "{status}");
    assert_eq!(status["task_id"], task_id.as_str(), "{status}");
    assert_eq!(status["writable"], true, "{status}");
    let listed = status["unreadable_task_files"].to_string();
    assert!(listed.contains("0123456789abcdef.json"), "{status}");
}

#[test]
fn a_write_cut_off_before_the_rename_leaves_the_task_intact_after_restart() {
    let fx = fixture();
    let task_id = {
        let ctx = connect(&fx);
        let task_id = start(&ctx);
        let tasks = store_dir(&fx, &ctx).join("tasks");
        // 写临时文件写到一半进程就没了，还没来得及改名。
        fs::write(
            tasks.join(format!("{task_id}.json.tmp.99999.0")),
            b"{\"id\":",
        )
        .expect("tmp");
        fs::write(tasks.join(format!("{task_id}.json.tmp")), b"{").expect("legacy tmp");
        task_id
    };
    let restarted = connect(&fx);
    let status = task(&restarted, "status", json!({}));
    assert_eq!(status["ok"], true, "{status}");
    assert_eq!(status["task_id"], task_id.as_str(), "{status}");
    assert!(
        status.get("unreadable_task_files").is_none(),
        "临时文件不算任务：{status}"
    );
}

#[test]
fn corrupt_index_files_fall_back_to_the_task_records() {
    let fx = fixture();
    let ctx = connect(&fx);
    let task_id = start(&ctx);
    let dir = store_dir(&fx, &ctx);
    // 两份都是从任务文件推出来的索引：活动任务是哪个、上次记账时的逐文件清单。
    fs::write(dir.join("state.json"), b"{").expect("corrupt state");
    fs::create_dir_all(dir.join("expected")).expect("expected dir");
    fs::write(dir.join("expected").join(format!("{task_id}.json")), b"[").expect("corrupt");

    let review = task(&ctx, "refresh_baseline", json!({"task_id": task_id}));
    assert_eq!(review["ok"], true, "{review}");
    assert_eq!(review["compared_with"], "task_start", "{review}");

    // 后台命令的终态要靠 active_task 找到任务才记得下来；状态文件坏了也得找得到。
    let passed = exec_session(&ctx);
    let done = task(
        &ctx,
        "finish",
        json!({"task_id": task_id, "evidence_session_ids": [passed]}),
    );
    assert_eq!(done["ok"], true, "{done}");
}

fn exec_session(ctx: &ToolContext) -> String {
    let output = call_tool(ctx, "exec_command", &json!({"cmd": PASSING}));
    assert_eq!(output["ok"], true, "{output}");
    output["session_id"]
        .as_str()
        .unwrap_or_else(|| panic!("没有 session_id：{output}"))
        .to_string()
}

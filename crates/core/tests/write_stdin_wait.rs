//! `write_stdin` 等后台命令：结束了就返回，不睡满 `yield_time_ms`（审查 §28）。
mod common;

use std::time::{Duration, Instant};

use gld_core::tools::{call_tool, ToolContext};
use serde_json::json;

fn ctx() -> (tempfile::TempDir, ToolContext) {
    common::isolate_data_home();
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).unwrap();
    (temp, ctx)
}

#[test]
fn waiting_on_a_background_command_returns_when_it_exits() {
    let (_temp, ctx) = ctx();
    let started = call_tool(
        &ctx,
        "exec_command",
        &json!({
            "argv": ["python3", "-c", "import time; time.sleep(0.5); print('done')"],
            "yield_time_ms": 0
        }),
    );
    assert_eq!(started["status"], "running", "{started}");
    // 还在跑的回包要告诉 AI 怎么等，而不是让它每秒问一次。
    assert!(
        started["suggestion"]
            .as_str()
            .is_some_and(|text| text.contains("yield_time_ms=30000")),
        "{started}"
    );

    let begin = Instant::now();
    let waited = call_tool(
        &ctx,
        "write_stdin",
        &json!({"session_id": started["session_id"], "yield_time_ms": 10_000}),
    );
    let elapsed = begin.elapsed();
    assert_eq!(waited["command_ok"], true, "{waited}");
    assert_eq!(waited["stdout"], "done\n", "{waited}");
    assert!(
        elapsed < Duration::from_secs(5),
        "命令 0.5 秒就结束了，却等了 {elapsed:?}"
    );
}

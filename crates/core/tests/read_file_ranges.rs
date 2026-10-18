//! `read_file` 带 `ranges`：一次读几个文件、几段行（审查 §28：AI 以前改用 python3 -c 一次读多段）。
mod common;

use std::fs;

use gld_core::tools::{call_tool, ToolContext};
use serde_json::{json, Value};

fn ctx() -> (tempfile::TempDir, std::path::PathBuf, ToolContext) {
    common::isolate_data_home();
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    fs::write(workspace.join("a.txt"), "a1\na2\na3\na4\n").unwrap();
    fs::write(workspace.join("b.txt"), "b1\nb2\n").unwrap();
    let ctx = ToolContext::for_test(workspace.clone(), temp.path().join("harness")).unwrap();
    (temp, workspace, ctx)
}

fn read(ctx: &ToolContext, args: Value) -> Value {
    call_tool(ctx, "read_file", &args)
}

#[test]
fn several_files_and_ranges_come_back_in_order() {
    let (_temp, _workspace, ctx) = ctx();
    let result = read(
        &ctx,
        json!({"ranges": [
            {"path": "a.txt", "start_line": 2, "end_line": 3},
            {"path": "b.txt"},
            {"path": "a.txt", "start_line": 4}
        ]}),
    );
    assert_eq!(result["ok"], true, "{result}");
    let results = result["results"].as_array().unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(results[0]["content"], "a2\na3\n");
    assert_eq!(results[1]["content"], "b1\nb2\n");
    assert_eq!(results[2]["content"], "a4\n");
    // 每段都带版本号，照样能交给 apply_patch 的 expected_versions。
    assert!(results[1]["version"].is_string(), "{result}");
}

#[test]
fn one_bad_item_does_not_stop_the_others() {
    let (_temp, _workspace, ctx) = ctx();
    let result = read(
        &ctx,
        json!({"ranges": [{"path": "missing.txt"}, {"path": "b.txt"}, {"path": "../outside.txt"}]}),
    );
    assert_eq!(result["ok"], true, "{result}");
    let results = result["results"].as_array().unwrap();
    assert_eq!(results[0]["ok"], false, "{result}");
    assert_eq!(results[0]["path"], "missing.txt");
    assert_eq!(results[1]["content"], "b1\nb2\n");
    assert_eq!(
        results[2]["ok"], false,
        "Workspace 外面照样读不到：{result}"
    );
}

#[test]
fn ranges_cannot_be_mixed_with_path_and_are_bounded() {
    let (_temp, _workspace, ctx) = ctx();
    let mixed = read(
        &ctx,
        json!({"path": "a.txt", "ranges": [{"path": "b.txt"}]}),
    );
    assert_eq!(mixed["error"]["code"], "INVALID_ARGUMENT", "{mixed}");
    let many = (0..21)
        .map(|_| json!({"path": "a.txt"}))
        .collect::<Vec<_>>();
    let too_many = read(&ctx, json!({ "ranges": many }));
    assert_eq!(too_many["error"]["code"], "INVALID_ARGUMENT", "{too_many}");
    let typo = read(&ctx, json!({"ranges": [{"path": "a.txt", "start": 2}]}));
    assert_eq!(typo["error"]["code"], "INVALID_ARGUMENT", "{typo}");
}

#[test]
fn the_total_is_capped_and_the_rest_is_marked_skipped() {
    let (_temp, workspace, ctx) = ctx();
    let line = "x".repeat(99) + "\n";
    fs::write(workspace.join("big.txt"), line.repeat(2_000)).unwrap();
    let result = read(
        &ctx,
        json!({
            "max_bytes": 200_000,
            "ranges": [{"path": "big.txt"}, {"path": "big.txt"}, {"path": "a.txt"}]
        }),
    );
    assert_eq!(result["ok"], true, "{result}");
    let results = result["results"].as_array().unwrap();
    assert_eq!(results[0]["bytes_read"], 200_000);
    assert_eq!(results[1]["truncated"], true, "第二段只剩额度的零头");
    assert_eq!(results[2]["skipped"], true, "{}", results[2]);
    assert!(result["bytes_read"].as_u64().unwrap() <= 256 * 1024);
}

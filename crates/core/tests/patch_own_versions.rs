//! 补丁带的 `expected_versions` 落后，但中间全是自己经 gld 写的：放行（审查 §31）。
//!
//! 记录里 9 次版本冲突全是这样：AI 改完一个文件，下一个补丁还带着改之前的版本号。
//! 别人写过、gld 之外改过，照旧拒。
mod common;

use std::fs;

use gld_core::auth::{AuthContext, Principal};
use gld_core::tools::caller::Caller;
use gld_core::tools::{call_tool, call_tool_as, ToolContext};
use serde_json::{json, Value};

fn ctx() -> (tempfile::TempDir, std::path::PathBuf, ToolContext) {
    common::isolate_data_home();
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    fs::write(workspace.join("notes.md"), "one\n").unwrap();
    let ctx = ToolContext::for_test(workspace.clone(), temp.path().join("harness")).unwrap();
    (temp, workspace, ctx)
}

fn patch(from: &str, to: &str) -> String {
    format!("--- a/notes.md\n+++ b/notes.md\n@@\n-{from}\n+{to}\n")
}

fn version(ctx: &ToolContext) -> String {
    let read = call_tool(ctx, "read_file", &json!({"path": "notes.md"}));
    read["version"].as_str().expect("version").to_string()
}

fn apply(ctx: &ToolContext, from: &str, to: &str, expected: &str) -> Value {
    call_tool(
        ctx,
        "apply_patch",
        &json!({"patch": patch(from, to), "expected_versions": {"notes.md": expected}}),
    )
}

fn stranger() -> Caller {
    Caller::from_auth(&AuthContext::new(
        Principal::OAuthClient {
            client_id: "stranger".into(),
        },
        "hub",
    ))
}

#[test]
fn a_version_from_before_my_own_edits_is_accepted_with_a_note() {
    let (_temp, workspace, ctx) = ctx();
    let read = version(&ctx);
    assert_eq!(apply(&ctx, "one", "two", &read)["ok"], true);
    assert_eq!(apply(&ctx, "two", "three", &read)["ok"], true);

    // 连着改两次，还拿最早读到的版本号：预检和落盘都放行，并说清怎么回事。
    let checked = call_tool(
        &ctx,
        "patch_check",
        &json!({"patch": patch("three", "four"), "expected_versions": {"notes.md": read}}),
    );
    assert_eq!(checked["ok"], true, "{checked}");
    let applied = apply(&ctx, "three", "four", &read);
    assert_eq!(applied["ok"], true, "{applied}");
    assert!(
        applied["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap_or("").contains("自己上次改它之前")),
        "{applied}"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("notes.md")).unwrap(),
        "four\n"
    );
}

#[test]
fn a_change_made_outside_gld_is_still_a_conflict() {
    let (_temp, workspace, ctx) = ctx();
    let read = version(&ctx);
    assert_eq!(apply(&ctx, "one", "two", &read)["ok"], true);
    // gld 之外的人改了它；长度也变，版本号才一定不同。
    fs::write(workspace.join("notes.md"), "two\nedited elsewhere\n").unwrap();
    let refused = apply(&ctx, "two", "three", &read);
    assert_eq!(
        refused["error"]["code"], "FILE_VERSION_CONFLICT",
        "{refused}"
    );
}

#[test]
fn a_write_by_another_caller_is_still_a_conflict() {
    let (_temp, _workspace, ctx) = ctx();
    let read = version(&ctx);
    assert_eq!(apply(&ctx, "one", "two", &read)["ok"], true);
    let theirs = call_tool_as(
        &ctx,
        &stranger(),
        "apply_patch",
        &json!({"patch": patch("two", "their edit")}),
    );
    assert_eq!(theirs["ok"], true, "{theirs}");
    let refused = apply(&ctx, "their edit", "mine again", &read);
    assert_eq!(
        refused["error"]["code"], "FILE_VERSION_CONFLICT",
        "{refused}"
    );
}

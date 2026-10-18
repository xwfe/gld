//! 大段内联脚本只执行一份，不再在命令元数据里整段回传。
mod common;

use gld_core::tools::{call_tool, ToolContext};
use serde_json::json;

#[test]
fn command_preflight_bounds_utf8_echo_without_changing_the_decision() {
    common::isolate_data_home();
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).unwrap();
    let data = "资料🚀".repeat(6000);
    let result = call_tool(
        &ctx,
        "check_command",
        &json!({
            "argv": ["git", "-c", format!("gld.test={data}"), "--version"]
        }),
    );
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["decision"], "deny");
    assert_eq!(result["rule"], "command_too_long");
    assert!(result["command"].as_str().unwrap().len() <= 2048);
    assert_eq!(result["command_truncated"], true);
    assert!(result["command_bytes"].as_u64().unwrap() > data.len() as u64);
    assert!(serde_json::to_vec(&result).unwrap().len() < 8192);
}

#[cfg(unix)]
#[test]
fn execution_receives_all_inline_data_while_only_the_echo_is_shortened() {
    common::isolate_data_home();
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).unwrap();
    // 执行路径原有 4000 字节限制，回显优化不能放宽它。
    let data = "资料🚀".repeat(300);
    let script = format!("data = '{data}'; print(len(data.encode())); print('tail-executed')");
    let result = call_tool(
        &ctx,
        "exec_command",
        &json!({
            "argv": ["python3", "-c", script], "max_output_bytes": 1024,
            "yield_time_ms": common::UNTIL_EXIT_MS
        }),
    );
    assert_eq!(result["command_ok"], true, "{result}");
    assert_eq!(result["stdout"], format!("{}\ntail-executed\n", data.len()));
    assert!(result["command"].as_str().unwrap().len() <= 2048);
    assert_eq!(result["command_truncated"], true);
    assert!(result["command_bytes"].as_u64().unwrap() > script.len() as u64);
    assert!(serde_json::to_vec(&result).unwrap().len() < 8192);
}

#[test]
fn ordinary_commands_keep_their_exact_echo() {
    common::isolate_data_home();
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).unwrap();
    let result = call_tool(
        &ctx,
        "exec_command",
        &json!({
            "cmd": "git --version", "yield_time_ms": common::UNTIL_EXIT_MS
        }),
    );
    assert_eq!(result["command_ok"], true, "{result}");
    assert_eq!(result["command"], "git --version");
    assert_eq!(result["command_bytes"], "git --version".len());
    assert_eq!(result["command_truncated"], false);
}

#[test]
fn a_command_that_is_too_long_says_how_long_and_what_to_do_instead() {
    common::isolate_data_home();
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).unwrap();
    let script = format!("print('{}')", "x".repeat(5000));
    let result = call_tool(
        &ctx,
        "exec_command",
        &json!({ "argv": ["python3", "-c", script] }),
    );
    assert_eq!(
        result["error"]["details"]["reason"], "command_too_long",
        "{result}"
    );
    let message = result["error"]["message"].as_str().unwrap();
    assert!(message.contains("the limit is 4000"), "{message}");
    assert!(message.contains("bytes"), "{message}");
    let suggestion = result["error"]["details"]["suggestion"].as_str().unwrap();
    assert!(suggestion.contains("stdin"), "{suggestion}");
}

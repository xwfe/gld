//! 守护进程环境里名字像密钥的变量不交给项目命令（2026-10-08：本机守护进程是从 Claude Code 会话里起的，
//! 带着 CLAUDE_CODE_MESSAGING_TOKEN，ChatGPT 经 gld 跑的命令一句 `print(os.environ)` 就读得到）。
//!
//! 这个文件只放这一条测试：它要改本进程的环境变量，而改环境和别的线程同时起子进程是未定义行为。

mod common;

use common::*;
use serde_json::json;

#[cfg(windows)]
const TEST_PYTHON: &str = "python";
#[cfg(not(windows))]
const TEST_PYTHON: &str = "python3";

#[test]
fn secret_looking_variables_do_not_reach_project_commands() {
    std::env::set_var("GLD_TEST_DEPLOY_TOKEN", "s3cr3t-value");
    std::env::set_var("GLD_TEST_PLAIN_SETTING", "visible-value");
    let dir = tempfile::tempdir().expect("workspace");
    let ctx = ctx_for(dir.path());

    let out = invoke(
        &ctx,
        "exec_command",
        json!({
            "argv": [
                TEST_PYTHON,
                "-c",
                "import os; print(os.environ.get('GLD_TEST_DEPLOY_TOKEN'), os.environ.get('GLD_TEST_PLAIN_SETTING'))"
            ],
            "yield_time_ms": UNTIL_EXIT_MS
        }),
    );
    let payload = assert_ok(&out);
    assert_eq!(payload["exit_code"], 0, "{payload}");
    assert_eq!(
        payload["stdout"].as_str().unwrap_or_default().trim(),
        "None visible-value",
        "名字像密钥的不给，别的照常给：{payload}"
    );

    // AI 查得到是哪些被扣下了，只有名字、没有值。
    let env = invoke(&ctx, "check_exec_environment", json!({}));
    let names = &assert_ok(&env)["withheld_environment"]["names"];
    assert!(
        names
            .as_array()
            .expect("names")
            .contains(&json!("GLD_TEST_DEPLOY_TOKEN")),
        "{env}"
    );
    assert!(
        !names
            .as_array()
            .expect("names")
            .contains(&json!("GLD_TEST_PLAIN_SETTING")),
        "{env}"
    );
    assert!(!env.to_string().contains("s3cr3t-value"), "{env}");
}

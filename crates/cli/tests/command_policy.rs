//! `gld set <项目> command-policy=all`：一键放行全部命令，打开时说清风险；本机 `gld ls` 标出来；
//! 经守护进程跑一条白名单外的命令，回包注明绕过了哪道关（审查 §60）。

#![cfg(unix)]

mod common;

use common::env::Env;
use serde_json::Value;

fn exec(env: &Env, argv: &str) -> Value {
    let output = env.gld(&[
        "--json",
        "tool",
        "call",
        "exec_command",
        &format!("argv:={argv}"),
        "yield_time_ms:=30000",
    ]);
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "不是 JSON（{error}）：stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn allowing_all_commands_warns_marks_and_reports_what_it_bypassed() {
    let env = Env::new();
    env.ok(&["add", ".", "--name", "it"]);
    env.ok(&["daemon", "start"]);

    let refused = exec(&env, r#"["sh","-c","echo ran"]"#);
    assert_eq!(refused["ok"], false, "默认 sh 不在白名单：{refused}");

    let typo = env.gld(&["set", "it", "command-policy=everything"]);
    assert!(!typo.status.success(), "写错的值要拒");

    let set = env.ok(&["set", "it", "command-policy=all"]);
    assert!(
        set.contains("允许 AI 跑任何命令") && set.contains("command-policy=guarded"),
        "打开时要说风险和怎么改回来：{set}"
    );
    let listed = env.ok(&["ls"]);
    assert!(listed.contains("命令全放行"), "{listed}");
    let detail = env.ok(&["ls", "it"]);
    assert!(
        detail.contains("all：") && detail.contains("一律放行"),
        "{detail}"
    );

    let ran = exec(&env, r#"["sh","-c","echo ran"]"#);
    assert_eq!(ran["ok"], true, "{ran}");
    assert_eq!(
        ran["policy_bypassed"]["rule"], "command_not_allowlisted",
        "{ran}"
    );

    let back = env.ok(&["set", "it", "command-policy=guarded"]);
    assert!(!back.contains("允许 AI 跑任何命令"), "{back}");
    assert!(!env.ok(&["ls"]).contains("命令全放行"));
    let refused = exec(&env, r#"["sh","-c","echo ran"]"#);
    assert_eq!(refused["ok"], false, "改回来就又拦：{refused}");

    env.ok(&["daemon", "stop"]);
}

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

fn check(env: &Env, cmd: &str, confirm: bool) -> Value {
    let cmd = format!("cmd={cmd}");
    let confirm = format!("confirm:={confirm}");
    let output = env.gld(&["--json", "tool", "call", "check_command", &cmd, &confirm]);
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "不是 JSON（{error}）：stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

/// 照 `config_command` 原样跑一遍真的 `gld set`：confirm 解不开、它解得开，还不冲掉已配的
/// 东西、不把 `only:` 改回默认全集（审查 §108）。
#[test]
fn the_config_command_lifts_the_denial_and_keeps_what_was_configured() {
    let env = Env::new();
    env.ok(&["add", ".", "--name", "it"]);

    let run_fix = |checked: &Value| {
        let command = checked["config_command"].as_str().expect("config_command");
        let args: Vec<&str> = command.split_whitespace().skip(1).collect();
        env.ok(&args);
    };
    let allowed = |checked: &Value, name: &str| {
        checked["policy"]["allowed_commands"]
            .as_array()
            .expect("allowed_commands")
            .iter()
            .any(|item| item == name)
    };

    // 追加模式：之前配过的 rg 要留下。
    env.ok(&["set", "it", "allowed-commands=rg"]);
    let denied = check(&env, "sh -c true", false);
    assert_eq!(denied["resolved_by"], "config_change", "{denied}");
    assert_eq!(
        denied["config_command"],
        "gld set it allowed-commands=rg,sh"
    );
    let confirmed = check(&env, "sh -c true", true);
    assert_eq!(confirmed["decision"], "deny", "{confirmed}");
    run_fix(&denied);
    let fixed = check(&env, "sh -c true", false);
    assert_eq!(fixed["decision"], "allow", "{fixed}");
    assert!(allowed(&fixed, "rg"), "rg 被冲掉了：{fixed}");

    // only 模式：改完还得是 only。
    env.ok(&["set", "it", "allowed-commands=only:cargo"]);
    let denied = check(&env, "sh -c true", false);
    assert_eq!(
        denied["config_command"],
        "gld set it allowed-commands=only:cargo,sh"
    );
    run_fix(&denied);
    let fixed = check(&env, "sh -c true", false);
    assert_eq!(fixed["decision"], "allow", "{fixed}");
    assert_eq!(fixed["policy"]["allowlist_mode"], "only", "{fixed}");
    assert!(allowed(&fixed, "cargo"), "{fixed}");
    assert!(!allowed(&fixed, "python3"), "only 被放宽了：{fixed}");
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

    // 委派开成 auto 也说一声。
    let auto = env.ok(&["set", "it", "agent-delegation=auto"]);
    assert!(
        auto.contains("不用你在本机放行") && auto.contains("agent-delegation=ask"),
        "{auto}"
    );
    env.ok(&["daemon", "stop"]);
}

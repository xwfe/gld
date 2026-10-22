//! 委派（RFC-0009）从头走到尾：真起守护进程，假 Agent 脚本代替 codex / claude，不花额度。
//!
//! 开关没开被拒 → 开 ask → AI 起任务、停在等放行 → 本机 `gld agent approve` → 跑完交回审核
//! → AI 审核通过；以及在跑的时候 gld 拦写和执行、`gld daemon restart` 拦重启、`gld agent stop` 停得掉。

#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::time::{Duration, Instant};

use common::env::Env;
use serde_json::Value;

/// 放一个假 Agent 脚本，登记进数据目录的 agents.toml。
fn agent(env: &Env, name: &str, body: &str) {
    let path = env.home.path().join(format!("{name}.sh"));
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'fake-agent 1.0'; exit 0; fi\n{body}\n"
        ),
    )
    .expect("script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let toml = env.home.path().join("agents.toml");
    let mut text = std::fs::read_to_string(&toml).unwrap_or_default();
    text.push_str(&format!(
        "[agents.{name}]\ncommand = [\"{}\"]\nresult = {{ from = \"stdout\" }}\n",
        path.display()
    ));
    std::fs::write(toml, text).expect("agents.toml");
}

/// `gld --json tool call agent_manage …`，不管成没成都把 JSON 拿回来。
fn manage(env: &Env, args: &[&str]) -> Value {
    let mut argv = vec!["--json", "tool", "call", "agent_manage"];
    argv.extend_from_slice(args);
    let output = env.gld(&argv);
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "不是 JSON（{error}）：stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

/// 等任务离开 running（或者还在等放行），最多 20 秒。
fn wait_done(env: &Env, task_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let task = manage(
            env,
            &[
                "action=wait",
                &format!("task_id={task_id}"),
                "wait_seconds:=5",
            ],
        );
        if task["state"] != "running" || Instant::now() >= deadline {
            return task;
        }
    }
}

fn project(env: &Env) {
    let ok = Command::new("git")
        .args(["init", "-q"])
        .current_dir(env.project.path())
        .status()
        .expect("git init")
        .success();
    assert!(ok);
    env.ok(&["add", ".", "--name", "it"]);
    env.ok(&["daemon", "start"]);
}

#[test]
fn ask_mode_goes_from_approval_to_review() {
    let env = Env::new();
    agent(
        &env,
        "fake",
        "cat > prompt-seen.txt\necho changed > touched.txt\necho 'recap: 改好了'",
    );
    project(&env);

    let off = manage(&env, &["action=start", "agent=fake", "prompt=改一下"]);
    assert_eq!(off["error"]["code"], "AGENT_DELEGATION_OFF", "{off}");
    assert!(
        off["summary"]
            .as_str()
            .unwrap_or("")
            .contains("agent-delegation=ask"),
        "得说清楚怎么开：{off}"
    );

    env.ok(&["set", "it", "agent-delegation=ask"]);
    let agents = manage(&env, &["action=agents"]);
    assert_eq!(agents["delegation"], "ask", "{agents}");
    let names: Vec<&str> = agents["agents"]
        .as_array()
        .expect("agents")
        .iter()
        .filter_map(|a| a["name"].as_str())
        .collect();
    assert!(
        names.contains(&"fake") && names.contains(&"codex"),
        "{names:?}"
    );

    let started = manage(
        &env,
        &[
            "action=start",
            "agent=fake",
            "prompt=把 README 改好",
            "brief=验收：README 第一行是标题",
        ],
    );
    assert_eq!(started["state"], "waiting_local_approval", "{started}");
    let task_id = started["task_id"].as_str().expect("task_id").to_string();
    assert!(
        started["next_action"]
            .as_str()
            .unwrap_or("")
            .contains("gld agent approve"),
        "{started}"
    );

    let listed = env.ok(&["agent", "ls"]);
    assert!(
        listed.contains("等你放行") && listed.contains(&task_id[..8]),
        "{listed}"
    );
    env.ok(&["agent", "approve", &task_id[..8]]);

    let done = wait_done(&env, &task_id);
    assert_eq!(done["state"], "awaiting_review", "{done}");
    assert!(
        done["recap"]
            .as_str()
            .unwrap_or("")
            .contains("recap: 改好了"),
        "{done}"
    );
    assert_eq!(
        done["changed_files"],
        serde_json::json!(["prompt-seen.txt", "touched.txt"])
    );
    assert!(done.get("caller").is_none(), "发起人标识不回给 AI：{done}");
    let diff = done["diff"].as_str().unwrap_or("");
    assert!(
        diff.contains("+changed") && diff.contains("touched.txt"),
        "回包里要有这次任务自己的 diff：{done}"
    );
    assert!(
        done["next_action"]
            .as_str()
            .unwrap_or("")
            .contains("diff field"),
        "{done}"
    );
    let seen = std::fs::read_to_string(env.project.path().join("prompt-seen.txt")).expect("提示");
    assert!(
        seen.contains("brief.md") && seen.contains("recap"),
        "{seen}"
    );

    let reviewed = manage(
        &env,
        &[
            "action=review",
            &format!("task_id={task_id}"),
            "verdict=accept",
            "note=diff 看过，测试过了",
        ],
    );
    assert_eq!(reviewed["state"], "accepted", "{reviewed}");
    let shown = env.ok(&["agent", "show", &task_id[..8]]);
    assert!(
        shown.contains("已通过") && shown.contains("touched.txt"),
        "{shown}"
    );
    env.ok(&["daemon", "stop"]);
}

#[test]
fn a_running_task_holds_writes_and_restart_until_stopped() {
    let env = Env::new();
    agent(
        &env,
        "ticker",
        "cat >/dev/null\nwhile true; do echo tick; sleep 0.2; done",
    );
    project(&env);
    env.ok(&["set", "it", "agent-delegation=auto"]);

    let started = manage(&env, &["action=start", "agent=ticker", "prompt=一直跑"]);
    assert_eq!(started["state"], "running", "{started}");
    let task_id = started["task_id"].as_str().expect("task_id").to_string();

    let output = env.gld(&["--json", "tool", "call", "exec_command", "cmd=git status"]);
    let refused: Value = serde_json::from_slice(&output.stdout).expect("JSON");
    assert_eq!(refused["error"]["code"], "AGENT_TASK_RUNNING", "{refused}");
    let busy = manage(&env, &["action=start", "agent=ticker", "prompt=再来一个"]);
    assert_eq!(busy["error"]["code"], "AGENT_TASK_RUNNING", "{busy}");

    let restart = env.gld(&["daemon", "restart"]);
    let stderr = String::from_utf8_lossy(&restart.stderr);
    assert!(!restart.status.success(), "委派在跑时不该重启：{stderr}");
    assert!(stderr.contains("委派给 ticker"), "{stderr}");

    env.ok(&["agent", "stop", &task_id[..8]]);
    let stopped = wait_done(&env, &task_id);
    assert_eq!(stopped["state"], "stopped", "{stopped}");
    let output = env.gld(&["--json", "tool", "call", "exec_command", "cmd=git status"]);
    assert!(
        output.status.success(),
        "停了就该放开：{}",
        String::from_utf8_lossy(&output.stdout)
    );
    env.ok(&["daemon", "restart"]);
    env.ok(&["daemon", "stop"]);
}

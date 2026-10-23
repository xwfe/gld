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
    for gone in ["prompt_preview", "owner", "owner_pid", "pid", "project_id"] {
        assert!(
            started.get(gone).is_none() && done.get(gone).is_none(),
            "{gone} 是 AI 自己写的或本机记账用的，不回给它：{done}"
        );
    }
    let tasks = manage(&env, &["action=list"]);
    assert!(
        tasks["tasks"][0]["prompt_preview"]
            .as_str()
            .unwrap_or("")
            .contains("把 README 改好"),
        "list 里还留着短预览，AI 认得出是哪个任务：{tasks}"
    );
    let touched = &done["file_stats"][1];
    assert_eq!(
        (&touched["path"], &touched["added"], &touched["deleted"]),
        (
            &serde_json::json!("touched.txt"),
            &serde_json::json!(1),
            &serde_json::json!(0)
        ),
        "{done}"
    );
    // 只读这一个文件：正好是它那一段。
    let one = manage(
        &env,
        &[
            "action=diff",
            &format!("task_id={task_id}"),
            "path=touched.txt",
        ],
    );
    let text = one["diff"].as_str().unwrap_or("");
    assert!(
        text.starts_with("diff --git a/touched.txt") && text.contains("+changed"),
        "{one}"
    );
    assert_eq!(one["offset"], touched["diff_offset"], "{one}");
    assert_eq!(
        text.len() as u64,
        touched["diff_bytes"].as_u64().unwrap_or(0),
        "{one}"
    );
    assert!(one["next_offset"].is_null(), "{one}");
    let tree = |key: &str| done[key].as_str().unwrap_or("").to_string();
    assert!(
        tree("before_tree").len() >= 40 && tree("before_tree") != tree("after_tree"),
        "回包要附跑前跑后两棵树的 id：{done}"
    );
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
    // 没截断时 action=diff 一页就读完，和回包里那份一样。
    let page = manage(&env, &["action=diff", &format!("task_id={task_id}")]);
    assert_eq!(page["diff"], done["diff"], "{page}");
    assert!(page["next_offset"].is_null(), "{page}");
    assert_eq!(page["total_bytes"], done["diff_bytes"], "{page}");
    let past = manage(
        &env,
        &[
            "action=diff",
            &format!("task_id={task_id}"),
            "offset:=99999999",
        ],
    );
    assert_eq!(past["error"]["code"], "INVALID_ARGUMENT", "{past}");
    assert!(
        past["summary"]
            .as_str()
            .unwrap_or("")
            .contains("超过了 diff 的总长"),
        "要是 offset 越界，不是参数名不认识：{past}"
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
    assert!(
        reviewed.get("diff").is_none() && reviewed.get("diff_truncated").is_none(),
        "审核的回包不再带一遍 diff：{reviewed}"
    );
    let status = manage(&env, &["action=status", &format!("task_id={task_id}")]);
    assert_eq!(status["diff"], done["diff"], "要再看 diff，status 照样给");
    let shown = env.ok(&["agent", "show", &task_id[..8]]);
    assert!(
        shown.contains("已通过")
            && shown.contains("+1 -0\ttouched.txt")
            && shown.contains("把 README 改好"),
        "本机 show 照样有提示：{shown}"
    );
    env.ok(&["daemon", "stop"]);
}

/// diff 超过 64 KB 时 `gld agent show` 只打前一段，要说清全文在本机哪个文件，而且那个文件真在、就是全文。
/// 同一份大 diff 顺带测 `action=diff max_bytes`：只看开头几行不用收一整页。
#[test]
fn a_big_diff_can_be_peeked_and_show_points_to_the_full_one() {
    let env = Env::new();
    agent(
        &env,
        "flood",
        "cat >/dev/null\ni=0; while [ $i -lt 3000 ]; do echo \"line $i of a long generated file\"; i=$((i+1)); done > big.txt\necho ok",
    );
    project(&env);
    env.ok(&["set", "it", "agent-delegation=auto"]);
    let started = manage(
        &env,
        &["action=start", "agent=flood", "prompt=写一个大文件"],
    );
    let task_id = started["task_id"].as_str().expect("task_id").to_string();
    let done = wait_done(&env, &task_id);
    assert_eq!(done["diff_truncated"], true, "{done}");
    // 超过一页的 diff 不附第一页，给 AI 的是 file_stats 和怎么读。
    assert!(
        done["diff"].is_null() && done["diff_next_offset"] == 0,
        "{done}"
    );
    assert!(
        done["next_action"].as_str().unwrap_or("").contains("path="),
        "{done}"
    );
    assert_eq!(done["file_stats"][0]["path"], "big.txt", "{done}");

    let peek = |max: &str| {
        manage(
            &env,
            &[
                "action=diff",
                &format!("task_id={task_id}"),
                "path=big.txt",
                max,
            ],
        )
    };
    let page = peek("max_bytes:=4096");
    let text = page["diff"].as_str().unwrap_or("");
    assert!(
        text.len() <= 4096 && text.ends_with('\n') && text.starts_with("diff --git a/big.txt"),
        "{page}"
    );
    assert!(page["next_offset"].as_u64().is_some(), "{page}");
    let tiny = peek("max_bytes:=10");
    let tiny_len = tiny["diff"].as_str().unwrap_or("").len();
    assert!(
        (11..=1024).contains(&tiny_len),
        "小于 1 KB 按 1 KB 算，不报错：{tiny_len}"
    );

    let shown = env.ok(&["agent", "show", &task_id[..8]]);
    let path = shown
        .split("全文在 ")
        .nth(1)
        .and_then(|rest| rest.split('）').next())
        .unwrap_or_else(|| panic!("没说全文在哪：{shown}"));
    let full = std::fs::read(path).unwrap_or_else(|error| panic!("{path} 打不开：{error}"));
    assert_eq!(Some(full.len() as u64), done["diff_bytes"].as_u64());
    env.ok(&["daemon", "stop"]);
}

/// `agent-approval=self`：AI 从 `action=agents` 看得到哪几家有"自行判断"那一档；拿没有的去起，
/// 报错说清有哪几家、怎么改，不退回全放开。
#[test]
fn self_approval_is_listed_and_enforced() {
    let env = Env::new();
    agent(&env, "fake", "cat >/dev/null\necho ran > ran.txt\necho ok");
    project(&env);
    env.ok(&["set", "it", "agent-delegation=auto"]);
    let typo = env.gld(&["set", "it", "agent-approval=maybe"]);
    assert!(!typo.status.success(), "写错的值要拒");
    let set = env.ok(&["set", "it", "agent-approval=self"]);
    assert!(set.contains("自行判断"), "{set}");
    env.ok(&["set", "it", "agent-delegation=ask"]);
    let asked = env.ok(&["set", "it", "agent-approval=self"]);
    assert!(
        !asked.contains("用 ask"),
        "已经是 ask 了不该再劝用 ask：{asked}"
    );
    env.ok(&["set", "it", "agent-delegation=auto"]);

    let agents = manage(&env, &["action=agents"]);
    assert_eq!(agents["approval"], "self", "{agents}");
    let supports = |name: &str| {
        agents["agents"]
            .as_array()
            .expect("agents")
            .iter()
            .find(|a| a["name"] == name)
            .map(|a| a["self_approval"].clone())
            .unwrap_or_default()
    };
    assert_eq!(supports("codex"), true, "{agents}");
    assert_eq!(supports("fake"), false, "{agents}");

    let refused = manage(&env, &["action=start", "agent=fake", "prompt=改一下"]);
    assert_eq!(
        refused["error"]["code"], "AGENT_APPROVAL_UNSUPPORTED",
        "{refused}"
    );
    assert!(
        refused["summary"].as_str().unwrap_or("").contains("codex"),
        "{refused}"
    );
    assert!(!env.project.path().join("ran.txt").exists());
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

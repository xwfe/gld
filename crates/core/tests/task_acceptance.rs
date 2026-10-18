//! 任务验收、命令失败传播、基线恢复（审查 D01–D03）的端到端反例。
//!
//! 全走 `call_tool`，也就是客户端那条路：参数校验、策略、写前检查、记账都在里面。

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use gld_core::tools::{call_tool, ToolContext};
use serde_json::{json, Value};

/// 不在任何 Git 仓库里跑也一定退出 128 的命令，各平台都有 git。
const FAILING: &str = "git rev-parse --verify refs/heads/no-such-branch-for-gld-test";
/// 一定退出 0、会真起一个子进程（有 session_id）的命令。
const PASSING: &str = "git --version";

struct Fixture {
    ctx: ToolContext,
    workspace: PathBuf,
    _temp: tempfile::TempDir,
}

fn fixture() -> Fixture {
    common::isolate_data_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    fs::write(workspace.join("README.md"), "初始内容\n").expect("file");
    let ctx = ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("ctx");
    Fixture {
        ctx,
        workspace,
        _temp: temp,
    }
}

/// 同 `fixture`，但工作区是有一个提交的 Git 仓库。
fn git_fixture() -> Fixture {
    let fx = fixture();
    for args in [
        &["init", "-q"][..],
        &["config", "user.email", "test@example.com"],
        &["config", "user.name", "test"],
        // 本机全局配置开了签名的话，测试里的提交会去找密钥。
        &["config", "commit.gpgsign", "false"],
        &["add", "README.md"],
        &["commit", "-q", "-m", "init"],
    ] {
        git(&fx.workspace, args);
    }
    fx
}

/// 不经 gld 直接跑 git，当成"任务之外的人"。
fn git(workspace: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(workspace)
        .args(args)
        .output()
        .expect("git");
    assert!(output.status.success(), "git {args:?}：{output:?}");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn patch_readme(ctx: &ToolContext, from: &str, to: &str) -> Value {
    call_tool(
        ctx,
        "apply_patch",
        &json!({"patch": format!("--- a/README.md\n+++ b/README.md\n@@\n-{from}\n+{to}\n")}),
    )
}

fn events_of_kind(ctx: &ToolContext, task_id: &str, kind: &str) -> Vec<Value> {
    ctx.harness
        .list_events(task_id, 0, usize::MAX)
        .expect("events")
        .into_items()
        .into_iter()
        .filter(|event| event.kind == kind)
        .map(|event| serde_json::to_value(event).expect("event json"))
        .collect()
}

/// 跑完才回来：台账、退出码、拿 session_id 当证据，都要命令已经结束。
fn exec(ctx: &ToolContext, cmd: &str) -> Value {
    call_tool(
        ctx,
        "exec_command",
        &json!({"cmd": cmd, "yield_time_ms": common::UNTIL_EXIT_MS}),
    )
}

fn session_id(output: &Value) -> String {
    output["session_id"]
        .as_str()
        .unwrap_or_else(|| panic!("没有 session_id：{output}"))
        .to_string()
}

fn ledger(ctx: &ToolContext) -> Value {
    let state = call_tool(ctx, "planning_state", &json!({}));
    assert_eq!(state["ok"], true, "{state}");
    state["state"]["execution"].clone()
}

fn start_task(ctx: &ToolContext) -> String {
    let started = call_tool(
        ctx,
        "task_manage",
        &json!({"action": "start", "objective": "验收"}),
    );
    assert_eq!(started["ok"], true, "{started}");
    started["task"]["id"].as_str().expect("task id").to_string()
}

fn finish(ctx: &ToolContext, task_id: &str, evidence: &[&str]) -> Value {
    call_tool(
        ctx,
        "task_manage",
        &json!({"action": "finish", "task_id": task_id, "evidence_session_ids": evidence}),
    )
}

fn rejected_codes(output: &Value) -> Vec<String> {
    output["error"]["details"]["rejected"]
        .as_array()
        .unwrap_or_else(|| panic!("没有 rejected：{output}"))
        .iter()
        .map(|item| item["code"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn finish_retry_returns_the_completed_receipt_without_rewriting_events() {
    let fx = fixture();
    let task = start_task(&fx.ctx);
    let passed = session_id(&exec(&fx.ctx, PASSING));
    let args = json!({"action": "finish", "task_id": task,
        "evidence_session_ids": [passed], "summary": "已交付"});
    let first = call_tool(&fx.ctx, "task_manage", &args);
    assert_eq!(first["task"]["status"], "completed", "{first}");
    let before = fx
        .ctx
        .harness
        .list_events(&task, 0, usize::MAX)
        .unwrap()
        .into_items();
    let again = call_tool(&fx.ctx, "task_manage", &args);
    assert_eq!(
        again["ok"], true,
        "丢失回包后重试不该报状态转换错误：{again}"
    );
    assert_eq!(again["finished"], true);
    assert_eq!(again["already_finished"], true);
    assert_eq!(again["verification_required"], false);
    assert_eq!(again["task"], first["task"]);
    let after = fx
        .ctx
        .harness
        .list_events(&task, 0, usize::MAX)
        .unwrap()
        .into_items();
    assert_eq!(
        serde_json::to_value(before).unwrap(),
        serde_json::to_value(after).unwrap()
    );
}

#[test]
fn finish_retry_never_revalidates_new_content_or_closes_a_new_task() {
    let fx = fixture();
    let old = start_task(&fx.ctx);
    let passed = session_id(&exec(&fx.ctx, PASSING));
    assert_eq!(
        finish(&fx.ctx, &old, &[&passed])["task"]["status"],
        "completed"
    );
    fs::write(fx.workspace.join("README.md"), "后来改的\n").unwrap();
    let new = start_task(&fx.ctx);
    let again = finish(&fx.ctx, &old, &[]);
    assert_eq!(again["ok"], true, "{again}");
    assert_eq!(again["already_finished"], true);
    assert!(!again["change_summary"]["risks"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(fx.ctx.harness.current_task().unwrap().unwrap().id, new);
    let invalid = finish(&fx.ctx, &old, &["not-the-accepted-session"]);
    assert_eq!(invalid["ok"], false, "不能把新的证据当成已验收：{invalid}");
    assert_eq!(
        fx.ctx.harness.task(&old).unwrap().status,
        gld_core::harness::model::TaskStatus::Completed
    );
}

#[test]
fn finish_retry_keeps_unverified_distinct_from_verified() {
    let fx = fixture();
    let task = start_task(&fx.ctx);
    let args = json!({"action": "finish", "task_id": task, "allow_unverified": true});
    assert_eq!(
        call_tool(&fx.ctx, "task_manage", &args)["task"]["status"],
        "completed_unverified"
    );
    let again = call_tool(&fx.ctx, "task_manage", &args);
    assert_eq!(again["ok"], true, "{again}");
    assert_eq!(again["task"]["status"], "completed_unverified");
    assert_eq!(again["finished"], true);
    assert_eq!(again["already_finished"], true);
    assert_eq!(finish(&fx.ctx, &task, &["invented"])["ok"], false);
}

#[test]
fn completed_receipt_survives_an_unavailable_change_summary() {
    let fx = fixture();
    let task = start_task(&fx.ctx);
    let passed = session_id(&exec(&fx.ctx, PASSING));
    assert_eq!(
        finish(&fx.ctx, &task, &[&passed])["task"]["status"],
        "completed"
    );
    // 只破坏隔离 fixture 的事件读取；任务记录本身依然完整。
    let events = fx
        .ctx
        .harness
        .store_root()
        .join("workspaces")
        .join(fx.ctx.harness.workspace_id())
        .join("events")
        .join(format!("{task}.jsonl"));
    fs::remove_file(&events).unwrap();
    fs::create_dir(&events).unwrap();
    let again = finish(&fx.ctx, &task, &[]);
    assert_eq!(again["ok"], true, "摘要故障不能伪装成任务没有完成：{again}");
    assert_eq!(again["task"]["status"], "completed");
    assert_eq!(again["finished"], true);
    assert_eq!(again["change_summary_available"], false);
    assert!(again["change_summary"].is_null());
    assert!(!again["warnings"].as_array().unwrap().is_empty());
}

#[test]
fn finish_marks_waiting_and_reports_unresolved_steps_without_inventing_completion() {
    let fx = fixture();
    let task = start_task(&fx.ctx);
    let updated = call_tool(
        &fx.ctx,
        "task_manage",
        &json!({
            "action": "update", "task_id": task, "pending_steps": ["用户另行部署"]
        }),
    );
    assert_eq!(updated["ok"], true);
    let waiting = finish(&fx.ctx, &task, &[]);
    assert_eq!(waiting["finished"], false);
    assert_eq!(waiting["verification_required"], true);
    let passed = session_id(&exec(&fx.ctx, PASSING));
    let done = finish(&fx.ctx, &task, &[&passed]);
    assert_eq!(done["finished"], true);
    assert_eq!(done["verification_required"], false);
    assert_eq!(done["task"]["pending_steps"], json!(["用户另行部署"]));
    assert!(done["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v.as_str().unwrap_or("").contains("pending_steps")));
    assert!(done["next"].as_str().unwrap().contains("无需"));
}

/// 反例（审查 D03）：命令退出 7，工具 ok=true、CLI 退出 0，台账却记 completed、last_error 为空。
#[test]
fn 命令退出非零时台账记失败而不是完成() {
    let fx = fixture();
    let output = exec(&fx.ctx, FAILING);
    assert_eq!(output["ok"], true, "调用本身是成功的：{output}");
    assert_eq!(output["command_ok"], false, "{output}");
    let exit_code = output["exit_code"].as_i64().expect("exit_code");
    assert_ne!(exit_code, 0);

    let execution = ledger(&fx.ctx);
    assert_eq!(execution["state"], "failed", "{execution}");
    assert_eq!(execution["call_ok"], true, "{execution}");
    assert_eq!(execution["command"]["exit_code"], exit_code, "{execution}");
    let last_error = execution["last_error"].as_str().unwrap_or_default();
    assert!(
        last_error.contains(&exit_code.to_string()),
        "last_error 要说出退出码：{execution}"
    );

    let log = call_tool(&fx.ctx, "operation_log", &json!({}));
    let finished = log["operations"]
        .as_array()
        .expect("operations")
        .iter()
        .rev()
        .find(|op| op["tool"] == "exec_command" && op["kind"] != "started")
        .cloned()
        .expect("exec_command 的结束记录");
    assert_eq!(finished["kind"], "failed", "{finished}");
    assert_eq!(
        finished["result_summary"]["command"]["exit_code"],
        exit_code
    );

    exec(&fx.ctx, PASSING);
    let execution = ledger(&fx.ctx);
    assert_eq!(execution["state"], "completed", "{execution}");
    assert_eq!(execution["last_error"], Value::Null, "{execution}");
}

/// 反例（审查 D03）：返回 running 的命令被台账记成 completed；之后读到的终态也不回写。
#[cfg(unix)]
#[test]
fn 后台命令的终态由_read_output_补回台账() {
    let fx = fixture();
    let output = call_tool(
        &fx.ctx,
        "exec_command",
        &json!({"cmd": "python3 -c \"import time,sys; time.sleep(0.3); sys.exit(3)\"", "yield_time_ms": 0}),
    );
    assert_eq!(output["status"], "running", "{output}");
    let session = session_id(&output);
    let execution = ledger(&fx.ctx);
    assert_eq!(execution["state"], "running", "{execution}");
    assert_eq!(execution["command"]["session_id"], session.as_str());

    let read = wait_until_finished(&fx.ctx, &session);
    assert_eq!(read["exit_code"], 3, "read_output 要说出退出码：{read}");
    assert_eq!(read["command_ok"], false, "{read}");

    let execution = ledger(&fx.ctx);
    assert_eq!(execution["state"], "failed", "{execution}");
    assert_eq!(execution["command"]["exit_code"], 3, "{execution}");
}

/// 超时和被杀各有各的状态，不和"失败"混在一起，更不是 completed。
#[cfg(unix)]
#[test]
fn 超时和取消在台账里各记各的() {
    let fx = fixture();
    let timed_out = call_tool(
        &fx.ctx,
        "exec_command",
        &json!({
            "cmd": "python3 -c \"import time; time.sleep(5)\"",
            "timeout_ms": 200,
            "yield_time_ms": common::UNTIL_EXIT_MS
        }),
    );
    assert_eq!(timed_out["termination_reason"], "timeout", "{timed_out}");
    assert_eq!(ledger(&fx.ctx)["state"], "timed_out");

    let running = call_tool(
        &fx.ctx,
        "exec_command",
        &json!({"cmd": "python3 -c \"import time; time.sleep(5)\"", "yield_time_ms": 0}),
    );
    let killed = call_tool(
        &fx.ctx,
        "kill_session",
        &json!({"session_id": session_id(&running), "wait_ms": 2000}),
    );
    assert_eq!(killed["killed"], true, "{killed}");
    let execution = ledger(&fx.ctx);
    assert_eq!(execution["state"], "cancelled", "{execution}");
    assert_eq!(execution["last_tool"], "kill_session");
}

#[cfg(unix)]
fn wait_until_finished(ctx: &ToolContext, session: &str) -> Value {
    for _ in 0..100 {
        let read = call_tool(
            ctx,
            "read_output",
            &json!({"output_ref": format!("session:{session}:stdout")}),
        );
        assert_eq!(read["ok"], true, "{read}");
        if read["running"] == false {
            return read;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("命令 5 秒还没结束");
}

/// 反例（审查 D01）：默认 finish 只到 verifying，此后除了放弃验收没有出口。
/// 现在：失败、找不到、内容已变的证据一律拒收且状态不动；在当前内容上退出 0 的才收。
#[test]
fn 只有在当前内容上通过的命令才能把任务收成_completed() {
    let fx = fixture();
    let task_id = start_task(&fx.ctx);

    let waiting = finish(&fx.ctx, &task_id, &[]);
    assert_eq!(waiting["ok"], true, "{waiting}");
    assert_eq!(waiting["task"]["status"], "verifying");
    assert_eq!(waiting["verification_required"], true);

    let failed = session_id(&exec(&fx.ctx, FAILING));
    let passed = session_id(&exec(&fx.ctx, PASSING));

    let rejected = finish(&fx.ctx, &task_id, &[&failed]);
    assert_eq!(rejected["ok"], false, "{rejected}");
    assert_eq!(rejected["error"]["code"], "VERIFICATION_REJECTED");
    assert_eq!(rejected_codes(&rejected), ["EVIDENCE_FAILED"]);
    assert_eq!(rejected["error"]["details"]["task_status"], "verifying");
    let candidates: Vec<&str> = rejected["error"]["details"]["evidence_candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .filter_map(|item| item["session_id"].as_str())
        .collect();
    assert_eq!(candidates, [passed.as_str()], "只有通过的那条能当候选");

    let unknown = finish(&fx.ctx, &task_id, &["made-up-session"]);
    assert_eq!(rejected_codes(&unknown), ["EVIDENCE_NOT_FOUND"]);

    // 通过之后又改了文件：那次测的不是现在的内容。
    let patched = call_tool(
        &fx.ctx,
        "apply_patch",
        &json!({"patch": "--- a/README.md\n+++ b/README.md\n@@\n-初始内容\n+改过\n"}),
    );
    assert_eq!(patched["ok"], true, "{patched}");
    let stale = finish(&fx.ctx, &task_id, &[&passed]);
    assert_eq!(rejected_codes(&stale), ["EVIDENCE_STALE"], "{stale}");

    let fresh = session_id(&exec(&fx.ctx, PASSING));
    let completed = finish(&fx.ctx, &task_id, &[&fresh]);
    assert_eq!(completed["ok"], true, "{completed}");
    assert_eq!(completed["task"]["status"], "completed");
    let verification = &completed["change_summary"]["verification"];
    assert_eq!(verification[0]["session_id"], fresh.as_str(), "{completed}");
    assert_eq!(verification[0]["command"], PASSING);
    assert_eq!(verification[0]["exit_code"], 0);
    assert_eq!(
        completed["change_summary"]["risks"],
        json!([]),
        "{completed}"
    );

    // 收成 completed 之后任务位让出来了。
    start_task(&fx.ctx);
}

/// 后台命令没结束不能当证据；读到它结束之后可以。
#[cfg(unix)]
#[test]
fn 后台命令读到终态之后才能当证据() {
    let fx = fixture();
    let task_id = start_task(&fx.ctx);
    let output = call_tool(
        &fx.ctx,
        "exec_command",
        &json!({"cmd": "python3 -c \"import time; time.sleep(0.3)\"", "yield_time_ms": 0}),
    );
    let session = session_id(&output);
    let early = finish(&fx.ctx, &task_id, &[&session]);
    assert_eq!(rejected_codes(&early), ["EVIDENCE_NOT_FINISHED"], "{early}");

    wait_until_finished(&fx.ctx, &session);
    let completed = finish(&fx.ctx, &task_id, &[&session]);
    assert_eq!(completed["ok"], true, "{completed}");
    assert_eq!(completed["task"]["status"], "completed");
}

/// 暂停是真的停：占着任务位，也不放行写入；提示的下一步是这一档调得到的名字。
#[test]
fn 暂停的任务不放行写入() {
    let fx = fixture();
    let task_id = start_task(&fx.ctx);
    let paused = call_tool(
        &fx.ctx,
        "task_manage",
        &json!({"action": "pause", "task_id": task_id}),
    );
    assert_eq!(paused["ok"], true, "{paused}");

    let blocked = exec(&fx.ctx, PASSING);
    assert_eq!(blocked["ok"], false, "{blocked}");
    assert_eq!(blocked["error"]["code"], "TASK_PAUSED");
    let next: Vec<&str> = blocked["harness"]["next_actions"]
        .as_array()
        .expect("next_actions")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(next.contains(&"task_manage:resume"), "{next:?}");

    call_tool(
        &fx.ctx,
        "task_manage",
        &json!({"action": "resume", "task_id": task_id}),
    );
    assert_eq!(exec(&fx.ctx, PASSING)["ok"], true);
}

/// 反例（审查 D02）：提示里有 refresh_baseline，调它却是 INVALID_ARGUMENT。
#[test]
fn 外部修改之后能先看再接纳() {
    let fx = fixture();
    let task_id = start_task(&fx.ctx);
    fs::write(fx.workspace.join("README.md"), "外部修改\n").expect("external");

    let blocked = exec(&fx.ctx, PASSING);
    assert_eq!(
        blocked["error"]["code"], "FILE_CHANGED_EXTERNALLY",
        "{blocked}"
    );
    let next: Vec<&str> = blocked["harness"]["next_actions"]
        .as_array()
        .expect("next_actions")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(next.contains(&"task_manage:refresh_baseline"), "{next:?}");
    // 拒掉的这次也要记在任务名下：写前检查拒的恰恰是任务期间的调用，按任务翻操作记录
    // 要能看到它（真机验收时发现以前这里 task_id 是空的）。
    let log = call_tool(&fx.ctx, "task_manage", &json!({"action": "operation_log"}));
    let rejected = log["operations"]
        .as_array()
        .expect("operations")
        .iter()
        .rev()
        .find(|op| op["kind"] == "rejected")
        .unwrap_or_else(|| panic!("没有 rejected 记录：{log}"));
    assert_eq!(rejected["task_id"], task_id.as_str(), "{rejected}");
    assert_eq!(
        rejected["result_summary"]["code"], "FILE_CHANGED_EXTERNALLY",
        "{rejected}"
    );

    let review = call_tool(
        &fx.ctx,
        "task_manage",
        &json!({"action": "refresh_baseline", "task_id": task_id}),
    );
    assert_eq!(review["ok"], true, "{review}");
    assert_eq!(review["accepted"], false);
    assert_eq!(review["changes"][0]["path"], "README.md", "{review}");
    assert_eq!(review["changes"][0]["status"], "modified");
    assert_eq!(
        exec(&fx.ctx, PASSING)["error"]["code"],
        "FILE_CHANGED_EXTERNALLY",
        "只看不接纳"
    );

    let accepted = call_tool(
        &fx.ctx,
        "task_manage",
        &json!({
            "action": "refresh_baseline",
            "task_id": task_id,
            "accept_fingerprint": review["current"]["fingerprint"],
            "reason": "用户手工改了 README"
        }),
    );
    assert_eq!(accepted["ok"], true, "{accepted}");
    assert_eq!(accepted["accepted"], true);
    assert_eq!(exec(&fx.ctx, PASSING)["ok"], true);
}

/// 反例（审查 §28 第 1 条）：AI 用 exec_command 提交之后，下一次写报 `BASELINE_STALE`，
/// 每次提交都要 refresh_baseline 先看、再接纳，多两次调用，接纳记录里的 changes 却是空的。
#[test]
fn gld_跑的提交只让_head_前进时直接记账() {
    let fx = git_fixture();
    let task_id = start_task(&fx.ctx);
    let before = git(&fx.workspace, &["rev-parse", "HEAD"]);
    assert_eq!(patch_readme(&fx.ctx, "初始内容", "改过")["ok"], true);

    assert_eq!(exec(&fx.ctx, "git add README.md")["command_ok"], true);
    let committed = exec(&fx.ctx, "git commit -q -m edit");
    assert_eq!(committed["command_ok"], true, "{committed}");
    let session = session_id(&committed);
    let after = git(&fx.workspace, &["rev-parse", "HEAD"]);
    assert_ne!(before, after);

    let next = patch_readme(&fx.ctx, "改过", "再改");
    assert_eq!(next["ok"], true, "提交之后直接能写：{next}");
    let status = call_tool(&fx.ctx, "task_manage", &json!({"action": "status"}));
    assert_eq!(status["baseline_matches"], true, "{status}");

    // 记账要留痕：哪条命令把 HEAD 从哪挪到哪。
    let advanced = events_of_kind(&fx.ctx, &task_id, "head_advanced");
    assert_eq!(advanced.len(), 1, "{advanced:?}");
    assert_eq!(
        advanced[0]["input_summary"]["payload"]["session_id"],
        session.as_str()
    );
    assert_eq!(advanced[0]["result_summary"]["before"], before.as_str());
    assert_eq!(advanced[0]["result_summary"]["after"], after.as_str());
    assert!(events_of_kind(&fx.ctx, &task_id, "baseline_refreshed").is_empty());

    // 证据照样认：命令结束时的位置就是记账后的位置。
    let fresh = session_id(&exec(&fx.ctx, PASSING));
    assert_eq!(
        finish(&fx.ctx, &task_id, &[&fresh])["task"]["status"],
        "completed"
    );
}

/// 不经 gld 的提交照旧要先看再接纳；之后 gld 起的命令也不能顺手把它记上账。
#[test]
fn 任务之外的提交照旧报_baseline_stale() {
    let fx = git_fixture();
    let task_id = start_task(&fx.ctx);
    git(
        &fx.workspace,
        &["commit", "-q", "--allow-empty", "-m", "outside"],
    );

    let patched = patch_readme(&fx.ctx, "初始内容", "改过");
    assert_eq!(patched["error"]["code"], "BASELINE_STALE", "{patched}");
    let blocked = exec(&fx.ctx, PASSING);
    assert_eq!(blocked["error"]["code"], "BASELINE_STALE", "{blocked}");
    assert!(events_of_kind(&fx.ctx, &task_id, "head_advanced").is_empty());
}

/// gld 跑的一条命令让 HEAD 动了，但不只是"在原分支上往前提交、文件没变"：照旧要先看再接纳。
#[test]
fn head_变了又不只是往前提交时照旧报_baseline_stale() {
    // 每种情况：任务开始前另开的 `other` 分支上提交什么，再让 gld 跑哪条命令。
    for (what, other_changes_readme, cmd) in [
        // 文件也跟着变了，像 pull、reset --hard。
        ("快进合并带进文件改动", true, "git merge -q --ff-only other"),
        // HEAD 往前、文件没变，但换了分支：不在这次范围，照旧报。
        ("换到前面的分支", false, "git checkout -q -b topic other"),
        // 改写历史：新 HEAD 不在旧 HEAD 之后。
        (
            "amend",
            false,
            "git commit -q --amend --allow-empty -m rewritten",
        ),
    ] {
        let fx = git_fixture();
        git(&fx.workspace, &["checkout", "-q", "-b", "other"]);
        if other_changes_readme {
            fs::write(fx.workspace.join("README.md"), "别的分支改的\n").expect("write");
            git(&fx.workspace, &["commit", "-q", "-am", "other"]);
        } else {
            git(
                &fx.workspace,
                &["commit", "-q", "--allow-empty", "-m", "other"],
            );
        }
        git(&fx.workspace, &["checkout", "-q", "-"]);

        let task_id = start_task(&fx.ctx);
        let ran = exec(&fx.ctx, cmd);
        assert_eq!(ran["command_ok"], true, "{what}：{ran}");
        let next = exec(&fx.ctx, PASSING);
        assert_eq!(next["error"]["code"], "BASELINE_STALE", "{what}：{next}");
        assert!(
            events_of_kind(&fx.ctx, &task_id, "head_advanced").is_empty(),
            "{what}"
        );
    }
}

/// 转了后台、回包之后才提交的，不自动记账：那段时间里别人在终端提交也是这个样子，
/// 分不清是谁。读到它结束之后，下一次写照旧报 `BASELINE_STALE`。
#[cfg(unix)]
#[test]
fn 回包之后才落地的提交不自动记账() {
    let fx = git_fixture();
    let task_id = start_task(&fx.ctx);
    let output = call_tool(
        &fx.ctx,
        "exec_command",
        &json!({
            "cmd": "python3 -c \"import subprocess,time; time.sleep(0.3); subprocess.run(['git','commit','-q','--allow-empty','-m','late'], check=True)\"",
            "yield_time_ms": 0
        }),
    );
    assert_eq!(output["status"], "running", "{output}");
    let read = wait_until_finished(&fx.ctx, &session_id(&output));
    assert_eq!(read["command_ok"], true, "{read}");

    let next = patch_readme(&fx.ctx, "初始内容", "改过");
    assert_eq!(next["error"]["code"], "BASELINE_STALE", "{next}");
    assert!(events_of_kind(&fx.ctx, &task_id, "head_advanced").is_empty());
}

/// "跑测试 → 提交 → finish"：提交没改文件，测的就是现在这份内容，证据照样认（审查 §30）。
#[test]
fn 只多了提交时之前跑的证据照样认() {
    let fx = git_fixture();
    let task_id = start_task(&fx.ctx);
    assert_eq!(patch_readme(&fx.ctx, "初始内容", "改过")["ok"], true);
    let tested = session_id(&exec(&fx.ctx, PASSING));
    assert_eq!(exec(&fx.ctx, "git add README.md")["command_ok"], true);
    assert_eq!(exec(&fx.ctx, "git commit -q -m edit")["command_ok"], true);

    let done = finish(&fx.ctx, &task_id, &[&tested]);
    assert_eq!(done["task"]["status"], "completed", "{done}");
    // 要说清楚：证据跑在提交之前。
    assert!(
        done["warnings"].as_array().is_some_and(|warnings| warnings
            .iter()
            .any(|w| w.as_str().unwrap_or("").contains("提交之前"))),
        "{done}"
    );
}

/// HEAD 往回走（`reset --soft`）不是"只多了提交"：接纳之后，之前的证据照旧不认。
#[test]
fn head_往回走之后之前的证据不认() {
    let fx = git_fixture();
    let task_id = start_task(&fx.ctx);
    assert_eq!(patch_readme(&fx.ctx, "初始内容", "改过")["ok"], true);
    assert_eq!(exec(&fx.ctx, "git add README.md")["command_ok"], true);
    assert_eq!(exec(&fx.ctx, "git commit -q -m edit")["command_ok"], true);
    let tested = session_id(&exec(&fx.ctx, PASSING));
    assert_eq!(
        exec(&fx.ctx, "git reset -q --soft HEAD~1")["command_ok"],
        true
    );

    let review = call_tool(
        &fx.ctx,
        "task_manage",
        &json!({"action": "refresh_baseline", "task_id": task_id}),
    );
    let fingerprint = review["current"]["fingerprint"]
        .as_str()
        .unwrap()
        .to_string();
    let accepted = call_tool(
        &fx.ctx,
        "task_manage",
        &json!({"action": "refresh_baseline", "task_id": task_id,
            "accept_fingerprint": fingerprint, "reason": "测试：撤回提交"}),
    );
    assert_eq!(accepted["accepted"], true, "{accepted}");

    let rejected = finish(&fx.ctx, &task_id, &[&tested]);
    assert_eq!(rejected_codes(&rejected), ["EVIDENCE_STALE"], "{rejected}");
}

/// 第一次收尾没带上 summary（或没写进去），重试时补写一次；已经有了就不再追加。
#[test]
fn 重试收尾补写第一次没写进去的_summary() {
    let fx = fixture();
    let task = start_task(&fx.ctx);
    let passed = session_id(&exec(&fx.ctx, PASSING));
    assert_eq!(
        finish(&fx.ctx, &task, &[&passed])["task"]["status"],
        "completed"
    );
    let with_summary = json!({"action": "finish", "task_id": task, "summary": "补上的交接说明"});
    let again = call_tool(&fx.ctx, "task_manage", &with_summary);
    assert_eq!(again["already_finished"], true, "{again}");
    assert_eq!(again["summary_recorded"], true, "{again}");
    let third = call_tool(&fx.ctx, "task_manage", &with_summary);
    assert_eq!(third["summary_recorded"], false, "{third}");
    assert_eq!(events_of_kind(&fx.ctx, &task, "task_summary").len(), 1);
}

/// finish 回包只带精简的改动摘要；完整的（哈希、大小、事件）在 action=change_summary。
#[test]
fn finish_回包只带精简的改动摘要() {
    let fx = fixture();
    let task = start_task(&fx.ctx);
    let made = exec(
        &fx.ctx,
        "python3 -c \"[open('f%02d.txt' % i, 'w').write('x' * 100) for i in range(60)]\"",
    );
    assert_eq!(made["command_ok"], true, "{made}");
    let passed = session_id(&exec(&fx.ctx, PASSING));
    let done = finish(&fx.ctx, &task, &[&passed]);
    assert_eq!(done["task"]["status"], "completed", "{done}");

    let summary = &done["change_summary"];
    assert_eq!(summary["total_changed_files"], 60, "{summary}");
    let files = summary["files"].as_array().unwrap();
    assert_eq!(files.len(), 50, "最多列 50 个");
    assert_eq!(
        files[0].as_object().unwrap().len(),
        2,
        "只留 path、status：{}",
        files[0]
    );
    assert!(summary.get("evidence").is_none(), "事件留给 change_summary");
    assert!(summary["detail"]
        .as_str()
        .unwrap()
        .contains("change_summary"));
    assert_eq!(summary["verification"].as_array().unwrap().len(), 1);

    let full = call_tool(
        &fx.ctx,
        "task_manage",
        &json!({"action": "change_summary", "task_id": task}),
    );
    assert_eq!(full["files"].as_array().unwrap().len(), 60, "{full}");
    let size = |value: &Value| serde_json::to_vec(value).unwrap().len();
    assert!(
        size(summary) * 3 < size(&full),
        "{} vs {}",
        size(summary),
        size(&full)
    );
}

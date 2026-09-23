//! 任务验收、命令失败传播、基线恢复（审查 D01–D03）的端到端反例。
//!
//! 全走 `call_tool`，也就是客户端那条路：参数校验、策略、写前检查、记账都在里面。

mod common;

use std::fs;
use std::path::PathBuf;

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

fn exec(ctx: &ToolContext, cmd: &str) -> Value {
    call_tool(ctx, "exec_command", &json!({"cmd": cmd}))
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
        &json!({"cmd": "python3 -c \"import time; time.sleep(5)\"", "timeout_ms": 200}),
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

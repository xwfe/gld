//! 一次工具调用"结果如何"的统一口径。
//!
//! 顶层 `ok` 只说明**工具调用本身**成没成：命令退出 7、超时被杀、转了后台还在跑，
//! `ok` 都是 true。台账（Planning 的 `execution`、Harness 的操作记录和任务事件）以前
//! 只看 `ok`，于是 `exit 7` 被记成 completed、`last_error` 为空，后台命令也被记成
//! completed（审查 D03）。这里把"调用成没成"和"命令结果如何"分开，三处共用。

use serde_json::Value;

use crate::harness::model::CommandOutcome;

#[derive(Debug, Clone)]
pub struct CallOutcome {
    /// completed / failed / running / timed_out / cancelled / unknown
    pub state: &'static str,
    /// 工具调用本身（参数、策略、传输）是否成功；命令失败时它仍然是 true。
    pub call_ok: bool,
    /// 返回里带命令会话状态时才有。
    pub command: Option<CommandOutcome>,
    pub error: Option<String>,
}

pub fn classify(output: &Value) -> CallOutcome {
    let call_ok = output.get("ok").and_then(Value::as_bool) != Some(false);
    let message = output
        .pointer("/error/message")
        .and_then(Value::as_str)
        .map(str::to_string);
    if !call_ok {
        // 连接断在半路（远端写操作）：做没做不知道，不能记成失败——失败会让人以为
        // 可以放心重发。
        let unknown = output
            .pointer("/error/details/outcome")
            .and_then(Value::as_str)
            == Some("unknown");
        return CallOutcome {
            state: if unknown { "unknown" } else { "failed" },
            call_ok,
            command: None,
            error: message,
        };
    }
    let Some(command) = command_outcome(output) else {
        return CallOutcome {
            state: "completed",
            call_ok,
            command: None,
            error: None,
        };
    };
    let state = command_state(&command);
    let error = match state {
        "completed" | "running" => None,
        _ => Some(message.unwrap_or_else(|| describe_failure(&command))),
    };
    CallOutcome {
        state,
        call_ok,
        command: Some(command),
        error,
    }
}

/// 返回里的命令会话状态。既没有 `termination_reason` 也没有 `command_ok` 的不是命令结果。
pub fn command_outcome(output: &Value) -> Option<CommandOutcome> {
    let object = output.as_object()?;
    if !object.contains_key("termination_reason") && !object.contains_key("command_ok") {
        return None;
    }
    // kill_session 发了信号但进程还没退时是 terminating：还没有终态。
    let still_running = matches!(
        object.get("status").and_then(Value::as_str),
        Some("running" | "terminating")
    ) || object.get("running").and_then(Value::as_bool) == Some(true);
    let status = if still_running {
        "running"
    } else {
        object
            .get("termination_reason")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    };
    Some(CommandOutcome {
        session_id: object
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        status: status.to_string(),
        exit_code: object.get("exit_code").and_then(Value::as_i64),
        command_ok: object.get("command_ok").and_then(Value::as_bool),
        workspace_writes_since_start: object
            .get("workspace_writes_since_start")
            .and_then(Value::as_u64),
    })
}

/// 一句话说清命令本身怎样了，放进回包顶层的 `command_summary`。
///
/// 顶层 `ok: true`、`status: "exited"` 都只说明工具调用成了，命令退出 101 时照样是这样：
/// D10 实测 ChatGPT 说"第一次看容易误以为命令成功"。`command_ok` / `exit_code` 才是命令结果，
/// 这一句把它们翻成人话，不改那两个字段的契约。不是命令结果的回包返回 `None`。
pub fn command_summary(output: &Value) -> Option<String> {
    if output.get("ok").and_then(Value::as_bool) == Some(false) {
        return None;
    }
    let command = command_outcome(output)?;
    Some(match command_state(&command) {
        "completed" => "command succeeded (exit code 0)".into(),
        "running" => {
            "command is still running; read_output for more, kill_session to stop it".into()
        }
        _ => format!(
            "command FAILED: {} (the tool call itself worked; see exit_code / command_ok)",
            describe_failure(&command)
        ),
    })
}

fn command_state(command: &CommandOutcome) -> &'static str {
    match command.status.as_str() {
        "running" => "running",
        "exited" if command.exit_code == Some(0) => "completed",
        "exited" => "failed",
        "timeout" => "timed_out",
        // interrupted：gld 退出时自己停掉的（运行记录，审查 D09）。结局是确定的——没跑完。
        "killed" | "interrupted" => "cancelled",
        // 服务重启丢了会话、或者 gld 没来得及记下结局：进程后来怎样没人看见。
        "server_restart" | "unknown" => "unknown",
        _ => "failed",
    }
}

fn describe_failure(command: &CommandOutcome) -> String {
    match (command.status.as_str(), command.exit_code) {
        ("exited", Some(code)) => format!("command exited with code {code}"),
        ("exited", None) => "command exited without an exit code (killed by a signal)".into(),
        ("timeout", _) => "command timed out and was killed".into(),
        ("killed", _) => "command was killed".into(),
        ("interrupted", _) => "command was stopped because the service process running it exited".into(),
        ("unknown", _) => {
            "how the command ended was never recorded (the service process running it went away first); it may still be running".into()
        }
        (status, _) => format!("command ended as {status}"),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// 顶层 ok: true 时一眼看不出命令失败了；command_summary 用一句话说清，不是命令结果的不加。
    #[test]
    fn command_summary_says_what_the_command_did() {
        let failed = json!({"ok": true, "status": "exited", "termination_reason": "exited", "exit_code": 101, "command_ok": false});
        let text = command_summary(&failed).expect("summary");
        assert!(text.starts_with("command FAILED"), "{text}");
        assert!(text.contains("101"), "{text}");
        let passed =
            json!({"ok": true, "termination_reason": "exited", "exit_code": 0, "command_ok": true});
        assert_eq!(
            command_summary(&passed).as_deref(),
            Some("command succeeded (exit code 0)")
        );
        let running = json!({"ok": true, "status": "running", "termination_reason": "running"});
        assert!(command_summary(&running)
            .expect("running")
            .contains("still running"));
        assert_eq!(command_summary(&json!({"ok": true, "content": "x"})), None);
        assert_eq!(
            command_summary(&json!({"ok": false, "termination_reason": "exited"})),
            None
        );
    }

    #[test]
    fn a_non_zero_exit_is_a_failed_call_even_though_ok_is_true() {
        let outcome = classify(&json!({
            "ok": true, "session_id": "s1", "status": "exited",
            "termination_reason": "exited", "exit_code": 7, "command_ok": false
        }));
        assert!(outcome.call_ok);
        assert_eq!(outcome.state, "failed");
        assert_eq!(outcome.error.as_deref(), Some("command exited with code 7"));
        assert_eq!(outcome.command.unwrap().exit_code, Some(7));
    }

    #[test]
    fn each_way_a_command_can_end_has_its_own_state() {
        let cases = [
            (
                json!({"status": "running", "termination_reason": "running", "command_ok": null}),
                "running",
            ),
            (
                json!({"status": "exited", "termination_reason": "exited", "exit_code": 0, "command_ok": true}),
                "completed",
            ),
            (
                json!({"status": "exited", "termination_reason": "timeout", "command_ok": false}),
                "timed_out",
            ),
            (
                json!({"status": "killed", "termination_reason": "killed", "command_ok": false}),
                "cancelled",
            ),
            // 发了信号还没退：不是终态。
            (
                json!({"status": "terminating", "termination_reason": "killed", "command_ok": false}),
                "running",
            ),
            (
                json!({"status": "spawn_failed", "termination_reason": "spawn_failed", "command_ok": false}),
                "failed",
            ),
            (
                json!({"status": "exited", "termination_reason": "server_restart", "command_ok": false}),
                "unknown",
            ),
            // 运行记录（审查 D09）：gld 退出时停掉的是 cancelled；没来得及记的是 unknown。
            (
                json!({"running": false, "termination_reason": "interrupted", "command_ok": false}),
                "cancelled",
            ),
            (
                json!({"running": false, "termination_reason": "unknown", "command_ok": null}),
                "unknown",
            ),
            // read_output 只说 running 与否。
            (
                json!({"running": true, "termination_reason": "running", "command_ok": null}),
                "running",
            ),
        ];
        for (mut output, expected) in cases {
            output["ok"] = json!(true);
            assert_eq!(classify(&output).state, expected, "{output}");
        }
    }

    #[test]
    fn a_lost_connection_is_unknown_and_other_errors_are_failed() {
        let unknown = classify(&json!({
            "ok": false,
            "error": {"code": "REMOTE_OUTCOME_UNKNOWN", "message": "lost", "details": {"outcome": "unknown"}}
        }));
        assert_eq!(unknown.state, "unknown");
        assert!(!unknown.call_ok);

        let failed =
            classify(&json!({"ok": false, "error": {"code": "PATCH_FAILED", "message": "no"}}));
        assert_eq!(failed.state, "failed");
        assert_eq!(failed.error.as_deref(), Some("no"));
    }

    #[test]
    fn a_result_without_command_fields_is_just_completed() {
        let outcome = classify(&json!({"ok": true, "affected_files": []}));
        assert_eq!(outcome.state, "completed");
        assert!(outcome.command.is_none());
    }
}

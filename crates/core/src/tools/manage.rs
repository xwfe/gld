use serde_json::Value;

use crate::tools::workspace::WorkspaceError;
use crate::tools::ToolContext;

use super::{history, planning};

pub const HISTORY_ACTIONS: &[&str] = &["bootstrap", "checkpoint", "validate", "search", "read"];

pub const PLANNING_ACTIONS: &[&str] = &[
    "state",
    "create_goal",
    "update_goal",
    "create_plan",
    "update_plan",
    "request_goal_review",
    "request_plan_review",
];

pub const TASK_ACTIONS: &[&str] = &[
    "status",
    "operation_log",
    "project_state",
    "start",
    "update",
    "pause",
    "resume",
    "finish",
    "context",
    "events",
    "change_summary",
];

fn action<'a>(args: &'a Value, label: &str, allowed: &[&str]) -> Result<&'a str, WorkspaceError> {
    args.get("action")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            WorkspaceError::invalid_argument(format!(
                "{label} action is required. Valid actions: {}",
                allowed.join(", ")
            ))
        })
}

/// 不认识的 action，把可选值一并列出来。
///
/// 读这条错误的主要是 AI。只说一句"不认识"，它得再去翻一遍工具 schema 才知道
/// 该填什么；把候选写在错误里，它下一次调用就能自己改对。人用 `gld tool call`
/// 手工试的时候同理。
fn unknown_action(label: &str, got: &str, allowed: &[&str]) -> WorkspaceError {
    WorkspaceError::invalid_argument(format!(
        "Unknown {label} action: {got}. Valid actions: {}",
        allowed.join(", ")
    ))
}

pub fn history_manage(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    match action(args, "history", HISTORY_ACTIONS)? {
        "bootstrap" => history::bootstrap(ctx, args),
        "checkpoint" => history::checkpoint(ctx, args),
        "validate" => history::validate(ctx, args),
        "search" => history::search(ctx, args),
        "read" => history::read(ctx, args),
        other => Err(unknown_action("history", other, HISTORY_ACTIONS)),
    }
}

pub fn planning_manage(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    match action(args, "planning", PLANNING_ACTIONS)? {
        "state" => planning::planning_state(ctx, args),
        "create_goal" => planning::create_goal(ctx, args),
        "update_goal" => planning::update_goal(ctx, args),
        "create_plan" => planning::create_plan(ctx, args),
        "update_plan" => planning::update_plan(ctx, args),
        "request_goal_review" => planning::request_goal_review(ctx, args),
        "request_plan_review" => planning::request_plan_review(ctx, args),
        other => Err(unknown_action("planning", other, PLANNING_ACTIONS)),
    }
}

pub fn task_manage(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let tool_name = match action(args, "task", TASK_ACTIONS)? {
        "status" => "harness_status",
        "operation_log" => "operation_log",
        "project_state" => "project_state",
        "start" => "start_task",
        "update" => "update_task",
        "pause" => "pause_task",
        "resume" => "resume_task",
        "finish" => "finish_task",
        "context" => "task_context",
        "events" => "list_task_events",
        "change_summary" => "change_summary",
        other => return Err(unknown_action("task", other, TASK_ACTIONS)),
    };
    crate::harness::tools::call(ctx, tool_name, args)
}

pub fn action_is_mutating(name: &str, args: &Value) -> Option<bool> {
    let action = args.get("action").and_then(Value::as_str)?;
    match name {
        "history_manage" => Some(matches!(action, "bootstrap" | "checkpoint" | "validate")),
        "planning_manage" => Some(!matches!(action, "state")),
        "task_manage" => Some(matches!(
            action,
            "start" | "update" | "pause" | "resume" | "finish"
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn context() -> (tempfile::TempDir, tempfile::TempDir, ToolContext) {
        let workspace = tempfile::tempdir().expect("workspace");
        let harness = tempfile::tempdir().expect("harness");
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");
        (workspace, harness, ctx)
    }

    #[test]
    fn planning_manager_routes_state_and_create_goal_actions() {
        let (_workspace, _harness, ctx) = context();

        let state = planning_manage(&ctx, &json!({"action":"state"})).expect("state");
        assert_eq!(state["ok"], true);
        assert_eq!(state["state"]["mode"], "direct");

        let created = planning_manage(
            &ctx,
            &json!({
                "action": "create_goal",
                "title": "Stable API",
                "objective": "Route through the v2 manager"
            }),
        )
        .expect("goal");
        assert_eq!(created["ok"], true);
        assert_eq!(created["goal"]["title"], "Stable API");
    }

    #[test]
    fn task_manager_routes_read_only_status_action() {
        let (_workspace, _harness, ctx) = context();
        let status = task_manage(&ctx, &json!({"action":"status"})).expect("status");
        assert_eq!(status["ok"], true);
    }

    #[test]
    fn managers_reject_unknown_actions() {
        let (_workspace, _harness, ctx) = context();
        let error = planning_manage(&ctx, &json!({"action":"explode"})).expect_err("invalid");
        assert_eq!(error.to_error_value()["code"], "INVALID_ARGUMENT");
        let message = error.to_error_value()["message"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            message.contains("create_goal"),
            "报错要把可选 action 列出来，否则调用方只能去翻 schema：{message}"
        );
    }

    /// 报错里列出来的 action 必须真的能路由到。
    ///
    /// 候选列表和 match 分支是两份东西，加了新 action 只改一处的话，
    /// 错误信息就会开始骗人——照着它填反而还是"Unknown"。
    #[test]
    fn every_advertised_action_is_routable() {
        type Manager = fn(&ToolContext, &Value) -> Result<Value, WorkspaceError>;

        let (_workspace, _harness, ctx) = context();
        let cases: [(&str, &[&str], Manager); 3] = [
            ("history", HISTORY_ACTIONS, history_manage),
            ("planning", PLANNING_ACTIONS, planning_manage),
            ("task", TASK_ACTIONS, task_manage),
        ];
        for (label, actions, manager) in cases {
            for action in actions {
                // 缺参数之类的失败没关系，只要不是"不认识这个 action"。
                if let Err(error) = manager(&ctx, &json!({ "action": action })) {
                    let message = error.to_error_value()["message"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    assert!(
                        !message.starts_with(&format!("Unknown {label} action")),
                        "{label} 宣称支持 {action}，实际路由不到：{message}"
                    );
                }
            }
        }
    }
}

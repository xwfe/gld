use serde_json::{json, Value};

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
    "refresh_baseline",
];

/// task_manage 的动作和它背后的 Harness 工具，一一对应。
const TASK_ACTION_TOOLS: &[(&str, &str)] = &[
    ("status", "harness_status"),
    ("operation_log", "operation_log"),
    ("project_state", "project_state"),
    ("start", "start_task"),
    ("update", "update_task"),
    ("pause", "pause_task"),
    ("resume", "resume_task"),
    ("finish", "finish_task"),
    ("context", "task_context"),
    ("events", "list_task_events"),
    ("change_summary", "change_summary"),
    ("refresh_baseline", "refresh_baseline"),
];

/// Harness 工具在 task_manage 里叫什么动作。
///
/// compact / core 档不单独暴露 `resume_task`、`refresh_baseline` 这些名字，只有
/// `task_manage`；状态里的"下一步"照原名写，客户端就找不到能调的东西（审查 D02）。
pub fn task_action_for_tool(tool: &str) -> Option<&'static str> {
    TASK_ACTION_TOOLS
        .iter()
        .find(|(_, name)| *name == tool)
        .map(|(action, _)| *action)
}

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
    let action = action(args, "task", TASK_ACTIONS)?;
    let Some((_, tool_name)) = TASK_ACTION_TOOLS.iter().find(|(name, _)| *name == action) else {
        return Err(unknown_action("task", action, TASK_ACTIONS));
    };
    reject_other_actions_arguments(action, tool_name, args)?;
    crate::harness::tools::call(ctx, tool_name, args)
}

/// 一个动作收到了只有别的动作才用的参数：当场说，并指出它归哪个动作。
///
/// task_manage 的 schema 是十几个动作参数的并集，分发层的未知参数检查按并集查，拦不住
/// "参数对、动作不对"。以前 `finish` 带 `completed_steps` 能过检查，任务也完成了，步骤却
/// 一条没记——返回的 `completed_steps` 是空的，调用方看不懂（D10 实测，ChatGPT 报的）。
/// 每个动作收哪些参数直接取它背后那个工具的 schema，不另写一份表。
fn reject_other_actions_arguments(
    action: &str,
    tool: &str,
    args: &Value,
) -> Result<(), WorkspaceError> {
    let Some(given) = args.as_object() else {
        return Ok(());
    };
    let takes = |tool: &str| -> Vec<String> {
        crate::tools::registry::input_schema(tool)
            .get("properties")
            .and_then(Value::as_object)
            .map(|properties| properties.keys().cloned().collect())
            .unwrap_or_default()
    };
    let accepted = takes(tool);
    let misplaced: Vec<&String> = given
        .keys()
        .filter(|name| *name != "action" && !name.starts_with('_') && !accepted.contains(name))
        .collect();
    if misplaced.is_empty() {
        return Ok(());
    }
    let owners: Vec<String> = misplaced
        .iter()
        .map(|name| {
            let actions: Vec<&str> = TASK_ACTION_TOOLS
                .iter()
                .filter(|(_, other)| takes(other).contains(name))
                .map(|(other, _)| *other)
                .collect();
            if actions.is_empty() {
                format!("{name} (no action takes it)")
            } else {
                format!("{name} (use action={})", actions.join(" / action="))
            }
        })
        .collect();
    Err(WorkspaceError::ToolDetails {
        code: "INVALID_ARGUMENT",
        message: format!(
            "task_manage action={action} does not take {}. It takes: {}",
            owners.join(", "),
            accepted.join(", ")
        ),
        category: "validation",
        retryable: false,
        details: json!({
            "action": action,
            "unknown_arguments": misplaced,
            "accepted_arguments": accepted,
            // 被拒的调用什么都没做：先用对应的动作记，再重发这一次。
            "executed": false
        }),
    })
}

pub fn action_is_mutating(name: &str, args: &Value) -> Option<bool> {
    // refresh_baseline 不带 accept_fingerprint 只是看，带了才改任务记账。
    let accepts_baseline = || args.get("accept_fingerprint").is_some();
    if name == "refresh_baseline" {
        return Some(accepts_baseline());
    }
    let action = args.get("action").and_then(Value::as_str)?;
    match name {
        "history_manage" => Some(matches!(action, "bootstrap" | "checkpoint" | "validate")),
        "planning_manage" => Some(!matches!(action, "state")),
        "task_manage" => Some(
            matches!(action, "start" | "update" | "pause" | "resume" | "finish")
                || (action == "refresh_baseline" && accepts_baseline()),
        ),
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

    fn started_task(ctx: &ToolContext) -> String {
        let started =
            task_manage(ctx, &json!({"action": "start", "objective": "验收"})).expect("start");
        started["task"]["id"].as_str().expect("task id").to_string()
    }

    /// `finish` 带 `completed_steps`：以前收下不用，任务照样结束、步骤一条没记。现在当场拒，
    /// 说清它归 `update`，任务状态不动。
    #[test]
    fn an_argument_of_another_action_is_refused_and_named() {
        let (_workspace, _harness, ctx) = context();
        let task_id = started_task(&ctx);
        let refused = task_manage(
            &ctx,
            &json!({"action": "finish", "task_id": task_id, "completed_steps": ["写测试"]}),
        )
        .expect_err("finish 不收 completed_steps");
        let text = format!("{refused:?}");
        assert!(text.contains("action=update"), "{text}");
        assert!(text.contains("completed_steps"), "{text}");
        let status = task_manage(&ctx, &json!({"action": "status"})).expect("status");
        assert_eq!(status["task_state"], "active", "{status}");

        // 步骤先用 update 记，再 finish：两步都生效。
        let updated = task_manage(
            &ctx,
            &json!({"action": "update", "task_id": task_id, "completed_steps": ["写测试"]}),
        )
        .expect("update");
        assert_eq!(updated["task"]["completed_steps"], json!(["写测试"]));
    }

    /// `finish` 的 `summary` 以前声明了却没人读；现在记成一条 task_summary 事件，先脱敏。
    #[test]
    fn a_finish_summary_is_kept_as_a_task_event() {
        let (_workspace, _harness, ctx) = context();
        let task_id = started_task(&ctx);
        let finished = task_manage(
            &ctx,
            &json!({
                "action": "finish",
                "task_id": task_id,
                "summary": "改了分词；token=ghp_0123456789abcdefghijklmnopqrstuvwxyzAB 别留"
            }),
        )
        .expect("finish");
        assert_eq!(finished["summary_recorded"], true, "{finished}");
        let events =
            task_manage(&ctx, &json!({"action": "events", "task_id": task_id})).expect("events");
        let summary = events["events"]
            .as_array()
            .expect("events")
            .iter()
            .find(|event| event["kind"] == "task_summary")
            .unwrap_or_else(|| panic!("没有 task_summary 事件：{events}"));
        let text = summary["input_summary"]["payload"]["summary"]
            .as_str()
            .expect("summary");
        assert!(text.starts_with("改了分词"), "{text}");
        assert!(!text.contains("ghp_0123456789"), "没脱敏：{text}");
    }

    /// 每个动作收的参数都在 task_manage 的 schema 里：并集检查先过了，才轮得到按动作的检查，
    /// 不然合法参数会在分发层就被当成未知参数拒掉。
    #[test]
    fn every_action_argument_is_in_the_task_manage_schema() {
        let union = crate::tools::registry::input_schema("task_manage");
        let union = union["properties"].as_object().expect("properties");
        for (action, tool) in TASK_ACTION_TOOLS {
            let schema = crate::tools::registry::input_schema(tool);
            for name in schema["properties"].as_object().expect(tool).keys() {
                assert!(
                    union.contains_key(name),
                    "{action}: {name} 不在 task_manage 里"
                );
            }
        }
    }
}

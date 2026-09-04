use gld_core::app::PlanStepUpdate;
use gld_core::planning::{
    Goal, GoalStatus, Plan, PlanStatus, PlanStepStatus, PlanningMode, PlanningState,
};
use gld_daemon::Request;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::{split_list, Ctx};
use crate::cli::{GoalCmd, PlanCmd, PlanningCmd};
use crate::error::{CliError, CliResult};
use crate::output::or_dash;

pub async fn run(ctx: &mut Ctx, command: PlanningCmd) -> CliResult {
    let target = ctx.target.clone();
    match command {
        PlanningCmd::Show => {
            let state: PlanningState = ctx
                .backend
                .call_typed(Request::PlanningState { target })
                .await?;
            if ctx.out.json_or(&state) {
                return Ok(());
            }
            print_state(ctx, &state);
            Ok(())
        }
        PlanningCmd::Mode { mode } => {
            let mode: PlanningMode = parse_enum(&mode, "模式只能是 direct | plan | goal")?;
            let state: PlanningState = ctx
                .backend
                .call_typed(Request::SetPlanningMode { target, mode })
                .await?;
            if !ctx.out.json_or(&state) {
                ctx.out.line(format!("模式已切换为 {mode:?}。"));
                if mode == PlanningMode::Plan {
                    ctx.out.note(
                        "Plan 模式下服务端拒绝写入与命令执行，工作区内仍在运行的命令会话已终止。",
                    );
                }
            }
            Ok(())
        }
        PlanningCmd::Goal(command) => goal(ctx, command).await,
        PlanningCmd::Plan(command) => plan(ctx, command).await,
    }
}

async fn goal(ctx: &mut Ctx, command: GoalCmd) -> CliResult {
    let target = ctx.target.clone();
    let verb = match &command {
        GoalCmd::Create { .. } => "已创建 Goal",
        GoalCmd::Update { .. } => "已更新 Goal",
        GoalCmd::Accept { .. } => "验收通过，Goal 已归档",
        GoalCmd::Reject { .. } => "已驳回，Goal 回到 active",
    };
    let goal: Goal = match command {
        GoalCmd::Create {
            title,
            objective,
            criteria,
            constraints,
        } => {
            ctx.backend
                .call_typed(Request::CreateGoal {
                    target,
                    title,
                    objective,
                    success_criteria: criteria,
                    constraints,
                })
                .await?
        }
        GoalCmd::Update {
            goal_id,
            title,
            objective,
            status,
            constraints,
            done,
            focus,
        } => {
            let status: Option<GoalStatus> = status
                .map(|s| parse_enum(&s, "状态只能是 active | paused | completed | awaiting_acceptance | archived | cancelled"))
                .transpose()?;
            ctx.backend
                .call_typed(Request::UpdateGoal {
                    target,
                    goal_id,
                    title,
                    objective,
                    status,
                    constraints,
                    completed_criteria_ids: done.map(|d| split_list(&d)),
                    focus,
                })
                .await?
        }
        GoalCmd::Accept { goal_id } => {
            ctx.backend
                .call_typed(Request::AcceptGoalReview { target, goal_id })
                .await?
        }
        GoalCmd::Reject { goal_id, feedback } => {
            ctx.backend
                .call_typed(Request::RejectGoalReview {
                    target,
                    goal_id,
                    feedback,
                })
                .await?
        }
    };
    if !ctx.out.json_or(&goal) {
        ctx.out.line(format!("{verb}。"));
        ctx.out.line("");
        print_goal(ctx, &goal, "");
        // 只在还能往下做的时候给建议。已归档 / 已取消的 Goal 再喊"加计划"是噪音。
        if matches!(goal.status, GoalStatus::Active) {
            ctx.out.line("");
            ctx.out.note(format!(
                "给它加计划：gld planning plan create --goal {} --title <标题> --objective <目标> --step <步骤>",
                goal.id
            ));
        }
    }
    Ok(())
}

async fn plan(ctx: &mut Ctx, command: PlanCmd) -> CliResult {
    let target = ctx.target.clone();
    let verb = match &command {
        PlanCmd::Create { .. } => "已创建 Plan",
        PlanCmd::Update { .. } => "已更新 Plan",
        PlanCmd::Accept { .. } => "验收通过，Plan 已归档",
        PlanCmd::Reject { .. } => "已驳回，Plan 回到 active",
    };
    let plan: Plan = match command {
        PlanCmd::Create {
            title,
            objective,
            goal,
            steps,
        } => {
            ctx.backend
                .call_typed(Request::CreatePlan {
                    target,
                    goal_id: goal,
                    title,
                    objective,
                    steps,
                })
                .await?
        }
        PlanCmd::Update {
            plan_id,
            status,
            steps,
            focus,
        } => {
            let status: Option<PlanStatus> = status
                .map(|s| parse_enum(&s, "状态只能是 draft | active | paused | completed | awaiting_acceptance | archived | cancelled"))
                .transpose()?;
            let step_updates = steps
                .iter()
                .map(|spec| parse_step(spec))
                .collect::<CliResult<Vec<_>>>()?;
            ctx.backend
                .call_typed(Request::UpdatePlan {
                    target,
                    plan_id,
                    status,
                    step_updates,
                    focus,
                })
                .await?
        }
        PlanCmd::Accept { plan_id } => {
            ctx.backend
                .call_typed(Request::AcceptPlanReview { target, plan_id })
                .await?
        }
        PlanCmd::Reject { plan_id, feedback } => {
            ctx.backend
                .call_typed(Request::RejectPlanReview {
                    target,
                    plan_id,
                    feedback,
                })
                .await?
        }
    };
    if !ctx.out.json_or(&plan) {
        ctx.out.line(format!("{verb}。"));
        ctx.out.line("");
        print_plan(ctx, &plan, "");
        // 同理：计划已经进入验收或归档了就别再提示推进步骤。
        let still_working = matches!(plan.status, PlanStatus::Draft | PlanStatus::Active);
        if let Some(step) = plan.steps.iter().find(|step| {
            still_working
                && matches!(
                    step.status,
                    PlanStepStatus::Pending | PlanStepStatus::InProgress
                )
        }) {
            ctx.out.line("");
            ctx.out.note(format!(
                "推进一步：gld planning plan update {} --step {}=completed",
                plan.id, step.id
            ));
        }
    }
    Ok(())
}

// ------------------------------------------------------------- 人类可读输出
//
// 这些命令原来不带 --json 也直接甩 serde_json::to_string_pretty，
// 一屏 id 和 null 里找不着重点。--json 已经能给脚本用了，
// 不带它的时候就该按人的读法来。

/// 状态枚举在 JSON 里就是 snake_case，借 serde 拿这个字符串，
/// 不再单独维护一份 Display。
fn status_text<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".into())
}

fn checkbox(done: bool) -> &'static str {
    if done {
        "☑"
    } else {
        "☐"
    }
}

/// 列表里的聚焦标记。当前 Goal / Plan 决定 AI 的写操作绑到哪儿，
/// 得一眼看得出来；只显示一个对象时不需要它，传空串。
fn focus_mark(focused: bool) -> &'static str {
    if focused {
        "● "
    } else {
        "  "
    }
}

fn print_goal(ctx: &Ctx, goal: &Goal, mark: &str) {
    ctx.out.line(format!(
        "{mark}{}  {}",
        ctx.out.bold(&goal.title),
        ctx.out.state(&status_text(&goal.status))
    ));
    let criteria = if goal.success_criteria.is_empty() {
        "-".to_string()
    } else {
        goal.success_criteria
            .iter()
            .map(|item| format!("{} {}", checkbox(item.completed), item.text))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut rows = vec![
        ("  ID", goal.id.clone()),
        ("  目标", goal.objective.clone()),
        ("  验收", criteria),
    ];
    if !goal.constraints.is_empty() {
        rows.push(("  约束", goal.constraints.join("\n")));
    }
    if let Some(feedback) = &goal.review_feedback {
        rows.push(("  驳回意见", feedback.clone()));
    }
    ctx.out.kv(&rows);
}

fn print_plan(ctx: &Ctx, plan: &Plan, mark: &str) {
    ctx.out.line(format!(
        "{mark}{}  {}",
        ctx.out.bold(&plan.title),
        ctx.out.state(&status_text(&plan.status))
    ));
    let steps = if plan.steps.is_empty() {
        "-".to_string()
    } else {
        plan.steps
            .iter()
            .map(|step| {
                let done = matches!(step.status, PlanStepStatus::Completed);
                let notes = step
                    .notes
                    .as_deref()
                    .filter(|notes| !notes.trim().is_empty())
                    .map(|notes| format!("  {notes}"))
                    .unwrap_or_default();
                format!(
                    "{} {}  {}{notes}",
                    checkbox(done),
                    step.title,
                    ctx.out.dim(&status_text(&step.status))
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut rows = vec![
        ("  ID", plan.id.clone()),
        ("  目标", plan.objective.clone()),
        ("  步骤", steps),
    ];
    if let Some(goal_id) = &plan.goal_id {
        rows.push(("  所属 Goal", goal_id.clone()));
    }
    if let Some(feedback) = &plan.review_feedback {
        rows.push(("  驳回意见", feedback.clone()));
    }
    ctx.out.kv(&rows);
}

fn print_state(ctx: &Ctx, state: &PlanningState) {
    let mode = status_text(&state.mode);
    ctx.out.kv(&[
        ("模式", ctx.out.bold(&mode)),
        (
            "",
            ctx.out
                .dim("direct = 自由改；plan = 只读，AI 先出计划；goal = 写操作须绑定当前 Goal"),
        ),
    ]);

    if state.goals.is_empty() && state.plans.is_empty() {
        ctx.out.line("");
        ctx.out.line("还没有 Goal 或 Plan。");
        ctx.out.note(
            "AI 在 plan / goal 模式下会自己提交；手工建：gld planning goal create --title <标题> --objective <目标>",
        );
        return;
    }

    if !state.goals.is_empty() {
        ctx.out.line("");
        ctx.out.line(format!("Goal（{}）", state.goals.len()));
        for goal in &state.goals {
            print_goal(
                ctx,
                goal,
                focus_mark(state.focus_goal_id.as_deref() == Some(goal.id.as_str())),
            );
        }
    }

    if !state.plans.is_empty() {
        ctx.out.line("");
        ctx.out.line(format!("Plan（{}）", state.plans.len()));
        for plan in &state.plans {
            print_plan(
                ctx,
                plan,
                focus_mark(state.focus_plan_id.as_deref() == Some(plan.id.as_str())),
            );
        }
    }

    // 执行台账：AI 最后一次写操作停在哪儿。没动过就不占版面。
    let ledger = &state.execution;
    if !ledger.state.is_empty() || ledger.last_tool.is_some() {
        ctx.out.line("");
        let mut rows = vec![(
            "最后动作",
            format!(
                "{} · {}",
                ledger.last_tool.as_deref().unwrap_or("-"),
                or_dash(&ledger.state)
            ),
        )];
        if let Some(error) = &ledger.last_error {
            rows.push(("报错", error.clone()));
        }
        if !ledger.changed_files.is_empty() {
            rows.push(("改动文件", ledger.changed_files.join("\n")));
        }
        ctx.out.kv(&rows);
    }

    if state.focus_goal_id.is_some() || state.focus_plan_id.is_some() {
        ctx.out.line("");
        ctx.out.note("● = 当前聚焦，AI 的写操作绑在它上面");
    }
}

/// `STEP_ID=STATUS[:备注]`
fn parse_step(spec: &str) -> CliResult<PlanStepUpdate> {
    let (step_id, rest) = spec.split_once('=').ok_or_else(|| {
        CliError::new(format!(
            "步骤格式应为 STEP_ID=STATUS[:备注]，收到「{spec}」"
        ))
    })?;
    let (status, notes) = match rest.split_once(':') {
        Some((status, notes)) => (status, Some(notes.to_string())),
        None => (rest, None),
    };
    let status: PlanStepStatus = parse_enum(
        status,
        "步骤状态只能是 pending | in_progress | completed | blocked | skipped",
    )?;
    Ok(PlanStepUpdate {
        step_id: step_id.trim().to_string(),
        status,
        notes,
    })
}

/// 借 serde 把 `snake_case` 字符串解析成枚举，错误信息由调用方给。
fn parse_enum<T: DeserializeOwned>(value: &str, hint: &str) -> CliResult<T> {
    serde_json::from_value(Value::String(value.trim().to_ascii_lowercase()))
        .map_err(|_| CliError::new(format!("{hint}（收到「{value}」）")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_step_specs() {
        let update = parse_step("s1=in_progress:写测试").unwrap();
        assert_eq!(update.step_id, "s1");
        assert_eq!(update.status, PlanStepStatus::InProgress);
        assert_eq!(update.notes.as_deref(), Some("写测试"));
        assert!(parse_step("s1").is_err());
        assert!(parse_step("s1=flying").is_err());
    }
}

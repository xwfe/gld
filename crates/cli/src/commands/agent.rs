//! `gld agent`：在本机看、放行、拒绝、停委派任务（RFC-0009）。

use gld_core::agents::store::{AgentTask, State};
use gld_core::agents::TaskDetail;
use gld_daemon::Request;

use super::Ctx;
use crate::cli::AgentCmd;
use crate::error::CliResult;
use crate::output::human_duration;

pub async fn run(ctx: &mut Ctx, command: AgentCmd) -> CliResult {
    match command {
        AgentCmd::List => list(ctx).await,
        AgentCmd::Show { task } => show(ctx, task).await,
        AgentCmd::Approve { task } => {
            let task: AgentTask = ctx
                .backend
                .call_typed(Request::ApproveAgentTask { task_id: task })
                .await?;
            done(ctx, &task, "已放行，Agent 起来了")
        }
        AgentCmd::Deny { task } => {
            let task: AgentTask = ctx
                .backend
                .call_typed(Request::DenyAgentTask { task_id: task })
                .await?;
            done(ctx, &task, "已拒")
        }
        AgentCmd::Stop { task } => {
            let task: AgentTask = ctx
                .backend
                .call_typed(Request::StopAgentTask { task_id: task })
                .await?;
            let what = if task.state == State::Running {
                "叫停了，等它收尾（几秒内状态变成 stopped）"
            } else {
                "已作废"
            };
            done(ctx, &task, what)
        }
    }
}

fn done(ctx: &Ctx, task: &AgentTask, what: &str) -> CliResult {
    if !ctx.out.json_or(task) {
        ctx.out.line(format!(
            "{what}：{}（{}，{}）",
            short(&task.task_id),
            task.workspace_name,
            task.agent
        ));
    }
    Ok(())
}

async fn list(ctx: &mut Ctx) -> CliResult {
    let tasks: Vec<AgentTask> = ctx.backend.call_typed(Request::ListAgentTasks).await?;
    if ctx.out.json_or(&tasks) {
        return Ok(());
    }
    if tasks.is_empty() {
        ctx.out
            .line("还没有委派任务。AI 要能委派，项目得先开：gld set <项目> agent-delegation=ask");
        return Ok(());
    }
    let now = gld_core::agents::run::now_ms();
    for task in &tasks {
        let age = human_duration(now.saturating_sub(task.created_at_ms) / 1000);
        let preview: String = task.prompt_preview.chars().take(40).collect();
        ctx.out.line(format!(
            "{}  {}  {}  {}  {age}前  {}",
            short(&task.task_id),
            task.workspace_name,
            task.agent,
            state_text(task.state),
            preview.replace('\n', " ")
        ));
    }
    let waiting: Vec<&AgentTask> = tasks
        .iter()
        .filter(|task| task.state == State::WaitingLocalApproval)
        .collect();
    if let Some(first) = waiting.first() {
        ctx.out.note(format!(
            "{} 个在等你放行：gld agent show {} 看提示，gld agent approve {} 放行，deny 拒掉。",
            waiting.len(),
            short(&first.task_id),
            short(&first.task_id)
        ));
    }
    Ok(())
}

async fn show(ctx: &mut Ctx, task: String) -> CliResult {
    let detail: TaskDetail = ctx
        .backend
        .call_typed(Request::ShowAgentTask { task_id: task })
        .await?;
    if ctx.out.json_or(&detail) {
        return Ok(());
    }
    let task = &detail.task;
    let mut rows = vec![
        ("任务", task.task_id.clone()),
        (
            "项目",
            format!("{}（{}）", task.workspace_name, task.workspace_root),
        ),
        ("Agent", agent_text(task)),
        ("状态", state_text(task.state).to_string()),
        ("发起人", task.caller.clone()),
        ("交接目录", detail.handoff_dir.clone()),
    ];
    if let Some(code) = task.exit_code {
        rows.push(("退出码", code.to_string()));
    }
    if let Some(failure) = &task.failure {
        rows.push(("没成的原因", failure.clone()));
    }
    if let Some(review) = &task.review {
        rows.push(("审核", format!("{}：{}", review.verdict, review.note)));
    }
    ctx.out.kv(&rows);
    ctx.out.line(format!("\n提示：\n{}", task.prompt_preview));
    if let Some(recap) = &task.recap {
        ctx.out.line(format!("\nrecap：\n{recap}"));
    }
    if task.changes_unknown {
        ctx.out.line("\n改动：不知道（项目不是 git 仓库）");
    } else if task.state.is_finished() {
        let more = if task.changed_files_truncated {
            "（只列前 200 个）"
        } else {
            ""
        };
        ctx.out.line(format!(
            "\n改了 {} 个文件{more}：\n{}",
            task.changed_files.len(),
            changed_files_text(task)
        ));
        if task.head_moved {
            ctx.out.line("（跑的时候 HEAD 动了：Agent 被叮嘱别提交，还是提交了。改动照样都在下面的 diff 里。）");
        }
    }
    if let Some(diff) = detail.diff.as_deref().filter(|diff| !diff.is_empty()) {
        let more = if detail.diff_truncated {
            "（只显示前 64 KB，全文在任务目录的 task.diff）"
        } else {
            ""
        };
        ctx.out
            .line(format!("\n这次任务自己的 diff{more}：\n{diff}"));
    }
    if let Some(report) = &detail.report {
        ctx.out.line(format!("\n交接报告：\n{report}"));
    }
    if let Some(tail) = &detail.output_tail {
        ctx.out.line(format!(
            "\n最后的输出（stdout）：\n{}\n（stderr）：\n{}",
            tail.stdout, tail.stderr
        ));
    }
    if task.state == State::WaitingLocalApproval {
        ctx.out.note(format!(
            "在等你放行：gld agent approve {}；不要就 gld agent deny {}。Agent 会全权跑。",
            short(&task.task_id),
            short(&task.task_id)
        ));
    }
    Ok(())
}

/// 每个文件一行，前面是增删行数。早于记行数的任务只有文件名。
fn changed_files_text(task: &AgentTask) -> String {
    if task.file_stats.is_empty() {
        return task.changed_files.join("\n");
    }
    task.file_stats
        .iter()
        .map(|stat| match (stat.added, stat.deleted) {
            (Some(added), Some(deleted)) => format!("+{added} -{deleted}\t{}", stat.path),
            _ => format!("二进制\t{}", stat.path),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn short(task_id: &str) -> &str {
    &task_id[..task_id.len().min(8)]
}

fn agent_text(task: &AgentTask) -> String {
    let version = task.agent_version.as_deref().unwrap_or("版本没问到");
    let flag = if task.version_unverified {
        "，没验过这个版本"
    } else {
        ""
    };
    format!("{}（{version}{flag}）", task.agent)
}

fn state_text(state: State) -> &'static str {
    match state {
        State::WaitingLocalApproval => "等你放行",
        State::Running => "在跑",
        State::AwaitingReview => "等审核",
        State::Accepted => "已通过",
        State::Rejected => "已打回",
        State::Failed => "失败",
        State::Timeout => "超时被停",
        State::Stopped => "已停",
        State::Interrupted => "gld 退出时中断",
        State::Unknown => "结局不知道",
        State::Denied => "已拒",
        State::Expired => "没人放行，过期",
    }
}

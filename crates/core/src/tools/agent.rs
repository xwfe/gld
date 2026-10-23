//! `agent_manage`：AI 把一段活委派给本机 Agent，做完交回审核（RFC-0009）。
//!
//! 把关都在这一层：
//!
//! - 项目开关 `agent-delegation` 是 off 就不让起（默认 off）；ask 档存下来等用户在本机 `gld agent approve`。
//! - 起、审、停只有服务主凭据能做（D1）。grant 来的一律不行：现在分不出浏览器 grant，
//!   而 Agent 是全权跑的，等于把全权借给了 grant 的持有人。
//! - 只看得见、只动得了**这个项目**的任务。
//! - Plan 模式下起不了（`manage::action_is_mutating` 把 start 算成写，分发层统一拦）。

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

use crate::agents::{self, store::State, AgentError, Approval, StartRequest, TaskDetail};
use crate::tools::caller::Caller;
use crate::tools::workspace::WorkspaceError;
use crate::tools::ToolContext;

/// `wait` 单次最多等多久、起跑的总时长上限，都在 `tools::args::BOUNDED` 里：wait 和
/// exec_command 一样最多 30 秒（经 OpenAI 隧道时官方上限 120 秒）。
pub const ACTIONS: &[&str] = &[
    "agents", "start", "status", "wait", "diff", "review", "stop", "list",
];

pub fn agent_manage(
    ctx: &ToolContext,
    caller: &Caller,
    args: &Value,
) -> Result<Value, WorkspaceError> {
    let action = args
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let data_home = crate::home::data_home().map_err(|error| {
        tool_error(
            "AGENT_STATE_UNAVAILABLE",
            format!("找不到数据目录：{error}"),
            "storage",
        )
    })?;
    match action {
        "agents" => agent_profiles(ctx, &data_home),
        "list" => Ok(list(ctx, &data_home)),
        "start" => {
            only_operator(caller, "start")?;
            start(ctx, caller, &data_home, args)
        }
        "status" => {
            let (dir, task) = this_project_task(ctx, &data_home, args)?;
            Ok(present(agents::detail(&dir, task)))
        }
        "wait" => {
            let secs = crate::tools::args::bounded(args, "agent_manage", "wait_seconds");
            let (_, task) = this_project_task(ctx, &data_home, args)?;
            let (dir, task) = agents::wait(&data_home, &task.task_id, Duration::from_secs(secs))
                .map_err(convert)?;
            Ok(present(agents::detail(&dir, task)))
        }
        "diff" => {
            let (dir, task) = this_project_task(ctx, &data_home, args)?;
            let offset = args.get("offset").and_then(Value::as_u64);
            let path = args
                .get("path")
                .and_then(Value::as_str)
                .filter(|path| !path.is_empty());
            let max = crate::tools::args::bounded(args, "agent_manage", "max_bytes") as usize;
            let page = agents::diff_page(&dir, &task, offset, path, max).map_err(convert)?;
            Ok(present_page(page))
        }
        "review" => {
            only_operator(caller, "review")?;
            let (dir, task) = this_project_task(ctx, &data_home, args)?;
            let verdict = string_arg(args, "verdict")?;
            let note = args.get("note").and_then(Value::as_str).unwrap_or_default();
            let task = agents::review(&data_home, &task.task_id, verdict, note).map_err(convert)?;
            let mut value = present(agents::detail(&dir, task));
            // 审核的人刚看完 diff，回包不再带一遍：审查 §50 那一轮光这一项就 143 KB。要再看用 status 或 action=diff。
            if let Some(object) = value.as_object_mut() {
                for key in ["diff", "diff_truncated", "diff_next_offset"] {
                    object.remove(key);
                }
            }
            Ok(value)
        }
        "stop" => {
            only_operator(caller, "stop")?;
            let (dir, task) = this_project_task(ctx, &data_home, args)?;
            let task = agents::stop(&data_home, &task.task_id).map_err(convert)?;
            Ok(present(agents::detail(&dir, task)))
        }
        other => Err(WorkspaceError::invalid_argument(format!(
            "agent_manage action「{other}」不认识。可用：{}",
            ACTIONS.join(", ")
        ))),
    }
}

fn start(
    ctx: &ToolContext,
    caller: &Caller,
    data_home: &Path,
    args: &Value,
) -> Result<Value, WorkspaceError> {
    let approval = match ctx.agent_delegation.as_str() {
        "ask" => Approval::Ask,
        "auto" => Approval::Auto,
        _ => {
            let name = if ctx.workspace_name.is_empty() {
                "<项目>"
            } else {
                ctx.workspace_name.as_str()
            };
            return Err(WorkspaceError::ToolDetails {
                code: "AGENT_DELEGATION_OFF",
                message: format!(
                    "这个项目没开委派。要开，请用户在本机运行 gld set {name} agent-delegation=ask（每次要他点头）或 =auto；Agent 会全权跑。"
                ),
                category: "permission",
                retryable: false,
                details: json!({ "setting": "agent-delegation", "current": ctx.agent_delegation }),
            });
        }
    };
    let task = agents::start(StartRequest {
        data_home,
        workspace_root: ctx.workspace.root(),
        workspace_name: &ctx.workspace_name,
        caller: caller.key(),
        agent: string_arg(args, "agent")?,
        prompt: string_arg(args, "prompt")?,
        brief: args.get("brief").and_then(Value::as_str),
        client_request_id: args.get("client_request_id").and_then(Value::as_str),
        approval,
        timeout_minutes: Some(crate::tools::args::bounded(
            args,
            "agent_manage",
            "timeout_minutes",
        )),
    })
    .map_err(convert)?;
    let (dir, task) = agents::get(data_home, &task.task_id).map_err(convert)?;
    Ok(present(agents::detail(&dir, task)))
}

fn agent_profiles(ctx: &ToolContext, data_home: &Path) -> Result<Value, WorkspaceError> {
    let profiles = agents::profiles(data_home).map_err(convert)?;
    let rows: Vec<Value> = profiles
        .values()
        .map(|profile| {
            let program = profile.command.first().cloned().unwrap_or_default();
            json!({
                "name": profile.name,
                "program": program,
                "installed": on_path(&program),
                "result_from": profile.result.from,
            })
        })
        .collect();
    Ok(json!({
        "delegation": ctx.agent_delegation,
        "agents": rows,
        "agents_toml": agents::agents_toml(data_home).display().to_string(),
        "note": "Agents run non-interactively with full permissions in this project; the finished task comes back as awaiting_review."
    }))
}

fn list(ctx: &ToolContext, data_home: &Path) -> Value {
    let project_id = crate::tools::runs::project_id(&canonical(ctx.workspace.root()));
    let tasks: Vec<Value> = agents::list(data_home, Some(&project_id))
        .into_iter()
        .take(50)
        .map(|task| {
            json!({
                "task_id": task.task_id,
                "agent": task.agent,
                "state": task.state,
                "created_at_ms": task.created_at_ms,
                "prompt_preview": task.prompt_preview.chars().take(200).collect::<String>(),
            })
        })
        .collect();
    json!({ "tasks": tasks })
}

/// 按 task_id 找，而且必须是这个项目的：别的项目的任务当作不存在。
fn this_project_task(
    ctx: &ToolContext,
    data_home: &Path,
    args: &Value,
) -> Result<(std::path::PathBuf, agents::store::AgentTask), WorkspaceError> {
    let given = string_arg(args, "task_id")?;
    let (dir, task) = agents::get(data_home, given).map_err(convert)?;
    let here = crate::tools::runs::project_id(&canonical(ctx.workspace.root()));
    if task.project_id != here {
        return Err(convert(AgentError {
            code: "AGENT_TASK_NOT_FOUND",
            message: format!("这个项目没有任务 {given}"),
        }));
    }
    Ok((dir, task))
}

/// 回包：任务详情加一句"下一步做什么"。
///
/// diff 超过一页（64 KB）时不附第一页，只给 `file_stats` 让 AI 挑着读：按路径排序，第一页常常全是生成的数据，
/// 审查 §50、§54 两轮都是，一次回包 143 KB 对审代码没用。`diff` 为 null、`diff_next_offset` 为 0，从头读也接得上。
/// 本机 `gld agent show` 不走这里，照样打第一页和全文在哪。
fn present(mut detail: TaskDetail) -> Value {
    if detail.diff_truncated {
        detail.diff = None;
        detail.diff_next_offset = Some(0);
    }
    let short = detail.task.task_id.chars().take(8).collect::<String>();
    let id = detail.task.task_id.clone();
    let next = match detail.task.state {
        State::WaitingLocalApproval => format!(
            "Ask the user to approve it on this machine with `gld agent approve {short}` (or deny it), then call agent_manage action=wait task_id={id}."
        ),
        State::Running => format!(
            "Call agent_manage action=wait task_id={id} (up to 30 s per call) until the state changes. The project's apply_patch and exec_command are refused while it runs."
        ),
        State::AwaitingReview | State::Failed | State::Timeout | State::Stopped | State::Interrupted => {
            let diff = if detail.diff_truncated {
                format!(
                    "This task's own diff (working tree before vs after the agent ran) is {} bytes, too big to include, so the diff field is null. file_stats lists every changed file with its added/deleted lines, diff_offset and diff_bytes: read the files you need with agent_manage action=diff task_id={id} path=<a path from file_stats> (add max_bytes=4096 to peek at a big data file), or page through all of it from offset=0.",
                    detail.task.diff_bytes.unwrap_or(0)
                )
            } else {
                "The diff field is this task's own change (working tree before vs after the agent ran).".to_string()
            };
            let committed = if detail.task.head_moved {
                " HEAD moved while it ran: the agent committed although told not to; the diff still covers those changes."
            } else {
                ""
            };
            format!(
                "Review it yourself. {diff} git_diff compares with HEAD and also shows any earlier uncommitted changes.{committed} Run the relevant tests with exec_command, then agent_manage action=review task_id={id} verdict=accept|reject note=\"what you checked\". The agent's own claims are not evidence."
            )
        }
        State::Rejected => "Rejected. To continue, start a new task whose prompt or brief includes the review note.".to_string(),
        _ => "Nothing to do for this task.".to_string(),
    };
    let mut value = serde_json::to_value(&detail).unwrap_or_else(|_| json!({}));
    if let Some(object) = value.as_object_mut() {
        object.insert("next_action".into(), Value::String(next));
        // 原样输出、提示全文只在本机看：给 AI 的是 recap、报告和最后一段输出。
        // 提示预览是 AI 自己写的，每次 status / wait / review 原样回去最多 2 KB、线上翻倍（审查 §57 量的：等审核那次回包里
        // 它最大）；发起人、gld 进程和项目哈希是本机记账用的，AI 拿去也做不了什么。本机 `gld agent show` 照样都有。
        for key in [
            "caller",
            "prompt_preview",
            "owner",
            "owner_pid",
            "pid",
            "project_id",
        ] {
            object.remove(key);
        }
    }
    value
}

fn present_page(page: agents::DiffPage) -> Value {
    let path_arg = page
        .path
        .as_deref()
        .map(|path| format!(" path={path}"))
        .unwrap_or_default();
    let next = match (page.next_offset, page.path.as_deref()) {
        (Some(offset), _) => format!(
            "More follows: call agent_manage action=diff task_id={}{path_arg} offset={offset}.",
            page.task_id
        ),
        (None, Some(path)) => format!("That is the end of {path} in this task's diff."),
        (None, None) => "That is the end of this task's diff.".to_string(),
    };
    let mut value = serde_json::to_value(&page).unwrap_or_else(|_| json!({}));
    if let Some(object) = value.as_object_mut() {
        object.insert("next_action".into(), Value::String(next));
    }
    value
}

fn only_operator(caller: &Caller, action: &str) -> Result<(), WorkspaceError> {
    if caller.grant_id().is_none() {
        return Ok(());
    }
    Err(WorkspaceError::ToolDetails {
        code: "AGENT_DELEGATION_NOT_ALLOWED",
        message: format!(
            "grant 的凭据不能 {action} 委派：Agent 是全权跑的。用服务自己的凭据连进来才行。"
        ),
        category: "permission",
        retryable: false,
        details: json!({ "action": action }),
    })
}

fn string_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, WorkspaceError> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| WorkspaceError::invalid_argument(format!("agent_manage 这个动作要 {key}")))
}

fn convert(error: AgentError) -> WorkspaceError {
    let category = match error.code {
        "INVALID_ARGUMENT" | "AGENT_UNKNOWN" => "argument",
        "AGENT_TASK_RUNNING" | "AGENT_TASK_STATE" => "conflict",
        "AGENT_TASK_NOT_FOUND" | "AGENT_TASK_AMBIGUOUS" | "AGENT_DIFF_UNAVAILABLE" => "not_found",
        _ => "runtime",
    };
    tool_error(error.code, error.message, category)
}

fn tool_error(code: &'static str, message: String, category: &'static str) -> WorkspaceError {
    WorkspaceError::ToolDetails {
        code,
        message,
        category,
        retryable: false,
        details: json!({}),
    }
}

fn canonical(path: &Path) -> std::path::PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// 程序在不在 PATH 上（或者给的就是一条存在的路径）。只是给 AI 看的提示，不挡着起。
fn on_path(program: &str) -> bool {
    let path = Path::new(program);
    if path.components().count() > 1 {
        return path.is_file();
    }
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 给 AI 的回包不带它自己写的提示、也不带 gld 本机记账的字段；审核要的照样都在。
    #[test]
    fn the_reply_leaves_out_the_prompt_and_local_bookkeeping() {
        let detail: TaskDetail = serde_json::from_value(json!({
            "task_id": "3f2a0000-0000-4000-8000-000000000000",
            "project_id": "e930f331b3792b0af10be1e667a89bae",
            "workspace_root": "/w",
            "workspace_name": "w",
            "agent": "codex",
            "caller": "local",
            "prompt_preview": "在 math_utils.py 末尾新增 quartiles(values) 和 iqr(values)……",
            "state": "awaiting_review",
            "created_at_ms": 1,
            "pid": 4242,
            "owner": "0000000000000000000000000000beef",
            "owner_pid": 32869,
            "recap": "做了什么：……",
            "file_stats": [{ "path": "a.py", "added": 1, "deleted": 0 }],
            "handoff_dir": "/w/.gld/handoff/x",
            "diff": "diff --git a/a.py b/a.py\n+x\n"
        }))
        .expect("detail");
        let value = present(detail);
        for gone in [
            "caller",
            "prompt_preview",
            "owner",
            "owner_pid",
            "pid",
            "project_id",
        ] {
            assert!(value.get(gone).is_none(), "{gone} 不该回给 AI：{value}");
        }
        for kept in [
            "task_id",
            "state",
            "recap",
            "file_stats",
            "diff",
            "next_action",
        ] {
            assert!(value.get(kept).is_some(), "{kept} 要在：{value}");
        }
    }

    /// diff 超过一页：给 AI 的回包不附第一页（审查 §54：两轮的第一页都是生成的数据，一次 143 KB），
    /// 下一步说清怎么按文件读、从哪接着读。不超过一页的照样整份附上。
    #[test]
    fn a_big_diff_is_left_out_and_the_reply_says_how_to_read_it() {
        let detail = |diff: &str, truncated: bool, next: Option<u64>| -> TaskDetail {
            serde_json::from_value(json!({
                "task_id": "3f2a0000-0000-4000-8000-000000000000",
                "project_id": "p",
                "workspace_root": "/w",
                "workspace_name": "w",
                "agent": "codex",
                "caller": "local",
                "prompt_preview": "fix",
                "state": "awaiting_review",
                "created_at_ms": 1,
                "diff_bytes": 171539,
                "handoff_dir": "/w/.gld/handoff/x",
                "diff": diff,
                "diff_truncated": truncated,
                "diff_next_offset": next
            }))
            .expect("detail")
        };
        let value = present(detail("diff --git a/x b/x\n", true, Some(65530)));
        assert!(value["diff"].is_null(), "{value}");
        assert_eq!(value["diff_truncated"], true);
        assert_eq!(value["diff_next_offset"], 0, "不附第一页，从头读得从 0 起");
        let next = value["next_action"].as_str().unwrap_or("");
        assert!(
            next.contains("171539 bytes")
                && next.contains("action=diff task_id=3f2a0000-0000-4000-8000-000000000000 path=")
                && next.contains("max_bytes=4096")
                && next.contains("offset=0"),
            "{next}"
        );

        let small = present(detail("diff --git a/x b/x\n+one\n", false, None));
        assert_eq!(small["diff"], "diff --git a/x b/x\n+one\n");
        assert!(small["next_action"]
            .as_str()
            .unwrap_or("")
            .contains("The diff field is this task's own change"));

        let page = present_page(agents::DiffPage {
            task_id: "t".into(),
            path: None,
            offset: 65530,
            next_offset: None,
            total_bytes: 70000,
            diff: String::new(),
        });
        assert!(page["next_offset"].is_null(), "{page}");
        assert!(page["next_action"].as_str().unwrap_or("").contains("end"));

        // 按文件读时，下一页的调用要带上同一个 path，不然读出去就到了下一个文件。
        let page = present_page(agents::DiffPage {
            task_id: "t".into(),
            path: Some("gen_cases.py".into()),
            offset: 170000,
            next_offset: Some(235530),
            total_bytes: 300000,
            diff: String::new(),
        });
        assert_eq!(page["path"], "gen_cases.py");
        assert!(
            page["next_action"]
                .as_str()
                .unwrap_or("")
                .contains("action=diff task_id=t path=gen_cases.py offset=235530"),
            "{page}"
        );
    }
}

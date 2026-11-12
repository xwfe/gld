//! Coordinate patch execution with caller-owned workspace state.

use serde_json::Value;

use gld_runtime::patch::{self, PatchOutcome, PatchRequest};

use crate::tools::caller::Caller;
use crate::tools::context::ToolContext;
use crate::tools::workspace::WorkspaceError;

pub use gld_runtime::patch::{failure_log_fields, operation_summary, patch_format};

pub fn apply_patch(
    ctx: &ToolContext,
    caller: &Caller,
    args: &Value,
) -> Result<Value, WorkspaceError> {
    // Decode before taking the lock to preserve argument-error precedence.
    let request = PatchRequest::from_args(args)?;
    // 真要落盘的那一路，从这里开始独占这个工作区，直到函数结束。
    //
    // 挡的是 gld 自己的并发写者：两个 MCP 会话同时打补丁，一个读原文、另一个
    // 正在落盘，第一个算出来的新内容就是基于已经过期的原文。dry_run 不写盘，
    // 不占这把锁——预检不该让真正的写操作排队。
    //
    // 锁是按**目录**取的，不是每个上下文一把，也不是整个进程一把：同一个目录
    // 经 hub、单工作区 listener、CLI 进来是三个 `ToolContext`，得排同一个队；
    // 两个不同的项目则各写各的，不该互相堵。跨进程那一层是数据目录下的文件锁。
    // 还管不到的（外部编辑器、后台跑的命令、Git 元数据）写在
    // `crate::tools::workspace_runtime` 的模块文档里，那一侧靠的是下面的版本
    // 前置条件和落盘前复核，两者都不是强 CAS，见 runtime 的提交复核。
    let _commit_guard = if request.dry_run() {
        None
    } else {
        // 等不到就直接说。占着写权的多半是 `exec_command` 里正在跑的命令，
        // 那一占可以到十分钟；挂在这儿等，客户端那边先超时断开，模型连"为什么
        // 没动静"都不知道。
        Some(
            ctx.runtime
                .lock_commits()
                .ok_or_else(crate::tools::workspace_runtime::write_lock_busy)?,
        )
    };
    let outcome = patch::apply(
        &ctx.workspace,
        request,
        |path, then, now| ctx.runtime.wrote_since(caller, path, then, now),
        || ctx.runtime.record_write(),
    )?;
    match outcome {
        PatchOutcome::Checked(result) => Ok(result),
        PatchOutcome::Applied {
            mut result,
            previous_versions,
        } => {
            // The same commit guard also covers caller ownership and warnings.
            for (path, version) in result["file_versions"]
                .as_object()
                .expect("applied patches contain file_versions")
            {
                ctx.runtime.record_own_write(
                    caller,
                    path,
                    previous_versions.get(path).and_then(Value::as_str),
                    version.as_str(),
                );
            }
            result["warnings"]
                .as_array_mut()
                .expect("applied patches contain warnings")
                .extend(
                    commands_still_running_warnings(ctx, caller)
                        .into_iter()
                        .map(Value::String),
                );
            Ok(result)
        }
    }
}

/// 补丁已经落盘了，但这个目录上还有命令在跑——它们读到的文件可能和刚写进去
/// 的不一样。
///
/// **为什么是写完之后提醒，而不是拦下来**：后台命令没有终点，拦住等于"只要有
/// 个 dev server 在跑就不能改代码"，那恰恰是日常工作流。这条提示要做到的是让
/// 模型**能判断**：自己起的命令直接给 `session_id`，它可以停掉重跑，也可以
/// 认下"那条结果是基于旧代码的"。
///
/// 别人起的只报个数。命令文本是另一个主体的东西，会话刚按主体分开，这里摊开
/// 就白分了（见 [`crate::tools::caller`]）。
fn commands_still_running_warnings(ctx: &ToolContext, caller: &Caller) -> Vec<String> {
    let running = ctx.runtime.running_commands(caller);
    let mut warnings = Vec::new();
    for command in &running.mine {
        warnings.push(format!(
            "Command still running while this patch landed: {} (session {}, {}s so far). \
             It may have read these files before the change, so its result can describe the old code. \
             kill_session and run it again if that matters.",
            short_command(&command.command),
            command.session_id,
            command.running_ms / 1000
        ));
    }
    if running.others > 0 {
        warnings.push(format!(
            "{} command(s) started over another connection are also running in this workspace and may be reading these files.",
            running.others
        ));
    }
    warnings
}

/// 命令文本进警告时截一下：整条 `cargo test --workspace --all-features …` 会把
/// 提示淹掉，而模型认出是哪条命令只需要开头。
fn short_command(command: &str) -> String {
    const MAX: usize = 60;
    let trimmed = command.trim();
    if trimmed.chars().count() <= MAX {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(MAX).collect();
    format!("{head}…")
}

pub fn patch_check(
    ctx: &ToolContext,
    caller: &Caller,
    args: &Value,
) -> Result<Value, WorkspaceError> {
    let mut check_args = args.clone();
    check_args["dry_run"] = Value::Bool(true);
    let mut result = apply_patch(ctx, caller, &check_args)?;
    if let Some(object) = result.as_object_mut() {
        object.insert("preflight".into(), Value::Bool(true));
    }
    Ok(result)
}

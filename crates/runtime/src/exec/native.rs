//! 不起子进程的最小诊断命令。

use std::path::Path;

use serde_json::{json, Value};

use crate::command_spec::CommandSpec;
use crate::workspace::WorkspaceError;

use super::resolve::which_on_path;
use super::{spec_error, Execution};

/// 这条命令服务端自己就能答，不用起子进程。
///
/// 单独一个函数，是因为预检（`check_command`）必须和真实执行看法一致：
/// 说"会起子进程"结果没起，或者反过来，模型据此做的判断就全是错的。
pub fn native_builtin(parts: &[String]) -> Option<&'static str> {
    match parts.first()?.to_ascii_lowercase().as_str() {
        "pwd" if parts.len() == 1 => Some("pwd"),
        "ls" => Some("ls"),
        "dir" => Some("dir"),
        "which" if parts.len() == 2 => Some("which"),
        "echo" => Some("echo"),
        _ => None,
    }
}

pub(super) fn run_native_diagnostic(
    execution: &Execution<'_>,
    spec: &CommandSpec,
    cwd: &Path,
) -> Result<Option<Value>, WorkspaceError> {
    let cmd = spec.display.as_str();
    let parts = spec.resolved_parts().map_err(spec_error)?;
    if parts.is_empty() {
        return Ok(None);
    }

    let stdout = match native_builtin(&parts) {
        Some("pwd") => Some(format!("{}\n", cwd.display())),
        Some("ls") | Some("dir") => Some(list_directory(execution, cwd, &parts[1..])?),
        Some("which") => {
            let search_path = execution.search_path.as_deref();
            let path =
                which_on_path(&parts[1], cwd, search_path).ok_or_else(|| WorkspaceError::Tool {
                    code: "COMMAND_NOT_FOUND",
                    message: format!("Program not found on PATH: {}", parts[1]),
                    category: "runtime",
                    retryable: false,
                })?;
            Some(format!("{}\n", path.display()))
        }
        Some("echo") => Some(format!("{}\n", parts[1..].join(" "))),
        _ => None,
    };

    Ok(stdout.map(|stdout| {
        json!({
            "command": cmd,
            "resolved_cwd": cwd.display().to_string(),
            "status": "exited",
            "termination_reason": "exited",
            "recoverable": false,
            "suggestion": "命令已完成",
            "exit_code": 0,
            "stdout": stdout,
            "stderr": "",
            "stdout_truncated": false,
            "stderr_truncated": false,
            "duration_ms": 0,
            "elapsed_ms": 0,
            "execution_mode": "native_builtin",
            "command_runner": "native_builtin",
            // 说清它是什么：服务端自己答的，不是系统上那个同名命令。只说
            // "没起子进程"的话，`ls` 少了 `-la` 的信息会被当成机器出了问题。
            "warnings": ["answered by the server itself, no child process: this is a minimal built-in (pwd / ls / dir / which / echo), not the system command. Options are not accepted — use list_dir, list_files or read_file for anything more."]
        })
    }))
}

fn list_directory(
    execution: &Execution<'_>,
    cwd: &Path,
    args: &[String],
) -> Result<String, WorkspaceError> {
    // 服务端内建的 `ls` 是个极简实现，不是系统 ls。带 flag 来的多半以为它是，
    // 而 `-la` 会被当成目录名，报出来的是"路径不存在"——看着像目录没了
    // （方案 B 的原生 ls 一条）。直接说清楚它是什么、该用哪个工具。
    if let Some(flag) = args.iter().find(|arg| arg.starts_with('-')) {
        return Err(WorkspaceError::invalid_argument(format!(
            "原生 ls/dir 不接受选项（{flag}）：它是服务端内建的目录列表，只支持 `ls [目录]`。要大小、时间、类型用 list_dir，要按 glob 找文件用 list_files"
        )));
    }
    let target = match args {
        [] => cwd.to_path_buf(),
        // 相对路径按 **workdir** 解析，不是工作区根：`workdir=crates` 时
        // `ls src` 指的是 `crates/src`，跟在真实 shell 里输入它的结果一致。
        [path] => execution.workspace.resolve_existing_at(cwd, path)?.path,
        _ => {
            return Err(WorkspaceError::invalid_argument(
                "ls/dir accepts at most one directory path",
            ))
        }
    };
    if !target.is_dir() {
        return Err(WorkspaceError::not_a_directory(
            "ls/dir target is not a directory",
        ));
    }

    let mut entries = std::fs::read_dir(target)
        .map_err(|error| WorkspaceError::ToolDetails {
            code: "DIRECTORY_READ_FAILED",
            message: format!("Failed to read directory: {error}"),
            category: "runtime",
            retryable: true,
            details: json!({
                "stage": "native_builtin",
                "reason": "directory_read_failed",
                "retryable": true
            }),
        })?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    entries.sort_unstable();
    Ok(if entries.is_empty() {
        String::new()
    } else {
        format!("{}\n", entries.join("\n"))
    })
}

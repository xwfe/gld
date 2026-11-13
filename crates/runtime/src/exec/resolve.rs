//! PATH 优先、工作区本地入口与无副作用的程序探测。

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::command_spec::CommandSpec;
use crate::workspace::WorkspaceError;

use super::{spec_error, ExecutableConfig, Execution};

/// 程序在不在、在哪儿。走的是真实执行那条 `resolve_program`。
pub fn probe_program(
    execution: &Execution<'_>,
    parts: &[String],
    cwd: &Path,
    builtin: Option<&str>,
) -> Value {
    let Some(raw) = parts.first() else {
        return json!({
            "requested": Value::Null,
            "found": Value::Null,
            "source": "unknown",
            "note": "命令是空的，没什么可解析"
        });
    };
    if builtin.is_some() {
        return json!({
            "requested": raw,
            "found": true,
            "source": "native_builtin",
            "path": Value::Null,
            "note": "服务端内建，不需要磁盘上的可执行文件"
        });
    }
    let search_path = execution.search_path.as_deref();
    match resolve_program(
        raw,
        cwd,
        execution.workspace.root(),
        &execution.executables,
        search_path,
    ) {
        Ok(path) => {
            let inside = Path::new(&path).starts_with(execution.workspace.root());
            json!({
                "requested": raw,
                "found": true,
                "path": path,
                "source": if inside { "workspace_entry" } else { "path" }
            })
        }
        Err(error) => json!({
            "requested": raw,
            "found": false,
            "path": Value::Null,
            "source": if error.code() == "COMMAND_REJECTED" { "not_found" } else { "rejected" },
            "reason": if error.code() == "EXECUTABLE_OUTSIDE_WORKSPACE" {
                "executable_outside_workspace"
            } else {
                "program_not_found"
            },
            "code": error.code(),
            "message": error.message()
        }),
    }
}

pub(super) fn parse_and_resolve(
    spec: &CommandSpec,
    cwd: &Path,
    workspace_root: &Path,
    config: &ExecutableConfig<'_>,
    search_path: Option<&OsStr>,
) -> Result<(String, Vec<String>), WorkspaceError> {
    let parts = spec.resolved_parts().map_err(spec_error)?;

    let program = resolve_program(&parts[0], cwd, workspace_root, config, search_path)?;
    Ok((program, parts[1..].to_vec()))
}

fn resolve_program(
    raw: &str,
    cwd: &Path,
    workspace_root: &Path,
    config: &ExecutableConfig<'_>,
    search_path: Option<&OsStr>,
) -> Result<String, WorkspaceError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(WorkspaceError::invalid_argument("Empty program"));
    }

    let explicit_path = trimmed.contains(['/', '\\']);
    // **裸名先查 PATH。**以前是反过来的：工作区里放一个叫 `python` 的文件，
    // `python --version` 跑的就是它——而白名单批准的是"python 这个系统命令"。
    // 批的和跑的不是同一个东西，正是方案 B 要消除的那种错位。shell 也不把
    // `.` 放进 PATH，同一个道理。
    //
    // 工作区里的脚本没有因此失去入口：PATH 上查不到的名字仍然回落到工作区
    // （下面那段），写 `./build` 或 `scripts/build.sh` 则一直是明确指路。
    if !explicit_path && !Path::new(trimmed).is_absolute() {
        if let Some(found) = which_on_path(trimmed, cwd, search_path) {
            return Ok(found.to_string_lossy().into_owned());
        }
    }
    let candidate = if Path::new(trimmed).is_absolute() {
        Path::new(trimmed).to_path_buf()
    } else {
        cwd.join(trimmed)
    };
    if candidate.is_file() {
        let resolved = candidate.canonicalize().map_err(|_| WorkspaceError::Tool {
            code: "COMMAND_REJECTED",
            message: format!("Program not found: {trimmed}"),
            category: "runtime",
            retryable: false,
        })?;
        let canonical_workspace =
            workspace_root
                .canonicalize()
                .map_err(|_| WorkspaceError::Tool {
                    code: "COMMAND_REJECTED",
                    message: "Workspace root is unavailable".into(),
                    category: "runtime",
                    retryable: true,
                })?;
        if !resolved.starts_with(&canonical_workspace) {
            return Err(WorkspaceError::Tool {
                code: "EXECUTABLE_OUTSIDE_WORKSPACE",
                message: format!("Workspace 外可执行文件被拒绝: {trimmed}"),
                category: "security",
                retryable: false,
            });
        }
        let extension = resolved
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| format!(".{}", value.to_ascii_lowercase()))
            .unwrap_or_default();
        // 扩展名名单是给 Windows 的：那边靠扩展名决定一个文件能不能跑。Unix 上靠的是
        // 可执行位，名单里又没有 .sh，结果没扩展名的脚本能跑、scripts/build.sh 反而被拒，
        // 而拒了也没换来什么——没扩展名的照样能跑任意内容（D10 第三轮）。
        if config.workspace_local_entries
            && (extension.is_empty()
                || config.workspace_script_extensions.contains(&extension)
                || (cfg!(unix) && is_executable_file(&resolved)))
        {
            return Ok(resolved.to_string_lossy().into_owned());
        }
        let message = if !config.workspace_local_entries {
            format!("这个项目关掉了工作区本地入口（workspace_local_entries），工作区里的文件不能直接执行: {trimmed}")
        } else if cfg!(windows) {
            format!(
                "扩展名 {extension} 不在可直接执行的名单里（workspace_script_extensions）: {trimmed}；用解释器跑它，例如 python {trimmed}"
            )
        } else {
            format!("{trimmed} 没有可执行权限；用解释器跑它（例如 python {trimmed}），或请用户 chmod +x")
        };
        return Err(WorkspaceError::Tool {
            code: "COMMAND_REJECTED",
            message,
            category: "policy",
            retryable: false,
        });
    }

    if explicit_path {
        return Err(WorkspaceError::Tool {
            code: "COMMAND_REJECTED",
            message: format!("Program not found: {trimmed}"),
            category: "runtime",
            retryable: false,
        });
    }

    which_on_path(trimmed, cwd, search_path)
        .map(|p| p.to_string_lossy().into_owned())
        .ok_or_else(|| WorkspaceError::Tool {
            code: "COMMAND_REJECTED",
            message: format!("Program not found on PATH: {trimmed}"),
            category: "runtime",
            retryable: false,
        })
}

pub(super) fn which_on_path(
    program: &str,
    cwd: &Path,
    search_path: Option<&OsStr>,
) -> Option<std::path::PathBuf> {
    let Some(paths) = search_path else {
        return which::which(program).ok();
    };

    for directory in std::env::split_paths(paths) {
        let directory = if directory.is_absolute() {
            directory
        } else {
            cwd.join(directory)
        };
        for candidate in executable_candidates(directory.join(program)) {
            if is_executable_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(not(windows))]
fn executable_candidates(candidate: PathBuf) -> Vec<PathBuf> {
    vec![candidate]
}

#[cfg(windows)]
fn executable_candidates(candidate: PathBuf) -> Vec<PathBuf> {
    if candidate.extension().is_some() {
        return vec![candidate];
    }
    let extensions = std::env::var_os("PATHEXT")
        .map(|value| {
            value
                .to_string_lossy()
                .split(';')
                .filter_map(|item| {
                    let extension = item.trim().trim_start_matches('.');
                    (!extension.is_empty()).then(|| extension.to_ascii_lowercase())
                })
                .collect::<Vec<_>>()
        })
        .filter(|items| !items.is_empty())
        .unwrap_or_else(|| vec!["com".into(), "exe".into(), "bat".into(), "cmd".into()]);

    extensions
        .into_iter()
        .map(|extension| candidate.with_extension(extension))
        .collect()
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(windows)]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use tempfile::tempdir;

    #[cfg(unix)]
    #[test]
    fn configured_path_resolution_uses_declared_order() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempdir().expect("root");
        let first = root.path().join("first");
        let second = root.path().join("second");
        std::fs::create_dir_all(&first).expect("first dir");
        std::fs::create_dir_all(&second).expect("second dir");
        for directory in [&first, &second] {
            let executable = directory.join("path-probe");
            std::fs::write(&executable, "#!/bin/sh\nexit 0\n").expect("probe");
            let mut permissions = std::fs::metadata(&executable)
                .expect("metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&executable, permissions).expect("permissions");
        }
        let search_path = std::env::join_paths([&first, &second]).expect("join path");

        let resolved = which_on_path("path-probe", root.path(), Some(search_path.as_os_str()))
            .expect("resolved executable");

        assert_eq!(resolved, first.join("path-probe"));
    }

    #[test]
    fn resolves_an_arbitrarily_named_workspace_local_entry() {
        let workspace = tempdir().expect("workspace");
        let entry = workspace.path().join("scripts").join("anything.cmd");
        std::fs::create_dir_all(entry.parent().expect("parent")).expect("scripts");
        std::fs::write(&entry, "echo test").expect("entry");
        let resolved = resolve_program(
            "scripts/anything.cmd",
            workspace.path(),
            workspace.path(),
            &ExecutableConfig {
                workspace_local_entries: true,
                workspace_script_extensions: &HashSet::from([".cmd".to_string()]),
            },
            None,
        )
        .expect("workspace entry resolves");
        assert_eq!(
            std::path::Path::new(&resolved),
            entry.canonicalize().unwrap()
        );
    }
}

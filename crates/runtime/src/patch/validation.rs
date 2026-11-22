//! Write classification, version preconditions, and stable tool errors.

use std::collections::HashMap;

use gld_policy::write_class::{Verdict, WriteClass};
use serde_json::{json, Value};

use crate::patch_diag::{Diagnostic, MAX_DIAGNOSTICS};
use crate::workspace::{FileState, WorkspaceError};

use super::parse::FilePatch;

/// 这个写目标过不过得去。分类和理由都来自 [`WriteClass`]，这里只负责把判定
/// 翻译成错误码：走不通的是 `PROTECTED_REPOSITORY_ASSET`，等确认的是
/// `DANGEROUS_OPERATION_REQUIRES_CONFIRMATION`。两个码都是原来就有的，客户端
/// 按码分支的代码不用改。
pub(super) fn gate_write(path: &str, is_delete: bool, confirm: bool) -> Result<(), WorkspaceError> {
    let class = WriteClass::of(path);
    let verdict = if is_delete {
        class.delete()
    } else {
        class.modify()
    };
    match verdict {
        Verdict::Allow => Ok(()),
        Verdict::NeedsConfirm(_) if confirm => Ok(()),
        Verdict::NeedsConfirm(why) => Err(dangerous_operation(format!("{path}：{why}"))),
        Verdict::Deny(why) => Err(protected_repository_asset(format!("{path}：{why}"))),
    }
}

fn dangerous_operation(message: impl Into<String>) -> WorkspaceError {
    WorkspaceError::Tool {
        code: "DANGEROUS_OPERATION_REQUIRES_CONFIRMATION",
        message: message.into(),
        category: "permission",
        retryable: false,
    }
}

fn protected_repository_asset(message: impl Into<String>) -> WorkspaceError {
    WorkspaceError::Tool {
        code: "PROTECTED_REPOSITORY_ASSET",
        message: message.into(),
        category: "security",
        retryable: false,
    }
}

/// 把收集到的诊断变成一个错误。
///
/// 整体错误码取第一条：`PATCH_AMBIGUOUS` 和 `PATCH_FAILED` 的下一步不一样，
/// 而多条失败时第一条才是模型最先要修的那个。消息里点名文件和第几段——
/// 原来只有一句 `Hunk context did not match file content.`，模型只能靠重读
/// 整个项目去猜（审查 P01、E03）。
pub(super) fn patch_diagnostics(
    diagnostics: Vec<Diagnostic>,
    not_checked: Vec<Value>,
) -> WorkspaceError {
    let first = diagnostics.first().expect("at least one diagnostic");
    let code = first.code;
    let mut message = match first.hunk_index {
        Some(index) => format!("{}: hunk #{}: {}", first.file, index + 1, first.message),
        None => format!("{}: {}", first.file, first.message),
    };
    if diagnostics.len() > 1 {
        message.push_str(&format!(
            " ({} problems in this patch; see details.diagnostics)",
            diagnostics.len()
        ));
    }
    let truncated = diagnostics.len() > MAX_DIAGNOSTICS;
    let listed = diagnostics
        .iter()
        .take(MAX_DIAGNOSTICS)
        .map(Diagnostic::to_value)
        .collect::<Vec<_>>();
    WorkspaceError::ToolDetails {
        code,
        message,
        // 分类跟着错误码走：NOT_FOUND 一直是 not_found 类，别因为它现在从
        // 补丁诊断里出来就换一个类别。
        category: match code {
            "NOT_FOUND" => "not_found",
            "FILE_VERSION_CONFLICT" => "conflict",
            _ => "validation",
        },
        retryable: false,
        details: json!({
            "stage": "patch",
            // 校验失败一律整批不落盘。说出来，模型才不会先去"收拾现场"。
            "files_changed": false,
            "problem_count": diagnostics.len(),
            "diagnostics": listed,
            "diagnostics_truncated": truncated,
            "not_checked": not_checked,
            "suggestion": "按 diagnostics[].suggested_read_range 重读这些文件，照它们现在的样子重建对不上的那几段，再把整个补丁重新提交"
        }),
    }
}

/// 文件在这之间被写过。和 `PATCH_FAILED` 分开：那个是补丁自己写错了，
/// 这个是补丁没错而世界变了——下一步是重读文件，不是琢磨 hunk。
pub(super) fn version_conflict(message: impl Into<String>) -> WorkspaceError {
    WorkspaceError::Tool {
        code: "FILE_VERSION_CONFLICT",
        message: message.into(),
        category: "conflict",
        retryable: false,
    }
}

pub(super) fn version_value(version: Option<String>) -> Value {
    version.map(Value::String).unwrap_or(Value::Null)
}

pub(super) fn state_version(state: &FileState) -> Option<String> {
    match state {
        FileState::Present(version) => Some(version.clone()),
        _ => None,
    }
}

/// 核对调用方给的版本前置条件；过不了就给一条诊断。
///
/// 三态各自的判法（跨仓评审 X02）：
///
/// | 磁盘上 | 说"应当是版本 v" | 说"应当什么都没有"（null） | 没给前置条件 |
/// | --- | --- | --- | --- |
/// | 在，版本一样 | 过 | 冲突：它在 | 过 |
/// | 在，版本不同 | 冲突：被写过 | 冲突：它在 | 过 |
/// | 确实不在 | 冲突：没了 | 过 | 过 |
/// | **问不出来** | **拒** | **拒** | **拒** |
///
/// 最后一行是这次收口的重点：读不出属性不等于文件不在。把它当成"不在"，
/// 一个"这路径应当是空的"前置条件就会在文件其实还在的时候放行。没给前置
/// 条件也照样拒——状态都问不出来，后面的落盘复核同样没法做。
///
/// 带的版本对不上、但从那个版本到现在全是这个调用方自己经 gld 写的（`self_written`），
/// 不算冲突：它只是没换上自己上次 `apply_patch` 回的新版本号，没人动过它改的东西。
/// 放行，在 `notes` 里留一句。
pub(super) fn version_precondition(
    display: &str,
    operation: &'static str,
    state: &FileState,
    expected_versions: &Option<HashMap<String, Option<String>>>,
    self_written: &dyn Fn(&str, &str) -> bool,
    notes: &mut Vec<String>,
) -> Option<Diagnostic> {
    if let FileState::Unknown(reason) = state {
        return Some(Diagnostic::version_conflict(
            format!(
                "cannot tell the state of {display} ({reason}), so this patch's preconditions cannot be checked; nothing was written"
            ),
            display.to_string(),
            operation,
            None,
            None,
        ));
    }
    let expected = expected_versions.as_ref()?.get(display)?;
    let actual = state_version(state);
    if let (Some(then), Some(now)) = (expected, &actual) {
        if then != now && self_written(then, now) {
            notes.push(format!(
                "{display}: expected_versions 带的 {then} 是你自己上次改它之前的版本；之后只有你经 gld 的写入，按现在的 {now} 处理。下次直接用 apply_patch 回包 file_versions 里的版本"
            ));
            return None;
        }
    }
    (expected != &actual).then(|| {
        Diagnostic::version_conflict(
            version_conflict_message(display, expected, &actual),
            display.to_string(),
            operation,
            expected.clone(),
            actual,
        )
    })
}

fn version_conflict_message(
    display: &str,
    expected: &Option<String>,
    actual: &Option<String>,
) -> String {
    match (expected, actual) {
        (Some(expected), Some(actual)) => format!(
            "{display} has changed since you read it (version {expected} is now {actual}); read it again before patching, or your edit would overwrite whatever changed"
        ),
        (Some(expected), None) => format!(
            "{display} is gone (you had version {expected}); it was deleted after you read it"
        ),
        (None, Some(actual)) => format!(
            "{display} already exists (version {actual}), but the precondition you sent says this path should be empty; read it and decide whether to update it instead"
        ),
        (None, None) => format!("{display}: version precondition failed"),
    }
}

pub(super) fn operation_label(file: &FilePatch) -> &'static str {
    if file.is_deleted {
        "delete"
    } else if file.is_new_file {
        "add"
    } else {
        "update"
    }
}

/// `expected_versions`：路径 → 那时的版本号，`null` 表示"那时这里应当什么
/// 都没有"。形状和 `patch_check` 回的 `observed_versions` 一模一样，原样粘
/// 回来就行。
pub(super) fn expected_versions(
    args: &Value,
) -> Result<Option<HashMap<String, Option<String>>>, WorkspaceError> {
    let Some(raw) = args.get("expected_versions") else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    let object = raw.as_object().ok_or_else(|| {
        WorkspaceError::invalid_argument(
            "expected_versions must be an object mapping paths to the version read_file or patch_check returned (null means the file should not exist)",
        )
    })?;
    let mut map = HashMap::with_capacity(object.len());
    for (path, value) in object {
        let version = match value {
            Value::Null => None,
            Value::String(version) => Some(version.clone()),
            _ => {
                return Err(WorkspaceError::invalid_argument(format!(
                    "expected_versions[{path}] must be a version string or null"
                )))
            }
        };
        map.insert(path.clone(), version);
    }
    Ok(Some(map))
}

/// 没走到逐段比对就失败了（补丁本身解析不了之类）。`reason_code` 和
/// `details.diagnostics[].reason_code` 是同一套名字：日志按它分类，消息是给模型读的。
pub(super) fn patch_failed(
    reason_code: &'static str,
    message: impl Into<String>,
) -> WorkspaceError {
    WorkspaceError::ToolDetails {
        code: "PATCH_FAILED",
        message: message.into(),
        category: "validation",
        retryable: false,
        details: json!({ "reason_code": reason_code, "files_changed": false }),
    }
}

//! Tool output summaries shared by dispatch and request logging.

use serde_json::{json, Value};

/// 补丁是哪种写法。解析和日志都按这一个判，别各判各的。
pub fn patch_format(patch: &str) -> &'static str {
    if patch
        .lines()
        .any(|line| line.trim_end_matches('\r') == "*** Begin Patch")
    {
        "codex"
    } else {
        "unified"
    }
}

/// operations.jsonl 里这次打补丁记什么：写法、大小；失败时再加诊断。
///
/// 成功的也记写法和大小，才算得出哪种写法、多大的补丁容易失败。诊断只挑
/// 行号、计数、原因码和版本号：`message`、`actual_excerpt` 带文件原文，不进来
/// ——operations 会经 `operation_log` 回给调用方，规矩是只记码不记内容。
pub fn operation_summary(args: &Value, output: &Value) -> Value {
    let patch = args.get("patch").and_then(Value::as_str).unwrap_or("");
    let mut summary = json!({
        // 只有 notebook_edits、没有文本补丁时是 none。
        "format": if patch.is_empty() { "none" } else { patch_format(patch) },
        "patch_bytes": patch.len()
    });
    if output.get("ok").and_then(Value::as_bool) != Some(false) {
        return summary;
    }
    let details = output.pointer("/error/details");
    let field = |key: &str| details.and_then(|details| details.get(key));
    summary["reason_code"] = field("reason_code").cloned().unwrap_or(Value::Null);
    summary["problem_count"] = field("problem_count").cloned().unwrap_or(Value::Null);
    summary["diagnostics"] = field("diagnostics")
        .and_then(Value::as_array)
        .map(|list| list.iter().map(diagnostic_summary).collect())
        .unwrap_or_default();
    summary["not_checked"] = json!(field("not_checked")
        .and_then(Value::as_array)
        .map_or(0, Vec::len));
    summary
}

fn diagnostic_summary(diagnostic: &Value) -> Value {
    const KEPT: [&str; 11] = [
        "file",
        "operation",
        "baseline",
        "code",
        "reason_code",
        "hunk_index",
        "expected_range",
        "candidate_ranges",
        "near_miss",
        "expected_version",
        "actual_version",
    ];
    Value::Object(
        KEPT.iter()
            .filter_map(|key| Some((key.to_string(), diagnostic.get(*key)?.clone())))
            .collect(),
    )
}

/// 请求日志里补丁失败那一行的字段：`code=… reasons=…`。成功的不出这一行。
///
/// `reasons` 每个对不上的地方一条，是原因码加上差在哪（有的话），比如
/// `context_drifted:trailing_whitespace`；没走到逐段比对的就是那一个原因码。
/// 全文在 operations.jsonl，靠同一行里的 `operation_id` 对上。
pub fn failure_log_fields(output: &Value) -> Option<String> {
    if output.get("ok").and_then(Value::as_bool) != Some(false) {
        return None;
    }
    let code = output
        .pointer("/error/code")
        .and_then(Value::as_str)
        .unwrap_or("");
    let details = output.pointer("/error/details");
    let reasons = match details
        .and_then(|details| details.get("diagnostics"))
        .and_then(Value::as_array)
    {
        Some(list) => list
            .iter()
            .map(|diagnostic| {
                let reason = diagnostic["reason_code"].as_str().unwrap_or("unknown");
                match diagnostic
                    .pointer("/near_miss/kind")
                    .and_then(Value::as_str)
                {
                    Some(kind) => format!("{reason}:{kind}"),
                    None => reason.to_string(),
                }
            })
            .collect::<Vec<_>>(),
        None => details
            .and_then(|details| details.get("reason_code"))
            .and_then(Value::as_str)
            .map(|reason| vec![reason.to_string()])
            .unwrap_or_default(),
    };
    let reasons = if reasons.is_empty() {
        "-".to_string()
    } else {
        reasons.join(",")
    };
    Some(format!("code={code} reasons={reasons}"))
}

pub(super) fn affected_paths(affected: &[Value], operation: &str) -> Vec<String> {
    affected
        .iter()
        .filter(|file| file["operation"] == operation)
        .filter_map(|file| file["path"].as_str().map(str::to_string))
        .collect()
}

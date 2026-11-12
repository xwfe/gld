//! 工具整型参数的默认值与上下界；纯数据契约，不读取注册表或运行时状态。

use serde_json::Value;

/// `(tool, argument, default, minimum, maximum)`.
pub const BOUNDED: &[(&str, &str, u64, u64, u64)] = &[
    ("read_file", "max_bytes", 32_768, 1, 1_048_576),
    ("read_notebook", "max_bytes", 32_768, 1, 1_048_576),
    ("list_dir", "max_depth", 1, 1, 20),
    ("list_dir", "max_entries", 100, 1, 10_000),
    ("list_files", "max_results", 5_000, 1, 50_000),
    ("search_text", "context_lines", 0, 0, 20),
    ("search_text", "max_preview_bytes", 256, 64, 4_096),
    ("search_text", "max_results", 100, 1, 10_000),
    ("search_text", "max_file_bytes", 2_097_152, 1, 67_108_864),
    // 上限是服务模式（带 service_port）的 1 小时；普通命令另由策略和 exec 限在 10 分钟。
    ("exec_command", "timeout_ms", 30_000, 1, 3_600_000),
    ("exec_command", "max_output_bytes", 32_768, 1_024, 1_048_576),
    ("exec_command", "yield_time_ms", 1_000, 0, 30_000),
    ("write_stdin", "yield_time_ms", 1_000, 0, 30_000),
    ("write_stdin", "max_output_bytes", 32_768, 1, 1_048_576),
    ("kill_session", "wait_ms", 5_000, 0, 30_000),
    ("kill_session", "max_output_bytes", 32_768, 1, 1_048_576),
    ("read_output", "limit", 4_096, 1, 1_048_576),
    ("list_runs", "limit", 20, 1, 64),
    ("git_status", "max_entries", 500, 1, 10_000),
    ("git_diff", "context_lines", 3, 0, 20),
    ("git_diff", "max_bytes", 65_536, 1_024, 1_048_576),
    ("git_log", "max_count", 20, 1, 100),
    ("git_log", "skip", 0, 0, 10_000),
    ("git_show", "context_lines", 3, 0, 20),
    ("git_show", "max_bytes", 65_536, 1, 1_048_576),
    ("git_blame", "max_lines", 200, 1, 1_000),
    ("view_image", "max_bytes", 5_242_880, 1_024, 10_485_760),
    ("view_image", "max_width", 2_000, 1, 10_000),
    // 委派（RFC-0009）：总时长默认半小时、最长 2 小时；wait 单次最多 30 秒，和 exec_command 一致。
    ("agent_manage", "timeout_minutes", 30, 1, 120),
    ("agent_manage", "wait_seconds", 30, 0, 30),
    // action=diff 每页最多多少字节：只想看大数据文件开头几行时不用收一整页（审查 §54）。
    ("agent_manage", "max_bytes", 65_536, 1_024, 65_536),
    ("view_image", "max_height", 2_000, 1, 10_000),
    ("operation_log", "limit", 50, 1, 200),
    ("list_task_events", "limit", 50, 1, 200),
    ("task_context", "max_bytes", 32_768, 8_192, 131_072),
    ("project_state", "max_files", 200, 1, 10_000),
];

/// A bounded integer argument. Missing or not a non-negative integer means
/// the default, as it always did; a value outside the declared range is
/// brought to the nearest end of it instead of being obeyed or refused.
pub fn bounded(args: &Value, tool: &str, arg: &str) -> u64 {
    let (default, min, max) = range(tool, arg)
        .unwrap_or_else(|| panic!("{tool}.{arg} is not in gld_protocol::tool_args::BOUNDED"));
    args.get(arg)
        .and_then(Value::as_u64)
        .unwrap_or(default)
        .clamp(min, max)
}

pub fn range(tool: &str, arg: &str) -> Option<(u64, u64, u64)> {
    BOUNDED
        .iter()
        .find(|(t, a, ..)| *t == tool && *a == arg)
        .map(|&(_, _, default, min, max)| (default, min, max))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_range_values_are_clamped_and_junk_is_the_default() {
        let args = serde_json::json!({ "max_results": 1_000_000, "context_lines": -3, "max_preview_bytes": 1 });
        assert_eq!(bounded(&args, "search_text", "max_results"), 10_000);
        assert_eq!(bounded(&args, "search_text", "context_lines"), 0);
        assert_eq!(bounded(&args, "search_text", "max_preview_bytes"), 64);
        assert_eq!(bounded(&args, "search_text", "max_file_bytes"), 2_097_152);
    }
}

//! Integer tool arguments whose schema declares a range.
//!
//! The schema is what a client reads before calling; the code is what it
//! gets. They used to be written separately and drifted: search_text said
//! "default 100" and returned up to 1000, several maxima were never applied.
//! Everything bounded now reads its default and range from this one table,
//! and the tests below fail when a schema and the table disagree.

use serde_json::Value;

/// `(tool, argument, default, minimum, maximum)`.
const BOUNDED: &[(&str, &str, u64, u64, u64)] = &[
    ("read_file", "max_bytes", 32_768, 1, 1_048_576),
    ("list_dir", "max_depth", 1, 1, 20),
    ("list_dir", "max_entries", 100, 1, 10_000),
    ("list_files", "max_results", 5_000, 1, 50_000),
    ("search_text", "context_lines", 0, 0, 20),
    ("search_text", "max_preview_bytes", 256, 64, 4_096),
    ("search_text", "max_results", 100, 1, 10_000),
    ("search_text", "max_file_bytes", 2_097_152, 1, 67_108_864),
    ("exec_command", "timeout_ms", 30_000, 1, 600_000),
    ("exec_command", "max_output_bytes", 32_768, 1_024, 1_048_576),
    ("exec_command", "yield_time_ms", 1_000, 0, 30_000),
    ("write_stdin", "yield_time_ms", 1_000, 0, 30_000),
    ("write_stdin", "max_output_bytes", 32_768, 1, 1_048_576),
    ("kill_session", "wait_ms", 5_000, 0, 30_000),
    ("kill_session", "max_output_bytes", 32_768, 1, 1_048_576),
    ("read_output", "limit", 4_096, 1, 1_048_576),
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
    ("view_image", "max_height", 2_000, 1, 10_000),
    ("operation_log", "limit", 50, 1, 200),
    ("list_task_events", "limit", 50, 1, 200),
    ("task_context", "max_bytes", 32_768, 8_192, 131_072),
    ("project_state", "max_files", 200, 1, 10_000),
];

/// A bounded integer argument. Missing or not a non-negative integer means
/// the default, as it always did; a value outside the declared range is
/// brought to the nearest end of it instead of being obeyed or refused.
pub(crate) fn bounded(args: &Value, tool: &str, arg: &str) -> u64 {
    let (default, min, max) =
        range(tool, arg).unwrap_or_else(|| panic!("{tool}.{arg} is not in tools::args::BOUNDED"));
    args.get(arg)
        .and_then(Value::as_u64)
        .unwrap_or(default)
        .clamp(min, max)
}

fn range(tool: &str, arg: &str) -> Option<(u64, u64, u64)> {
    BOUNDED
        .iter()
        .find(|(t, a, ..)| *t == tool && *a == arg)
        .map(|&(_, _, default, min, max)| (default, min, max))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::registry::list_tools_for_profile;
    use regex::Regex;

    /// Tools whose bounded arguments are checked somewhere else, and why.
    const NOT_HERE: &[(&str, &str)] = &[
        // History tools refuse out-of-range values with their own error
        // codes (tools::history::bounded_usize) rather than clamping.
        ("history_session_search", "limit"),
        ("history_session_read", "max_bytes"),
        ("history_manage", "limit"),
        ("history_manage", "max_bytes"),
        // Hidden from clients and not read by any code.
        ("request_permissions", "ttl_seconds"),
    ];

    /// The aggregate tool routes these arguments to the tool that owns them.
    fn owner<'a>(tool: &'a str, arg: &str) -> &'a str {
        match (tool, arg) {
            ("grep_text", _) => "search_text",
            ("task_manage", "limit") => "operation_log",
            ("task_manage", "max_files") => "project_state",
            ("task_manage", "max_bytes") => "task_context",
            _ => tool,
        }
    }

    #[test]
    fn every_bounded_integer_in_a_schema_matches_the_code() {
        let mut checked = 0;
        for tool in list_tools_for_profile("advanced") {
            let name = tool["name"].as_str().unwrap();
            let Some(properties) = tool["inputSchema"]["properties"].as_object() else {
                continue;
            };
            for (arg, schema) in properties {
                if schema["type"] != "integer" || schema.get("maximum").is_none() {
                    continue;
                }
                if NOT_HERE.contains(&(name, arg.as_str())) {
                    continue;
                }
                let (default, min, max) = range(owner(name, arg), arg)
                    .unwrap_or_else(|| panic!("{name}.{arg} has a schema range but no entry"));
                assert_eq!(
                    schema["minimum"].as_u64(),
                    Some(min),
                    "{name}.{arg} minimum"
                );
                assert_eq!(
                    schema["maximum"].as_u64(),
                    Some(max),
                    "{name}.{arg} maximum"
                );
                if let Some(declared) = schema.get("default") {
                    assert_eq!(declared.as_u64(), Some(default), "{name}.{arg} default");
                }
                checked += 1;
            }
        }
        assert!(
            checked >= BOUNDED.len(),
            "only {checked} schema ranges checked"
        );
    }

    /// A call site naming a pair the table does not have would panic at run
    /// time, so every literal call in the tool sources is checked here.
    #[test]
    fn every_call_site_names_an_entry() {
        let sources = [
            include_str!("exec.rs"),
            include_str!("file.rs"),
            include_str!("git.rs"),
            include_str!("image_tool.rs"),
            include_str!("session.rs"),
            include_str!("../harness/tools.rs"),
        ];
        let call = Regex::new(r#"bounded\(\s*[^,]+,\s*"([a-z_]+)",\s*"([a-z_]+)""#).unwrap();
        let mut calls = 0;
        for source in sources {
            for found in call.captures_iter(source) {
                assert!(
                    range(&found[1], &found[2]).is_some(),
                    "{}.{} is not in BOUNDED",
                    &found[1],
                    &found[2]
                );
                calls += 1;
            }
        }
        assert_eq!(
            calls,
            BOUNDED.len(),
            "every entry should have exactly one call site"
        );
    }

    #[test]
    fn out_of_range_values_are_clamped_and_junk_is_the_default() {
        let args = serde_json::json!({ "max_results": 1_000_000, "context_lines": -3, "max_preview_bytes": 1 });
        assert_eq!(bounded(&args, "search_text", "max_results"), 10_000);
        assert_eq!(bounded(&args, "search_text", "context_lines"), 0);
        assert_eq!(bounded(&args, "search_text", "max_preview_bytes"), 64);
        assert_eq!(bounded(&args, "search_text", "max_file_bytes"), 2_097_152);
    }
}

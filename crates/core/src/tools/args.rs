//! Tool arguments: the declared range of the bounded ones, and the rule that
//! a name nobody declared does not quietly disappear.
//!
//! The schema is what a client reads before calling; the code is what it
//! gets. They used to be written separately and drifted: search_text said
//! "default 100" and returned up to 1000, several maxima were never applied.
//! Everything bounded now reads its default and range from gld_protocol::tool_args,
//! and the tests below fail when a schema and the table disagree.

use serde_json::{json, Value};

use crate::tools::registry::{canonical_tool_name, input_schema};
use crate::tools::workspace::WorkspaceError;

pub(crate) use gld_protocol::tool_args::bounded;
#[cfg(test)]
use gld_protocol::tool_args::{range, BOUNDED};

/// 多给的参数要当场说，不能悄悄扔掉。
///
/// 每个 schema 都写了 `additionalProperties: false`，但那只是**给客户端看的
/// 声明**——真正决定行为的是这边读了哪几个 key，多出来的以前直接被忽略。
/// 于是把名字写错（`timeout` 而不是 `timeout_ms`）、或者照着别的工具的参数表
/// 来调（往 `patch_check` 里塞 `dry_run`），都变成"按默认值跑了一遍"：调用方
/// 以为自己设了超时/只是预检，实际完全是另一回事，而且**返回值里看不出来**。
///
/// 允许的名字直接从 schema 取，不另写一份表——两份迟早对不上，而客户端读的是
/// schema 那份。下划线开头的是服务端自己注入的（`_host_session_key` 由 MCP
/// server 填），本来就不在 schema 里，不能拒。
///
/// schema 明说收任意键的工具（`additionalProperties` 不是 `false`）不在此列。
pub(crate) fn reject_unknown(tool: &str, args: &Value) -> Result<(), WorkspaceError> {
    let Some(given) = args.as_object() else {
        return Ok(());
    };
    let schema = input_schema(canonical_tool_name(tool));
    if schema.get("additionalProperties") != Some(&Value::Bool(false)) {
        return Ok(());
    }
    let Some(known) = schema.get("properties").and_then(Value::as_object) else {
        return Ok(());
    };
    let unknown: Vec<String> = given
        .keys()
        .filter(|name| !name.starts_with('_') && !known.contains_key(*name))
        .cloned()
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    let accepted: Vec<String> = known.keys().cloned().collect();
    Err(WorkspaceError::ToolDetails {
        code: "INVALID_ARGUMENT",
        message: format!(
            "{tool} does not take {}. It takes: {}",
            unknown.join(", "),
            accepted.join(", ")
        ),
        category: "validation",
        retryable: false,
        details: json!({
            "unknown_arguments": unknown,
            "accepted_arguments": accepted,
            // 被拒的调用什么都没做：重发一次正确的即可，不用先去查状态。
            "executed": false
        }),
    })
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
        // A filter, not a size: clamping 20000 to 10080 would quietly list
        // a different set of runs, so list_runs refuses it instead.
        ("list_runs", "started_within_minutes"),
        // A port, not a size: clamping 70000 to 65535 would wait on someone
        // else's port, so exec_command refuses it (tools::service::port_arg).
        ("exec_command", "service_port"),
        ("check_command", "service_port"),
    ];

    /// The aggregate tool routes these arguments to the tool that owns them.
    fn owner<'a>(tool: &'a str, arg: &str) -> &'a str {
        match (tool, arg) {
            ("grep_text", _) => "search_text",
            // 预检判的是同一组 exec_command 参数，范围当然也得是同一份。
            ("check_command", _) => "exec_command",
            ("task_manage", "limit") => "operation_log",
            ("task_manage", "max_files") => "project_state",
            ("task_manage", "max_bytes") => "task_context",
            _ => tool,
        }
    }

    /// 工具实现读参数的地方，`args.get("x")` 或 `args["x"]`。
    ///
    /// 不扫 `registry.rs`：那是 schema 自己，拿它对自己没有意义。
    fn argument_names_read_by_the_code() -> std::collections::BTreeSet<String> {
        const SOURCES: &[(&str, &str)] = &[
            ("dispatch.rs", include_str!("dispatch.rs")),
            ("exec.rs", include_str!("exec.rs")),
            (
                "runtime/command_spec.rs",
                include_str!("../../../runtime/src/command_spec.rs"),
            ),
            (
                "runtime/exec/mod.rs",
                include_str!("../../../runtime/src/exec/mod.rs"),
            ),
            (
                "runtime/exec/native.rs",
                include_str!("../../../runtime/src/exec/native.rs"),
            ),
            (
                "runtime/exec/process.rs",
                include_str!("../../../runtime/src/exec/process.rs"),
            ),
            (
                "runtime/exec/resolve.rs",
                include_str!("../../../runtime/src/exec/resolve.rs"),
            ),
            (
                "runtime/exec/platform.rs",
                include_str!("../../../runtime/src/exec/platform.rs"),
            ),
            (
                "runtime/service.rs",
                include_str!("../../../runtime/src/service.rs"),
            ),
            ("file.rs", include_str!("../../../runtime/src/file.rs")),
            (
                "runtime/file/read.rs",
                include_str!("../../../runtime/src/file/read.rs"),
            ),
            (
                "runtime/file/listing.rs",
                include_str!("../../../runtime/src/file/listing.rs"),
            ),
            (
                "runtime/file/search.rs",
                include_str!("../../../runtime/src/file/search.rs"),
            ),
            ("git.rs", include_str!("git.rs")),
            (
                "image_tool.rs",
                include_str!("../../../runtime/src/image_tool.rs"),
            ),
            ("manage.rs", include_str!("manage.rs")),
            (
                "notebook.rs",
                include_str!("../../../runtime/src/notebook.rs"),
            ),
            ("patch.rs", include_str!("patch.rs")),
            (
                "runtime/patch.rs",
                include_str!("../../../runtime/src/patch.rs"),
            ),
            (
                "runtime/patch/validation.rs",
                include_str!("../../../runtime/src/patch/validation.rs"),
            ),
            ("planning.rs", include_str!("planning.rs")),
            ("policy.rs", include_str!("policy.rs")),
            (
                "runtime/session/output.rs",
                include_str!("../../../runtime/src/session/output.rs"),
            ),
            (
                "runtime/session/control.rs",
                include_str!("../../../runtime/src/session/control.rs"),
            ),
            (
                "runtime/session/list.rs",
                include_str!("../../../runtime/src/session/list.rs"),
            ),
            ("skill.rs", include_str!("skill.rs")),
            ("history/mod.rs", include_str!("history/mod.rs")),
            ("../harness/tools.rs", include_str!("../harness/tools.rs")),
        ];
        let read = Regex::new(r#"(?:args|arguments)\s*(?:\.get\(|\[)\s*"([a-z_0-9]+)""#).unwrap();
        let mut names = std::collections::BTreeSet::new();
        for (_, source) in SOURCES {
            for found in read.captures_iter(source) {
                // 下划线开头的是服务端自己注入的，本来就不在 schema 里。
                if !found[1].starts_with('_') {
                    names.insert(found[1].to_string());
                }
            }
        }
        names
    }

    /// 代码读了、schema 没写的参数 = 客户端永远发现不了的能力。
    ///
    /// 这不只是"文档缺一条"：`reject_unknown` 现在按 schema 拒未知参数，所以
    /// 没写进 schema 的名字一旦有人发过来就会被拒，那段读它的代码是死的。
    /// `include_ignored` 就是这么被抓出来的——`search_text` 一直读它，schema
    /// 里只有 `list_dir`/`list_files` 写了。
    #[test]
    fn every_argument_the_code_reads_is_declared_in_some_schema() {
        /// 故意不写进 schema 的名字，以及为什么。
        const ON_PURPOSE: &[(&str, &str)] = &[
            // 策略层专门拦它，报的是"服务端不让调用方设环境变量"，比
            // "没有这个参数"有用。声明一个只会被拒的参数更容易让人以为能用。
            (
                "env",
                "policy.rs 用 EnvironmentNotAllowed 单独拒，理由比未知参数具体",
            ),
        ];
        let mut declared = std::collections::BTreeSet::new();
        for tool in list_tools_for_profile("advanced") {
            collect_property_names(&tool["inputSchema"], &mut declared);
        }
        let undeclared: Vec<String> = argument_names_read_by_the_code()
            .into_iter()
            .filter(|name| !declared.contains(name))
            .filter(|name| !ON_PURPOSE.iter().any(|(excused, _)| excused == name))
            .collect();
        assert!(
            undeclared.is_empty(),
            "这些参数代码读了但没有任何 schema 声明，发过来会被 reject_unknown 拒掉：{undeclared:?}"
        );
    }

    /// 嵌套对象里的参数名也算声明过：`notebook_edits[].cell_id` 是在
    /// `notebook.rs` 里按名字读的，但它只出现在 items 的 properties 里。
    fn collect_property_names(schema: &Value, into: &mut std::collections::BTreeSet<String>) {
        if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
            for (name, child) in properties {
                into.insert(name.clone());
                collect_property_names(child, into);
            }
        }
        if let Some(items) = schema.get("items") {
            collect_property_names(items, into);
        }
        for branch in ["oneOf", "anyOf", "allOf"] {
            if let Some(list) = schema.get(branch).and_then(Value::as_array) {
                for child in list {
                    collect_property_names(child, into);
                }
            }
        }
    }

    #[test]
    fn unknown_arguments_are_refused_and_the_message_names_the_real_ones() {
        let error = reject_unknown(
            "exec_command",
            &json!({ "cmd": "echo hi", "timeout": 5_000 }),
        )
        .expect_err("timeout 不是 exec_command 的参数");
        assert_eq!(error.code(), "INVALID_ARGUMENT");
        // 报错要能直接改：说出多了哪个，也说出真名叫什么。
        assert!(error.message().contains("timeout_ms"), "{error}");
        assert!(reject_unknown("exec_command", &json!({ "cmd": "echo hi" })).is_ok());
        // MCP server 自己注入的内部键不在 schema 里，不能拒。
        assert!(reject_unknown(
            "history_session_bootstrap",
            &json!({ "session_key": "s", "_host_session_key": "chatgpt" })
        )
        .is_ok());
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
            include_str!("agent.rs"),
            include_str!("../../../runtime/src/exec/mod.rs"),
            include_str!("../../../runtime/src/file.rs"),
            include_str!("../../../runtime/src/file/read.rs"),
            include_str!("../../../runtime/src/file/listing.rs"),
            include_str!("../../../runtime/src/file/search.rs"),
            include_str!("git.rs"),
            include_str!("../../../runtime/src/image_tool.rs"),
            include_str!("../../../runtime/src/notebook.rs"),
            include_str!("../../../runtime/src/session/output.rs"),
            include_str!("../../../runtime/src/session/control.rs"),
            include_str!("../../../runtime/src/session/list.rs"),
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
}

//! hub 暴露给 Web AI 的远端只读工具。
//!
//! **是一份静态名单，不是把远端的 `tools/list` 转出去。**这是跨仓计划 v2
//! 第 7 节定的：「只开放已评审工具，不自动跟随上游增加权限」。远端 ccnm
//! 哪天加了个工具，这边不声明就调不到；哪天给某个工具加了参数，这边不
//! 声明就传不过去。两者都得先有人看过再加进来。
//!
//! 名字都带 `remote_` 前缀：本地成员的 `read_file` 和远端的不是一个契约
//! （分页、错误码、参数都不同），同名会让模型以为可以混着用。gld 本机也有
//! `search_text` 和 `list_files`，重名的坑是真的。
//!
//! ## 参数名的出处：源码，不是 fixture
//!
//! 最早这份名单是照着 ccnm 冻结协议的 `tools-list-read.json` fixture 写的。
//! **那份 fixture 是删节过的**，2026-09-16 跟真二进制（ccnm 0.7.0）对了一遍：
//! `read_file` 实际还有 `end_line` / `max_bytes`，`list_files` 还有
//! `include_hidden`，`search_text` 还有 `glob` / `case_sensitive` /
//! `context_lines`，fixture 里一个都没有。更糟的是 `tools-list-coding.json`
//! 把 `apply_patch` 的参数写成 `changes`，而实现收的是 `files`——照抄就是
//! 每次 patch 都失败。
//!
//! 所以现在的出处是 **ccnm 的 `*Args` 结构体**（`crates/ccnm-core/src/mcp/`
//! 下的 `read.rs` / `list.rs` / `search.rs` / `patch.rs` / `exec.rs` /
//! `output.rs`），它们就是 schemars 生成 schema 的那个源头。加参数前先去那儿
//! 核一眼，别信 fixture。

use serde_json::{json, Value};

/// hub 用来路由到哪个成员的参数名。转发给 ccnm 之前会被摘掉——它是 gld
/// 的东西，ccnm 不认识。
pub const WORKSPACE_ARG: &str = "workspace";

/// coding 工具用来点名会话的参数名，和 `workspace` 一样不往外转发。
pub const HANDLE_ARG: &str = "coding_handle";

/// 一个远端工具：hub 这边叫什么、转发到 ccnm 的哪个工具、能带哪些参数。
pub struct RemoteTool {
    /// hub 暴露的名字。
    pub name: &'static str,
    /// 转发到远端时用的名字。
    pub remote_name: &'static str,
    pub description: &'static str,
    /// 允许转发的参数名。**白名单**：不在这里的一律不往外发。
    pub arguments: &'static [Argument],
    /// 这个工具会不会改东西。决定 `annotations`，也决定它要不要 coding 会话。
    pub effect: Effect,
}

/// 工具对远端的影响。annotations 照 ccnm 冻结协议第 5 节的那张表。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// 只读。read 和 coding 模式都有。
    Read,
    /// 会改文件。只有 coding 模式有。
    Write,
    /// 跑任意命令。只有 coding 模式有，而且永远按 destructive + open-world
    /// 处理——不因为这次命令看着像 `ls` 就改注解（协议第 5 节第 2 条）。
    Exec,
}

impl Effect {
    fn annotations(self) -> Value {
        match self {
            Effect::Read => json!({ "readOnlyHint": true, "openWorldHint": false }),
            Effect::Write => json!({
                "readOnlyHint": false, "destructiveHint": true,
                "idempotentHint": false, "openWorldHint": false
            }),
            Effect::Exec => json!({
                "readOnlyHint": false, "destructiveHint": true,
                "idempotentHint": false, "openWorldHint": true
            }),
        }
    }
}

pub struct Argument {
    pub name: &'static str,
    pub ty: Type,
    pub required: bool,
    /// 给模型看的一句话。空串就不写进 schema。
    pub note: &'static str,
}

/// 参数的 JSON 形状。
///
/// 不用一个 `&'static str` 装 JSON Schema 的 `type` 了：`exec_command.cmd`
/// 是**字符串数组**（程序加参数，不是 shell 行），只写 `"array"` 的话模型
/// 完全可能传一个字符串进去，而那正是验收项 H04 点名要防的
/// 「绝不将 gld 字符串 cmd 当 ccnm argv」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    String,
    Integer,
    Boolean,
    /// 字符串数组，例如 `["cargo", "test"]`。
    StringArray,
    /// 对象数组。里面的形状由远端校验，gld 不复述——见
    /// [`RemoteTool::forward_arguments`] 的「只挑不改」。
    ObjectArray,
    /// 固定取值。
    OneOf(&'static [&'static str]),
}

impl Type {
    fn schema(self) -> Value {
        match self {
            Type::String => json!({ "type": "string" }),
            Type::Integer => json!({ "type": "integer" }),
            Type::Boolean => json!({ "type": "boolean" }),
            Type::StringArray => json!({ "type": "array", "items": { "type": "string" } }),
            Type::ObjectArray => json!({ "type": "array", "items": { "type": "object" } }),
            Type::OneOf(values) => json!({ "type": "string", "enum": values }),
        }
    }
}

const fn arg(name: &'static str, ty: Type, note: &'static str) -> Argument {
    Argument {
        name,
        ty,
        required: false,
        note,
    }
}

const fn required(name: &'static str, ty: Type, note: &'static str) -> Argument {
    Argument {
        name,
        ty,
        required: true,
        note,
    }
}

/// 只读模式开放的四个。跟 ccnm `--mode read` 给的四个一一对应。
///
/// 没有 `exec_command`、没有 `apply_patch`、没有 `read_output`——只读模式
/// 产不出 `output_ref`，开了也没东西可读（ccnm 协议 4.3 专门解释了这一格）。
pub const READ_TOOLS: &[RemoteTool] = &[
    RemoteTool {
        name: "remote_workspace_info",
        remote_name: "workspace_info",
        description: "Name, git status and platform of a remote workspace. Call it once before working in that workspace; every other remote path is relative to its root.",
        arguments: &[],
        effect: Effect::Read,
    },
    RemoteTool {
        name: "remote_read_file",
        remote_name: "read_file",
        description: "Read a text file from a remote workspace, as numbered lines. Paths are relative to that workspace's root. Long files come back in pages with the line to continue from, and the version you need to edit them.",
        arguments: &[
            required("path", Type::String, "Relative to the remote workspace root."),
            arg("start_line", Type::Integer, "1-based line to start at."),
            arg("max_lines", Type::Integer, "At most 2000; the reply says where to continue."),
            arg("end_line", Type::Integer, "Last line to read, instead of a count."),
            arg("max_bytes", Type::Integer, "At most 65536. Default 32768."),
        ],
        effect: Effect::Read,
    },
    RemoteTool {
        name: "remote_list_files",
        remote_name: "list_files",
        description: "List a directory of a remote workspace, or search it with a glob. Without a glob you get the immediate children of one directory. Files .gitignore rules out are never listed.",
        arguments: &[
            arg("path", Type::String, "Directory to list. Default: the workspace root."),
            arg("glob", Type::String, "Match recursively under `path`, e.g. `**/*.{rs,toml}`."),
            arg("include_hidden", Type::Boolean, "Include names starting with a dot."),
            arg("max_entries", Type::Integer, "At most 1000."),
        ],
        effect: Effect::Read,
    },
    RemoteTool {
        name: "remote_search_text",
        remote_name: "search_text",
        description: "Search a remote workspace for a string, or a regex if you ask for one. The search runs where the files are, so only the matching lines cross the network.",
        arguments: &[
            required("query", Type::String, "Literal text, or a regex when `regex` is true."),
            arg("regex", Type::Boolean, "Treat `query` as a regular expression."),
            arg("path", Type::String, "Limit the search to this subdirectory."),
            arg("glob", Type::String, "Limit the search to matching files, e.g. `**/*.rs`."),
            arg("case_sensitive", Type::Boolean, "Default is case-insensitive."),
            arg("context_lines", Type::Integer, "Lines of context around each hit, at most 10."),
            arg("max_results", Type::Integer, "At most 200."),
        ],
        effect: Effect::Read,
    },
];

/// coding 模式多出来的三个。**只有拿到 coding 会话句柄才调得到。**
///
/// 参数名照 ccnm 的 `*Args` 结构体，不照 fixture——见模块头那段。
pub const CODING_TOOLS: &[RemoteTool] = &[
    RemoteTool {
        name: "remote_apply_patch",
        remote_name: "apply_patch",
        description: "Change files in a remote workspace: add, update, delete or move. This is the only way to write there. An update replaces exact strings and must carry the `version` remote_read_file returned, so an edit built on content that has since changed is refused. Either every file in the patch is applied or none is.",
        arguments: &[
            required(
                "files",
                Type::ObjectArray,
                "One entry per file: {op: add|update|delete|move, path, and then content for add, edits:[{old,new,replace_all}] plus version for update, version for delete, to plus version for move}. At most 50 files and 1 MiB of new content per call.",
            ),
            arg("dry_run", Type::Boolean, "Check everything and report what would happen, writing nothing."),
        ],
        effect: Effect::Write,
    },
    RemoteTool {
        name: "remote_exec_command",
        remote_name: "exec_command",
        description: "Run a command in a remote workspace. `cmd` is a program and its arguments as an array, NOT a shell line: there are no pipes, redirection or globs. Long output stays on that machine; what comes back is the head and tail plus an output_ref for remote_read_output. This runs with the full access of the account the remote runtime uses.",
        arguments: &[
            required(
                "cmd",
                Type::StringArray,
                "Program and arguments, e.g. [\"cargo\", \"test\", \"--lib\"]. Never a single shell string.",
            ),
            arg("cwd", Type::String, "Directory to run in, relative to the workspace root."),
            arg("timeout_ms", Type::Integer, "Kill the command after this long. Default 120000, max 600000."),
            arg("preview_bytes", Type::Integer, "Bytes of output to return inline. Default 4096, max 16384."),
        ],
        effect: Effect::Exec,
    },
    RemoteTool {
        name: "remote_read_output",
        remote_name: "read_output",
        description: "Page through what a remote command wrote, using the output_ref remote_exec_command returned. Offsets are byte offsets and stable. An output_ref only means something inside the coding session that produced it.",
        arguments: &[
            required("output_ref", Type::String, "The output_ref remote_exec_command returned."),
            arg("stream", Type::OneOf(&["stdout", "stderr"]), "Default stdout."),
            arg("offset", Type::Integer, "Byte offset to start at. Default 0."),
            arg("limit", Type::Integer, "Bytes to return, at most 32768. Default 16384."),
        ],
        effect: Effect::Read,
    },
];

/// 按 hub 这边的名字找工具。找不到就是没开放——不去问远端有没有。
///
/// **read 和 coding 两张表都找**：找到不等于能调，能不能调由 hub 按成员
/// 模式和会话句柄决定。这里只回答「gld 认不认识这个名字」。
pub fn find(name: &str) -> Option<&'static RemoteTool> {
    READ_TOOLS
        .iter()
        .chain(CODING_TOOLS)
        .find(|tool| tool.name == name)
}

/// 这个工具要不要一个 coding 会话。
pub fn needs_coding(tool: &RemoteTool) -> bool {
    CODING_TOOLS.iter().any(|t| t.name == tool.name)
}

impl RemoteTool {
    /// `tools/list` 里的一条。
    pub fn definition(&self) -> Value {
        let mut properties = json!({
            WORKSPACE_ARG: {
                "type": "string",
                "description": "Which remote workspace, by the id or name from list_workspaces."
            }
        });
        let mut required_names = vec![json!(WORKSPACE_ARG)];
        if needs_coding(self) {
            properties[HANDLE_ARG] = json!({
                "type": "string",
                "description": "The handle remote_coding_begin returned for this workspace."
            });
            required_names.push(json!(HANDLE_ARG));
        }
        for argument in self.arguments {
            let mut schema = argument.ty.schema();
            if !argument.note.is_empty() {
                schema["description"] = json!(argument.note);
            }
            properties[argument.name] = schema;
            if argument.required {
                required_names.push(json!(argument.name));
            }
        }
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": {
                "type": "object",
                "properties": properties,
                "required": required_names,
                "additionalProperties": false
            },
            "annotations": self.effect.annotations()
        })
    }

    /// 把 hub 收到的参数挑成要发给 ccnm 的那份。
    ///
    /// **白名单**：只有这个工具声明过的参数会被带上，`workspace` 和任何
    /// 别的键都留在 gld 这边。所以模型没法借着某个远端工具往 ccnm 塞它
    /// 自己没声明的字段。
    ///
    /// 只挑不改：声明过的参数原样带过去，包括 `null`——`null` 在 ccnm 那边
    /// 是「没给」的合法写法，替它改成缺省是替远端做决定。**类型也不改**：
    /// `cmd` 收到字符串就原样发字符串，让远端按自己的契约拒，不替它猜成
    /// `["sh","-c",…]`（那正是验收项 H04 点名禁止的）。
    pub fn forward_arguments(&self, incoming: &Value) -> Value {
        let mut out = serde_json::Map::new();
        for argument in self.arguments {
            if let Some(value) = incoming.get(argument.name) {
                out.insert(argument.name.to_string(), value.clone());
            }
        }
        Value::Object(out)
    }

    /// 少了哪个必填参数。空的就是齐了。
    pub fn missing_required(&self, incoming: &Value) -> Vec<&'static str> {
        self.arguments
            .iter()
            .filter(|argument| argument.required && incoming.get(argument.name).is_none())
            .map(|argument| argument.name)
            .collect()
    }
}

/// 开一个远端 coding 会话。gld 自己的工具，不转发给 ccnm。
pub const CODING_BEGIN: &str = "remote_coding_begin";
/// 关掉一个远端 coding 会话。
pub const CODING_END: &str = "remote_coding_end";

/// 会话工具的名字（这两个不是 [`RemoteTool`]：它们不往远端转发）。
pub fn is_session_tool(name: &str) -> bool {
    name == CODING_BEGIN || name == CODING_END
}

fn coding_begin_definition() -> Value {
    json!({
        "name": CODING_BEGIN,
        "description": "Open a writing session on a remote workspace and get the handle the remote_apply_patch / remote_exec_command / remote_read_output tools need. Opening it takes that workspace's write lock on the remote machine for as long as the session lives, so open it only when you are about to change something, and call remote_coding_end as soon as you are done. If somebody else holds the lock this fails and says so; it never waits and never silently downgrades to reading.",
        "inputSchema": {
            "type": "object",
            "properties": {
                WORKSPACE_ARG: {
                    "type": "string",
                    "description": "Which remote workspace, by the id or name from list_workspaces."
                }
            },
            "required": [WORKSPACE_ARG],
            "additionalProperties": false
        },
        // 开会话本身不改文件，但它拿远端的写锁——对别人是有后果的，
        // 所以不敢标 readOnlyHint。
        "annotations": {
            "readOnlyHint": false, "destructiveHint": false,
            "idempotentHint": false, "openWorldHint": false
        }
    })
}

fn coding_end_definition() -> Value {
    json!({
        "name": CODING_END,
        "description": "Close a remote coding session and release that workspace's write lock. Any output_ref from this session stops working: they only exist inside the session that produced them, and there is no way to resume one.",
        "inputSchema": {
            "type": "object",
            "properties": {
                WORKSPACE_ARG: {
                    "type": "string",
                    "description": "Which remote workspace, by the id or name from list_workspaces."
                },
                HANDLE_ARG: {
                    "type": "string",
                    "description": "The handle remote_coding_begin returned."
                }
            },
            "required": [WORKSPACE_ARG, HANDLE_ARG],
            "additionalProperties": false
        },
        "annotations": {
            "readOnlyHint": false, "destructiveHint": false,
            "idempotentHint": true, "openWorldHint": false
        }
    })
}

/// `tools/list` 里的远端工具条目。
///
/// `with_coding` 为假时**连 `remote_coding_begin` 都不列**：一个成员配的是
/// 只读，列出一个必然失败的开会话工具只会引着模型去试。
pub fn definitions(with_coding: bool) -> Vec<Value> {
    let mut out: Vec<Value> = READ_TOOLS.iter().map(RemoteTool::definition).collect();
    if with_coding {
        out.push(coding_begin_definition());
        out.push(coding_end_definition());
        out.extend(CODING_TOOLS.iter().map(RemoteTool::definition));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_is_prefixed_and_maps_to_a_ccnm_tool() {
        for tool in READ_TOOLS {
            assert!(tool.name.starts_with("remote_"), "{}", tool.name);
            assert_eq!(
                tool.name,
                format!("remote_{}", tool.remote_name),
                "名字应该是 remote_ 加上远端工具名"
            );
        }
    }

    /// 只读模式产不出 output_ref，也不该出现任何会写的工具。
    #[test]
    fn nothing_that_writes_or_runs_is_in_the_read_set() {
        for forbidden in ["exec_command", "apply_patch", "read_output", "write_stdin"] {
            assert!(
                !READ_TOOLS.iter().any(|t| t.remote_name == forbidden),
                "只读名单里不该有 {forbidden}"
            );
        }
        assert_eq!(READ_TOOLS.len(), 4, "read 模式就是四个工具");
    }

    #[test]
    fn the_definition_requires_a_workspace_and_says_it_is_read_only() {
        let def = find("remote_read_file").expect("有这个工具").definition();
        assert_eq!(
            def["inputSchema"]["properties"]["workspace"]["type"],
            json!("string")
        );
        let required = def["inputSchema"]["required"].as_array().expect("required");
        assert!(required.contains(&json!("workspace")), "{required:?}");
        assert!(required.contains(&json!("path")), "{required:?}");
        assert_eq!(def["annotations"]["readOnlyHint"], json!(true));
        assert_eq!(def["inputSchema"]["additionalProperties"], json!(false));
    }

    /// 路由字段是 gld 的，不能跟着转发出去——ccnm 不认识 `workspace`。
    #[test]
    fn the_routing_field_does_not_get_forwarded() {
        let tool = find("remote_read_file").expect("有这个工具");
        let out = tool.forward_arguments(&json!({ "workspace": "m1", "path": "a.txt" }));
        assert_eq!(out, json!({ "path": "a.txt" }));
    }

    /// 没声明过的键一律不往外发，哪怕它在远端真的存在。
    /// 这条钉住的是「不自动跟随上游增加权限」。
    ///
    /// 用 `include_hidden` 当例子：它在 ccnm 的 `list_files` 上是真参数，
    /// 但 `read_file` 上没有，所以这边也不该替它转发过去。
    #[test]
    fn an_argument_this_side_never_declared_is_dropped() {
        let tool = find("remote_read_file").expect("有这个工具");
        let out = tool.forward_arguments(&json!({
            "path": "a.txt",
            "include_hidden": true,
            "encoding": "utf-16",
            "anything_else": true
        }));
        assert_eq!(out, json!({ "path": "a.txt" }), "只有声明过的 path 该过去");
    }

    /// `null` 在 ccnm 那边是「没给」的合法写法，不替它改成缺省。
    #[test]
    fn an_explicit_null_is_passed_through_as_it_is() {
        let tool = find("remote_list_files").expect("有这个工具");
        let out = tool.forward_arguments(&json!({ "path": null, "glob": "*.rs" }));
        assert_eq!(out, json!({ "path": null, "glob": "*.rs" }));
    }

    #[test]
    fn a_missing_required_argument_is_named() {
        let tool = find("remote_search_text").expect("有这个工具");
        assert_eq!(
            tool.missing_required(&json!({ "path": "src" })),
            vec!["query"]
        );
        assert!(tool.missing_required(&json!({ "query": "x" })).is_empty());
    }

    /// 没声明的名字不去问远端，直接就是没有。
    ///
    /// `find` 回答的是「gld 认不认识这个名字」，不是「现在能不能调」——
    /// 能不能调由 hub 按成员模式和会话句柄决定，所以 `remote_exec_command`
    /// 在这里是找得到的。
    #[test]
    fn an_unknown_tool_name_is_simply_not_there() {
        assert!(find("read_file").is_none(), "本地名字不该命中远端工具");
        assert!(find("remote_anything").is_none());
        assert!(
            find("remote_write_stdin").is_none(),
            "ccnm 根本没有这个工具"
        );
        assert!(
            find("remote_kill_session").is_none(),
            "同上，那是 gld 本地的"
        );
        // 会话工具不是 RemoteTool：它们不往远端转发。
        assert!(find(CODING_BEGIN).is_none());
        assert!(is_session_tool(CODING_BEGIN) && is_session_tool(CODING_END));
    }

    #[test]
    fn a_read_only_member_is_not_shown_any_coding_tool() {
        let names: Vec<String> = definitions(false)
            .iter()
            .filter_map(|d| d["name"].as_str().map(str::to_string))
            .collect();
        assert_eq!(names.len(), READ_TOOLS.len(), "{names:?}");
        assert!(
            names.contains(&"remote_workspace_info".to_string()),
            "{names:?}"
        );
        for forbidden in [
            CODING_BEGIN,
            CODING_END,
            "remote_apply_patch",
            "remote_exec_command",
        ] {
            assert!(
                !names.contains(&forbidden.to_string()),
                "{forbidden} 不该出现"
            );
        }
    }

    #[test]
    fn a_coding_member_gets_the_session_tools_too() {
        let names: Vec<String> = definitions(true)
            .iter()
            .filter_map(|d| d["name"].as_str().map(str::to_string))
            .collect();
        assert_eq!(
            names.len(),
            READ_TOOLS.len() + CODING_TOOLS.len() + 2,
            "{names:?}"
        );
        for expected in [
            CODING_BEGIN,
            CODING_END,
            "remote_apply_patch",
            "remote_exec_command",
            "remote_read_output",
        ] {
            assert!(names.contains(&expected.to_string()), "少了 {expected}");
        }
    }

    /// 改东西的工具必须带对 annotations，别的 Host 靠它决定要不要弹确认。
    /// exec 永远是 destructive + open-world，不因为某次命令看着安全就改。
    #[test]
    fn what_a_tool_can_do_is_declared_honestly() {
        let ann = |name: &str| find(name).expect(name).definition()["annotations"].clone();
        assert_eq!(ann("remote_read_file")["readOnlyHint"], json!(true));
        assert_eq!(
            ann("remote_read_output")["readOnlyHint"],
            json!(true),
            "读输出是只读的"
        );

        let patch = ann("remote_apply_patch");
        assert_eq!(patch["readOnlyHint"], json!(false));
        assert_eq!(patch["destructiveHint"], json!(true));
        assert_eq!(
            patch["openWorldHint"],
            json!(false),
            "patch 只动这个 workspace"
        );

        let exec = ann("remote_exec_command");
        assert_eq!(exec["destructiveHint"], json!(true));
        assert_eq!(exec["openWorldHint"], json!(true), "exec 能联网能起进程");
    }

    /// `cmd` 是**字符串数组**。schema 里只写 "array" 的话，模型完全可能塞一个
    /// shell 字符串进去——那正是验收项 H04 点名要防的。
    #[test]
    fn the_command_is_declared_as_an_array_of_strings() {
        let def = find("remote_exec_command")
            .expect("有这个工具")
            .definition();
        let cmd = &def["inputSchema"]["properties"]["cmd"];
        assert_eq!(cmd["type"], json!("array"), "{cmd}");
        assert_eq!(cmd["items"]["type"], json!("string"), "{cmd}");
        assert!(
            def["description"]
                .as_str()
                .unwrap_or("")
                .contains("NOT a shell line"),
            "描述里得说清楚不是 shell 行"
        );
    }

    /// 传了字符串 cmd 也原样发出去，不替远端猜成 ["sh","-c",…]。
    /// 让 ccnm 按自己的契约拒，比 gld 编一条命令出来安全得多。
    #[test]
    fn a_string_command_is_forwarded_as_it_is_not_wrapped_in_a_shell() {
        let tool = find("remote_exec_command").expect("有这个工具");
        let out = tool.forward_arguments(&json!({ "cmd": "rm -rf / && echo done" }));
        assert_eq!(out, json!({ "cmd": "rm -rf / && echo done" }), "不该被改写");
    }

    /// coding 工具必须带会话句柄；只读工具不能要它。
    #[test]
    fn only_the_coding_tools_require_a_session_handle() {
        for name in [
            "remote_apply_patch",
            "remote_exec_command",
            "remote_read_output",
        ] {
            let def = find(name).expect(name).definition();
            let req = def["inputSchema"]["required"].as_array().expect("required");
            assert!(req.contains(&json!(HANDLE_ARG)), "{name} 该要句柄：{req:?}");
            assert!(needs_coding(find(name).expect(name)), "{name}");
        }
        for name in ["remote_read_file", "remote_search_text"] {
            let def = find(name).expect(name).definition();
            let req = def["inputSchema"]["required"].as_array().expect("required");
            assert!(!req.contains(&json!(HANDLE_ARG)), "{name} 不该要句柄");
            assert!(!needs_coding(find(name).expect(name)), "{name}");
        }
    }

    /// 句柄和 workspace 一样是 gld 的东西，不能跟着转发——ccnm 不认识它。
    #[test]
    fn neither_routing_field_is_forwarded() {
        let tool = find("remote_apply_patch").expect("有这个工具");
        let out = tool.forward_arguments(&json!({
            "workspace": "prod",
            "coding_handle": "h-abc123",
            "files": [{ "op": "add", "path": "a.txt", "content": "x" }]
        }));
        assert_eq!(out["files"][0]["path"], json!("a.txt"));
        assert!(out.get(WORKSPACE_ARG).is_none(), "{out}");
        assert!(out.get(HANDLE_ARG).is_none(), "{out}");
    }

    /// 参数名以 ccnm 的 *Args 结构体为准，不以删节过的 fixture 为准。
    /// 这条钉的是 apply_patch：fixture 写的是 changes，实现收的是 files。
    #[test]
    fn the_patch_argument_is_named_the_way_ccnm_actually_reads_it() {
        let tool = find("remote_apply_patch").expect("有这个工具");
        let names: Vec<&str> = tool.arguments.iter().map(|a| a.name).collect();
        assert!(names.contains(&"files"), "{names:?}");
        assert!(!names.contains(&"changes"), "changes 是 fixture 里的错名字");
        // 没声明的键不往外发，所以照 fixture 写的调用会被拦在 gld 这边，
        // 而不是到远端才失败。
        let out = tool.forward_arguments(&json!({ "changes": [] }));
        assert_eq!(out, json!({}), "{out}");
        assert_eq!(
            tool.missing_required(&json!({ "changes": [] })),
            vec!["files"]
        );
    }

    /// read 模式的参数是照真二进制核过的，不是 fixture 那份删节版。
    #[test]
    fn the_read_whitelist_covers_what_ccnm_really_accepts() {
        let names = |tool: &str| -> Vec<&'static str> {
            find(tool)
                .expect(tool)
                .arguments
                .iter()
                .map(|a| a.name)
                .collect()
        };
        for expected in ["path", "start_line", "max_lines", "end_line", "max_bytes"] {
            assert!(
                names("remote_read_file").contains(&expected),
                "read_file 少了 {expected}"
            );
        }
        for expected in [
            "query",
            "regex",
            "path",
            "glob",
            "case_sensitive",
            "context_lines",
            "max_results",
        ] {
            assert!(
                names("remote_search_text").contains(&expected),
                "search_text 少了 {expected}"
            );
        }
        assert!(names("remote_list_files").contains(&"include_hidden"));
    }
}

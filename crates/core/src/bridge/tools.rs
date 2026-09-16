//! hub 暴露给 Web AI 的远端只读工具。
//!
//! **是一份静态名单，不是把远端的 `tools/list` 转出去。**这是跨仓计划 v2
//! 第 7 节定的：「只开放已评审工具，不自动跟随上游增加权限」。远端 ccnm
//! 哪天加了个工具，这边不声明就调不到；哪天给某个工具加了参数，这边不
//! 声明就传不过去。两者都得先有人看过再加进来。
//!
//! schema 抄自 ccnm 冻结协议的 `tools-list-read` fixture，只多一个 gld 自己
//! 的路由字段 `workspace`。
//!
//! 名字都带 `remote_` 前缀：本地成员的 `read_file` 和远端的不是一个契约
//! （分页、错误码、参数都不同），同名会让模型以为可以混着用。

use serde_json::{json, Value};

/// hub 用来路由到哪个成员的参数名。转发给 ccnm 之前会被摘掉——它是 gld
/// 的东西，ccnm 不认识。
pub const WORKSPACE_ARG: &str = "workspace";

/// 一个远端工具：hub 这边叫什么、转发到 ccnm 的哪个工具、能带哪些参数。
pub struct RemoteTool {
    /// hub 暴露的名字。
    pub name: &'static str,
    /// 转发到远端时用的名字。
    pub remote_name: &'static str,
    pub description: &'static str,
    /// 允许转发的参数名。**白名单**：不在这里的一律不往外发。
    pub arguments: &'static [Argument],
}

pub struct Argument {
    pub name: &'static str,
    /// JSON Schema 的 `type`。
    pub json_type: &'static str,
    pub required: bool,
}

const fn arg(name: &'static str, json_type: &'static str) -> Argument {
    Argument {
        name,
        json_type,
        required: false,
    }
}

const fn required(name: &'static str, json_type: &'static str) -> Argument {
    Argument {
        name,
        json_type,
        required: true,
    }
}

/// 只读模式开放的四个。跟 ccnm `--mode read` 给的四个一一对应。
///
/// 没有 `exec_command`、没有 `apply_patch`、没有 `read_output`——只读模式
/// 产不出 `output_ref`，开了也没东西可读。
pub const READ_TOOLS: &[RemoteTool] = &[
    RemoteTool {
        name: "remote_workspace_info",
        remote_name: "workspace_info",
        description: "Name, git status and platform of a remote workspace. Call it once before working in that workspace; every other remote path is relative to its root.",
        arguments: &[],
    },
    RemoteTool {
        name: "remote_read_file",
        remote_name: "read_file",
        description: "Read a text file from a remote workspace, as numbered lines. Paths are relative to that workspace's root. Long files come back in pages with the line to continue from.",
        arguments: &[required("path", "string"), arg("start_line", "integer"), arg("max_lines", "integer")],
    },
    RemoteTool {
        name: "remote_list_files",
        remote_name: "list_files",
        description: "List a directory of a remote workspace, or search it with a glob. Without a glob you get the immediate children of one directory.",
        arguments: &[arg("path", "string"), arg("glob", "string"), arg("max_entries", "integer")],
    },
    RemoteTool {
        name: "remote_search_text",
        remote_name: "search_text",
        description: "Search a remote workspace for a string, or a regex if you ask for one. The search runs where the files are, so only the matches cross the network.",
        arguments: &[required("query", "string"), arg("regex", "boolean"), arg("path", "string"), arg("max_results", "integer")],
    },
];

/// 按 hub 这边的名字找工具。找不到就是没开放——不去问远端有没有。
pub fn find(name: &str) -> Option<&'static RemoteTool> {
    READ_TOOLS.iter().find(|tool| tool.name == name)
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
        for argument in self.arguments {
            properties[argument.name] = json!({ "type": argument.json_type });
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
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        })
    }

    /// 把 hub 收到的参数挑成要发给 ccnm 的那份。
    ///
    /// **白名单**：只有这个工具声明过的参数会被带上，`workspace` 和任何
    /// 别的键都留在 gld 这边。所以模型没法借着某个远端工具往 ccnm 塞它
    /// 自己没声明的字段。
    ///
    /// 只挑不改：声明过的参数原样带过去，包括 `null`——`null` 在 ccnm 那边
    /// 是「没给」的合法写法，替它改成缺省是替远端做决定。
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

/// 全部远端工具的 `tools/list` 条目。
pub fn definitions() -> Vec<Value> {
    READ_TOOLS.iter().map(RemoteTool::definition).collect()
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
    #[test]
    fn an_argument_this_side_never_declared_is_dropped() {
        let tool = find("remote_read_file").expect("有这个工具");
        let out = tool.forward_arguments(&json!({
            "path": "a.txt",
            "max_bytes": 65536,
            "end_line": 10,
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

    /// 没开放的名字不去问远端，直接就是没有。
    #[test]
    fn an_unknown_tool_name_is_simply_not_there() {
        assert!(find("remote_exec_command").is_none());
        assert!(find("read_file").is_none(), "本地名字不该命中远端工具");
        assert!(find("remote_anything").is_none());
    }

    #[test]
    fn all_definitions_are_listed() {
        let defs = definitions();
        assert_eq!(defs.len(), READ_TOOLS.len());
        let names: Vec<_> = defs.iter().map(|d| d["name"].clone()).collect();
        assert!(names.contains(&json!("remote_workspace_info")), "{names:?}");
    }
}

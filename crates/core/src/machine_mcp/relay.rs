//! 模型看到的三个工具，和它们的结果怎么交出去。
//!
//! | 工具 | 做什么 |
//! | --- | --- |
//! | `list_mcp_tools` | 不带参数：开着的 server 和它们的状态；带 `server`：它的工具和参数表；再带 `tool`：一个工具的全部定义 |
//! | `call_mcp_tool` | 调一个工具，结果原样交回（大的分段） |
//! | `read_mcp_result` | 读一个太大、没能一次交完的结果的后面部分 |
//!
//! 为什么是"几个工具去操作它们"而不是把每个 server 的工具平铺进 gld 的工具表：
//! 平铺要在 `tools/list` 时把开着的 server 全拉起来（冷启动、起不来的拖累整张
//! 表），工具表跟着变大（本机实测 playwright 一家 25 个工具、21 KB），而
//! ChatGPT 只在连上时读一次工具表，开一个新 server 得删了连接器重建。
//!
//! ## 结果多大算大
//!
//! 实测 deepwiki 的 `read_wiki_contents` 一次回 839 KB：407 KB 正文，外加一份
//! 内容相同的 `structuredContent`（FastMCP 的 `{"result": 正文}` 包装）。所以：
//!
//! - `structuredContent` 是正文的副本时**不带**（这三个工具没有 `outputSchema`，
//!   按协议客户端本来就不看它）；不是副本、或者只有它没有文字时，转成一段文字交出去。
//!   怎么判副本见 `toexec_mcp::shape`——toexec-mcp 0.2.0 之前只要有文字就整个丢掉，
//!   上游只放在结构化结果里的字段模型就看不到（审查 D05）；
//! - 文字超过 [`INLINE_BYTES`] 就先交这么多，全文留在内存里
//!   （[`KEEP_FOR`]、单条最多 [`MAX_KEPT_BYTES`]、总共 [`KEEP_TOTAL_BYTES`]），
//!   末尾写明用 `read_mcp_result` 从哪接着读——**不静默截断**；
//! - 图片、音频原样带，单个超过 [`MAX_MEDIA_BYTES`] 的换成一句说明。

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Map, Value};
use toexec_mcp::installed::{Installed, Server, Transport as Config};
use toexec_mcp::kept::{self, Kept};
use toexec_mcp::pool::{Pool, CALL_TIMEOUT};
use toexec_mcp::shape::{self, cut_text, fit_listing, part_end};
use toexec_mcp::{Error, Open};

use crate::tools::workspace::{tool_err, tool_ok, WorkspaceError};
use crate::tools::wrap_mcp_tool_result;

pub const LIST: &str = "list_mcp_tools";
pub const CALL: &str = "call_mcp_tool";
pub const READ: &str = "read_mcp_result";

/// 一次直接交回的文字上限，和 `read_output` 的默认页一样大。
pub const INLINE_BYTES: usize = 64 * 1024;
/// 留着分段读的全文，单条最多这么大；再大的只留前面这些，并且说明。
pub const MAX_KEPT_BYTES: usize = 16 * 1024 * 1024;
/// 所有留着的全文加起来最多这么大，超了先扔最早的。
pub const KEEP_TOTAL_BYTES: usize = 64 * 1024 * 1024;
pub const KEEP_FOR: Duration = Duration::from_secs(10 * 60);
/// 单张图片 / 单段音频（base64 之后）最多这么大，和 `view_image` 的默认上限一样。
pub const MAX_MEDIA_BYTES: usize = 5 * 1024 * 1024;
/// `read_mcp_result` 的 `max_bytes`：默认、最小、最大。
const READ_BYTES: (usize, usize, usize) = (INLINE_BYTES, 1024, 256 * 1024);
/// server 的 `instructions` 最多带这么多（DeepWiki 实测 3 KB）。
const MAX_INSTRUCTIONS: usize = 4 * 1024;
/// `list_mcp_tools server=…` 一次交回的上限：超了先去掉参数表，再去掉描述。
const MAX_LISTING_BYTES: usize = 64 * 1024;
/// 多久看一次有没有闲着的 server 该收（闲多久算闲见 `pool::IDLE_AFTER`）。
const SWEEP_EVERY: Duration = Duration::from_secs(60);

pub fn is_tool(name: &str) -> bool {
    matches!(name, LIST | CALL | READ)
}

/// 这次调用能用的：开着的 server 名单、装了哪些、怎么起、谁在调。
pub struct Scope<'a> {
    /// 操作员开了的名字（`gld mcp on`）。
    pub on: &'a [String],
    pub installed: &'a Installed,
    /// 怎么起（服务传 [`super::open::Opener`]，测试传内存里的 server）。
    pub opener: &'a dyn Open,
    /// 调用方主体（`AuthContext::tag`）。连接和留着的结果都按它分。
    pub caller: &'a str,
}

impl Scope<'_> {
    /// 开着、而且确实装着的，按装的顺序。
    pub fn offered(&self) -> Vec<&Server> {
        offered(self.on, self.installed)
    }
}

pub fn offered<'a>(on: &[String], installed: &'a Installed) -> Vec<&'a Server> {
    installed
        .servers
        .iter()
        .filter(|server| on.contains(&server.name))
        .collect()
}

pub struct Relay {
    pool: Arc<Pool>,
    results: Kept,
}

impl Default for Relay {
    fn default() -> Self {
        Relay::new()
    }
}

impl Relay {
    pub fn new() -> Relay {
        let relay = Relay::with_pool(Pool::new());
        Pool::sweep(&relay.pool, SWEEP_EVERY);
        relay
    }

    pub fn with_pool(pool: Pool) -> Relay {
        Relay {
            pool: Arc::new(pool),
            results: Kept::new(kept::Limits {
                keep_for: KEEP_FOR,
                max_item: MAX_KEPT_BYTES,
                max_total: KEEP_TOTAL_BYTES,
            }),
        }
    }

    /// 三个工具的定义。`offered` 为空时调用方不该列它们。
    pub fn definitions(offered: &[&Server]) -> Vec<Value> {
        let catalog = offered
            .iter()
            .map(|server| format!("{} ({})", server.name, server.kind().as_str()))
            .collect::<Vec<_>>()
            .join(", ");
        vec![
            json!({
                "name": LIST,
                "title": "List MCP tools on this machine",
                "description": format!(
                    "MCP servers installed on the machine this service runs on, relayed through this service. Turned on here: {catalog}. Without arguments: those servers and whether each is running. With server: its tools, each with its input schema, and the server's own instructions. With server and tool: that one tool in full. Then call it with {CALL}."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "server": { "type": "string", "description": "A server name from the list above." },
                        "tool": { "type": "string", "description": "One tool of that server, to see its full definition." }
                    },
                    "additionalProperties": false
                },
                "annotations": {
                    "title": "List MCP tools on this machine",
                    "readOnlyHint": true,
                    "destructiveHint": false,
                    "idempotentHint": true,
                    "openWorldHint": true
                }
            }),
            json!({
                "name": CALL,
                "title": "Call an MCP tool on this machine",
                "description": format!(
                    "Call a tool of an MCP server installed on the machine this service runs on (see {LIST} for the servers, their tools and each tool's input schema). It runs as the account this service runs as, the way it would in a local AI client: what it can reach depends on the server, not on any workspace's settings. A result over {} KiB comes back in parts; the note at the end says how to read on with {READ}.",
                    INLINE_BYTES / 1024
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "server": { "type": "string", "minLength": 1 },
                        "tool": { "type": "string", "minLength": 1 },
                        "arguments": { "type": "object", "description": "The tool's own arguments, as its input schema says." }
                    },
                    "required": ["server", "tool"],
                    "additionalProperties": false
                },
                "annotations": {
                    "title": "Call an MCP tool on this machine",
                    "readOnlyHint": false,
                    "destructiveHint": true,
                    "idempotentHint": false,
                    "openWorldHint": true
                }
            }),
            json!({
                "name": READ,
                "title": "Read the rest of an MCP tool result",
                "description": format!(
                    "Read the next part of a {CALL} result that was too large to return at once. Pass the ref and offset from the note at the end of the previous part. Parts are kept for {} minutes.",
                    KEEP_FOR.as_secs() / 60
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "ref": { "type": "string", "minLength": 1 },
                        "offset": { "type": "integer", "minimum": 0, "default": 0 },
                        "max_bytes": {
                            "type": "integer",
                            "minimum": READ_BYTES.1,
                            "maximum": READ_BYTES.2,
                            "default": READ_BYTES.0
                        }
                    },
                    "required": ["ref"],
                    "additionalProperties": false
                },
                "annotations": {
                    "title": "Read the rest of an MCP tool result",
                    "readOnlyHint": true,
                    "destructiveHint": false,
                    "idempotentHint": true,
                    "openWorldHint": false
                }
            }),
        ]
    }

    /// 处理一次调用，返回整个 MCP `tools/call` 结果。
    pub fn call(&self, tool: &str, args: &Value, scope: &Scope) -> Value {
        self.pool.retain(scope.on);
        let outcome = match tool {
            LIST => self.list(args, scope),
            CALL => self.call_tool(args, scope),
            READ => self.read(args, scope),
            other => Err(refuse(
                "INVALID_ARGUMENT",
                format!("{other} is not one of the MCP relay tools"),
                "validation",
                json!({}),
            )),
        };
        outcome.unwrap_or_else(|error| error)
    }

    pub fn close_all(&self) {
        self.pool.close_all();
    }

    /// 只留开着的 server 的连接。服务每次请求都调，所以 `gld mcp off` 之后
    /// 不管下一次调的是什么工具，被关掉的 server 都会跟着收。
    pub fn retain(&self, on: &[String]) {
        self.pool.retain(on);
    }

    #[cfg(test)]
    pub(crate) fn has_connection(&self, server: &str, caller: &str) -> bool {
        self.pool.known_tools(server, caller).is_some()
    }

    fn list(&self, args: &Value, scope: &Scope) -> Result<Value, Value> {
        accepts(LIST, args, &["server", "tool"])?;
        let tool = text_arg(args, "tool")?;
        let Some(name) = text_arg(args, "server")? else {
            if tool.is_some() {
                return Err(refuse(
                    "INVALID_ARGUMENT",
                    "tool needs server: which server's tool?".into(),
                    "validation",
                    json!({}),
                ));
            }
            return Ok(plain(tool_ok(self.overview(scope))));
        };
        let server = find(scope, &name)?;
        let wait = server.tool_timeout.unwrap_or(CALL_TIMEOUT);
        let seen = self
            .pool
            .with(server, scope.caller, scope.opener, wait, |live| {
                Ok((
                    live.tools.clone(),
                    live.client.instructions.clone(),
                    json!({ "name": live.client.server_name, "version": live.client.server_version }),
                ))
            });
        let (tools, instructions, info) = seen.map_err(|error| failure(server, error))?;
        let tools: Vec<Value> = tools
            .into_iter()
            .filter(|t| t["name"].as_str().is_some_and(|n| server.allows_tool(n)))
            .collect();

        if let Some(tool) = tool {
            let Some(found) = tools.iter().find(|t| t["name"] == json!(tool)) else {
                return Err(unknown_tool(server, &tool, &tools));
            };
            return Ok(plain(tool_ok(json!({
                "server": server.name,
                "tool": found,
                "usage": format!("Call it with {CALL} server={} tool={tool}.", server.name)
            }))));
        }

        let (listed, omitted) = fit_listing(&tools, MAX_LISTING_BYTES);
        let mut out = json!({
            "server": server.name,
            "kind": server.kind().as_str(),
            "server_info": info,
            "count": tools.len(),
            "tools": listed,
            "usage": format!("Call a tool with {CALL}, passing its own arguments in `arguments`.")
        });
        if let Some(omitted) = omitted {
            out["omitted"] = json!(omitted);
        }
        if let Some(instructions) = instructions {
            out["instructions"] = json!(cut_text(&instructions, MAX_INSTRUCTIONS));
        }
        Ok(plain(tool_ok(out)))
    }

    /// 不带参数时：开着的 server 和它们的状态。不为了列清单去起 server。
    fn overview(&self, scope: &Scope) -> Value {
        let servers: Vec<Value> = scope
            .offered()
            .into_iter()
            .map(|server| {
                let mut entry = json!({ "name": server.name, "kind": server.kind().as_str() });
                if let Some(problem) = unusable(server) {
                    entry["state"] = json!("unusable");
                    entry["problem"] = json!(problem);
                } else if let Some(tools) = self.pool.known_tools(&server.name, scope.caller) {
                    entry["state"] = json!("running");
                    entry["tools"] = json!(tools
                        .iter()
                        .filter_map(|t| t["name"].as_str())
                        .filter(|n| server.allows_tool(n))
                        .collect::<Vec<_>>());
                } else {
                    entry["state"] = json!("not started");
                }
                entry
            })
            .collect();
        let usage = if servers.is_empty() {
            "No MCP server is turned on for this service. The person running this service turns them on with `gld mcp on <name>`.".to_string()
        } else {
            format!("Call {LIST} with server=<name> to see its tools and their input schemas (this starts it if it is not running), then {CALL}.")
        };
        json!({ "servers": servers, "count": servers.len(), "usage": usage })
    }

    fn call_tool(&self, args: &Value, scope: &Scope) -> Result<Value, Value> {
        accepts(CALL, args, &["server", "tool", "arguments"])?;
        let name = required(args, "server")?;
        let tool = required(args, "tool")?;
        let arguments = match args.get("arguments") {
            None | Some(Value::Null) => json!({}),
            Some(Value::Object(object)) => Value::Object(object.clone()),
            Some(_) => {
                return Err(refuse(
                    "INVALID_ARGUMENT",
                    "arguments must be an object: the tool's own arguments by name".into(),
                    "validation",
                    json!({ "executed": false }),
                ))
            }
        };
        let server = find(scope, &name)?;
        let timeout = server.tool_timeout.unwrap_or(CALL_TIMEOUT);
        let called = self
            .pool
            .with(server, scope.caller, scope.opener, timeout, |live| {
                let offered = live.tools.iter().any(|t| t["name"] == json!(tool));
                if !offered || !server.allows_tool(&tool) {
                    let visible: Vec<Value> = live
                        .tools
                        .iter()
                        .filter(|t| t["name"].as_str().is_some_and(|n| server.allows_tool(n)))
                        .cloned()
                        .collect();
                    return Ok(Err(unknown_tool(server, &tool, &visible)));
                }
                live.client.call_tool(&tool, arguments, timeout).map(Ok)
            });
        match called {
            Ok(Ok(result)) => Ok(self.shape(result, &server.name, &tool, scope.caller)),
            Ok(Err(refused)) => Err(refused),
            Err(error) => Err(failure(server, error)),
        }
    }

    fn read(&self, args: &Value, scope: &Scope) -> Result<Value, Value> {
        accepts(READ, args, &["ref", "offset", "max_bytes"])?;
        let reference = required(args, "ref")?;
        // 留着的全文没了，不等于那次调用没做：它已经做完、只是后半截结果取不回来。
        // 以前这里直接说"再调一次"，对会改东西的工具就是做第二遍（审查 D05）。
        let Some(text) = self.results.get(scope.caller, &reference) else {
            return Err(refuse(
                "MCP_RESULT_GONE",
                format!(
                    "No kept result {reference}: results are kept for {} minutes and only for the client that made the call. If this ref came from your own call, that call already ran; only the rest of its output cannot be read. Calling again is safe only for tools that just read; for one that changes anything, check the current state first instead of repeating it.",
                    KEEP_FOR.as_secs() / 60
                ),
                "validation",
                json!({ "ref": reference, "output_recoverable": false }),
            ));
        };
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
        let max = args
            .get("max_bytes")
            .and_then(Value::as_u64)
            .map(|n| (n as usize).clamp(READ_BYTES.1, READ_BYTES.2))
            .unwrap_or(READ_BYTES.0);
        let Some((start, end)) = kept::page(&text, offset, max) else {
            return Err(refuse(
                "INVALID_ARGUMENT",
                format!("offset {offset} is past the end ({} bytes)", text.len()),
                "validation",
                json!({ "size": text.len() }),
            ));
        };
        let note = if end < text.len() {
            format!(
                "[gld: bytes {start}-{end} of {}. Next part: {READ} ref={reference} offset={end}]",
                text.len()
            )
        } else {
            format!(
                "[gld: bytes {start}-{end} of {}; that is the end]",
                text.len()
            )
        };
        Ok(json!({
            "content": [
                { "type": "text", "text": &text[start..end] },
                { "type": "text", "text": note }
            ],
            "isError": false
        }))
    }

    /// server 的结果交出去之前的整理，见模块说明"结果多大算大"。
    fn shape(&self, result: Value, server: &str, tool: &str, caller: &str) -> Value {
        let shaped = shape::shape(
            &result,
            &shape::Limits {
                inline_bytes: INLINE_BYTES,
                max_media_bytes: MAX_MEDIA_BYTES,
            },
        );
        let content = match shaped.long {
            Some(whole) => self.split(whole, caller, shaped.items),
            None => shaped.items,
        };
        let mut meta = Map::new();
        meta.insert("gld/mcp_server".into(), json!(server));
        meta.insert("gld/mcp_tool".into(), json!(tool));
        json!({ "content": content, "isError": shaped.is_error, "_meta": meta })
    }

    /// 大文字：先交一段，全文留着，末尾写明怎么接着读。
    fn split(&self, whole: String, caller: &str, others: Vec<Value>) -> Vec<Value> {
        // 第一段远小于单条上限，先切出来，再把全文交给 `Kept`（超长的它来截）。
        let end = part_end(&whole, 0, INLINE_BYTES);
        let first = whole[..end].to_string();
        let stored = self.results.put(caller, whole);
        let mut note = format!(
            "[gld: this result is {} bytes, too long to return at once; this part is bytes 0-{end}. Next part: {READ} ref={} offset={end}. Kept for {} minutes.",
            stored.size,
            stored.reference,
            KEEP_FOR.as_secs() / 60
        );
        if stored.kept < stored.size {
            note.push_str(&format!(
                " Only the first {} bytes were kept; the rest is gone, so ask the tool for less if you need it.",
                stored.kept
            ));
        }
        note.push(']');
        let mut content = vec![
            json!({ "type": "text", "text": first }),
            json!({ "type": "text", "text": note }),
        ];
        content.extend(others);
        content
    }
}

fn find<'a>(scope: &'a Scope, name: &str) -> Result<&'a Server, Value> {
    let offered = scope.offered();
    if let Some(server) = offered.iter().find(|server| server.name == name) {
        if let Some(problem) = unusable(server) {
            return Err(refuse(
                "MCP_SERVER_UNUSABLE",
                format!("MCP server {name} cannot be used as it is set up: {problem}. Tell the user; this is fixed in its config, not by calling again."),
                "runtime",
                json!({ "server": name }),
            ));
        }
        return Ok(server);
    }
    let names: Vec<&str> = offered.iter().map(|server| server.name.as_str()).collect();
    // 只列开着的：没开的、没装的在模型这边一样不存在，不给它枚举的机会。
    let message = if names.is_empty() {
        "No MCP server is turned on for this service. The person running this service turns them on with `gld mcp on <name>`.".to_string()
    } else {
        format!("No MCP server {name} here. Turned on: {}", names.join(", "))
    };
    Err(refuse(
        "MCP_SERVER_UNKNOWN",
        message,
        "validation",
        json!({ "servers": names }),
    ))
}

/// 配置本身决定了它起不来的情况：缺环境变量、老的 SSE 传输。
fn unusable(server: &Server) -> Option<String> {
    if !server.missing_env.is_empty() {
        return Some(format!(
            "its config uses environment variable(s) {} that this service does not have",
            server.missing_env.join(", ")
        ));
    }
    if matches!(server.transport, Config::Sse { .. }) {
        return Some("it uses the old HTTP+SSE transport, which is not supported here".to_string());
    }
    None
}

fn unknown_tool(server: &Server, tool: &str, tools: &[Value]) -> Value {
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    refuse(
        "MCP_TOOL_UNKNOWN",
        format!(
            "MCP server {} has no tool {tool}. It has: {}",
            server.name,
            names.join(", ")
        ),
        "validation",
        json!({ "server": server.name, "tools": names, "executed": false }),
    )
}

/// 连接或调用没走通。每种一个码：重试有没有用、该谁去修。
fn failure(server: &Server, error: Error) -> Value {
    let name = &server.name;
    let in_call = matches!(&error, Error::Timeout { during, .. } | Error::Closed { during, .. } if during == "tools/call");
    let (code, category, retryable, advice) = match &error {
        _ if in_call => (
            "MCP_OUTCOME_UNKNOWN",
            "runtime",
            false,
            "The call may or may not have done its work before that. The connection was dropped and the next call starts the server again; check before repeating anything that changes state.",
        ),
        Error::Busy { .. } => ("MCP_SERVER_BUSY", "runtime", true, "Try again in a moment."),
        Error::Timeout { .. } => (
            "MCP_SERVER_TIMEOUT",
            "runtime",
            true,
            "It may still be starting (a first run can download packages); try once more.",
        ),
        Error::Closed { .. } | Error::Start(_) | Error::Protocol(_) => (
            "MCP_SERVER_NOT_STARTED",
            "runtime",
            false,
            "Tell the user; `gld mcp test <name>` on that machine shows why.",
        ),
        Error::NeedsLogin { .. } => (
            "MCP_SERVER_NEEDS_LOGIN",
            "permission",
            false,
            "Tell the user; this service cannot log in to it.",
        ),
        Error::Http { status, .. } => (
            "MCP_SERVER_HTTP_ERROR",
            "runtime",
            *status >= 500,
            "",
        ),
        Error::Refused { .. } => (
            "MCP_TOOL_REFUSED",
            "validation",
            false,
            "Check the arguments against the tool's input schema from list_mcp_tools.",
        ),
    };
    let mut message = format!("MCP server {name}: {error}.");
    if !advice.is_empty() {
        message.push(' ');
        message.push_str(&advice.replace("<name>", name));
    }
    plain(tool_err(WorkspaceError::ToolDetails {
        code,
        message,
        category,
        retryable,
        details: json!({ "server": name }),
    }))
}

/// gld 自己的错误，包成 MCP 结果（`isError: true`）。重试也没用的那种。
fn refuse(code: &'static str, message: String, category: &'static str, details: Value) -> Value {
    plain(tool_err(WorkspaceError::ToolDetails {
        code,
        message,
        category,
        retryable: false,
        details,
    }))
}

fn plain(structured: Value) -> Value {
    wrap_mcp_tool_result("", &json!({}), structured)
}

/// 多给的参数当场说，不按默认值悄悄跑（gld 的规矩，见 `tools::args`）。
fn accepts(tool: &str, args: &Value, known: &[&str]) -> Result<(), Value> {
    let Some(given) = args.as_object() else {
        return Ok(());
    };
    let unknown: Vec<&String> = given
        .keys()
        .filter(|name| !name.starts_with('_') && !known.contains(&name.as_str()))
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    Err(refuse(
        "INVALID_ARGUMENT",
        format!(
            "{tool} does not take {}. It takes: {}",
            unknown
                .iter()
                .map(|name| name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            known.join(", ")
        ),
        "validation",
        json!({ "unknown_arguments": unknown, "accepted_arguments": known, "executed": false }),
    ))
}

fn text_arg(args: &Value, key: &str) -> Result<Option<String>, Value> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if text.trim().is_empty() => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.trim().to_string())),
        Some(_) => Err(refuse(
            "INVALID_ARGUMENT",
            format!("{key} must be a string"),
            "validation",
            json!({ "argument": key }),
        )),
    }
}

fn required(args: &Value, key: &str) -> Result<String, Value> {
    text_arg(args, key)?.ok_or_else(|| {
        refuse(
            "MISSING_ARGUMENT",
            format!("{key} is required"),
            "validation",
            json!({ "missing": [key], "executed": false }),
        )
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use toexec_mcp::installed::Source;
    use toexec_mcp::scripted::{reply, Scripted};
    use toexec_mcp::Transport;

    /// 一个装好的 stdio server，名字随意，测试不会真起它。
    pub(crate) fn server(name: &str, command: &str) -> Server {
        Server {
            name: name.into(),
            source: Source::Claude,
            transport: Config::Stdio {
                command: command.into(),
                args: Vec::new(),
                env: BTreeMap::new(),
                cwd: None,
            },
            off_in_source: false,
            enabled_tools: None,
            disabled_tools: Vec::new(),
            startup_timeout: None,
            tool_timeout: None,
            missing_env: Vec::new(),
        }
    }

    /// 一个内存里的 server，工具 `big` 回 `size` 字节的文字，`pic` 回一张图，
    /// `structured` 同时回文字和一份一样的 structuredContent。
    struct Demo;

    impl Open for Demo {
        fn me(&self) -> (&str, &str) {
            ("gld-test", "0")
        }

        fn open(&self, _server: &Server) -> Result<Box<dyn Transport>, Error> {
            Ok(Box::new(Scripted::new(|request| {
                match request["method"].as_str().unwrap_or("") {
                    "initialize" => vec![reply(
                        request,
                        json!({ "protocolVersion": "2025-06-18", "serverInfo": { "name": "demo" }, "instructions": "Prefer small queries." }),
                    )],
                    "tools/list" => vec![reply(
                        request,
                        json!({ "tools": [
                            { "name": "big", "description": "Lots of text", "inputSchema": { "type": "object", "properties": { "size": { "type": "integer" } } } },
                            { "name": "pic", "inputSchema": { "type": "object" } },
                            { "name": "structured", "inputSchema": { "type": "object" } },
                            { "name": "fails", "inputSchema": { "type": "object" } },
                            { "name": "hidden", "inputSchema": { "type": "object" } }
                        ] }),
                    )],
                    "tools/call" => {
                        let args = &request["params"]["arguments"];
                        let result = match request["params"]["name"].as_str().unwrap_or("") {
                            "big" => {
                                let size = args["size"].as_u64().unwrap_or(10) as usize;
                                let line = "0123456789abcdef\n".repeat(size / 17 + 1);
                                json!({ "content": [{ "type": "text", "text": &line[..size] }] })
                            }
                            "pic" => json!({ "content": [
                                { "type": "image", "data": "a".repeat(args["bytes"].as_u64().unwrap_or(8) as usize), "mimeType": "image/png" }
                            ] }),
                            // kind 不给：文字和结构化是同一份。其余几种见
                            // `structured_results_reach_the_model_unless_they_repeat_the_text`。
                            "structured" => match args["kind"].as_str().unwrap_or("") {
                                "wrapped" => json!({
                                    "content": [{ "type": "text", "text": "# Page" }],
                                    "structuredContent": { "result": "# Page" }
                                }),
                                "richer" => json!({
                                    "content": [{ "type": "text", "text": "2 hits" }],
                                    "structuredContent": { "hits": ["a.rs", "b.rs"], "total": 2 }
                                }),
                                "mixed" => json!({
                                    "content": [
                                        { "type": "text", "text": "see" },
                                        { "type": "image", "data": "abc", "mimeType": "image/png" },
                                        { "type": "resource_link", "uri": "file:///a.txt", "name": "a.txt" }
                                    ],
                                    "structuredContent": { "path": "/a.txt" }
                                }),
                                "huge" => json!({
                                    "content": [{ "type": "text", "text": "summary" }],
                                    "structuredContent": { "rows": "r".repeat(100_000), "total": 1 }
                                }),
                                _ => json!({
                                    "content": [{ "type": "text", "text": "{\"n\":1}" }],
                                    "structuredContent": { "n": 1 }
                                }),
                            },
                            _ => {
                                json!({ "content": [{ "type": "text", "text": "nope" }], "isError": true })
                            }
                        };
                        vec![reply(request, result)]
                    }
                    _ => Vec::new(),
                }
            })))
        }
    }

    fn installed() -> Installed {
        let mut demo = server("demo", "demo-server");
        demo.disabled_tools = vec!["hidden".into()];
        let mut needs_key = server("keyed", "x");
        needs_key.missing_env = vec!["API_KEY".into()];
        let off = server("off", "x");
        let old = Server {
            transport: Config::Sse {
                url: "http://127.0.0.1:9/sse".into(),
                headers: BTreeMap::new(),
            },
            source: Source::Codex,
            ..server("old", "x")
        };
        Installed {
            servers: vec![demo, needs_key, off, old],
            ..Installed::default()
        }
    }

    fn on() -> Vec<String> {
        vec!["demo".into(), "keyed".into(), "old".into()]
    }

    fn call(relay: &Relay, tool: &str, args: Value) -> Value {
        call_as(relay, "alice", tool, args)
    }

    fn call_as(relay: &Relay, caller: &str, tool: &str, args: Value) -> Value {
        let installed = installed();
        let on = on();
        relay.call(
            tool,
            &args,
            &Scope {
                on: &on,
                installed: &installed,
                opener: &Demo,
                caller,
            },
        )
    }

    fn relay() -> Relay {
        Relay::with_pool(Pool::new())
    }

    fn code(result: &Value) -> &str {
        result["structuredContent"]["error"]["code"]
            .as_str()
            .unwrap_or("")
    }

    #[test]
    fn the_overview_names_only_what_is_on_and_starts_nothing() {
        let relay = relay();
        let result = call(&relay, LIST, json!({}));
        let servers = &result["structuredContent"]["servers"];
        let names: Vec<&str> = servers
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["demo", "keyed", "old"], "off 没开，不能出现");
        assert_eq!(servers[0]["state"], json!("not started"));
        assert_eq!(servers[1]["state"], json!("unusable"));
        assert!(servers[1]["problem"].as_str().unwrap().contains("API_KEY"));

        call(&relay, LIST, json!({ "server": "demo" }));
        let result = call(&relay, LIST, json!({}));
        let demo = &result["structuredContent"]["servers"][0];
        assert_eq!(demo["state"], json!("running"));
        assert_eq!(
            demo["tools"],
            json!(["big", "pic", "structured", "fails"]),
            "hidden 被来源配置关了"
        );
    }

    #[test]
    fn a_server_listing_carries_schemas_and_instructions() {
        let result = call(&relay(), LIST, json!({ "server": "demo" }));
        let listed = &result["structuredContent"];
        assert_eq!(listed["count"], json!(4));
        assert_eq!(listed["instructions"], json!("Prefer small queries."));
        assert_eq!(listed["tools"][0]["inputSchema"]["type"], json!("object"));

        let one = call(&relay(), LIST, json!({ "server": "demo", "tool": "big" }));
        assert_eq!(
            one["structuredContent"]["tool"]["description"],
            json!("Lots of text")
        );
    }

    #[test]
    fn servers_that_are_off_unusable_or_unknown_are_refused_without_starting() {
        let relay = relay();
        assert_eq!(
            code(&call(&relay, CALL, json!({ "server": "off", "tool": "x" }))),
            "MCP_SERVER_UNKNOWN"
        );
        let unknown = call(&relay, CALL, json!({ "server": "nope", "tool": "x" }));
        let message = unknown["structuredContent"]["error"]["message"]
            .as_str()
            .unwrap();
        assert!(
            message.contains("demo, keyed, old") && !message.contains("off"),
            "{message}"
        );
        assert_eq!(
            code(&call(
                &relay,
                CALL,
                json!({ "server": "keyed", "tool": "x" })
            )),
            "MCP_SERVER_UNUSABLE"
        );
        assert_eq!(
            code(&call(&relay, CALL, json!({ "server": "old", "tool": "x" }))),
            "MCP_SERVER_UNUSABLE"
        );
        assert_eq!(
            code(&call(
                &relay,
                CALL,
                json!({ "server": "demo", "tool": "hidden" })
            )),
            "MCP_TOOL_UNKNOWN"
        );
        assert_eq!(
            code(&call(
                &relay,
                CALL,
                json!({ "server": "demo", "tool": "big", "args": {} })
            )),
            "INVALID_ARGUMENT",
            "写错的参数名当场拒"
        );
        assert_eq!(
            code(&call(
                &relay,
                CALL,
                json!({ "server": "demo", "tool": "big", "arguments": [1] })
            )),
            "INVALID_ARGUMENT"
        );
    }

    #[test]
    fn a_small_result_comes_back_as_the_server_sent_it() {
        let result = call(
            &relay(),
            CALL,
            json!({ "server": "demo", "tool": "big", "arguments": { "size": 40 } }),
        );
        assert_eq!(result["isError"], json!(false));
        assert_eq!(result["content"].as_array().unwrap().len(), 1);
        assert_eq!(result["content"][0]["text"].as_str().unwrap().len(), 40);
        assert_eq!(result["_meta"]["gld/mcp_server"], json!("demo"));
    }

    #[test]
    fn a_duplicate_structured_copy_is_not_sent_twice() {
        let result = call(
            &relay(),
            CALL,
            json!({ "server": "demo", "tool": "structured" }),
        );
        assert!(result.get("structuredContent").is_none(), "{result}");
        assert_eq!(result["content"][0]["text"], json!("{\"n\":1}"));
    }

    /// 结构化结果只有是正文的副本时才省掉；多出来的信息、图、资源链接一样都不能丢，
    /// 太长的照样分段、读得回来（审查 D05：以前只要有文字就整个丢掉结构化结果）。
    #[test]
    fn structured_results_reach_the_model_unless_they_repeat_the_text() {
        let relay = relay();
        let run = |kind: &str| {
            call(
                &relay,
                CALL,
                json!({ "server": "demo", "tool": "structured", "arguments": { "kind": kind } }),
            )
        };
        let texts = |result: &Value| -> Vec<String> {
            result["content"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|item| item["text"].as_str().map(str::to_string))
                .collect()
        };

        // deepwiki 那种 FastMCP 包装：同一份正文，不发两遍。
        assert_eq!(texts(&run("wrapped")), ["# Page"]);

        // 文字是摘要、结构化是数据：数据得到得了模型那里。
        let richer = texts(&run("richer"));
        assert_eq!(richer.len(), 2, "{richer:?}");
        let data: Value = serde_json::from_str(&richer[1]).expect("json");
        assert_eq!(data["hits"], json!(["a.rs", "b.rs"]));

        let mixed = run("mixed");
        let kinds: Vec<&str> = mixed["content"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["type"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, ["text", "image", "resource_link", "text"], "{mixed}");

        let huge = run("huge");
        let note = huge["content"][1]["text"].as_str().unwrap();
        let reference = note
            .split("ref=")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .unwrap()
            .to_string();
        let mut whole = huge["content"][0]["text"].as_str().unwrap().to_string();
        let mut offset = whole.len();
        loop {
            let next = call(
                &relay,
                READ,
                json!({ "ref": reference, "offset": offset, "max_bytes": 256 * 1024 }),
            );
            let text = next["content"][0]["text"].as_str().unwrap();
            whole.push_str(text);
            offset += text.len();
            if next["content"][1]["text"]
                .as_str()
                .unwrap()
                .contains("that is the end")
            {
                break;
            }
        }
        let (summary, structured) = whole.split_once('\n').expect("summary then data");
        assert_eq!(summary, "summary");
        let data: Value = serde_json::from_str(structured).expect("json");
        assert_eq!(data["rows"].as_str().map(str::len), Some(100_000));
    }

    #[test]
    fn a_large_result_comes_in_parts_and_nothing_is_lost() {
        let relay = relay();
        let size = 200_000;
        let first = call(
            &relay,
            CALL,
            json!({ "server": "demo", "tool": "big", "arguments": { "size": size } }),
        );
        let part = first["content"][0]["text"].as_str().unwrap().to_string();
        assert!(
            part.len() <= INLINE_BYTES && part.ends_with('\n'),
            "在换行处断"
        );
        let note = first["content"][1]["text"].as_str().unwrap();
        let reference = note
            .split("ref=")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .unwrap()
            .to_string();
        let mut whole = part.clone();
        let mut offset = part.len();
        loop {
            let next = call(
                &relay,
                READ,
                json!({ "ref": reference, "offset": offset, "max_bytes": 100000 }),
            );
            let text = next["content"][0]["text"].as_str().unwrap();
            whole.push_str(text);
            offset += text.len();
            if next["content"][1]["text"]
                .as_str()
                .unwrap()
                .contains("that is the end")
            {
                break;
            }
        }
        assert_eq!(whole.len(), size, "分段读完和原文一样长");
        assert_eq!(whole, "0123456789abcdef\n".repeat(size / 17 + 1)[..size]);

        let stranger = call_as(&relay, "bob", READ, json!({ "ref": reference }));
        assert_eq!(code(&stranger), "MCP_RESULT_GONE", "别人的结果读不到");
        // 读不到了不等于没做过：不能一句"再调一次"，要先核对（审查 D05）。
        let error = &stranger["structuredContent"]["error"];
        assert_eq!(error["details"]["output_recoverable"], false);
        let message = error["message"].as_str().unwrap_or_default();
        assert!(message.contains("check the current state"), "{message}");
    }

    #[test]
    fn an_oversized_image_is_replaced_by_a_note() {
        let relay = relay();
        let small = call(
            &relay,
            CALL,
            json!({ "server": "demo", "tool": "pic", "arguments": { "bytes": 100 } }),
        );
        assert_eq!(small["content"][0]["type"], json!("image"));
        let big = call(
            &relay,
            CALL,
            json!({ "server": "demo", "tool": "pic", "arguments": { "bytes": MAX_MEDIA_BYTES + 1 } }),
        );
        assert_eq!(big["content"][0]["type"], json!("text"));
        assert!(big["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("left out"));
    }

    /// 工具自己说"没办成"是它的结果，原样交回，不改写成 gld 的错误。
    #[test]
    fn the_tool_error_flag_is_passed_through() {
        let result = call(&relay(), CALL, json!({ "server": "demo", "tool": "fails" }));
        assert_eq!(result["isError"], json!(true));
        assert_eq!(result["content"][0]["text"], json!("nope"));
        assert!(
            result.get("structuredContent").is_none(),
            "不是 gld 的错误信封"
        );
    }
}

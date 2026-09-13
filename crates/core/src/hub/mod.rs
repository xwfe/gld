//! 聚合入口（hub）：一条 MCP 连接访问多个工作区。
//!
//! 它是什么、怎么用、代价是什么见 docs/concepts.md 的「聚合入口」一节；
//! 这里只记实现上必须守住的规矩。
//!
//! 隔离靠下面三条，改代码时哪条都别松：
//!
//! 1. **服务端不记"当前工作区"。** 除 `list_workspaces` 外，每次 `tools/call` 都必须带
//!    `workspace`。hub 被所有对话、所有客户端共用，服务端一旦记住"刚切到了 api"，
//!    另一个对话里没带参数的调用就会落到 api 上——单工作区里 `set_default_cwd`
//!    已经这么坑过人。所以 hub 连 `set_default_cwd` / `get_default_cwd` 都不暴露。
//! 2. **每个成员一份独立的 [`ToolContext`]**，和成员自己的监听器走同一个
//!    [`build_tool_context`]：根目录边界、命令白名单、读限制、Planning 闸门、
//!    Durable Task 基线、exec 会话表都跟着成员走。A 里起的 session 拿到 B 去读，
//!    得到的是 SESSION_NOT_FOUND，而不是 A 的输出。
//! 3. **只收紧、不放宽。** 一个工具要同时在 hub 的工具集和成员自己的工具集里
//!    才能调。成员是 read-only，经 hub 照样写不了。
//!
//! 另外两条是"别漏出去"：不在 hub 里的工作区和不存在的工作区报同一个错，
//! 不给枚举机会；成员的说明文件 / Skill / 历史摘要不在 initialize 里混着注入，
//! 由 `workspace_context` 按工作区单独取，免得 A 的 AGENTS.md 被拿去指导 B。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::agent_context::render_skill_catalog;
use crate::data::DataStore;
use crate::planning::PlanningService;
use crate::settings::{AppSettings, HubConfig};
use crate::tools::registry::{
    canonical_tool_name, exposed_tool_names, list_tools_for_profile, normalize_tool_profile,
};
use crate::tools::workspace::{tool_err, tool_ok, WorkspaceError};
use crate::tools::{build_tool_context, call_tool, wrap_mcp_tool_result, SharedToolContext};
use crate::usage::ServiceUsage;
use crate::workspace::{AuthConfig, WorkspaceProfile};

/// 日志目录、OAuth 客户端注册表、密钥共用的作用域名。
///
/// 工作区 id 是 32 位十六进制，`hub` 不可能和它撞上。
pub const HUB_SCOPE: &str = "hub";

/// `serverInfo.name`。不用任何成员的名字：它代表的是一组工作区。
pub const SERVER_NAME: &str = "gld-hub";

/// hub 自己提供的两个工具。列成员不需要 `workspace`，取说明需要。
pub const LIST_WORKSPACES: &str = "list_workspaces";
pub const WORKSPACE_CONTEXT: &str = "workspace_context";

/// 经 hub 不暴露的工具：它们改的是所有对话共享的服务端状态，见模块说明第 1 条。
const HIDDEN_TOOLS: &[&str] = &["set_default_cwd", "get_default_cwd"];

const INSTRUCTIONS: &str = "This server is a gld hub: one connection to several local workspaces. Every tool call except list_workspaces must include `workspace` (an id or name returned by list_workspaces). The server keeps no current workspace, so pass it again on every call and never rely on the workspace of an earlier call. Workspaces are isolated from each other: paths are relative to the selected workspace root, command sessions and output_ref values only exist in the workspace that created them, and planning mode, permissions and tool sets are enforced per workspace. Before working in a workspace for the first time in a conversation, call workspace_context for it and follow only that workspace's instructions; never apply one workspace's instructions or skills to another. Planning mode is controlled exclusively by the gld CLI, and only the human operator can accept Goal or Plan reviews. If an operation returns DANGEROUS_OPERATION_REQUIRES_CONFIRMATION, retry the same tool with confirm=true only when the user's request already clearly authorizes it; otherwise ask the user.";

/// 成员名单从哪儿来。
enum Members {
    /// 每次请求从数据文件读：增删成员、成员改配置立刻生效，不用重启 hub。
    DataFile,
    /// 固定的一份，给单元测试用；套 Mutex 是为了测"改完配置下一次调用就生效"。
    #[cfg(test)]
    Fixed(Box<Mutex<(Vec<WorkspaceProfile>, AppSettings)>>),
}

struct CachedContext {
    fingerprint: String,
    context: SharedToolContext,
}

/// 一次请求实际落到了哪个成员，监听器拿它往那个工作区的日志里记一笔。
pub struct Routed {
    pub workspace_id: String,
    pub context: SharedToolContext,
}

pub struct Hub {
    members: Members,
    tool_profile: String,
    /// 只用来让 `server_info` 报出真实的认证方式（hub 的，不是成员自己监听器的）。
    auth: AuthConfig,
    usage: Arc<ServiceUsage>,
    contexts: Mutex<HashMap<String, CachedContext>>,
}

impl Hub {
    pub fn new(config: &HubConfig) -> Self {
        Self::with_members(config, Members::DataFile)
    }

    #[cfg(test)]
    fn fixed(config: &HubConfig, profiles: Vec<WorkspaceProfile>, settings: AppSettings) -> Self {
        Self::with_members(
            config,
            Members::Fixed(Box::new(Mutex::new((profiles, settings)))),
        )
    }

    fn with_members(config: &HubConfig, members: Members) -> Self {
        Self {
            members,
            tool_profile: normalize_tool_profile(&config.tool_profile).into(),
            auth: AuthConfig {
                auth_type: config.auth_type.clone(),
                ..AuthConfig::default()
            },
            usage: Arc::new(ServiceUsage::default()),
            contexts: Mutex::new(HashMap::new()),
        }
    }

    pub fn usage(&self) -> Arc<ServiceUsage> {
        self.usage.clone()
    }

    /// 结束所有成员上下文里还在跑的命令。hub 停掉之后没人能再读它们的输出。
    ///
    /// 会阻塞等进程退出，必须在 `spawn_blocking` 里调。
    pub fn shutdown(&self) {
        let contexts: Vec<CachedContext> = self
            .contexts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain()
            .map(|(_, cached)| cached)
            .collect();
        for cached in contexts {
            cached.context.sessions.terminate_all();
        }
    }

    /// 处理一条 JSON-RPC 请求。会同步跑工具，必须在 `spawn_blocking` 里调。
    pub fn handle_request(&self, body: &Value) -> (Value, Option<Routed>) {
        let method = body.get("method").and_then(Value::as_str).unwrap_or("");
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        let params = body.get("params").cloned().unwrap_or(Value::Null);

        if id.is_null() && method.starts_with("notifications/") {
            return (Value::Null, None);
        }

        let mut routed = None;
        let result = match method {
            "initialize" => Ok(self.initialize_result()),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": self.list_tools() })),
            "tools/call" => self.call(&params, &mut routed),
            _ => Err(json!({
                "code": -32601,
                "message": format!("Method not found: {method}")
            })),
        };
        let response = match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
        };
        (response, routed)
    }

    fn initialize_result(&self) -> Value {
        let catalog = match self.snapshot() {
            Ok((members, _)) if members.is_empty() => {
                "No workspaces are in this hub yet; the operator adds them from the gld CLI."
                    .to_string()
            }
            Ok((members, _)) => {
                let lines = members
                    .iter()
                    .map(|member| format!("- {} (id {})", member.name, member.id))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!(
                    "Workspaces in this hub when the connection was made (call list_workspaces for the current list):\n{lines}"
                )
            }
            Err(error) => format!("The workspace list is unavailable right now: {error}"),
        };
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {
                "tools": { "listChanged": false },
                "logging": {}
            },
            "serverInfo": {
                "name": SERVER_NAME,
                "title": "hub · gld",
                "version": env!("CARGO_PKG_VERSION")
            },
            "instructions": format!("{INSTRUCTIONS}\n\n{catalog}")
        })
    }

    pub fn list_tools(&self) -> Vec<Value> {
        let mut tools = vec![list_workspaces_definition(), workspace_context_definition()];
        tools.extend(
            list_tools_for_profile(&self.tool_profile)
                .into_iter()
                .filter(|tool| {
                    tool.get("name")
                        .and_then(Value::as_str)
                        .is_some_and(|name| !HIDDEN_TOOLS.contains(&name))
                })
                .map(|mut tool| {
                    require_workspace(&mut tool["inputSchema"]);
                    tool
                }),
        );
        tools
    }

    fn exposes(&self, name: &str) -> bool {
        name == LIST_WORKSPACES
            || name == WORKSPACE_CONTEXT
            || (!HIDDEN_TOOLS.contains(&name)
                && exposed_tool_names(&self.tool_profile).contains(&name))
    }

    fn call(&self, params: &Value, routed: &mut Option<Routed>) -> Result<Value, Value> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| json!({ "code": -32602, "message": "Missing tool name" }))?;
        let canonical = canonical_tool_name(name);
        if !self.exposes(canonical) {
            return Err(json!({
                "code": -32602,
                "message": format!("Unknown tool: {name}"),
                "data": { "reason": "unknown_tool" }
            }));
        }

        let (members, settings) = match self.snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return Ok(plain_result(tool_err(WorkspaceError::Tool {
                    code: "HUB_UNAVAILABLE",
                    message: format!("The hub cannot read its workspace list: {error}"),
                    category: "storage",
                    retryable: true,
                })))
            }
        };
        self.forget_departed(&members);
        if canonical == LIST_WORKSPACES {
            return Ok(plain_result(list_workspaces(&members)));
        }

        let mut args = crate::mcp::tool_arguments(name, params);
        // 必须从参数里拿掉：工具的 schema 大多是 additionalProperties=false，
        // 带着它进 call_tool 的话，校验更严的工具会把整次调用拒掉。
        let selector = args
            .as_object_mut()
            .and_then(|object| object.remove("workspace"));
        let Some(selector) = selector
            .as_ref()
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(plain_result(workspace_required(canonical, &members)));
        };
        let member = match resolve_member(&members, selector) {
            Ok(member) => member,
            Err(error) => return Ok(plain_result(error)),
        };
        let context = match self.context_for(member, &settings) {
            Ok(context) => context,
            Err(message) => {
                return Ok(plain_result(tool_err(WorkspaceError::ToolDetails {
                    code: "WORKSPACE_UNAVAILABLE",
                    message: format!("Workspace {} cannot be opened: {message}", member.name),
                    category: "runtime",
                    retryable: false,
                    details: json!({ "workspace": member_ref(member) }),
                })))
            }
        };
        *routed = Some(Routed {
            workspace_id: member.id.clone(),
            context: context.clone(),
        });

        let structured = if canonical == WORKSPACE_CONTEXT {
            self.workspace_context(member, &context)
        } else if !exposed_tool_names(&context.tool_profile).contains(&canonical) {
            tool_not_in_workspace(canonical, member, &context.tool_profile)
        } else {
            call_tool(&context, canonical, &args)
        };
        let result = wrap_mcp_tool_result(canonical, &args, tag_workspace(structured, member));
        context.record_context_block("tool_return", &result);
        Ok(result)
    }

    /// 当前成员（按加入顺序，跳过已经不存在的 id）和这一刻的全局设置。
    fn snapshot(&self) -> Result<(Vec<WorkspaceProfile>, AppSettings), String> {
        let (profiles, settings) = match &self.members {
            Members::DataFile => DataStore::read_file(|data| {
                Ok((data.profiles.clone(), AppSettings::from_data(data)))
            })
            .map_err(|error| error.to_string())?,
            #[cfg(test)]
            Members::Fixed(fixed) => fixed.lock().expect("fixed members").clone(),
        };
        let members = settings
            .hub
            .members
            .iter()
            .filter_map(|id| profiles.iter().find(|profile| &profile.id == id).cloned())
            .collect();
        Ok((members, settings))
    }

    /// 拿成员的工具上下文；配置变了就按新配置重建。
    ///
    /// 不重建的后果是：`gld ws set allowed-commands=...` 收紧了白名单，
    /// 成员自己的监听器重启后已经生效，经 hub 却还能跑旧白名单里的命令。
    fn context_for(
        &self,
        member: &WorkspaceProfile,
        settings: &AppSettings,
    ) -> Result<SharedToolContext, String> {
        let fingerprint = context_fingerprint(member, settings);
        let mut contexts = self
            .contexts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(cached) = contexts.get(&member.id) {
            if cached.fingerprint == fingerprint {
                return Ok(cached.context.clone());
            }
        }
        let context = Arc::new(build_tool_context(
            PathBuf::from(&member.path),
            &member.name,
            self.auth.clone(),
            &member.runtime,
            settings,
            self.usage.clone(),
        )?);
        let stale = contexts.insert(
            member.id.clone(),
            CachedContext {
                fingerprint,
                context: context.clone(),
            },
        );
        drop(contexts);
        // 旧上下文里起的命令不会再有人来读（session 表跟着旧上下文一起没了），
        // 不收掉就是孤儿进程。
        if let Some(stale) = stale {
            stale.context.sessions.terminate_all();
        }
        Ok(context)
    }

    /// 被移出 hub 或已经 destroy 的成员：丢掉上下文，收掉它的命令。
    fn forget_departed(&self, members: &[WorkspaceProfile]) {
        let departed: Vec<CachedContext> = {
            let mut contexts = self
                .contexts
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let gone: Vec<String> = contexts
                .keys()
                .filter(|id| !members.iter().any(|member| &member.id == *id))
                .cloned()
                .collect();
            gone.iter().filter_map(|id| contexts.remove(id)).collect()
        };
        for cached in departed {
            cached.context.sessions.terminate_all();
        }
    }

    fn workspace_context(&self, member: &WorkspaceProfile, context: &SharedToolContext) -> Value {
        let member_tools = exposed_tool_names(&context.tool_profile);
        let tools: Vec<&str> = exposed_tool_names(&self.tool_profile)
            .into_iter()
            .filter(|name| !HIDDEN_TOOLS.contains(name) && member_tools.contains(name))
            .collect();
        // compact 下不带 Skill 目录，和单工作区 initialize 的口径一致。
        let skills = if context.tool_profile == "compact" {
            String::new()
        } else {
            render_skill_catalog(&context.current_skills())
        };
        tool_ok(json!({
            "name": member.name,
            "path": member.path,
            "tool_profile": context.tool_profile,
            "tools": tools,
            "planning_mode": PlanningService::new(context.workspace.root())
                .state()
                .ok()
                .map(|state| state.mode),
            "instructions": context.current_ai_instructions(),
            "skills": skills,
            "history_context": crate::tools::history::context_snapshot(context).ok().flatten(),
            "scope": "These instructions and skills apply to this workspace only."
        }))
    }
}

/// 决定成员上下文长什么样的全部输入。
///
/// 和 [`build_tool_context`] 读的字段一一对应：那边多读一个字段而这里没加，
/// 改了那个配置 hub 就会一直用旧的。不直接拿整份设置比，是因为里面有密钥、
/// hub 成员列表这些常变但无关的东西，每次变都重建会把正在跑的命令一起杀掉。
fn context_fingerprint(member: &WorkspaceProfile, settings: &AppSettings) -> String {
    serde_json::to_string(&(
        &member.path,
        &member.name,
        &member.runtime,
        &settings.global_executable_paths,
        &settings.global_ai_instructions,
        &settings.global_instruction_sources,
        &settings.global_skill_sources,
        &settings.global_custom_instruction_paths,
        &settings.global_custom_skill_paths,
    ))
    .unwrap_or_default()
}

/// 按 id、名称、不分大小写的名称、id 前缀（≥4 位）的顺序找成员。
///
/// 不复用命令行的 `-w` 解析：那边还认路径、报错教人跑 `gld workspace list`，
/// 而这里的读者是模型，候选列表只能来自 hub 成员——漏出其他工作区的名字就是泄露。
fn resolve_member<'a>(
    members: &'a [WorkspaceProfile],
    selector: &str,
) -> Result<&'a WorkspaceProfile, Value> {
    if let Some(found) = members.iter().find(|member| member.id == selector) {
        return Ok(found);
    }
    let rules: [&dyn Fn(&WorkspaceProfile) -> bool; 3] = [
        &|member| member.name == selector,
        &|member| member.name.eq_ignore_ascii_case(selector),
        &|member| selector.len() >= 4 && member.id.starts_with(selector),
    ];
    for rule in rules {
        let matched: Vec<&WorkspaceProfile> =
            members.iter().filter(|member| rule(member)).collect();
        match matched.as_slice() {
            [] => continue,
            [only] => return Ok(only),
            several => {
                return Err(tool_err(WorkspaceError::ToolDetails {
                    code: "WORKSPACE_AMBIGUOUS",
                    message: format!(
                        "\"{selector}\" matches several workspaces in this hub; pass the full id instead: {}",
                        describe(several.iter().copied())
                    ),
                    category: "validation",
                    retryable: false,
                    details: json!({ "candidates": several.iter().map(|member| member_ref(member)).collect::<Vec<_>>() }),
                }))
            }
        }
    }
    Err(tool_err(WorkspaceError::ToolDetails {
        code: "WORKSPACE_NOT_IN_HUB",
        message: format!(
            "No workspace \"{selector}\" in this hub. Available: {}",
            describe(members.iter())
        ),
        category: "validation",
        retryable: false,
        details: json!({ "available": members.iter().map(member_ref).collect::<Vec<_>>() }),
    }))
}

fn workspace_required(tool: &str, members: &[WorkspaceProfile]) -> Value {
    tool_err(WorkspaceError::ToolDetails {
        code: "WORKSPACE_REQUIRED",
        message: format!(
            "{tool} needs a `workspace` argument: this hub serves several workspaces and keeps no current one. Available: {}",
            describe(members.iter())
        ),
        category: "validation",
        retryable: false,
        details: json!({ "available": members.iter().map(member_ref).collect::<Vec<_>>() }),
    })
}

fn tool_not_in_workspace(tool: &str, member: &WorkspaceProfile, profile: &str) -> Value {
    tool_err(WorkspaceError::ToolDetails {
        code: "TOOL_NOT_ALLOWED_IN_WORKSPACE",
        message: format!(
            "{tool} is not available in workspace {} (its tool profile is {profile}). The hub never widens a workspace's own tool set.",
            member.name
        ),
        category: "permission",
        retryable: false,
        details: json!({ "workspace": member_ref(member), "tool_profile": profile }),
    })
}

fn list_workspaces(members: &[WorkspaceProfile]) -> Value {
    tool_ok(json!({
        "workspaces": members
            .iter()
            .map(|member| json!({
                "id": member.id,
                "name": member.name,
                "path": member.path,
                "tool_profile": normalize_tool_profile(&member.runtime.tool_profile),
            }))
            .collect::<Vec<_>>(),
        "count": members.len(),
        "usage": "Pass one of these ids or names as `workspace` on every other tool call."
    }))
}

fn member_ref(member: &WorkspaceProfile) -> Value {
    json!({ "id": member.id, "name": member.name })
}

fn describe<'a>(members: impl Iterator<Item = &'a WorkspaceProfile>) -> String {
    let listed = members
        .map(|member| format!("{} (id {})", member.name, member.id))
        .collect::<Vec<_>>();
    if listed.is_empty() {
        "none".into()
    } else {
        listed.join(", ")
    }
}

/// 在结果里注明是哪个工作区答的。
///
/// 单独用 `hub_workspace` 这个键：`server_info`、`check_exec_environment` 的结果里
/// 已经有一个表示根目录路径的 `workspace`，覆盖掉就丢了信息。
fn tag_workspace(mut structured: Value, member: &WorkspaceProfile) -> Value {
    if let Some(object) = structured.as_object_mut() {
        object.insert("hub_workspace".into(), member_ref(member));
    }
    structured
}

fn plain_result(structured: Value) -> Value {
    wrap_mcp_tool_result("", &json!({}), structured)
}

fn require_workspace(schema: &mut Value) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    let properties = object.entry("properties").or_insert_with(|| json!({}));
    if let Some(properties) = properties.as_object_mut() {
        properties.insert("workspace".into(), workspace_property());
    }
    let required = object.entry("required").or_insert_with(|| json!([]));
    if let Some(required) = required.as_array_mut() {
        required.insert(0, json!("workspace"));
    }
}

fn workspace_property() -> Value {
    json!({
        "type": "string",
        "minLength": 1,
        "description": "Target workspace id or name from list_workspaces. Required on every call; the hub keeps no current workspace."
    })
}

fn list_workspaces_definition() -> Value {
    json!({
        "name": LIST_WORKSPACES,
        "title": "List workspaces",
        "description": "List the workspaces reachable through this hub. Pass one of their ids or names as `workspace` on every other tool call.",
        "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
        "annotations": {
            "title": "List workspaces",
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false
        }
    })
}

fn workspace_context_definition() -> Value {
    let mut schema = json!({ "type": "object", "properties": {}, "additionalProperties": false });
    require_workspace(&mut schema);
    json!({
        "name": WORKSPACE_CONTEXT,
        "title": "Workspace context",
        "description": "Load one workspace's agent instructions, skills, selected history context, planning mode and available tools. Call it before working in a workspace for the first time; its instructions apply to that workspace only.",
        "inputSchema": schema,
        "annotations": {
            "title": "Workspace context",
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planning::PlanningMode;

    struct Fixture {
        _dirs: Vec<tempfile::TempDir>,
        hub: Hub,
        api: WorkspaceProfile,
        web: WorkspaceProfile,
        outsider: WorkspaceProfile,
    }

    fn workspace(name: &str, files: &[(&str, &str)]) -> (tempfile::TempDir, WorkspaceProfile) {
        let dir = tempfile::tempdir().expect("workspace dir");
        for (path, content) in files {
            std::fs::write(dir.path().join(path), content).expect("fixture file");
        }
        let root = dir.path().canonicalize().expect("canonical root");
        let profile = WorkspaceProfile::new(root.display().to_string(), Some(name.into()));
        (dir, profile)
    }

    /// 两个成员 api / web，外加一个登记了但没加进 hub 的 outsider。
    fn fixture() -> Fixture {
        crate::home::isolate_for_tests();
        let (api_dir, api) = workspace("api", &[("only-api.txt", "api secret\n")]);
        let (web_dir, web) = workspace("web", &[("only-web.txt", "web page\n")]);
        let (outsider_dir, outsider) = workspace("outsider", &[]);
        let mut settings = AppSettings::default();
        settings.hub.members = vec![api.id.clone(), web.id.clone()];
        let config = settings.hub.clone();
        let hub = Hub::fixed(
            &config,
            vec![api.clone(), web.clone(), outsider.clone()],
            settings,
        );
        Fixture {
            _dirs: vec![api_dir, web_dir, outsider_dir],
            hub,
            api,
            web,
            outsider,
        }
    }

    fn call(hub: &Hub, tool: &str, arguments: Value) -> Value {
        let (response, _) = hub.handle_request(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": tool, "arguments": arguments }
        }));
        response["result"]["structuredContent"].clone()
    }

    fn update_fixed(hub: &Hub, change: impl FnOnce(&mut Vec<WorkspaceProfile>, &mut AppSettings)) {
        let Members::Fixed(fixed) = &hub.members else {
            unreachable!("tests use fixed members")
        };
        let mut guard = fixed.lock().expect("fixed members");
        let (profiles, settings) = &mut *guard;
        change(profiles, settings);
    }

    #[test]
    fn every_workspace_tool_requires_the_workspace_argument() {
        let fixture = fixture();
        let tools = fixture.hub.list_tools();
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();

        assert!(names.contains(&LIST_WORKSPACES));
        assert!(names.contains(&"read_file"));
        // 改服务端共享状态的工具不进 hub，见模块说明第 1 条。
        assert!(!names.contains(&"set_default_cwd"));
        assert!(!names.contains(&"get_default_cwd"));
        for tool in &tools {
            if tool["name"] == LIST_WORKSPACES {
                continue;
            }
            let required = tool["inputSchema"]["required"]
                .as_array()
                .unwrap_or_else(|| panic!("{} 没有 required", tool["name"]));
            assert!(
                required.contains(&json!("workspace")),
                "{} 没把 workspace 设成必填",
                tool["name"]
            );
        }
    }

    /// 没带 workspace 就拒绝，并把能选的列出来——模型按报错重试一次就能对。
    /// 绝不能退回到"上一次用的那个"或"第一个成员"。
    #[test]
    fn a_call_without_workspace_is_refused_instead_of_guessing() {
        let fixture = fixture();
        let first = call(
            &fixture.hub,
            "read_file",
            json!({ "workspace": "api", "path": "only-api.txt" }),
        );
        assert_eq!(first["ok"], true, "{first}");

        let refused = call(&fixture.hub, "read_file", json!({ "path": "only-api.txt" }));

        assert_eq!(refused["error"]["code"], "WORKSPACE_REQUIRED", "{refused}");
        let available = refused["error"]["details"]["available"].to_string();
        assert!(
            available.contains("api") && available.contains("web"),
            "{available}"
        );
    }

    #[test]
    fn paths_resolve_inside_the_selected_workspace_only() {
        let fixture = fixture();

        let own = call(
            &fixture.hub,
            "read_file",
            json!({ "workspace": "api", "path": "only-api.txt" }),
        );
        assert_eq!(own["ok"], true, "{own}");
        assert_eq!(own["hub_workspace"]["id"], fixture.api.id.as_str());

        let other = call(
            &fixture.hub,
            "read_file",
            json!({ "workspace": "web", "path": "only-api.txt" }),
        );
        assert_eq!(other["ok"], false, "web 读到了 api 的文件：{other}");

        // 拿 api 的绝对路径从 web 读：默认 confine-reads 挡住。
        let absolute = format!("{}/only-api.txt", fixture.api.path);
        let escaped = call(
            &fixture.hub,
            "read_file",
            json!({ "workspace": "web", "path": absolute }),
        );
        assert_eq!(escaped["ok"], false, "{escaped}");
    }

    /// 不在 hub 里的工作区和根本不存在的，模型看到的要一模一样，
    /// 候选列表里也不能出现它的名字。
    #[test]
    fn a_workspace_outside_the_hub_looks_the_same_as_a_missing_one() {
        let fixture = fixture();

        let outsider = call(
            &fixture.hub,
            "list_dir",
            json!({ "workspace": fixture.outsider.id, "path": "." }),
        );
        let missing = call(
            &fixture.hub,
            "list_dir",
            json!({ "workspace": "no-such-workspace", "path": "." }),
        );

        assert_eq!(outsider["error"]["code"], "WORKSPACE_NOT_IN_HUB");
        assert_eq!(outsider["error"]["code"], missing["error"]["code"]);
        assert!(!outsider.to_string().contains("outsider"), "{outsider}");
        assert!(!missing.to_string().contains("outsider"), "{missing}");
        let listed = call(&fixture.hub, LIST_WORKSPACES, json!({}));
        assert_eq!(listed["count"], 2, "{listed}");
        assert!(!listed.to_string().contains("outsider"), "{listed}");
    }

    #[test]
    fn a_command_session_cannot_be_read_from_another_workspace() {
        let fixture = fixture();
        let started = call(
            &fixture.hub,
            "exec_command",
            json!({
                "workspace": "api",
                "cmd": "python3 -c \"import time; time.sleep(5)\"",
                "yield_time_ms": 0,
                "timeout_ms": 10_000
            }),
        );
        assert_eq!(started["ok"], true, "{started}");
        let output_ref = started["output_refs"]["stdout"]
            .as_str()
            .unwrap_or_else(|| panic!("没有 output_ref：{started}"))
            .to_string();
        let session_id = started["session_id"]
            .as_str()
            .expect("session id")
            .to_string();

        let from_web = call(
            &fixture.hub,
            "read_output",
            json!({ "workspace": "web", "output_ref": output_ref }),
        );
        assert_eq!(from_web["error"]["code"], "SESSION_NOT_FOUND", "{from_web}");

        let from_api = call(
            &fixture.hub,
            "read_output",
            json!({ "workspace": "api", "output_ref": output_ref }),
        );
        assert_eq!(from_api["ok"], true, "{from_api}");

        let killed = call(
            &fixture.hub,
            "kill_session",
            json!({ "workspace": "api", "session_id": session_id }),
        );
        assert_eq!(killed["ok"], true, "{killed}");
    }

    /// hub 的工具集是 compact（能写），成员 web 自己是 read-only：经 hub 也写不了。
    #[test]
    fn the_hub_never_widens_a_member_tool_profile() {
        let fixture = fixture();
        let web_id = fixture.web.id.clone();
        update_fixed(&fixture.hub, |profiles, _| {
            let web = profiles.iter_mut().find(|p| p.id == web_id).expect("web");
            web.runtime.tool_profile = "read-only".into();
        });

        let blocked = call(
            &fixture.hub,
            "exec_command",
            json!({ "workspace": "web", "cmd": "git --version" }),
        );
        assert_eq!(
            blocked["error"]["code"], "TOOL_NOT_ALLOWED_IN_WORKSPACE",
            "{blocked}"
        );

        let allowed = call(
            &fixture.hub,
            "exec_command",
            json!({ "workspace": "api", "cmd": "git --version" }),
        );
        assert_eq!(allowed["ok"], true, "{allowed}");
    }

    #[test]
    fn planning_mode_only_gates_its_own_workspace() {
        let fixture = fixture();
        PlanningService::new(std::path::Path::new(&fixture.api.path))
            .set_mode(PlanningMode::Plan)
            .expect("plan mode");
        let patch =
            |file: &str| format!("*** Begin Patch\n*** Add File: {file}\n+hello\n*** End Patch\n");

        let in_api = call(
            &fixture.hub,
            "apply_patch",
            json!({ "workspace": "api", "patch": patch("new-in-api.txt") }),
        );
        assert_eq!(in_api["error"]["code"], "PLAN_MODE_READ_ONLY", "{in_api}");

        let in_web = call(
            &fixture.hub,
            "apply_patch",
            json!({ "workspace": "web", "patch": patch("new-in-web.txt") }),
        );
        assert_eq!(in_web["ok"], true, "{in_web}");
        assert!(std::path::Path::new(&fixture.web.path)
            .join("new-in-web.txt")
            .exists());
        assert!(!std::path::Path::new(&fixture.api.path)
            .join("new-in-web.txt")
            .exists());
    }

    #[test]
    fn workspace_context_carries_only_that_workspace_instructions() {
        let fixture = fixture();
        let (api_id, web_id) = (fixture.api.id.clone(), fixture.web.id.clone());
        update_fixed(&fixture.hub, |profiles, _| {
            for profile in profiles.iter_mut() {
                if profile.id == api_id {
                    profile.runtime.ai_instructions = "rule-for-api".into();
                } else if profile.id == web_id {
                    profile.runtime.ai_instructions = "rule-for-web".into();
                }
            }
        });

        let api = call(
            &fixture.hub,
            WORKSPACE_CONTEXT,
            json!({ "workspace": "api" }),
        );

        let instructions = api["instructions"].as_str().expect("instructions");
        assert!(instructions.contains("rule-for-api"), "{api}");
        assert!(!api.to_string().contains("rule-for-web"), "{api}");

        let (initialized, _) = fixture
            .hub
            .handle_request(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }));
        let text = initialized["result"]["instructions"]
            .as_str()
            .expect("instructions");
        assert!(
            !text.contains("rule-for-api") && !text.contains("rule-for-web"),
            "{text}"
        );
        assert!(text.contains("api") && text.contains("web"), "{text}");
    }

    /// 收紧成员配置必须下一次调用就生效；没变就复用，否则每次调用都丢 exec 会话。
    #[test]
    fn member_config_changes_rebuild_its_context_and_nothing_else() {
        let fixture = fixture();
        let (members, settings) = fixture.hub.snapshot().expect("snapshot");
        let api = &members[0];
        let web = &members[1];
        let api_before = fixture.hub.context_for(api, &settings).expect("api");
        let web_before = fixture.hub.context_for(web, &settings).expect("web");
        assert!(Arc::ptr_eq(
            &api_before,
            &fixture.hub.context_for(api, &settings).expect("api again")
        ));

        let mut tightened = api.clone();
        tightened.runtime.allowed_commands = "only:git".into();
        let api_after = fixture
            .hub
            .context_for(&tightened, &settings)
            .expect("api after");
        assert!(!Arc::ptr_eq(&api_before, &api_after), "改了白名单却没重建");
        assert!(Arc::ptr_eq(
            &web_before,
            &fixture.hub.context_for(web, &settings).expect("web again")
        ));

        let mut global = settings.clone();
        global.global_ai_instructions = "new global rule".into();
        let web_after = fixture.hub.context_for(web, &global).expect("web after");
        assert!(
            !Arc::ptr_eq(&web_before, &web_after),
            "改了全局说明却没重建"
        );
    }

    #[test]
    fn removing_a_member_cuts_access_on_the_next_call() {
        let fixture = fixture();
        let before = call(
            &fixture.hub,
            "read_file",
            json!({ "workspace": "web", "path": "only-web.txt" }),
        );
        assert_eq!(before["ok"], true, "{before}");

        let web_id = fixture.web.id.clone();
        update_fixed(&fixture.hub, |_, settings| {
            settings.hub.members.retain(|id| id != &web_id);
        });

        let after = call(
            &fixture.hub,
            "read_file",
            json!({ "workspace": "web", "path": "only-web.txt" }),
        );
        assert_eq!(after["error"]["code"], "WORKSPACE_NOT_IN_HUB", "{after}");
        assert!(!fixture
            .hub
            .contexts
            .lock()
            .expect("contexts")
            .contains_key(&fixture.web.id));
    }

    #[test]
    fn duplicate_names_must_be_disambiguated_by_id() {
        let fixture = fixture();
        let web_id = fixture.web.id.clone();
        update_fixed(&fixture.hub, |profiles, _| {
            profiles
                .iter_mut()
                .find(|p| p.id == web_id)
                .expect("web")
                .name = "api".into();
        });

        let ambiguous = call(
            &fixture.hub,
            "read_file",
            json!({ "workspace": "api", "path": "only-api.txt" }),
        );
        assert_eq!(
            ambiguous["error"]["code"], "WORKSPACE_AMBIGUOUS",
            "{ambiguous}"
        );

        let by_id = call(
            &fixture.hub,
            "read_file",
            json!({ "workspace": fixture.web.id, "path": "only-web.txt" }),
        );
        assert_eq!(by_id["ok"], true, "{by_id}");
    }
}

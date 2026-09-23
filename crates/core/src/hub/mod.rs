//! 聚合入口（hub）：一条 MCP 连接访问多个工作区。
//!
//! 它是什么、怎么用、代价是什么见 docs/concepts.md 的「聚合入口」一节；
//! 这里只记实现上必须守住的规矩。
//!
//! 隔离靠下面三条，改代码时哪条都别松：
//!
//! 1. **服务端不记"当前工作区"。** 除 `list_workspaces` 和转发本机 MCP server 的
//!    三个工具（它们不属于任何工作区，见 [`crate::machine_mcp`]）外，每次
//!    `tools/call` 都必须带 `workspace`。hub 被所有对话、所有客户端共用，服务端
//!    一旦记住"刚切到了 api"，另一个对话里没带参数的调用就会落到 api 上——单工作区里 `set_default_cwd`
//!    已经这么坑过人。所以 hub 连 `set_default_cwd` / `get_default_cwd` 都不暴露。
//! 2. **每个成员一份独立的 [`ToolContext`]**，和成员自己的监听器走同一个
//!    [`build_tool_context`]：根目录边界、命令白名单、读限制、Planning 闸门、
//!    Durable Task 基线都跟着成员走。A 里起的 session 拿到 B 去读，得到的是
//!    SESSION_NOT_FOUND，而不是 A 的输出。
//!
//!    **exec 会话表不在上下文里**，归 [`crate::tools::workspace_runtime`] 按
//!    「目录 + 调用方主体」发。hub 的上下文是按成员 id 缓存的，所有客户端共用
//!    一份——会话表要是挂在它上面，B 拿到 A 工具返回里的 `session_id` 就能读 A
//!    的命令输出。所以 [`Hub::call_local`] 必须把 `auth` 带下去
//!    （[`call_tool_as`]），换成 [`crate::tools::call_tool`] 等于把所有连接并
//!    成一个人。
//! 3. **只收紧、不放宽。** 一个工具要同时在 hub 的工具集和成员自己的工具集里
//!    才能调。成员是 read-only，经 hub 照样写不了。
//!
//! 另外两条是"别漏出去"：不在 hub 里的工作区和不存在的工作区报同一个错，
//! 不给枚举机会；成员的说明文件 / Skill / 历史摘要不在 initialize 里混着注入，
//! 由 `workspace_context` 按工作区单独取，免得 A 的 AGENTS.md 被拿去指导 B。
//!
//! ## 成员有两种
//!
//! [`Member::Local`] 是本机的一个工作区目录，[`Member::Remote`] 是另一台机器上
//! 由 ccnm 管着的 workspace。两者**不共用类型、不互相伪装**（RFC-0002 5.1）：
//! 远端成员没有本机根目录、没有 [`ToolContext`]、不建 Planning 文件，gld 这边
//! 连它在对面是哪个目录都不知道。
//!
//! 工具也是两套，名字不重叠：远端的一律带 `remote_` 前缀，见
//! [`crate::bridge::tools`]。用错了直接报错，**绝不落到另一边执行**——
//! gld 本机也有 `search_text` 和 `list_files`，跟 ccnm 的同名工具根本不是
//! 一个契约（分页、参数、错误码都不同）。

pub mod runtime;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::agent_context::render_skill_catalog_for_profile;
use crate::auth::AuthContext;
use crate::bridge::member::{CcnmMember, Mode};
use crate::bridge::peer::PeerError;
#[cfg(test)]
use crate::bridge::session::Open;
use crate::bridge::session::{CodingError, Connections};
use crate::bridge::tools::{self as remote_tools, RemoteTool};
use crate::data::DataStore;
use crate::machine_mcp::{self, installed::Installed, relay::Relay};
use crate::planning::PlanningService;
use crate::settings::{AppSettings, HubConfig};
use crate::tools::registry::{
    canonical_tool_name, exposed_tool_names, list_tools_for_profile, normalize_tool_profile,
};
use crate::tools::workspace::{tool_err, tool_ok, WorkspaceError};
use crate::tools::{
    build_tool_context, call_tool_as, wrap_mcp_tool_result, Caller, SharedToolContext,
};
use crate::usage::ServiceUsage;
use crate::workspace::{AuthConfig, WorkspaceProfile};

/// 日志目录、OAuth 客户端注册表、密钥共用的作用域名。
///
/// 工作区 id 是 32 位十六进制，`hub` 不可能和它撞上。
pub const HUB_SCOPE: &str = "hub";

/// `serverInfo.name`。不用任何成员的名字：它代表的是一组工作区。
pub const SERVER_NAME: &str = "gld-hub";

/// hub 自己的凭据名。存在数据文件的 `app_secrets["hub"]` 里，和任何工作区、
/// 共享密钥池都不共用——工作区凭据泄露不该连带打开 hub。
pub const HUB_SECRET_KEYS: &[&str] = &[
    "bearer_token",
    "oauth_password",
    "oauth_token_secret",
    "oauth_client_id",
];

/// 启动监听器要的凭据，由调用方（`App`）从它的数据存储里取出来传进来。
pub struct HubSecrets {
    pub bearer_token: String,
    pub oauth_client_id: String,
    pub oauth_password: String,
    pub oauth_token_secret: String,
}

/// 起隧道要的密钥，只能由用户给（`gld secret set`），不能重新生成：
/// 随机一串 Cloudflare 不认。
pub const HUB_TUNNEL_SECRET_KEYS: &[&str] = &["cloudflare_token"];

/// 按配置推得出的公网基地址（不带 `/mcp`）；没有公网入口、或者要等隧道起来才知道
/// （Cloudflare 临时地址）时为空串。跑着的服务以 [`runtime::public_base`] 为准。
///
/// 服务自己的隧道（RFC-0004）优先；没配隧道时走老路子：经全局入口是
/// `<入口公网地址>/hub`，入口按这个前缀转到本地端口；入口没启用就退回手动填的
/// public_url，和工作区 `effective_public_url` 一个口径。
pub fn public_base_url(settings: &AppSettings) -> String {
    let hub = &settings.hub;
    match hub.tunnel_type.as_str() {
        "frp" => return crate::tunnel::standalone::frp_public_base(&frp_spec(hub), settings),
        "cloudflare" if hub.cloudflare_mode == "named" => {
            return hub.public_url.trim_end_matches('/').to_string()
        }
        "cloudflare" => return String::new(),
        _ => {}
    }
    if settings.hub.use_global_gateway && settings.global_gateway.enabled {
        let base = settings.global_gateway.public_url.trim_end_matches('/');
        if base.is_empty() {
            return String::new();
        }
        return format!("{base}/hub");
    }
    settings.hub.public_url.trim_end_matches('/').to_string()
}

/// 服务那条 FRP 隧道的参数。`name` 也是它的 frpc 配置和日志目录名，和
/// [`HUB_SCOPE`] 一样，于是 frpc 的日志就落在服务自己的日志目录里。
pub fn frp_spec(hub: &HubConfig) -> crate::tunnel::standalone::FrpSpec<'_> {
    crate::tunnel::standalone::FrpSpec {
        name: HUB_SCOPE,
        port: hub.local_port,
        frp_profile_id: &hub.frp_profile_id,
        frp_server: "",
        frp_server_port: 0,
        subdomain: &hub.frp_subdomain,
        use_proxy: hub.use_proxy,
    }
}

/// hub 自己提供的两个工具。列成员不需要 `workspace`，取说明需要。
pub const LIST_WORKSPACES: &str = "list_workspaces";
pub const WORKSPACE_CONTEXT: &str = "workspace_context";

/// 经 hub 不暴露的工具：它们改的是所有对话共享的服务端状态，见模块说明第 1 条。
const HIDDEN_TOOLS: &[&str] = &["set_default_cwd", "get_default_cwd"];

const INSTRUCTIONS: &str = "This server is a gld hub: one connection to several workspaces. Every tool call except list_workspaces must include `workspace` (an id or name returned by list_workspaces). The server keeps no current workspace, so pass it again on every call and never rely on the workspace of an earlier call. Workspaces are isolated from each other: paths are relative to the selected workspace root, command sessions and output_ref values only exist in the workspace that created them, and planning mode, permissions and tool sets are enforced per workspace. Before working in a workspace for the first time in a conversation, call workspace_context for it and follow only that workspace's instructions; never apply one workspace's instructions or skills to another. Planning mode is controlled exclusively by the gld CLI, and only the human operator can accept Goal or Plan reviews. If an operation returns DANGEROUS_OPERATION_REQUIRES_CONFIRMATION, retry the same tool with confirm=true only when the user's request already clearly authorizes it; otherwise ask the user. A workspace listed with kind=remote lives on another machine and is reached only through the tools named remote_*, which it lists; the hub's own tools do not work on it and the remote_* tools do not work on a local workspace. The two sets are different contracts even where the names look alike, so never substitute one for the other, and read a remote workspace only through its own tools rather than assuming anything about it from a local one.";

/// 成员名单从哪儿来。
enum Members {
    /// 每次请求从数据文件读：增删成员、成员改配置立刻生效，不用重启 hub。
    DataFile,
    /// 固定的一份，给单元测试用；套 Mutex 是为了测"改完配置下一次调用就生效"。
    #[cfg(test)]
    Fixed(Box<Mutex<Fixed>>),
}

/// 测试里那份固定的成员名单。
#[cfg(test)]
#[derive(Clone)]
struct Fixed {
    profiles: Vec<WorkspaceProfile>,
    remotes: Vec<CcnmMember>,
    settings: AppSettings,
}

/// hub 里的一个成员。
///
/// **不拿假路径把远端塞进本地那套**（RFC-0002 5.1）：`Local` 有本机根目录、
/// 工具上下文、Planning 和 Harness，`Remote` 一样都没有，它只有「哪台机器上的
/// 哪个 workspace」和一个访问上限。
/// 两个变体差了 776 字节（本地的 900+，远端的 128）。不 Box：成员名单每次
/// 请求都要重建，一个 hub 也就十来个成员，多这点内存远比多十来次堆分配和
/// 一层解引用划算——何况本地那份 `WorkspaceProfile` 本来就是整份克隆出来的。
#[allow(clippy::large_enum_variant)]
enum Member {
    Local(WorkspaceProfile),
    Remote(CcnmMember),
}

impl Member {
    fn id(&self) -> &str {
        match self {
            Member::Local(profile) => &profile.id,
            Member::Remote(remote) => &remote.id,
        }
    }

    fn name(&self) -> &str {
        match self {
            Member::Local(profile) => &profile.name,
            Member::Remote(remote) => &remote.name,
        }
    }
}

struct CachedContext {
    fingerprint: String,
    context: SharedToolContext,
}

/// 一次请求实际落到了哪个成员，监听器拿它往那个工作区的日志里记一笔。
pub struct Routed {
    pub workspace_id: String,
    /// 落到远端成员时是 `None`——远端没有本机工具上下文，这正是分型的意义。
    pub context: Option<SharedToolContext>,
}

pub struct Hub {
    members: Members,
    tool_profile: String,
    /// 只用来让 `server_info` 报出真实的认证方式（hub 的，不是成员自己监听器的）。
    auth: AuthConfig,
    usage: Arc<ServiceUsage>,
    contexts: Mutex<HashMap<String, CachedContext>>,
    /// 远端成员的 bridge 连接。本地成员一条都用不到。
    connections: Connections,
    /// 转发本机装好的 MCP server（`gld mcp on` 开了才有东西）。
    relay: Relay,
    /// 测试用：不读这台机器的 `~/.claude.json`，用给定的一份；也不真起进程。
    #[cfg(test)]
    installed: Option<Installed>,
    #[cfg(test)]
    opener: Option<Box<dyn toexec_mcp::Open>>,
}

impl Hub {
    pub fn new(config: &HubConfig) -> Self {
        Self::with_members(config, Members::DataFile, Connections::new())
    }

    #[cfg(test)]
    fn fixed(config: &HubConfig, fixed: Fixed) -> Self {
        Self::with_members(
            config,
            Members::Fixed(Box::new(Mutex::new(fixed))),
            Connections::new(),
        )
    }

    /// 测试用：成员名单固定，远端连接也用合成通道，不起任何子进程。
    #[cfg(test)]
    fn fixed_with_opener(config: &HubConfig, fixed: Fixed, opener: Box<dyn Open>) -> Self {
        Self::with_members(
            config,
            Members::Fixed(Box::new(Mutex::new(fixed))),
            Connections::with_opener(opener),
        )
    }

    fn with_members(config: &HubConfig, members: Members, connections: Connections) -> Self {
        Self {
            members,
            tool_profile: normalize_tool_profile(&config.tool_profile).into(),
            auth: AuthConfig {
                auth_type: config.auth_type.clone(),
                ..AuthConfig::default()
            },
            usage: Arc::new(ServiceUsage::default()),
            contexts: Mutex::new(HashMap::new()),
            connections,
            relay: Relay::new(),
            #[cfg(test)]
            installed: None,
            #[cfg(test)]
            opener: None,
        }
    }

    /// 测试用：换成内存里的 MCP server，装了哪些也由测试给。
    #[cfg(test)]
    fn with_relay(mut self, opener: Box<dyn toexec_mcp::Open>, installed: Installed) -> Self {
        self.opener = Some(opener);
        self.installed = Some(installed);
        self
    }

    fn installed_mcp(&self) -> Installed {
        #[cfg(test)]
        if let Some(installed) = &self.installed {
            return installed.clone();
        }
        machine_mcp::read_installed()
    }

    /// 开着而且装着的 MCP server 转给 AI 的那三个工具；一个都没有、或者服务是
    /// read-only 时不列（转过去的工具能做什么由 server 决定，只读管不住它）。
    fn relay_definitions(&self, settings: &AppSettings) -> Vec<Value> {
        if self.tool_profile == "read-only" || settings.relayed_mcp_servers.is_empty() {
            return Vec::new();
        }
        let installed = self.installed_mcp();
        let offered = machine_mcp::relay::offered(&settings.relayed_mcp_servers, &installed);
        if offered.is_empty() {
            return Vec::new();
        }
        let mut tools = Relay::definitions(&offered);
        // 这个工具集的全部意义就是把标注改成只读（见 docs/concepts.md），这里
        // 跟着改，不然它就漏了三个。
        if self.tool_profile == "compat-readonly-all" {
            for tool in &mut tools {
                tool["annotations"]["readOnlyHint"] = json!(true);
                tool["annotations"]["destructiveHint"] = json!(false);
                tool["annotations"]["openWorldHint"] = json!(false);
            }
        }
        tools
    }

    fn call_relay(
        &self,
        auth: &AuthContext,
        tool: &str,
        args: &Value,
        settings: &AppSettings,
    ) -> Value {
        if self.tool_profile == "read-only" {
            return plain_result(tool_err(WorkspaceError::Tool {
                code: "TOOL_NOT_ALLOWED",
                message: format!(
                    "{tool} is not available: this service's tool profile is read-only, and a relayed MCP tool can do whatever its server does."
                ),
                category: "permission",
                retryable: false,
            }));
        }
        let installed = self.installed_mcp();
        let caller = auth.tag();
        let real = machine_mcp::open::Opener::new(machine_mcp::launch(settings));
        #[allow(unused_mut)]
        let mut opener: &dyn toexec_mcp::Open = &real;
        #[cfg(test)]
        if let Some(fake) = &self.opener {
            opener = fake.as_ref();
        }
        self.relay.call(
            tool,
            args,
            &machine_mcp::Scope {
                on: &settings.relayed_mcp_servers,
                installed: &installed,
                opener,
                caller: &caller,
            },
        )
    }

    pub fn usage(&self) -> Arc<ServiceUsage> {
        self.usage.clone()
    }

    /// 结束**经 hub** 起的、还在跑的命令，并关掉所有远端 bridge。
    ///
    /// 只停 [`HUB_SCOPE`] 这个入口的：会话表按目录 + 主体分，同一个目录上
    /// 还有命令行和工作区自己监听器起的命令，操作员停的是 hub，不该连累它们。
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
            cached
                .context
                .runtime
                .terminate_sessions_in_scope(HUB_SCOPE);
        }
        // 远端 bridge 必须走正常关闭：直接留着进程不管，远端 Runtime 的写锁
        // 会留下 held 标记要人工恢复。
        self.connections.close_all();
        self.relay.close_all();
    }

    /// 处理一条 JSON-RPC 请求。会同步跑工具，必须在 `spawn_blocking` 里调。
    ///
    /// `auth` 是监听器验完鉴权之后的主体。远端成员的 bridge 按它分：同一个
    /// 工作区、两个不同的主体，拿到的是两条连接（RFC-0002 5.3）。
    pub fn handle_request(&self, auth: &AuthContext, body: &Value) -> (Value, Option<Routed>) {
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
            "tools/call" => self.call(auth, &params, &mut routed),
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
                    .map(|member| match member {
                        Member::Local(_) => format!("- {} (id {})", member.name(), member.id()),
                        Member::Remote(_) => format!(
                            "- {} (id {}, remote: use the remote_* tools)",
                            member.name(),
                            member.id()
                        ),
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                format!(
                    "Workspaces in this hub when the connection was made (call list_workspaces for the current list):\n{lines}"
                )
            }
            Err(error) => format!("The workspace list is unavailable right now: {error}"),
        };
        let relayed = match self.snapshot() {
            Ok((_, settings)) if !self.relay_definitions(&settings).is_empty() => format!(
                "\n\nMCP servers installed on this machine are relayed too, outside any workspace: {} lists them and their tools, {} calls one.",
                machine_mcp::relay::LIST,
                machine_mcp::relay::CALL
            ),
            _ => String::new(),
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
            "instructions": format!("{INSTRUCTIONS}\n\n{catalog}{relayed}")
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
        // 有远端成员才报远端工具。一个只有本地成员的 hub 列出四个永远调不通
        // 的 `remote_*`，只会引着模型去试；`listChanged` 是 false，但这里本来
        // 就以 list_workspaces 为准，跟成员名单一个口径。
        //
        // coding 那几个同理，再窄一层：要有成员的上限真的到 coding 才列。
        // 全是只读成员时列出 `remote_coding_begin`，模型只会得到一个必然
        // 失败的调用。
        if let Ok((members, settings)) = self.snapshot() {
            tools.extend(self.relay_definitions(&settings));
            let remotes: Vec<&CcnmMember> = members
                .iter()
                .filter_map(|m| match m {
                    Member::Remote(remote) => Some(remote),
                    Member::Local(_) => None,
                })
                .collect();
            if !remotes.is_empty() {
                let any_coding = remotes.iter().any(|r| r.max_mode == Mode::Coding);
                tools.extend(remote_tools::definitions(any_coding));
            }
        }
        tools
    }

    /// server_info 里补上**这条连接**的 tools/list 实际给出的工具表（审查 D04）。
    ///
    /// 成员自己的 server_info 报的是成员工具集那一份：含 hub 隐藏掉的
    /// get/set_default_cwd，不含 list_workspaces、远端和中继工具，也没有参数。
    /// 拿它跟客户端看到的表对，对不上也说明不了问题。
    fn with_connection_surface(&self, mut info: Value) -> Value {
        if let Some(object) = info.as_object_mut() {
            let mut surface = crate::tools::registry::surface_digest(&self.list_tools());
            surface["note"] = json!(
                "The tool list this connection's tools/list returns right now. If your own tool list lacks any of these tools or parameters, the client is using a cached older list: refresh the connector's tools (ChatGPT: connector settings → refresh) instead of guessing arguments or asking for permissions."
            );
            object.insert("connection".into(), surface);
        }
        info
    }

    fn exposes(&self, name: &str) -> bool {
        name == LIST_WORKSPACES
            || name == WORKSPACE_CONTEXT
            || machine_mcp::is_tool(name)
            || remote_tools::find(name).is_some()
            || remote_tools::is_session_tool(name)
            || (!HIDDEN_TOOLS.contains(&name)
                && exposed_tool_names(&self.tool_profile).contains(&name))
    }

    fn call(
        &self,
        auth: &AuthContext,
        params: &Value,
        routed: &mut Option<Routed>,
    ) -> Result<Value, Value> {
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
        self.relay.retain(&settings.relayed_mcp_servers);
        if canonical == LIST_WORKSPACES {
            return Ok(plain_result(list_workspaces(&members)));
        }
        if machine_mcp::is_tool(canonical) {
            let args = crate::mcp::tool_arguments(name, params);
            return Ok(self.call_relay(auth, canonical, &args, &settings));
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

        // 会话工具（begin/end）也是远端的，先分出去。
        if remote_tools::is_session_tool(canonical) {
            return Ok(match member {
                Member::Remote(remote) => {
                    *routed = Some(Routed {
                        workspace_id: remote.id.clone(),
                        context: None,
                    });
                    self.coding_session(auth, canonical, remote, &args)
                }
                Member::Local(local) => plain_result(tool_err(WorkspaceError::ToolDetails {
                    code: "TOOL_IS_FOR_REMOTE_WORKSPACES",
                    message: format!(
                        "{canonical} only works on a remote ccnm workspace, and {} is a local one.",
                        local.name
                    ),
                    category: "validation",
                    retryable: false,
                    details: json!({ "workspace": member_ref(member) }),
                })),
            });
        }

        // 两套工具、两种成员，只有对角线上的两格能往下走。用错的那两格都
        // 只报错，**绝不落到另一边执行**（RFC-0002 5.2）。
        match (remote_tools::find(canonical), member) {
            (Some(tool), Member::Remote(remote)) => {
                *routed = Some(Routed {
                    workspace_id: remote.id.clone(),
                    context: None,
                });
                Ok(self.call_remote(auth, tool, remote, &args))
            }
            (Some(_), Member::Local(local)) => Ok(plain_result(tool_err(
                WorkspaceError::ToolDetails {
                    code: "TOOL_IS_FOR_REMOTE_WORKSPACES",
                    message: format!(
                        "{canonical} only works on a remote ccnm workspace, and {} is a local one. Use this hub's own tools for it.",
                        local.name
                    ),
                    category: "validation",
                    retryable: false,
                    details: json!({ "workspace": member_ref(member) }),
                },
            ))),
            (None, Member::Remote(remote)) => Ok(plain_result(tool_err(
                WorkspaceError::ToolDetails {
                    code: "TOOL_IS_LOCAL_ONLY",
                    message: format!(
                        "{canonical} runs on this machine, and {} is a remote ccnm workspace. Remote workspaces have their own tools: {}. They are a different contract, not the same tool over a network.",
                        remote.name,
                        remote_tool_names().join(", ")
                    ),
                    category: "validation",
                    retryable: false,
                    details: json!({
                        "workspace": member_ref(member),
                        "remote_tools": remote_tool_names(),
                    }),
                },
            ))),
            (None, Member::Local(local)) => {
                self.call_local(auth, canonical, local, &args, &settings, routed)
            }
        }
    }

    /// 本地成员：和分型之前一模一样，只多带一个调用方主体。
    fn call_local(
        &self,
        auth: &AuthContext,
        canonical: &str,
        member: &WorkspaceProfile,
        args: &Value,
        settings: &AppSettings,
        routed: &mut Option<Routed>,
    ) -> Result<Value, Value> {
        let context = match self.context_for(member, settings) {
            Ok(context) => context,
            Err(message) => {
                return Ok(plain_result(tool_err(WorkspaceError::ToolDetails {
                    code: "WORKSPACE_UNAVAILABLE",
                    message: format!("Workspace {} cannot be opened: {message}", member.name),
                    category: "runtime",
                    retryable: false,
                    details: json!({ "workspace": local_ref(member) }),
                })))
            }
        };
        *routed = Some(Routed {
            workspace_id: member.id.clone(),
            context: Some(context.clone()),
        });

        let structured = if canonical == WORKSPACE_CONTEXT {
            self.workspace_context(member, &context)
        } else if !exposed_tool_names(&context.tool_profile).contains(&canonical) {
            tool_not_in_workspace(canonical, member, &context.tool_profile)
        } else if canonical == "server_info" {
            self.with_connection_surface(call_tool_as(
                &context,
                &Caller::from_auth(auth),
                canonical,
                args,
            ))
        } else {
            call_tool_as(&context, &Caller::from_auth(auth), canonical, args)
        };
        let result = wrap_mcp_tool_result(canonical, args, tag_workspace(structured, member));
        context.record_context_block("tool_return", &result);
        Ok(result)
    }

    /// 远端成员：参数过白名单，经 bridge 转一次，结果原样带回来。
    ///
    /// 远端的 `content` / `structuredContent` / `isError` 一个字都不改
    /// （验收项 H04）——那是 ccnm 的契约，改了模型看到的就不是远端说的话。
    /// 是哪个成员答的放在 `_meta` 里，不往 ccnm 的结构化结果里塞字段。
    fn call_remote(
        &self,
        auth: &AuthContext,
        tool: &'static RemoteTool,
        member: &CcnmMember,
        args: &Value,
    ) -> Value {
        let missing = tool.missing_required(args);
        if !missing.is_empty() {
            return plain_result(tool_err(WorkspaceError::ToolDetails {
                code: "MISSING_ARGUMENT",
                message: format!("{} needs {}", tool.name, missing.join(", ")),
                category: "validation",
                retryable: false,
                details: json!({ "missing": missing }),
            }));
        }
        // 超了 gld 这边上限的参数（现在只有 wait_ms）：在这边拒，不替调用方
        // 改小——改小它会以为自己等过了（见 remote_tools::MAX_WAIT_MS）。
        if let Some((argument, max)) = tool.over_limit(args) {
            return plain_result(tool_err(WorkspaceError::ToolDetails {
                code: "ARGUMENT_OUT_OF_RANGE",
                message: format!(
                    "{} takes {argument} up to {max} through this hub, which cuts a remote call off at {} seconds. Ask for less, and call again if the command is still running.",
                    tool.name,
                    crate::bridge::session::CALL_TIMEOUT.as_secs()
                ),
                category: "validation",
                retryable: false,
                details: json!({ "argument": argument, "max": max }),
            }));
        }
        // exec_command 的期限要分前台后台看：后台的期限是命令自己的事（调用
        // 立刻返回），前台的却要跟 hub 的调用预算比——远端那个 120000 的默认
        // 值本身就打得爆它，见 remote_tools::MAX_FOREGROUND_TIMEOUT_MS。
        let foreground = remote_tools::foreground_timeout(tool, args);
        if let remote_tools::ForegroundTimeout::TooLong { max } = foreground {
            return plain_result(tool_err(WorkspaceError::ToolDetails {
                code: "ARGUMENT_OUT_OF_RANGE",
                message: format!(
                    "{} takes timeout_ms up to {max} in the foreground through this hub, which cuts a remote call off at {} seconds; a longer call would end the coding session and stop every background command in it. Set run_in_background instead, then watch it with remote_read_output.",
                    tool.name,
                    crate::bridge::session::CALL_TIMEOUT.as_secs()
                ),
                category: "validation",
                retryable: false,
                details: json!({ "argument": "timeout_ms", "max": max }),
            }));
        }
        let mut forwarded = tool.forward_arguments(args);
        if let remote_tools::ForegroundTimeout::Fill(ms) = foreground {
            forwarded["timeout_ms"] = json!(ms);
        }
        if remote_tools::needs_coding(tool) {
            return self.call_coding(auth, tool, member, args, forwarded);
        }
        match self
            .connections
            .call(&auth.tag(), member, tool.remote_name, forwarded)
        {
            Ok(result) => tag_remote(result, member, Mode::Read),
            Err(error) => plain_result(remote_failure(tool.name, member, error)),
        }
    }

    /// 要写的那三个工具：必须带一个还认的 coding 句柄。
    fn call_coding(
        &self,
        auth: &AuthContext,
        tool: &'static RemoteTool,
        member: &CcnmMember,
        args: &Value,
        forwarded: Value,
    ) -> Value {
        let Some(handle) = args
            .get(remote_tools::HANDLE_ARG)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return plain_result(tool_err(WorkspaceError::ToolDetails {
                code: "MISSING_ARGUMENT",
                message: format!(
                    "{} needs {}: call remote_coding_begin on {} first.",
                    tool.name,
                    remote_tools::HANDLE_ARG,
                    member.name
                ),
                category: "validation",
                retryable: false,
                details: json!({ "missing": [remote_tools::HANDLE_ARG] }),
            }));
        };
        match self
            .connections
            .call_coding(&auth.tag(), member, handle, tool.remote_name, forwarded)
        {
            Ok(result) => tag_remote(result, member, Mode::Coding),
            Err(error) => plain_result(coding_failure(tool.name, member, error)),
        }
    }

    /// `remote_coding_begin` / `remote_coding_end`。
    fn coding_session(
        &self,
        auth: &AuthContext,
        tool: &str,
        member: &CcnmMember,
        args: &Value,
    ) -> Value {
        // RFC-0002 5.3：第一版 remote coding 要求可验证的认证身份，
        // noauth 不开放该能力。只读不受这条限制。
        if !auth.is_authenticated() {
            return plain_result(tool_err(WorkspaceError::ToolDetails {
                code: "CODING_REQUIRES_AUTH",
                message: format!(
                    "Writing to a remote workspace needs an authenticated connection, and this hub is running with auth_type=noauth. Reading {} still works.",
                    member.name
                ),
                category: "permission",
                retryable: false,
                details: json!({ "workspace": remote_ref(member) }),
            }));
        }
        if member.max_mode < Mode::Coding {
            return plain_result(tool_err(WorkspaceError::ToolDetails {
                code: "REMOTE_IS_READ_ONLY",
                message: format!(
                    "Remote workspace {} is configured read-only, so it has no writing session. The operator raises that with `gld remote add ... --mode coding`.",
                    member.name
                ),
                category: "permission",
                retryable: false,
                details: json!({ "workspace": remote_ref(member) }),
            }));
        }
        if tool == remote_tools::CODING_END {
            let handle = args
                .get(remote_tools::HANDLE_ARG)
                .and_then(Value::as_str)
                .unwrap_or_default();
            self.connections.end_coding(&auth.tag(), member, handle);
            // 幂等：句柄本来就没了也报成功。调用方重试一次关闭不该拿到失败，
            // 而"已经关了"和"刚刚关掉"对它是同一件事。
            return plain_result(tool_ok(json!({
                "closed": true,
                "workspace": remote_ref(member),
                "note": "The write lock on that machine is released. Any output_ref from this session is gone."
            })));
        }
        match self.connections.begin_coding(&auth.tag(), member) {
            Ok(handle) => plain_result(tool_ok(json!({
                "coding_handle": handle,
                "workspace": remote_ref(member),
                "tools": remote_tools::CODING_TOOLS.iter().map(|t| t.name).collect::<Vec<_>>(),
                "note": "This holds the write lock on that machine. Call remote_coding_end as soon as you are done; it also ends by itself after a few idle minutes, and its output_ref values do not survive that."
            }))),
            Err(error) => plain_result(remote_failure(remote_tools::CODING_BEGIN, member, error)),
        }
    }

    /// 当前成员（按加入顺序，跳过已经不存在的 id）和这一刻的全局设置。
    ///
    /// 一个 id 先在本地工作区里找，找不到再去远端成员里找。两边的 id 撞上
    /// 是配置错误，不是这里该兜的事——本地 id 是 32 位十六进制，远端 id 由
    /// 操作员起，撞上得手动改。
    fn snapshot(&self) -> Result<(Vec<Member>, AppSettings), String> {
        let (profiles, remotes, settings) = match &self.members {
            Members::DataFile => DataStore::read_file(|data| {
                Ok((
                    data.profiles.clone(),
                    data.ccnm_members.clone(),
                    AppSettings::from_data(data),
                ))
            })
            .map_err(|error| error.to_string())?,
            #[cfg(test)]
            Members::Fixed(fixed) => {
                let fixed = fixed.lock().expect("fixed members").clone();
                (fixed.profiles, fixed.remotes, fixed.settings)
            }
        };
        let members = settings
            .hub
            .members
            .iter()
            .filter_map(|id| {
                profiles
                    .iter()
                    .find(|profile| &profile.id == id)
                    .cloned()
                    .map(Member::Local)
                    .or_else(|| {
                        remotes
                            .iter()
                            .find(|remote| &remote.id == id)
                            .cloned()
                            .map(Member::Remote)
                    })
            })
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
        contexts.insert(
            member.id.clone(),
            CachedContext {
                fingerprint,
                context: context.clone(),
            },
        );
        // 这里**不收命令**。以前是收的，理由是"旧上下文里起的命令不会再有人来
        // 读，不收就是孤儿进程"——会话表跟着上下文走，上下文一换那些 session_id
        // 就查不到了。现在表归目录 + 主体所有，换上下文不影响，同一个客户端接着
        // 拿旧 session_id 读得到。
        //
        // 所以那个理由没了，而收命令的代价一直都在：指纹里有 AI 说明、可执行
        // 路径这些字段，改一行全局说明就会重建一次上下文，正在跑的 `npm run dev`
        // 被顺手杀掉。要停某个入口的命令有专门的办法，见 [`Hub::shutdown`]。
        Ok(context)
    }

    /// 被移出 hub 或已经 destroy 的成员：丢掉上下文，收掉它的命令和远端连接。
    ///
    /// 远端那半是 RFC-0002 5.1「成员删除/权限收紧/配置变化须使对应旧会话
    /// 失效」——操作员把成员摘出去了，正在开着的 bridge 不能还留着。
    fn forget_departed(&self, members: &[Member]) {
        let departed: Vec<CachedContext> = {
            let mut contexts = self
                .contexts
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let gone: Vec<String> = contexts
                .keys()
                .filter(|id| !members.iter().any(|member| member.id() == id.as_str()))
                .cloned()
                .collect();
            gone.iter().filter_map(|id| contexts.remove(id)).collect()
        };
        // 只收经 hub 起的：操作员收回的是"经这个入口访问它"的权限，那个工作区
        // 自己的监听器和命令行起的命令不在这次收回的范围里。
        for cached in departed {
            cached
                .context
                .runtime
                .terminate_sessions_in_scope(HUB_SCOPE);
        }
        let still_here: Vec<String> = members
            .iter()
            .filter(|member| matches!(member, Member::Remote(_)))
            .map(|member| member.id().to_string())
            .collect();
        self.connections.retain(&still_here);
    }

    fn workspace_context(&self, member: &WorkspaceProfile, context: &SharedToolContext) -> Value {
        let member_tools = exposed_tool_names(&context.tool_profile);
        let tools: Vec<&str> = exposed_tool_names(&self.tool_profile)
            .into_iter()
            .filter(|name| !HIDDEN_TOOLS.contains(name) && member_tools.contains(name))
            .collect();
        // 目录怎么给（compact 有字符预算）和单工作区 initialize 走同一个函数，
        // 两边口径不会漂。
        let skills =
            render_skill_catalog_for_profile(&context.current_skills(), &context.tool_profile).text;
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
        &settings.global_hidden_skills,
    ))
    .unwrap_or_default()
}

/// 按 id、名称、不分大小写的名称、id 前缀（≥4 位）的顺序找成员。
///
/// 不复用命令行的 `-w` 解析：那边还认路径、报错教人跑 `gld workspace list`，
/// 而这里的读者是模型，候选列表只能来自 hub 成员——漏出其他工作区的名字就是泄露。
fn resolve_member<'a>(members: &'a [Member], selector: &str) -> Result<&'a Member, Value> {
    if let Some(found) = members.iter().find(|member| member.id() == selector) {
        return Ok(found);
    }
    let rules: [&dyn Fn(&Member) -> bool; 3] = [
        &|member| member.name() == selector,
        &|member| member.name().eq_ignore_ascii_case(selector),
        &|member| selector.len() >= 4 && member.id().starts_with(selector),
    ];
    for rule in rules {
        let matched: Vec<&Member> = members.iter().filter(|member| rule(member)).collect();
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

fn workspace_required(tool: &str, members: &[Member]) -> Value {
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
        details: json!({ "workspace": local_ref(member), "tool_profile": profile }),
    })
}

fn list_workspaces(members: &[Member]) -> Value {
    tool_ok(json!({
        "workspaces": members
            .iter()
            .map(|member| match member {
                Member::Local(local) => json!({
                    "id": local.id,
                    "name": local.name,
                    "kind": "local",
                    "path": local.path,
                    "tool_profile": normalize_tool_profile(&local.runtime.tool_profile),
                }),
                // 远端不报 path：那是对面机器上的目录，gld 这边根本不知道，
                // 报一个假的比不报更糟。也不报 node —— 模型路由只需要 id。
                Member::Remote(remote) => json!({
                    "id": remote.id,
                    "name": remote.name,
                    "kind": "remote",
                    "mode": remote.max_mode.as_str(),
                    "tools": remote_tool_names(),
                }),
            })
            .collect::<Vec<_>>(),
        "count": members.len(),
        "usage": "Pass one of these ids or names as `workspace` on every other tool call. A workspace with kind=remote lives on another machine: use only the tools it lists, not this hub's own ones."
    }))
}

/// hub 暴露的远端工具名，用在报错和 `list_workspaces` 里。
fn remote_tool_names() -> Vec<&'static str> {
    remote_tools::READ_TOOLS.iter().map(|t| t.name).collect()
}

fn member_ref(member: &Member) -> Value {
    json!({ "id": member.id(), "name": member.name() })
}

fn local_ref(member: &WorkspaceProfile) -> Value {
    json!({ "id": member.id, "name": member.name })
}

fn remote_ref(member: &CcnmMember) -> Value {
    json!({ "id": member.id, "name": member.name })
}

fn describe<'a>(members: impl Iterator<Item = &'a Member>) -> String {
    let listed = members
        .map(|member| format!("{} (id {})", member.name(), member.id()))
        .collect::<Vec<_>>();
    if listed.is_empty() {
        "none".into()
    } else {
        listed.join(", ")
    }
}

/// 一次远端调用没走通，翻译成模型和操作员都能照着做事的错。
///
/// 每种失败一个码：告诉模型「重试有没有用」，告诉操作员「该去看哪儿」。
/// 合成一个 REMOTE_ERROR 的话，「ccnm 没装」和「网络抖了一下」看起来一样。
///
/// `Closed` 会把远端 stderr 的最后一段带出来——ccnm 的 `CCNM_E_*` 诊断只在
/// 那里，丢掉就只剩「连接断了」，没人查得下去。gld 从不往 bridge 传凭据，
/// 这段文本里也就不会有。
fn remote_failure(tool: &str, member: &CcnmMember, error: PeerError) -> Value {
    let (code, category, retryable) = match &error {
        // 本机根本没起来这个程序：装没装、路径对不对，是操作员的事。
        PeerError::Spawn { .. } => ("REMOTE_BRIDGE_NOT_STARTED", "runtime", false),
        PeerError::Write(_) => ("REMOTE_BRIDGE_CLOSED", "runtime", true),
        PeerError::Closed { stderr, .. } => classify_startup_failure(stderr),
        // 只读工具没有副作用，重试是安全的。写操作另走 coding_failure。
        PeerError::Timeout { .. } => ("REMOTE_TIMEOUT", "runtime", true),
        PeerError::Malformed { .. } => ("REMOTE_PROTOCOL_ERROR", "runtime", false),
        PeerError::Remote { .. } => ("REMOTE_REFUSED", "runtime", false),
        PeerError::Handshake(_) => ("REMOTE_PROTOCOL_MISMATCH", "runtime", false),
        // 远端那一版没这个工具 / 不收这个参数。**调用没发出去**，所以它跟
        // 「断在半路」是两回事：那边什么都没做，重试也没用，要升级 ccnm。
        PeerError::Unsupported { .. } => ("REMOTE_TOOL_UNSUPPORTED", "validation", false),
    };
    // 拒绝是照着对面哪一份能力做的，说出来才判断得了"我升过 ccnm 了吗"、
    // "是不是连错机器了"（跨仓评审 X10：能力代次不用另造，握手的 serverInfo
    // 加 tools/list 就是）。只有这一种错有这份依据——别的错发生在核对之前。
    let mut details = json!({ "workspace": remote_ref(member) });
    if let PeerError::Unsupported { remote, .. } = &error {
        details["remote_capabilities"] = remote.to_value();
    }
    // 远端的 ccnm 只在有 server 可转时才列 call_mcp_tool（P49），所以"没有这个
    // 工具"多半不是版本旧，而是那边没东西可转：说"升级 ccnm"会把人支错方向。
    let message = match &error {
        PeerError::Unsupported {
            argument: None,
            remote,
            ..
        } if tool == "remote_call_mcp_tool" => format!(
            "{tool} on remote workspace {}: the ccnm on that machine offers no MCP server to relay -- none is declared in the project's .mcp.json or installed for the account the runtime uses, [runtime_mcp] is off in its config, or it predates relaying MCP servers. It reports itself as {}; the call was not sent.",
            member.name,
            remote.label()
        ),
        _ => format!("{tool} on remote workspace {}: {error}", member.name),
    };
    tool_err(WorkspaceError::ToolDetails {
        code,
        message,
        category,
        retryable,
        details,
    })
}

/// 远端在 MCP 握手之前就失败了，从它 stderr 上那句话认出是哪一种。
///
/// ccnm 的写锁有两种拿不到（协议第 7 节），**给调用方的指示完全相反**：
///
/// - **busy**：别人正开着一个会话。等一会儿，或者改用只读。可重试。
/// - **unknown**：锁的状态说不清（锁文件坏了、上次的持有者被打断留下了
///   `held`）。协议原话是「人去看现场，不要重试到它"好了"」——因为"好了"
///   的另一种可能是两个 Agent 同时在改一棵树。**绝不标可重试。**
///
/// **只看消息的第一行。**ccnm 的 fixture 自己写着「第一行之后是给人的排查
/// 指引，措辞会变」：busy 那条后面跟着三行"去哪儿找持有者"，里面也带着
/// `held`、`guard` 这些词。在整段 stderr 上做包含匹配，ccnm 哪天改一句指引，
/// 一个 busy 就会被读成 unknown。
///
/// 认不出来按「连接断了、可以重试」走——那是原来的行为。但 unknown 的关键词
/// 优先匹配：把 unknown 说成可重试才是危险的那一边。
fn classify_startup_failure(stderr: &str) -> (&'static str, &'static str, bool) {
    let line = guard_message_line(stderr);
    if line.contains("state is unknown")
        || line.contains("state is incomplete or unknown")
        || line.contains("left held")
        || line.contains("refusing to transfer write authority")
    {
        return ("REMOTE_WRITE_LOCK_UNKNOWN", "runtime", false);
    }
    if line.contains("write guard is busy") {
        return ("REMOTE_WRITE_LOCK_BUSY", "runtime", true);
    }
    ("REMOTE_BRIDGE_CLOSED", "runtime", true)
}

/// ccnm 启动失败的 stderr 里，真正表示"哪一种失败"的那一行。
///
/// 形状是固定的（协议 11.2）：第一行是 `CCNM_E_*:` 这个名字，第二行是消息，
/// 再往后是给人看的排查步骤。所以取的是**跳过错误码名之后的第一行非空内容**。
fn guard_message_line(stderr: &str) -> String {
    stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        // `CCNM_E_POLICY:` 这类名字本身不带信息量，跳过去看下一行。
        .find(|line| !(line.starts_with("CCNM_E_") && line.ends_with(':')))
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// coding 会话调用失败。
fn coding_failure(tool_name: &str, member: &CcnmMember, error: CodingError) -> Value {
    let tool = tool_name;
    match error {
        CodingError::NoSuchSession => tool_err(WorkspaceError::ToolDetails {
            code: "REMOTE_CODING_HANDLE_UNKNOWN",
            message: format!("{tool} on {}: {error}", member.name),
            category: "validation",
            retryable: false,
            details: json!({ "workspace": remote_ref(member) }),
        }),
        // 同一个会话上还有调用在跑。这个可以重试——等前一个完事就行，
        // 跟"远端做没做不知道"完全是两回事。
        CodingError::Busy => tool_err(WorkspaceError::ToolDetails {
            code: "REMOTE_CODING_BUSY",
            message: format!("{tool} on {}: {error}", member.name),
            category: "runtime",
            retryable: true,
            details: json!({ "workspace": remote_ref(member) }),
        }),
        CodingError::SessionEnded => tool_err(WorkspaceError::ToolDetails {
            code: "REMOTE_CODING_SESSION_ENDED",
            message: format!("{tool} on {}: {error}", member.name),
            category: "runtime",
            // 可重试，但重试的是「重新 begin」，不是「再发一次这条」。
            retryable: false,
            details: json!({
                "workspace": remote_ref(member),
                "next": "remote_coding_begin"
            }),
        }),
        // 写操作断在半路：**远端做没做，这边不知道**。不标可重试——
        // 重发一次 apply_patch 或 exec_command 可能是第二次执行
        // （RFC-0002 5.4 / 验收项 H07）。
        // 远端没这个工具 / 不收这个参数：调用根本没发出去，远端什么都没做。
        // 绝不能报成 outcome unknown——那会让人去远端翻有没有半截的改动。
        CodingError::Peer(PeerError::Unsupported {
            tool,
            argument,
            remote,
        }) => remote_failure(
            tool_name,
            member,
            PeerError::Unsupported {
                tool,
                argument,
                remote,
            },
        ),
        CodingError::Peer(peer) => {
            let unknown = matches!(
                peer,
                PeerError::Timeout { .. } | PeerError::Closed { .. } | PeerError::Write(_)
            );
            if unknown {
                tool_err(WorkspaceError::ToolDetails {
                    code: "REMOTE_OUTCOME_UNKNOWN",
                    message: format!(
                        "{tool} on {} lost the connection mid-call, so whether the remote machine did it is unknown: {peer}. The coding session went with the connection, and so did anything it had running in the background there. Check the remote workspace before trying again; do not just resend.",
                        member.name
                    ),
                    category: "runtime",
                    retryable: false,
                    details: json!({
                        "workspace": remote_ref(member),
                        "outcome": "unknown",
                        "next": "remote_coding_begin, then look at the workspace"
                    }),
                })
            } else {
                remote_failure(tool, member, peer)
            }
        }
    }
}

/// 在远端结果上标一句「谁答的、什么模式」。
///
/// 放 `_meta` 不放 `structuredContent`：那是 ccnm 的契约，往里塞字段会跟
/// 它自己的键撞（验收项 H04 要求原样保留）。
fn tag_remote(mut result: Value, member: &CcnmMember, mode: Mode) -> Value {
    if let Some(object) = result.as_object_mut() {
        object.insert(
            "_meta".into(),
            json!({ "gld/workspace": remote_ref(member), "gld/mode": mode.as_str() }),
        );
    }
    result
}

/// 在结果里注明是哪个工作区答的。
///
/// 单独用 `hub_workspace` 这个键：`server_info`、`check_exec_environment` 的结果里
/// 已经有一个表示根目录路径的 `workspace`，覆盖掉就丢了信息。
fn tag_workspace(mut structured: Value, member: &WorkspaceProfile) -> Value {
    if let Some(object) = structured.as_object_mut() {
        object.insert("hub_workspace".into(), local_ref(member));
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
            Fixed {
                profiles: vec![api.clone(), web.clone(), outsider.clone()],
                remotes: Vec::new(),
                settings,
            },
        );
        Fixture {
            _dirs: vec![api_dir, web_dir, outsider_dir],
            hub,
            api,
            web,
            outsider,
        }
    }

    /// 测试里统一用这个主体；专门验"按主体分连接"的那条会换。
    fn caller() -> AuthContext {
        AuthContext::new(crate::auth::Principal::SharedSecret, HUB_SCOPE)
    }

    /// 一个具名 OAuth 客户端。两个不同的 `client_id` 是两个主体。
    fn oauth_client(id: &str) -> AuthContext {
        AuthContext::new(
            crate::auth::Principal::OAuthClient {
                client_id: id.into(),
            },
            HUB_SCOPE,
        )
    }

    fn call(hub: &Hub, tool: &str, arguments: Value) -> Value {
        call_as(hub, &caller(), tool, arguments)
    }

    /// 同上，但指定是谁在调。
    fn call_as(hub: &Hub, auth: &AuthContext, tool: &str, arguments: Value) -> Value {
        let (response, _) = hub.handle_request(
            auth,
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": tool, "arguments": arguments }
            }),
        );
        response["result"]["structuredContent"].clone()
    }

    /// 整个 MCP result，不只是 structuredContent——远端调用要看 content/isError。
    fn raw_call(hub: &Hub, tool: &str, arguments: Value) -> Value {
        let (response, _) = hub.handle_request(
            &caller(),
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": tool, "arguments": arguments }
            }),
        );
        response["result"].clone()
    }

    fn update_fixed(hub: &Hub, change: impl FnOnce(&mut Vec<WorkspaceProfile>, &mut AppSettings)) {
        let Members::Fixed(fixed) = &hub.members else {
            unreachable!("tests use fixed members")
        };
        let mut guard = fixed.lock().expect("fixed members");
        let fixed = &mut *guard;
        change(&mut fixed.profiles, &mut fixed.settings);
    }

    #[test]
    fn public_base_url_follows_the_gateway_only_when_it_is_enabled() {
        let mut settings = AppSettings::default();
        settings.hub.public_url = "https://hub.example.com/".into();
        assert_eq!(public_base_url(&settings), "https://hub.example.com");

        settings.hub.use_global_gateway = true;
        settings.global_gateway.public_url = "https://gw.example.com".into();
        // 入口没启用：走它也没用，退回手动地址。
        assert_eq!(public_base_url(&settings), "https://hub.example.com");

        settings.global_gateway.enabled = true;
        assert_eq!(public_base_url(&settings), "https://gw.example.com/hub");
    }

    /// 服务自己的隧道优先于老路子（全局入口、手填地址）。临时地址推不出来，
    /// 是空的——跑着的时候以运行时拿到的为准。
    #[test]
    fn the_services_own_tunnel_decides_the_public_address() {
        let mut settings = AppSettings::default();
        settings.hub.public_url = "https://fixed.example.com".into();
        settings.hub.use_global_gateway = true;
        settings.global_gateway.enabled = true;
        settings.global_gateway.public_url = "https://gw.example.com".into();

        settings.hub.tunnel_type = "cloudflare".into();
        settings.hub.cloudflare_mode = "quick".into();
        assert_eq!(public_base_url(&settings), "");

        settings.hub.cloudflare_mode = "named".into();
        assert_eq!(public_base_url(&settings), "https://fixed.example.com");

        settings.hub.tunnel_type = "frp".into();
        settings.hub.frp_profile_id = "p1".into();
        settings.hub.frp_subdomain = "mcp".into();
        settings.frp_profiles.push(crate::settings::FrpProfile {
            id: "p1".into(),
            name: "公司".into(),
            server: "frp.example.com".into(),
            server_port: 7000,
        });
        assert_eq!(public_base_url(&settings), "https://mcp.frp.example.com");
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

    /// 客户端拿到的工具表 = 注册表那一档，去掉 hub 隐藏的、每个加上 workspace，
    /// 逐个工具、逐个参数对得上；server_info 报的指纹就是这一份（审查 D04）。
    ///
    /// 审查时客户端看不到 check_command 等四个工具和几个参数，查下来服务端每一层
    /// 都给了——是客户端缓存了旧表。这条把"服务端确实给了"钉住，再出现同样的
    /// 现象，先怀疑客户端。
    #[test]
    fn the_served_tool_list_matches_the_registry_and_server_info_reports_it() {
        let fixture = fixture();
        let served = fixture.hub.list_tools();
        let params = |tool: &Value| {
            let mut names: Vec<String> = tool["inputSchema"]["properties"]
                .as_object()
                .map(|properties| properties.keys().cloned().collect())
                .unwrap_or_default();
            names.sort();
            names
        };
        for tool in list_tools_for_profile(&fixture.hub.tool_profile) {
            let name = tool["name"].as_str().expect("name");
            let offered = served.iter().find(|offered| offered["name"] == name);
            if HIDDEN_TOOLS.contains(&name) {
                assert!(offered.is_none(), "{name} 应该被 hub 隐藏");
                continue;
            }
            let offered = offered.unwrap_or_else(|| panic!("hub 没给 {name}"));
            let mut expected = params(&tool);
            expected.push("workspace".into());
            expected.sort();
            assert_eq!(params(offered), expected, "{name} 的参数和注册表对不上");
        }

        let digest = crate::tools::registry::surface_digest(&served);
        for (tool, param) in [
            ("check_command", "cmd"),
            ("list_skills", "workspace"),
            ("get_skill", "name"),
            ("read_notebook", "path"),
            ("read_file", "start_byte"),
            ("apply_patch", "expected_versions"),
            ("apply_patch", "notebook_edits"),
            ("exec_command", "argv"),
            ("exec_command", "stdin_mode"),
        ] {
            let listed = digest["tools"][tool]
                .as_array()
                .unwrap_or_else(|| panic!("服务端没给 {tool}"));
            assert!(listed.contains(&json!(param)), "{tool} 没有 {param}");
        }

        let info = call(&fixture.hub, "server_info", json!({ "workspace": "api" }));
        assert_eq!(info["ok"], true, "{info}");
        assert_eq!(
            info["connection"]["tools_fingerprint"], digest["tools_fingerprint"],
            "{info}"
        );
        assert_eq!(info["connection"]["tool_count"], served.len());
        assert!(info.get("build_commit").is_some(), "{info}");
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

    /// 同一个工作区上，另一个客户端拿着你的 `session_id` 什么也读不到。
    ///
    /// hub 是所有对话、所有客户端共用的一条连接，成员上下文也是按成员 id 缓存
    /// 的。会话表要是挂在上下文上，这里两个客户端看到的就是同一张表——B 拿到
    /// A 的 `session_id`（它就在 A 的工具返回里，模型之间转述一句就有了）就能
    /// 读 A 的命令输出。所以会话按目录 + 主体分表，这条钉的就是那件事。
    #[test]
    fn one_client_cannot_read_another_clients_command_session() {
        let fixture = fixture();
        let (alpha, beta) = (oauth_client("alpha"), oauth_client("beta"));

        let started = call_as(
            &fixture.hub,
            &alpha,
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

        let stolen = call_as(
            &fixture.hub,
            &beta,
            "read_output",
            json!({ "workspace": "api", "output_ref": output_ref }),
        );
        // 报的是"查无此 id"而不是"不许读"：另一个客户端连"这条会话存在"
        // 都不该知道。
        assert_eq!(stolen["error"]["code"], "SESSION_NOT_FOUND", "{stolen}");

        let stolen_kill = call_as(
            &fixture.hub,
            &beta,
            "kill_session",
            json!({ "workspace": "api", "session_id": session_id }),
        );
        assert_eq!(
            stolen_kill["error"]["code"], "SESSION_NOT_FOUND",
            "别人的命令被停掉了：{stolen_kill}"
        );

        let mine = call_as(
            &fixture.hub,
            &alpha,
            "read_output",
            json!({ "workspace": "api", "output_ref": output_ref }),
        );
        assert_eq!(mine["ok"], true, "自己的会话读不到了：{mine}");

        let killed = call_as(
            &fixture.hub,
            &alpha,
            "kill_session",
            json!({ "workspace": "api", "session_id": session_id }),
        );
        assert_eq!(killed["ok"], true, "{killed}");
    }

    /// 配置一变，hub 就按新配置重建成员上下文。正在跑的命令不该被这件事顺手
    /// 杀掉，`session_id` 也得还认。
    ///
    /// 以前是杀的，理由是"会话表跟着旧上下文一起没了，不收就是孤儿进程"。
    /// 现在表归目录 + 主体所有，那个理由不成立了，而代价一直都在：改一行全局
    /// AI 说明就会重建上下文，正在跑的 `npm run dev` 跟着被杀。
    #[test]
    fn rebuilding_a_member_context_keeps_its_running_commands() {
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
        let before = fixture
            .hub
            .contexts
            .lock()
            .expect("contexts")
            .get(&fixture.api.id)
            .map(|cached| Arc::as_ptr(&cached.context))
            .expect("api 的上下文");

        // 指纹里有全局 AI 说明，改它就会让下一次调用重建上下文。
        update_fixed(&fixture.hub, |_, settings| {
            settings.global_ai_instructions = "改了一行说明".into();
        });
        let after_change = call(
            &fixture.hub,
            "read_output",
            json!({ "workspace": "api", "output_ref": output_ref }),
        );
        assert_eq!(
            after_change["ok"], true,
            "重建上下文把正在跑的命令连输出一起弄没了：{after_change}"
        );
        let after = fixture
            .hub
            .contexts
            .lock()
            .expect("contexts")
            .get(&fixture.api.id)
            .map(|cached| Arc::as_ptr(&cached.context))
            .expect("api 的上下文");
        assert_ne!(
            before, after,
            "上下文根本没重建，这条测试没测到它该测的东西"
        );

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

        let (initialized, _) = fixture.hub.handle_request(
            &caller(),
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }),
        );
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
        let local = |index: usize| match &members[index] {
            Member::Local(profile) => profile,
            Member::Remote(_) => unreachable!("这个 fixture 里都是本地成员"),
        };
        let api = local(0);
        let web = local(1);
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

    // ---- 远端成员 ----

    /// 合成的远端 ccnm：握手照答，工具调用把收到的东西原样记下来。
    ///
    /// 整套远端测试都不起子进程、不碰 SSH——验收项 H1 要的就是「用合成 stdio
    /// peer 验证」。
    #[derive(Clone)]
    struct RemoteSpy {
        /// 每次 `tools/call` 收到的 params，用来验参数白名单真的生效了。
        calls: Arc<Mutex<Vec<Value>>>,
        opens: Arc<Mutex<usize>>,
        coding_opens: Arc<Mutex<usize>>,
        closes: Arc<Mutex<usize>>,
        /// 让远端工具自己说「这次没成」，验 `isError` 不被外层吞掉。
        tool_fails: bool,
        /// 连 bridge 都起不来，验错误分类。
        cannot_start: bool,
        /// 工具调用一律不回，验"断在半路"。
        black_hole: bool,
        /// 最多允许开几条 coding 连接，之后一律报远端写锁被占着。
        /// `usize::MAX` = 不限制。
        coding_opens_allowed: usize,
        /// 每次工具调用先睡这么久，用来制造"上一个还在跑"。
        call_delay_ms: u64,
        /// 这个远端报自己有哪些工具（gld 握手后问一次）。默认是和白名单
        /// 同代的那份；装老版本 ccnm 时把它删几项。
        offers: Arc<Mutex<Vec<Value>>>,
        /// 工具调用的结果。默认是一段文本；验图片透传时换成带 image 块的。
        tool_result: Arc<Mutex<Option<Value>>>,
    }

    impl RemoteSpy {
        fn new() -> Self {
            RemoteSpy {
                calls: Arc::new(Mutex::new(Vec::new())),
                opens: Arc::new(Mutex::new(0)),
                coding_opens: Arc::new(Mutex::new(0)),
                closes: Arc::new(Mutex::new(0)),
                tool_fails: false,
                cannot_start: false,
                black_hole: false,
                coding_opens_allowed: usize::MAX,
                call_delay_ms: 0,
                offers: Arc::new(Mutex::new(crate::bridge::tools::offered_by_current_ccnm())),
                tool_result: Arc::new(Mutex::new(None)),
            }
        }

        /// 装一个老版本的远端：这些工具它没有。
        fn without_tools(self, missing: &[&str]) -> Self {
            {
                let mut offers = self.offers.lock().expect("offers");
                offers.retain(|tool| !missing.contains(&tool["name"].as_str().unwrap_or_default()));
            }
            self
        }

        /// 装一个老版本的远端：这个工具它不收这个参数。
        fn without_argument(self, tool: &str, argument: &str) -> Self {
            {
                let mut offers = self.offers.lock().expect("offers");
                for offered in offers.iter_mut() {
                    if offered["name"] == json!(tool) {
                        offered["inputSchema"]["properties"]
                            .as_object_mut()
                            .expect("properties")
                            .remove(argument);
                    }
                }
            }
            self
        }

        /// 远端这次回什么（验结果原样透传）。
        fn answering(self, result: Value) -> Self {
            *self.tool_result.lock().expect("tool result") = Some(result);
            self
        }
        fn calls(&self) -> Vec<Value> {
            self.calls.lock().expect("calls").clone()
        }
        fn opens(&self) -> usize {
            *self.opens.lock().expect("opens")
        }
        fn closes(&self) -> usize {
            *self.closes.lock().expect("closes")
        }
    }

    /// 冒充一条起不来的 bridge：进程退了，stderr 上留下 ccnm 的诊断。
    ///
    /// 这正是真 ccnm 抢不到写锁时的样子——失败发生在 MCP 握手之前，所以它
    /// 是启动失败，不是工具结果（协议第 7 节）。
    struct RefusedTransport {
        stderr: &'static str,
    }

    impl crate::bridge::peer::Transport for RefusedTransport {
        fn send_line(&mut self, _line: &str) -> std::io::Result<()> {
            Ok(())
        }
        fn recv_line(
            &mut self,
            _timeout: std::time::Duration,
        ) -> Result<Option<String>, std::sync::mpsc::RecvTimeoutError> {
            Ok(None) // 管道关了
        }
        fn stderr_tail(&self) -> String {
            self.stderr.to_string()
        }
    }

    struct SpyTransport {
        spy: RemoteSpy,
        pending: Option<String>,
    }

    /// 合成通道等回复的最长睡眠。比调用预算短，测试不会真的等 60 秒。
    const SPY_WAIT: std::time::Duration = std::time::Duration::from_millis(20);

    impl crate::bridge::peer::Transport for SpyTransport {
        fn send_line(&mut self, line: &str) -> std::io::Result<()> {
            let request: Value = serde_json::from_str(line).expect("请求是 JSON");
            let Some(id) = request.get("id").cloned() else {
                return Ok(());
            };
            let result = match request["method"].as_str().unwrap_or("") {
                "initialize" => json!({
                    "protocolVersion": crate::bridge::peer::PROTOCOL_VERSION,
                    "serverInfo": { "name": "ccnm", "version": "0.7.0" }
                }),
                // 握手之后 gld 问一次远端有哪些工具。**不记进 calls**：
                // 那里数的是工具调用。
                "tools/list" => json!({ "tools": self.spy.offers.lock().expect("offers").clone() }),
                _ => {
                    self.spy
                        .calls
                        .lock()
                        .expect("calls")
                        .push(request["params"].clone());
                    if self.spy.call_delay_ms > 0 {
                        std::thread::sleep(std::time::Duration::from_millis(
                            self.spy.call_delay_ms,
                        ));
                    }
                    if self.spy.black_hole {
                        return Ok(()); // 什么都不回，让调用方超时
                    }
                    if let Some(answer) = self.spy.tool_result.lock().expect("tool result").clone()
                    {
                        answer
                    } else if self.spy.tool_fails {
                        json!({
                            "content": [{ "type": "text", "text": "CCNM_E_PATH_OUTSIDE_ROOT" }],
                            "isError": true
                        })
                    } else {
                        json!({
                            "content": [{ "type": "text", "text": "line 1 of the remote file" }],
                            "structuredContent": { "ok": true, "lines": 1 },
                            "isError": false
                        })
                    }
                }
            };
            self.pending =
                Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string());
            Ok(())
        }

        fn recv_line(
            &mut self,
            timeout: std::time::Duration,
        ) -> Result<Option<String>, std::sync::mpsc::RecvTimeoutError> {
            match self.pending.take() {
                Some(line) => Ok(Some(line)),
                None => {
                    std::thread::sleep(timeout.min(SPY_WAIT));
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                }
            }
        }

        fn shutdown(&mut self, _grace: std::time::Duration) {
            *self.spy.closes.lock().expect("closes") += 1;
        }
    }

    impl Open for RemoteSpy {
        fn open(
            &self,
            _member: &CcnmMember,
            _mode: Mode,
        ) -> Result<Box<dyn crate::bridge::peer::Transport>, PeerError> {
            if self.cannot_start {
                return Err(PeerError::Spawn {
                    program: "ccnm".into(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "no such file or directory",
                    ),
                });
            }
            if _mode == Mode::Coding {
                let mut opened = self.coding_opens.lock().expect("coding opens");
                if *opened >= self.coding_opens_allowed {
                    // 写锁被别人占着：进程起得来，但握手之前就退了。
                    return Ok(Box::new(RefusedTransport {
                        stderr: FIXTURE_GUARD_BUSY,
                    }));
                }
                *opened += 1;
            }
            *self.opens.lock().expect("opens") += 1;
            Ok(Box::new(SpyTransport {
                spy: self.clone(),
                pending: None,
            }))
        }
    }

    struct RemoteFixture {
        _dirs: Vec<tempfile::TempDir>,
        hub: Hub,
        api: WorkspaceProfile,
        remote: CcnmMember,
        spy: RemoteSpy,
    }

    /// 一个本地成员 api，一个远端成员 prod（只读）。
    fn remote_fixture(spy: RemoteSpy) -> RemoteFixture {
        remote_fixture_with_mode(spy, Mode::Read)
    }

    /// 远端成员上限设成 coding。
    fn coding_fixture(spy: RemoteSpy) -> RemoteFixture {
        remote_fixture_with_mode(spy, Mode::Coding)
    }

    fn remote_fixture_with_mode(spy: RemoteSpy, max_mode: Mode) -> RemoteFixture {
        crate::home::isolate_for_tests();
        let (api_dir, api) = workspace("api", &[("only-api.txt", "api secret\n")]);
        let remote = CcnmMember {
            id: "remote-prod".into(),
            name: "prod".into(),
            ccnm_bin: "ccnm".into(),
            node: "work".into(),
            workspace: "server".into(),
            max_mode,
        };
        let mut settings = AppSettings::default();
        settings.hub.members = vec![api.id.clone(), remote.id.clone()];
        let config = settings.hub.clone();
        let hub = Hub::fixed_with_opener(
            &config,
            Fixed {
                profiles: vec![api.clone()],
                remotes: vec![remote.clone()],
                settings,
            },
            Box::new(spy.clone()),
        );
        RemoteFixture {
            _dirs: vec![api_dir],
            hub,
            api,
            remote,
            spy,
        }
    }

    /// 远端工具只在 hub 真有远端成员时才列出来。
    #[test]
    fn the_remote_tools_show_up_only_when_a_remote_member_is_configured() {
        let names = |hub: &Hub| -> Vec<String> {
            hub.list_tools()
                .iter()
                .filter_map(|tool| tool["name"].as_str().map(str::to_string))
                .collect()
        };

        let only_local = fixture();
        assert!(
            !names(&only_local.hub)
                .iter()
                .any(|name| name.starts_with("remote_")),
            "没有远端成员却列了远端工具：{:?}",
            names(&only_local.hub)
        );

        let with_remote = remote_fixture(RemoteSpy::new());
        let listed = names(&with_remote.hub);
        for expected in remote_tool_names() {
            assert!(listed.contains(&expected.to_string()), "{listed:?}");
        }
        // 本地工具一个没少。
        assert!(listed.contains(&"read_file".to_string()), "{listed:?}");
    }

    /// 远端成员的调用经 bridge 转发，结果**原样**回来。
    #[test]
    fn a_remote_call_goes_through_the_bridge_and_comes_back_unchanged() {
        let fixture = remote_fixture(RemoteSpy::new());
        let result = raw_call(
            &fixture.hub,
            "remote_read_file",
            json!({ "workspace": "prod", "path": "src/main.rs", "max_lines": 20 }),
        );

        assert_eq!(fixture.spy.opens(), 1, "该开一条 bridge");
        let calls = fixture.spy.calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        // 转发的是 ccnm 的工具名，不是 hub 对外那个带前缀的。
        assert_eq!(calls[0]["name"], json!("read_file"));
        assert_eq!(
            calls[0]["arguments"],
            json!({ "path": "src/main.rs", "max_lines": 20 }),
            "路由字段 workspace 不该跟着出去"
        );

        // 远端说什么就是什么：content 和 structuredContent 一个字不改。
        assert_eq!(
            result["content"][0]["text"],
            json!("line 1 of the remote file")
        );
        assert_eq!(
            result["structuredContent"],
            json!({ "ok": true, "lines": 1 })
        );
        assert_eq!(result["isError"], json!(false));
        // 是谁答的放在 _meta，不塞进 ccnm 的结构化结果里。
        assert_eq!(result["_meta"]["gld/workspace"]["id"], json!("remote-prod"));
        assert_eq!(result["_meta"]["gld/mode"], json!("read"));
    }

    /// 远端工具自己报的错是工具的话，不是连接出问题，外层不能改写成成功，
    /// 也不能翻译成 hub 自己的错误码。
    #[test]
    fn an_error_from_the_remote_tool_itself_is_passed_through() {
        let mut spy = RemoteSpy::new();
        spy.tool_fails = true;
        let fixture = remote_fixture(spy);

        let result = raw_call(
            &fixture.hub,
            "remote_read_file",
            json!({ "workspace": "prod", "path": "../../etc/passwd" }),
        );
        assert_eq!(result["isError"], json!(true), "{result}");
        assert_eq!(
            result["content"][0]["text"],
            json!("CCNM_E_PATH_OUTSIDE_ROOT"),
            "远端的诊断被改写了：{result}"
        );
    }

    /// 本地工具用在远端成员上：明确拒绝，**不在本机执行**。
    ///
    /// gld 本机也有 `search_text`、`list_files`，跟 ccnm 的同名工具不是一个
    /// 契约——悄悄在本机跑一遍，模型拿到的是另一台机器的答案。
    #[test]
    fn a_local_tool_aimed_at_a_remote_member_is_refused_and_runs_nowhere() {
        let fixture = remote_fixture(RemoteSpy::new());
        for tool in ["read_file", "search_text", "list_files", "exec_command"] {
            let refused = call(
                &fixture.hub,
                tool,
                json!({ "workspace": "prod", "path": "only-api.txt", "query": "x", "cmd": "id" }),
            );
            assert_eq!(
                refused["error"]["code"], "TOOL_IS_LOCAL_ONLY",
                "{tool}: {refused}"
            );
            // 报错要告诉模型该用哪个。
            assert!(
                refused["error"]["details"]["remote_tools"]
                    .to_string()
                    .contains("remote_read_file"),
                "{refused}"
            );
        }
        assert_eq!(fixture.spy.opens(), 0, "既没本地跑，也不该往远端转");
        assert!(
            !fixture
                .hub
                .contexts
                .lock()
                .expect("contexts")
                .contains_key(&fixture.remote.id),
            "远端成员不该有本机工具上下文"
        );
    }

    /// 远端工具用在本地成员上：一样拒绝，不往任何 bridge 转。
    #[test]
    fn a_remote_tool_aimed_at_a_local_member_is_refused_and_not_forwarded() {
        let fixture = remote_fixture(RemoteSpy::new());
        let refused = call(
            &fixture.hub,
            "remote_read_file",
            json!({ "workspace": "api", "path": "only-api.txt" }),
        );
        assert_eq!(
            refused["error"]["code"], "TOOL_IS_FOR_REMOTE_WORKSPACES",
            "{refused}"
        );
        assert_eq!(fixture.spy.opens(), 0, "本地成员不该开 bridge");
        assert!(
            !refused.to_string().contains("api secret"),
            "不该读到文件内容：{refused}"
        );
    }

    /// 远端成员不产生任何本机工作区状态：没有 ToolContext，也没有 exec 会话表。
    /// 这是验收项 H01 的后半句。
    #[test]
    fn a_remote_member_leaves_no_local_workspace_state() {
        let fixture = remote_fixture(RemoteSpy::new());
        let (response, routed) = fixture.hub.handle_request(
            &caller(),
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "remote_workspace_info",
                    "arguments": { "workspace": "prod" }
                }
            }),
        );
        assert_eq!(response["result"]["isError"], json!(false), "{response}");
        let routed = routed.expect("该记下落到了哪个成员");
        assert_eq!(routed.workspace_id, "remote-prod");
        assert!(routed.context.is_none(), "远端不该带本机工具上下文");
        assert!(fixture.hub.contexts.lock().expect("contexts").is_empty());
    }

    /// 少了必填参数在 hub 这边就拦下，不白开一条 bridge。
    #[test]
    fn a_remote_call_missing_a_required_argument_never_reaches_the_bridge() {
        let fixture = remote_fixture(RemoteSpy::new());
        let refused = call(
            &fixture.hub,
            "remote_read_file",
            json!({ "workspace": "prod" }),
        );
        assert_eq!(refused["error"]["code"], "MISSING_ARGUMENT", "{refused}");
        assert!(refused["error"]["details"]["missing"]
            .to_string()
            .contains("path"));
        assert_eq!(fixture.spy.opens(), 0);
    }

    /// bridge 起不来要说清是哪一种失败：ccnm 没装和网络抖了一下，
    /// 操作员要去看的地方完全不同。
    #[test]
    fn a_bridge_that_cannot_start_is_reported_as_such() {
        let mut spy = RemoteSpy::new();
        spy.cannot_start = true;
        let fixture = remote_fixture(spy);

        let failed = call(
            &fixture.hub,
            "remote_read_file",
            json!({ "workspace": "prod", "path": "a.rs" }),
        );
        assert_eq!(
            failed["error"]["code"], "REMOTE_BRIDGE_NOT_STARTED",
            "{failed}"
        );
        assert_eq!(failed["error"]["retryable"], json!(false), "{failed}");
        assert!(
            failed["summary"].as_str().unwrap_or("").contains("ccnm"),
            "得说出是哪个程序没起来：{failed}"
        );
    }

    /// list_workspaces 要分清哪些在别的机器上，**不给远端编一个本机路径**。
    #[test]
    fn list_workspaces_marks_the_remote_ones_and_invents_no_path() {
        let fixture = remote_fixture(RemoteSpy::new());
        let listed = call(&fixture.hub, LIST_WORKSPACES, json!({}));
        let workspaces = listed["workspaces"].as_array().expect("workspaces");
        assert_eq!(workspaces.len(), 2, "{listed}");

        let local = &workspaces[0];
        assert_eq!(local["kind"], json!("local"));
        assert_eq!(local["path"], json!(fixture.api.path));

        let remote = &workspaces[1];
        assert_eq!(remote["kind"], json!("remote"));
        assert_eq!(remote["mode"], json!("read"));
        assert!(remote.get("path").is_none(), "远端不该有 path：{remote}");
        assert!(
            remote["tools"].to_string().contains("remote_read_file"),
            "{remote}"
        );
        // 对面机器上的节点名和 workspace 名不外报，模型路由只需要 id。
        assert!(!listed.to_string().contains("server"), "{listed}");
    }

    /// 一条 bridge 属于某个主体，不属于某个工作区名字（RFC-0002 5.3）。
    /// 换个 OAuth 客户端来调同一个远端成员，拿到的是另一条连接。
    #[test]
    fn two_principals_do_not_share_one_bridge() {
        let fixture = remote_fixture(RemoteSpy::new());
        let as_client = |client: &str| {
            let auth = AuthContext::new(
                crate::auth::Principal::OAuthClient {
                    client_id: client.into(),
                },
                HUB_SCOPE,
            );
            let (response, _) = fixture.hub.handle_request(
                &auth,
                &json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": {
                        "name": "remote_workspace_info",
                        "arguments": { "workspace": "prod" }
                    }
                }),
            );
            assert_eq!(response["result"]["isError"], json!(false), "{response}");
        };

        as_client("chatgpt");
        assert_eq!(fixture.spy.opens(), 1);
        as_client("chatgpt");
        assert_eq!(fixture.spy.opens(), 1, "同一个客户端该复用");
        as_client("claude");
        assert_eq!(fixture.spy.opens(), 2, "另一个客户端复用了别人的 bridge");
    }

    /// 成员被移出 hub：下一次请求就断掉它的 bridge，不等空闲到期。
    #[test]
    fn removing_a_remote_member_closes_its_bridge() {
        let fixture = remote_fixture(RemoteSpy::new());
        let ok = raw_call(
            &fixture.hub,
            "remote_workspace_info",
            json!({ "workspace": "prod" }),
        );
        assert_eq!(ok["isError"], json!(false), "{ok}");
        assert_eq!(fixture.spy.opens(), 1);

        let remote_id = fixture.remote.id.clone();
        update_fixed(&fixture.hub, |_, settings| {
            settings.hub.members.retain(|id| id != &remote_id);
        });

        let after = call(
            &fixture.hub,
            "remote_workspace_info",
            json!({ "workspace": "prod" }),
        );
        assert_eq!(after["error"]["code"], "WORKSPACE_NOT_IN_HUB", "{after}");
        assert_eq!(fixture.spy.closes(), 1, "摘掉了成员却没断连接");
    }

    // ---- 远端 coding ----

    /// 开一个 coding 会话，返回句柄。
    fn begin(fixture: &RemoteFixture) -> String {
        let out = call(
            &fixture.hub,
            "remote_coding_begin",
            json!({ "workspace": "prod" }),
        );
        assert_eq!(out["ok"], true, "{out}");
        out["coding_handle"].as_str().expect("句柄").to_string()
    }

    /// 远端那台机器上的 ccnm 比这份名单老：**在 gld 这边就拒**，说清楚是
    /// 版本的事，而且调用一个字节都没发出去。
    #[test]
    fn a_remote_ccnm_that_is_too_old_is_named_not_guessed_at() {
        let fixture = coding_fixture(RemoteSpy::new().without_tools(&["stop_command"]));
        let handle = begin(&fixture);
        let refused = raw_call(
            &fixture.hub,
            "remote_stop_command",
            json!({ "workspace": "prod", "coding_handle": handle, "output_ref": "r-0000" }),
        );
        let text = refused["content"][0]["text"].as_str().unwrap_or_default();
        assert_eq!(
            refused["structuredContent"]["error"]["code"], "REMOTE_TOOL_UNSUPPORTED",
            "{refused}"
        );
        assert_eq!(
            refused["structuredContent"]["error"]["retryable"],
            json!(false),
            "重试也没用，要升级远端的 ccnm：{refused}"
        );
        assert!(text.contains("Upgrade ccnm"), "{text}");
        assert!(
            fixture.spy.calls().is_empty(),
            "不该发出去：{:?}",
            fixture.spy.calls()
        );

        // 会话还在：被拒的那次调用没碰传输层，句柄不该跟着作废。
        let ok = raw_call(
            &fixture.hub,
            "remote_exec_command",
            json!({ "workspace": "prod", "coding_handle": handle, "cmd": ["true"] }),
        );
        assert_eq!(ok["isError"], json!(false), "{ok}");
    }

    /// 老版本 ccnm 会**悄悄忽略**不认识的参数（它的参数结构体不拒绝未知字段），
    /// 于是 `run_in_background` 等于没写、命令在前台跑满 timeout，而模型以为
    /// 自己起了一个后台命令。所以这种参数也在这边拒，并且不能报成
    /// `REMOTE_OUTCOME_UNKNOWN`——远端什么都没做，没有半截状态要人去核对。
    #[test]
    fn an_argument_an_older_remote_would_ignore_is_refused_here() {
        let fixture =
            coding_fixture(RemoteSpy::new().without_argument("exec_command", "run_in_background"));
        let handle = begin(&fixture);
        let refused = raw_call(
            &fixture.hub,
            "remote_exec_command",
            json!({ "workspace": "prod", "coding_handle": handle,
                    "cmd": ["npm", "run", "dev"], "run_in_background": true }),
        );
        assert_eq!(
            refused["structuredContent"]["error"]["code"], "REMOTE_TOOL_UNSUPPORTED",
            "{refused}"
        );
        assert_ne!(
            refused["structuredContent"]["error"]["code"], "REMOTE_OUTCOME_UNKNOWN",
            "没发出去就不是'做没做不知道'"
        );
        assert!(fixture.spy.calls().is_empty());
    }

    /// `wait_ms` 超过 hub 自己的调用预算：在这边拒，不替它改小。
    ///
    /// 改小的话调用方会以为自己等满了；而真发出去的话，这次调用会在 60 秒
    /// 被当成传输层出问题，连接一丢，远端 ccnm 会把这个会话起的后台命令
    /// 一起停掉——模型等于自己把要等的那个命令弄没了。
    #[test]
    fn a_wait_longer_than_the_call_budget_is_refused_before_it_is_sent() {
        let fixture = coding_fixture(RemoteSpy::new());
        let handle = begin(&fixture);
        let refused = raw_call(
            &fixture.hub,
            "remote_read_output",
            json!({ "workspace": "prod", "coding_handle": handle,
                    "output_ref": "r-0000", "wait_ms": 600_000 }),
        );
        assert_eq!(
            refused["structuredContent"]["error"]["code"], "ARGUMENT_OUT_OF_RANGE",
            "{refused}"
        );
        assert_eq!(
            refused["structuredContent"]["error"]["details"]["max"],
            json!(crate::bridge::tools::MAX_WAIT_MS)
        );
        assert!(fixture.spy.calls().is_empty(), "不该发出去");

        // 预算之内的照常发，参数原样带过去。
        let ok = raw_call(
            &fixture.hub,
            "remote_read_output",
            json!({ "workspace": "prod", "coding_handle": handle,
                    "output_ref": "r-0000", "wait_ms": 5_000 }),
        );
        assert_eq!(ok["isError"], json!(false), "{ok}");
        let calls = fixture.spy.calls();
        assert_eq!(calls[0]["arguments"]["wait_ms"], json!(5_000), "{calls:?}");
    }

    /// 前台 `exec_command` 不给期限时，hub 替它填一个自己撑得住的。
    ///
    /// **这是唯一一处替调用方改参数**，因为不改必踩：ccnm 那边不给 `timeout_ms`
    /// 就是 120 秒，比 hub 的 60 秒调用预算长一倍。真让它跑满，这次调用会在
    /// 60 秒被当成传输层出问题，连接一丢，远端把这个会话起的**所有后台命令**
    /// 一起停掉——出事的是前台这条，陪葬的是后台那些，而模型只看到一句超时。
    #[test]
    fn a_foreground_command_gets_a_deadline_the_hub_can_wait_out() {
        let fixture = coding_fixture(RemoteSpy::new());
        let handle = begin(&fixture);

        let ok = raw_call(
            &fixture.hub,
            "remote_exec_command",
            json!({ "workspace": "prod", "coding_handle": handle, "cmd": ["cargo", "test"] }),
        );
        assert_eq!(ok["isError"], json!(false), "{ok}");
        assert_eq!(
            fixture.spy.calls()[0]["arguments"]["timeout_ms"],
            json!(crate::bridge::tools::MAX_FOREGROUND_TIMEOUT_MS),
            "前台没给期限就该填上 hub 撑得住的那个"
        );

        // 自己给的、撑得住的期限原样带过去，一个字不改。
        let ok = raw_call(
            &fixture.hub,
            "remote_exec_command",
            json!({ "workspace": "prod", "coding_handle": handle,
                    "cmd": ["cargo", "test"], "timeout_ms": 3_000 }),
        );
        assert_eq!(ok["isError"], json!(false), "{ok}");
        assert_eq!(
            fixture.spy.calls()[1]["arguments"]["timeout_ms"],
            json!(3_000)
        );

        // 后台命令不管：调用立刻返回，期限是那条命令自己的事。
        let ok = raw_call(
            &fixture.hub,
            "remote_exec_command",
            json!({ "workspace": "prod", "coding_handle": handle,
                    "cmd": ["npm", "run", "dev"], "run_in_background": true }),
        );
        assert_eq!(ok["isError"], json!(false), "{ok}");
        assert_eq!(
            fixture.spy.calls()[2]["arguments"].get("timeout_ms"),
            None,
            "后台调用不该被塞一个期限进去"
        );
    }

    /// 前台要一个比 hub 调用预算还长的期限：在这边拒，并指路 `run_in_background`。
    ///
    /// 不替它改小——那和 `wait_ms` 一样，改小它会以为自己拿到了那么久。
    #[test]
    fn a_foreground_deadline_past_the_call_budget_is_refused_and_says_what_to_do() {
        let fixture = coding_fixture(RemoteSpy::new());
        let handle = begin(&fixture);
        let refused = raw_call(
            &fixture.hub,
            "remote_exec_command",
            json!({ "workspace": "prod", "coding_handle": handle,
                    "cmd": ["cargo", "build", "--release"], "timeout_ms": 600_000 }),
        );
        assert_eq!(
            refused["structuredContent"]["error"]["code"], "ARGUMENT_OUT_OF_RANGE",
            "{refused}"
        );
        assert_eq!(
            refused["structuredContent"]["error"]["details"]["max"],
            json!(crate::bridge::tools::MAX_FOREGROUND_TIMEOUT_MS)
        );
        let text = refused["structuredContent"]["error"]["message"]
            .as_str()
            .unwrap_or_default();
        assert!(text.contains("run_in_background"), "{text}");
        assert!(fixture.spy.calls().is_empty(), "不该发出去");

        // 同样长的期限，后台要就给：那条调用立刻返回，撞不上 hub 的预算。
        let ok = raw_call(
            &fixture.hub,
            "remote_exec_command",
            json!({ "workspace": "prod", "coding_handle": handle,
                    "cmd": ["cargo", "build", "--release"],
                    "timeout_ms": 600_000, "run_in_background": true }),
        );
        assert_eq!(ok["isError"], json!(false), "{ok}");
        assert_eq!(
            fixture.spy.calls()[0]["arguments"]["timeout_ms"],
            json!(600_000)
        );
    }

    /// 远端返回的图片块原样回来：`remote_view_image` 和 `remote_read_notebook`
    /// 靠它，而 H04 要的就是 content 一个字不改。
    #[test]
    fn an_image_from_the_remote_comes_back_as_an_image() {
        let picture = json!({
            "content": [
                { "type": "text", "text": "shots/red.png: PNG, 73 bytes" },
                { "type": "image", "data": "iVBORw0KGgo=", "mimeType": "image/png" }
            ],
            "isError": false
        });
        let fixture = remote_fixture(RemoteSpy::new().answering(picture.clone()));
        let result = raw_call(
            &fixture.hub,
            "remote_view_image",
            json!({ "workspace": "prod", "path": "shots/red.png" }),
        );
        assert_eq!(result["content"], picture["content"], "{result}");
        assert_eq!(fixture.spy.calls()[0]["name"], json!("view_image"));
    }

    /// 只读成员不该看到任何 coding 工具——列出来只会引着模型去试。
    #[test]
    fn a_read_only_remote_member_is_shown_no_coding_tool() {
        let names = |f: &RemoteFixture| -> Vec<String> {
            f.hub
                .list_tools()
                .iter()
                .filter_map(|t| t["name"].as_str().map(str::to_string))
                .collect()
        };

        let read_only = names(&remote_fixture(RemoteSpy::new()));
        assert!(
            read_only.contains(&"remote_read_file".into()),
            "{read_only:?}"
        );
        for forbidden in [
            "remote_coding_begin",
            "remote_apply_patch",
            "remote_exec_command",
        ] {
            assert!(
                !read_only.contains(&forbidden.into()),
                "{forbidden} 不该列出来"
            );
        }

        let coding = names(&coding_fixture(RemoteSpy::new()));
        for expected in [
            "remote_coding_begin",
            "remote_coding_end",
            "remote_apply_patch",
            "remote_exec_command",
            "remote_read_output",
            "remote_stop_command",
        ] {
            assert!(coding.contains(&expected.into()), "少了 {expected}");
        }
    }

    /// 只读成员上硬调 begin：拒绝，并告诉操作员怎么放开。
    #[test]
    fn a_read_only_member_refuses_to_open_a_writing_session() {
        let fixture = remote_fixture(RemoteSpy::new());
        let out = call(
            &fixture.hub,
            "remote_coding_begin",
            json!({ "workspace": "prod" }),
        );
        assert_eq!(out["error"]["code"], "REMOTE_IS_READ_ONLY", "{out}");
        assert!(
            out["summary"]
                .as_str()
                .unwrap_or("")
                .contains("--mode coding"),
            "得说清楚操作员怎么放开：{out}"
        );
        assert_eq!(fixture.spy.opens(), 0, "连都不该连");

        // 拿不到句柄，写工具也就无从调起——句柄只能由 begin 发，而 begin
        // 在只读成员上过不去。这是"只收紧不放宽"在远端这一侧的样子。
        let forged = call(
            &fixture.hub,
            "remote_apply_patch",
            json!({ "workspace": "prod", "coding_handle": "rc-whatever",
                    "files": [{ "op": "delete", "path": "a.rs", "version": "1" }] }),
        );
        assert_eq!(
            forged["error"]["code"], "REMOTE_CODING_HANDLE_UNKNOWN",
            "{forged}"
        );
        assert_eq!(fixture.spy.opens(), 0, "还是一条都不该开");
    }

    /// noauth 不开放 remote coding（RFC-0002 5.3），但只读照常。
    #[test]
    fn writing_needs_an_authenticated_connection() {
        let fixture = coding_fixture(RemoteSpy::new());
        let anonymous = AuthContext::anonymous(HUB_SCOPE);
        let ask = |tool: &str, args: Value| -> Value {
            let (response, _) = fixture.hub.handle_request(
                &anonymous,
                &json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                         "params": { "name": tool, "arguments": args } }),
            );
            response["result"]["structuredContent"].clone()
        };

        let refused = ask("remote_coding_begin", json!({ "workspace": "prod" }));
        assert_eq!(
            refused["error"]["code"], "CODING_REQUIRES_AUTH",
            "{refused}"
        );
        assert_eq!(fixture.spy.opens(), 0);

        // 只读不受这条限制：操作员选了 noauth 就是自己决定把端口敞开。
        let (response, _) = fixture.hub.handle_request(
            &anonymous,
            &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                     "params": { "name": "remote_read_file",
                                 "arguments": { "workspace": "prod", "path": "a.rs" } } }),
        );
        assert_eq!(response["result"]["isError"], json!(false), "{response}");
    }

    /// 一轮完整的写：begin → patch → exec → 读输出 → end。
    #[test]
    fn a_writing_session_carries_the_handle_through_every_call() {
        let fixture = coding_fixture(RemoteSpy::new());
        let handle = begin(&fixture);
        assert!(handle.starts_with("rc-"), "{handle}");

        for (tool, args) in [
            (
                "remote_apply_patch",
                json!({ "files": [{ "op": "add", "path": "new.rs", "content": "x" }] }),
            ),
            ("remote_exec_command", json!({ "cmd": ["cargo", "test"] })),
            ("remote_read_output", json!({ "output_ref": "out-1" })),
        ] {
            let mut full = args.clone();
            full["workspace"] = json!("prod");
            full["coding_handle"] = json!(handle);
            let out = raw_call(&fixture.hub, tool, full);
            assert_eq!(out["isError"], json!(false), "{tool}: {out}");
            assert_eq!(out["_meta"]["gld/mode"], json!("coding"), "{tool}");
        }

        // 转发的是 ccnm 的名字，句柄和 workspace 都没跟着出去。
        let calls = fixture.spy.calls();
        let names: Vec<&str> = calls.iter().filter_map(|c| c["name"].as_str()).collect();
        assert_eq!(
            names,
            vec!["apply_patch", "exec_command", "read_output"],
            "{names:?}"
        );
        for c in &calls {
            let args = &c["arguments"];
            assert!(args.get("coding_handle").is_none(), "句柄不该转发：{args}");
            assert!(args.get("workspace").is_none(), "路由字段不该转发：{args}");
        }
        // cmd 原样是数组，不会被拍成 shell 字符串。
        assert_eq!(calls[1]["arguments"]["cmd"], json!(["cargo", "test"]));

        let closed = call(
            &fixture.hub,
            "remote_coding_end",
            json!({ "workspace": "prod", "coding_handle": handle }),
        );
        assert_eq!(closed["ok"], true, "{closed}");
        assert_eq!(fixture.spy.closes(), 1, "关了会话就该断连接，把写锁还回去");
    }

    /// 不带句柄、或者带一个编的句柄：都拒，而且不往远端发。
    #[test]
    fn a_write_without_a_valid_handle_never_reaches_the_remote() {
        let fixture = coding_fixture(RemoteSpy::new());
        let patch = json!({ "files": [{ "op": "delete", "path": "a.rs", "version": "1" }] });

        let mut no_handle = patch.clone();
        no_handle["workspace"] = json!("prod");
        let out = call(&fixture.hub, "remote_apply_patch", no_handle);
        assert_eq!(out["error"]["code"], "MISSING_ARGUMENT", "{out}");
        assert!(
            out["summary"]
                .as_str()
                .unwrap_or("")
                .contains("remote_coding_begin"),
            "得告诉它先去哪儿拿句柄：{out}"
        );

        let mut made_up = patch;
        made_up["workspace"] = json!("prod");
        made_up["coding_handle"] = json!("rc-0000000000000000");
        let out = call(&fixture.hub, "remote_apply_patch", made_up);
        assert_eq!(
            out["error"]["code"], "REMOTE_CODING_HANDLE_UNKNOWN",
            "{out}"
        );

        assert_eq!(fixture.spy.opens(), 0, "一条 bridge 都不该开");
        assert!(fixture.spy.calls().is_empty(), "什么都不该发出去");
    }

    /// 写到一半断了：报 outcome unknown，**不标可重试**。
    ///
    /// 重发一次 apply_patch 或 exec_command 可能是第二次执行（验收项 H07）。
    #[test]
    fn a_write_that_loses_the_connection_reports_an_unknown_outcome() {
        let mut spy = RemoteSpy::new();
        spy.black_hole = true;
        let fixture = coding_fixture(spy);
        let handle = begin(&fixture);

        let out = call(
            &fixture.hub,
            "remote_exec_command",
            json!({ "workspace": "prod", "coding_handle": handle, "cmd": ["rm", "-rf", "build"] }),
        );
        assert_eq!(out["error"]["code"], "REMOTE_OUTCOME_UNKNOWN", "{out}");
        assert_eq!(
            out["error"]["retryable"],
            json!(false),
            "绝不能标可重试：{out}"
        );
        assert_eq!(out["error"]["details"]["outcome"], json!("unknown"));
        assert!(
            out["summary"]
                .as_str()
                .unwrap_or("")
                .contains("do not just resend"),
            "{out}"
        );
    }

    // ---- 写入互斥（验收项 H06）----

    /// ccnm 冻结协议的 `docs/protocol/fixtures-mcp/start-refused-busy.json`，
    /// 2026-09-16 逐字抄来。工作树被另一个会话占着时，bridge 在 MCP 握手之前
    /// 就退出，这段是它 stderr 上留下的全部内容。
    ///
    /// **后面那几行是给人的排查指引**，fixture 的 `$note` 写明措辞会变。
    /// 抄全了才测得出「只看消息第一行」这件事。
    const FIXTURE_GUARD_BUSY: &str = "CCNM_E_POLICY:\n\
         workspace write guard is busy; another session still owns this working tree\n\
         who holds it, on the Runtime Node: the `held <session> <workspace>` file in\n\
         ${XDG_STATE_HOME:-~/.local/state}/ccnm/write-guards/\n\
         `ccnm status` alone does not prove nobody is using it: a --print run holds\n\
         this guard and never appears there";

    /// 同上，`start-refused-guard-unknown.json`。
    const FIXTURE_GUARD_UNKNOWN: &str =
        "CCNM_E_POLICY:\nworkspace write guard state is unknown; refusing to transfer write authority";

    /// ccnm `write_guard.rs` 里第三种：上一个持有者被打断，留下了 `held` 标记。
    /// 它那句话的措辞被 ccnm 自己的 p12 dogfood 测试锁住，恢复步骤是追加在后面的。
    const FIXTURE_GUARD_LEFT_HELD: &str = "CCNM_E_POLICY:\n\
         workspace write guard was left held by an interrupted process; old children may still exist, so authority is not transferred automatically\n\
         recover on the Runtime Node, in this order:\n\
         1. prove the old ones are gone: `ccnm status <workspace>` AND a process list\n\
         never clear it just because time passed";

    /// 远端写锁被别人占着 vs 状态说不清，是两个错，给的指示相反。
    #[test]
    fn a_busy_lock_and_an_unknown_lock_are_not_the_same_error() {
        let busy = classify_startup_failure(FIXTURE_GUARD_BUSY);
        assert_eq!(busy.0, "REMOTE_WRITE_LOCK_BUSY");
        assert!(busy.2, "别人占着，等一会儿可以再试");

        for text in [FIXTURE_GUARD_UNKNOWN, FIXTURE_GUARD_LEFT_HELD] {
            let unknown = classify_startup_failure(text);
            assert_eq!(unknown.0, "REMOTE_WRITE_LOCK_UNKNOWN", "{text}");
            assert!(
                !unknown.2,
                "状态说不清绝不能标可重试：重试到它「好了」的另一种可能，是两个 Agent 同时改一棵树。{text}"
            );
        }

        // 认不出来就按老样子走，宁可少标一个 unknown。
        assert_eq!(
            classify_startup_failure("connection reset by peer").0,
            "REMOTE_BRIDGE_CLOSED"
        );
    }

    /// **只看消息的第一行**，不在整段 stderr 上做包含匹配。
    ///
    /// busy 那条的排查指引里带着 `held <session>`、`this guard` 这些词。
    /// 在整段上匹配的话，ccnm 哪天把指引改成带 "state is unknown" 的一句，
    /// 一个本来可以重试的 busy 就会被读成"要人工介入"。
    #[test]
    fn only_the_first_message_line_decides_which_failure_it_is() {
        assert!(
            FIXTURE_GUARD_BUSY.contains("held <session>"),
            "fixture 抄得不全，这条测试就没意义了"
        );
        assert_eq!(
            classify_startup_failure(FIXTURE_GUARD_BUSY).0,
            "REMOTE_WRITE_LOCK_BUSY"
        );

        // 指引里出现 unknown 的字样也不该改变判断。
        let noisy =
            format!("{FIXTURE_GUARD_BUSY}\nif the guard state is unknown, see docs/operations.md");
        assert_eq!(
            classify_startup_failure(&noisy).0,
            "REMOTE_WRITE_LOCK_BUSY",
            "被排查指引带偏了"
        );
    }

    /// 两个会话争同一把远端写锁：第二个拿到的是 busy，**不会**降级成只读，
    /// 也不会静默重试到它"好了"。
    #[test]
    fn a_second_session_competing_for_the_same_writer_is_refused() {
        let mut spy = RemoteSpy::new();
        spy.coding_opens_allowed = 1; // 第一条 coding 连接成功，之后都报 busy
        let fixture = coding_fixture(spy);

        let first = call(
            &fixture.hub,
            "remote_coding_begin",
            json!({ "workspace": "prod" }),
        );
        assert_eq!(first["ok"], true, "{first}");

        // 换一个主体来开：它拿的是另一条连接，于是撞上远端那把锁。
        let other = AuthContext::new(
            crate::auth::Principal::OAuthClient {
                client_id: "another-client".into(),
            },
            HUB_SCOPE,
        );
        let (response, _) = fixture.hub.handle_request(
            &other,
            &json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/call",
                     "params": { "name": "remote_coding_begin",
                                 "arguments": { "workspace": "prod" } } }),
        );
        let refused = response["result"]["structuredContent"].clone();
        assert_eq!(
            refused["error"]["code"], "REMOTE_WRITE_LOCK_BUSY",
            "{refused}"
        );
        assert_eq!(refused["error"]["retryable"], json!(true), "{refused}");

        // gld 不替远端认定占锁的是谁：ccnm 的 guard 是 Managed session 和
        // 外部 coding 共用的（协议 4.4），说成"另一个远端会话"是在瞎猜。
        let text = refused.to_string();
        assert!(
            !text.contains("another remote") && !text.contains("another managed"),
            "不该替远端认定占锁的是哪一种会话：{text}"
        );
        assert!(
            text.contains("still owns this working tree"),
            "远端原话该带出来：{text}"
        );
    }

    /// 写锁被占着的时候，**只读照常**——ccnm 的 read 模式不碰这把锁（协议 4.4）。
    #[test]
    fn a_busy_writer_does_not_block_reading() {
        let mut spy = RemoteSpy::new();
        spy.coding_opens_allowed = 0; // coding 一律 busy
        let fixture = coding_fixture(spy);

        let refused = call(
            &fixture.hub,
            "remote_coding_begin",
            json!({ "workspace": "prod" }),
        );
        assert_eq!(
            refused["error"]["code"], "REMOTE_WRITE_LOCK_BUSY",
            "{refused}"
        );

        let read = raw_call(
            &fixture.hub,
            "remote_read_file",
            json!({ "workspace": "prod", "path": "src/main.rs" }),
        );
        assert_eq!(read["isError"], json!(false), "只读不该被写锁挡住：{read}");
    }

    /// 同一个会话上的两个调用串行，第二个拿到的是"忙"。
    ///
    /// **并发本身在 session 层测**（那儿能把等待宽限调短，不用真等两秒）：
    /// `bridge::session::a_second_call_on_one_session_waits_briefly_then_says_busy`。
    /// 这里只钉住它到了 hub 这一层长什么样——尤其是**可重试**：等前一个完事
    /// 就行，跟"远端做没做不知道"是两回事，后者绝不能标可重试。
    #[test]
    fn a_busy_session_is_retryable_unlike_an_unknown_outcome() {
        let member = CcnmMember {
            id: "m1".into(),
            name: "prod".into(),
            ccnm_bin: "ccnm".into(),
            node: "work".into(),
            workspace: "server".into(),
            max_mode: Mode::Coding,
        };
        let busy = coding_failure("remote_apply_patch", &member, CodingError::Busy);
        assert_eq!(busy["error"]["code"], "REMOTE_CODING_BUSY");
        assert_eq!(busy["error"]["retryable"], json!(true), "{busy}");
        assert!(
            busy["summary"]
                .as_str()
                .unwrap_or("")
                .contains("one call at a time"),
            "{busy}"
        );

        // 对照：断在半路的那个绝不可重试。
        let unknown = coding_failure(
            "remote_apply_patch",
            &member,
            CodingError::Peer(PeerError::Closed {
                method: "tools/call".into(),
                stderr: String::new(),
            }),
        );
        assert_eq!(unknown["error"]["code"], "REMOTE_OUTCOME_UNKNOWN");
        assert_eq!(unknown["error"]["retryable"], json!(false), "{unknown}");
        // 连接一丢，远端就把这个会话起的后台命令全停了（ccnm 协议第 6 节）。
        // 这件事是确定的，得说出来——不然模型只知道"这次调用结果未知"，会
        // 以为它起的那个构建还在跑。
        assert!(
            unknown["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("running in the background"),
            "{unknown}"
        );
    }

    /// 前一个跑完之后，同一个句柄接着能用——"忙"是暂时的，不是会话作废了。
    #[test]
    fn the_session_still_works_after_the_busy_call_finishes() {
        let fixture = coding_fixture(RemoteSpy::new());
        let handle = begin(&fixture);

        for path in ["a.txt", "b.txt"] {
            let out = raw_call(
                &fixture.hub,
                "remote_apply_patch",
                json!({ "workspace": "prod", "coding_handle": handle,
                        "files": [{ "op": "add", "path": path, "content": "1" }] }),
            );
            assert_eq!(out["isError"], json!(false), "串行跑完照样能用：{out}");
        }
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

    /// 远端那台机器上的 MCP server（ccnm P49）：跟 exec 一样要 coding 句柄，
    /// 参数原样转，`arguments` 是对象、不会被拍平。
    #[test]
    fn a_remote_mcp_call_goes_through_the_coding_session() {
        let fixture = coding_fixture(RemoteSpy::new());
        let without = raw_call(
            &fixture.hub,
            "remote_call_mcp_tool",
            json!({ "workspace": "prod", "server": "db" }),
        );
        assert_eq!(
            without["structuredContent"]["error"]["code"],
            json!("MISSING_ARGUMENT"),
            "没有句柄就不起远端的 server：{without}"
        );
        let handle = begin(&fixture);
        let out = raw_call(
            &fixture.hub,
            "remote_call_mcp_tool",
            json!({
                "workspace": "prod", "coding_handle": handle,
                "server": "db", "tool": "query", "arguments": { "sql": "select 1" }
            }),
        );
        assert_eq!(out["isError"], json!(false), "{out}");
        let calls = fixture.spy.calls();
        assert_eq!(calls[0]["name"], json!("call_mcp_tool"));
        assert_eq!(
            calls[0]["arguments"],
            json!({ "server": "db", "tool": "query", "arguments": { "sql": "select 1" } })
        );
    }

    /// 远端只在有 server 可转时才列这个工具。没列的时候，要说的是"那边没东西
    /// 可转"，不是"去升级 ccnm"。
    #[test]
    fn a_remote_with_nothing_to_relay_is_not_blamed_on_its_version() {
        let fixture = coding_fixture(RemoteSpy::new().without_tools(&["call_mcp_tool"]));
        let handle = begin(&fixture);
        let refused = raw_call(
            &fixture.hub,
            "remote_call_mcp_tool",
            json!({ "workspace": "prod", "coding_handle": handle }),
        );
        assert_eq!(
            refused["structuredContent"]["error"]["code"],
            json!("REMOTE_TOOL_UNSUPPORTED")
        );
        let text = refused["content"][0]["text"].as_str().unwrap_or_default();
        assert!(text.contains("offers no MCP server to relay"), "{text}");
        assert!(!text.contains("Upgrade ccnm"), "{text}");
        assert!(fixture.spy.calls().is_empty());
    }

    // ---- 本机装好的 MCP server（RFC-0006）----

    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 每开一条连接记一次数，连上的是内存里的 server（两个工具，调用回显参数）。
    struct Counting(Arc<AtomicUsize>);

    impl toexec_mcp::Open for Counting {
        fn open(
            &self,
            _server: &toexec_mcp::Server,
        ) -> Result<Box<dyn toexec_mcp::Transport>, toexec_mcp::Error> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(toexec_mcp::scripted::plain_server("2025-06-18")))
        }

        fn me(&self) -> (&str, &str) {
            ("gld-test", "0")
        }
    }

    /// 装了 context7 和 desktop-commander，开了 `on` 里那几个。
    fn relay_hub(on: &[&str], profile: &str) -> (Hub, Arc<AtomicUsize>, tempfile::TempDir) {
        crate::home::isolate_for_tests();
        let (dir, api) = workspace("api", &[]);
        let mut settings = AppSettings::default();
        settings.hub.members = vec![api.id.clone()];
        settings.hub.tool_profile = profile.into();
        settings.relayed_mcp_servers = on.iter().map(|name| name.to_string()).collect();
        let opened = Arc::new(AtomicUsize::new(0));
        let installed = Installed {
            servers: vec![
                crate::machine_mcp::relay::tests::server("context7", "npx"),
                crate::machine_mcp::relay::tests::server("desktop-commander", "npx"),
            ],
            ..Installed::default()
        };
        let hub = Hub::fixed(
            &settings.hub.clone(),
            Fixed {
                profiles: vec![api],
                remotes: Vec::new(),
                settings,
            },
        )
        .with_relay(Box::new(Counting(opened.clone())), installed);
        (hub, opened, dir)
    }

    fn relay_call(hub: &Hub, auth: &AuthContext, tool: &str, arguments: Value) -> Value {
        let (response, _) = hub.handle_request(
            auth,
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": tool, "arguments": arguments }
            }),
        );
        response["result"].clone()
    }

    fn listed(hub: &Hub) -> Vec<Value> {
        hub.list_tools()
    }

    fn named<'a>(tools: &'a [Value], name: &str) -> Option<&'a Value> {
        tools.iter().find(|tool| tool["name"] == json!(name))
    }

    #[test]
    fn the_relay_tools_show_up_only_when_a_server_is_turned_on() {
        let (hub, _, _dir) = relay_hub(&[], "compact");
        assert!(
            named(&listed(&hub), "list_mcp_tools").is_none(),
            "一个都没开就不列"
        );

        let (hub, opened, _dir) = relay_hub(&["context7"], "compact");
        let tools = listed(&hub);
        for name in ["list_mcp_tools", "call_mcp_tool", "read_mcp_result"] {
            let tool = named(&tools, name).unwrap_or_else(|| panic!("{name} 该列出来"));
            let required = tool["inputSchema"]["required"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            assert!(
                !required.contains(&json!("workspace")),
                "{name} 不属于任何工作区"
            );
        }
        let description = named(&tools, "list_mcp_tools").unwrap()["description"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            description.contains("context7 (local process)"),
            "{description}"
        );
        assert!(!description.contains("desktop-commander"), "没开的不能出现");
        let (init, _) = hub.handle_request(
            &caller(),
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }),
        );
        assert!(init["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("list_mcp_tools"));
        assert_eq!(opened.load(Ordering::SeqCst), 0, "列工具不起 server");
    }

    #[test]
    fn a_read_only_service_relays_nothing() {
        let (hub, opened, _dir) = relay_hub(&["context7"], "read-only");
        assert!(named(&listed(&hub), "call_mcp_tool").is_none());
        let refused = relay_call(
            &hub,
            &caller(),
            "call_mcp_tool",
            json!({ "server": "context7", "tool": "echo" }),
        );
        assert_eq!(
            refused["structuredContent"]["error"]["code"],
            json!("TOOL_NOT_ALLOWED")
        );
        assert_eq!(opened.load(Ordering::SeqCst), 0);

        let (hub, _, _dir) = relay_hub(&["context7"], "compat-readonly-all");
        let call = named(&listed(&hub), "call_mcp_tool").cloned().unwrap();
        assert_eq!(
            call["annotations"]["readOnlyHint"],
            json!(true),
            "这个工具集只改标注"
        );
    }

    #[test]
    fn a_relayed_call_needs_no_workspace_and_each_client_gets_its_own_server() {
        let (hub, opened, _dir) = relay_hub(&["context7"], "compact");
        let args = json!({ "server": "context7", "tool": "echo", "arguments": { "q": 1 } });
        let result = relay_call(&hub, &caller(), "call_mcp_tool", args.clone());
        assert_eq!(result["isError"], json!(false), "{result}");
        assert_eq!(result["content"][0]["text"], json!("{\"q\":1}"));
        relay_call(&hub, &caller(), "call_mcp_tool", args.clone());
        assert_eq!(opened.load(Ordering::SeqCst), 1, "同一个调用方复用一条");
        relay_call(&hub, &oauth_client("other"), "call_mcp_tool", args.clone());
        assert_eq!(opened.load(Ordering::SeqCst), 2, "另一个客户端另起一个");

        let off = relay_call(
            &hub,
            &caller(),
            "call_mcp_tool",
            json!({ "server": "desktop-commander", "tool": "start_process" }),
        );
        assert_eq!(
            off["structuredContent"]["error"]["code"],
            json!("MCP_SERVER_UNKNOWN"),
            "装了但没开的，和没装的一样"
        );

        update_fixed(&hub, |_, settings| settings.relayed_mcp_servers.clear());
        assert!(hub.relay.has_connection("context7", &caller().tag()));
        // 随便调一个别的工具，被关掉的 server 就跟着收了。
        call(&hub, LIST_WORKSPACES, json!({}));
        assert!(!hub.relay.has_connection("context7", &caller().tag()));
        let gone = relay_call(&hub, &caller(), "call_mcp_tool", args);
        assert_eq!(
            gone["structuredContent"]["error"]["code"],
            json!("MCP_SERVER_UNKNOWN"),
            "关掉之后下一次调用就不给了"
        );
        assert_eq!(opened.load(Ordering::SeqCst), 2);
    }
}

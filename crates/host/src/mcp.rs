use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::listeners::ShutdownSender;
use crate::nodes::{endpoint, registry::Nodes};
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Form, Path, Query, State};
use axum::http::{
    header::{
        ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
        ACCESS_CONTROL_EXPOSE_HEADERS, ALLOW, AUTHORIZATION, CACHE_CONTROL, ORIGIN,
        REFERRER_POLICY, USER_AGENT, VARY, WWW_AUTHENTICATE,
    },
    HeaderMap, HeaderValue, StatusCode,
};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use gld_core::async_rt::{self, JoinHandle};
use gld_core::auth::{AccessReader, UrlRejection};
use gld_core::data::{AppData, DataStore};
use gld_core::generation::CallError;
use gld_core::hub::{public_base_url, Hub, HubSecrets, HUB_SCOPE, SERVER_NAME};
use gld_core::local_network;
use gld_core::logs::append_profile_log;
use gld_core::mcp::handle_request;
use gld_core::settings::{AppSettings, HubConfig};
use gld_core::tools::{build_tool_context, patch::failure_log_fields, SharedToolContext};
use gld_core::usage::ServiceUsage;
use gld_core::workspace::{AuthConfig, RuntimeConfig};
use serde_json::{json, Value};
use tokio::sync::oneshot;
use tower_http::cors::CorsLayer;

use crate::auth::source::{self, CredentialSource};
use crate::auth::{
    authorization_server_metadata, authorize_get, authorize_post, external_base_url, hub_audience,
    protected_resource_metadata, protected_resource_metadata_url, register_client, token_exchange,
    workspace_audience, AuthContext, AuthorizeForm, AuthorizeParams, ClientRegistrationRequest,
    ClientRegistry, OAuthRuntime, TokenForm,
};
use crate::generation::Router as GenerationRouter;

/// 监听器背后是谁在答 JSON-RPC。
///
/// 认证、OAuth 路由、请求日志工作区和 hub 完全一样，只有这一处不同。
/// 分成两份监听器的话，下次修一个 OAuth 的坑就得记得修两遍。
#[derive(Clone)]
enum Endpoint {
    /// 一个工作区自己的 MCP 服务。
    Workspace(SharedToolContext),
    /// 聚合入口：每次调用按 `workspace` 参数分到成员，见 [`gld_core::hub`]。
    Hub { hub: Arc<Hub>, nodes: Arc<Nodes> },
}

/// 一条请求处理完的结果。
struct Handled {
    response: Value,
    /// 这次实际用到的工具上下文的审计摘要；hub 没路由到成员时为 Null。
    audit: Value,
    /// hub 请求落到的成员 id。工作区自己的监听器永远是 None。
    member: Option<String>,
}

impl Endpoint {
    fn server_name(&self) -> String {
        match self {
            Self::Workspace(context) => context.server_name().to_string(),
            Self::Hub { .. } => SERVER_NAME.to_string(),
        }
    }

    fn usage(&self) -> Arc<ServiceUsage> {
        match self {
            Self::Workspace(context) => context.usage(),
            Self::Hub { hub, .. } => hub.usage(),
        }
    }

    /// 同步跑工具，必须在 `spawn_blocking` 里调。
    ///
    /// 两支都要 `auth`：它是这次调用的主体，命令会话按它分表。不传的话，
    /// 这个入口上所有连接共用一张表，谁都能拿别人的 `session_id` 去读
    /// （见 [`gld_core::tools::caller`]）。
    fn handle(
        &self,
        scope: &str,
        auth: &AuthContext,
        body: &Value,
        generation: Option<&GenerationRouter>,
    ) -> Handled {
        // 宿主进程里工具不在本进程跑，鉴权已经验完，发给执行代。
        if let Some(generation) = generation {
            return self.handle_routed(generation, scope, auth, body);
        }
        match self {
            Self::Workspace(context) => {
                let response = handle_request(context, auth, body);
                Handled {
                    response,
                    audit: context.context_audit_snapshot(),
                    member: None,
                }
            }
            Self::Hub { hub, .. } => {
                let (response, routed) = hub.handle_request(auth, body);
                Handled {
                    response,
                    member: routed.as_ref().map(|routed| routed.workspace_id.clone()),
                    // 远端成员没有本机工具上下文，取上下文审计这一段跳过。
                    audit: routed
                        .and_then(|routed| routed.context)
                        .map(|context| context.context_audit_snapshot())
                        .unwrap_or(Value::Null),
                }
            }
        }
    }

    fn handle_routed(
        &self,
        generation: &GenerationRouter,
        scope: &str,
        auth: &AuthContext,
        body: &Value,
    ) -> Handled {
        let routed = match self {
            Self::Workspace(context) => {
                generation.workspace_mcp(scope, context.workspace.root(), auth, body)
            }
            Self::Hub { hub, .. } => {
                generation.hub_mcp(hub.execution_route(auth, body), auth, body)
            }
        };
        match routed {
            Ok(reply) => Handled {
                response: reply.response,
                audit: reply.audit,
                member: reply.member,
            },
            Err(error) => Handled {
                response: generation_failure(body, &error),
                audit: Value::Null,
                member: None,
            },
        }
    }
}

/// 执行代不可用或中途退出时给客户端的回包。
///
/// 还是 HTTP 200 的 JSON-RPC 错误：入口和认证都好好的，错的只是这一次调用，回 5xx 的话
/// 客户端（和隧道那头）会以为整个服务挂了。`Lost` 必须说清结果未知：调用可能已经产生了副作用，
/// 所以这边绝不替它重放，由调用方查过再决定。
fn generation_failure(body: &Value, error: &CallError) -> Value {
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    if id.is_null() {
        // 通知没有回包可言。
        return Value::Null;
    }
    let (reason, suggestion) = match error {
        CallError::Lost => (
            "generation_lost",
            "The tool process exited while handling this call, so the outcome is unknown and it may already have taken effect. It was not retried. Check the current state (for example with list_runs) before calling again.",
        ),
        CallError::Unavailable(_) => (
            "generation_unavailable",
            "The call was not executed. The tool process is restarting; retry shortly.",
        ),
        CallError::Refused(_) | CallError::TimedOut => (
            "worker_failed",
            "The call failed inside the tool process and may already have taken effect. Check the current state before calling again; if it keeps failing, restart the service with gld daemon restart.",
        ),
    };
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32603,
            "message": "Exec RPC worker failed",
            "data": {
                "stage": "generation",
                "reason": reason,
                "retryable": error.surely_not_executed(),
                "suggestion": suggestion,
            }
        }
    })
}

/// 一条请求的日志往哪儿写。
///
/// hub 请求在 hub 自己的日志里记一份，再在落到的成员日志里记一份（带 `[hub]` 前缀）：
/// `gld logs -w api` 看得到经 hub 对 api 做了什么，又看不到别的成员的请求。
struct RequestLog<'a> {
    scope: &'a str,
    member: Option<&'a str>,
}

impl RequestLog<'_> {
    fn line(&self, text: &str) {
        append_profile_log(self.scope, "mcp-requests.log", text);
        if let Some(member) = self.member {
            append_profile_log(member, "mcp-requests.log", &format!("[hub] {text}"));
        }
    }
}

async fn oauth_register_post(
    State(state): State<ListenerState>,
    Json(request): Json<ClientRegistrationRequest>,
) -> Response {
    let Some(oauth) = state.oauth.as_ref() else {
        return oauth_not_configured();
    };
    register_client(oauth, request)
}

#[derive(Clone)]
struct ListenerState {
    endpoint: Endpoint,
    /// 与 App 的授权写者共享快照；不在宿主另开一份存储或保存认证结果。
    access: AccessReader,
    /// 仅单工作区端点使用；配置未约束读取时禁止网络 grant 越界读取。
    workspace_confined: bool,
    generation: Option<Arc<GenerationRouter>>,
    /// 日志目录和 OAuth 客户端注册表的作用域：工作区 id，或 [`HUB_SCOPE`]。
    scope: String,
    /// 授权页上告诉用户"你在授权什么"的那一行。
    authorize_label: String,
    bind_port: u16,
    /// 启动时记下的公网基址。只在地址纯运行时才知道时（Cloudflare 临时隧道、
    /// OpenAI 隧道）由它兜底；其余情况每次请求按当前配置重算，见
    /// [`configured_oauth_base`]。
    spawn_public_url: String,
    oauth: Option<Arc<OAuthRuntime>>,
    oauth_client_secret: Option<String>,
}

#[allow(clippy::too_many_arguments)]
pub fn spawn_listener(
    port: u16,
    workspace_path: PathBuf,
    workspace_id: String,
    workspace_name: String,
    auth: AuthConfig,
    public_base_url: String,
    oauth_client_secret: Option<String>,
    _oauth_password: Option<String>,
    oauth_token_secret: Option<String>,
    runtime: RuntimeConfig,
    usage: Arc<ServiceUsage>,
    generation: Option<Arc<GenerationRouter>>,
    access: AccessReader,
) -> Result<(ShutdownSender, JoinHandle<()>), String> {
    let workspace_display = workspace_path.display().to_string();
    let workspace_confined = runtime.confine_reads;
    let global = AppSettings::load_or_default();
    // 和命令行 `gld tool call` 共用同一个构建入口，两边看到的工具集与策略必然一致。
    let mcp = Arc::new(build_tool_context(
        workspace_path,
        &workspace_name,
        auth.clone(),
        &runtime,
        &global,
        usage,
    )?);
    let spawn_public_url = public_base_url.trim().to_string();
    let oauth = Some(Arc::new(OAuthRuntime::new_mcp(
        workspace_audience(&workspace_id),
        workspace_id.clone(),
        auth.oauth_client_id.clone(),
        oauth_client_secret.clone(),
        oauth_token_secret.unwrap_or_default(),
        Arc::new(ClientRegistry::load(&workspace_id, &workspace_id)),
        access.clone(),
    )));
    listen(ListenerState {
        endpoint: Endpoint::Workspace(mcp),
        access,
        workspace_confined,
        generation,
        scope: workspace_id,
        authorize_label: workspace_display,
        bind_port: port,
        spawn_public_url,
        oauth,
        oauth_client_secret,
    })
}

/// 起聚合入口的监听器。路由、认证、日志和工作区监听器是同一套，只是答请求的换成 [`Hub`]。
/// `nodes` 必须是宿主注入该 Hub 的同一张节点表，停止时也由宿主显式关闭。
///
/// 令牌受众是 [`hub_audience`]，客户端注册表是独立的 [`HUB_SCOPE`]：工作区发出去的令牌
/// 进不了 hub，hub 的令牌也进不了任何工作区——拿到一个项目的授权不等于拿到全部。
#[allow(clippy::too_many_arguments)]
pub fn spawn_hub_listener(
    port: u16,
    hub: Arc<Hub>,
    nodes: Arc<Nodes>,
    _auth_type: &str,
    public_base_url: String,
    secrets: HubSecrets,
    generation: Option<Arc<GenerationRouter>>,
    access: AccessReader,
) -> Result<(ShutdownSender, JoinHandle<()>), String> {
    let oauth = Some(Arc::new(OAuthRuntime::new_mcp(
        hub_audience(),
        HUB_SCOPE.into(),
        secrets.oauth_client_id,
        None,
        secrets.oauth_token_secret,
        Arc::new(ClientRegistry::load(HUB_SCOPE, HUB_SCOPE)),
        access.clone(),
    )));
    listen(ListenerState {
        endpoint: Endpoint::Hub { hub, nodes },
        access,
        workspace_confined: true,
        generation,
        scope: HUB_SCOPE.into(),
        authorize_label: "gld 客户端授权".into(),
        bind_port: port,
        spawn_public_url: public_base_url.trim().to_string(),
        oauth,
        oauth_client_secret: None,
    })
}

fn listen(state: ListenerState) -> Result<(ShutdownSender, JoinHandle<()>), String> {
    let port = state.bind_port;
    // 在返回 Running 之前完成 bind，避免后台任务里的端口冲突被伪装成启动成功。
    let allow_lan_access = AppSettings::load_or_default().allow_lan_access;
    let listener = bind_listener(port, allow_lan_access)?;
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let profile_id = state.scope.clone();
    let handle = async_rt::spawn(async move {
        let result = serve(listener, port, allow_lan_access, state, shutdown_rx).await;
        if let Err(err) = &result {
            append_profile_log(
                &profile_id,
                "stderr.log",
                &format!("[mcp] listener stopped: {err}"),
            );
            eprintln!("mcp listener stopped: {err}");
        } else {
            append_profile_log(&profile_id, "stderr.log", "[mcp] listener stopped");
        }
    });
    Ok((shutdown_tx, handle))
}

async fn serve(
    listener: tokio::net::TcpListener,
    port: u16,
    allow_lan_access: bool,
    state: ListenerState,
    shutdown: oneshot::Receiver<()>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let profile_id = state.scope.clone();
    let app = Router::new()
        .route("/mcp", get(mcp_discovery).post(mcp_post))
        .route("/nodes/v1/connect/{machine_id}", get(node_connect))
        .route("/nodes/v1/pair", post(node_pair))
        .route(
            "/.well-known/oauth-authorization-server",
            get(oauth_authorization_server_metadata),
        )
        .route(
            "/.well-known/oauth-protected-resource",
            get(oauth_protected_resource_metadata),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(oauth_protected_resource_metadata),
        )
        .route("/register", post(oauth_register_post))
        .route(
            "/oauth/authorize",
            get(oauth_authorize_get).post(oauth_authorize_post),
        )
        .route("/oauth/token", post(oauth_token_post))
        .layer(CorsLayer::permissive())
        // URL 凭据只用于专用路径；它的认证不会回退到标准 Bearer。
        .merge(url_router())
        .with_state(state);

    append_profile_log(
        &profile_id,
        "stdout.log",
        &format!(
            "[mcp] listening on http://{}:{port}/mcp",
            local_network::bind_host(allow_lan_access)
        ),
    );
    let shutdown_profile = profile_id.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            // 分清两件事：收到停止信号（有人停它）和发送端被丢弃（没人停它，
            // 但持有它的那条运行记录没了）。后者是 bug 的征兆，日志里必须能看出来。
            let reason = match shutdown.await {
                Ok(()) => "收到停止信号",
                Err(_) => "停止信号的发送端被丢弃——没有人显式停止它",
            };
            append_profile_log(
                &shutdown_profile,
                "stderr.log",
                &format!("[mcp] 开始优雅关闭：{reason}"),
            );
        })
        .await?;
    Ok(())
}

fn bind_listener(port: u16, allow_lan_access: bool) -> Result<tokio::net::TcpListener, String> {
    let addr = local_network::bind_addr(port, allow_lan_access);
    let listener = std::net::TcpListener::bind(addr)
        .map_err(|err| format!("MCP 本地端口 {port} 绑定失败: {err}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|err| format!("MCP 本地端口 {port} 设置非阻塞失败: {err}"))?;
    tokio::net::TcpListener::from_std(listener)
        .map_err(|err| format!("MCP 本地监听器初始化失败: {err}"))
}

async fn mcp_discovery(State(state): State<ListenerState>) -> Response {
    (
        [(CACHE_CONTROL, "no-store")],
        Json(mcp_discovery_payload(&state)),
    )
        .into_response()
}

/// 节点与 MCP 分开鉴权；不接受 OAuth、grant 或隧道服务凭据代替节点凭据。
async fn node_connect(
    State(state): State<ListenerState>,
    Path(machine_id): Path<String>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let Endpoint::Hub { nodes, .. } = state.endpoint else {
        return StatusCode::NOT_FOUND.into_response();
    };
    endpoint::upgrade(nodes, machine_id, headers, ws).await
}

async fn node_pair(
    State(state): State<ListenerState>,
    request: axum::extract::Request,
) -> Response {
    if !matches!(state.endpoint, Endpoint::Hub { .. }) {
        return StatusCode::NOT_FOUND.into_response();
    }
    endpoint::pair(request).await
}

fn mcp_discovery_payload(state: &ListenerState) -> Value {
    json!({
        "name": state.endpoint.server_name(),
        "version": gld_core::VERSION,
        "protocolVersion": "2025-06-18"
    })
}

/// OAuth 发现类应答（401 挑战、两个 well-known、authorize、token）共用的公网基址。
///
/// 每次请求按当前配置重算：入口换地址后（`gld gateway start` 换了公网地址、
/// `hub set --global-gateway false` 关掉入口），监听器不能还指着启动时的旧地址，
/// 否则旧地址下线后从新地址来的客户端完成不了授权。
fn resolve_oauth_base(state: &ListenerState, headers: &HeaderMap) -> String {
    external_base_url(headers, state.bind_port, &configured_oauth_base(state))
}

/// 当前配置推导的公网基址；推不出来（纯运行时地址）或数据文件读失败时用启动时的。
///
/// 配置能表达的入口（none / frp / 命名 Cloudflare / 手填 public_url / 全局入口）一律以当前
/// 配置为准：算出来是空就传空串给 [`external_base_url`]，走转发头或回环，不回退到启动时的
/// 旧地址。只有运行时才知道的地址（Cloudflare 临时隧道、OpenAI 隧道）用启动时记下的那个。
/// 读数据文件失败时记一条不带秘密的警告、回退启动时的地址，绝不让请求变成 500。
fn configured_oauth_base(state: &ListenerState) -> String {
    match DataStore::read_file(|data| Ok(configured_base_from_data(&state.scope, data))) {
        Ok(Some(base)) => base,
        Ok(None) => state.spawn_public_url.clone(),
        Err(_) => {
            append_profile_log(
                &state.scope,
                "stderr.log",
                "[oauth] 数据文件读取失败，OAuth 发现地址沿用启动时的值",
            );
            state.spawn_public_url.clone()
        }
    }
}

/// 数据文件里的当前配置能算出基址就返回它（可以是空串）；纯运行时地址返回 None。
///
/// 工作区不分支：临时隧道的地址由隧道运行时写回 `profile.tunnel.public_url`，现读就是现值。
fn configured_base_from_data(scope: &str, data: &AppData) -> Option<String> {
    let settings = AppSettings::from_data(data);
    if scope == HUB_SCOPE {
        if hub_runtime_only(&settings.hub) {
            return None;
        }
        return Some(public_base_url(&settings));
    }
    let profile = data.profiles.iter().find(|profile| profile.id == scope)?;
    Some(profile.effective_public_url_with(&settings))
}

/// 这个隧道类型的公网地址要等隧道起来才知道（OpenAI 隧道根本没有），配置里推不出来。
/// 与 [`public_base_url`] 的分支保持一致。
fn hub_runtime_only(hub: &HubConfig) -> bool {
    match hub.tunnel_type.as_str() {
        "cloudflare" => hub.cloudflare_mode != "named",
        "openai" => true,
        _ => false,
    }
}

/// URL-only 客户端不一定实现 OAuth / 自定义认证头。独立短期 grant 复用现有
/// Hub 和权限检查，不开无鉴权入口，也不维护另一套 MCP 会话或执行器。
///
/// 这条路由不用通配的 CORS 层：只有秘密、期限、grant 和 Origin 都通过的请求，
/// 响应里才回显它自己的 Origin。预检不带凭据头，但秘密在路径里，同样完整核对。
fn url_router() -> Router<ListenerState> {
    Router::new()
        .route(
            "/mcp/connect/{token}",
            post(mcp_url_post)
                .get(mcp_url_no_stream)
                .delete(mcp_url_no_stream)
                .options(mcp_url_preflight),
        )
        .layer(middleware::map_response(url_response))
}

/// 浏览器扩展要带的请求头。不含 Authorization：这条路由上同时带路径秘密和
/// Authorization 是来源冲突（400），放进预检许可只会让浏览器发出一个必然失败的请求。
const URL_CORS_ALLOW_HEADERS: &str =
    "content-type, accept, mcp-session-id, mcp-protocol-version, last-event-id";
/// 不列出来的响应头，跨源脚本读不到；客户端要从响应里取会话 ID 和协商后的版本。
const URL_CORS_EXPOSE_HEADERS: &str = "mcp-session-id, mcp-protocol-version";

async fn url_response(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    // 同一地址对不同 Origin 的响应头不同，中间缓存不能混用。
    headers.insert(VARY, HeaderValue::from_static("Origin"));
    response
}

/// 给已经通过秘密和 Origin 核对的请求回显它的 Origin。不设 Allow-Credentials：
/// 凭据在路径里，不用 cookie。请求没带 Origin 时什么都不加。
fn url_cors(mut response: Response, origin: Option<&str>) -> Response {
    let Some(origin) = origin.and_then(|value| HeaderValue::from_str(value).ok()) else {
        return response;
    };
    let headers = response.headers_mut();
    headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    headers.insert(
        ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static(URL_CORS_EXPOSE_HEADERS),
    );
    response
}

/// URL 入口通过认证的请求：本次权限，加上已经核对过、可以回显的 Origin。
struct UrlAccess {
    auth: AuthContext,
    origin: Option<String>,
}

/// `/mcp/connect/<secret>` 的认证：只接受 URL 用途的凭据，失败不回退到别的来源。
///
/// 顺序是固定的：来源冲突和畸形 Origin（400）→ 授权存储可用（503）→ 秘密、期限和
/// grant 状态（401）→ Origin（403）→ 单工作区监听器的项目范围（403）。
/// 401 不带 OAuth 发现信息：这个入口不能让客户端以为它是标准 OAuth 资源。
fn require_url_auth(
    state: &ListenerState,
    headers: &HeaderMap,
    token: &str,
) -> Result<UrlAccess, Box<Response>> {
    let credential = match source::url(headers, token) {
        Err("conflicting_credential_sources") => return Err(Box::new(mcp_source_conflict())),
        Ok(CredentialSource::Url { id, secret }) => Some((id, secret)),
        _ => None,
    };
    let origin = source::request_origin(headers).map_err(|_| Box::new(url_invalid_origin()))?;
    let current = state
        .access
        .snapshot()
        .map_err(|_| Box::new(mcp_auth_not_ready()))?;
    // 未初始化的新授权文件不能让旧的浏览器地址恢复权限。
    if !current.is_initialized() {
        return Err(Box::new(mcp_auth_not_ready()));
    }
    drop(current);
    let (id, secret) = credential.ok_or_else(|| Box::new(url_unauthorized()))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Box::new(mcp_auth_not_ready()))?
        .as_secs();
    let auth = state
        .access
        .authenticate_url(&state.scope, id, secret, origin, now)
        .map_err(|rejection| {
            Box::new(match rejection {
                UrlRejection::Unavailable => mcp_auth_not_ready(),
                UrlRejection::Credential => url_unauthorized(),
                UrlRejection::Origin => url_origin_not_allowed(),
            })
        })?;
    if matches!(&state.endpoint, Endpoint::Workspace(_))
        && (!auth.allows_project(&state.scope) || !state.workspace_confined)
    {
        // 秘密和 Origin 都已核对，回显 Origin 让扩展能读到这条说明。
        return Err(Box::new(url_cors(mcp_forbidden(), origin)));
    }
    Ok(UrlAccess {
        auth,
        origin: origin.map(str::to_string),
    })
}

fn url_unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": "AUTHENTICATION_FAILED",
            "message": "Missing or invalid credential"
        })),
    )
        .into_response()
}

fn url_origin_not_allowed() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": "ORIGIN_NOT_ALLOWED",
            "message": "这个地址不允许从当前 Origin 使用；请在创建地址时把它加入 --origin"
        })),
    )
        .into_response()
}

fn url_invalid_origin() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "error": "INVALID_ORIGIN",
            "message": "请求的 Origin 头重复、为空或不是有效文本"
        })),
    )
        .into_response()
}

/// URL 入口的拒绝也记一行，和 `/mcp` 一样用来分清「请求没到」和「到了但被拒」。
/// 秘密在路径里，所以这里从不记路径或查询串，只记状态、Origin 和来源线索。
fn log_url_rejected(scope: &str, headers: &HeaderMap, status: StatusCode) {
    append_profile_log(
        scope,
        "mcp-requests.log",
        &format!(
            "[auth] rejected status={} entry=url origin={} forwarded_for={} user_agent={} （请求到了 gld 的 URL 入口，但地址、Origin 或请求头不被接受）",
            status.as_u16(),
            header_for_log(headers, &[ORIGIN.as_str()]),
            header_for_log(headers, &["cf-connecting-ip", "x-forwarded-for", "x-real-ip"]),
            header_for_log(headers, &[USER_AGENT.as_str()]),
        ),
    );
}

/// 预检不执行任何工具，也不生成权限上下文之外的东西：只有随后的实际请求会被接受时，
/// 才回显 Origin 和允许的方法、请求头。其余情况回一个不带 CORS 头的 204，浏览器会拦下
/// 实际请求；响应不区分秘密错误和 Origin 不在名单里。
async fn mcp_url_preflight(
    State(state): State<ListenerState>,
    Path(token): Path<String>,
    headers: HeaderMap,
) -> Response {
    let mut response = (StatusCode::NO_CONTENT, [(ALLOW, "POST, OPTIONS")]).into_response();
    let allowed = match require_url_auth(&state, &headers, &token) {
        Ok(access) => access.origin,
        Err(_) => None,
    };
    if let Some(origin) = allowed.as_deref() {
        response = url_cors(response, Some(origin));
        let headers = response.headers_mut();
        headers.insert(
            ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("POST, OPTIONS"),
        );
        headers.insert(
            ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static(URL_CORS_ALLOW_HEADERS),
        );
    }
    response
}

async fn mcp_url_post(
    State(state): State<ListenerState>,
    Path(token): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let UrlAccess { auth, origin } = match require_url_auth(&state, &headers, &token) {
        Ok(access) => access,
        Err(response) => {
            log_url_rejected(&state.scope, &headers, response.status());
            return *response;
        }
    };
    if let Err(response) = require_url_protocol(&headers) {
        return url_cors(*response, origin.as_deref());
    }
    url_cors(dispatch_mcp(state, auth, body).await, origin.as_deref())
}

fn require_url_protocol(headers: &HeaderMap) -> Result<(), Box<Response>> {
    let versions = headers.get_all("mcp-protocol-version");
    if versions.iter().count() <= 1
        && headers
            .get("mcp-protocol-version")
            .is_none_or(|version| matches!(version.to_str(), Ok("2025-03-26" | "2025-06-18")))
    {
        // 首次 initialize 还没有版本头；params 中的新版本可以协商回 2025-06-18。
        return Ok(());
    }
    Err(Box::new(
        (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "unsupported_mcp_protocol_version",
                "message": "请使用 initialize 协商后的 MCP-Protocol-Version"
            })),
        )
            .into_response(),
    ))
}

/// Streamable HTTP 的独立 GET 流是可选的。SDK 接受 405；保持无状态 JSON
/// 传输，不能给 GET 返回 discovery JSON 冒充 SSE，也不签发可复用的 session。
async fn mcp_url_no_stream(
    State(state): State<ListenerState>,
    Path(token): Path<String>,
    headers: HeaderMap,
) -> Response {
    let origin = match require_url_auth(&state, &headers, &token) {
        Ok(access) => access.origin,
        Err(response) => {
            log_url_rejected(&state.scope, &headers, response.status());
            return *response;
        }
    };
    if let Err(response) = require_url_protocol(&headers) {
        return url_cors(*response, origin.as_deref());
    }
    url_cors(
        (StatusCode::METHOD_NOT_ALLOWED, [(ALLOW, "POST, OPTIONS")]).into_response(),
        origin.as_deref(),
    )
}

async fn mcp_post(
    State(state): State<ListenerState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let auth = match require_mcp_auth(&state, &headers) {
        Ok(auth) => auth,
        Err(response) => {
            log_rejected(&state.scope, &headers, response.status());
            return *response;
        }
    };
    dispatch_mcp(state, auth, body).await
}

/// 两种入口只在鉴权处不同：日志、范围限制、工具执行及错误结构走同一条路径。
async fn dispatch_mcp(state: ListenerState, auth: AuthContext, body: Value) -> Response {
    let method = body
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let request_id = body.get("id").cloned().unwrap_or(Value::Null);
    let request_bytes = serde_json::to_vec(&body)
        .map(|bytes| bytes.len())
        .unwrap_or_default();
    let tool_name = body
        .get("params")
        .and_then(|params| params.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    append_profile_log(
        &state.scope,
        "mcp-requests.log",
        // 记的是"怎么过的鉴权"，不是令牌——auth.tag() 里没有凭据。
        &format!(
            "[rpc] request id={} method={} tool={}{} auth={}",
            request_id,
            method,
            tool_name,
            relay_target(&tool_name, &body),
            auth.tag()
        ),
    );

    let endpoint = state.endpoint.clone();
    let scope = state.scope.clone();
    let generation = state.generation.clone();
    let result = tokio::task::spawn_blocking(move || {
        endpoint.handle(&scope, &auth, &body, generation.as_deref())
    })
    .await;
    match result {
        Ok(handled) => {
            let response = handled.response;
            let log = RequestLog {
                scope: &state.scope,
                member: handled.member.as_deref(),
            };
            let response_bytes = serde_json::to_vec(&response)
                .map(|bytes| bytes.len())
                .unwrap_or_default();
            let is_error = response.get("error").is_some()
                || response
                    .get("result")
                    .and_then(|result| result.get("isError"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
            state.endpoint.usage().record(
                request_bytes,
                response_bytes,
                method == "tools/call",
                is_error,
            );
            let audit = handled.audit;
            let repeated_bytes = audit
                .get("repeated_bytes")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            let latest_block = audit
                .get("blocks")
                .and_then(Value::as_array)
                .and_then(|blocks| blocks.last());
            log.line(&format!(
                "[rpc] completed id={} method={} tool={} response_bytes={} repeated_bytes={}",
                request_id, method, tool_name, response_bytes, repeated_bytes
            ));
            if let Some(block) = latest_block {
                log.line(&format!(
                        "[context-audit] kind={} bytes={} hash={} repeated={} total_bytes={} repeated_bytes={}",
                        block.get("kind").and_then(Value::as_str).unwrap_or("unknown"),
                        block.get("bytes").and_then(Value::as_u64).unwrap_or_default(),
                        block.get("hash").and_then(Value::as_str).unwrap_or("unknown"),
                        block.get("repeated").and_then(Value::as_bool).unwrap_or(false),
                        audit.get("total_bytes").and_then(Value::as_u64).unwrap_or_default(),
                        repeated_bytes
                ));
            }
            if tool_name == "apply_patch" || tool_name == "patch_check" {
                let structured = response.pointer("/result/structuredContent");
                if let Some(fields) = structured.and_then(failure_log_fields) {
                    let operation_id = structured
                        .and_then(|value| value.get("operation_id"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    log.line(&format!(
                        "[patch] id={} tool={} operation_id={} {}",
                        request_id, tool_name, operation_id, fields
                    ));
                }
            }
            if tool_name == "exec_command" || tool_name == "exec_health_check" {
                let structured = response
                    .get("result")
                    .and_then(|result| result.get("structuredContent"));
                let status = structured
                    .and_then(|value| value.get("status"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let termination_reason = structured
                    .and_then(|value| value.get("termination_reason"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let exit_code = structured
                    .and_then(|value| value.get("exit_code"))
                    .map(Value::to_string)
                    .unwrap_or_default();
                let is_error = response
                    .get("result")
                    .and_then(|result| result.get("isError"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                log.line(&format!(
                    "[exec] id={} tool={} is_error={} status={} termination_reason={} exit_code={}",
                    request_id, tool_name, is_error, status, termination_reason, exit_code
                ));
            }
            if response.is_null() {
                // MCP 规范要求通知回 202、没有响应体。官方 TS SDK 1.25 收到 200 + null
                // 只是丢掉响应体，但只在 202 后才去探测可选的 GET 推送流；按规范回，
                // 少一处客户端之间的分歧。
                StatusCode::ACCEPTED.into_response()
            } else {
                Json(response).into_response()
            }
        }
        Err(error) => {
            let error_response = json!({
                "jsonrpc": "2.0",
                "id": request_id.clone(),
                "error": {
                    "code": -32603,
                    "message": "Exec RPC worker failed",
                    "data": {
                        "stage": "rpc_worker",
                        "reason": "worker_failed",
                        "retryable": true,
                        "suggestion": "重试请求或重启 MCP 运行时"
                    }
                }
            });
            let response_bytes = serde_json::to_vec(&error_response)
                .map(|bytes| bytes.len())
                .unwrap_or_default();
            state.endpoint.usage().record(
                request_bytes,
                response_bytes,
                method == "tools/call",
                true,
            );
            append_profile_log(
                &state.scope,
                "mcp-requests.log",
                &format!(
                    "[rpc] worker_failed id={} method={} tool={} error={error}",
                    request_id, method, tool_name
                ),
            );
            Json(error_response).into_response()
        }
    }
}

/// 被鉴权挡下的请求也记一行。
///
/// 客户端连不上时第一个要分清的是：请求根本没到 gld（隧道 / 地址的问题），还是到了
/// 但凭据不对。不记的话两种情况日志都是一片空白。公网上被扫描时也靠这几行看出来。
///
/// 只记"带没带凭据"，凭据本身绝不进日志：日志常被整段贴出去求助，而填错的 token
/// 往往只差一两个字符。`forwarded_for` 来自隧道加的请求头，直连时客户端能随便填，
/// 只能当线索。
fn log_rejected(scope: &str, headers: &HeaderMap, status: StatusCode) {
    let (credential, hint) = if headers.contains_key(AUTHORIZATION) {
        ("rejected", "请求到了 gld，但凭据不对或已失效")
    } else {
        ("missing", "请求到了 gld，但没带凭据")
    };
    append_profile_log(
        scope,
        "mcp-requests.log",
        &format!(
            "[auth] rejected status={} credential={credential} forwarded_for={} user_agent={} （{hint}）",
            status.as_u16(),
            header_for_log(headers, &["cf-connecting-ip", "x-forwarded-for", "x-real-ip"]),
            header_for_log(headers, &[USER_AGENT.as_str()]),
        ),
    );
}

/// 取第一个存在的请求头写进日志：没有就是 `-`，最多 120 个字符，空格和控制字符换成 `_`。
/// 不换的话，请求方能在 User-Agent 里写一段长得像 `status=200` 的字段，把日志带偏。
fn header_for_log(headers: &HeaderMap, names: &[&str]) -> String {
    names
        .iter()
        .find_map(|name| headers.get(*name))
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .chars()
                .take(120)
                .map(|c| {
                    if c.is_whitespace() || c.is_control() {
                        '_'
                    } else {
                        c
                    }
                })
                .collect()
        })
        .unwrap_or_else(|| "-".to_string())
}

/// `/mcp` 的认证分派：先确定唯一的凭据来源，再只按那一种验证，失败即拒绝。
/// API token 和 OAuth access token 已接入统一授权；URL 凭据走自己的路径，见 [`require_url_auth`]；
/// 隧道仍未接入，返回 AUTH_NOT_READY。
fn require_mcp_auth(
    state: &ListenerState,
    headers: &HeaderMap,
) -> Result<AuthContext, Box<Response>> {
    let source = match source::standard(headers) {
        Err("duplicate_credential_source" | "conflicting_credential_sources") => {
            return Err(Box::new(mcp_source_conflict()));
        }
        Err("invalid_credential") => {
            return Err(Box::new(mcp_unauthorized(state, headers)));
        }
        // 隧道的显式 grant 绑定还没接线：格式合法也不取得工具访问权。
        Ok(CredentialSource::Tunnel(key)) => {
            let _ = key;
            return Err(Box::new(mcp_auth_not_ready()));
        }
        Ok(source) => Some(source),
        Err(_) => None,
    };
    let current = state
        .access
        .snapshot()
        .map_err(|_| Box::new(mcp_auth_not_ready()))?;
    // 未初始化的新授权文件不能让旧 service token、旧 grants 或旧 OAuth token 恢复权限。
    if !current.is_initialized() {
        return Err(Box::new(mcp_auth_not_ready()));
    }
    drop(current);
    let auth = match source {
        Some(CredentialSource::Token { id, secret }) => {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| Box::new(mcp_auth_not_ready()))?
                .as_secs();
            state
                .access
                .authenticate_token(&state.scope, id, secret, now)
                .ok()
        }
        // 验签、用途、受众、期限、client 注册和当前 grant/修订号都在 OAuth 运行时里核对；
        // 不通过就是 401，不把这串 Bearer 再当成别的凭据。
        Some(CredentialSource::OAuthBearer(token)) => state
            .oauth
            .as_ref()
            .and_then(|oauth| oauth.authenticate_mcp_access(token)),
        // 没带凭据：401 里的发现信息让 OAuth 客户端知道去哪里授权。
        _ => None,
    };
    let auth = auth.ok_or_else(|| Box::new(mcp_unauthorized(state, headers)))?;
    if matches!(&state.endpoint, Endpoint::Workspace(_))
        && (!auth.allows_project(&state.scope) || !state.workspace_confined)
    {
        return Err(Box::new(mcp_forbidden()));
    }
    Ok(auth)
}

fn mcp_unauthorized(state: &ListenerState, headers: &HeaderMap) -> Response {
    let resource = protected_resource_metadata_url(&resolve_oauth_base(state, headers));
    let mut response = (
        StatusCode::UNAUTHORIZED,
        [
            (CACHE_CONTROL, "no-store"),
            (REFERRER_POLICY, "no-referrer"),
        ],
        Json(json!({"error": "AUTHENTICATION_FAILED", "message": "Missing or invalid credential"})),
    )
        .into_response();
    let challenge = format!("Bearer resource_metadata=\"{resource}\"");
    if let Ok(value) = challenge.parse() {
        response.headers_mut().insert(WWW_AUTHENTICATE, value);
    }
    response
}

fn mcp_forbidden() -> Response {
    (
        StatusCode::FORBIDDEN,
        [(CACHE_CONTROL, "no-store"), (REFERRER_POLICY, "no-referrer")],
        Json(json!({"error": "PROJECT_ACCESS_DENIED", "message": "Grant cannot access this workspace"})),
    )
        .into_response()
}

fn mcp_source_conflict() -> Response {
    (
        StatusCode::BAD_REQUEST,
        [
            (CACHE_CONTROL, "no-store"),
            (REFERRER_POLICY, "no-referrer"),
        ],
        Json(json!({
            "error": "CONFLICTING_CREDENTIAL_SOURCES",
            "message": "请求同时提供多个认证来源"
        })),
    )
        .into_response()
}

fn mcp_auth_not_ready() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(CACHE_CONTROL, "no-store")],
        Json(json!({
            "error": "AUTH_NOT_READY",
            "message": "这台机器还没有可用的统一授权，或请求来自尚未绑定 grant 的隧道；匿名和旧凭据一律拒绝"
        })),
    )
        .into_response()
}

async fn oauth_authorization_server_metadata(
    State(state): State<ListenerState>,
    headers: HeaderMap,
) -> Response {
    if state.oauth.is_none() {
        return oauth_not_configured();
    }
    let base = resolve_oauth_base(&state, &headers);
    Json(authorization_server_metadata(
        &base,
        state.oauth_client_secret.as_deref(),
    ))
    .into_response()
}

async fn oauth_protected_resource_metadata(
    State(state): State<ListenerState>,
    headers: HeaderMap,
) -> Response {
    if state.oauth.is_none() {
        return oauth_not_configured();
    }
    Json(protected_resource_metadata(&resolve_oauth_base(
        &state, &headers,
    )))
    .into_response()
}

async fn oauth_authorize_get(
    State(state): State<ListenerState>,
    headers: HeaderMap,
    Query(params): Query<AuthorizeParams>,
) -> Response {
    let Some(oauth) = state.oauth.as_ref() else {
        return oauth_not_configured();
    };
    authorize_get(
        oauth,
        params,
        Some(state.authorize_label.as_str()),
        &resolve_oauth_base(&state, &headers),
    )
}

async fn oauth_authorize_post(
    State(state): State<ListenerState>,
    headers: HeaderMap,
    Form(form): Form<AuthorizeForm>,
) -> Response {
    let Some(oauth) = state.oauth.as_ref() else {
        return oauth_not_configured();
    };
    authorize_post(oauth, form, &resolve_oauth_base(&state, &headers))
}

async fn oauth_token_post(
    State(state): State<ListenerState>,
    headers: HeaderMap,
    Form(form): Form<TokenForm>,
) -> Response {
    let Some(oauth) = state.oauth.as_ref() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "unsupported_grant_type" })),
        )
            .into_response();
    };
    token_exchange(oauth, &headers, form, &resolve_oauth_base(&state, &headers))
}

fn oauth_not_configured() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "OAuth not configured" })),
    )
        .into_response()
}

/// 转发调用（`call_mcp_tool`、`remote_call_mcp_tool`）实际调的是哪个 server 的哪个工具，记进请求日志。
///
/// 以前只记 `tool=call_mcp_tool`，事后查不出 AI 经转发调了什么：2026-10-08 发现隔离浏览器漏关了能跑任意代码的
/// `browser_run_code_unsafe`，回头想确认上一轮有没有被调过，日志里没有这个名字。只记名字、不记参数；名字是调用方
/// 传的，只留字母数字和 `_-.`，最多 64 个字符，免得往日志里塞换行伪造一行。
fn relay_target(tool_name: &str, body: &Value) -> String {
    if !matches!(tool_name, "call_mcp_tool" | "remote_call_mcp_tool") {
        return String::new();
    }
    let arguments = body.pointer("/params/arguments");
    let field = |key: &str| -> String {
        arguments
            .and_then(|arguments| arguments.get(key))
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
                    c
                } else {
                    '_'
                }
            })
            .take(64)
            .collect()
    };
    format!(" relay={}:{}", field("server"), field("tool"))
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use axum::extract::State;
    use axum::http::header::CACHE_CONTROL;
    use axum::response::IntoResponse;
    use gld_core::auth::{
        AccessReader, AccessStore, CredentialKind, GrantCreation, GrantPermissions, ProjectScope,
    };
    use gld_core::tools::{PolicySettings, ToolContext, Workspace};
    use gld_core::workspace::AuthConfig;

    use super::{bind_listener, mcp_discovery, mcp_discovery_payload, Endpoint, ListenerState};
    use crate::auth::test_home::run_isolated;

    fn state_named(root: &Path, workspace_name: &str) -> ListenerState {
        state_with_access(
            root,
            workspace_name,
            AccessStore::open_in(root).unwrap().reader(),
        )
    }

    fn state_with_access(root: &Path, workspace_name: &str, access: AccessReader) -> ListenerState {
        let workspace = Workspace::new(root.to_path_buf(), Vec::new()).expect("workspace");
        let context = ToolContext::from_workspace_with_harness_root(
            workspace,
            AuthConfig {
                auth_type: "noauth".into(),
                ..AuthConfig::default()
            },
            PolicySettings::default(),
            "full".into(),
            "trusted".into(),
            root.join("harness"),
        )
        .expect("tool context")
        .with_workspace_name(workspace_name);
        ListenerState {
            endpoint: Endpoint::Workspace(Arc::new(context)),
            access,
            workspace_confined: true,
            generation: None,
            scope: "w1".into(),
            authorize_label: "/tmp/x".into(),
            bind_port: 0,
            spawn_public_url: String::new(),
            oauth: None,
            oauth_client_secret: None,
        }
    }

    /// 配置能表达的入口（手填地址、全局入口）一律以当前配置为准，入口停用后是空串
    /// （走转发头或回环），绝不回退到启动时的旧地址。
    #[test]
    fn hub_oauth_base_follows_current_config() {
        use gld_core::data::AppData;
        use gld_core::hub::HUB_SCOPE;

        let mut data = AppData::default();
        data.hub.public_url = "https://manual.example.com/".into();
        assert_eq!(
            super::configured_base_from_data(HUB_SCOPE, &data).as_deref(),
            Some("https://manual.example.com")
        );

        data.hub.use_global_gateway = true;
        data.global_gateway.enabled = true;
        data.global_gateway.public_url = "https://gw.example.com".into();
        assert_eq!(
            super::configured_base_from_data(HUB_SCOPE, &data).as_deref(),
            Some("https://gw.example.com/hub")
        );

        // 入口换地址：不重启监听器也要跟着变。
        data.global_gateway.public_url = "https://gw-two.example.com".into();
        assert_eq!(
            super::configured_base_from_data(HUB_SCOPE, &data).as_deref(),
            Some("https://gw-two.example.com/hub")
        );

        // 入口停用：回落到手填地址；连手填也没有就是空串，交给 external_base_url。
        data.global_gateway.enabled = false;
        assert_eq!(
            super::configured_base_from_data(HUB_SCOPE, &data).as_deref(),
            Some("https://manual.example.com")
        );
        data.hub.public_url.clear();
        assert_eq!(
            super::configured_base_from_data(HUB_SCOPE, &data).as_deref(),
            Some("")
        );

        // 命名 Cloudflare 是配置能表达的。
        let mut data = AppData::default();
        data.hub.tunnel_type = "cloudflare".into();
        data.hub.cloudflare_mode = "named".into();
        data.hub.public_url = "https://cf.example.com".into();
        assert_eq!(
            super::configured_base_from_data(HUB_SCOPE, &data).as_deref(),
            Some("https://cf.example.com")
        );
    }

    /// Cloudflare 临时隧道和 OpenAI 隧道的地址运行时才知道：返回 None，调用方用启动时的值。
    #[test]
    fn hub_oauth_base_keeps_spawn_url_for_runtime_only_tunnels() {
        use gld_core::data::AppData;
        use gld_core::hub::HUB_SCOPE;

        for (tunnel_type, cloudflare_mode) in [("cloudflare", "quick"), ("openai", "quick")] {
            let mut data = AppData::default();
            data.hub.tunnel_type = tunnel_type.into();
            data.hub.cloudflare_mode = cloudflare_mode.into();
            assert_eq!(super::configured_base_from_data(HUB_SCOPE, &data), None);
        }
    }

    /// 工作区：全局入口、手填地址、临时隧道写回值都从当前数据文件现读。
    #[test]
    fn workspace_oauth_base_follows_current_profile_config() {
        use gld_core::data::AppData;
        use gld_core::workspace::WorkspaceProfile;

        fn profile(id: &str) -> WorkspaceProfile {
            let mut profile = WorkspaceProfile::new("/tmp/ws".into(), None);
            profile.id = id.into();
            profile
        }

        // 老配置：MCP 走全局入口。
        let mut data = AppData::default();
        data.global_gateway.enabled = true;
        data.global_gateway.public_url = "https://gw.example.com".into();
        let mut routed = profile("w1");
        routed.tunnel.use_global_gateway = true;
        data.profiles.push(routed);
        assert_eq!(
            super::configured_base_from_data("w1", &data).as_deref(),
            Some("https://gw.example.com/w/w1")
        );

        // 入口换地址跟着变；停用后是空串，不指向已下线的入口。
        data.global_gateway.public_url = "https://gw-two.example.com".into();
        assert_eq!(
            super::configured_base_from_data("w1", &data).as_deref(),
            Some("https://gw-two.example.com/w/w1")
        );
        data.global_gateway.enabled = false;
        assert_eq!(
            super::configured_base_from_data("w1", &data).as_deref(),
            Some("")
        );

        // 手填地址。
        let mut data = AppData::default();
        let mut manual = profile("w2");
        manual.tunnel.public_url = "https://manual.example.com/".into();
        data.profiles.push(manual);
        assert_eq!(
            super::configured_base_from_data("w2", &data).as_deref(),
            Some("https://manual.example.com")
        );

        // Cloudflare 临时隧道：地址由隧道运行时写回 public_url，现读就是现值。
        let mut data = AppData::default();
        let mut quick = profile("w3");
        quick.tunnel.tunnel_type = "cloudflare".into();
        quick.tunnel.public_url = "https://quick.example.com".into();
        data.profiles.push(quick);
        assert_eq!(
            super::configured_base_from_data("w3", &data).as_deref(),
            Some("https://quick.example.com")
        );

        // 项目已经不在：用启动时的值。
        assert_eq!(super::configured_base_from_data("missing", &data), None);
    }

    /// 数据文件读失败：回退到启动时记下的地址并记警告，不能让请求变成 500。
    #[test]
    fn oauth_base_falls_back_to_spawn_url_when_the_data_file_is_unreadable() {
        run_isolated(
            module_path!(),
            "oauth_base_falls_back_to_spawn_url_when_the_data_file_is_unreadable",
            || {
                let home = std::env::var_os(gld_core::home::HOME_ENV).expect("isolated home");
                let data_dir = Path::new(&home).join("data");
                std::fs::create_dir_all(&data_dir).unwrap();
                std::fs::write(data_dir.join("profiles.json"), "{ not json").unwrap();
                let root = tempfile::tempdir().unwrap();
                let mut state = state_named(root.path(), "ws");
                state.spawn_public_url = "https://spawn.example.com".into();
                let base = super::resolve_oauth_base(&state, &axum::http::HeaderMap::new());
                assert_eq!(base, "https://spawn.example.com");
            },
        );
    }

    /// 用公开的授权页和 token 端点走一遍 PKCE，拿到 (access, refresh)。
    fn oauth_tokens(oauth: &super::OAuthRuntime, confirmation: &str) -> (String, String) {
        use super::{authorize_post, token_exchange, AuthorizeForm, TokenForm};
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;
        use sha2::{Digest, Sha256};

        let verifier = "dBjftJeZ4CVP-mB92Kpru-AEJvkQlLgi3ThpmQ45N_Xyo";
        let redirect = "https://chatgpt.com/connector/oauth/test";
        let granted = authorize_post(
            oauth,
            AuthorizeForm {
                client_id: "static-client".into(),
                redirect_uri: redirect.into(),
                code_challenge: URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
                code_challenge_method: "S256".into(),
                state: String::new(),
                password: confirmation.into(),
            },
            "http://127.0.0.1",
        );
        let location = granted.headers()["location"].to_str().unwrap().to_string();
        let code = location.split("code=").nth(1).unwrap();
        let code = code.split('&').next().unwrap().to_string();
        let response = token_exchange(
            oauth,
            &axum::http::HeaderMap::new(),
            TokenForm {
                grant_type: "authorization_code".into(),
                code,
                redirect_uri: redirect.into(),
                code_verifier: verifier.into(),
                client_id: "static-client".into(),
                ..TokenForm::default()
            },
            "http://127.0.0.1",
        );
        let bytes = gld_core::async_rt::block_on(async {
            axum::body::to_bytes(response.into_body(), 65_536)
                .await
                .unwrap()
        });
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        (
            body["access_token"].as_str().unwrap().to_string(),
            body["refresh_token"].as_str().unwrap().to_string(),
        )
    }

    #[test]
    fn oauth_bearer_is_checked_against_the_live_grant_and_never_falls_back() {
        run_isolated(
            module_path!(),
            "oauth_bearer_is_checked_against_the_live_grant_and_never_falls_back",
            || {
                let root = tempfile::tempdir().unwrap();
                let store = AccessStore::open_in(root.path()).unwrap();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                let issued = store
                    .create_grant(
                        GrantCreation {
                            name: "Chat".into(),
                            scope: ProjectScope::Selected(vec!["w1".into()]),
                            permissions: GrantPermissions::default(),
                            kind: CredentialKind::OAuth,
                            ttl: None,
                            allowed_origins: vec![],
                        },
                        now,
                    )
                    .unwrap();
                let confirmation = issued.secret.into_string();
                let mut state = state_with_access(root.path(), "api", store.reader());
                let oauth = Arc::new(super::OAuthRuntime::new_mcp(
                    "gld:mcp:w1".into(),
                    "w1".into(),
                    "static-client".into(),
                    None,
                    "signing-secret".into(),
                    Arc::new(gld_core::auth::ClientRegistry::in_memory()),
                    store.reader(),
                ));
                state.oauth = Some(oauth.clone());
                let (access, refresh) = oauth_tokens(&oauth, &confirmation);
                let bearer = |value: &str| {
                    let mut headers = axum::http::HeaderMap::new();
                    headers.insert(
                        axum::http::header::AUTHORIZATION,
                        format!("Bearer {value}").parse().unwrap(),
                    );
                    headers
                };
                let unauthorized = |state: &ListenerState, headers: &axum::http::HeaderMap| {
                    let failure = super::require_mcp_auth(state, headers).unwrap_err();
                    assert_eq!(failure.status(), axum::http::StatusCode::UNAUTHORIZED);
                    assert_eq!(failure.headers()[CACHE_CONTROL], "no-store");
                    let challenge = failure.headers()[axum::http::header::WWW_AUTHENTICATE]
                        .to_str()
                        .unwrap()
                        .to_string();
                    assert!(challenge.contains("resource_metadata="), "{challenge}");
                };

                let caller = super::require_mcp_auth(&state, &bearer(&access)).unwrap();
                assert!(caller.allows_project("w1"));
                assert!(!caller.may_write_project());
                // refresh、确认口令本身、别的密钥签的 JWT、没带凭据：都是带发现信息的 401。
                unauthorized(&state, &bearer(&refresh));
                unauthorized(&state, &bearer(&confirmation));
                unauthorized(&state, &bearer("old-oauth-jwt"));
                unauthorized(&state, &axum::http::HeaderMap::new());

                // 隧道还没接入统一授权，仍是 AUTH_NOT_READY，不因 OAuth 接通而放行。
                let mut tunnel = axum::http::HeaderMap::new();
                tunnel.insert(gld_core::hub::OPENAI_TUNNEL_HEADER, "key".parse().unwrap());
                assert_eq!(
                    super::require_mcp_auth(&state, &tunnel)
                        .unwrap_err()
                        .status(),
                    axum::http::StatusCode::SERVICE_UNAVAILABLE
                );

                // 单工作区入口沿用 token 的上限：grant 不含本项目，或没开读取限制，都是 403。
                let mut changed = issued.record.clone();
                changed.grant.scope = ProjectScope::Selected(vec!["another-project".into()]);
                store.update(changed.clone()).unwrap();
                assert_eq!(
                    super::require_mcp_auth(&state, &bearer(&access))
                        .unwrap_err()
                        .status(),
                    axum::http::StatusCode::FORBIDDEN
                );
                changed.grant.scope = ProjectScope::Selected(vec!["w1".into()]);
                store.update(changed).unwrap();
                state.workspace_confined = false;
                assert_eq!(
                    super::require_mcp_auth(&state, &bearer(&access))
                        .unwrap_err()
                        .status(),
                    axum::http::StatusCode::FORBIDDEN
                );
                state.workspace_confined = true;

                let rotated = store.rotate_grant("Chat", None, now).unwrap();
                unauthorized(&state, &bearer(&access));
                let (renewed, _) = oauth_tokens(&oauth, &rotated.secret.into_string());
                let after = super::require_mcp_auth(&state, &bearer(&renewed)).unwrap();
                assert_eq!(caller.id(), after.id());
                store.revoke(&issued.record.grant.id).unwrap();
                unauthorized(&state, &bearer(&renewed));

                // 没有 OAuth 运行时的入口不接受任何 OAuth Bearer。
                state.oauth = None;
                unauthorized(&state, &bearer(&renewed));
            },
        );
    }

    #[test]
    fn token_auth_rechecks_grant_scope_secret_and_revocation_without_restarting_listener() {
        run_isolated(
            module_path!(),
            "token_auth_rechecks_grant_scope_secret_and_revocation_without_restarting_listener",
            || {
                let root = tempfile::tempdir().unwrap();
                let store = AccessStore::open_in(root.path()).unwrap();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                let issued = store
                    .create_grant(
                        GrantCreation {
                            name: "Client".into(),
                            scope: ProjectScope::Selected(vec!["w1".into()]),
                            permissions: GrantPermissions::default(),
                            kind: CredentialKind::Token,
                            ttl: None,
                            allowed_origins: vec![],
                        },
                        now,
                    )
                    .unwrap();
                let mut state = state_with_access(root.path(), "api", store.reader());
                let secret = issued.secret.into_string();
                let mut headers = axum::http::HeaderMap::new();
                headers.insert(
                    axum::http::header::AUTHORIZATION,
                    format!("Bearer {secret}").parse().unwrap(),
                );
                let first = super::require_mcp_auth(&state, &headers).unwrap();
                assert!(first.allows_project("w1"));
                assert!(!first.may_write_project());

                let mut changed = issued.record.clone();
                changed.grant.permissions.write = true;
                store.update(changed.clone()).unwrap();
                let updated = super::require_mcp_auth(&state, &headers).unwrap();
                assert_eq!(first.id(), updated.id());
                assert!(updated.may_write_project());

                changed.grant.scope = ProjectScope::Selected(vec!["another-project".into()]);
                store.update(changed.clone()).unwrap();
                assert_eq!(
                    super::require_mcp_auth(&state, &headers)
                        .unwrap_err()
                        .status(),
                    axum::http::StatusCode::FORBIDDEN
                );
                changed.grant.scope = ProjectScope::Selected(vec!["w1".into()]);
                store.update(changed).unwrap();
                state.workspace_confined = false;
                assert_eq!(
                    super::require_mcp_auth(&state, &headers)
                        .unwrap_err()
                        .status(),
                    axum::http::StatusCode::FORBIDDEN
                );
                state.workspace_confined = true;

                let rotated = store.rotate_grant("Client", None, now).unwrap();
                let failure = super::require_mcp_auth(&state, &headers).unwrap_err();
                assert_eq!(failure.status(), axum::http::StatusCode::UNAUTHORIZED);
                assert_eq!(failure.headers()[CACHE_CONTROL], "no-store");
                assert!(failure
                    .headers()
                    .contains_key(axum::http::header::WWW_AUTHENTICATE));
                headers.insert(
                    axum::http::header::AUTHORIZATION,
                    format!("Bearer {}", rotated.secret.into_string())
                        .parse()
                        .unwrap(),
                );
                let after = super::require_mcp_auth(&state, &headers).unwrap();
                assert_eq!(first.id(), after.id());
                store.revoke(&issued.record.grant.id).unwrap();
                assert_eq!(
                    super::require_mcp_auth(&state, &headers)
                        .unwrap_err()
                        .status(),
                    axum::http::StatusCode::UNAUTHORIZED
                );
            },
        );
    }

    #[test]
    fn token_post_reaches_mcp_tools_and_revocation_blocks_the_next_request() {
        run_isolated(
            module_path!(),
            "token_post_reaches_mcp_tools_and_revocation_blocks_the_next_request",
            || {
                let root = tempfile::tempdir().unwrap();
                let store = AccessStore::open_in(root.path()).unwrap();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                let issued = store
                    .create_grant(
                        GrantCreation {
                            name: "HTTP client".into(),
                            scope: ProjectScope::Selected(vec!["w1".into()]),
                            permissions: GrantPermissions::default(),
                            kind: CredentialKind::Token,
                            ttl: None,
                            allowed_origins: vec![],
                        },
                        now,
                    )
                    .unwrap();
                let state = state_with_access(root.path(), "api", store.reader());
                let mut headers = axum::http::HeaderMap::new();
                let secret = issued.secret.into_string();
                headers.insert(
                    axum::http::header::AUTHORIZATION,
                    format!("Bearer {secret}").parse().unwrap(),
                );
                let body = serde_json::json!({
                    "jsonrpc": "2.0", "id": 1, "method": "tools/list"
                });
                let call = |state: ListenerState, headers: axum::http::HeaderMap| {
                    gld_core::async_rt::block_on(super::mcp_post(
                        State(state),
                        headers,
                        axum::Json(body.clone()),
                    ))
                };
                let response = call(state.clone(), headers.clone());
                assert_eq!(response.status(), axum::http::StatusCode::OK);
                let bytes =
                    gld_core::async_rt::block_on(axum::body::to_bytes(response.into_body(), 65536))
                        .unwrap();
                let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(result["id"], 1);
                let tools = result["result"]["tools"].as_array().unwrap();
                assert!(tools.iter().any(|tool| tool["name"] == "read_file"));
                assert!(!tools.iter().any(|tool| tool["name"] == "exec_command"));

                store.revoke(&issued.record.grant.id).unwrap();
                let response = call(state, headers);
                assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
                assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
                let bytes =
                    gld_core::async_rt::block_on(axum::body::to_bytes(response.into_body(), 4096))
                        .unwrap();
                assert!(!String::from_utf8_lossy(&bytes).contains(&secret));
            },
        );
    }

    /// 请求头是对方随便填的：写进日志前得压成一个字段，不能带空格凒出假字段，也不能无限长。
    #[test]
    fn headers_are_squashed_into_one_log_field() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "user-agent",
            "curl/8 status=200 credential=ok".parse().unwrap(),
        );
        headers.insert("x-forwarded-for", "a".repeat(500).parse().unwrap());

        assert_eq!(
            super::header_for_log(&headers, &["user-agent"]),
            "curl/8_status=200_credential=ok"
        );
        assert_eq!(
            super::header_for_log(&headers, &["cf-connecting-ip", "x-forwarded-for"]),
            "a".repeat(120)
        );
        assert_eq!(super::header_for_log(&headers, &["x-real-ip"]), "-");
    }

    #[test]
    fn bind_listener_reports_port_conflict_synchronously() {
        let occupied = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("占用测试端口");
        let port = occupied.local_addr().expect("读取测试端口").port();

        assert!(bind_listener(port, false).is_err());
    }

    #[test]
    fn discovery_reports_the_current_package_version() {
        run_isolated(
            module_path!(),
            "discovery_reports_the_current_package_version",
            || {
                let root = tempfile::tempdir().expect("fixture");
                let discovery = mcp_discovery_payload(&state_named(root.path(), "api"));

                assert_eq!(discovery["version"], env!("CARGO_PKG_VERSION"));
            },
        );
    }

    /// 服务器名要跟着工作区走。
    ///
    /// 这里以前写死 `coding-tools-mcp`（上游项目的名字）：接了三个工作区，
    /// 客户端的服务器列表里就是三个一模一样的条目，谁也分不出谁是哪个项目。
    #[test]
    fn discovery_names_the_workspace() {
        run_isolated(module_path!(), "discovery_names_the_workspace", || {
            let root = tempfile::tempdir().expect("fixture");
            assert_eq!(
                mcp_discovery_payload(&state_named(root.path(), "api"))["name"],
                "api"
            );
            // 空的 serverInfo.name 在有些客户端那儿直接显示成一行空白。
            assert_eq!(
                mcp_discovery_payload(&state_named(root.path(), ""))["name"],
                "gld"
            );
        });
    }

    #[test]
    fn discovery_prevents_stale_tool_catalog_caching() {
        run_isolated(
            module_path!(),
            "discovery_prevents_stale_tool_catalog_caching",
            || {
                let root = tempfile::tempdir().expect("fixture");
                let response = gld_core::async_rt::block_on(mcp_discovery(State(state_named(
                    root.path(),
                    "api",
                ))))
                .into_response();

                assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
            },
        );
    }

    #[test]
    fn all_unconnected_mcp_credentials_are_refused_without_fallback() {
        run_isolated(
            module_path!(),
            "all_unconnected_mcp_credentials_are_refused_without_fallback",
            || {
                let root = tempfile::tempdir().expect("fixture");
                let state = state_named(root.path(), "api");
                let mut headers = axum::http::HeaderMap::new();
                for credential in [
                    None,
                    Some("Bearer old-service-token"),
                    Some("Bearer old-grant-token"),
                    Some("Bearer old-oauth-jwt"),
                ] {
                    headers.remove(axum::http::header::AUTHORIZATION);
                    if let Some(value) = credential {
                        headers.insert(axum::http::header::AUTHORIZATION, value.parse().unwrap());
                    }
                    let response = super::require_mcp_auth(&state, &headers).unwrap_err();
                    assert_eq!(
                        response.status(),
                        axum::http::StatusCode::SERVICE_UNAVAILABLE
                    );
                    assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
                }
                headers.insert(
                    gld_core::hub::OPENAI_TUNNEL_HEADER,
                    "old-tunnel-key".parse().unwrap(),
                );
                assert_eq!(
                    super::require_mcp_auth(&state, &headers)
                        .unwrap_err()
                        .status(),
                    axum::http::StatusCode::BAD_REQUEST
                );
                let response = super::require_url_auth(&state, &headers, "old-url-secret")
                    .err()
                    .unwrap();
                let body =
                    gld_core::async_rt::block_on(axum::body::to_bytes(response.into_body(), 4096))
                        .unwrap();
                let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(value["error"], "CONFLICTING_CREDENTIAL_SOURCES");
                assert!(!String::from_utf8_lossy(&body).contains("old-url-secret"));
            },
        );
    }

    const EXTENSION: &str = "chrome-extension://kngiafgkdnlkgmefdafaibkibegkcaef";

    fn url_headers(origin: Option<&str>) -> axum::http::HeaderMap {
        let mut headers = axum::http::HeaderMap::new();
        if let Some(origin) = origin {
            headers.insert(axum::http::header::ORIGIN, origin.parse().unwrap());
        }
        headers
    }

    fn url_failure(
        state: &ListenerState,
        headers: &axum::http::HeaderMap,
        secret: &str,
    ) -> (axum::http::StatusCode, String, bool) {
        let response = *super::require_url_auth(state, headers, secret)
            .err()
            .expect("request must be refused");
        let status = response.status();
        let cors = response
            .headers()
            .contains_key(axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN);
        assert!(!response
            .headers()
            .contains_key(axum::http::header::WWW_AUTHENTICATE));
        let body =
            gld_core::async_rt::block_on(axum::body::to_bytes(response.into_body(), 4096)).unwrap();
        let text = String::from_utf8_lossy(&body).into_owned();
        assert!(!text.contains(secret), "secret echoed in {text}");
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        (status, value["error"].as_str().unwrap().to_string(), cors)
    }

    #[test]
    fn url_auth_rechecks_secret_origin_scope_and_revocation_without_restarting_listener() {
        use axum::http::StatusCode;
        run_isolated(
            module_path!(),
            "url_auth_rechecks_secret_origin_scope_and_revocation_without_restarting_listener",
            || {
                let root = tempfile::tempdir().unwrap();
                let store = AccessStore::open_in(root.path()).unwrap();
                let mut state = state_with_access(root.path(), "api", store.reader());
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                let wrong = "gld_url_v1.0123456789abcdef0123456789abcdef.\
                    AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

                // 还没有任何授权：格式合法的地址也只得到 503，不是 401。
                assert_eq!(
                    url_failure(&state, &url_headers(Some(EXTENSION)), wrong).0,
                    StatusCode::SERVICE_UNAVAILABLE
                );

                let issued = store
                    .create_grant(
                        GrantCreation {
                            name: "Sidebar".into(),
                            scope: ProjectScope::Selected(vec!["w1".into()]),
                            permissions: GrantPermissions::default(),
                            kind: CredentialKind::Url,
                            ttl: None,
                            allowed_origins: vec![EXTENSION.into()],
                        },
                        now,
                    )
                    .unwrap();
                let secret = issued.secret.into_string();

                let first = super::require_url_auth(&state, &url_headers(None), &secret)
                    .ok()
                    .unwrap();
                assert_eq!(first.origin, None);
                assert!(!first.auth.may_write_project());
                assert!(!first.auth.may_delegate());
                let listed =
                    super::require_url_auth(&state, &url_headers(Some(EXTENSION)), &secret)
                        .ok()
                        .unwrap();
                assert_eq!(listed.origin.as_deref(), Some(EXTENSION));
                assert_eq!(first.auth.id(), listed.auth.id());

                // 秘密有效但 Origin 不在名单里：403，不回显 Origin。
                for origin in ["https://untrusted.example.test", "null"] {
                    assert_eq!(
                        url_failure(&state, &url_headers(Some(origin)), &secret),
                        (StatusCode::FORBIDDEN, "ORIGIN_NOT_ALLOWED".into(), false)
                    );
                }
                // 秘密不对：不管 Origin 在不在名单里都是同一个 401。
                for origin in [
                    Some(EXTENSION),
                    Some("https://untrusted.example.test"),
                    None,
                ] {
                    for candidate in [wrong, "not-a-url-credential"] {
                        assert_eq!(
                            url_failure(&state, &url_headers(origin), candidate),
                            (
                                StatusCode::UNAUTHORIZED,
                                "AUTHENTICATION_FAILED".into(),
                                false
                            )
                        );
                    }
                }
                // 重复的 Origin 头无法对应到名单里的一项。
                let mut doubled = url_headers(Some(EXTENSION));
                doubled.append(axum::http::header::ORIGIN, EXTENSION.parse().unwrap());
                assert_eq!(
                    url_failure(&state, &doubled, &secret),
                    (StatusCode::BAD_REQUEST, "INVALID_ORIGIN".into(), false)
                );
                // 路径秘密之外再带别的凭据是来源冲突，不挑一个来用。
                let mut conflict = url_headers(Some(EXTENSION));
                conflict.insert(
                    axum::http::header::AUTHORIZATION,
                    format!("Bearer {secret}").parse().unwrap(),
                );
                assert_eq!(
                    url_failure(&state, &conflict, &secret).1,
                    "CONFLICTING_CREDENTIAL_SOURCES"
                );
                // URL 秘密不能改走标准入口的 Bearer。
                assert_eq!(
                    super::require_mcp_auth(&state, &conflict)
                        .unwrap_err()
                        .status(),
                    StatusCode::UNAUTHORIZED
                );

                // 权限、范围在下一次请求生效；单工作区监听器沿用项目范围和 confine-reads 的 403。
                let mut changed = issued.record.clone();
                changed.grant.permissions.write = true;
                store.update(changed.clone()).unwrap();
                let widened =
                    super::require_url_auth(&state, &url_headers(Some(EXTENSION)), &secret)
                        .ok()
                        .unwrap();
                assert!(widened.auth.may_write_project());
                assert!(!widened.auth.may_delegate());
                changed.grant.scope = ProjectScope::Selected(vec!["another-project".into()]);
                store.update(changed.clone()).unwrap();
                assert_eq!(
                    url_failure(&state, &url_headers(Some(EXTENSION)), &secret),
                    (StatusCode::FORBIDDEN, "PROJECT_ACCESS_DENIED".into(), true)
                );
                changed.grant.scope = ProjectScope::Selected(vec!["w1".into()]);
                store.update(changed).unwrap();
                state.workspace_confined = false;
                assert_eq!(
                    url_failure(&state, &url_headers(None), &secret),
                    (StatusCode::FORBIDDEN, "PROJECT_ACCESS_DENIED".into(), false)
                );
                state.workspace_confined = true;

                // 轮换：旧地址立刻失效，新地址沿用同一份 Origin 名单和同一个调用方。
                let rotated = store.rotate_grant("Sidebar", None, now).unwrap();
                let replacement = rotated.secret.into_string();
                assert_eq!(
                    url_failure(&state, &url_headers(Some(EXTENSION)), &secret).0,
                    StatusCode::UNAUTHORIZED
                );
                let after =
                    super::require_url_auth(&state, &url_headers(Some(EXTENSION)), &replacement)
                        .ok()
                        .unwrap();
                assert_eq!(first.auth.id(), after.auth.id());
                assert_eq!(
                    url_failure(
                        &state,
                        &url_headers(Some("https://untrusted.example.test")),
                        &replacement
                    )
                    .0,
                    StatusCode::FORBIDDEN
                );

                // 撤销后不再透露 Origin 是否在名单里。
                store.revoke(&issued.record.grant.id).unwrap();
                for origin in [
                    Some(EXTENSION),
                    Some("https://untrusted.example.test"),
                    None,
                ] {
                    assert_eq!(
                        url_failure(&state, &url_headers(origin), &replacement).0,
                        StatusCode::UNAUTHORIZED
                    );
                }
            },
        );
    }

    #[test]
    fn url_cors_echoes_only_the_verified_origin() {
        use axum::http::header::{
            ACCESS_CONTROL_ALLOW_CREDENTIALS, ACCESS_CONTROL_ALLOW_HEADERS,
            ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
            ACCESS_CONTROL_EXPOSE_HEADERS, ORIGIN,
        };
        run_isolated(
            module_path!(),
            "url_cors_echoes_only_the_verified_origin",
            || {
                let root = tempfile::tempdir().unwrap();
                let store = AccessStore::open_in(root.path()).unwrap();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                let secret = store
                    .create_grant(
                        GrantCreation {
                            name: "Sidebar".into(),
                            scope: ProjectScope::AllProjects,
                            permissions: GrantPermissions::default(),
                            kind: CredentialKind::Url,
                            ttl: None,
                            allowed_origins: vec![EXTENSION.into()],
                        },
                        now,
                    )
                    .unwrap()
                    .secret
                    .into_string();
                let state = state_with_access(root.path(), "api", store.reader());
                let preflight = |origin: Option<&str>, secret: &str| {
                    let mut headers = url_headers(origin);
                    headers.insert("access-control-request-method", "POST".parse().unwrap());
                    gld_core::async_rt::block_on(super::mcp_url_preflight(
                        axum::extract::State(state.clone()),
                        axum::extract::Path(secret.to_string()),
                        headers,
                    ))
                };

                let allowed = preflight(Some(EXTENSION), &secret);
                assert_eq!(allowed.status(), axum::http::StatusCode::NO_CONTENT);
                assert_eq!(allowed.headers()[ACCESS_CONTROL_ALLOW_ORIGIN], EXTENSION);
                assert_eq!(
                    allowed.headers()[ACCESS_CONTROL_ALLOW_METHODS],
                    "POST, OPTIONS"
                );
                let request_headers = allowed.headers()[ACCESS_CONTROL_ALLOW_HEADERS]
                    .to_str()
                    .unwrap();
                for name in [
                    "content-type",
                    "accept",
                    "mcp-session-id",
                    "mcp-protocol-version",
                    "last-event-id",
                ] {
                    assert!(request_headers.split(", ").any(|item| item == name));
                }
                assert!(!request_headers.contains("authorization"));
                assert!(allowed.headers()[ACCESS_CONTROL_EXPOSE_HEADERS]
                    .to_str()
                    .unwrap()
                    .split(", ")
                    .any(|item| item == "mcp-session-id"));
                assert!(!allowed
                    .headers()
                    .contains_key(ACCESS_CONTROL_ALLOW_CREDENTIALS));

                // 其余预检一律是不带 CORS 头的 204，彼此看不出差别。
                let wrong = format!("{secret}x");
                for (origin, candidate) in [
                    (Some("https://untrusted.example.test"), secret.as_str()),
                    (Some("null"), secret.as_str()),
                    (Some(EXTENSION), wrong.as_str()),
                    (None, secret.as_str()),
                ] {
                    let refused = preflight(origin, candidate);
                    assert_eq!(refused.status(), axum::http::StatusCode::NO_CONTENT);
                    for name in [
                        ACCESS_CONTROL_ALLOW_ORIGIN,
                        ACCESS_CONTROL_ALLOW_METHODS,
                        ACCESS_CONTROL_ALLOW_HEADERS,
                        ACCESS_CONTROL_EXPOSE_HEADERS,
                    ] {
                        assert!(!refused.headers().contains_key(name), "{origin:?}");
                    }
                }

                // 实际请求：通过的回显自己的 Origin，没带 Origin 的不加 CORS 头。
                let mut headers = url_headers(Some(EXTENSION));
                headers.insert(ORIGIN, EXTENSION.parse().unwrap());
                let get = gld_core::async_rt::block_on(super::mcp_url_no_stream(
                    axum::extract::State(state.clone()),
                    axum::extract::Path(secret.clone()),
                    headers,
                ));
                assert_eq!(get.status(), axum::http::StatusCode::METHOD_NOT_ALLOWED);
                assert_eq!(get.headers()[ACCESS_CONTROL_ALLOW_ORIGIN], EXTENSION);
                assert!(get.headers().contains_key(ACCESS_CONTROL_EXPOSE_HEADERS));
                assert!(!get.headers().contains_key(ACCESS_CONTROL_ALLOW_CREDENTIALS));
                let plain = gld_core::async_rt::block_on(super::mcp_url_no_stream(
                    axum::extract::State(state.clone()),
                    axum::extract::Path(secret.clone()),
                    url_headers(None),
                ));
                assert_eq!(plain.status(), axum::http::StatusCode::METHOD_NOT_ALLOWED);
                assert!(!plain.headers().contains_key(ACCESS_CONTROL_ALLOW_ORIGIN));
            },
        );
    }
}

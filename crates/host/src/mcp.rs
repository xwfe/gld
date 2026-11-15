use std::path::PathBuf;
use std::sync::Arc;

use crate::listeners::ShutdownSender;
use crate::nodes::{endpoint, registry::Nodes};
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Form, Path, Query, State};
use axum::http::{
    header::{
        ACCEPT, ALLOW, AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE, REFERRER_POLICY, USER_AGENT,
    },
    HeaderMap, HeaderName, Method, StatusCode,
};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use gld_core::async_rt::{self, JoinHandle};
use gld_core::generation::CallError;
use gld_core::hub::{Hub, HubSecrets, HUB_SCOPE, SERVER_NAME};
use gld_core::local_network;
use gld_core::logs::append_profile_log;
use gld_core::mcp::handle_request;
use gld_core::settings::AppSettings;
use gld_core::tools::{build_tool_context, patch::failure_log_fields, SharedToolContext};
use gld_core::usage::ServiceUsage;
use gld_core::workspace::{AuthConfig, RuntimeConfig};
use serde_json::{json, Value};
use tokio::sync::oneshot;
use tower_http::cors::{Any, CorsLayer};

use crate::auth::{
    authorization_server_metadata, authorize_get, authorize_post, external_base_url, hub_audience,
    protected_resource_metadata, register_client, token_exchange, workspace_audience, AuthContext,
    AuthorizeForm, AuthorizeParams, ClientRegistrationRequest, ClientRegistry, OAuthRuntime,
    TokenForm,
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
    generation: Option<Arc<GenerationRouter>>,
    /// 日志目录和 OAuth 客户端注册表的作用域：工作区 id，或 [`HUB_SCOPE`]。
    scope: String,
    /// 授权页上告诉用户"你在授权什么"的那一行。
    authorize_label: String,
    bind_port: u16,
    configured_public_url: String,
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
) -> Result<(ShutdownSender, JoinHandle<()>), String> {
    let workspace_display = workspace_path.display().to_string();
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
    let configured_public_url = public_base_url.trim().to_string();
    let oauth = Some(Arc::new(OAuthRuntime::new_mcp_pending(
        workspace_audience(&workspace_id),
        auth.oauth_client_id.clone(),
        oauth_client_secret.clone(),
        oauth_token_secret.unwrap_or_default(),
        Arc::new(ClientRegistry::load(&workspace_id, &workspace_id)),
    )));
    listen(ListenerState {
        endpoint: Endpoint::Workspace(mcp),
        generation,
        scope: workspace_id,
        authorize_label: workspace_display,
        bind_port: port,
        configured_public_url,
        oauth,
        oauth_client_secret,
    })
}

/// 起聚合入口的监听器。路由、认证、日志和工作区监听器是同一套，只是答请求的换成 [`Hub`]。
/// `nodes` 必须是宿主注入该 Hub 的同一张节点表，停止时也由宿主显式关闭。
///
/// 令牌受众是 [`hub_audience`]，客户端注册表是独立的 [`HUB_SCOPE`]：工作区发出去的令牌
/// 进不了 hub，hub 的令牌也进不了任何工作区——拿到一个项目的授权不等于拿到全部。
pub fn spawn_hub_listener(
    port: u16,
    hub: Arc<Hub>,
    nodes: Arc<Nodes>,
    _auth_type: &str,
    public_base_url: String,
    secrets: HubSecrets,
    generation: Option<Arc<GenerationRouter>>,
) -> Result<(ShutdownSender, JoinHandle<()>), String> {
    let oauth = Some(Arc::new(OAuthRuntime::new_mcp_pending(
        hub_audience(),
        secrets.oauth_client_id,
        None,
        secrets.oauth_token_secret,
        Arc::new(ClientRegistry::load(HUB_SCOPE, HUB_SCOPE)),
    )));
    listen(ListenerState {
        endpoint: Endpoint::Hub { hub, nodes },
        generation,
        scope: HUB_SCOPE.into(),
        authorize_label: "gld 客户端授权".into(),
        bind_port: port,
        configured_public_url: public_base_url.trim().to_string(),
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

fn resolve_oauth_base(state: &ListenerState, headers: &HeaderMap) -> String {
    external_base_url(headers, state.bind_port, &state.configured_public_url)
}

/// URL-only 客户端不一定实现 OAuth / 自定义认证头。独立短期 grant 复用现有
/// Hub 和权限检查，不开无鉴权入口，也不维护另一套 MCP 会话或执行器。
fn url_router() -> Router<ListenerState> {
    Router::new()
        .route(
            "/mcp/connect/{token}",
            post(mcp_url_post)
                .get(mcp_url_no_stream)
                .delete(mcp_url_no_stream),
        )
        .layer(
            CorsLayer::new()
                // 不用跨源 cookie。允许预检，实际请求逐次核验 grant 的 Origin。
                // CORS 不是鉴权；含密钥的 URL 也必须通过有效期和项目范围检查。
                .allow_origin(Any)
                .allow_methods([Method::POST, Method::GET, Method::DELETE, Method::OPTIONS])
                .allow_headers([
                    CONTENT_TYPE,
                    ACCEPT,
                    AUTHORIZATION,
                    HeaderName::from_static("mcp-session-id"),
                    HeaderName::from_static("mcp-protocol-version"),
                    HeaderName::from_static("last-event-id"),
                ])
                .expose_headers([
                    HeaderName::from_static("mcp-session-id"),
                    HeaderName::from_static("mcp-protocol-version"),
                ]),
        )
        .layer(middleware::map_response(url_response))
}

async fn url_response(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(CACHE_CONTROL, "no-store".parse().expect("static header"));
    response.headers_mut().insert(
        REFERRER_POLICY,
        "no-referrer".parse().expect("static header"),
    );
    response
}

fn require_url_auth(
    _state: &ListenerState,
    _headers: &HeaderMap,
    _token: &str,
) -> Result<AuthContext, Box<Response>> {
    Err(Box::new(mcp_auth_not_ready()))
}

async fn mcp_url_post(
    State(state): State<ListenerState>,
    Path(token): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let auth = match require_url_auth(&state, &headers, &token) {
        Ok(auth) => auth,
        Err(response) => return *response,
    };
    if let Err(response) = require_url_protocol(&headers) {
        return *response;
    }
    dispatch_mcp(state, auth, body).await
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
    if let Err(response) = require_url_auth(&state, &headers, &token) {
        return *response;
    }
    if let Err(response) = require_url_protocol(&headers) {
        return *response;
    }
    (StatusCode::METHOD_NOT_ALLOWED, [(ALLOW, "POST, OPTIONS")]).into_response()
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

/// No network MCP identity is constructed before the new credential store is connected.
fn require_mcp_auth(
    _state: &ListenerState,
    _headers: &HeaderMap,
) -> Result<AuthContext, Box<Response>> {
    Err(Box::new(mcp_auth_not_ready()))
}

fn mcp_auth_not_ready() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(CACHE_CONTROL, "no-store")],
        Json(json!({
            "error": "AUTH_NOT_READY",
            "message": "统一 MCP 授权尚未接入；当前入口拒绝匿名、旧凭据及未绑定 grant 的隧道请求"
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
    use gld_core::tools::{PolicySettings, ToolContext, Workspace};
    use gld_core::workspace::AuthConfig;

    use super::{bind_listener, mcp_discovery, mcp_discovery_payload, Endpoint, ListenerState};
    use crate::auth::test_home::run_isolated;

    fn state_named(root: &Path, workspace_name: &str) -> ListenerState {
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
        .with_workspace_name(workspace_name);
        ListenerState {
            endpoint: Endpoint::Workspace(Arc::new(context)),
            generation: None,
            scope: "w1".into(),
            authorize_label: "/tmp/x".into(),
            bind_port: 0,
            configured_public_url: String::new(),
            oauth: None,
            oauth_client_secret: None,
        }
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
                    axum::http::StatusCode::SERVICE_UNAVAILABLE
                );
                let response =
                    super::require_url_auth(&state, &headers, "old-url-secret").unwrap_err();
                let body =
                    gld_core::async_rt::block_on(axum::body::to_bytes(response.into_body(), 4096))
                        .unwrap();
                let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(value["error"], "AUTH_NOT_READY");
                assert!(!String::from_utf8_lossy(&body).contains("old-url-secret"));
            },
        );
    }
}

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Form, Query, State};
use axum::http::{
    header::{AUTHORIZATION, CACHE_CONTROL, USER_AGENT, WWW_AUTHENTICATE},
    HeaderMap, StatusCode,
};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::sync::oneshot;
use tower_http::cors::CorsLayer;

use crate::auth::{
    authorization_server_metadata, authorize_get, authorize_post, external_base_url, hub_audience,
    protected_resource_metadata, protected_resource_metadata_url, register_client, token_exchange,
    verify_bearer_header, verify_oauth_bearer_header, workspace_audience, AuthContext,
    AuthorizeForm, AuthorizeParams, ClientRegistrationRequest, ClientRegistry, OAuthRuntime,
    Principal, TokenForm,
};
use crate::hub::{Hub, HubSecrets, HUB_SCOPE};
use crate::local_network;
use crate::logs::append_profile_log;
use crate::mcp::server::{handle_request, SharedState};
use crate::secret::SecretStore;
use crate::settings::AppSettings;
use crate::tools::{build_tool_context, Caller, SharedToolContext};
use crate::usage::ServiceUsage;
use crate::workspace::{AuthConfig, RuntimeConfig};

pub type ShutdownSender = oneshot::Sender<()>;

/// 监听器背后是谁在答 JSON-RPC。
///
/// 认证、OAuth 路由、请求日志工作区和 hub 完全一样，只有这一处不同。
/// 分成两份监听器的话，下次修一个 OAuth 的坑就得记得修两遍。
#[derive(Clone)]
enum Endpoint {
    /// 一个工作区自己的 MCP 服务。
    Workspace(SharedState),
    /// 聚合入口：每次调用按 `workspace` 参数分到成员，见 [`crate::hub`]。
    Hub(Arc<Hub>),
}

/// 一条请求处理完的结果。
struct Handled {
    response: Value,
    /// 这次实际用到的工具上下文，取上下文审计用；hub 没路由到成员时为 None。
    context: Option<SharedToolContext>,
    /// hub 请求落到的成员 id。工作区自己的监听器永远是 None。
    member: Option<String>,
}

impl Endpoint {
    fn server_name(&self) -> String {
        match self {
            Self::Workspace(context) => context.server_name().to_string(),
            Self::Hub(_) => crate::hub::SERVER_NAME.to_string(),
        }
    }

    fn usage(&self) -> Arc<ServiceUsage> {
        match self {
            Self::Workspace(context) => context.usage(),
            Self::Hub(hub) => hub.usage(),
        }
    }

    /// 同步跑工具，必须在 `spawn_blocking` 里调。
    ///
    /// 两支都要 `auth`：它是这次调用的主体，命令会话按它分表。不传的话，
    /// 这个入口上所有连接共用一张表，谁都能拿别人的 `session_id` 去读
    /// （见 [`crate::tools::caller`]）。
    fn handle(&self, auth: &AuthContext, body: &Value) -> Handled {
        match self {
            Self::Workspace(context) => Handled {
                response: handle_request(context, &Caller::from_auth(auth), body),
                context: Some(context.clone()),
                member: None,
            },
            Self::Hub(hub) => {
                let (response, routed) = hub.handle_request(auth, body);
                Handled {
                    response,
                    member: routed.as_ref().map(|routed| routed.workspace_id.clone()),
                    // 远端成员没有本机工具上下文，取上下文审计这一段跳过。
                    context: routed.and_then(|routed| routed.context),
                }
            }
        }
    }
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
    auth: AuthConfig,
    /// 日志目录和 OAuth 客户端注册表的作用域：工作区 id，或 [`HUB_SCOPE`]。
    scope: String,
    /// 授权页上告诉用户"你在授权什么"的那一行。
    authorize_label: String,
    bind_port: u16,
    configured_public_url: String,
    bearer_token: Option<String>,
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
    oauth_password: Option<String>,
    oauth_token_secret: Option<String>,
    runtime: RuntimeConfig,
    usage: Arc<ServiceUsage>,
) -> Result<(ShutdownSender, crate::async_rt::JoinHandle<()>), String> {
    let workspace_display = workspace_path.display().to_string();
    let global = AppSettings::load_or_default();
    // 和命令行 `gld tool call` 共用同一个构建入口，两边看到的工具集与策略必然一致。
    let mcp: SharedState = Arc::new(build_tool_context(
        workspace_path,
        &workspace_name,
        auth.clone(),
        &runtime,
        &global,
        usage,
    )?);
    let bearer_token = if auth.bearer_enabled() {
        let key = "bearer_token";
        if auth.use_shared_secrets {
            SecretStore::get_shared(key).map_err(|e| e.to_string())?
        } else {
            SecretStore::get(&workspace_id, key).map_err(|e| e.to_string())?
        }
    } else {
        None
    };
    // 没有 token 就不要起来。以前是照起不误，结果服务显示 running、
    // 端口也通，但**任何**请求都拿 401——包括配置正确的客户端。
    // 用户看到的是"服务好好的，客户端连不上"，最难查的一类问题。
    // Actions 那侧一直是这么做的（"Actions API key is not configured"），
    // 这里补齐。
    if auth.bearer_enabled() && bearer_token.as_deref().is_none_or(str::is_empty) {
        let scope = if auth.use_shared_secrets {
            "gld secret shared regen bearer_token"
        } else {
            "gld secret regen bearer_token"
        };
        return Err(format!(
            "MCP 认证方式是 bearer，但没有 bearer_token，任何客户端都会拿 401。\
             先执行 `{scope}`，或改用 `gld ws set mcp.auth=oauth`。"
        ));
    }
    let configured_public_url = public_base_url.trim().to_string();
    let oauth = auth.oauth_enabled().then(|| {
        Arc::new(OAuthRuntime::new(
            workspace_audience(&workspace_id),
            auth.oauth_client_id.clone(),
            oauth_client_secret.clone(),
            oauth_password.unwrap_or_default(),
            oauth_token_secret.unwrap_or_default(),
            Arc::new(ClientRegistry::load(&workspace_id, &workspace_id)),
        ))
    });
    listen(ListenerState {
        endpoint: Endpoint::Workspace(mcp),
        auth,
        scope: workspace_id,
        authorize_label: workspace_display,
        bind_port: port,
        configured_public_url,
        bearer_token,
        oauth,
        oauth_client_secret,
    })
}

/// 起聚合入口的监听器。路由、认证、日志和工作区监听器是同一套，只是答请求的换成 [`Hub`]。
///
/// 令牌受众是 [`hub_audience`]，客户端注册表是独立的 [`HUB_SCOPE`]：工作区发出去的令牌
/// 进不了 hub，hub 的令牌也进不了任何工作区——拿到一个项目的授权不等于拿到全部。
pub fn spawn_hub_listener(
    port: u16,
    hub: Arc<Hub>,
    auth_type: &str,
    public_base_url: String,
    secrets: HubSecrets,
) -> Result<(ShutdownSender, crate::async_rt::JoinHandle<()>), String> {
    let auth = AuthConfig {
        auth_type: auth_type.to_string(),
        oauth_client_id: secrets.oauth_client_id.clone(),
        use_shared_secrets: false,
    };
    if auth.bearer_enabled() && secrets.bearer_token.is_empty() {
        return Err(
            "聚合入口的认证方式是 bearer，但没有 bearer_token，任何客户端都会拿 401。\
             先执行 `gld hub regen bearer_token`。"
                .into(),
        );
    }
    let oauth = auth.oauth_enabled().then(|| {
        Arc::new(OAuthRuntime::new(
            hub_audience(),
            secrets.oauth_client_id.clone(),
            None,
            secrets.oauth_password.clone(),
            secrets.oauth_token_secret.clone(),
            Arc::new(ClientRegistry::load(HUB_SCOPE, HUB_SCOPE)),
        ))
    });
    let bearer_token = auth.bearer_enabled().then_some(secrets.bearer_token);
    listen(ListenerState {
        endpoint: Endpoint::Hub(hub),
        auth,
        scope: HUB_SCOPE.into(),
        authorize_label: "gld 聚合入口（hub）：授权后可访问它的全部成员工作区".into(),
        bind_port: port,
        configured_public_url: public_base_url.trim().to_string(),
        bearer_token,
        oauth,
        oauth_client_secret: None,
    })
}

fn listen(
    state: ListenerState,
) -> Result<(ShutdownSender, crate::async_rt::JoinHandle<()>), String> {
    let port = state.bind_port;
    // 在返回 Running 之前完成 bind，避免后台任务里的端口冲突被伪装成启动成功。
    let allow_lan_access = AppSettings::load_or_default().allow_lan_access;
    let listener = bind_listener(port, allow_lan_access)?;
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let profile_id = state.scope.clone();
    let handle = crate::async_rt::spawn(async move {
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
        .with_state(state)
        .layer(CorsLayer::permissive());

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

fn mcp_discovery_payload(state: &ListenerState) -> Value {
    json!({
        "name": state.endpoint.server_name(),
        "version": env!("CARGO_PKG_VERSION"),
        "protocolVersion": "2025-06-18"
    })
}

fn resolve_oauth_base(state: &ListenerState, headers: &HeaderMap) -> String {
    external_base_url(headers, state.bind_port, &state.configured_public_url)
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
            "[rpc] request id={} method={} tool={} auth={}",
            request_id,
            method,
            tool_name,
            auth.tag()
        ),
    );

    let endpoint = state.endpoint.clone();
    let result = tokio::task::spawn_blocking(move || endpoint.handle(&auth, &body)).await;
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
            let audit = handled
                .context
                .as_ref()
                .map(|context| context.context_audit_snapshot())
                .unwrap_or(Value::Null);
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
            Json(response).into_response()
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

/// 验这条请求的鉴权，验过了带出**是怎么过的**。
///
/// 以前这里只回答放行还是 401。远端成员要知道是谁在调：一条通往远端的 bridge
/// 属于某个主体，不属于某个工作区名字（RFC-0002 5.3）。原始令牌到此为止，
/// 不进 [`AuthContext`]，也就进不了日志和错误消息。
fn require_mcp_auth(
    state: &ListenerState,
    headers: &HeaderMap,
) -> Result<AuthContext, Box<Response>> {
    if state.auth.bearer_enabled() {
        let expected = state.bearer_token.as_deref().unwrap_or("");
        return match verify_bearer_header(headers, expected) {
            Some(refused) => Err(Box::new(refused)),
            None => Ok(AuthContext::new(Principal::SharedSecret, &state.scope)),
        };
    }
    if state.auth.oauth_enabled() {
        if let Some(oauth) = state.oauth.as_ref() {
            let server_url = resolve_oauth_base(state, headers);
            return match verify_oauth_bearer_header(headers, oauth, &server_url) {
                Ok(client_id) => Ok(AuthContext::new(
                    Principal::OAuthClient { client_id },
                    &state.scope,
                )),
                Err(mut response) => {
                    if response.status() == StatusCode::UNAUTHORIZED {
                        let metadata_url = protected_resource_metadata_url(&server_url);
                        if let Ok(value) =
                            format!("Bearer resource_metadata=\"{metadata_url}\"").parse()
                        {
                            response.headers_mut().insert(WWW_AUTHENTICATE, value);
                        }
                    }
                    Err(response)
                }
            };
        }
    }
    Ok(AuthContext::anonymous(&state.scope))
}

async fn oauth_authorization_server_metadata(
    State(state): State<ListenerState>,
    headers: HeaderMap,
) -> Response {
    if !state.auth.oauth_enabled() {
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
    if !state.auth.oauth_enabled() {
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

#[cfg(test)]
mod tests {
    use axum::http::header::CACHE_CONTROL;
    use axum::response::IntoResponse;

    use super::{bind_listener, mcp_discovery, mcp_discovery_payload, Endpoint, ListenerState};
    use crate::tools::ToolContext;
    use crate::workspace::AuthConfig;
    use axum::extract::State;
    use std::sync::Arc;

    fn state_named(workspace_name: &str) -> ListenerState {
        let workspace = tempfile::tempdir().expect("workspace");
        let harness = tempfile::tempdir().expect("harness");
        let context = ToolContext::for_test(workspace.keep(), harness.keep())
            .expect("context")
            .with_workspace_name(workspace_name);
        ListenerState {
            endpoint: Endpoint::Workspace(Arc::new(context)),
            auth: AuthConfig::default(),
            scope: "w1".into(),
            authorize_label: "/tmp/x".into(),
            bind_port: 0,
            configured_public_url: String::new(),
            bearer_token: None,
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

    #[tokio::test]
    async fn discovery_reports_the_current_package_version() {
        let discovery = mcp_discovery_payload(&state_named("api"));

        assert_eq!(discovery["version"], env!("CARGO_PKG_VERSION"));
    }

    /// 服务器名要跟着工作区走。
    ///
    /// 这里以前写死 `coding-tools-mcp`（上游项目的名字）：接了三个工作区，
    /// 客户端的服务器列表里就是三个一模一样的条目，谁也分不出谁是哪个项目。
    #[tokio::test]
    async fn discovery_names_the_workspace() {
        assert_eq!(mcp_discovery_payload(&state_named("api"))["name"], "api");
        // 没有工作区名时（命令行直连、测试）回落到项目名，不能是空串——
        // 空的 serverInfo.name 在有些客户端那儿直接显示成一行空白。
        assert_eq!(mcp_discovery_payload(&state_named(""))["name"], "gld");
    }

    #[tokio::test]
    async fn discovery_prevents_stale_tool_catalog_caching() {
        let response = mcp_discovery(State(state_named("api")))
            .await
            .into_response();

        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
    }
}

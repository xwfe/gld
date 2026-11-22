use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::{header::AUTHORIZATION, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use gld_core::auth::{
    bearer_token, constant_time_eq_str, parse_credential, AccessReader, AuthContext,
    ClientRegistry, CredentialKind, OAuthBinding, RegisteredClient, RegistrationError,
};

pub const OAUTH_CODE_TTL_SECONDS: u64 = 300;
/// 同时等待兑换的授权码上限；每个码最多占用几百字节。
const MAX_PENDING_CODES: usize = 256;
pub const OAUTH_TOKEN_TTL_SECONDS: i64 = 60 * 60 * 24 * 30;
pub const OAUTH_REFRESH_TOKEN_TTL_SECONDS: i64 = 60 * 60 * 24 * 90;
const ACTIONS_CLAIMS_VERSION: u32 = 2;
/// MCP 的 claims 带 grant 绑定，字段和版本都与 Actions 不同，两边的 token 互不通过。
const MCP_CLAIMS_VERSION: u32 = 3;

#[derive(Clone)]
pub struct OAuthRuntime {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub token_secret: String,
    /// 令牌里写的 `iss`/`aud`。用的是工作区标识而不是公网地址——
    /// 地址是会变的（临时隧道每次重启换一个、换域名、开关全局入口），
    /// 绑地址就等于地址一动所有已发令牌全废，用户得重新授权。
    /// 工作区标识不变，加上每个工作区自己的 `token_secret`，
    /// 已经能保证"A 工作区的令牌进不了 B 工作区"。
    audience: String,
    pending: Arc<Mutex<HashMap<String, PendingCode>>>,
    clients: Arc<ClientRegistry>,
    authorization: OAuthAuthorization,
}

#[derive(Clone)]
enum OAuthAuthorization {
    /// MCP 授权页收 OAuth 用途 grant 的确认口令（RFC-0015 §2.2）；签发和每次验证都读
    /// 与 App 写者共享的同一份授权快照，这里不保存认证结果。
    Mcp {
        access: AccessReader,
        /// 写进 CallerId 的入口名：工作区 id 或聚合入口。
        entry: String,
    },
    Actions {
        password: String,
    },
}

/// A verified Actions token. MCP tokens go through [`OAuthRuntime::authenticate_mcp_access`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessIdentity {
    pub client_id: String,
}

fn registration_error(error: &str, description: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        axum::Json(json!({
            "error": error,
            "error_description": description
        })),
    )
        .into_response()
}

fn registration_unavailable(error: RegistrationError) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        axum::Json(json!({
            "error": "temporarily_unavailable",
            "error_description": error.to_string()
        })),
    )
        .into_response()
}

fn valid_redirect_uri(uri: &str) -> bool {
    let uri = uri.trim();
    (uri.starts_with("https://") || uri.starts_with("http://")) && !uri.contains(['\r', '\n', '#'])
}

#[derive(Clone)]
struct PendingCode {
    code_challenge: String,
    client_id: String,
    redirect_uri: String,
    expires_at: u64,
    /// MCP 授权码记下确认口令当时对应的 grant 和凭据修订号；Actions 没有 grant。
    binding: Option<OAuthBinding>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenClaims {
    version: u32,
    iss: String,
    aud: String,
    iat: i64,
    exp: i64,
    scope: String,
    client_id: String,
    token_use: String,
}

/// MCP access/refresh 的 claims。`grant_id` 和 `credential_revision` 是签发时的绑定，
/// 验证时必须与当前授权快照一致；没有这两项的旧 token 反序列化就失败。
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpTokenClaims {
    version: u32,
    iss: String,
    aud: String,
    iat: i64,
    exp: i64,
    client_id: String,
    grant_id: String,
    credential_revision: u64,
    token_use: String,
}

impl McpTokenClaims {
    fn binding(&self) -> OAuthBinding {
        OAuthBinding {
            grant_id: self.grant_id.clone(),
            credential_revision: self.credential_revision,
        }
    }
}

impl OAuthRuntime {
    /// MCP 入口：`access` 必须是 App 授权写者发出的共享读者，`entry` 是该入口的作用域。
    pub fn new_mcp(
        audience: String,
        entry: String,
        client_id: String,
        client_secret: Option<String>,
        token_secret: String,
        clients: Arc<ClientRegistry>,
        access: AccessReader,
    ) -> Self {
        Self {
            client_id,
            client_secret,
            token_secret,
            audience,
            pending: Arc::new(Mutex::new(HashMap::new())),
            clients,
            authorization: OAuthAuthorization::Mcp { access, entry },
        }
    }

    /// Actions is a separate protocol with explicit workspace authority at its listener.
    pub fn new_actions(
        audience: String,
        client_id: String,
        client_secret: Option<String>,
        password: String,
        token_secret: String,
        clients: Arc<ClientRegistry>,
    ) -> Self {
        Self {
            client_id,
            client_secret,
            token_secret,
            audience,
            pending: Arc::new(Mutex::new(HashMap::new())),
            clients,
            authorization: OAuthAuthorization::Actions { password },
        }
    }

    /// 空签名密钥等于谁都能伪造 token：没有密钥的入口不签发也不接受 OAuth token。
    fn can_sign(&self) -> bool {
        !self.token_secret.is_empty()
    }

    /// 验证 `/mcp` 上的 OAuth access token：验签、版本、用途、受众、期限、client 仍已注册，
    /// 再用共享读者核对 grant 有效且凭据修订号仍是签发时那个。任何一步失败都返回 None，
    /// 调用方直接拒绝，不拿这个 token 去试别的凭据类型。
    pub fn authenticate_mcp_access(&self, token: &str) -> Option<AuthContext> {
        let OAuthAuthorization::Mcp { access, entry } = &self.authorization else {
            return None;
        };
        let claims = self.decode_mcp(token, "access")?;
        access
            .authenticate_oauth(entry, &claims.binding(), &claims.client_id, unix_now())
            .ok()
    }

    fn decode_mcp(&self, token: &str, kind: &str) -> Option<McpTokenClaims> {
        if !self.can_sign() {
            return None;
        }
        let claims: McpTokenClaims =
            decode_claims(token, &self.token_secret, &self.audience).ok()?;
        (claims.version == MCP_CLAIMS_VERSION
            && claims.token_use == mcp_token_use(kind)
            && self.client_id_allowed(&claims.client_id))
        .then_some(claims)
    }

    fn actions_enabled(&self) -> bool {
        matches!(self.authorization, OAuthAuthorization::Actions { .. })
    }

    pub fn client_id_allowed(&self, client_id: &str) -> bool {
        if client_id.is_empty() {
            return false;
        }
        (!self.client_id.is_empty() && constant_time_eq_str(client_id, &self.client_id))
            || self.clients.contains(client_id)
    }

    fn redirect_uri_allowed(&self, client_id: &str, redirect_uri: &str) -> bool {
        if !self.client_id.is_empty() && constant_time_eq_str(client_id, &self.client_id) {
            return !redirect_uri.trim().is_empty();
        }
        self.clients
            .get(client_id)
            .is_some_and(|client| client.redirect_uris.iter().any(|uri| uri == redirect_uri))
    }

    fn client_credentials_allowed(&self, client_id: &str, client_secret: &str) -> bool {
        if !self.client_id.is_empty() && constant_time_eq_str(client_id, &self.client_id) {
            return self
                .client_secret
                .as_deref()
                .is_none_or(|expected| constant_time_eq_str(client_secret, expected));
        }
        let Some(client) = self.clients.get(client_id) else {
            return false;
        };
        if client.token_endpoint_auth_method == "none" {
            return true;
        }
        client
            .client_secret
            .as_deref()
            .is_some_and(|expected| constant_time_eq_str(client_secret, expected))
    }

    pub fn register(
        &self,
        client_id: String,
        client: RegisteredClient,
    ) -> Result<(), RegistrationError> {
        self.clients.insert(client_id, client)
    }

    pub fn verify_access_token(&self, token: &str, server_url: &str) -> bool {
        self.identify_access_token(token, server_url).is_some()
    }

    pub fn identify_access_token(&self, token: &str, _server_url: &str) -> Option<AccessIdentity> {
        if !self.actions_enabled() {
            return None;
        }
        let claims = self.decode_current(token)?;
        if claims.token_use != token_use("access") || !self.client_id_allowed(&claims.client_id) {
            return None;
        }
        Some(AccessIdentity {
            client_id: claims.client_id,
        })
    }

    fn decode_current(&self, token: &str) -> Option<TokenClaims> {
        let claims: TokenClaims = decode_claims(token, &self.token_secret, &self.audience).ok()?;
        (claims.version == ACTIONS_CLAIMS_VERSION
            && claims.scope == "actions"
            && !claims.client_id.is_empty())
        .then_some(claims)
    }
}

#[derive(Debug, Deserialize)]
pub struct ClientRegistrationRequest {
    pub redirect_uris: Vec<String>,
    #[serde(default)]
    pub token_endpoint_auth_method: String,
    #[serde(default)]
    pub grant_types: Vec<String>,
    #[serde(default)]
    pub response_types: Vec<String>,
    #[serde(default)]
    pub client_name: String,
}

pub fn register_client(oauth: &OAuthRuntime, request: ClientRegistrationRequest) -> Response {
    if request.redirect_uris.is_empty()
        || request
            .redirect_uris
            .iter()
            .any(|uri| !valid_redirect_uri(uri))
    {
        return registration_error(
            "invalid_redirect_uri",
            "redirect_uris must contain valid http(s) URLs",
        );
    }
    if !request.grant_types.is_empty()
        && request
            .grant_types
            .iter()
            .any(|grant| grant != "authorization_code" && grant != "refresh_token")
    {
        return registration_error("invalid_client_metadata", "Unsupported grant_types");
    }
    if !request.response_types.is_empty()
        && request
            .response_types
            .iter()
            .any(|response| response != "code")
    {
        return registration_error(
            "invalid_client_metadata",
            "Only response_type code is supported",
        );
    }
    let auth_method = match request.token_endpoint_auth_method.trim() {
        "" | "none" => "none",
        "client_secret_post" => "client_secret_post",
        "client_secret_basic" => "client_secret_basic",
        _ => {
            return registration_error(
                "invalid_client_metadata",
                "Unsupported token_endpoint_auth_method",
            )
        }
    };
    let client_id = format!("dcr-{}", uuid::Uuid::new_v4().simple());
    let client_secret = (auth_method != "none").then(|| uuid::Uuid::new_v4().simple().to_string());
    if let Err(error) = oauth.register(
        client_id.clone(),
        RegisteredClient {
            redirect_uris: request.redirect_uris.clone(),
            token_endpoint_auth_method: auth_method.to_string(),
            client_secret: client_secret.clone(),
            client_name: request.client_name.trim().to_string(),
            created_at: unix_now(),
        },
    ) {
        return registration_unavailable(error);
    }

    let mut body = json!({
        "client_id": client_id,
        "client_id_issued_at": unix_now(),
        "redirect_uris": request.redirect_uris,
        "token_endpoint_auth_method": auth_method,
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"]
    });
    if !request.client_name.trim().is_empty() {
        body["client_name"] = Value::String(request.client_name);
    }
    if let Some(secret) = client_secret {
        body["client_secret"] = Value::String(secret);
    }
    (StatusCode::CREATED, axum::Json(body)).into_response()
}

/// Verify an Actions access token and return only its signed, registered client ID.
pub fn verify_oauth_bearer_header(
    headers: &HeaderMap,
    oauth: &OAuthRuntime,
    server_url: &str,
) -> Result<AccessIdentity, Box<Response>> {
    if headers.get_all(AUTHORIZATION).iter().count() != 1 {
        return Err(Box::new(
            (
                StatusCode::UNAUTHORIZED,
                "Exactly one Authorization header is required",
            )
                .into_response(),
        ));
    }
    // Err 那支装箱：成功路径不为 HTTP 错误响应增大。
    let refused = || -> Box<Response> {
        Box::new((StatusCode::UNAUTHORIZED, "Invalid bearer token").into_response())
    };
    let Some(header_value) = headers.get(AUTHORIZATION) else {
        return Err(Box::new(
            (StatusCode::UNAUTHORIZED, "Missing Authorization header").into_response(),
        ));
    };
    let Ok(header_str) = header_value.to_str() else {
        return Err(Box::new(
            (StatusCode::UNAUTHORIZED, "Invalid Authorization header").into_response(),
        ));
    };
    // 和静态 bearer 走同一个解析：scheme 大小写不敏感，空 token 直接算无效。
    let Some(token) = bearer_token(header_str) else {
        return Err(refused());
    };
    oauth
        .identify_access_token(token, server_url)
        .ok_or_else(refused)
}

#[derive(Debug, Deserialize)]
pub struct AuthorizeParams {
    pub response_type: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub code_challenge_method: String,
    #[serde(default)]
    pub state: String,
}

#[derive(Debug, Deserialize)]
pub struct AuthorizeForm {
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub code_challenge_method: String,
    #[serde(default)]
    pub state: String,
    pub password: String,
}

#[derive(Debug, Deserialize, Default)]
pub struct TokenForm {
    pub grant_type: String,
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub redirect_uri: String,
    #[serde(default)]
    pub code_verifier: String,
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    #[serde(default)]
    pub refresh_token: String,
}

pub fn authorize_get(
    oauth: &OAuthRuntime,
    params: AuthorizeParams,
    workspace_path: Option<&str>,
    server_url: &str,
) -> Response {
    if !oauth.can_sign() {
        return oauth_unavailable();
    }
    if params.response_type != "code" {
        return html_error("response_type must be 'code'", StatusCode::BAD_REQUEST);
    }
    if !oauth.client_id_allowed(&params.client_id) {
        return html_error("Unknown client_id", StatusCode::BAD_REQUEST);
    }
    if !oauth.redirect_uri_allowed(&params.client_id, &params.redirect_uri) {
        return html_error(
            "redirect_uri is not registered for this client",
            StatusCode::BAD_REQUEST,
        );
    }
    if params.code_challenge_method != "S256" || params.code_challenge.is_empty() {
        return html_error(
            "code_challenge_method must be S256 and code_challenge is required",
            StatusCode::BAD_REQUEST,
        );
    }
    page(
        StatusCode::OK,
        login_page(
            &params.client_id,
            &params.redirect_uri,
            &params.code_challenge,
            &params.code_challenge_method,
            &params.state,
            "",
            workspace_path,
            server_url,
        ),
    )
}

pub fn authorize_post(oauth: &OAuthRuntime, form: AuthorizeForm, server_url: &str) -> Response {
    if !oauth.can_sign() {
        return oauth_unavailable();
    }
    // 出错时重画授权页；页面里只回填客户端参数，从不回填或回显提交的口令。
    let retry = |status: StatusCode, error: &str| {
        page(
            status,
            login_page(
                &form.client_id,
                &form.redirect_uri,
                &form.code_challenge,
                &form.code_challenge_method,
                &form.state,
                error,
                None,
                server_url,
            ),
        )
    };
    if !oauth.client_id_allowed(&form.client_id) {
        return retry(StatusCode::OK, "Invalid client");
    }
    if !oauth.redirect_uri_allowed(&form.client_id, &form.redirect_uri) {
        return html_error(
            "redirect_uri is not registered for this client",
            StatusCode::BAD_REQUEST,
        );
    }
    if form.code_challenge_method != "S256" || !valid_s256_challenge(&form.code_challenge) {
        return retry(StatusCode::OK, "Invalid PKCE parameters");
    }
    let binding = match &oauth.authorization {
        OAuthAuthorization::Actions { password } => {
            if password.is_empty() || !constant_time_eq_str(&form.password, password) {
                return retry(StatusCode::UNAUTHORIZED, "Invalid password");
            }
            None
        }
        OAuthAuthorization::Mcp { access, .. } => {
            // 公开 ID 不存在、用途不是 OAuth、秘密错误、已轮换或已撤销，页面上是同一句话。
            let confirmed = parse_credential(&form.password, CredentialKind::OAuth)
                .ok()
                .and_then(|id| access.confirm_oauth(id, &form.password, unix_now()).ok());
            let Some(binding) = confirmed else {
                return retry(StatusCode::UNAUTHORIZED, "Invalid authorization credential");
            };
            Some(binding)
        }
    };

    let code = uuid::Uuid::new_v4().to_string().replace('-', "");
    let now = unix_now();
    {
        let mut pending = oauth.pending.lock().expect("oauth pending lock");
        pending.retain(|_, v| v.expires_at >= now);
        // 未兑换的码只活 5 分钟；满了就拒绝新的，不挤掉别人已经拿到的码。
        if pending.len() >= MAX_PENDING_CODES {
            return pending_codes_full();
        }
        pending.insert(
            code.clone(),
            PendingCode {
                code_challenge: form.code_challenge.clone(),
                client_id: form.client_id.clone(),
                redirect_uri: form.redirect_uri.clone(),
                expires_at: now + OAUTH_CODE_TTL_SECONDS,
                binding,
            },
        );
    }

    let mut qs = format!("code={}", urlencoding_encode(&code));
    if !form.state.is_empty() {
        qs.push_str(&format!("&state={}", urlencoding_encode(&form.state)));
    }
    let sep = if form.redirect_uri.contains('?') {
        '&'
    } else {
        '?'
    };
    // 授权页面通过 POST 表单提交，但客户端回调必须使用 GET。
    // 307 会保留 POST 并把表单体转发到 ChatGPT connector，导致 Bad Request。
    (
        NO_STORE_HEADERS,
        Redirect::to(&format!("{}{}{}", form.redirect_uri, sep, qs)),
    )
        .into_response()
}

pub fn token_exchange(
    oauth: &OAuthRuntime,
    headers: &HeaderMap,
    mut form: TokenForm,
    _server_url: &str,
) -> Response {
    if !oauth.can_sign() {
        return oauth_unavailable();
    }
    if let Some((id, secret)) = basic_auth_credentials(headers) {
        if form.client_id.is_empty() {
            form.client_id = id;
        }
        if form.client_secret.is_empty() {
            form.client_secret = secret;
        }
    }

    match form.grant_type.as_str() {
        "authorization_code" => authorization_code_exchange(oauth, form),
        "refresh_token" => refresh_token_exchange(oauth, form),
        _ => token_error(
            "unsupported_grant_type",
            "Only authorization_code and refresh_token are supported",
        ),
    }
}

fn authorization_code_exchange(oauth: &OAuthRuntime, form: TokenForm) -> Response {
    if !oauth.client_id_allowed(&form.client_id)
        || !oauth.client_credentials_allowed(&form.client_id, &form.client_secret)
    {
        return token_error("invalid_client", "Invalid client credentials");
    }
    if form.code.is_empty() {
        return token_error("invalid_grant", "code is required");
    }
    if !valid_code_verifier(&form.code_verifier) {
        return token_error("invalid_grant", "Invalid code_verifier");
    }

    let code_data = {
        let mut pending = oauth.pending.lock().expect("oauth pending lock");
        pending.remove(&form.code)
    };
    let Some(code_data) = code_data else {
        return token_error(
            "invalid_grant",
            "Unknown or already-used authorization code",
        );
    };
    if unix_now() > code_data.expires_at {
        return token_error("invalid_grant", "Authorization code expired");
    }
    if !constant_time_eq_str(&code_data.client_id, &form.client_id) {
        return token_error("invalid_grant", "client_id mismatch");
    }
    if !constant_time_eq_str(&code_data.redirect_uri, &form.redirect_uri) {
        return token_error("invalid_grant", "redirect_uri mismatch");
    }
    if !verify_pkce(&form.code_verifier, &code_data.code_challenge) {
        return token_error("invalid_grant", "PKCE verification failed");
    }
    match (&oauth.authorization, code_data.binding) {
        (OAuthAuthorization::Actions { .. }, None) => {
            issue_actions_token_pair(oauth, &form.client_id)
        }
        (OAuthAuthorization::Mcp { .. }, Some(binding)) => {
            issue_mcp_token_pair(oauth, &form.client_id, &binding)
        }
        _ => token_error("invalid_grant", "Authorization code is not valid here"),
    }
}

fn refresh_token_exchange(oauth: &OAuthRuntime, mut form: TokenForm) -> Response {
    if form.refresh_token.is_empty() {
        return token_error("invalid_grant", "refresh_token is required");
    }
    // 只取 refresh 用途的 claims；MCP 的续签沿用 refresh 里签名过的 grant 绑定。
    let (claimed_client, binding) = match &oauth.authorization {
        OAuthAuthorization::Actions { .. } => {
            let Some(claims) = oauth
                .decode_current(&form.refresh_token)
                .filter(|claims| claims.token_use == token_use("refresh"))
            else {
                return token_error("invalid_grant", "Invalid refresh_token");
            };
            (claims.client_id, None)
        }
        OAuthAuthorization::Mcp { .. } => {
            let Some(claims) = oauth.decode_mcp(&form.refresh_token, "refresh") else {
                return token_error("invalid_grant", "Invalid refresh_token");
            };
            let binding = claims.binding();
            (claims.client_id, Some(binding))
        }
    };
    if form.client_id.is_empty() {
        form.client_id = claimed_client.clone();
    }
    if !constant_time_eq_str(&form.client_id, &claimed_client)
        || !oauth.client_id_allowed(&form.client_id)
        || !oauth.client_credentials_allowed(&form.client_id, &form.client_secret)
    {
        return token_error("invalid_client", "Invalid client credentials");
    }
    match binding {
        Some(binding) => issue_mcp_token_pair(oauth, &form.client_id, &binding),
        None => issue_actions_token_pair(oauth, &form.client_id),
    }
}

/// 授权码兑换和 refresh 共用：签发前用共享读者确认 grant 仍有效、修订号仍是绑定的那个。
/// 轮换或撤销之后，旧授权码和旧 refresh 都换不出新 token。
fn issue_mcp_token_pair(oauth: &OAuthRuntime, client_id: &str, binding: &OAuthBinding) -> Response {
    let OAuthAuthorization::Mcp { access, entry } = &oauth.authorization else {
        return token_error("invalid_grant", "Authorization is not valid here");
    };
    if access
        .authenticate_oauth(entry, binding, client_id, unix_now())
        .is_err()
    {
        return token_error("invalid_grant", "Authorization is no longer valid");
    }
    let sign = |ttl: i64, kind: &str| {
        let now = unix_now() as i64;
        sign_claims(
            &McpTokenClaims {
                version: MCP_CLAIMS_VERSION,
                iss: oauth.audience.clone(),
                aud: oauth.audience.clone(),
                iat: now,
                exp: now + ttl,
                client_id: client_id.to_string(),
                grant_id: binding.grant_id.clone(),
                credential_revision: binding.credential_revision,
                token_use: mcp_token_use(kind),
            },
            &oauth.token_secret,
        )
    };
    token_pair_response(
        sign(OAUTH_TOKEN_TTL_SECONDS, "access"),
        sign(OAUTH_REFRESH_TOKEN_TTL_SECONDS, "refresh"),
    )
}

fn issue_actions_token_pair(oauth: &OAuthRuntime, client_id: &str) -> Response {
    token_pair_response(
        sign_token(
            &oauth.audience,
            &oauth.token_secret,
            OAUTH_TOKEN_TTL_SECONDS,
            client_id,
            "access",
        ),
        sign_token(
            &oauth.audience,
            &oauth.token_secret,
            OAUTH_REFRESH_TOKEN_TTL_SECONDS,
            client_id,
            "refresh",
        ),
    )
}

fn token_pair_response(access: Result<String, ()>, refresh: Result<String, ()>) -> Response {
    match (access, refresh) {
        (Ok(access_token), Ok(refresh_token)) => (
            StatusCode::OK,
            NO_STORE_HEADERS,
            axum::Json(json!({
                "access_token": access_token,
                "token_type": "Bearer",
                "expires_in": OAUTH_TOKEN_TTL_SECONDS,
                "refresh_token": refresh_token
            })),
        )
            .into_response(),
        _ => token_error("server_error", "Failed to issue token pair"),
    }
}

#[cfg(test)]
fn create_token(
    server_url: &str,
    token_secret: &str,
    ttl: i64,
    client_id: &str,
    token_use: &str,
) -> Result<String, ()> {
    sign_token(server_url, token_secret, ttl, client_id, token_use)
}

fn token_use(kind: &str) -> String {
    format!("actions_{kind}_v2")
}

fn mcp_token_use(kind: &str) -> String {
    format!("mcp_{kind}_v3")
}

fn sign_token(
    server_url: &str,
    token_secret: &str,
    ttl: i64,
    client_id: &str,
    kind: &str,
) -> Result<String, ()> {
    let now = unix_now() as i64;
    let claims = TokenClaims {
        version: ACTIONS_CLAIMS_VERSION,
        iss: server_url.to_string(),
        aud: server_url.to_string(),
        iat: now,
        exp: now + ttl,
        scope: "actions".into(),
        client_id: client_id.to_string(),
        token_use: token_use(kind),
    };
    sign_claims(&claims, token_secret)
}

fn sign_claims<T: Serialize>(claims: &T, token_secret: &str) -> Result<String, ()> {
    if token_secret.is_empty() {
        return Err(());
    }
    encode(
        &Header::new(Algorithm::HS256),
        claims,
        &EncodingKey::from_secret(token_secret.as_bytes()),
    )
    .map_err(|_| ())
}

fn decode_claims<T: serde::de::DeserializeOwned>(
    token: &str,
    token_secret: &str,
    server_url: &str,
) -> Result<T, ()> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_audience(&[server_url]);
    validation.set_issuer(&[server_url]);
    validation.leeway = 0;
    decode::<T>(
        token,
        &DecodingKey::from_secret(token_secret.as_bytes()),
        &validation,
    )
    .map(|data| data.claims)
    .map_err(|_| ())
}

fn verify_pkce(code_verifier: &str, code_challenge: &str) -> bool {
    let digest = Sha256::digest(code_verifier.as_bytes());
    let expected = URL_SAFE_NO_PAD.encode(digest);
    constant_time_eq_str(&expected, code_challenge)
}

/// S256 的 challenge 是 SHA-256 摘要的无填充 base64url，固定 43 个字符（RFC 7636 §4.2）。
fn valid_s256_challenge(challenge: &str) -> bool {
    challenge.len() == 43
        && challenge
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn valid_code_verifier(verifier: &str) -> bool {
    (43..=128).contains(&verifier.len())
        && verifier
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '.' | '_' | '~'))
}

fn basic_auth_credentials(headers: &HeaderMap) -> Option<(String, String)> {
    let header = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let encoded = header.strip_prefix("Basic ")?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let (id, secret) = text.split_once(':')?;
    Some((id.to_string(), secret.to_string()))
}

/// 授权页、授权码跳转和 token 响应都带口令或凭据的上下文：不缓存，不外带 Referer。
const NO_STORE_HEADERS: [(axum::http::HeaderName, &str); 2] = [
    (axum::http::header::CACHE_CONTROL, "no-store"),
    (axum::http::header::REFERRER_POLICY, "no-referrer"),
];

fn page(status: StatusCode, html: String) -> Response {
    (status, NO_STORE_HEADERS, Html(html)).into_response()
}

/// 这个入口没有配置 token 签名密钥（工作区没开 OAuth）：发现和注册仍可用，但不能授权。
fn oauth_unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        NO_STORE_HEADERS,
        axum::Json(json!({
            "error": "temporarily_unavailable",
            "error_description": "OAuth is not enabled for this endpoint."
        })),
    )
        .into_response()
}

/// 未兑换的授权码已到上限：稍后重试即可，已有的码不受影响。
fn pending_codes_full() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        NO_STORE_HEADERS,
        axum::Json(json!({
            "error": "temporarily_unavailable",
            "error_description": "Too many authorizations are waiting to be exchanged. Try again in a few minutes."
        })),
    )
        .into_response()
}

fn token_error(error: &str, description: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        axum::Json(json!({
            "error": error,
            "error_description": description
        })),
    )
        .into_response()
}

fn html_error(message: &str, status: StatusCode) -> Response {
    (status, Html(format!("<h2>Error</h2><p>{message}</p>"))).into_response()
}

#[allow(clippy::too_many_arguments)]
fn login_page(
    client_id: &str,
    redirect_uri: &str,
    code_challenge: &str,
    code_challenge_method: &str,
    state: &str,
    error: &str,
    workspace_path: Option<&str>,
    server_url: &str,
) -> String {
    let error_block = if error.is_empty() {
        String::new()
    } else {
        format!("<p style=\"color:red\">{}</p>", html_escape(error))
    };
    let workspace_block = workspace_path
        .filter(|path| !path.is_empty())
        .map(|path| format!("<p>Workspace: <code>{}</code></p>", html_escape(path)))
        .unwrap_or_default();
    format!(
        "<!DOCTYPE html><html lang='en'><head><meta charset='utf-8'>\
        <title>Authorize MCP Server</title>\
        <style>body{{font-family:sans-serif;max-width:380px;margin:4rem auto;padding:1rem}}\
        input{{width:100%;padding:.5rem;margin:.4rem 0;box-sizing:border-box}}\
        button{{width:100%;padding:.7rem;background:#0066cc;color:#fff;border:none;cursor:pointer}}</style>\
        </head><body>\
        <h2>Authorize gld</h2>\
        {workspace_block}\
        <p>Client: <strong>{}</strong></p>\
        <p>Redirect URI: <code>{}</code></p>\
        {error_block}\
        <form method='POST' action='{}/oauth/authorize'>\
        <input type='hidden' name='client_id' value='{}'>\
        <input type='hidden' name='redirect_uri' value='{}'>\
        <input type='hidden' name='code_challenge' value='{}'>\
        <input type='hidden' name='code_challenge_method' value='{}'>\
        <input type='hidden' name='state' value='{}'>\
        <label>Password or authorization credential<input type='password' name='password' autocomplete='off' required></label>\
        <button type='submit'>Authorize</button>\
        </form></body></html>",
        html_escape(client_id),
        html_escape(redirect_uri),
        html_escape(server_url.trim_end_matches('/')),
        html_escape(client_id),
        html_escape(redirect_uri),
        html_escape(code_challenge),
        html_escape(code_challenge_method),
        html_escape(state),
    )
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\"', "&quot;")
        .replace('\'', "&#39;")
}

fn urlencoding_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;

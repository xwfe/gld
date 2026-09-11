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

use super::bearer::constant_time_eq_str;
use super::client_registry::{ClientRegistry, RegisteredClient};

pub const OAUTH_CODE_TTL_SECONDS: u64 = 300;
pub const OAUTH_TOKEN_TTL_SECONDS: i64 = 60 * 60 * 24 * 30;
pub const OAUTH_REFRESH_TOKEN_TTL_SECONDS: i64 = 60 * 60 * 24 * 90;
#[allow(dead_code)]
pub const OAUTH_MAX_BODY_BYTES: usize = 8_192;

#[derive(Clone)]
pub struct OAuthRuntime {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub password: String,
    pub token_secret: String,
    /// 令牌里写的 `iss`/`aud`。用的是工作区标识而不是公网地址——
    /// 地址是会变的（临时隧道每次重启换一个、换域名、开关全局入口），
    /// 绑地址就等于地址一动所有已发令牌全废，用户得重新授权。
    /// 工作区标识不变，加上每个工作区自己的 `token_secret`，
    /// 已经能保证"A 工作区的令牌进不了 B 工作区"。
    audience: String,
    pending: Arc<Mutex<HashMap<String, PendingCode>>>,
    clients: Arc<ClientRegistry>,
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

fn valid_redirect_uri(uri: &str) -> bool {
    let uri = uri.trim();
    (uri.starts_with("https://") || uri.starts_with("http://")) && !uri.contains(['\r', '\n', '#'])
}

#[derive(Clone)]
#[allow(dead_code)]
struct PendingCode {
    code_challenge: String,
    client_id: String,
    redirect_uri: String,
    state: String,
    expires_at: u64,
}

#[derive(Serialize, Deserialize)]
struct TokenClaims {
    iss: String,
    aud: String,
    iat: i64,
    exp: i64,
    scope: String,
    client_id: String,
    token_use: String,
}

impl OAuthRuntime {
    pub fn new(
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
            password,
            token_secret,
            audience,
            pending: Arc::new(Mutex::new(HashMap::new())),
            clients,
        }
    }

    pub fn client_id_allowed(&self, client_id: &str) -> bool {
        if client_id.is_empty() {
            return false;
        }
        if self.client_id.is_empty() {
            return true;
        }
        constant_time_eq_str(client_id, &self.client_id) || self.clients.contains(client_id)
    }

    fn redirect_uri_allowed(&self, client_id: &str, redirect_uri: &str) -> bool {
        if constant_time_eq_str(client_id, &self.client_id) || self.client_id.is_empty() {
            return !redirect_uri.trim().is_empty();
        }
        self.clients
            .get(client_id)
            .is_some_and(|client| client.redirect_uris.iter().any(|uri| uri == redirect_uri))
    }

    fn client_credentials_allowed(&self, client_id: &str, client_secret: &str) -> bool {
        if constant_time_eq_str(client_id, &self.client_id) || self.client_id.is_empty() {
            return self
                .client_secret
                .as_deref()
                .is_none_or(|expected| constant_time_eq_str(client_secret, expected));
        }
        let Some(client) = self.clients.get(client_id) else {
            // 注册表里没有这个 id：可能是升级前注册的（那时还不落盘），
            // 也可能是文件损坏后重建的。这里不认，但刷新令牌那条路会另行放行，
            // 见 refresh_token_exchange。
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

    pub fn register(&self, client_id: String, client: RegisteredClient) {
        self.clients.insert(client_id, client);
    }

    pub fn verify_access_token(&self, token: &str, server_url: &str) -> bool {
        self.decode_any(token, server_url)
            .is_some_and(|claims| claims.token_use == "access")
    }

    /// 先按稳定受众验，再按"当前公网地址"验一次。
    ///
    /// 第二次是给升级前签发的令牌留的：那时候 `aud` 写的是地址。没有这一步的话，
    /// 用户一升级 gld，所有连接器当场全部 401，全得重新授权——正是这次要消灭的事。
    /// 旧令牌刷新一次就换成新的稳定令牌，之后地址再变也不受影响。
    fn decode_any(&self, token: &str, server_url: &str) -> Option<TokenClaims> {
        decode_token_claims(token, &self.token_secret, &self.audience)
            .ok()
            .or_else(|| {
                let legacy = server_url.trim_end_matches('/');
                if legacy.is_empty() {
                    return None;
                }
                decode_token_claims(token, &self.token_secret, legacy).ok()
            })
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
    oauth.register(
        client_id.clone(),
        RegisteredClient {
            redirect_uris: request.redirect_uris.clone(),
            token_endpoint_auth_method: auth_method.to_string(),
            client_secret: client_secret.clone(),
            client_name: request.client_name.trim().to_string(),
            created_at: unix_now(),
        },
    );

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

pub fn verify_oauth_bearer_header(
    headers: &HeaderMap,
    oauth: &OAuthRuntime,
    server_url: &str,
) -> Option<Response> {
    let Some(header_value) = headers.get(AUTHORIZATION) else {
        return Some((StatusCode::UNAUTHORIZED, "Missing Authorization header").into_response());
    };
    let Ok(header_str) = header_value.to_str() else {
        return Some((StatusCode::UNAUTHORIZED, "Invalid Authorization header").into_response());
    };
    // 和静态 bearer 走同一个解析：scheme 大小写不敏感，空 token 直接算无效。
    let Some(token) = super::bearer::bearer_token(header_str) else {
        return Some((StatusCode::UNAUTHORIZED, "Invalid bearer token").into_response());
    };
    if oauth.verify_access_token(token, server_url) {
        None
    } else {
        Some((StatusCode::UNAUTHORIZED, "Invalid bearer token").into_response())
    }
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
    Html(login_page(
        &params.client_id,
        &params.redirect_uri,
        &params.code_challenge,
        &params.code_challenge_method,
        &params.state,
        "",
        workspace_path,
        server_url,
    ))
    .into_response()
}

pub fn authorize_post(oauth: &OAuthRuntime, form: AuthorizeForm, server_url: &str) -> Response {
    if !oauth.client_id_allowed(&form.client_id) {
        return Html(login_page(
            &form.client_id,
            &form.redirect_uri,
            &form.code_challenge,
            &form.code_challenge_method,
            &form.state,
            "Invalid client",
            None,
            server_url,
        ))
        .into_response();
    }
    if !oauth.redirect_uri_allowed(&form.client_id, &form.redirect_uri) {
        return html_error(
            "redirect_uri is not registered for this client",
            StatusCode::BAD_REQUEST,
        );
    }
    if form.code_challenge_method != "S256" || form.code_challenge.is_empty() {
        return Html(login_page(
            &form.client_id,
            &form.redirect_uri,
            &form.code_challenge,
            &form.code_challenge_method,
            &form.state,
            "Invalid PKCE parameters",
            None,
            server_url,
        ))
        .into_response();
    }
    if !constant_time_eq_str(&form.password, &oauth.password) {
        return (
            StatusCode::UNAUTHORIZED,
            Html(login_page(
                &form.client_id,
                &form.redirect_uri,
                &form.code_challenge,
                &form.code_challenge_method,
                &form.state,
                "Invalid password",
                None,
                server_url,
            )),
        )
            .into_response();
    }

    let code = uuid::Uuid::new_v4().to_string().replace('-', "");
    let now = unix_now();
    {
        let mut pending = oauth.pending.lock().expect("oauth pending lock");
        pending.retain(|_, v| v.expires_at >= now);
        pending.insert(
            code.clone(),
            PendingCode {
                code_challenge: form.code_challenge.clone(),
                client_id: form.client_id.clone(),
                redirect_uri: form.redirect_uri.clone(),
                state: form.state.clone(),
                expires_at: now + OAUTH_CODE_TTL_SECONDS,
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
    Redirect::to(&format!("{}{}{}", form.redirect_uri, sep, qs)).into_response()
}

pub fn token_exchange(
    oauth: &OAuthRuntime,
    headers: &HeaderMap,
    mut form: TokenForm,
    server_url: &str,
) -> Response {
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
        "refresh_token" => refresh_token_exchange(oauth, form, server_url),
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

    issue_token_pair(oauth, &form.client_id)
}

fn refresh_token_exchange(oauth: &OAuthRuntime, mut form: TokenForm, server_url: &str) -> Response {
    if form.refresh_token.is_empty() {
        return token_error("invalid_grant", "refresh_token is required");
    }
    let Some(claims) = oauth
        .decode_any(&form.refresh_token, server_url)
        .filter(|claims| claims.token_use == "refresh")
    else {
        return token_error("invalid_grant", "Invalid refresh_token");
    };
    if form.client_id.is_empty() {
        form.client_id = claims.client_id.clone();
    }
    if !constant_time_eq_str(&form.client_id, &claims.client_id) {
        return token_error("invalid_client", "Invalid client credentials");
    }
    // 注册表里还认识这个 client，就按它登记的方式校验（机密客户端要带 secret）。
    //
    // 不认识就放行：refresh_token 是本服务签的、client_id 就写在令牌里，
    // 而 ChatGPT 这类客户端注册的是公共客户端（auth_method=none），本来就不需要
    // 任何 secret——所以这里卡一道并不增加安全性，只会让"注册表丢了"变成
    // "所有连接器要删掉重建"。真要吊销全部授权，用
    // `gld secret regen oauth_token_secret`，那是连令牌一起作废的。
    let known_client = oauth.clients.get(&form.client_id).is_some()
        || constant_time_eq_str(&form.client_id, &oauth.client_id);
    if known_client && !oauth.client_credentials_allowed(&form.client_id, &form.client_secret) {
        return token_error("invalid_client", "Invalid client credentials");
    }
    issue_token_pair(oauth, &form.client_id)
}

fn issue_token_pair(oauth: &OAuthRuntime, client_id: &str) -> Response {
    let access = create_token(
        &oauth.audience,
        &oauth.token_secret,
        OAUTH_TOKEN_TTL_SECONDS,
        client_id,
        "access",
    );
    let refresh = create_token(
        &oauth.audience,
        &oauth.token_secret,
        OAUTH_REFRESH_TOKEN_TTL_SECONDS,
        client_id,
        "refresh",
    );
    match (access, refresh) {
        (Ok(access_token), Ok(refresh_token)) => (
            StatusCode::OK,
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

fn create_token(
    server_url: &str,
    token_secret: &str,
    ttl: i64,
    client_id: &str,
    token_use: &str,
) -> Result<String, ()> {
    let now = unix_now() as i64;
    let claims = TokenClaims {
        iss: server_url.to_string(),
        aud: server_url.to_string(),
        iat: now,
        exp: now + ttl,
        scope: "mcp".into(),
        client_id: client_id.to_string(),
        token_use: token_use.to_string(),
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(token_secret.as_bytes()),
    )
    .map_err(|_| ())
}

fn decode_token_claims(
    token: &str,
    token_secret: &str,
    server_url: &str,
) -> Result<TokenClaims, ()> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_audience(&[server_url]);
    validation.set_issuer(&[server_url]);
    decode::<TokenClaims>(
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
        <label>Password<input type='password' name='password' autocomplete='current-password' required></label>\
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
mod tests {
    use super::*;
    use axum::http::HeaderMap;

    const AUDIENCE: &str = "gld:ws:test-workspace";
    const PASSWORD: &str = "test-password";
    const TOKEN_SECRET: &str = "token-signing-secret";
    const REDIRECT_URI: &str = "https://chatgpt.com/connector/oauth/test";
    const VERIFIER: &str = "dBjftJeZ4CVP-mB92Kpru-AEJvkQlLgi3ThpmQ45N_Xyo";

    fn runtime(client_id: &str) -> OAuthRuntime {
        runtime_with(client_id, Arc::new(ClientRegistry::in_memory()))
    }

    fn runtime_with(client_id: &str, clients: Arc<ClientRegistry>) -> OAuthRuntime {
        OAuthRuntime::new(
            AUDIENCE.into(),
            client_id.into(),
            None,
            PASSWORD.into(),
            TOKEN_SECRET.into(),
            clients,
        )
    }

    fn challenge() -> String {
        URL_SAFE_NO_PAD.encode(Sha256::digest(VERIFIER.as_bytes()))
    }

    fn body_json(response: Response) -> Value {
        let bytes = crate::async_rt::block_on(async {
            axum::body::to_bytes(response.into_body(), OAUTH_MAX_BODY_BYTES * 16)
                .await
                .expect("read body")
        });
        serde_json::from_slice(&bytes).expect("json body")
    }

    /// 走完一次授权码流程，返回 (access_token, refresh_token)。
    fn authorize_and_exchange(
        oauth: &OAuthRuntime,
        client_id: &str,
        base: &str,
    ) -> (String, String) {
        let redirect = authorize_post(
            oauth,
            AuthorizeForm {
                client_id: client_id.into(),
                redirect_uri: REDIRECT_URI.into(),
                code_challenge: challenge(),
                code_challenge_method: "S256".into(),
                state: "state".into(),
                password: PASSWORD.into(),
            },
            base,
        );
        assert_eq!(redirect.status(), StatusCode::SEE_OTHER);
        let code = {
            let pending = oauth.pending.lock().expect("lock");
            pending.keys().next().cloned().expect("pending code")
        };
        let response = token_exchange(
            oauth,
            &HeaderMap::new(),
            TokenForm {
                grant_type: "authorization_code".into(),
                code,
                redirect_uri: REDIRECT_URI.into(),
                code_verifier: VERIFIER.into(),
                client_id: client_id.into(),
                ..TokenForm::default()
            },
            base,
        );
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response);
        (
            body["access_token"].as_str().expect("access").to_string(),
            body["refresh_token"].as_str().expect("refresh").to_string(),
        )
    }

    fn register(oauth: &OAuthRuntime) -> String {
        let response = register_client(
            oauth,
            ClientRegistrationRequest {
                redirect_uris: vec![REDIRECT_URI.into()],
                token_endpoint_auth_method: "none".into(),
                grant_types: vec!["authorization_code".into(), "refresh_token".into()],
                response_types: vec!["code".into()],
                client_name: "ChatGPT".into(),
            },
        );
        assert_eq!(response.status(), StatusCode::CREATED);
        body_json(response)["client_id"]
            .as_str()
            .expect("client_id")
            .to_string()
    }

    #[test]
    fn token_exchange_without_client_secret() {
        let oauth = runtime("chatgpt-client-test");
        authorize_and_exchange(&oauth, "chatgpt-client-test", "https://lb.example.com");
    }

    #[test]
    fn refresh_token_cannot_authenticate_as_an_access_token() {
        let oauth = runtime("chatgpt-client-test");
        let refresh = create_token(
            AUDIENCE,
            &oauth.token_secret,
            OAUTH_REFRESH_TOKEN_TTL_SECONDS,
            "chatgpt-client-test",
            "refresh",
        )
        .expect("refresh token");

        assert!(!oauth.verify_access_token(&refresh, "https://lb.example.com"));
    }

    #[test]
    fn pkce_round_trip() {
        assert!(verify_pkce(VERIFIER, &challenge()));
    }

    #[test]
    fn dynamic_registration_restricts_redirect_uri() {
        let oauth = runtime("legacy-client");
        let client_id = register(&oauth);
        assert!(oauth.redirect_uri_allowed(&client_id, REDIRECT_URI));
        assert!(!oauth.redirect_uri_allowed(&client_id, "https://attacker.example/callback"));
    }

    #[test]
    fn refresh_token_issues_a_new_token_pair() {
        let oauth = runtime("chatgpt-client-test");
        let refresh = create_token(
            AUDIENCE,
            &oauth.token_secret,
            OAUTH_REFRESH_TOKEN_TTL_SECONDS,
            "chatgpt-client-test",
            "refresh",
        )
        .expect("refresh token");
        let response = token_exchange(
            &oauth,
            &HeaderMap::new(),
            TokenForm {
                grant_type: "refresh_token".into(),
                client_id: "chatgpt-client-test".into(),
                refresh_token: refresh,
                ..TokenForm::default()
            },
            "https://lb.example.com",
        );
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// 重启服务不该让已经装好的连接器掉授权。
    ///
    /// 这里用"同一份注册表 + 新的 OAuthRuntime"模拟重启：进程内的 pending 表、
    /// 内存态全丢，只有落盘的注册表还在。
    #[test]
    fn restart_keeps_registered_clients_usable() {
        let clients = Arc::new(ClientRegistry::in_memory());
        let before = runtime_with("chatgpt-client-test", clients.clone());
        let client_id = register(&before);
        let (_, refresh) = authorize_and_exchange(&before, &client_id, "https://lb.example.com");

        let after = runtime_with("chatgpt-client-test", clients);
        assert!(after.client_id_allowed(&client_id));
        let response = token_exchange(
            &after,
            &HeaderMap::new(),
            TokenForm {
                grant_type: "refresh_token".into(),
                client_id: client_id.clone(),
                refresh_token: refresh,
                ..TokenForm::default()
            },
            "https://lb.example.com",
        );
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// 注册表整个丢了（文件损坏、从旧版本升上来）也要能续命，
    /// 否则用户得把连接器删了重建——这正是要避免的体验。
    #[test]
    fn refresh_still_works_when_the_registry_is_lost() {
        let before = runtime_with("chatgpt-client-test", Arc::new(ClientRegistry::in_memory()));
        let client_id = register(&before);
        let (_, refresh) = authorize_and_exchange(&before, &client_id, "https://lb.example.com");

        // 空注册表 = 文件没了
        let after = runtime_with("chatgpt-client-test", Arc::new(ClientRegistry::in_memory()));
        assert!(!after.client_id_allowed(&client_id));
        let response = token_exchange(
            &after,
            &HeaderMap::new(),
            TokenForm {
                grant_type: "refresh_token".into(),
                client_id,
                refresh_token: refresh,
                ..TokenForm::default()
            },
            "https://lb.example.com",
        );
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// 换域名、临时隧道换地址、开关全局入口——地址变了，令牌照样有效。
    #[test]
    fn tokens_survive_a_public_url_change() {
        let oauth = runtime("chatgpt-client-test");
        let (access, refresh) =
            authorize_and_exchange(&oauth, "chatgpt-client-test", "https://old.example.com");

        assert!(oauth.verify_access_token(&access, "https://brand-new.example.com/w/ws1"));
        let response = token_exchange(
            &oauth,
            &HeaderMap::new(),
            TokenForm {
                grant_type: "refresh_token".into(),
                client_id: "chatgpt-client-test".into(),
                refresh_token: refresh,
                ..TokenForm::default()
            },
            "https://brand-new.example.com/w/ws1",
        );
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// 升级 gld 之前签发的令牌（`aud` 写的是公网地址）不能当场作废，
    /// 否则一次升级就让所有连接器全部重新授权。
    #[test]
    fn tokens_issued_before_the_upgrade_still_verify() {
        let oauth = runtime("chatgpt-client-test");
        let legacy_access = create_token(
            "https://lb.example.com",
            &oauth.token_secret,
            OAUTH_TOKEN_TTL_SECONDS,
            "chatgpt-client-test",
            "access",
        )
        .expect("legacy access token");

        assert!(oauth.verify_access_token(&legacy_access, "https://lb.example.com"));
        // 但换个地址就不认了——旧令牌本来就是绑地址签的。
        assert!(!oauth.verify_access_token(&legacy_access, "https://other.example.com"));
    }

    /// 另一个工作区的令牌不能拿来打这个工作区，哪怕开了共享密钥池
    /// （几个工作区共用同一个 token_secret，这时只有受众能区分它们）。
    #[test]
    fn tokens_from_another_workspace_are_rejected() {
        let oauth = runtime("chatgpt-client-test");
        let other_workspace = create_token(
            "gld:ws:some-other-workspace",
            TOKEN_SECRET,
            OAUTH_TOKEN_TTL_SECONDS,
            "chatgpt-client-test",
            "access",
        )
        .expect("token");

        assert!(!oauth.verify_access_token(&other_workspace, "https://lb.example.com"));
    }

    #[test]
    fn login_page_posts_back_to_workspace_oauth_path() {
        let html = login_page(
            "client",
            "https://chatgpt.com/callback",
            "challenge",
            "S256",
            "state",
            "",
            Some("/workspace"),
            "https://mcp.example.com/w/workspace-id",
        );
        assert!(html.contains("action='https://mcp.example.com/w/workspace-id/oauth/authorize'"));
    }
}

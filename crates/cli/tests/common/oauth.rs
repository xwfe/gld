//! 走一遍 ChatGPT 连接器那套 OAuth 的辅助函数：动态注册 → 授权页 → PKCE 换 token。
//!
//! 工作区和聚合入口的集成测试都要走这条路。各拷一份的话，哪天授权流程改了字段，
//! 只改到一边，另一边的测试就在测一条已经不存在的路。

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use sha2::{Digest, Sha256};

use super::http::{get, post_form, post_json, query_param, Reply};

/// 随便挑一个 ChatGPT 那边形状的回调地址；服务端只校验它前后一致。
pub const REDIRECT: &str = "https://chatgpt.com/connector_platform_oauth_redirect";

/// 从元数据里取端点，只留 path + query——请求是自己拼 TCP 发的，
/// 主机部分用不上。
pub fn endpoint_path(metadata: &serde_json::Value, key: &str) -> String {
    let url = metadata[key]
        .as_str()
        .unwrap_or_else(|| panic!("元数据缺 {key}：{metadata}"));
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    match after_scheme.find('/') {
        Some(at) => after_scheme[at..].to_string(),
        None => "/".to_string(),
    }
}

/// 生成一对 PKCE 参数。verifier 必须是 43~128 个 unreserved 字符，
/// base64url 编码 48 字节正好落在区间里。
pub fn pkce() -> (String, String) {
    static SEQ: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
    let nth = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let seed: Vec<u8> = (0..48u8)
        .map(|i| i.wrapping_mul(7).wrapping_add(nth))
        .collect();
    let verifier = URL_SAFE_NO_PAD.encode(seed);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

/// 走完一次授权页，返回 (状态码, Location)。
pub fn authorize(port: u16, path: &str, client_id: &str, challenge: &str, password: &str) -> Reply {
    post_form(
        port,
        path,
        &[
            ("client_id", client_id),
            ("redirect_uri", REDIRECT),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
            ("state", "test-state"),
            ("password", password),
        ],
    )
}

/// 把一个连接器从头接进来：动态注册 → 授权页输口令 → 换 token。
/// 返回 (client_id, access_token, refresh_token, token 端点路径)。
pub fn connect_a_connector(port: u16, password: &str) -> (String, String, String, String) {
    let metadata = get(port, "/.well-known/oauth-authorization-server").json();
    let register_path = endpoint_path(&metadata, "registration_endpoint");
    let authorize_path = endpoint_path(&metadata, "authorization_endpoint");
    let token_path = endpoint_path(&metadata, "token_endpoint");

    let registered = post_json(
        port,
        &register_path,
        &serde_json::json!({
            "redirect_uris": [REDIRECT],
            "client_name": "integration-test",
            "token_endpoint_auth_method": "none",
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
        })
        .to_string(),
        None,
    );
    let client_id = registered.json()["client_id"]
        .as_str()
        .expect("注册要返回 client_id")
        .to_string();

    let (verifier, challenge) = pkce();
    let granted = authorize(port, &authorize_path, &client_id, &challenge, password);
    let code = query_param(granted.header("location").unwrap_or_default(), "code");
    let exchanged = post_form(
        port,
        &token_path,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", REDIRECT),
            ("client_id", &client_id),
            ("code_verifier", &verifier),
        ],
    );
    assert_eq!(exchanged.status, 200, "换 token 失败：{}", exchanged.body);
    let tokens = exchanged.json();
    (
        client_id,
        tokens["access_token"].as_str().expect("access").to_string(),
        tokens["refresh_token"]
            .as_str()
            .expect("refresh")
            .to_string(),
        token_path,
    )
}

//! ChatGPT 连接器接进来时走的那套 OAuth，从发现文档一路跑到 tools/call。
//!
//! 这是 `mcp.auth` 的默认值，也就是绝大多数人第一次接客户端时走的路径，
//! 而它由「发现文档 → 动态注册 → 授权页 → PKCE 换 token → 带 token 调用」
//! 五段拼成，任何一段的字段名写错，客户端那边只会显示一句
//! “无法连接到 MCP 服务器”，从日志里分不出断在哪一段。
//!
//! 所以这里不 mock 任何一段：起真的服务，发真的 HTTP，自己算 PKCE。

mod common;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use sha2::{Digest, Sha256};

use common::env::{free_port, Env};
use common::http::{get, post_form, post_json, query_param, urlencode, Reply};

/// 随便挑一个 ChatGPT 那边形状的回调地址；服务端只校验它前后一致。
const REDIRECT: &str = "https://chatgpt.com/connector_platform_oauth_redirect";

/// 从元数据里取端点，只留 path + query——请求是自己拼 TCP 发的，
/// 主机部分用不上。
fn endpoint_path(metadata: &serde_json::Value, key: &str) -> String {
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
fn pkce() -> (String, String) {
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
fn authorize(port: u16, path: &str, client_id: &str, challenge: &str, password: &str) -> Reply {
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

#[test]
fn chatgpt_connector_oauth_flow_works_end_to_end() {
    let env = Env::new();
    env.write("hello.txt", "oauth-e2e-marker\n");
    let port = free_port();
    env.ok(&[
        "ws",
        "add",
        ".",
        "--name",
        "oauth",
        "--mcp-port",
        &port.to_string(),
    ]);
    // 不设 mcp.auth：默认就是 oauth，顺带确认默认值没被改掉。
    assert_eq!(
        env.json(&["--json", "ws", "show"])["auth"]["type"],
        "oauth",
        "新工作区的 MCP 默认认证方式应当是 oauth"
    );
    env.ok(&["start"]);

    let password = env.json(&["--json", "secret", "show", "oauth_password", "--reveal"])["value"]
        .as_str()
        .expect("oauth_password")
        .to_string();

    // 1. 没凭据的请求必须被拒，并且要告诉客户端去哪儿拿凭据。
    //    少了 WWW-Authenticate 里的 resource_metadata，ChatGPT 不会发起授权，
    //    只会直接报连接失败。
    let denied = post_json(
        port,
        "/mcp",
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        None,
    );
    assert_eq!(denied.status, 401);
    let challenge_header = denied
        .header("www-authenticate")
        .expect("401 必须带 WWW-Authenticate");
    assert!(
        challenge_header.contains("resource_metadata"),
        "WWW-Authenticate 要指向 protected-resource 元数据，实际：{challenge_header}"
    );

    // 2. 两份发现文档。
    let protected = get(port, "/.well-known/oauth-protected-resource");
    assert_eq!(protected.status, 200);
    assert!(
        protected.json()["authorization_servers"]
            .as_array()
            .is_some_and(|servers| !servers.is_empty()),
        "protected-resource 要指出授权服务器：{}",
        protected.body
    );

    let metadata = get(port, "/.well-known/oauth-authorization-server");
    assert_eq!(metadata.status, 200);
    let metadata = metadata.json();
    assert!(
        metadata["code_challenge_methods_supported"]
            .as_array()
            .is_some_and(|methods| methods.iter().any(|m| m == "S256")),
        "必须声明支持 S256，ChatGPT 只用 PKCE：{metadata}"
    );
    let register_path = endpoint_path(&metadata, "registration_endpoint");
    let authorize_path = endpoint_path(&metadata, "authorization_endpoint");
    let token_path = endpoint_path(&metadata, "token_endpoint");

    // 3. 动态注册（DCR）：ChatGPT 不会让人手填 client_id，它自己注册。
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
    assert!(
        matches!(registered.status, 200 | 201),
        "动态注册失败：{} {}",
        registered.status,
        registered.body
    );
    let client_id = registered.json()["client_id"]
        .as_str()
        .expect("注册要返回 client_id")
        .to_string();

    // 注册只接受声明过的回调地址，否则任何人拿到地址都能把 code 引走。
    let evil = post_json(
        port,
        &register_path,
        &serde_json::json!({ "redirect_uris": ["not-a-url"] }).to_string(),
        None,
    );
    assert_eq!(evil.status, 400, "非法 redirect_uri 应当被拒");

    // 4. 授权页：口令错了不能下发 code。
    let (verifier, challenge) = pkce();
    let page = get(
        port,
        &format!(
            "{authorize_path}?response_type=code&client_id={}&redirect_uri={}\
             &code_challenge={challenge}&code_challenge_method=S256&state=test-state",
            urlencode(&client_id),
            urlencode(REDIRECT)
        ),
    );
    assert_eq!(page.status, 200);
    assert!(page.body.contains("password"), "授权页要有口令输入框");

    let wrong = authorize(
        port,
        &authorize_path,
        &client_id,
        &challenge,
        "wrong-password",
    );
    assert!(
        !wrong
            .header("location")
            .unwrap_or_default()
            .contains("code="),
        "口令错了不能下发 code：{:?}",
        wrong.header("location")
    );

    // 5. 口令对了拿 code。
    let granted = authorize(port, &authorize_path, &client_id, &challenge, &password);
    let location = granted
        .header("location")
        .expect("授权成功要重定向回 redirect_uri")
        .to_string();
    assert!(
        location.starts_with(REDIRECT),
        "必须重定向回注册时的地址：{location}"
    );
    let code = query_param(&location, "code");
    assert!(!code.is_empty(), "回调要带 code：{location}");
    assert_eq!(
        query_param(&location, "state"),
        "test-state",
        "state 要原样带回，客户端靠它防 CSRF"
    );

    // 6. PKCE：verifier 不对换不到 token，且这个 code 立刻作废（防止逐个试）。
    //    code 是一次性的，所以这里得单独申请一个，不能复用上面那个。
    let (verifier2, challenge2) = pkce();
    let second = authorize(port, &authorize_path, &client_id, &challenge2, &password);
    let code2 = query_param(second.header("location").unwrap_or_default(), "code");
    let bad_verifier = post_form(
        port,
        &token_path,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code2),
            ("redirect_uri", REDIRECT),
            ("client_id", &client_id),
            ("code_verifier", &"x".repeat(64)),
        ],
    );
    assert!(
        bad_verifier.status >= 400,
        "错误的 code_verifier 必须被拒：{}",
        bad_verifier.body
    );
    let retry = post_form(
        port,
        &token_path,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code2),
            ("redirect_uri", REDIRECT),
            ("client_id", &client_id),
            ("code_verifier", &verifier2),
        ],
    );
    assert!(
        retry.status >= 400,
        "verifier 试错一次后该 code 应当作废：{}",
        retry.body
    );

    // 7. redirect_uri 与授权时不一致也必须被拒。
    let (verifier3, challenge3) = pkce();
    let third = authorize(port, &authorize_path, &client_id, &challenge3, &password);
    let code3 = query_param(third.header("location").unwrap_or_default(), "code");
    let mismatched = post_form(
        port,
        &token_path,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code3),
            ("redirect_uri", "https://evil.example.com/cb"),
            ("client_id", &client_id),
            ("code_verifier", &verifier3),
        ],
    );
    assert!(
        mismatched.status >= 400,
        "redirect_uri 对不上必须被拒：{}",
        mismatched.body
    );

    // 8. 正常换 token。
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
    let access = tokens["access_token"]
        .as_str()
        .expect("access_token")
        .to_string();
    let refresh = tokens["refresh_token"]
        .as_str()
        .expect("refresh_token")
        .to_string();

    // 同一个 code 不能用第二次。
    let replay = post_form(
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
    assert!(replay.status >= 400, "code 重放必须被拒：{}", replay.body);

    // 9. 拿着 token 真的把 MCP 用起来：握手 → 列工具 → 读文件。
    let initialized = post_json(
        port,
        "/mcp",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#,
        Some(&access),
    );
    assert_eq!(
        initialized.status, 200,
        "initialize 失败：{}",
        initialized.body
    );
    assert!(
        initialized.json()["result"]["capabilities"]["tools"].is_object(),
        "握手要声明 tools 能力：{}",
        initialized.body
    );

    let listed = post_json(
        port,
        "/mcp",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        Some(&access),
    );
    assert_eq!(listed.status, 200);
    let tool_names: Vec<String> = listed.json()["result"]["tools"]
        .as_array()
        .expect("tools 数组")
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_string))
        .collect();
    assert!(
        tool_names.iter().any(|name| name == "read_file"),
        "工具清单里应当有 read_file：{tool_names:?}"
    );

    let called = post_json(
        port,
        "/mcp",
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"read_file","arguments":{"path":"hello.txt"}}}"#,
        Some(&access),
    );
    assert_eq!(called.status, 200);
    assert!(
        called.body.contains("oauth-e2e-marker"),
        "read_file 应当读到工作区里的文件：{}",
        called.body
    );

    // 10. refresh 续期后新 token 可用；refresh_token 本身不能当凭据用。
    let refreshed = post_form(
        port,
        &token_path,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", &refresh),
            ("client_id", &client_id),
        ],
    );
    assert_eq!(refreshed.status, 200, "refresh 失败：{}", refreshed.body);
    let renewed = refreshed.json()["access_token"]
        .as_str()
        .expect("续期要返回 access_token")
        .to_string();
    let with_renewed = post_json(
        port,
        "/mcp",
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/list"}"#,
        Some(&renewed),
    );
    assert_eq!(with_renewed.status, 200, "续期后的 token 应当可用");

    let as_access = post_json(
        port,
        "/mcp",
        r#"{"jsonrpc":"2.0","id":5,"method":"tools/list"}"#,
        Some(&refresh),
    );
    assert_eq!(as_access.status, 401, "refresh_token 不能当访问凭据");

    let forged = post_json(
        port,
        "/mcp",
        r#"{"jsonrpc":"2.0","id":6,"method":"tools/list"}"#,
        Some("not-a-real-token"),
    );
    assert_eq!(forged.status, 401, "伪造的 token 必须被拒");

    env.ok(&["stop"]);
}

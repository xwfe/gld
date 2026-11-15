use super::*;
use gld_core::auth::{remove_client_registry, GrantRecord};
use gld_core::home::data_home;

use crate::auth::test_home::run_isolated;

const OAUTH_MAX_BODY_BYTES: usize = 8_192;

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
    let bytes = gld_core::async_rt::block_on(async {
        axum::body::to_bytes(response.into_body(), OAUTH_MAX_BODY_BYTES * 16)
            .await
            .expect("read body")
    });
    serde_json::from_slice(&bytes).expect("json body")
}

/// 走完一次授权码流程，返回 (access_token, refresh_token)。
fn authorize_and_exchange(oauth: &OAuthRuntime, client_id: &str, base: &str) -> (String, String) {
    let code = authorize_code(oauth, client_id, base, PASSWORD);
    let response = exchange_code(oauth, client_id, base, code);
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response);
    (
        body["access_token"].as_str().expect("access").to_string(),
        body["refresh_token"].as_str().expect("refresh").to_string(),
    )
}

/// 授权页填 `password` 提交，返回发出来的授权码。
fn authorize_code(oauth: &OAuthRuntime, client_id: &str, base: &str, password: &str) -> String {
    let redirect = authorize_post(
        oauth,
        AuthorizeForm {
            client_id: client_id.into(),
            redirect_uri: REDIRECT_URI.into(),
            code_challenge: challenge(),
            code_challenge_method: "S256".into(),
            state: "state".into(),
            password: password.into(),
        },
        base,
    );
    assert_eq!(redirect.status(), StatusCode::SEE_OTHER);
    let location = redirect.headers()["location"].to_str().unwrap().to_string();
    let code = location
        .split(['?', '&'])
        .find_map(|pair| pair.strip_prefix("code="))
        .expect("code in redirect");
    code.to_string()
}

fn exchange_code(oauth: &OAuthRuntime, client_id: &str, base: &str, code: String) -> Response {
    token_exchange(
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
    )
}

fn refresh_with(oauth: &OAuthRuntime, client_id: &str, refresh_token: String) -> Response {
    token_exchange(
        oauth,
        &HeaderMap::new(),
        TokenForm {
            grant_type: "refresh_token".into(),
            client_id: client_id.into(),
            refresh_token,
            ..TokenForm::default()
        },
        "https://lb.example.com",
    )
}

struct FixedGrants(Mutex<Vec<GrantRecord>>);

impl FixedGrants {
    fn new(records: Vec<GrantRecord>) -> Self {
        Self(Mutex::new(records))
    }

    fn revoke(&self, id: &str) {
        self.0
            .lock()
            .unwrap()
            .retain(|record| record.grant.id != id);
    }

    fn find(&self, matches: impl Fn(&GrantRecord) -> bool) -> Option<Grant> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .find(|record| record.browser.is_none() && matches(record))
            .map(|record| Grant {
                browser: false,
                ..record.grant.clone()
            })
    }
}

impl GrantBook for FixedGrants {
    fn by_id(&self, id: &str) -> Option<Grant> {
        self.find(|record| record.grant.id == id)
    }

    fn by_password(&self, password: &str) -> Option<Grant> {
        self.find(|record| !password.is_empty() && record.oauth_password == password)
    }

    fn by_bearer_token(&self, token: &str) -> Option<Grant> {
        self.find(|record| !token.is_empty() && record.bearer_token == token)
    }
}

fn grant_record(id: &str, password: &str) -> GrantRecord {
    GrantRecord {
        grant: Grant {
            id: id.into(),
            name: "alice".into(),
            workspaces: vec!["ws-a".into()],
            read_only: false,
            browser: false,
        },
        oauth_password: password.into(),
        bearer_token: format!("{id}-bearer"),
        created_at: String::new(),
        browser: None,
    }
}

fn runtime_with_grants(grants: Arc<FixedGrants>) -> OAuthRuntime {
    runtime("chatgpt-client-test").with_grants(grants)
}

fn tokens(response: Response) -> (String, String) {
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response);
    (
        body["access_token"].as_str().expect("access").to_string(),
        body["refresh_token"].as_str().expect("refresh").to_string(),
    )
}

/// 授权页填 grant 的口令，发出来的令牌只带那个 grant；填服务口令还是全权（RFC-0007）。
#[test]
fn a_grant_password_issues_tokens_scoped_to_that_grant() {
    let grants = Arc::new(FixedGrants::new(vec![grant_record("g1", "grant-pw")]));
    let oauth = runtime_with_grants(grants);
    let base = "https://lb.example.com";
    let client = "chatgpt-client-test";

    let code = authorize_code(&oauth, client, base, "grant-pw");
    let (access, _) = tokens(exchange_code(&oauth, client, base, code));
    let identity = oauth.identify_access_token(&access, base).expect("valid");
    assert_eq!(identity.grant.map(|grant| grant.id), Some("g1".into()));

    let (owner, _) = authorize_and_exchange(&oauth, client, base);
    let identity = oauth.identify_access_token(&owner, base).expect("valid");
    assert_eq!(identity.grant, None, "服务口令进来的是全权");

    // 全权令牌的格式和加 grant 之前一样：不写这一格。
    let payload = owner.split('.').nth(1).unwrap();
    let claims: Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).unwrap()).expect("claims");
    assert!(claims.get("grant").is_none(), "{claims}");
}

/// 删掉 grant：它的访问令牌下一次就验不过，刷新令牌也换不来新的；删了再建一个同名、
/// 同口令的，旧令牌照样不能用——令牌认的是随机 id，不是名字。
#[test]
fn revoking_a_grant_voids_its_access_and_refresh_tokens() {
    let grants = Arc::new(FixedGrants::new(vec![grant_record("g1", "grant-pw")]));
    let oauth = runtime_with_grants(grants.clone());
    let base = "https://lb.example.com";
    let client = "chatgpt-client-test";
    let code = authorize_code(&oauth, client, base, "grant-pw");
    let (access, refresh) = tokens(exchange_code(&oauth, client, base, code));
    let (owner, _) = authorize_and_exchange(&oauth, client, base);

    grants.revoke("g1");
    assert!(oauth.identify_access_token(&access, base).is_none());
    let refused = refresh_with(&oauth, client, refresh.clone());
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body_json(refused)["error"], "invalid_grant");
    assert!(
        oauth.identify_access_token(&owner, base).is_some(),
        "全权令牌不受影响"
    );

    grants
        .0
        .lock()
        .unwrap()
        .push(grant_record("g2", "grant-pw"));
    assert!(oauth.identify_access_token(&access, base).is_none());
    assert_eq!(
        refresh_with(&oauth, client, refresh).status(),
        StatusCode::BAD_REQUEST
    );
}

/// 回滚到不认 grant 的旧版本时，grant 的令牌不能被当成全权令牌。旧版本只认
/// `token_use == "access"` / `"refresh"`，这里照它的判法验一遍。
#[test]
fn a_grant_token_is_refused_by_a_version_that_does_not_know_grants() {
    let grants = Arc::new(FixedGrants::new(vec![grant_record("g1", "grant-pw")]));
    let oauth = runtime_with_grants(grants);
    let base = "https://lb.example.com";
    let client = "chatgpt-client-test";
    let code = authorize_code(&oauth, client, base, "grant-pw");
    let (access, refresh) = tokens(exchange_code(&oauth, client, base, code));
    let (owner, owner_refresh) = authorize_and_exchange(&oauth, client, base);

    let old_kind = |token: &str| {
        decode_token_claims(token, TOKEN_SECRET, AUDIENCE)
            .expect("signature and audience are the same")
            .token_use
    };
    assert_ne!(old_kind(&access), "access", "旧版本会把它当全权访问令牌");
    assert_ne!(old_kind(&refresh), "refresh", "旧版本会拿它换全权令牌");
    assert_eq!(old_kind(&owner), "access", "全权令牌格式要和旧版本一样");
    assert_eq!(old_kind(&owner_refresh), "refresh");
}

/// 授权码活 5 分钟：这期间 grant 被删了，拿着码也换不出令牌。
#[test]
fn a_code_issued_for_a_revoked_grant_cannot_be_exchanged() {
    let grants = Arc::new(FixedGrants::new(vec![grant_record("g1", "grant-pw")]));
    let oauth = runtime_with_grants(grants.clone());
    let base = "https://lb.example.com";
    let code = authorize_code(&oauth, "chatgpt-client-test", base, "grant-pw");
    grants.revoke("g1");
    let refused = exchange_code(&oauth, "chatgpt-client-test", base, code);
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body_json(refused)["error"], "invalid_grant");
}

/// 刷新 grant 的令牌，新令牌还是那个 grant 的，不会刷成全权。
#[test]
fn refreshing_keeps_the_grant() {
    let grants = Arc::new(FixedGrants::new(vec![grant_record("g1", "grant-pw")]));
    let oauth = runtime_with_grants(grants);
    let base = "https://lb.example.com";
    let client = "chatgpt-client-test";
    let code = authorize_code(&oauth, client, base, "grant-pw");
    let (_, refresh) = tokens(exchange_code(&oauth, client, base, code));
    let (access, _) = tokens(refresh_with(&oauth, client, refresh));
    let identity = oauth.identify_access_token(&access, base).expect("valid");
    assert_eq!(identity.grant.map(|grant| grant.id), Some("g1".into()));
}

/// 没接 grant 的入口（工作区、GPT Actions）不认任何 grant 口令。
#[test]
fn an_entry_without_grants_refuses_a_grant_password() {
    let oauth = runtime("chatgpt-client-test");
    let refused = authorize_post(
        &oauth,
        AuthorizeForm {
            client_id: "chatgpt-client-test".into(),
            redirect_uri: REDIRECT_URI.into(),
            code_challenge: challenge(),
            code_challenge_method: "S256".into(),
            state: "state".into(),
            password: "grant-pw".into(),
        },
        "https://lb.example.com",
    );
    assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
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

fn confidential_registration(oauth: &OAuthRuntime) -> Response {
    register_client(
        oauth,
        ClientRegistrationRequest {
            redirect_uris: vec![REDIRECT_URI.into()],
            token_endpoint_auth_method: "client_secret_post".into(),
            grant_types: vec!["authorization_code".into(), "refresh_token".into()],
            response_types: vec!["code".into()],
            client_name: "private-registration-fixture".into(),
        },
    )
}

fn assert_registration_unavailable(response: Response, description: &str) {
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(
        body_json(response),
        json!({
            "error": "temporarily_unavailable",
            "error_description": description
        }),
        "an unsuccessful registration must not expose a client id, secret, or I/O detail"
    );
}

#[test]
fn registration_storage_failure_does_not_acknowledge_or_publish_a_client() {
    run_isolated(
        module_path!(),
        "registration_storage_failure_does_not_acknowledge_or_publish_a_client",
        || {
            let scope = "oauth-http-write-failure";
            let clients = Arc::new(ClientRegistry::load(scope, scope));
            let oauth = runtime_with("chatgpt-client-test", clients.clone());
            let existing_id = register(&oauth);
            let base = "https://lb.example.com";
            let (access, refresh) = authorize_and_exchange(&oauth, &existing_id, base);
            let path = data_home()
                .expect("test home")
                .join("data/oauth-clients")
                .join(format!("{scope}.json"));
            let before = std::fs::read(&path).expect("original registry");
            let blocked_tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
            std::fs::create_dir(&blocked_tmp).expect("force temporary write failure");

            assert_registration_unavailable(
                confidential_registration(&oauth),
                "Client registration is temporarily unavailable.",
            );
            assert_eq!(
                clients.len(),
                1,
                "the rejected registration must not enter memory"
            );
            assert!(clients.contains(&existing_id));
            assert_eq!(std::fs::read(&path).expect("preserved registry"), before);

            let after = runtime_with(
                "chatgpt-client-test",
                Arc::new(ClientRegistry::load(scope, scope)),
            );
            assert!(after.client_id_allowed(&existing_id));
            assert!(after.verify_access_token(&access, base));
            assert_eq!(
                refresh_with(&after, &existing_id, refresh).status(),
                StatusCode::OK
            );
            authorize_and_exchange(&after, &existing_id, base);
            std::fs::remove_dir(blocked_tmp).expect("remove write blocker");
            remove_client_registry(scope).expect("clean registry");
        },
    );
}

#[test]
fn registration_capacity_preserves_existing_clients_and_returns_a_specific_error() {
    let clients = Arc::new(ClientRegistry::in_memory());
    let oauth = runtime_with("chatgpt-client-test", clients.clone());
    let mut registered = Vec::new();
    // 通过真实注册入口达到容量，不依赖测试专用的 HTTP 错误构造。
    for _ in 0..50 {
        registered.push(register(&oauth));
    }
    assert_registration_unavailable(
        confidential_registration(&oauth),
        "Client registration capacity has been reached; existing clients are preserved.",
    );
    assert_eq!(clients.len(), registered.len());
    for id in &registered {
        assert!(oauth.client_id_allowed(id));
    }
    authorize_and_exchange(&oauth, &registered[0], "https://lb.example.com");
}

#[test]
fn damaged_registry_returns_service_unavailable_without_overwriting_it() {
    run_isolated(
        module_path!(),
        "damaged_registry_returns_service_unavailable_without_overwriting_it",
        || {
            let scope = "oauth-http-damaged-registry";
            let path = data_home()
                .expect("test home")
                .join("data/oauth-clients")
                .join(format!("{scope}.json"));
            std::fs::create_dir_all(path.parent().unwrap()).expect("test registry directory");
            let original = b"{private damaged registry fixture";
            std::fs::write(&path, original).expect("damaged registry");
            let clients = Arc::new(ClientRegistry::load(scope, scope));
            let oauth = runtime_with("chatgpt-client-test", clients.clone());

            assert_registration_unavailable(
                confidential_registration(&oauth),
                "Client registration is temporarily unavailable.",
            );
            assert!(clients.is_empty(), "unknown identities were not recovered");
            assert_eq!(
                std::fs::read(&path).expect("preserved damaged registry"),
                original.as_slice()
            );
            remove_client_registry(scope).expect("clean registry");
        },
    );
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
/// 重新从磁盘加载注册表，再构造 OAuthRuntime；不复用原来的注册表 Arc。
#[test]
fn restart_keeps_registered_clients_usable() {
    run_isolated(
        module_path!(),
        "restart_keeps_registered_clients_usable",
        || {
            let scope = "oauth-restart-persisted";
            let before = runtime_with(
                "chatgpt-client-test",
                Arc::new(ClientRegistry::load(scope, scope)),
            );
            let client_id = register(&before);
            let (access, refresh) =
                authorize_and_exchange(&before, &client_id, "https://lb.example.com");
            drop(before);

            let after = runtime_with(
                "chatgpt-client-test",
                Arc::new(ClientRegistry::load(scope, scope)),
            );
            assert!(after.client_id_allowed(&client_id));
            assert!(after.verify_access_token(&access, "https://lb.example.com"));
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
            authorize_and_exchange(&after, &client_id, "https://lb.example.com");
            remove_client_registry(scope).expect("clean registry");
        },
    );
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

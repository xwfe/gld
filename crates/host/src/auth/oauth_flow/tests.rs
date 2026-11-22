use super::*;
use gld_core::auth::remove_client_registry;
use gld_core::home::data_home;

use crate::auth::test_home::run_isolated;

const OAUTH_MAX_BODY_BYTES: usize = 8_192;

const AUDIENCE: &str = "gld:actions:test-workspace";
const PASSWORD: &str = "test-password";
const TOKEN_SECRET: &str = "token-signing-secret";
const REDIRECT_URI: &str = "https://chatgpt.com/connector/oauth/test";
const VERIFIER: &str = "dBjftJeZ4CVP-mB92Kpru-AEJvkQlLgi3ThpmQ45N_Xyo";

fn runtime(client_id: &str) -> OAuthRuntime {
    runtime_with(client_id, Arc::new(ClientRegistry::in_memory()))
}

fn runtime_with(client_id: &str, clients: Arc<ClientRegistry>) -> OAuthRuntime {
    OAuthRuntime::new_actions(
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

const DAY: i64 = 60 * 60 * 24;

/// MCP 运行时连着一份真实的授权存储：`store` 是写者，`oauth` 里是它发出的共享读者。
struct McpFixture {
    _home: tempfile::TempDir,
    store: gld_core::auth::AccessStore,
    oauth: OAuthRuntime,
    confirmation: String,
    grant_id: String,
}

fn create(
    store: &gld_core::auth::AccessStore,
    name: &str,
    kind: CredentialKind,
) -> gld_core::auth::IssuedAccess {
    store
        .create_grant(
            gld_core::auth::GrantCreation {
                name: name.into(),
                scope: gld_core::auth::ProjectScope::AllProjects,
                permissions: gld_core::auth::GrantPermissions::default(),
                kind,
                ttl: None,
                allowed_origins: vec![],
            },
            unix_now(),
        )
        .unwrap()
}

fn mcp_with_secret(token_secret: &str) -> McpFixture {
    let home = tempfile::tempdir().unwrap();
    let store = gld_core::auth::AccessStore::open_in(home.path()).unwrap();
    let issued = create(&store, "chat-client", CredentialKind::OAuth);
    let oauth = OAuthRuntime::new_mcp(
        AUDIENCE.into(),
        "hub".into(),
        "chatgpt-client-test".into(),
        None,
        token_secret.into(),
        Arc::new(ClientRegistry::in_memory()),
        store.reader(),
    );
    McpFixture {
        _home: home,
        store,
        oauth,
        confirmation: issued.secret.into_string(),
        grant_id: issued.record.grant.id,
    }
}

fn mcp() -> McpFixture {
    mcp_with_secret(TOKEN_SECRET)
}

fn mcp_claims(token: &str) -> McpTokenClaims {
    decode_claims(token, TOKEN_SECRET, AUDIENCE).expect("mcp claims")
}

fn submit(oauth: &OAuthRuntime, client_id: &str, offered: &str) -> Response {
    authorize_post(
        oauth,
        AuthorizeForm {
            client_id: client_id.into(),
            redirect_uri: REDIRECT_URI.into(),
            code_challenge: challenge(),
            code_challenge_method: "S256".into(),
            state: "state".into(),
            password: offered.into(),
        },
        "https://service.example.com",
    )
}

fn body_text(response: Response) -> String {
    let bytes = gld_core::async_rt::block_on(async {
        axum::body::to_bytes(response.into_body(), OAUTH_MAX_BODY_BYTES * 16)
            .await
            .expect("read body")
    });
    String::from_utf8(bytes.to_vec()).expect("utf8 body")
}

fn assert_no_store(response: &Response) {
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
}

#[test]
fn mcp_tokens_bind_grant_client_and_revision_with_rfc_lifetimes() {
    let fixture = mcp();
    let oauth = &fixture.oauth;
    let base = "https://service.example.com";
    let code = authorize_code(oauth, "chatgpt-client-test", base, &fixture.confirmation);
    let response = exchange_code(oauth, "chatgpt-client-test", base, code);
    assert_eq!(response.status(), StatusCode::OK);
    assert_no_store(&response);
    let body = body_json(response);
    assert_eq!(body["expires_in"], 30 * DAY);
    let access = body["access_token"].as_str().unwrap();
    let refresh = body["refresh_token"].as_str().unwrap();

    for (token, kind, ttl) in [(access, "access", 30 * DAY), (refresh, "refresh", 90 * DAY)] {
        let claims = mcp_claims(token);
        assert_eq!(claims.version, MCP_CLAIMS_VERSION);
        assert_eq!(claims.grant_id, fixture.grant_id);
        assert_eq!(claims.credential_revision, 1);
        assert_eq!(claims.client_id, "chatgpt-client-test");
        assert_eq!(claims.token_use, mcp_token_use(kind));
        assert_eq!(claims.exp - claims.iat, ttl);
    }
    let caller = oauth.authenticate_mcp_access(access).expect("access");
    assert!(caller.allows_project("any-project"));
    // refresh 不能当 access 用，access 也不能拿去续签。
    assert!(oauth.authenticate_mcp_access(refresh).is_none());
    let misuse = refresh_with(oauth, "chatgpt-client-test", access.to_string());
    assert_eq!(body_json(misuse)["error"], "invalid_grant");

    let renewed = refresh_with(oauth, "chatgpt-client-test", refresh.to_string());
    assert_eq!(renewed.status(), StatusCode::OK);
    let renewed = body_json(renewed);
    let next = oauth
        .authenticate_mcp_access(renewed["access_token"].as_str().unwrap())
        .expect("renewed access");
    assert_eq!(caller.id(), next.id());
    assert_eq!(
        mcp_claims(renewed["refresh_token"].as_str().unwrap()).grant_id,
        fixture.grant_id
    );
}

#[test]
fn authorization_page_rejects_every_wrong_credential_the_same_way_without_echoing_it() {
    let fixture = mcp();
    let token = create(&fixture.store, "token", CredentialKind::Token)
        .secret
        .into_string();
    let other = create(&fixture.store, "other", CredentialKind::OAuth);
    fixture.store.revoke(&other.record.grant.id).unwrap();
    let revoked = other.secret.into_string();
    // 公开 ID 存在但秘密错误，以及格式正确但 ID 不存在。
    let (prefix, secret) = fixture.confirmation.rsplit_once('.').unwrap();
    let wrong_secret = format!("{prefix}.{}", "A".repeat(secret.len()));
    let unknown_id = format!("gld_oauth_v1.{}.{secret}", "0".repeat(32));
    let mut bodies = Vec::new();
    for offered in [
        wrong_secret.as_str(),
        unknown_id.as_str(),
        token.as_str(),
        revoked.as_str(),
        PASSWORD,
        "",
    ] {
        let response = submit(&fixture.oauth, "chatgpt-client-test", offered);
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_no_store(&response);
        assert!(response.headers().get("location").is_none());
        let body = body_text(response);
        assert!(body.contains("Invalid authorization credential"));
        if !offered.is_empty() {
            assert!(!body.contains(offered));
        }
        bodies.push(body);
    }
    assert!(bodies.iter().all(|body| body == &bodies[0]));

    let granted = submit(&fixture.oauth, "chatgpt-client-test", &fixture.confirmation);
    assert_eq!(granted.status(), StatusCode::SEE_OTHER);
    assert_no_store(&granted);
    let location = granted.headers()["location"].to_str().unwrap();
    assert!(!location.contains(&fixture.confirmation));
}

#[test]
fn rotation_and_revocation_reject_old_access_refresh_and_codes_without_restart() {
    let fixture = mcp();
    let oauth = &fixture.oauth;
    let base = "https://service.example.com";
    let (access, refresh) = {
        let code = authorize_code(oauth, "chatgpt-client-test", base, &fixture.confirmation);
        let body = body_json(exchange_code(oauth, "chatgpt-client-test", base, code));
        (
            body["access_token"].as_str().unwrap().to_string(),
            body["refresh_token"].as_str().unwrap().to_string(),
        )
    };
    let before = oauth.authenticate_mcp_access(&access).expect("access");
    let stale_code = authorize_code(oauth, "chatgpt-client-test", base, &fixture.confirmation);

    let rotated = fixture
        .store
        .rotate_grant("chat-client", None, unix_now())
        .unwrap()
        .secret
        .into_string();
    assert!(oauth.authenticate_mcp_access(&access).is_none());
    for response in [
        refresh_with(oauth, "chatgpt-client-test", refresh.clone()),
        exchange_code(oauth, "chatgpt-client-test", base, stale_code),
    ] {
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(response)["error"], "invalid_grant");
    }
    assert_eq!(
        submit(oauth, "chatgpt-client-test", &fixture.confirmation).status(),
        StatusCode::UNAUTHORIZED
    );

    let code = authorize_code(oauth, "chatgpt-client-test", base, &rotated);
    let body = body_json(exchange_code(oauth, "chatgpt-client-test", base, code));
    let access = body["access_token"].as_str().unwrap();
    assert_eq!(mcp_claims(access).credential_revision, 2);
    let after = oauth.authenticate_mcp_access(access).expect("new access");
    assert_eq!(before.id(), after.id());

    fixture.store.revoke(&fixture.grant_id).unwrap();
    assert!(oauth.authenticate_mcp_access(access).is_none());
    let refused = refresh_with(
        oauth,
        "chatgpt-client-test",
        body["refresh_token"].as_str().unwrap().to_string(),
    );
    assert_eq!(body_json(refused)["error"], "invalid_grant");
}

#[test]
fn clients_sharing_a_grant_get_separate_callers_and_cannot_swap_refresh_tokens() {
    let fixture = mcp();
    let oauth = &fixture.oauth;
    let base = "https://service.example.com";
    let first = register(oauth);
    let second = register(oauth);
    let mut callers = Vec::new();
    let mut refreshes = Vec::new();
    for client in [&first, &second] {
        let code = authorize_code(oauth, client, base, &fixture.confirmation);
        let body = body_json(exchange_code(oauth, client, base, code));
        let access = body["access_token"].as_str().unwrap();
        assert_eq!(mcp_claims(access).grant_id, fixture.grant_id);
        callers.push(oauth.authenticate_mcp_access(access).unwrap());
        refreshes.push(body["refresh_token"].as_str().unwrap().to_string());
    }
    assert_ne!(callers[0].id(), callers[1].id());
    let swapped = refresh_with(oauth, &second, refreshes[0].clone());
    assert_eq!(body_json(swapped)["error"], "invalid_client");
}

#[test]
fn mcp_and_actions_tokens_and_forged_claims_never_cross_over() {
    let fixture = mcp();
    let oauth = &fixture.oauth;
    let base = "https://service.example.com";
    // 同一把签名密钥、同一受众的 Actions 运行时：claims 形状不同，两边互不接受。
    let actions = runtime("chatgpt-client-test");
    let (actions_access, actions_refresh) =
        authorize_and_exchange(&actions, "chatgpt-client-test", base);
    assert!(oauth.authenticate_mcp_access(&actions_access).is_none());
    let refused = refresh_with(oauth, "chatgpt-client-test", actions_refresh);
    assert_eq!(body_json(refused)["error"], "invalid_grant");
    let code = authorize_code(oauth, "chatgpt-client-test", base, &fixture.confirmation);
    let body = body_json(exchange_code(oauth, "chatgpt-client-test", base, code));
    let access = body["access_token"].as_str().unwrap();
    assert!(actions.identify_access_token(access, base).is_none());
    assert_eq!(
        submit(&actions, "chatgpt-client-test", &fixture.confirmation).status(),
        StatusCode::UNAUTHORIZED
    );

    let now = unix_now() as i64;
    let valid = json!({
        "version": MCP_CLAIMS_VERSION, "iss": AUDIENCE, "aud": AUDIENCE,
        "iat": now, "exp": now + 3600, "client_id": "chatgpt-client-test",
        "grant_id": fixture.grant_id, "credential_revision": 1,
        "token_use": mcp_token_use("access")
    });
    assert!(oauth
        .authenticate_mcp_access(&sign_claim_value(&valid))
        .is_some());
    for (field, value) in [
        ("version", json!(2)),
        ("aud", json!("gld:actions:another-workspace")),
        ("iss", json!("gld:actions:another-workspace")),
        ("exp", json!(now - 1)),
        ("client_id", json!("unregistered-client")),
        ("grant_id", json!("another-grant")),
        ("credential_revision", json!(2)),
        ("token_use", json!(mcp_token_use("refresh"))),
        ("token_use", json!(token_use("access"))),
    ] {
        let mut forged = valid.clone();
        forged[field] = value;
        assert!(
            oauth
                .authenticate_mcp_access(&sign_claim_value(&forged))
                .is_none(),
            "{field}"
        );
    }
    let mut missing = valid.clone();
    missing.as_object_mut().unwrap().remove("grant_id");
    assert!(oauth
        .authenticate_mcp_access(&sign_claim_value(&missing))
        .is_none());
    let mut extra = valid.clone();
    extra["scope"] = json!("actions");
    assert!(oauth
        .authenticate_mcp_access(&sign_claim_value(&extra))
        .is_none());
    // 换一把密钥签出来的同样 claims。
    let other_key = encode(
        &Header::new(Algorithm::HS256),
        &valid,
        &EncodingKey::from_secret(b"another-secret"),
    )
    .unwrap();
    assert!(oauth.authenticate_mcp_access(&other_key).is_none());
}

#[test]
fn endpoint_without_a_signing_key_neither_issues_nor_accepts_tokens() {
    let fixture = mcp_with_secret("");
    let response = submit(&fixture.oauth, "chatgpt-client-test", &fixture.confirmation);
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body_json(response)["error"], "temporarily_unavailable");
    let response = refresh_with(&fixture.oauth, "chatgpt-client-test", "anything".into());
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let now = unix_now() as i64;
    let forged = encode(
        &Header::new(Algorithm::HS256),
        &json!({
            "version": MCP_CLAIMS_VERSION, "iss": AUDIENCE, "aud": AUDIENCE,
            "iat": now, "exp": now + 3600, "client_id": "chatgpt-client-test",
            "grant_id": fixture.grant_id, "credential_revision": 1,
            "token_use": mcp_token_use("access")
        }),
        &EncodingKey::from_secret(b""),
    )
    .unwrap();
    assert!(fixture.oauth.authenticate_mcp_access(&forged).is_none());
}

fn sign_claim_value(value: &Value) -> String {
    encode(
        &Header::new(Algorithm::HS256),
        value,
        &EncodingKey::from_secret(TOKEN_SECRET.as_bytes()),
    )
    .unwrap()
}

fn current_claim_value() -> Value {
    let now = unix_now();
    json!({
        "version": ACTIONS_CLAIMS_VERSION,
        "iss": AUDIENCE, "aud": AUDIENCE, "iat": now, "exp": now + 3600,
        "scope": "actions", "client_id": "chatgpt-client-test", "token_use": token_use("access")
    })
}

#[test]
fn legacy_optional_grant_and_missing_claims_are_rejected() {
    let oauth = runtime("chatgpt-client-test");
    let mut legacy = current_claim_value();
    legacy.as_object_mut().unwrap().remove("version");
    legacy["scope"] = json!("mcp");
    legacy["token_use"] = json!("access");
    assert!(!oauth.verify_access_token(&sign_claim_value(&legacy), "https://service.example.com"));
    legacy["grant"] = json!("old-grant");
    legacy["token_use"] = json!("grant_access");
    assert!(!oauth.verify_access_token(&sign_claim_value(&legacy), "https://service.example.com"));
    for missing in ["version", "scope", "client_id", "token_use"] {
        let mut value = current_claim_value();
        value.as_object_mut().unwrap().remove(missing);
        assert!(
            !oauth.verify_access_token(&sign_claim_value(&value), "https://service.example.com")
        );
    }
    let mut unknown = current_claim_value();
    unknown["grant"] = json!("ignored-authority-is-forbidden");
    assert!(!oauth.verify_access_token(&sign_claim_value(&unknown), "https://service.example.com"));
}

#[test]
fn signed_claims_must_match_version_scope_use_client_and_expiry() {
    let oauth = runtime("chatgpt-client-test");
    assert!(oauth.verify_access_token(
        &sign_claim_value(&current_claim_value()),
        "https://service.example.com"
    ));
    for (field, invalid) in [
        ("version", json!(1)),
        ("scope", json!("mcp")),
        ("token_use", json!("access")),
        ("token_use", json!(token_use("refresh"))),
        ("client_id", json!("unknown-client")),
        ("client_id", json!("")),
        ("exp", json!(unix_now() - 1)),
    ] {
        let mut value = current_claim_value();
        value[field] = invalid;
        assert!(
            !oauth.verify_access_token(&sign_claim_value(&value), "https://service.example.com"),
            "{field}"
        );
    }
}

#[test]
fn actions_claims_have_no_optional_grant_or_legacy_token_use() {
    let oauth = runtime("chatgpt-client-test");
    let (access, refresh) =
        authorize_and_exchange(&oauth, "chatgpt-client-test", "https://service.example.com");
    for (token, kind) in [(&access, "access"), (&refresh, "refresh")] {
        let claims: TokenClaims = decode_claims(token, TOKEN_SECRET, AUDIENCE).unwrap();
        assert_eq!(claims.version, ACTIONS_CLAIMS_VERSION);
        assert_eq!(claims.scope, "actions");
        assert_eq!(claims.token_use, token_use(kind));
        assert_ne!(claims.token_use, kind);
    }
}

#[test]
fn authorization_codes_are_single_use_and_pkce_is_required() {
    let oauth = runtime("chatgpt-client-test");
    let base = "https://service.example.com";
    let code = authorize_code(&oauth, "chatgpt-client-test", base, PASSWORD);
    let bad = token_exchange(
        &oauth,
        &HeaderMap::new(),
        TokenForm {
            grant_type: "authorization_code".into(),
            code: code.clone(),
            redirect_uri: REDIRECT_URI.into(),
            code_verifier: "a".repeat(43),
            client_id: "chatgpt-client-test".into(),
            ..TokenForm::default()
        },
        base,
    );
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body_json(bad)["error"], "invalid_grant");
    assert_eq!(
        exchange_code(&oauth, "chatgpt-client-test", base, code).status(),
        StatusCode::BAD_REQUEST
    );
    let code = authorize_code(&oauth, "chatgpt-client-test", base, PASSWORD);
    assert_eq!(
        exchange_code(&oauth, "chatgpt-client-test", base, code.clone()).status(),
        StatusCode::OK
    );
    assert_eq!(
        exchange_code(&oauth, "chatgpt-client-test", base, code).status(),
        StatusCode::BAD_REQUEST
    );
}

/// Actions passwords cannot be replaced by an unrelated MCP grant credential.
#[test]
fn actions_refuses_an_unrelated_grant_password() {
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

/// A signed token does not restore a client that is absent from the current registry.
#[test]
fn refresh_is_refused_when_the_registered_client_is_missing() {
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
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body_json(response)["error"], "invalid_client");
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

/// Only the stable audience is accepted; legacy public-URL audiences do not fall back.
#[test]
fn legacy_public_url_audiences_are_rejected() {
    let oauth = runtime("chatgpt-client-test");
    let legacy_access = create_token(
        "https://lb.example.com",
        &oauth.token_secret,
        OAUTH_TOKEN_TTL_SECONDS,
        "chatgpt-client-test",
        "access",
    )
    .expect("legacy access token");

    assert!(!oauth.verify_access_token(&legacy_access, "https://lb.example.com"));
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

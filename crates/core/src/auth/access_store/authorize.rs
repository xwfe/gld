//! 从当前已提交状态生成本次权限；CallerId 不承担权限缓存职责。

use gld_policy::{AuthContext, CredentialKind, GrantStatus, McpSubject};

use super::AccessReader;
use crate::error::{AppError, AppResult};

/// OAuth 确认口令验证通过后得到的绑定：签发的 access/refresh 只记这两项，
/// 权限、范围和撤销状态每次请求再从当前快照读取。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthBinding {
    pub grant_id: String,
    pub credential_revision: u64,
}

/// URL 入口拒绝请求的两种原因。秘密本身不对时永远是 `Credential`，
/// 所以拿到 `Origin` 的调用方必然已经持有有效秘密，它不能用来试探秘密。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlRejection {
    /// 授权文件读不出来，无法判断。
    Unavailable,
    /// 公开 ID 不存在、用途不符、秘密错误、不在有效期内或 grant 已撤销，不向调用方区分。
    Credential,
    /// 秘密有效，但请求带的 Origin 不在这份 grant 的精确名单里。
    Origin,
}

impl AccessReader {
    /// 验证 `/mcp/connect/<secret>` 路径里拆出的公开 ID 和秘密，每次调用读取最新快照。
    /// `origin` 是请求头原文：没有 Origin 时只凭秘密通过；有 Origin 时必须和名单里的某一项
    /// 逐字节相同（名单为空就只放行不带 Origin 的请求）。不接受 token、OAuth 或隧道凭据。
    pub fn authenticate_url(
        &self,
        entry: &str,
        credential_id: &str,
        secret: &str,
        origin: Option<&str>,
        now_unix_secs: u64,
    ) -> Result<AuthContext, UrlRejection> {
        let current = self.snapshot().map_err(|_| UrlRejection::Unavailable)?;
        let record = current
            .by_credential_id(credential_id)
            .ok_or(UrlRejection::Credential)?;
        if record.credential.kind != CredentialKind::Url
            || record.grant.status != GrantStatus::Active
            || !record.credential.is_valid_at(now_unix_secs)
            || !record.credential.matches_secret(secret)
        {
            return Err(UrlRejection::Credential);
        }
        // Origin 只在秘密验证通过之后才看：它是附加限制，不是凭据。
        if !record.credential.allows_origin(origin) {
            return Err(UrlRejection::Origin);
        }
        AuthContext::verified_mcp(
            entry,
            record.grant.clone(),
            CredentialKind::Url,
            McpSubject::Url,
        )
        .map_err(|_| UrlRejection::Credential)
    }

    /// 验证标准 token 入口已经拆出的公开 ID 和秘密，每次调用读取最新快照。
    /// HTTP 来源/格式分派由宿主负责；此方法不接受 OAuth、URL 或隧道证明。
    pub fn authenticate_token(
        &self,
        entry: &str,
        credential_id: &str,
        secret: &str,
        now_unix_secs: u64,
    ) -> AppResult<AuthContext> {
        let current = self.snapshot()?;
        let record = current
            .by_credential_id(credential_id)
            .ok_or_else(rejected)?;
        if record.credential.kind != CredentialKind::Token
            || record.grant.status != GrantStatus::Active
            || !record.credential.is_valid_at(now_unix_secs)
            || !record.credential.matches_secret(secret)
        {
            return Err(rejected());
        }
        AuthContext::verified_mcp(
            entry,
            record.grant.clone(),
            CredentialKind::Token,
            McpSubject::Token,
        )
        .map_err(|_| rejected())
    }

    /// 授权页提交的确认口令：只接受 OAuth 用途的凭据，并要求 grant 有效、口令在期限内。
    /// 公开 ID 不存在、用途不符、秘密错误、过期和已撤销返回同一个错误。
    pub fn confirm_oauth(
        &self,
        credential_id: &str,
        secret: &str,
        now_unix_secs: u64,
    ) -> AppResult<OAuthBinding> {
        let current = self.snapshot()?;
        let record = current
            .by_credential_id(credential_id)
            .ok_or_else(oauth_rejected)?;
        if record.credential.kind != CredentialKind::OAuth
            || record.grant.status != GrantStatus::Active
            || !record.credential.is_valid_at(now_unix_secs)
            || !record.credential.matches_secret(secret)
        {
            return Err(oauth_rejected());
        }
        Ok(OAuthBinding {
            grant_id: record.grant.id.clone(),
            credential_revision: record.credential.revision,
        })
    }

    /// 验证 OAuth access/refresh 里签名过的绑定是否仍是当前状态，每次调用读取最新快照。
    /// 验签、用途、受众和 token 期限由宿主先完成；这里不接受 token 或 URL 用途的 grant。
    pub fn authenticate_oauth(
        &self,
        entry: &str,
        binding: &OAuthBinding,
        client_id: &str,
        now_unix_secs: u64,
    ) -> AppResult<AuthContext> {
        let current = self.snapshot()?;
        let record = current
            .by_grant_id(&binding.grant_id)
            .ok_or_else(oauth_rejected)?;
        if record.credential.kind != CredentialKind::OAuth
            || record.grant.status != GrantStatus::Active
            || record.credential.revision != binding.credential_revision
            || !record.credential.is_valid_at(now_unix_secs)
        {
            return Err(oauth_rejected());
        }
        AuthContext::verified_mcp(
            entry,
            record.grant.clone(),
            CredentialKind::OAuth,
            McpSubject::OAuth {
                client_id: client_id.to_string(),
            },
        )
        .map_err(|_| oauth_rejected())
    }
}

fn oauth_rejected() -> AppError {
    // 不向调用方区分 grant 不存在、用途错误、修订号过期或已撤销。
    AppError::Message("AUTHENTICATION_FAILED: Invalid OAuth credential".into())
}

fn rejected() -> AppError {
    // 不向调用方区分 ID 不存在、用途错误、秘密错误或已撤销。
    AppError::Message("AUTHENTICATION_FAILED: Invalid API token".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{
        AccessRecord, AccessStore, CredentialRecord, Grant, GrantPermissions, ProjectScope,
    };

    fn record(id: &str, name: &str, secret: &str) -> AccessRecord {
        AccessRecord {
            grant: Grant {
                id: id.into(),
                name: name.into(),
                scope: ProjectScope::AllProjects,
                permissions: GrantPermissions::default(),
                status: GrantStatus::Active,
            },
            credential: CredentialRecord::prepare(
                format!("credential-{id}"),
                CredentialKind::Token,
                secret,
                1,
                100,
                Some(3600),
                Vec::new(),
            )
            .unwrap(),
        }
    }

    #[test]
    fn each_request_uses_current_permissions_but_keeps_the_same_caller() {
        let home = tempfile::tempdir().unwrap();
        let store = AccessStore::open_in(home.path()).unwrap();
        store
            .insert(record("one", "Client", "first-secret"))
            .unwrap();
        let reader = store.reader();
        let authenticate = |secret| reader.authenticate_token("hub", "credential-one", secret, 200);
        let first = authenticate("first-secret").unwrap();
        assert!(first.allows_project("future-project"));
        assert!(!first.may_write_project());

        // 保留旧快照不会影响后续认证；权限收紧无需改变凭据修订号或身份。
        let stale = reader.snapshot().unwrap();
        let mut changed = stale.by_grant_id("one").unwrap().clone();
        changed.grant.name = "Renamed".into();
        changed.grant.scope = ProjectScope::Selected(vec!["selected-project".into()]);
        changed.grant.permissions.write = true;
        store.update(changed.clone()).unwrap();
        let current = authenticate("first-secret").unwrap();
        assert_eq!(first.id(), current.id());
        assert!(current.allows_project("selected-project"));
        assert!(!current.allows_project("future-project"));
        assert!(current.may_write_project());
        assert!(stale
            .by_grant_id("one")
            .unwrap()
            .grant
            .allows("future-project"));

        changed.credential = CredentialRecord::prepare(
            "credential-one".into(),
            CredentialKind::Token,
            "replacement-secret",
            2,
            200,
            Some(3600),
            Vec::new(),
        )
        .unwrap();
        store.update(changed).unwrap();
        assert!(authenticate("first-secret").is_err());
        assert_eq!(first.id(), authenticate("replacement-secret").unwrap().id());
        store.revoke("one").unwrap();
        assert!(authenticate("replacement-secret").is_err());

        store
            .insert(record("two", "Renamed", "new-grant-secret"))
            .unwrap();
        let rebuilt = reader
            .authenticate_token("hub", "credential-two", "new-grant-secret", 200)
            .unwrap();
        assert_ne!(first.id(), rebuilt.id());
    }

    #[test]
    fn token_verification_rejects_wrong_purpose_secret_and_time_without_fallback() {
        let home = tempfile::tempdir().unwrap();
        let store = AccessStore::open_in(home.path()).unwrap();
        store
            .insert(record("token", "Token", "test-secret"))
            .unwrap();
        for (id, kind, ttl) in [
            ("oauth", CredentialKind::OAuth, None),
            ("url", CredentialKind::Url, Some(3600)),
        ] {
            let mut other = record(id, id, "test-secret");
            other.credential = CredentialRecord::prepare(
                format!("credential-{id}"),
                kind,
                "test-secret",
                1,
                100,
                ttl,
                Vec::new(),
            )
            .unwrap();
            store.insert(other).unwrap();
        }
        let reader = store.reader();
        for (id, secret, now) in [
            ("credential-token", "wrong-secret", 100),
            ("credential-token", "test-secret", 99),
            ("credential-token", "test-secret", 3700),
            ("credential-missing", "test-secret", 100),
            ("credential-oauth", "test-secret", 100),
            ("credential-url", "test-secret", 100),
        ] {
            let error = reader
                .authenticate_token("hub", id, secret, now)
                .unwrap_err();
            assert_eq!(
                error.to_string(),
                "AUTHENTICATION_FAILED: Invalid API token"
            );
        }
        assert!(reader
            .authenticate_token("hub", "credential-token", "test-secret", 100)
            .is_ok());
        assert!(reader
            .authenticate_token("hub", "credential-token", "test-secret", 3699)
            .is_ok());
    }

    #[test]
    fn reopening_preserves_scope_revision_and_revocation() {
        let home = tempfile::tempdir().unwrap();
        let store = AccessStore::open_in(home.path()).unwrap();
        let mut active = record("active", "Active", "first-secret");
        let revoked = record("revoked", "Revoked", "revoked-secret");
        store.insert(active.clone()).unwrap();
        store.insert(revoked).unwrap();
        let before = store
            .reader()
            .authenticate_token("hub", "credential-active", "first-secret", 200)
            .unwrap();
        active.grant.scope = ProjectScope::Selected(vec!["project".into()]);
        active.credential = CredentialRecord::prepare(
            "credential-active".into(),
            CredentialKind::Token,
            "second-secret",
            2,
            200,
            Some(3600),
            Vec::new(),
        )
        .unwrap();
        store.update(active).unwrap();
        store.revoke("revoked").unwrap();
        drop(store);

        let reopened = AccessStore::open_in(home.path()).unwrap();
        let reader = reopened.reader();
        let after = reader
            .authenticate_token("hub", "credential-active", "second-secret", 201)
            .unwrap();
        assert_eq!(before.id(), after.id());
        assert!(after.allows_project("project"));
        assert!(!after.allows_project("different"));
        assert!(reader
            .authenticate_token("hub", "credential-active", "first-secret", 201)
            .is_err());
        assert!(reader
            .authenticate_token("hub", "credential-revoked", "revoked-secret", 201)
            .is_err());
        assert_eq!(
            reader
                .snapshot()
                .unwrap()
                .by_grant_id("active")
                .unwrap()
                .credential
                .revision,
            2
        );
    }

    fn oauth_record(id: &str, secret: &str, revision: u64) -> AccessRecord {
        let mut value = record(id, id, secret);
        value.credential = CredentialRecord::prepare(
            format!("credential-{id}"),
            CredentialKind::OAuth,
            secret,
            revision,
            100,
            None,
            Vec::new(),
        )
        .unwrap();
        value
    }

    #[test]
    fn oauth_confirmation_binds_the_current_revision_and_rejects_other_purposes() {
        let home = tempfile::tempdir().unwrap();
        let store = AccessStore::open_in(home.path()).unwrap();
        store.insert(oauth_record("oauth", "confirm", 1)).unwrap();
        store.insert(record("token", "Token", "confirm")).unwrap();
        let reader = store.reader();
        let binding = reader
            .confirm_oauth("credential-oauth", "confirm", 100)
            .unwrap();
        assert_eq!(binding.grant_id, "oauth");
        assert_eq!(binding.credential_revision, 1);
        for (id, secret, now) in [
            ("credential-oauth", "wrong", 100),
            ("credential-oauth", "confirm", 99),
            ("credential-missing", "confirm", 100),
            ("credential-token", "confirm", 100),
        ] {
            assert_eq!(
                reader
                    .confirm_oauth(id, secret, now)
                    .unwrap_err()
                    .to_string(),
                "AUTHENTICATION_FAILED: Invalid OAuth credential"
            );
        }

        store.update(oauth_record("oauth", "rotated", 2)).unwrap();
        assert!(reader
            .confirm_oauth("credential-oauth", "confirm", 100)
            .is_err());
        assert_eq!(
            reader
                .confirm_oauth("credential-oauth", "rotated", 100)
                .unwrap()
                .credential_revision,
            2
        );
        store.revoke("oauth").unwrap();
        assert!(reader
            .confirm_oauth("credential-oauth", "rotated", 100)
            .is_err());
    }

    #[test]
    fn oauth_binding_follows_rotation_and_revocation_and_separates_clients() {
        let home = tempfile::tempdir().unwrap();
        let store = AccessStore::open_in(home.path()).unwrap();
        store.insert(oauth_record("oauth", "confirm", 1)).unwrap();
        store.insert(record("token", "Token", "secret")).unwrap();
        let reader = store.reader();
        let binding = reader
            .confirm_oauth("credential-oauth", "confirm", 100)
            .unwrap();
        let first = reader
            .authenticate_oauth("hub", &binding, "client-a", 100)
            .unwrap();
        let other = reader
            .authenticate_oauth("hub", &binding, "client-b", 100)
            .unwrap();
        assert_ne!(first.id(), other.id());
        assert_eq!(
            first.id(),
            reader
                .authenticate_oauth("hub", &binding, "client-a", 5000)
                .unwrap()
                .id()
        );

        // token 用途的 grant 不能被 OAuth 绑定引用，即使修订号一致。
        let token_binding = OAuthBinding {
            grant_id: "token".into(),
            credential_revision: 1,
        };
        assert!(reader
            .authenticate_oauth("hub", &token_binding, "client-a", 100)
            .is_err());
        let missing = OAuthBinding {
            grant_id: "missing".into(),
            credential_revision: 1,
        };
        assert!(reader
            .authenticate_oauth("hub", &missing, "client-a", 100)
            .is_err());

        store.update(oauth_record("oauth", "rotated", 2)).unwrap();
        assert!(reader
            .authenticate_oauth("hub", &binding, "client-a", 100)
            .is_err());
        let rotated = reader
            .confirm_oauth("credential-oauth", "rotated", 100)
            .unwrap();
        let after = reader
            .authenticate_oauth("hub", &rotated, "client-a", 100)
            .unwrap();
        assert_eq!(first.id(), after.id());
        store.revoke("oauth").unwrap();
        assert!(reader
            .authenticate_oauth("hub", &rotated, "client-a", 100)
            .is_err());
    }

    fn url_record(id: &str, secret: &str, revision: u64, origins: &[&str]) -> AccessRecord {
        let mut value = record(id, id, secret);
        value.credential = CredentialRecord::prepare(
            format!("credential-{id}"),
            CredentialKind::Url,
            secret,
            revision,
            100,
            Some(3600),
            origins.iter().map(|origin| origin.to_string()).collect(),
        )
        .unwrap();
        value
    }

    const EXTENSION: &str = "chrome-extension://kngiafgkdnlkgmefdafaibkibegkcaef";

    #[test]
    fn url_secret_is_checked_before_origin_and_origin_must_match_exactly() {
        let home = tempfile::tempdir().unwrap();
        let store = AccessStore::open_in(home.path()).unwrap();
        store
            .insert(url_record(
                "url",
                "url-secret",
                1,
                &[EXTENSION, "https://app.example.test"],
            ))
            .unwrap();
        store
            .insert(url_record("bare", "bare-secret", 1, &[]))
            .unwrap();
        let reader = store.reader();
        let check = |id: &str, secret: &str, origin: Option<&str>, now: u64| {
            reader
                .authenticate_url("hub", id, secret, origin, now)
                .map(|auth| auth.id().clone())
        };

        // 没有 Origin 时只凭秘密；名单里的 Origin 通过，三种写法是同一个调用方。
        let caller = check("credential-url", "url-secret", None, 100).unwrap();
        for origin in [EXTENSION, "https://app.example.test"] {
            assert_eq!(
                check("credential-url", "url-secret", Some(origin), 100).unwrap(),
                caller
            );
        }
        // 不做归一化：大小写、尾斜杠、端口、子域、`null` 都不是名单里的那一项。
        for origin in [
            "https://other.example.test",
            "https://app.example.test/",
            "https://APP.example.test",
            "https://app.example.test:443",
            "https://sub.app.example.test",
            "http://app.example.test",
            "null",
            "",
        ] {
            assert_eq!(
                check("credential-url", "url-secret", Some(origin), 100),
                Err(UrlRejection::Origin),
                "{origin}"
            );
        }
        // 名单为空：不带 Origin 可以，带任何 Origin 都不行。
        assert!(check("credential-bare", "bare-secret", None, 100).is_ok());
        assert_eq!(
            check("credential-bare", "bare-secret", Some(EXTENSION), 100),
            Err(UrlRejection::Origin)
        );

        // 秘密不对时不管 Origin 在不在名单里，都是同一个凭据错误。
        for (id, secret, origin, now) in [
            ("credential-url", "wrong-secret", Some(EXTENSION), 100),
            (
                "credential-url",
                "wrong-secret",
                Some("https://x.test"),
                100,
            ),
            ("credential-url", "wrong-secret", None, 100),
            ("credential-url", "bare-secret", Some(EXTENSION), 100),
            ("credential-missing", "url-secret", Some(EXTENSION), 100),
            ("credential-url", "url-secret", Some(EXTENSION), 99),
            ("credential-url", "url-secret", Some(EXTENSION), 3700),
            ("credential-url", "url-secret", Some("https://x.test"), 3700),
        ] {
            assert_eq!(
                check(id, secret, origin, now),
                Err(UrlRejection::Credential),
                "{id} {origin:?} {now}"
            );
        }
        assert!(check("credential-url", "url-secret", Some(EXTENSION), 3699).is_ok());
    }

    #[test]
    fn url_entry_rejects_other_purposes_and_other_entries_reject_url_secrets() {
        let home = tempfile::tempdir().unwrap();
        let store = AccessStore::open_in(home.path()).unwrap();
        store
            .insert(url_record("url", "shared-secret", 1, &[EXTENSION]))
            .unwrap();
        store
            .insert(record("token", "Token", "shared-secret"))
            .unwrap();
        store
            .insert(oauth_record("oauth", "shared-secret", 1))
            .unwrap();
        let reader = store.reader();
        for id in ["credential-token", "credential-oauth"] {
            assert_eq!(
                reader
                    .authenticate_url("hub", id, "shared-secret", None, 100)
                    .unwrap_err(),
                UrlRejection::Credential
            );
        }
        assert!(reader
            .authenticate_token("hub", "credential-url", "shared-secret", 100)
            .is_err());
        assert!(reader
            .confirm_oauth("credential-url", "shared-secret", 100)
            .is_err());

        let url = reader
            .authenticate_url("hub", "credential-url", "shared-secret", None, 100)
            .unwrap();
        let token = reader
            .authenticate_token("hub", "credential-token", "shared-secret", 100)
            .unwrap();
        assert_ne!(url.id(), token.id());
        assert!(!url.may_delegate());
    }

    #[test]
    fn url_rotation_keeps_caller_and_origins_and_revocation_blocks_the_next_request() {
        let home = tempfile::tempdir().unwrap();
        let store = AccessStore::open_in(home.path()).unwrap();
        let mut current = url_record("url", "first-secret", 1, &[EXTENSION]);
        current.grant.permissions.write = true;
        store.insert(current.clone()).unwrap();
        let reader = store.reader();
        let check = |secret: &str, origin: Option<&str>| {
            reader.authenticate_url("hub", "credential-url", secret, origin, 200)
        };
        let before = check("first-secret", Some(EXTENSION)).unwrap();
        assert!(before.may_write_project());
        // 可写的 URL grant 也不能委派 Agent。
        assert!(!before.may_delegate());

        // 权限收紧在下一次请求生效，身份不变。
        current.grant.permissions.write = false;
        current.grant.scope = ProjectScope::Selected(vec!["selected".into()]);
        store.update(current.clone()).unwrap();
        let narrowed = check("first-secret", Some(EXTENSION)).unwrap();
        assert_eq!(before.id(), narrowed.id());
        assert!(!narrowed.may_write_project());
        assert!(!narrowed.allows_project("other"));

        let mut rotated = url_record("url", "second-secret", 2, &[EXTENSION]);
        rotated.grant = current.grant.clone();
        store.update(rotated).unwrap();
        assert_eq!(
            check("first-secret", Some(EXTENSION)).unwrap_err(),
            UrlRejection::Credential
        );
        assert_eq!(
            before.id(),
            check("second-secret", Some(EXTENSION)).unwrap().id()
        );
        assert_eq!(
            check("second-secret", Some("https://other.example.test")).unwrap_err(),
            UrlRejection::Origin
        );

        store.revoke("url").unwrap();
        assert_eq!(
            check("second-secret", Some(EXTENSION)).unwrap_err(),
            UrlRejection::Credential
        );
        // 已撤销的 grant 不再透露 Origin 是否在名单里。
        assert_eq!(
            check("second-secret", Some("https://other.example.test")).unwrap_err(),
            UrlRejection::Credential
        );
    }
}

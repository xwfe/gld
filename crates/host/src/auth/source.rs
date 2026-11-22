//! 在触碰授权存储前确定唯一来源；不能因一种方式失败再尝试另一种。

use axum::http::{header::AUTHORIZATION, HeaderMap};
use gld_core::auth::{bearer_token, parse_credential, CredentialKind};
use gld_core::hub::OPENAI_TUNNEL_HEADER;

/// 仅做来源分类，绝不把格式匹配当作认证成功。
pub(crate) enum CredentialSource<'a> {
    Token { id: &'a str, secret: &'a str },
    OAuthBearer(&'a str),
    Url { id: &'a str, secret: &'a str },
    Tunnel(&'a str),
}

/// API token 的用途前缀是封闭格式。以该前缀开头但无法解析时拒绝，
/// 不允许把格式错误的 token 重新解释为 OAuth。
pub(crate) fn standard(headers: &HeaderMap) -> Result<CredentialSource<'_>, &'static str> {
    let auth_values = headers.get_all(AUTHORIZATION);
    let tunnel_values = headers.get_all(OPENAI_TUNNEL_HEADER);
    if auth_values.iter().count() > 1 || tunnel_values.iter().count() > 1 {
        return Err("duplicate_credential_source");
    }
    let auth = auth_values.iter().next();
    let tunnel = tunnel_values.iter().next();
    match (auth, tunnel) {
        (Some(_), Some(_)) => Err("conflicting_credential_sources"),
        (None, None) => Err("missing_credential"),
        (None, Some(value)) => value
            .to_str()
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(CredentialSource::Tunnel)
            .ok_or("invalid_credential"),
        (Some(value), None) => {
            let header = value.to_str().map_err(|_| "invalid_credential")?;
            let secret = bearer_token(header).ok_or("invalid_credential")?;
            if secret.starts_with("gld_") {
                let id = parse_credential(secret, CredentialKind::Token)
                    .map_err(|_| "invalid_credential")?;
                Ok(CredentialSource::Token { id, secret })
            } else {
                Ok(CredentialSource::OAuthBearer(secret))
            }
        }
    }
}

pub(crate) fn url<'a>(
    headers: &HeaderMap,
    secret: &'a str,
) -> Result<CredentialSource<'a>, &'static str> {
    if headers.contains_key(AUTHORIZATION) || headers.contains_key(OPENAI_TUNNEL_HEADER) {
        return Err("conflicting_credential_sources");
    }
    let id = parse_credential(secret, CredentialKind::Url).map_err(|_| "invalid_credential")?;
    Ok(CredentialSource::Url { id, secret })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use gld_core::auth::{
        AccessStore, CredentialTtl, GrantCreation, GrantPermissions, ProjectScope,
    };

    fn issued(kind: CredentialKind) -> String {
        let tmp = tempfile::tempdir().unwrap();
        let store = AccessStore::open_in(tmp.path()).unwrap();
        store
            .create_grant(
                GrantCreation {
                    name: "test".into(),
                    scope: ProjectScope::AllProjects,
                    permissions: GrantPermissions::default(),
                    kind,
                    ttl: (kind == CredentialKind::Url).then_some(CredentialTtl::Seconds(600)),
                    allowed_origins: vec![],
                },
                100,
            )
            .unwrap()
            .secret
            .into_string()
    }

    #[test]
    fn explicit_source_never_falls_back_to_another_purpose() {
        let token = issued(CredentialKind::Token);
        let oauth = issued(CredentialKind::OAuth);
        let url_secret = issued(CredentialKind::Url);
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        match standard(&headers).unwrap() {
            CredentialSource::Token { id, secret } => {
                assert_eq!(id.len(), 32);
                assert_eq!(secret, token);
            }
            _ => panic!("wrong credential purpose"),
        }
        for wrong in [&oauth, &url_secret, &"gld_token_v1.invalid".to_string()] {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {wrong}")).unwrap(),
            );
            assert!(standard(&headers).is_err());
        }
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer eyJhbGciOiJIUzI1NiJ9.payload.signature"),
        );
        assert!(matches!(
            standard(&headers),
            Ok(CredentialSource::OAuthBearer(_))
        ));
        headers.clear();
        match url(&headers, &url_secret).unwrap() {
            CredentialSource::Url { id, secret } => {
                assert_eq!(id.len(), 32);
                assert_eq!(secret, url_secret);
            }
            _ => panic!("wrong url credential"),
        }
        assert!(url(&headers, &token).is_err());
    }

    #[test]
    fn duplicate_conflicting_and_missing_sources_are_rejected() {
        let mut headers = HeaderMap::new();
        assert!(standard(&headers).is_err());
        headers.append(AUTHORIZATION, HeaderValue::from_static("Bearer one"));
        headers.append(AUTHORIZATION, HeaderValue::from_static("Bearer two"));
        assert!(standard(&headers).is_err());
        headers.remove(AUTHORIZATION);
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer opaque"));
        headers.insert(OPENAI_TUNNEL_HEADER, HeaderValue::from_static("tunnel"));
        assert!(standard(&headers).is_err());
        assert!(url(&headers, &issued(CredentialKind::Url)).is_err());
        headers.remove(AUTHORIZATION);
        match standard(&headers).unwrap() {
            CredentialSource::Tunnel(value) => assert_eq!(value, "tunnel"),
            _ => panic!("wrong tunnel source"),
        }
        headers.append(OPENAI_TUNNEL_HEADER, HeaderValue::from_static("another"));
        assert!(standard(&headers).is_err());
    }
}

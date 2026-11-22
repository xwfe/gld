use std::fmt;

use gld_policy::{CredentialKind, Grant};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::auth::constant_time_eq_str;

pub const MAX_URL_TTL_SECONDS: u64 = 24 * 60 * 60;

/// 存储中只有验证摘要；原始秘密由创建用例一次性交付。
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialRecord {
    pub id: String,
    pub kind: CredentialKind,
    pub revision: u64,
    pub issued_at_unix_secs: u64,
    #[serde(deserialize_with = "required_option")]
    pub valid_for_secs: Option<u64>,
    #[serde(deserialize_with = "required_option")]
    pub expires_at_unix_secs: Option<u64>,
    pub allowed_origins: Vec<String>,
    secret_sha256: String,
}

impl CredentialRecord {
    /// 不生成秘密或读取时钟；创建/轮换用例负责高熵生成及一次性交付。
    pub fn prepare(
        id: String,
        kind: CredentialKind,
        secret: &str,
        revision: u64,
        issued_at_unix_secs: u64,
        valid_for_secs: Option<u64>,
        allowed_origins: Vec<String>,
    ) -> Result<Self, String> {
        if secret.is_empty() {
            return Err("Credential secret must not be empty".into());
        }
        let expires_at_unix_secs = valid_for_secs
            .map(|ttl| {
                issued_at_unix_secs
                    .checked_add(ttl)
                    .ok_or("Credential expiry overflows")
            })
            .transpose()?;
        let record = Self {
            id,
            kind,
            revision,
            issued_at_unix_secs,
            valid_for_secs,
            expires_at_unix_secs,
            allowed_origins,
            secret_sha256: secret_hash(secret),
        };
        record.validate()?;
        Ok(record)
    }

    /// 这里只比摘要；认证入口还须验证用途、期限、Origin 和 grant 状态。
    pub fn matches_secret(&self, secret: &str) -> bool {
        !secret.is_empty() && constant_time_eq_str(&secret_hash(secret), &self.secret_sha256)
    }

    pub fn is_valid_at(&self, now_unix_secs: u64) -> bool {
        now_unix_secs >= self.issued_at_unix_secs
            && self
                .expires_at_unix_secs
                .is_none_or(|expires| now_unix_secs < expires)
    }

    pub fn allows_origin(&self, origin: Option<&str>) -> bool {
        origin.is_none_or(|value| self.allowed_origins.iter().any(|item| item == value))
    }

    pub(super) fn validate(&self) -> Result<(), String> {
        if self.id.is_empty()
            || self.id.len() > 128
            || !self
                .id
                .bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'-' | b'_'))
        {
            return Err("Invalid credential ID".into());
        }
        if self.revision == 0 {
            return Err("Credential revision must be positive".into());
        }
        if self.secret_sha256.len() != 64
            || !self
                .secret_sha256
                .bytes()
                .all(|ch| ch.is_ascii_digit() || (b'a'..=b'f').contains(&ch))
        {
            return Err("Invalid credential verification digest".into());
        }
        let expected_expiry = match self.valid_for_secs {
            Some(0) => return Err("Credential lifetime must be positive".into()),
            Some(ttl) => Some(
                self.issued_at_unix_secs
                    .checked_add(ttl)
                    .ok_or("Credential expiry overflows")?,
            ),
            None => None,
        };
        if expected_expiry != self.expires_at_unix_secs {
            return Err("Credential expiry does not match its lifetime".into());
        }
        match self.kind {
            CredentialKind::OAuth => {
                if self.valid_for_secs.is_some() || !self.allowed_origins.is_empty() {
                    return Err("OAuth confirmation credentials do not accept TTL or Origin".into());
                }
            }
            CredentialKind::Token => {
                if !self.allowed_origins.is_empty() {
                    return Err("API tokens do not accept Origin restrictions".into());
                }
            }
            CredentialKind::Url => {
                if !matches!(self.valid_for_secs, Some(1..=MAX_URL_TTL_SECONDS)) {
                    return Err(
                        "URL credential lifetime must be between 1 second and 24 hours".into(),
                    );
                }
                let mut seen = std::collections::HashSet::new();
                if self
                    .allowed_origins
                    .iter()
                    .any(|origin| !valid_origin(origin) || !seen.insert(origin.as_str()))
                {
                    return Err("URL Origins must be unique exact supported origins".into());
                }
            }
        }
        Ok(())
    }

    pub(super) fn same_material(&self, other: &Self) -> bool {
        self.id == other.id
            && std::mem::discriminant(&self.kind) == std::mem::discriminant(&other.kind)
            && self.revision == other.revision
            && self.issued_at_unix_secs == other.issued_at_unix_secs
            && self.valid_for_secs == other.valid_for_secs
            && self.expires_at_unix_secs == other.expires_at_unix_secs
            && self.allowed_origins == other.allowed_origins
            && constant_time_eq_str(&self.secret_sha256, &other.secret_sha256)
    }
}

impl fmt::Debug for CredentialRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.kind {
            CredentialKind::OAuth => "OAuth",
            CredentialKind::Token => "Token",
            CredentialKind::Url => "Url",
        };
        f.debug_struct("CredentialRecord")
            .field("id", &self.id)
            .field("kind", &kind)
            .field("revision", &self.revision)
            .field("issued_at_unix_secs", &self.issued_at_unix_secs)
            .field("expires_at_unix_secs", &self.expires_at_unix_secs)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessRecord {
    pub grant: Grant,
    pub credential: CredentialRecord,
}

impl AccessRecord {
    pub(super) fn validate(&self) -> Result<(), String> {
        self.grant.validate().map_err(|error| error.to_string())?;
        self.credential.validate()?;
        if matches!(self.credential.kind, CredentialKind::Url) {
            let service = &self.grant.permissions.service;
            if service.user_prompts || service.machine_mcp || service.managed_workspaces {
                return Err("URL credentials cannot carry service capabilities".into());
            }
        }
        Ok(())
    }
}

fn secret_hash(secret: &str) -> String {
    format!("{:x}", Sha256::digest(secret.as_bytes()))
}

fn valid_origin(origin: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(origin) else {
        return false;
    };
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let canonical = match url.scheme() {
        "https" => url.origin().ascii_serialization(),
        "http"
            if url.host_str().is_some_and(|host| {
                host == "localhost"
                    || host
                        .trim_start_matches('[')
                        .trim_end_matches(']')
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|address| address.is_loopback())
            }) =>
        {
            url.origin().ascii_serialization()
        }
        "chrome-extension" | "moz-extension" if url.port().is_none() => {
            let Some(host) = url.host_str() else {
                return false;
            };
            if host.is_empty()
                || !host
                    .bytes()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == b'-')
            {
                return false;
            }
            format!("{}://{host}", url.scheme())
        }
        _ => return false,
    };
    origin == canonical
}

fn required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

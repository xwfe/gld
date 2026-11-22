//! 统一创建凭据的材料格式与期限；不负责 HTTP 分派或存储提交。

use std::{fmt, str::FromStr};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use gld_policy::CredentialKind;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Serialize, Serializer};

use super::access_store::{CredentialRecord, MAX_URL_TTL_SECONDS};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialTtl {
    Unlimited,
    Seconds(u64),
}

impl FromStr for CredentialTtl {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || "TTL must be none or a positive integer followed by s/m/h/d".to_string();
        if value == "none" {
            return Ok(Self::Unlimited);
        }
        let (number, factor) = match value.as_bytes().last() {
            Some(b's') => (&value[..value.len() - 1], 1),
            Some(b'm') => (&value[..value.len() - 1], 60),
            Some(b'h') => (&value[..value.len() - 1], 3600),
            Some(b'd') => (&value[..value.len() - 1], 86400),
            _ => return Err(invalid()),
        };
        if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid());
        }
        number
            .parse::<u64>()
            .ok()
            .and_then(|number| number.checked_mul(factor))
            .filter(|seconds| *seconds > 0)
            .map(Self::Seconds)
            .ok_or_else(invalid)
    }
}

pub fn credential_lifetime(
    kind: CredentialKind,
    ttl: Option<CredentialTtl>,
    previous: Option<&CredentialRecord>,
) -> Result<Option<u64>, String> {
    if previous.is_some_and(|previous| previous.kind != kind) {
        return Err("A grant cannot change credential kind".into());
    }
    if kind == CredentialKind::OAuth {
        return if ttl.is_some() {
            Err("OAuth confirmation credentials do not accept TTL".into())
        } else {
            Ok(None)
        };
    }
    let seconds = match ttl {
        Some(CredentialTtl::Unlimited) => None,
        Some(CredentialTtl::Seconds(seconds)) => Some(seconds),
        None => match previous {
            Some(previous) => previous.valid_for_secs,
            None if kind == CredentialKind::Token => Some(30 * 86400),
            None => Some(8 * 3600),
        },
    };
    if seconds == Some(0) {
        return Err("Credential lifetime must be positive".into());
    }
    if kind == CredentialKind::Url && !matches!(seconds, Some(1..=MAX_URL_TTL_SECONDS)) {
        return Err("URL credential lifetime must be between 1 second and 24 hours".into());
    }
    Ok(seconds)
}

/// 只应放入创建或轮换成功的一次性交付响应。
pub struct IssuedSecret(String);

impl IssuedSecret {
    pub fn into_string(self) -> String {
        self.0
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for IssuedSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("IssuedSecret([REDACTED])")
    }
}

impl Serialize for IssuedSecret {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

pub(crate) fn generate_secret(kind: CredentialKind, id: &str) -> Result<IssuedSecret, String> {
    if !valid_id(id) {
        return Err("Invalid credential ID".into());
    }
    let mut random = [0u8; 32];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| "Cannot generate credential secret".to_string())?;
    Ok(IssuedSecret(format!(
        "{}.{id}.{}",
        prefix(kind),
        URL_SAFE_NO_PAD.encode(random)
    )))
}

/// 仅解析一种已确定用途；失败不暴露原始秘密或尝试其他用途。
pub fn parse_credential(value: &str, expected_kind: CredentialKind) -> Result<&str, String> {
    let invalid = || "Invalid credential".to_string();
    let mut parts = value.split('.');
    let kind = parts.next().ok_or_else(invalid)?;
    let id = parts.next().ok_or_else(invalid)?;
    let secret = parts.next().ok_or_else(invalid)?;
    if parts.next().is_some()
        || kind != prefix(expected_kind)
        || !valid_id(id)
        || secret.len() != 43
    {
        return Err(invalid());
    }
    let bytes = URL_SAFE_NO_PAD.decode(secret).map_err(|_| invalid())?;
    if bytes.len() != 32 || URL_SAFE_NO_PAD.encode(&bytes) != secret {
        return Err(invalid());
    }
    Ok(id)
}

fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn prefix(kind: CredentialKind) -> &'static str {
    match kind {
        CredentialKind::Token => "gld_token_v1",
        CredentialKind::OAuth => "gld_oauth_v1",
        CredentialKind::Url => "gld_url_v1",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn generated_material_is_purpose_bound_and_redacted() {
        for kind in [
            CredentialKind::Token,
            CredentialKind::OAuth,
            CredentialKind::Url,
        ] {
            let secret = generate_secret(kind, ID).unwrap();
            assert_eq!(parse_credential(secret.as_str(), kind).unwrap(), ID);
            for other in [
                CredentialKind::Token,
                CredentialKind::OAuth,
                CredentialKind::Url,
            ] {
                assert_eq!(
                    parse_credential(secret.as_str(), other).is_ok(),
                    other == kind
                );
            }
            assert_eq!(format!("{secret:?}"), "IssuedSecret([REDACTED])");
            assert_eq!(
                serde_json::to_string(&secret).unwrap(),
                format!("\"{}\"", secret.as_str())
            );
            assert!(secret.into_string().starts_with(prefix(kind)));
        }
        assert!(generate_secret(CredentialKind::Token, &ID.to_ascii_uppercase()).is_err());
    }

    #[test]
    fn parser_rejects_noncanonical_and_malformed_material() {
        let secret = URL_SAFE_NO_PAD.encode([0u8; 32]);
        let valid = format!("gld_token_v1.{ID}.{secret}");
        assert_eq!(parse_credential(&valid, CredentialKind::Token).unwrap(), ID);
        let bad_tail = format!("{}B", &secret[..42]);
        for value in [
            format!(" {valid}"),
            format!("{valid} "),
            format!("{valid}="),
            format!("{valid}.extra"),
            format!("gld_token_v1.{ID}.{bad_tail}"),
            format!("gld_token_v1.{}.{secret}", ID.to_ascii_uppercase()),
            format!("gld_token_v2.{ID}.{secret}"),
            format!("gld_token_v1..{secret}"),
        ] {
            assert_eq!(
                parse_credential(&value, CredentialKind::Token).unwrap_err(),
                "Invalid credential"
            );
        }
    }

    #[test]
    fn ttl_syntax_is_strict_and_checked() {
        assert_eq!("none".parse(), Ok(CredentialTtl::Unlimited));
        assert_eq!("30d".parse(), Ok(CredentialTtl::Seconds(2592000)));
        assert_eq!("24h".parse(), Ok(CredentialTtl::Seconds(86400)));
        assert_eq!("2m".parse(), Ok(CredentialTtl::Seconds(120)));
        assert_eq!("1s".parse(), Ok(CredentialTtl::Seconds(1)));
        for value in [
            "",
            "none ",
            " 1h",
            "+1h",
            "-1h",
            "0s",
            "1.5h",
            "1H",
            "1",
            "18446744073709551616s",
            "18446744073709551615d",
        ] {
            assert!(value.parse::<CredentialTtl>().is_err(), "{value:?}");
        }
    }

    #[test]
    fn lifetimes_enforce_kind_and_preserve_original_duration() {
        use CredentialKind::{OAuth, Token, Url};
        use CredentialTtl::{Seconds, Unlimited};
        assert_eq!(
            credential_lifetime(Token, None, None).unwrap(),
            Some(2592000)
        );
        assert_eq!(credential_lifetime(Url, None, None).unwrap(), Some(28800));
        assert_eq!(credential_lifetime(OAuth, None, None).unwrap(), None);
        assert_eq!(
            credential_lifetime(Token, Some(Unlimited), None).unwrap(),
            None
        );
        assert_eq!(
            credential_lifetime(Url, Some(Seconds(86400)), None).unwrap(),
            Some(86400)
        );
        for ttl in [Unlimited, Seconds(0), Seconds(86401)] {
            assert!(credential_lifetime(Url, Some(ttl), None).is_err());
        }
        for ttl in [Unlimited, Seconds(1)] {
            assert!(credential_lifetime(OAuth, Some(ttl), None).is_err());
        }
        assert!(credential_lifetime(Token, Some(Seconds(0)), None).is_err());
        for (kind, duration) in [
            (Token, None),
            (Token, Some(3600)),
            (Url, Some(600)),
            (OAuth, None),
        ] {
            let previous =
                CredentialRecord::prepare(ID.into(), kind, "test-only", 7, 100, duration, vec![])
                    .unwrap();
            assert_eq!(
                credential_lifetime(kind, None, Some(&previous)).unwrap(),
                duration
            );
            if kind != OAuth {
                assert_eq!(
                    credential_lifetime(kind, Some(Seconds(42)), Some(&previous)).unwrap(),
                    Some(42)
                );
            }
        }
    }
}

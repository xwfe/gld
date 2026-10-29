//! 已认证 Hub 在 params._meta 中传递调用方；身份从不取自工具 arguments。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::config::{valid_id, validate_workspace_ids};
use crate::auth::{AuthContext, Grant, Principal};

pub const META_KEY: &str = "gld.node";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForwardedCaller {
    hub_id: String,
    subject: String,
}

/// 返回整个 _meta；调用者须替换外部 metadata，不能合并其中的身份。
pub fn forwarded_meta(auth: &AuthContext, hub_id: &str) -> Result<Value, String> {
    if !auth.is_authenticated() || auth.grant().is_some() {
        return Err("Native nodes require full service authentication".into());
    }
    let subject = auth.tag();
    if !valid_id(hub_id) || !valid_subject(&subject) {
        return Err("Invalid node caller identity".into());
    }
    Ok(json!({ META_KEY: ForwardedCaller { hub_id: hub_id.into(), subject } }))
}

/// 仅在已认证到预置 Hub 的节点 socket 上调用。
pub fn auth_from_meta(
    params: &Value,
    expected_hub_id: &str,
    shared_ids: &[String],
) -> Result<AuthContext, String> {
    validate_workspace_ids(shared_ids)?;
    let caller: ForwardedCaller = serde_json::from_value(
        params
            .get("_meta")
            .and_then(|v| v.get(META_KEY))
            .cloned()
            .ok_or("Trusted node caller metadata is required")?,
    )
    .map_err(|_| "Invalid trusted node caller metadata")?;
    if !valid_id(expected_hub_id)
        || caller.hub_id != expected_hub_id
        || !valid_subject(&caller.subject)
    {
        return Err("Invalid trusted node caller metadata".into());
    }
    // Tuple 编码避免分隔符碰撞；node 作用域与本机/MCP 调用方分开。
    let client_id = serde_json::to_string(&(expected_hub_id, caller.subject))
        .map_err(|_| "Cannot encode node caller identity")?;
    Ok(
        AuthContext::new(Principal::OAuthClient { client_id }, "node").with_grant(Some(Grant {
            id: format!("node-share-{expected_hub_id}"),
            name: "Explicit node project share".into(),
            workspaces: shared_ids.to_vec(),
            read_only: true,
            browser: false,
        })),
    )
}

fn valid_subject(subject: &str) -> bool {
    !subject.is_empty() && subject.len() <= 4096 && !subject.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Caller;
    const HUB: &str = "11111111111111111111111111111111";
    const OTHER: &str = "22222222222222222222222222222222";
    const PROJECT: &str = "33333333333333333333333333333333";

    fn owner(id: &str) -> AuthContext {
        AuthContext::new(
            Principal::OAuthClient {
                client_id: id.into(),
            },
            "hub",
        )
    }

    fn forwarded(id: &str, hub: &str) -> AuthContext {
        auth_from_meta(
            &json!({"_meta":forwarded_meta(&owner(id), hub).unwrap()}),
            hub,
            &[PROJECT.into()],
        )
        .unwrap()
    }

    #[test]
    fn forwarded_callers_are_stable_and_separate_hubs_and_clients() {
        let alice = Caller::from_auth(&forwarded("alice", HUB));
        assert_eq!(alice, Caller::from_auth(&forwarded("alice", HUB)));
        assert_ne!(alice, Caller::from_auth(&forwarded("bob", HUB)));
        assert_ne!(alice, Caller::from_auth(&forwarded("alice", OTHER)));
        assert_ne!(alice, Caller::from_auth(&owner("alice")));
        assert_ne!(alice, Caller::local());
        assert!(!alice.may_read_user_prompts());
        let grant = alice.grant().unwrap();
        assert!(grant.read_only && grant.allows(PROJECT));
        assert!(!grant.allows(OTHER));
    }

    #[test]
    fn anonymous_and_grant_subjects_cannot_be_forwarded() {
        assert!(forwarded_meta(&AuthContext::anonymous("hub"), HUB).is_err());
        assert!(forwarded_meta(&forwarded("alice", HUB), HUB).is_err());
    }

    #[test]
    fn metadata_is_required_bound_and_not_read_from_arguments() {
        let meta = forwarded_meta(&owner("alice"), HUB).unwrap();
        let ids = vec![PROJECT.into()];
        assert!(auth_from_meta(&json!({"arguments":{"_meta":meta}}), HUB, &ids).is_err());
        assert!(auth_from_meta(&json!({"_meta":meta}), OTHER, &ids).is_err());
        let mut extra = meta.clone();
        extra[META_KEY]["grant"] = json!({"read_only":false});
        assert!(auth_from_meta(&json!({"_meta":extra}), HUB, &ids).is_err());
    }
}

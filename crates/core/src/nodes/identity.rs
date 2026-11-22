//! 原生节点的调用方必须由完整的受限身份协议验证；旧 metadata 不构成授权。

use serde_json::Value;

use crate::auth::AuthContext;

pub const META_KEY: &str = "gld.node";
/// 经节点进来的调用方的入口名，和 [`crate::hub::HUB_SCOPE`] 分开。
pub const NODE_SCOPE: &str = "node";

/// D18-C 接入 grant、主体与有效权限的完整绑定前，不发送旧版裸主体 metadata。
pub fn forwarded_meta(_auth: &AuthContext, _hub_id: &str) -> Result<Value, String> {
    Err(
        "Native node authorization requires scoped identity forwarding, which is not supported yet"
            .into(),
    )
}

/// 配对 Hub 的消息签名只证明发送者，不能替代本次工具调用的主体与权限。
/// 在新协议落地前，空、非法和旧版 metadata 都必须拒绝。
pub fn auth_from_meta(
    _params: &Value,
    _expected_hub_id: &str,
    _shared_ids: &[String],
    _writable_ids: &[String],
) -> Result<AuthContext, String> {
    Err("Native node caller authorization is unavailable; legacy metadata is not accepted".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const HUB: &str = "11111111111111111111111111111111";
    const OTHER: &str = "22222222222222222222222222222222";
    const PROJECT: &str = "33333333333333333333333333333333";

    #[test]
    fn local_operator_cannot_be_serialized_as_a_network_caller() {
        assert!(forwarded_meta(&AuthContext::local_operator(), HUB).is_err());
    }

    #[test]
    fn missing_and_invalid_metadata_are_rejected() {
        for params in [
            Value::Null,
            json!({}),
            json!({"_meta":null}),
            json!({"_meta":{META_KEY:"invalid"}}),
        ] {
            assert!(auth_from_meta(&params, HUB, &[PROJECT.into()], &[]).is_err());
        }
    }

    #[test]
    fn legacy_subject_metadata_cannot_create_a_project_grant() {
        let meta = json!({META_KEY:{"hub_id":HUB,"subject":"hub:secret"}});
        for params in [
            json!({"_meta":meta}),
            json!({"arguments":{"workspace":PROJECT,"_meta":meta}}),
            json!({"_meta":meta,"arguments":{"workspace":PROJECT,"writable_workspaces":[PROJECT]}}),
        ] {
            assert!(auth_from_meta(&params, HUB, &[PROJECT.into()], &[PROJECT.into()]).is_err());
        }
    }

    #[test]
    fn wrong_hub_and_invalid_shares_do_not_bypass_refusal() {
        let params = json!({"_meta":{META_KEY:{"hub_id":HUB,"subject":"hub:secret"}}});
        assert!(auth_from_meta(&params, OTHER, &[PROJECT.into()], &[]).is_err());
        assert!(auth_from_meta(&params, HUB, &[], &[PROJECT.into()]).is_err());
        assert!(auth_from_meta(&params, HUB, &[PROJECT.into()], &[OTHER.into()]).is_err());
    }
}

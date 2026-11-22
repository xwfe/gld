use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::bridge::member::CcnmMember;
use crate::nodes::config::MachineRecord;
use crate::nodes::pairing::InvitationRecord;
use crate::settings::{FrpProfile, GlobalGatewayConfig, HubConfig, ProxyConfig};
use crate::workspace::WorkspaceProfile;

/// Unified on-disk payload stored in `data/profiles.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppData {
    #[serde(default)]
    pub frp_profiles: Vec<FrpProfile>,
    #[serde(default)]
    pub last_workspace_id: String,
    #[serde(default)]
    pub proxy: ProxyConfig,
    #[serde(default)]
    pub global_executable_paths: String,
    #[serde(default)]
    pub global_ai_instructions: String,
    #[serde(default)]
    pub global_instruction_sources: Vec<String>,
    #[serde(default)]
    pub global_skill_sources: Vec<String>,
    #[serde(default)]
    pub global_custom_instruction_paths: String,
    #[serde(default)]
    pub global_custom_skill_paths: String,
    #[serde(default)]
    pub global_hidden_skills: Vec<String>,
    #[serde(default)]
    pub relayed_mcp_servers: Vec<String>,
    #[serde(default)]
    pub allow_lan_access: bool,
    #[serde(default)]
    pub restore_runtime_state_on_launch: bool,
    #[serde(default)]
    pub restore_mcp_workspace_ids: Vec<String>,
    #[serde(default)]
    pub restore_actions_workspace_ids: Vec<String>,
    #[serde(default)]
    pub global_gateway: GlobalGatewayConfig,
    #[serde(default)]
    pub hub: HubConfig,
    #[serde(default)]
    pub shared_secrets: HashMap<String, String>,
    #[serde(default)]
    pub workspace_secrets: HashMap<String, HashMap<String, String>>,
    #[serde(default)]
    pub app_secrets: HashMap<String, HashMap<String, String>>,
    #[serde(default)]
    pub profiles: Vec<WorkspaceProfile>,
    /// 远端 ccnm workspace 成员。跟 `profiles` 是两份名单：本地成员有 root
    /// 路径、隧道和 Planning，远端成员一样都没有（RFC-0002 5.1）。
    /// 旧数据文件里没有这个键，`default` 让它读出来是空的。
    #[serde(default)]
    pub ccnm_members: Vec<CcnmMember>,
    /// 原生节点的稳定 Hub 身份和显式登记；旧配置默认不开节点。
    #[serde(default)]
    pub node_hub_id: String,
    /// Hub 向节点证明身份的 Ed25519 私钥（PKCS#8 十六进制），与 hub_id 一起生成。
    /// 节点配对时钉住它的公钥；换掉或丢失，所有节点都要重新配对。
    #[serde(default)]
    pub node_hub_key: String,
    #[serde(default)]
    pub node_machines: Vec<MachineRecord>,
    /// 未消费的一次性节点邀请；仅保存摘要，过期记录在下次创建邀请时清理。
    #[serde(default)]
    pub node_invitations: Vec<InvitationRecord>,
    /// 旧授权原件只随普通配置保存保留，不参与新授权查询或验证。
    #[serde(default, skip_serializing_if = "OpaqueLegacyGrants::is_absent")]
    pub grants: OpaqueLegacyGrants,
}

/// Legacy `{ "profiles": [...] }` file at the app root.
#[derive(Debug, Deserialize)]
pub struct LegacyProfilesOnlyFile {
    pub profiles: Vec<WorkspaceProfile>,
}

/// 旧授权只作为未解释的数据保留；仅判断是否需要提示重建，不作为授权来源。
#[derive(Clone, Default)]
pub struct OpaqueLegacyGrants(Option<serde_json::Value>);

impl OpaqueLegacyGrants {
    fn is_absent(&self) -> bool {
        self.0.is_none()
    }

    pub(crate) fn requires_rebuild(&self) -> bool {
        match self.0.as_ref() {
            None | Some(serde_json::Value::Null) => false,
            Some(serde_json::Value::Array(values)) => !values.is_empty(),
            Some(serde_json::Value::Object(values)) => !values.is_empty(),
            Some(serde_json::Value::String(value)) => !value.is_empty(),
            Some(_) => true,
        }
    }
}

impl std::fmt::Debug for OpaqueLegacyGrants {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OpaqueLegacyGrants(<redacted>)")
    }
}

impl Serialize for OpaqueLegacyGrants {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for OpaqueLegacyGrants {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // present null 也保留；只有整个字段缺席才使用 Default。
        serde_json::Value::deserialize(deserializer).map(|value| Self(Some(value)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rebuild_detection_does_not_interpret_legacy_fields() {
        assert!(!OpaqueLegacyGrants::default().requires_rebuild());
        for (value, expected) in [
            (json!(null), false),
            (json!([]), false),
            (json!({}), false),
            (json!(""), false),
            (json!([null]), true),
            (json!({"unknown": null}), true),
            (json!(" "), true),
            (json!(0), true),
            (json!(false), true),
        ] {
            let opaque: OpaqueLegacyGrants = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(opaque.requires_rebuild(), expected);
            assert_eq!(serde_json::to_value(opaque).unwrap(), value);
        }
    }

    #[test]
    fn legacy_authorization_is_opaque_and_absent_data_stays_absent() {
        assert!(serde_json::to_value(AppData::default())
            .unwrap()
            .get("grants")
            .is_none());
        for original in [
            serde_json::Value::Null,
            json!([]),
            json!({"unrecognized": {"bearer_token": "preserve-secret"}}),
            json!([{"id": "old", "oauth_password": "preserve-secret"}]),
        ] {
            let data: AppData = serde_json::from_value(json!({"grants": original})).unwrap();
            assert_eq!(serde_json::to_value(&data).unwrap()["grants"], original);
            assert!(!format!("{data:?}").contains("preserve-secret"));
        }
    }
}

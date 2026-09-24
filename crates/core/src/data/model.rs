use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::auth::GrantRecord;
use crate::bridge::member::CcnmMember;
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
    /// 只开部分项目的凭据（RFC-0007），连同它们自己的钥匙。
    #[serde(default)]
    pub grants: Vec<GrantRecord>,
}

/// Legacy `{ "profiles": [...] }` file at the app root.
#[derive(Debug, Deserialize)]
pub struct LegacyProfilesOnlyFile {
    pub profiles: Vec<WorkspaceProfile>,
}

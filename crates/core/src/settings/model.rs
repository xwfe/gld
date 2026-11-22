use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{FrpProfile, GlobalGatewayConfig, HubConfig, ProxyConfig};
use crate::data::AppData;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default)]
    pub frp_profiles: Vec<FrpProfile>,
    #[serde(default)]
    pub last_workspace_id: String,
    /// Global outbound proxy (Cloudflare tunnel, etc.).
    #[serde(default)]
    pub proxy: ProxyConfig,
    /// Global executable search paths inherited by every workspace runtime.
    #[serde(default)]
    pub global_executable_paths: String,
    /// Global agent instructions prepended to workspace-specific instructions.
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
    /// Skills installed on this machine that are never offered, by name.
    #[serde(default)]
    pub global_hidden_skills: Vec<String>,
    /// 这台机器上装好的 MCP server 里，经服务转给 AI 的那几个（按名字，
    /// `gld mcp on`）。默认空：一个都不转，见 `machine_mcp`。
    #[serde(default)]
    pub relayed_mcp_servers: Vec<String>,
    /// Allow MCP, Actions and Global Gateway listeners to bind to all LAN interfaces.
    /// Defaults to false so services remain loopback-only unless explicitly enabled.
    #[serde(default)]
    pub allow_lan_access: bool,
    /// Restore the MCP / Actions services that were running in the previous app session.
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
    /// Shared secrets indexed by key name (e.g. "bearer_token").
    /// Persisted alongside other app settings in app_settings.json.
    #[serde(default)]
    pub shared_secrets: HashMap<String, String>,
    /// Per-workspace secrets: workspace_id -> secret_key -> value.
    #[serde(default)]
    pub workspace_secrets: HashMap<String, HashMap<String, String>>,
    /// App-scoped secrets: scope -> item_id -> value (e.g. frp profile tokens).
    #[serde(default)]
    pub app_secrets: HashMap<String, HashMap<String, String>>,
}

impl AppSettings {
    pub fn from_data(data: &AppData) -> Self {
        Self {
            frp_profiles: data.frp_profiles.clone(),
            last_workspace_id: data.last_workspace_id.clone(),
            proxy: data.proxy.clone(),
            global_executable_paths: data.global_executable_paths.clone(),
            global_ai_instructions: data.global_ai_instructions.clone(),
            global_instruction_sources: data.global_instruction_sources.clone(),
            global_skill_sources: data.global_skill_sources.clone(),
            global_custom_instruction_paths: data.global_custom_instruction_paths.clone(),
            global_custom_skill_paths: data.global_custom_skill_paths.clone(),
            global_hidden_skills: data.global_hidden_skills.clone(),
            relayed_mcp_servers: data.relayed_mcp_servers.clone(),
            allow_lan_access: data.allow_lan_access,
            restore_runtime_state_on_launch: data.restore_runtime_state_on_launch,
            restore_mcp_workspace_ids: data.restore_mcp_workspace_ids.clone(),
            restore_actions_workspace_ids: data.restore_actions_workspace_ids.clone(),
            global_gateway: data.global_gateway.clone(),
            hub: data.hub.clone(),
            shared_secrets: data.shared_secrets.clone(),
            workspace_secrets: data.workspace_secrets.clone(),
            app_secrets: data.app_secrets.clone(),
        }
    }

    pub fn apply_to(&self, data: &mut AppData) {
        data.frp_profiles = self.frp_profiles.clone();
        data.last_workspace_id = self.last_workspace_id.clone();
        data.proxy = self.proxy.clone();
        data.global_executable_paths = self.global_executable_paths.clone();
        data.global_ai_instructions = self.global_ai_instructions.clone();
        data.global_instruction_sources = self.global_instruction_sources.clone();
        data.global_skill_sources = self.global_skill_sources.clone();
        data.global_custom_instruction_paths = self.global_custom_instruction_paths.clone();
        data.global_custom_skill_paths = self.global_custom_skill_paths.clone();
        data.global_hidden_skills = self.global_hidden_skills.clone();
        data.relayed_mcp_servers = self.relayed_mcp_servers.clone();
        data.allow_lan_access = self.allow_lan_access;
        data.restore_runtime_state_on_launch = self.restore_runtime_state_on_launch;
        data.restore_mcp_workspace_ids = self.restore_mcp_workspace_ids.clone();
        data.restore_actions_workspace_ids = self.restore_actions_workspace_ids.clone();
        data.global_gateway = self.global_gateway.clone();
        data.hub = self.hub.clone();
        data.shared_secrets = self.shared_secrets.clone();
        data.workspace_secrets = self.workspace_secrets.clone();
        data.app_secrets = self.app_secrets.clone();
    }

    pub fn load_or_default() -> Self {
        crate::data::DataStore::read_file(|data| Ok(Self::from_data(data))).unwrap_or_default()
    }

    pub fn find_frp_profile(&self, id: &str) -> Option<&FrpProfile> {
        if id.trim().is_empty() {
            return None;
        }
        self.frp_profiles.iter().find(|profile| profile.id == id)
    }
}

/// Create an FRP profile with a locally generated identifier.
pub fn new_frp_profile(name: String, server: String, server_port: u16) -> FrpProfile {
    FrpProfile {
        id: uuid::Uuid::new_v4().to_string().replace('-', ""),
        name,
        server: server.trim().to_string(),
        server_port,
    }
}

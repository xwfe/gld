use serde::{Deserialize, Serialize};

use crate::settings::AppSettings;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceProfile {
    pub id: String,
    pub name: String,
    pub path: String,
    pub tunnel: TunnelConfig,
    pub auth: AuthConfig,
    pub runtime: RuntimeConfig,
    #[serde(default)]
    pub actions: ActionsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelConfig {
    #[serde(rename = "type", default = "default_tunnel_type")]
    pub tunnel_type: String,
    #[serde(default)]
    pub public_url: String,
    #[serde(default)]
    pub frp_server: String,
    #[serde(default)]
    pub frp_subdomain: String,
    #[serde(default)]
    pub frp_profile_id: String,
    #[serde(default = "default_frp_server_port")]
    pub frp_server_port: u16,
    #[serde(default = "default_cloudflare_mode")]
    pub cloudflare_mode: String,
    /// When true, apply global proxy from Settings → General when starting the tunnel.
    #[serde(default = "default_use_proxy")]
    pub use_proxy: bool,
    /// Route this service through the shared global gateway instead of a dedicated tunnel.
    #[serde(default)]
    pub use_global_gateway: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    #[serde(rename = "type", default = "default_auth_type")]
    pub auth_type: String,
    #[serde(default = "default_oauth_client_id")]
    pub oauth_client_id: String,
    #[serde(default)]
    pub use_shared_secrets: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    #[serde(default = "default_mcp_port")]
    pub local_port: u16,
    #[serde(default = "default_tool_profile")]
    pub tool_profile: String,
    /// Persist the current MCP conversation through explicit checkpoint calls.
    #[serde(default = "default_history_recording")]
    pub history_recording: bool,
    /// Numeric history archives selected by the user for context injection.
    #[serde(default)]
    pub history_context_sessions: Vec<u64>,
    #[serde(default = "default_permission_mode")]
    pub permission_mode: String,
    #[serde(default)]
    pub runtime_command: String,
    /// Workspace execution policy shared by MCP clients.
    #[serde(default = "default_allowed_commands")]
    pub allowed_commands: String,
    /// Additional executable search paths for workspace runtime commands.
    #[serde(default)]
    pub executable_paths: String,
    /// Workspace-level instructions injected into agent context.
    #[serde(default)]
    pub ai_instructions: String,
    /// IDE / coding-agent instruction providers enabled for this workspace.
    #[serde(default)]
    pub instruction_sources: Vec<String>,
    /// IDE / coding-agent skill providers enabled for this workspace.
    #[serde(default)]
    pub skill_sources: Vec<String>,
    #[serde(default)]
    pub custom_instruction_paths: String,
    #[serde(default)]
    pub custom_skill_paths: String,
    #[serde(default = "default_workspace_local_entries")]
    pub workspace_local_entries: bool,
    #[serde(default = "default_workspace_script_extensions")]
    pub workspace_script_extensions: String,
    /// 读工具是否只许读 Workspace 内。默认 true。
    ///
    /// 老的配置文件里没有这个字段，serde default 会给 true——也就是升级后
    /// 越界读会被挡。这是有意的：这个服务可以挂到公网给 ChatGPT 用，
    /// 默认能读整台机器风险太大。需要的人显式打开。
    #[serde(default = "default_confine_reads")]
    pub confine_reads: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionsConfig {
    #[serde(default)]
    pub public_url: String,
    #[serde(default = "default_tunnel_type")]
    pub tunnel_type: String,
    #[serde(default)]
    pub frp_server: String,
    #[serde(default)]
    pub frp_subdomain: String,
    #[serde(default)]
    pub frp_profile_id: String,
    #[serde(default = "default_frp_server_port")]
    pub frp_server_port: u16,
    #[serde(default = "default_cloudflare_mode")]
    pub cloudflare_mode: String,
    #[serde(default)]
    pub cloudflare_token: String,
    #[serde(default = "default_use_proxy")]
    pub use_proxy: bool,
    /// Route this service through the shared global gateway instead of a dedicated tunnel.
    #[serde(default)]
    pub use_global_gateway: bool,
    #[serde(default = "default_actions_port")]
    pub local_port: u16,
    #[serde(default = "default_permission_mode")]
    pub permission_mode: String,
    #[serde(default)]
    pub runtime_command: String,
    #[serde(default = "default_actions_auth_type")]
    pub auth_type: String,
    #[serde(default = "default_actions_oauth_client_id")]
    pub oauth_client_id: String,
    #[serde(default)]
    pub oauth_scopes: String,
    #[serde(default = "default_allowed_commands")]
    pub allowed_commands: String,
    #[serde(default = "default_max_patch_bytes")]
    pub max_patch_bytes: u32,
    #[serde(default)]
    pub use_shared_secrets: bool,
    /// 同 [`RuntimeConfig::confine_reads`]，Actions 侧独立配置。
    #[serde(default = "default_confine_reads")]
    pub confine_reads: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStatusDto {
    pub state: String,
    pub pid: Option<u32>,
    pub local_message: String,
    pub public_message: String,
    pub local_endpoint: String,
    pub public_endpoint: String,
}

fn default_tunnel_type() -> String {
    "frp".to_string()
}

fn default_cloudflare_mode() -> String {
    "quick".to_string()
}

fn default_use_proxy() -> bool {
    true
}

fn default_auth_type() -> String {
    "oauth".to_string()
}

fn default_frp_server_port() -> u16 {
    7000
}

fn default_actions_auth_type() -> String {
    "api_key".to_string()
}

fn default_actions_oauth_client_id() -> String {
    format!(
        "chatgpt-actions-{}",
        &uuid::Uuid::new_v4().to_string()[..12]
    )
}

fn default_oauth_client_id() -> String {
    format!("chatgpt-client-{}", &uuid::Uuid::new_v4().to_string()[..12])
}

fn default_mcp_port() -> u16 {
    28766
}

fn default_actions_port() -> u16 {
    8787
}

fn default_tool_profile() -> String {
    "compact".to_string()
}

fn default_history_recording() -> bool {
    true
}

fn default_permission_mode() -> String {
    "trusted".to_string()
}

fn default_allowed_commands() -> String {
    "pytest,python,python3,npm,npx,node,pnpm,yarn,make,mvn,mvnw,gradle,gradlew,cargo,go,ruff,mypy,eslint,tsc,git,cmd,powershell,pwsh".to_string()
}

fn default_workspace_local_entries() -> bool {
    true
}

fn default_confine_reads() -> bool {
    true
}

fn default_workspace_script_extensions() -> String {
    ".exe,.bat,.cmd,.ps1".to_string()
}

fn default_max_patch_bytes() -> u32 {
    200_000
}

impl Default for TunnelConfig {
    fn default() -> Self {
        Self {
            tunnel_type: default_tunnel_type(),
            public_url: String::new(),
            frp_server: String::new(),
            frp_subdomain: String::new(),
            frp_profile_id: String::new(),
            frp_server_port: default_frp_server_port(),
            cloudflare_mode: default_cloudflare_mode(),
            use_proxy: default_use_proxy(),
            use_global_gateway: false,
        }
    }
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            auth_type: default_auth_type(),
            oauth_client_id: default_oauth_client_id(),
            use_shared_secrets: false,
        }
    }
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            local_port: default_mcp_port(),
            tool_profile: default_tool_profile(),
            history_recording: default_history_recording(),
            history_context_sessions: Vec::new(),
            permission_mode: default_permission_mode(),
            runtime_command: String::new(),
            allowed_commands: default_allowed_commands(),
            executable_paths: String::new(),
            ai_instructions: String::new(),
            instruction_sources: Vec::new(),
            skill_sources: Vec::new(),
            custom_instruction_paths: String::new(),
            custom_skill_paths: String::new(),
            workspace_local_entries: default_workspace_local_entries(),
            workspace_script_extensions: default_workspace_script_extensions(),
            confine_reads: default_confine_reads(),
        }
    }
}

impl Default for ActionsConfig {
    fn default() -> Self {
        Self {
            public_url: String::new(),
            tunnel_type: default_tunnel_type(),
            frp_server: String::new(),
            frp_subdomain: String::new(),
            frp_profile_id: String::new(),
            frp_server_port: default_frp_server_port(),
            cloudflare_mode: default_cloudflare_mode(),
            cloudflare_token: String::new(),
            use_proxy: default_use_proxy(),
            use_global_gateway: false,
            local_port: default_actions_port(),
            permission_mode: default_permission_mode(),
            runtime_command: String::new(),
            auth_type: default_actions_auth_type(),
            oauth_client_id: default_actions_oauth_client_id(),
            oauth_scopes: String::new(),
            allowed_commands: default_allowed_commands(),
            max_patch_bytes: default_max_patch_bytes(),
            use_shared_secrets: false,
            confine_reads: default_confine_reads(),
        }
    }
}

#[allow(dead_code)]
impl WorkspaceProfile {
    pub fn new(path: String, name: Option<String>) -> Self {
        let cleaned = path.trim_end_matches(['\\', '/']).to_string();
        let label = name.unwrap_or_else(|| {
            cleaned
                .replace('\\', "/")
                .split('/')
                .next_back()
                .unwrap_or("工作区")
                .to_string()
        });
        Self {
            id: uuid::Uuid::new_v4().to_string().replace('-', ""),
            name: label,
            path: cleaned,
            tunnel: TunnelConfig::default(),
            auth: AuthConfig::default(),
            runtime: RuntimeConfig::default(),
            actions: ActionsConfig::default(),
        }
    }

    pub fn local_endpoint(&self) -> String {
        format!("http://127.0.0.1:{}/mcp", self.runtime.local_port)
    }

    pub fn effective_public_url(&self) -> String {
        self.effective_public_url_with(&AppSettings::load_or_default())
    }

    pub fn effective_public_url_with(&self, settings: &AppSettings) -> String {
        if self.tunnel.use_global_gateway && settings.global_gateway.enabled {
            return gateway_workspace_base(&settings.global_gateway.public_url, &self.id, false);
        }
        computed_public_url(
            &self.tunnel.tunnel_type,
            &self.tunnel.frp_server,
            &self.tunnel.frp_subdomain,
            &self.tunnel.public_url,
            &self.tunnel.frp_profile_id,
            settings,
        )
    }

    pub fn public_endpoint(&self) -> String {
        let base = self.effective_public_url();
        if base.is_empty() {
            return String::new();
        }
        format!("{}/mcp", base.trim_end_matches('/'))
    }

    pub fn actions_local_base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.actions.local_port)
    }

    pub fn actions_effective_public_url(&self) -> String {
        self.actions_effective_public_url_with(&AppSettings::load_or_default())
    }

    pub fn actions_effective_public_url_with(&self, settings: &AppSettings) -> String {
        if self.actions.use_global_gateway && settings.global_gateway.enabled {
            return gateway_workspace_base(&settings.global_gateway.public_url, &self.id, true);
        }
        computed_public_url(
            &self.actions.tunnel_type,
            &self.actions.frp_server,
            &self.actions.frp_subdomain,
            &self.actions.public_url,
            &self.actions.frp_profile_id,
            settings,
        )
    }

    pub fn actions_openapi_url(&self) -> String {
        let base = self.actions_public_base_url();
        if base.is_empty() {
            return String::new();
        }
        format!("{}/openapi.json", base.trim_end_matches('/'))
    }

    pub fn actions_privacy_url(&self) -> String {
        let base = self.actions_public_base_url();
        if base.is_empty() {
            return String::new();
        }
        format!("{}/privacy", base.trim_end_matches('/'))
    }

    pub fn actions_oauth_authorization_url(&self) -> String {
        let base = self.actions_public_base_url();
        if base.is_empty() {
            return String::new();
        }
        format!("{}/oauth/authorize", base.trim_end_matches('/'))
    }

    pub fn actions_oauth_token_url(&self) -> String {
        let base = self.actions_public_base_url();
        if base.is_empty() {
            return String::new();
        }
        format!("{}/oauth/token", base.trim_end_matches('/'))
    }

    /// Public URL for GPT schema import; falls back to localhost when no tunnel is configured.
    pub fn actions_public_base_url(&self) -> String {
        let public = self.actions_effective_public_url();
        if public.is_empty() {
            self.actions_local_base_url()
        } else {
            public
        }
    }
}

fn gateway_workspace_base(public_url: &str, workspace_id: &str, actions: bool) -> String {
    let base = public_url.trim_end_matches('/');
    if base.is_empty() {
        return String::new();
    }
    if actions {
        format!("{base}/w/{workspace_id}/actions")
    } else {
        format!("{base}/w/{workspace_id}")
    }
}

fn computed_public_url(
    tunnel_type: &str,
    frp_server: &str,
    frp_subdomain: &str,
    public_url: &str,
    frp_profile_id: &str,
    settings: &AppSettings,
) -> String {
    if tunnel_type == "frp" {
        let server = settings
            .find_frp_profile(frp_profile_id)
            .map(|profile| profile.server.as_str())
            .unwrap_or(frp_server);
        if !server.is_empty() && !frp_subdomain.is_empty() {
            return format!("https://{frp_subdomain}.{server}");
        }
    }
    public_url.trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 0.2.x 写出来的配置文件里没有 `confine_reads` 字段，
    /// 反序列化必须补成 `true`——升级后越界读要被挡住，而不是维持旧的放开状态。
    ///
    /// serde 的 `#[serde(default = ...)]` 很容易在加字段时写成 `#[serde(default)]`
    /// （bool 的 Default 是 false），那样升级就悄悄留了个洞，还没人会发现。
    #[test]
    fn an_old_config_without_confine_reads_defaults_to_confined() {
        let runtime: RuntimeConfig = serde_json::from_str("{}").expect("旧配置也该能读");
        assert!(runtime.confine_reads, "MCP 侧升级后应当默认收紧");

        let actions: ActionsConfig = serde_json::from_str("{}").expect("旧配置也该能读");
        assert!(actions.confine_reads, "Actions 侧升级后应当默认收紧");
    }

    /// 显式写了 false 就要认，别被 default 覆盖掉。
    #[test]
    fn an_explicit_false_is_kept() {
        let runtime: RuntimeConfig =
            serde_json::from_str(r#"{"confine_reads": false}"#).expect("解析");
        assert!(!runtime.confine_reads);
    }
}

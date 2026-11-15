//! 按已装配的执行位置创建 HTTP 监听器。

use std::path::PathBuf;
use std::sync::Arc;

use gld_core::generation::ServiceGeneration;
use gld_core::hub::{Hub, HubSecrets};
use gld_core::nodes::registry::Nodes;
use gld_core::runtime::{BoundListener, ListenerBinder, ListenerManager, ServiceKind};
use gld_core::secret::SecretStore;
use gld_core::settings::HubConfig;
use gld_core::tools::policy::PolicySettings;
use gld_core::usage::ServiceUsage;
use gld_core::workspace::WorkspaceProfile;
use gld_core::AppResult;

use crate::generation::Router;
use crate::{actions, mcp};

/// 本机直连与 daemon 复用相同的 HTTP 适配；后一条路径的容器里只有一个 Router。
pub fn listener_manager(router: Option<Arc<Router>>) -> ListenerManager {
    let binder = Arc::new(HttpListeners {
        router: router.clone(),
    });
    let generation = router.map(|router| -> Arc<dyn ServiceGeneration> { router });
    ListenerManager::new(binder, generation)
}

struct HttpListeners {
    router: Option<Arc<Router>>,
}

impl ListenerBinder for HttpListeners {
    fn bind_workspace(
        &self,
        profile: &WorkspaceProfile,
        kind: ServiceKind,
        port: u16,
        usage: Arc<ServiceUsage>,
    ) -> AppResult<Result<BoundListener, String>> {
        Ok(match kind {
            ServiceKind::Mcp => {
                let use_shared = profile.auth.use_shared_secrets;
                let mut auth = profile.auth.clone();
                if use_shared {
                    if let Some(client_id) = SecretStore::get_shared("oauth_client_id")? {
                        auth.oauth_client_id = client_id;
                    }
                }
                // MCP OAuth matches legacy Python: client_secret is optional.
                // ChatGPT connectors use PKCE only and do not send client_secret.
                let oauth_client_secret = None;
                let oauth_password = if profile.auth.oauth_enabled() {
                    resolve_secret(&profile.id, "oauth_password", use_shared)?
                } else {
                    None
                };
                let oauth_token_secret = if profile.auth.oauth_enabled() {
                    resolve_secret(&profile.id, "oauth_token_secret", use_shared)?
                } else {
                    None
                };
                mcp::spawn_listener(
                    port,
                    PathBuf::from(&profile.path),
                    profile.id.clone(),
                    profile.name.clone(),
                    auth,
                    profile.effective_public_url(),
                    oauth_client_secret,
                    oauth_password,
                    oauth_token_secret,
                    profile.runtime.clone(),
                    usage.clone(),
                    self.router.clone(),
                )
            }
            ServiceKind::Actions => {
                let auth_type = profile.actions.auth_type.clone();
                let use_shared = profile.actions.use_shared_secrets;
                let api_key = if auth_type == "api_key" {
                    resolve_secret(&profile.id, "actions_api_key", use_shared)?
                } else {
                    None
                };
                let oauth_client_secret = if auth_type == "oauth" {
                    if use_shared {
                        resolve_secret(&profile.id, "actions_oauth_client_secret", true)?
                    } else {
                        Some(actions_oauth_secret(
                            &profile.id,
                            "actions_oauth_client_secret",
                        )?)
                    }
                } else {
                    None
                };
                let oauth_password = if auth_type == "oauth" {
                    if use_shared {
                        resolve_secret(&profile.id, "actions_oauth_password", true)?
                    } else {
                        Some(actions_oauth_secret(&profile.id, "actions_oauth_password")?)
                    }
                } else {
                    None
                };
                let oauth_token_secret = if auth_type == "oauth" {
                    if use_shared {
                        resolve_secret(&profile.id, "actions_oauth_token_secret", true)?
                    } else {
                        Some(actions_oauth_secret(
                            &profile.id,
                            "actions_oauth_token_secret",
                        )?)
                    }
                } else {
                    None
                };
                let public_base_url = profile.actions_public_base_url();
                let policy = PolicySettings::from_actions_config(&profile.actions);
                actions::spawn_listener(
                    &profile.id,
                    port,
                    PathBuf::from(&profile.path),
                    public_base_url,
                    auth_type,
                    api_key,
                    profile.actions.oauth_client_id.clone(),
                    oauth_client_secret,
                    oauth_password,
                    oauth_token_secret,
                    policy,
                    usage.clone(),
                    self.router.clone(),
                )
            }
        })
    }

    fn bind_hub(
        &self,
        config: &HubConfig,
        hub: Arc<Hub>,
        nodes: Arc<Nodes>,
        public_base: String,
        secrets: HubSecrets,
    ) -> Result<BoundListener, String> {
        mcp::spawn_hub_listener(
            config.local_port,
            hub,
            nodes,
            &config.auth_type,
            public_base,
            secrets,
            self.router.clone(),
        )
    }
}

/// Resolve a secret from the shared pool or per-workspace keyring.
fn resolve_secret(profile_id: &str, key: &str, use_shared: bool) -> AppResult<Option<String>> {
    if use_shared {
        SecretStore::get_shared(key)
    } else {
        SecretStore::get(profile_id, key)
    }
}

fn actions_oauth_secret(profile_id: &str, key: &str) -> AppResult<String> {
    match SecretStore::get(profile_id, key)? {
        Some(value) if !value.is_empty() => Ok(value),
        _ => SecretStore::regenerate(profile_id, key),
    }
}

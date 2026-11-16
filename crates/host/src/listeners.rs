//! 按已装配的执行位置创建 HTTP 监听器。

use std::path::PathBuf;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use gld_core::app::App;
use gld_core::async_rt::JoinHandle;
use gld_core::hub::runtime::{HubState, TunnelOutcome, TunnelSnapshot};
use gld_core::hub::HubSecrets;
use gld_core::runtime::{ListenerHost, ListenerManager, ServiceKind};
use gld_core::secret::SecretStore;
use gld_core::settings::HubConfig;
use gld_core::tools::policy::PolicySettings;
use gld_core::usage::ServiceUsage;
use gld_core::workspace::WorkspaceProfile;
use gld_core::AppResult;

use crate::generation::Router;
use crate::listener_supervisor::RuntimeSupervisor;
use crate::{actions, hub_runtime, mcp};

pub(crate) type ShutdownSender = tokio::sync::oneshot::Sender<()>;
pub(crate) type BoundListener = (ShutdownSender, JoinHandle<()>);

/// 本机直连与 daemon 复用相同的 HTTP 适配；后一条路径的容器里只有一个 Router。
pub fn listener_manager(app: Arc<App>, router: Option<Arc<Router>>) -> ListenerManager {
    let runtime = Box::new(RuntimeSupervisor::new(app.clone(), router.clone()));
    ListenerManager::new(Arc::new(HttpListeners { app, router }), runtime)
}

struct HttpListeners {
    app: Arc<App>,
    router: Option<Arc<Router>>,
}

impl ListenerHost for HttpListeners {
    fn ensure_gateway(&self) -> BoxFuture<'_, AppResult<()>> {
        Box::pin(async { crate::global_gateway::ensure_started().await.map(|_| ()) })
    }

    fn start_hub<'a>(
        &'a self,
        config: &'a HubConfig,
        outcome: TunnelOutcome,
        secrets: HubSecrets,
    ) -> BoxFuture<'a, AppResult<()>> {
        Box::pin(async move {
            let reader = self.app.listener_access_reader()?;
            hub_runtime::start(config, outcome, secrets, self.router.clone(), reader).await
        })
    }

    fn stop_hub(&self) -> BoxFuture<'_, ()> {
        Box::pin(hub_runtime::stop())
    }
    fn attach_hub_tunnel(&self, outcome: TunnelOutcome) -> BoxFuture<'_, ()> {
        Box::pin(hub_runtime::attach_tunnel(outcome))
    }
    fn hub_state(&self) -> BoxFuture<'_, HubState> {
        Box::pin(hub_runtime::state())
    }
    fn hub_public_base(&self) -> BoxFuture<'_, Option<(String, String)>> {
        Box::pin(hub_runtime::public_base())
    }
    fn hub_tunnel_snapshot(&self) -> BoxFuture<'_, Option<TunnelSnapshot>> {
        Box::pin(hub_runtime::tunnel_snapshot())
    }
}

pub(crate) fn bind_workspace(
    profile: &WorkspaceProfile,
    kind: ServiceKind,
    port: u16,
    usage: Arc<ServiceUsage>,
    router: Option<Arc<Router>>,
    app: &App,
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
                router,
                app.listener_access_reader()?,
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
                router,
            )
        }
    })
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

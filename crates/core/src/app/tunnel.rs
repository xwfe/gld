use serde::{Deserialize, Serialize};

use super::App;
use crate::error::{AppError, AppResult};
use crate::platform::platform;
use crate::tunnel::{frp_snippet, supervisor, TunnelServiceKind, TunnelStatus};
use crate::workspace::resources::validate_service_start;
use crate::workspace::WorkspaceProfile;

/// `tunnel test` 的结果：是否连通、拿到的公网地址、隧道是否保留在运行状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelTestResult {
    pub success: bool,
    pub public_url: String,
    pub kept_running: bool,
    pub message: String,
}

impl App {
    /// 生成手动运行 frpc 时可直接使用的配置片段。
    pub fn frp_snippet(
        &self,
        id: &str,
        kind: TunnelServiceKind,
        reveal: bool,
    ) -> AppResult<String> {
        let profile = self.profile_by_id(id)?;
        Ok(frp_snippet(&profile, kind, reveal))
    }

    pub async fn tunnel_status(
        &self,
        id: &str,
        kind: TunnelServiceKind,
    ) -> AppResult<TunnelStatus> {
        let profile = self.profile_by_id(id)?;
        let settings = self.settings()?;
        let guard = supervisor().lock().await;
        Ok(guard.status(&profile, kind, &settings))
    }

    pub async fn start_tunnel(&self, id: &str, kind: TunnelServiceKind) -> AppResult<TunnelStatus> {
        let profile = self.profile_by_id(id)?;
        self.validate_tunnel_start(id, kind)?;
        self.sync_tunnel_routes_from_runtime().await?;
        let settings = self.settings()?;
        let status = {
            let mut guard = supervisor().lock().await;
            guard.start(&profile, kind, &settings).await?
        };
        self.persist_tunnel_url(id, kind, &status.public_url)?;
        Ok(status)
    }

    pub async fn stop_tunnel(&self, id: &str, kind: TunnelServiceKind) -> AppResult<TunnelStatus> {
        let profile = self.profile_by_id(id)?;
        let settings = self.settings()?;
        let mut guard = supervisor().lock().await;
        guard.stop(&profile, kind, &settings).await?;
        Ok(guard.status(&profile, kind, &settings))
    }

    /// 重启隧道。FRP 走 supervisor 的原子替换：新子域名成功后才释放旧线路，失败则回滚配置。
    pub async fn restart_tunnel(
        &self,
        id: &str,
        kind: TunnelServiceKind,
    ) -> AppResult<TunnelStatus> {
        let profile = self.profile_by_id(id)?;
        self.validate_tunnel_start(id, kind)?;
        self.sync_tunnel_routes_from_runtime().await?;
        let settings = self.settings()?;

        let result = {
            let mut guard = supervisor().lock().await;
            let was_running = guard.status(&profile, kind, &settings).state == "running";
            if was_running && tunnel_type_for(&profile, kind) == "frp" {
                guard
                    .start(&profile, kind, &settings)
                    .await
                    .map_err(|error| (error, guard.route_profile(id, kind)))
            } else if was_running {
                match guard.stop(&profile, kind, &settings).await {
                    Ok(()) => guard
                        .start(&profile, kind, &settings)
                        .await
                        .map_err(|error| (error, None)),
                    Err(error) => Err((error, None)),
                }
            } else {
                Ok(guard.status(&profile, kind, &settings))
            }
        };

        let status = self.unwrap_tunnel_result(id, kind, &profile, result, "FRP 线路已恢复")?;
        self.persist_tunnel_url(id, kind, &status.public_url)?;
        Ok(status)
    }

    /// 验证隧道配置：本地服务没在跑时，测完自动断开，不留下悬空隧道。
    pub async fn test_tunnel(
        &self,
        id: &str,
        kind: TunnelServiceKind,
    ) -> AppResult<TunnelTestResult> {
        let profile = self.profile_by_id(id)?;
        self.validate_tunnel_start(id, kind)?;
        self.sync_tunnel_routes_from_runtime().await?;
        let settings = self.settings()?;
        let runtime_running = local_service_listening(&profile, kind)?;

        let result = {
            let mut guard = supervisor().lock().await;
            let was_tunnel_running = guard.status(&profile, kind, &settings).state == "running";
            if was_tunnel_running && tunnel_type_for(&profile, kind) == "frp" {
                guard
                    .start(&profile, kind, &settings)
                    .await
                    .map_err(|error| (error, guard.route_profile(id, kind)))
            } else {
                let stop_result = if was_tunnel_running {
                    guard.stop(&profile, kind, &settings).await
                } else {
                    Ok(())
                };
                match stop_result {
                    Ok(()) => guard
                        .start(&profile, kind, &settings)
                        .await
                        .map_err(|error| (error, None)),
                    Err(error) => Err((error, None)),
                }
            }
        };

        let status = self.unwrap_tunnel_result(id, kind, &profile, result, "FRP 测试失败")?;
        let public_url = status.public_url.clone();

        if runtime_running {
            self.persist_tunnel_url(id, kind, &public_url)?;
            return Ok(TunnelTestResult {
                success: !public_url.is_empty() || status.state == "running",
                public_url,
                kept_running: true,
                message: "隧道测试成功，本地服务运行中，已保持连接。".into(),
            });
        }

        {
            let mut guard = supervisor().lock().await;
            guard.stop(&profile, kind, &settings).await?;
        }

        let success = !public_url.is_empty();
        Ok(TunnelTestResult {
            success,
            message: if success {
                "隧道配置验证通过。本地服务未运行，测试连接已自动断开。".into()
            } else {
                "隧道进程已退出，未获取到公网地址。".into()
            },
            public_url,
            kept_running: false,
        })
    }

    fn validate_tunnel_start(&self, id: &str, kind: TunnelServiceKind) -> AppResult<()> {
        self.with_data(|store| validate_service_start(store.list(), id, kind.workspace_service()))
    }

    fn unwrap_tunnel_result(
        &self,
        id: &str,
        kind: TunnelServiceKind,
        failed: &WorkspaceProfile,
        result: Result<TunnelStatus, (AppError, Option<WorkspaceProfile>)>,
        rollback_label: &str,
    ) -> AppResult<TunnelStatus> {
        match result {
            Ok(status) => Ok(status),
            Err((error, restored)) => {
                if let Some(restored) = restored {
                    if let Err(rollback_error) =
                        self.restore_tunnel_config(id, kind, failed, &restored)
                    {
                        return Err(AppError::Message(format!(
                            "{rollback_label}，但配置回滚失败：{error}; rollback: {rollback_error}"
                        )));
                    }
                }
                Err(error)
            }
        }
    }

    fn restore_tunnel_config(
        &self,
        id: &str,
        kind: TunnelServiceKind,
        failed: &WorkspaceProfile,
        restored: &WorkspaceProfile,
    ) -> AppResult<()> {
        self.with_data(|store| {
            let Some(mut current) = store.get(id).cloned() else {
                return Ok(());
            };
            let unchanged_since_failure = match kind {
                TunnelServiceKind::Mcp => mcp_tunnel_matches(&current, failed),
                TunnelServiceKind::Actions => actions_tunnel_matches(&current, failed),
            };
            if !unchanged_since_failure {
                return Err(AppError::Message(
                    "检测到更新的隧道配置，已拒绝用旧请求覆盖。".into(),
                ));
            }
            match kind {
                TunnelServiceKind::Mcp => current.tunnel = restored.tunnel.clone(),
                TunnelServiceKind::Actions => {
                    current.actions.public_url = restored.actions.public_url.clone();
                    current.actions.tunnel_type = restored.actions.tunnel_type.clone();
                    current.actions.frp_server = restored.actions.frp_server.clone();
                    current.actions.frp_subdomain = restored.actions.frp_subdomain.clone();
                    current.actions.frp_profile_id = restored.actions.frp_profile_id.clone();
                    current.actions.frp_server_port = restored.actions.frp_server_port;
                    current.actions.cloudflare_mode = restored.actions.cloudflare_mode.clone();
                    current.actions.cloudflare_token = restored.actions.cloudflare_token.clone();
                    current.actions.use_proxy = restored.actions.use_proxy;
                }
            }
            store.update(current)
        })
    }
}

fn tunnel_type_for(profile: &WorkspaceProfile, kind: TunnelServiceKind) -> &str {
    match kind {
        TunnelServiceKind::Mcp => profile.tunnel.tunnel_type.as_str(),
        TunnelServiceKind::Actions => profile.actions.tunnel_type.as_str(),
    }
}

fn local_service_listening(profile: &WorkspaceProfile, kind: TunnelServiceKind) -> AppResult<bool> {
    let port = match kind {
        TunnelServiceKind::Mcp => profile.runtime.local_port,
        TunnelServiceKind::Actions => profile.actions.local_port,
    };
    Ok(platform().find_pid_listening_on_port(port)?.is_some())
}

fn mcp_tunnel_matches(left: &WorkspaceProfile, right: &WorkspaceProfile) -> bool {
    left.tunnel.tunnel_type == right.tunnel.tunnel_type
        && left.tunnel.public_url == right.tunnel.public_url
        && left.tunnel.frp_server == right.tunnel.frp_server
        && left.tunnel.frp_subdomain == right.tunnel.frp_subdomain
        && left.tunnel.frp_profile_id == right.tunnel.frp_profile_id
        && left.tunnel.frp_server_port == right.tunnel.frp_server_port
        && left.tunnel.cloudflare_mode == right.tunnel.cloudflare_mode
        && left.tunnel.use_proxy == right.tunnel.use_proxy
}

fn actions_tunnel_matches(left: &WorkspaceProfile, right: &WorkspaceProfile) -> bool {
    left.actions.public_url == right.actions.public_url
        && left.actions.tunnel_type == right.actions.tunnel_type
        && left.actions.frp_server == right.actions.frp_server
        && left.actions.frp_subdomain == right.actions.frp_subdomain
        && left.actions.frp_profile_id == right.actions.frp_profile_id
        && left.actions.frp_server_port == right.actions.frp_server_port
        && left.actions.cloudflare_mode == right.actions.cloudflare_mode
        && left.actions.cloudflare_token == right.actions.cloudflare_token
        && left.actions.use_proxy == right.actions.use_proxy
}

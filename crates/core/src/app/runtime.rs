use std::sync::atomic::Ordering;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::App;
use crate::error::{AppError, AppResult};
use crate::global_gateway;
use crate::platform::platform;
use crate::runtime::{
    await_listener_shutdown, is_own_process, port_busy_message, wait_for_port_free, ServiceKind,
};
use crate::tunnel::{
    maybe_start_for_runtime, stop_for_runtime, supervisor as tunnel_supervisor,
    sync_managed_runtime_routes, TunnelServiceKind, TunnelStatus,
};
use crate::workspace::resources::validate_service_start;
use crate::workspace::{RuntimeStatusDto, WorkspaceProfile};

/// 一个正在运行的服务（用于总览与优雅退出）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunningService {
    pub workspace_id: String,
    pub kind: ServiceKind,
}

/// 单个工作区两条服务的状态汇总。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceOverview {
    pub workspace: WorkspaceProfile,
    pub mcp: RuntimeStatusDto,
    pub actions: RuntimeStatusDto,
    pub mcp_tunnel: TunnelStatus,
    pub actions_tunnel: TunnelStatus,
}

impl App {
    /// 启动 MCP 或 Actions，并记住它以便下次守护进程启动时恢复。
    pub async fn start_service(&self, id: &str, kind: ServiceKind) -> AppResult<RuntimeStatusDto> {
        let status = self.start_inner(id, kind).await?;
        if status.state == "running" || status.state == "starting" {
            self.remember_runtime_state(id, kind, true)?;
        }
        Ok(status)
    }

    pub async fn stop_service(&self, id: &str, kind: ServiceKind) -> AppResult<RuntimeStatusDto> {
        let status = self.stop_inner(id, kind).await?;
        self.remember_runtime_state(id, kind, false)?;
        Ok(status)
    }

    /// stop → start，串行化以免和密钥变更触发的重启互相踩踏。
    pub async fn restart_service(
        &self,
        id: &str,
        kind: ServiceKind,
    ) -> AppResult<RuntimeStatusDto> {
        let _guard = self.restart_gate.lock().await;
        let was_running = self.with_runtime(|runtime| Ok(runtime.is_running(id, kind)))?;
        if was_running {
            let _ = self.stop_inner(id, kind).await?;
        }
        let status = self.start_inner(id, kind).await?;
        if status.state == "running" || status.state == "starting" {
            self.remember_runtime_state(id, kind, true)?;
        }
        Ok(status)
    }

    /// 这个工作区的服务已经在跑（或正在起）时返回它的状态，否则 None。
    fn status_if_already_up(
        &self,
        id: &str,
        kind: ServiceKind,
    ) -> AppResult<Option<RuntimeStatusDto>> {
        let profile = self.profile_by_id(id)?;
        self.with_runtime(|runtime| {
            Ok(runtime
                .is_active(id, kind)
                .then(|| status_for(runtime, &profile, kind)))
        })
    }

    /// 刷新并返回服务状态（会探测端口，能发现监听器意外退出）。
    pub fn service_status(&self, id: &str, kind: ServiceKind) -> AppResult<RuntimeStatusDto> {
        let profile = self.profile_by_id(id)?;
        self.with_runtime(|runtime| {
            match kind {
                ServiceKind::Mcp => runtime.refresh_mcp(&profile),
                ServiceKind::Actions => runtime.refresh_actions(&profile),
            }
            Ok(status_for(runtime, &profile, kind))
        })
    }

    pub fn is_service_running(&self, id: &str, kind: ServiceKind) -> AppResult<bool> {
        self.with_runtime(|runtime| Ok(runtime.is_running(id, kind)))
    }

    /// 当前所有运行中的服务。
    pub fn running_services(&self) -> AppResult<Vec<RunningService>> {
        self.with_runtime(|runtime| {
            let mut services = Vec::new();
            for kind in [ServiceKind::Mcp, ServiceKind::Actions] {
                for workspace_id in runtime.running_workspace_ids(kind) {
                    services.push(RunningService { workspace_id, kind });
                }
            }
            Ok(services)
        })
    }

    /// 每个工作区的服务 + 隧道状态。
    pub async fn overview(&self) -> AppResult<Vec<ServiceOverview>> {
        let profiles = self.list_workspaces()?;
        let settings = self.settings()?;
        let tunnels = tunnel_supervisor().lock().await;
        let mut items = Vec::with_capacity(profiles.len());
        for profile in profiles {
            let (mcp, actions) = self.with_runtime(|runtime| {
                runtime.refresh_mcp(&profile);
                runtime.refresh_actions(&profile);
                Ok((
                    runtime.mcp_status(&profile),
                    runtime.actions_status(&profile),
                ))
            })?;
            items.push(ServiceOverview {
                mcp_tunnel: tunnels.status(&profile, TunnelServiceKind::Mcp, &settings),
                actions_tunnel: tunnels.status(&profile, TunnelServiceKind::Actions, &settings),
                workspace: profile,
                mcp,
                actions,
            });
        }
        Ok(items)
    }

    /// 守护进程启动时恢复上一次运行的服务。只执行一次；失败的项写到 stderr，不中断其他项。
    pub async fn restore_runtime_state(&self) -> AppResult<Vec<RunningService>> {
        if self.startup_restore_attempted.swap(true, Ordering::SeqCst) {
            return Ok(Vec::new());
        }
        let settings = self.settings()?;
        if !settings.restore_runtime_state_on_launch {
            return Ok(Vec::new());
        }
        let mut restored = Vec::new();
        let planned = settings
            .restore_mcp_workspace_ids
            .iter()
            .map(|id| (id.clone(), ServiceKind::Mcp))
            .chain(
                settings
                    .restore_actions_workspace_ids
                    .iter()
                    .map(|id| (id.clone(), ServiceKind::Actions)),
            );
        for (id, kind) in planned {
            if self.profile_by_id(&id).is_err() {
                continue;
            }
            match self.start_inner(&id, kind).await {
                Ok(_) => restored.push(RunningService {
                    workspace_id: id,
                    kind,
                }),
                Err(error) => eprintln!("恢复 {} {} 失败：{error}", kind.as_str(), id),
            }
        }
        Ok(restored)
    }

    /// 优雅退出：停掉所有服务、隧道和全局入口，但保留“下次恢复”记录。
    pub async fn shutdown_all(&self) {
        for service in self.running_services().unwrap_or_default() {
            if let Err(error) = self.stop_inner(&service.workspace_id, service.kind).await {
                eprintln!(
                    "停止 {} {} 失败：{error}",
                    service.kind.as_str(),
                    service.workspace_id
                );
            }
        }
        if let Err(error) = global_gateway::stop().await {
            eprintln!("停止全局入口失败：{error}");
        }
    }

    async fn start_inner(&self, id: &str, kind: ServiceKind) -> AppResult<RuntimeStatusDto> {
        self.with_data(|store| validate_service_start(store.list(), id, kind.workspace_service()))?;
        let profile = self.profile_by_id(id)?;

        // start 是幂等的：已经在跑（或正在起）就把当前状态还回去。
        //
        // 这一步必须排在端口检查前面。服务的监听器就住在守护进程自己的进程里，
        // 端口检查看到占用者的 pid 等于自己，会判成"上一次残留的服务"，
        // 回一句「请先停止服务或稍后再试」——可服务本来就该在跑。
        // 症状是再敲一次 gld start 就报错，或者并发的两个 start 挂掉一个。
        if let Some(status) = self.status_if_already_up(id, kind)? {
            return Ok(status);
        }
        if let Err(error) = ensure_port_available(port_for(&profile, kind), kind.label()).await {
            // 上面那次检查之后、这次报错之前，可能正好有另一个 start 把服务拉起来了，
            // 端口就是它占的。再问一次运行时，是自己的服务在跑就不算错。
            return match self.status_if_already_up(id, kind)? {
                Some(status) => Ok(status),
                None => Err(error),
            };
        }
        if uses_global_gateway(&profile, kind) {
            global_gateway::ensure_started().await?;
        }
        let profile = self.profile_by_id(id)?;
        self.with_runtime(|runtime| match kind {
            ServiceKind::Mcp => runtime.start_mcp(&profile),
            ServiceKind::Actions => runtime.start_actions(&profile),
        })?;
        self.sync_tunnel_routes_from_runtime().await?;

        match maybe_start_for_runtime(&profile, kind.tunnel_kind()).await {
            Ok(Some(url)) => self.persist_tunnel_url(id, kind.tunnel_kind(), &url)?,
            Ok(None) => {}
            Err(error) => {
                crate::logs::append_profile_log(
                    id,
                    kind.stderr_log_name(),
                    &format!("[tunnel] 自动启动隧道失败：{error}"),
                );
                eprintln!("{} 隧道自动启动失败（{id}）：{error}", kind.as_str());
            }
        }

        let profile = self.profile_by_id(id)?;
        tokio::time::sleep(Duration::from_millis(250)).await;
        self.with_runtime(|runtime| {
            match kind {
                ServiceKind::Mcp => runtime.refresh_mcp(&profile),
                ServiceKind::Actions => runtime.refresh_actions(&profile),
            }
            let status = status_for(runtime, &profile, kind);
            if status.state == "error" {
                return Err(AppError::Message(status.local_message));
            }
            Ok(status)
        })
    }

    async fn stop_inner(&self, id: &str, kind: ServiceKind) -> AppResult<RuntimeStatusDto> {
        let profile = self.profile_by_id(id)?;
        let port = port_for(&profile, kind);
        let handle = self.with_runtime(|runtime| Ok(runtime.begin_stop(id, kind)))?;
        await_listener_shutdown(handle, port).await;
        self.with_runtime(|runtime| {
            runtime.finish_stop(id, kind);
            Ok(())
        })?;
        stop_for_runtime(&profile, kind.tunnel_kind()).await?;
        self.sync_tunnel_routes_from_runtime().await?;
        self.with_runtime(|runtime| Ok(status_for(runtime, &profile, kind)))
    }

    pub(super) async fn sync_tunnel_routes_from_runtime(&self) -> AppResult<()> {
        let active_keys = self.with_runtime(|runtime| Ok(runtime.active_tunnel_service_keys()))?;
        sync_managed_runtime_routes(active_keys).await
    }

    pub(super) fn persist_tunnel_url(
        &self,
        id: &str,
        kind: TunnelServiceKind,
        url: &str,
    ) -> AppResult<()> {
        if url.is_empty() {
            return Ok(());
        }
        self.with_data(|store| {
            let Some(mut profile) = store.get(id).cloned() else {
                return Ok(());
            };
            match kind {
                TunnelServiceKind::Mcp => profile.tunnel.public_url = url.to_string(),
                TunnelServiceKind::Actions => profile.actions.public_url = url.to_string(),
            }
            store.update(profile)
        })
    }

    fn remember_runtime_state(&self, id: &str, kind: ServiceKind, running: bool) -> AppResult<()> {
        self.update_settings(|settings| {
            let ids = match kind {
                ServiceKind::Mcp => &mut settings.restore_mcp_workspace_ids,
                ServiceKind::Actions => &mut settings.restore_actions_workspace_ids,
            };
            if running {
                if !ids.iter().any(|workspace_id| workspace_id == id) {
                    ids.push(id.to_string());
                    ids.sort();
                }
            } else {
                ids.retain(|workspace_id| workspace_id != id);
            }
            Ok(())
        })
    }
}

fn status_for(
    runtime: &crate::runtime::RuntimeSupervisor,
    profile: &WorkspaceProfile,
    kind: ServiceKind,
) -> RuntimeStatusDto {
    match kind {
        ServiceKind::Mcp => runtime.mcp_status(profile),
        ServiceKind::Actions => runtime.actions_status(profile),
    }
}

fn port_for(profile: &WorkspaceProfile, kind: ServiceKind) -> u16 {
    match kind {
        ServiceKind::Mcp => profile.runtime.local_port,
        ServiceKind::Actions => profile.actions.local_port,
    }
}

fn uses_global_gateway(profile: &WorkspaceProfile, kind: ServiceKind) -> bool {
    match kind {
        ServiceKind::Mcp => profile.tunnel.use_global_gateway,
        ServiceKind::Actions => profile.actions.use_global_gateway,
    }
}

async fn ensure_port_available(port: u16, service_label: &str) -> AppResult<()> {
    let Some(pid) = platform().find_pid_listening_on_port(port)? else {
        return Ok(());
    };
    if is_own_process(pid) && wait_for_port_free(port, Duration::from_secs(3)).await {
        return Ok(());
    }
    if let Some(pid) = platform().find_pid_listening_on_port(port)? {
        return Err(AppError::Message(port_busy_message(
            port,
            service_label,
            pid,
        )));
    }
    Ok(())
}

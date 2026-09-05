//! 协议 → 业务的唯一翻译层。
//!
//! 每个 [`Request`] 变体对应一次 [`App`] 调用，结果统一序列化成 JSON。
//! 命令行直连模式和守护进程模式都走这个函数，所以两边不可能行为不一致。

use std::sync::Arc;

use gld_core::app::{App, WorkspaceTarget};
use gld_core::workspace::WorkspaceProfile;
use serde::Serialize;
use serde_json::Value;

use crate::protocol::{Request, RpcError};

/// 守护进程才有的上下文；直连模式下为 `None`（`DaemonInfo` / `Shutdown` 会被拒绝）。
pub struct DaemonContext {
    pub info: Box<dyn Fn() -> crate::protocol::DaemonInfo + Send + Sync>,
    pub request_shutdown: Box<dyn Fn() + Send + Sync>,
}

pub async fn dispatch(
    app: &Arc<App>,
    daemon: Option<&DaemonContext>,
    request: Request,
) -> Result<Value, RpcError> {
    use Request as R;
    match request {
        // ---- 守护进程 ----
        R::Ping => ok(&"pong"),
        R::DaemonInfo => match daemon {
            Some(context) => ok(&(context.info)()),
            None => Err(RpcError::protocol("当前不是守护进程，没有 daemon_info")),
        },
        R::Shutdown => match daemon {
            Some(context) => {
                (context.request_shutdown)();
                ok(&"shutting_down")
            }
            None => Err(RpcError::protocol("当前不是守护进程，无法 shutdown")),
        },

        // ---- 工作区 ----
        R::ListWorkspaces => ok(&app.list_workspaces()?),
        R::ResolveWorkspace { target } => ok(&app.resolve_workspace(&target)?),
        R::CreateWorkspace { path, options } => ok(&app.create_workspace(&path, options)?),
        R::EnsureWorkspace { path, options } => ok(&app.ensure_workspace(&path, options)?),
        R::SetWorkspaceFields {
            target,
            assignments,
        } => {
            let profile = resolve(app, &target)?;
            ok(&app.set_workspace_fields(&profile.id, &assignments).await?)
        }
        R::DeleteWorkspace { target } => {
            let profile = resolve(app, &target)?;
            ok(&app.delete_workspace(&profile.id).await?)
        }
        R::UseWorkspace { target } => {
            let profile = resolve(app, &target)?;
            app.set_last_workspace(&profile.id)?;
            ok(&profile)
        }

        // ---- 服务 ----
        R::StartService { target, kind } => {
            let profile = resolve(app, &target)?;
            ok(&app.start_service(&profile.id, kind).await?)
        }
        R::StopService { target, kind } => {
            let profile = resolve(app, &target)?;
            ok(&app.stop_service(&profile.id, kind).await?)
        }
        R::RestartService { target, kind } => {
            let profile = resolve(app, &target)?;
            ok(&app.restart_service(&profile.id, kind).await?)
        }
        R::ServiceStatus { target, kind } => {
            let profile = resolve(app, &target)?;
            ok(&app.service_status(&profile.id, kind)?)
        }
        R::Overview => ok(&app.overview().await?),

        // ---- 隧道 ----
        R::TunnelStart { target, kind } => {
            let profile = resolve(app, &target)?;
            ok(&app.start_tunnel(&profile.id, kind).await?)
        }
        R::TunnelStop { target, kind } => {
            let profile = resolve(app, &target)?;
            ok(&app.stop_tunnel(&profile.id, kind).await?)
        }
        R::TunnelRestart { target, kind } => {
            let profile = resolve(app, &target)?;
            ok(&app.restart_tunnel(&profile.id, kind).await?)
        }
        R::TunnelTest { target, kind } => {
            let profile = resolve(app, &target)?;
            ok(&app.test_tunnel(&profile.id, kind).await?)
        }
        R::TunnelStatus { target, kind } => {
            let profile = resolve(app, &target)?;
            ok(&app.tunnel_status(&profile.id, kind).await?)
        }
        R::FrpSnippet {
            target,
            kind,
            reveal,
        } => {
            let profile = resolve(app, &target)?;
            ok(&app.frp_snippet(&profile.id, kind, reveal)?)
        }

        // ---- 全局入口 ----
        R::GatewayConfig => ok(&app.gateway_config()?),
        R::SetGatewayConfig { config } => {
            app.set_gateway_config(config)?;
            ok(&app.gateway_config()?)
        }
        R::GatewayStart => ok(&app.start_gateway().await?),
        R::GatewayStop => {
            app.stop_gateway().await?;
            ok(&app.gateway_status().await)
        }
        R::GatewayStatus => ok(&app.gateway_status().await),
        R::GatewayHealth => ok(&app.gateway_health().await),

        // ---- 密钥 ----
        R::WorkspaceSecret { target, key } => {
            let profile = resolve(app, &target)?;
            ok(&app.workspace_secret(&profile.id, &key)?)
        }
        R::SetWorkspaceSecret { target, key, value } => {
            let profile = resolve(app, &target)?;
            app.set_workspace_secret(&profile.id, &key, &value).await?;
            ok(&true)
        }
        R::RegenerateWorkspaceSecret { target, key } => {
            let profile = resolve(app, &target)?;
            ok(&app.regenerate_workspace_secret(&profile.id, &key).await?)
        }
        R::SharedSecret { key } => ok(&app.shared_secret(&key)?),
        R::SetSharedSecret { key, value } => {
            app.set_shared_secret(&key, &value).await?;
            ok(&true)
        }
        R::RegenerateSharedSecret { key } => ok(&app.regenerate_shared_secret(&key).await?),

        // ---- 设置 ----
        R::Proxy => ok(&app.proxy()?),
        R::SetProxy { proxy } => {
            app.set_proxy(proxy)?;
            ok(&app.proxy()?)
        }
        R::RuntimeSettings => ok(&app.global_runtime_settings()?),
        R::SetRuntimeSettings { runtime } => {
            app.set_global_runtime_settings(runtime)?;
            ok(&app.global_runtime_settings()?)
        }
        R::ListFrpProfiles => ok(&app.list_frp_profiles()?),
        R::SaveFrpProfile { profile, token } => ok(&app.save_frp_profile(profile, token)?),
        R::DeleteFrpProfile { id, force } => {
            app.delete_frp_profile(&id, force)?;
            ok(&true)
        }

        // ---- 日志 ----
        R::Logs {
            target,
            kind,
            max_bytes,
        } => {
            let profile = resolve(app, &target)?;
            ok(&app.workspace_logs(&profile.id, kind, max_bytes)?)
        }
        R::LogDir { target } => {
            let profile = resolve(app, &target)?;
            ok(&app.workspace_log_dir(&profile.id)?)
        }

        // ---- Planning ----
        R::PlanningState { target } => {
            let profile = resolve(app, &target)?;
            ok(&app.planning_state(&profile.id)?)
        }
        R::SetPlanningMode { target, mode } => {
            let profile = resolve(app, &target)?;
            ok(&app.set_planning_mode(&profile.id, mode)?)
        }
        R::CreateGoal {
            target,
            title,
            objective,
            success_criteria,
            constraints,
        } => {
            let profile = resolve(app, &target)?;
            ok(&app.create_goal(
                &profile.id,
                &title,
                &objective,
                success_criteria,
                constraints,
            )?)
        }
        R::UpdateGoal {
            target,
            goal_id,
            title,
            objective,
            status,
            constraints,
            completed_criteria_ids,
            focus,
        } => {
            let profile = resolve(app, &target)?;
            ok(&app.update_goal(
                &profile.id,
                &goal_id,
                title,
                objective,
                status,
                constraints,
                completed_criteria_ids,
                focus,
            )?)
        }
        R::CreatePlan {
            target,
            goal_id,
            title,
            objective,
            steps,
        } => {
            let profile = resolve(app, &target)?;
            ok(&app.create_plan(&profile.id, goal_id, &title, &objective, steps)?)
        }
        R::UpdatePlan {
            target,
            plan_id,
            status,
            step_updates,
            focus,
        } => {
            let profile = resolve(app, &target)?;
            ok(&app.update_plan(&profile.id, &plan_id, status, step_updates, focus)?)
        }
        R::AcceptGoalReview { target, goal_id } => {
            let profile = resolve(app, &target)?;
            ok(&app.accept_goal_review(&profile.id, &goal_id)?)
        }
        R::RejectGoalReview {
            target,
            goal_id,
            feedback,
        } => {
            let profile = resolve(app, &target)?;
            ok(&app.reject_goal_review(&profile.id, &goal_id, feedback)?)
        }
        R::AcceptPlanReview { target, plan_id } => {
            let profile = resolve(app, &target)?;
            ok(&app.accept_plan_review(&profile.id, &plan_id)?)
        }
        R::RejectPlanReview {
            target,
            plan_id,
            feedback,
        } => {
            let profile = resolve(app, &target)?;
            ok(&app.reject_plan_review(&profile.id, &plan_id, feedback)?)
        }

        // ---- 工具内核 ----
        R::ListTools { target } => {
            let profile = resolve(app, &target)?;
            ok(&app.list_tools(&profile.id)?)
        }
        R::CallTool { target, name, args } => {
            let profile = resolve(app, &target)?;
            // 工具内核是同步 API，内部用 block_on 驱动子进程，不能占用异步 worker 线程。
            let app = app.clone();
            let id = profile.id;
            tokio::task::spawn_blocking(move || app.call_tool(&id, &name, args))
                .await
                .map_err(|error| RpcError::internal(format!("工具调用任务失败：{error}")))?
                .map_err(RpcError::from)
        }

        // ---- 观察 ----
        R::Doctor => ok(&app.doctor()?),
        R::Health { target } => {
            let profile = resolve(app, &target)?;
            ok(&app.health_checks(&profile.id).await?)
        }
        R::HistorySessions { target } => {
            let profile = resolve(app, &target)?;
            ok(&app.history_sessions(&profile.id)?)
        }
        R::Usage { target } => {
            let profile = resolve(app, &target)?;
            ok(&app.usage_stats(&profile.id)?)
        }
        R::AgentContext { target } => {
            let profile = resolve(app, &target)?;
            ok(&app.agent_context(&profile.id)?)
        }
        R::GlobalAgentContext => ok(&app.global_agent_context()),
        // ---- 软件 ----
    }
}

fn resolve(app: &App, target: &WorkspaceTarget) -> Result<WorkspaceProfile, RpcError> {
    app.resolve_workspace(target).map_err(RpcError::from)
}

fn ok<T: Serialize>(value: &T) -> Result<Value, RpcError> {
    serde_json::to_value(value).map_err(|error| RpcError::internal(error.to_string()))
}

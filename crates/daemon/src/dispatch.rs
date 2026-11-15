//! 协议 → 业务的唯一翻译层。
//!
//! 每个 [`Request`] 变体映射到应用操作或执行代请求，结果统一序列化成 JSON。
//! 直连模式使用本地应用操作，守护进程显式把执行请求交给它持有的 Router。

use std::sync::Arc;

use gld_core::app::{App, WorkspaceTarget};
use gld_core::runtime::ListenerManager;
use gld_core::workspace::WorkspaceProfile;
use gld_host::global_gateway;
use serde::Serialize;
use serde_json::Value;

use crate::protocol::{Request, RpcError};

/// 守护进程才有的上下文；直连模式下为 `None`（`DaemonInfo` / `Shutdown` 会被拒绝）。
pub struct DaemonContext {
    pub info: Box<dyn Fn() -> crate::protocol::DaemonInfo + Send + Sync>,
    pub request_shutdown: Box<dyn Fn() + Send + Sync>,
    pub router: Arc<gld_host::generation::Router>,
}

pub async fn dispatch(
    app: &Arc<App>,
    listeners: &ListenerManager,
    daemon: Option<&DaemonContext>,
    request: Request,
) -> Result<Value, RpcError> {
    use Request as R;
    match request {
        // ---- 守护进程 ----
        R::Ping => ok(&"pong"),
        R::NodeInvite { name } => {
            let invitation = gld_core::nodes::pairing::invite(&name).map_err(RpcError::app)?;
            ok(&invitation)
        }
        R::NodeRegister {
            name,
            id,
            credential,
        } => {
            let registered =
                gld_core::nodes::config::register_machine(&name, id.as_deref(), &credential)
                    .map_err(RpcError::app)?;
            ok(&registered)
        }
        R::NodeRevoke { machine_id } => change_node_credential(machine_id, None).await,
        R::NodeRemove { machine } => remove_node(machine).await,
        R::NodeRotateCredential {
            machine_id,
            credential,
        } => change_node_credential(machine_id, Some(credential)).await,
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
            ok(&app
                .set_workspace_fields(listeners, &profile.id, &assignments)
                .await?)
        }
        R::DeleteWorkspace { target } => {
            let profile = resolve(app, &target)?;
            ok(&app.delete_workspace(listeners, &profile.id).await?)
        }
        R::Prune { confirmed } => ok(&app.prune(listeners, confirmed).await?),
        R::UseWorkspace { target } => {
            let profile = resolve(app, &target)?;
            app.set_last_workspace(&profile.id)?;
            ok(&profile)
        }

        // ---- 服务 ----
        R::StartService { target, kind } => {
            let profile = resolve(app, &target)?;
            ok(&app.start_service(listeners, &profile.id, kind).await?)
        }
        R::StopService { target, kind } => {
            let profile = resolve(app, &target)?;
            ok(&app.stop_service(listeners, &profile.id, kind).await?)
        }
        R::RestartService { target, kind } => {
            let profile = resolve(app, &target)?;
            ok(&app.restart_service(listeners, &profile.id, kind).await?)
        }
        R::ServiceStatus { target, kind } => {
            let profile = resolve(app, &target)?;
            ok(&app.service_status(listeners, &profile.id, kind)?)
        }
        R::Overview => ok(&app.overview(listeners).await?),

        // ---- 隧道 ----
        R::TunnelStart { target, kind } => {
            let profile = resolve(app, &target)?;
            ok(&app.start_tunnel(listeners, &profile.id, kind).await?)
        }
        R::TunnelStop { target, kind } => {
            let profile = resolve(app, &target)?;
            ok(&app.stop_tunnel(&profile.id, kind).await?)
        }
        R::TunnelRestart { target, kind } => {
            let profile = resolve(app, &target)?;
            ok(&app.restart_tunnel(listeners, &profile.id, kind).await?)
        }
        R::TunnelTest { target, kind } => {
            let profile = resolve(app, &target)?;
            ok(&app.test_tunnel(listeners, &profile.id, kind).await?)
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
        R::GatewayStart => ok(&global_gateway::ensure_started().await?),
        R::GatewayStop => {
            global_gateway::stop().await?;
            ok(&global_gateway::status().await)
        }
        R::GatewayStatus => ok(&global_gateway::status().await),
        R::GatewayHealth => ok(&global_gateway::health().await),

        // ---- 聚合入口 ----
        R::HubStatus => ok(&app.hub_status(listeners).await?),
        R::SetHubConfig { config } => ok(&app.set_hub_config(listeners, config).await?),
        R::HubAddMembers { targets } => ok(&app.add_hub_members(listeners, &targets).await?),
        R::HubRemoveMembers { targets } => ok(&app.remove_hub_members(listeners, &targets).await?),
        R::HubAddRemote { spec } => ok(&app.add_ccnm_member(listeners, spec).await?),
        R::HubRemoveRemote { selector } => {
            ok(&app.remove_ccnm_member(listeners, &selector).await?)
        }
        R::HubStart => ok(&app.start_hub(listeners).await?),
        R::HubStop => ok(&app.stop_hub(listeners).await?),
        R::HubSecret { key } => ok(&app.hub_secret(&key)?),
        R::RegenerateHubSecret { key } => ok(&app.regenerate_hub_secret(listeners, &key).await?),
        R::SetHubSecret { key, value } => {
            app.set_hub_secret(listeners, &key, &value).await?;
            ok(&true)
        }
        R::HubJoinAll => ok(&app.join_all_workspaces()?),
        R::HubHealth => ok(&app.hub_health(listeners).await?),
        R::HubEnsureStarted => ok(&app.ensure_hub_started(listeners).await?),
        R::HubUsage => ok(&gld_host::hub_runtime::usage().await),
        R::HubLogs { max_bytes } => ok(&app.hub_logs(max_bytes)?),

        // ---- 密钥 ----
        R::WorkspaceSecret { target, key } => {
            let profile = resolve(app, &target)?;
            ok(&app.workspace_secret(&profile.id, &key)?)
        }
        R::SetWorkspaceSecret { target, key, value } => {
            let profile = resolve(app, &target)?;
            app.set_workspace_secret(listeners, &profile.id, &key, &value)
                .await?;
            ok(&true)
        }
        R::RegenerateWorkspaceSecret { target, key } => {
            let profile = resolve(app, &target)?;
            ok(&app
                .regenerate_workspace_secret(listeners, &profile.id, &key)
                .await?)
        }
        R::SharedSecret { key } => ok(&app.shared_secret(&key)?),
        R::SetSharedSecret { key, value } => {
            app.set_shared_secret(listeners, &key, &value).await?;
            ok(&true)
        }
        R::RegenerateSharedSecret { key } => {
            ok(&app.regenerate_shared_secret(listeners, &key).await?)
        }

        // ---- 设置 ----
        R::Proxy => ok(&app.proxy()?),
        R::SetProxy { proxy } => {
            app.set_proxy(proxy)?;
            ok(&app.proxy()?)
        }
        R::RuntimeSettings => ok(&app.global_runtime_settings()?),
        R::SetRuntimeSettings { runtime } => {
            app.set_global_runtime_settings(listeners, runtime)?;
            ok(&app.global_runtime_settings()?)
        }
        R::ListGrants => ok(&app.list_grants()?),
        R::AddGrant { spec } => ok(&app.add_grant(spec)?),
        R::AddBrowserGrant { spec } => ok(&app.add_browser_grant(spec)?),
        R::RemoveGrant { selector } => {
            // 停命令要等子进程收尾（每条最多 1.5 秒），不能占异步 worker。
            let app = app.clone();
            let router = daemon.map(|context| context.router.clone());
            let removed = tokio::task::spawn_blocking(move || match router {
                Some(router) => app.remove_grant_with_cleanup(&selector, move |grant_id, _home| {
                    router.revoke_grant(grant_id)
                }),
                None => app.remove_grant_local(&selector),
            })
            .await
            .map_err(|error| RpcError::internal(format!("作废 grant 的任务失败：{error}")))?
            .map_err(RpcError::from)?;
            ok(&removed)
        }
        R::ListAgentTasks => ok(&app.list_agent_tasks()?),
        R::ShowAgentTask { task_id } => ok(&app.agent_task(&task_id)?),
        R::ApproveAgentTask {
            task_id,
            agent,
            model,
            effort,
        } => {
            // 起 Agent 前要问一次 `--version`（最多 5 秒）、拍 git 快照，不能占异步 worker。
            let app = app.clone();
            let router = daemon.map(|context| context.router.clone());
            let started = tokio::task::spawn_blocking(move || match router {
                Some(router) => {
                    let root = app.agent_task_root(&task_id)?;
                    agent_reply(router.approve_agent(
                        &root,
                        &task_id,
                        agent.as_deref(),
                        model.as_deref(),
                        effort.as_deref(),
                    ))
                }
                None => app.approve_agent_task_local(
                    &task_id,
                    agent.as_deref(),
                    model.as_deref(),
                    effort.as_deref(),
                ),
            })
            .await
            .map_err(|error| RpcError::internal(format!("放行委派的任务失败：{error}")))?
            .map_err(RpcError::from)?;
            ok(&started)
        }
        R::DenyAgentTask { task_id } => ok(&app.deny_agent_task(&task_id)?),
        R::StopAgentTask { task_id } => {
            // 要经管道问执行代，不占异步 worker。
            let app = app.clone();
            let router = daemon.map(|context| context.router.clone());
            let stopped = tokio::task::spawn_blocking(move || match router {
                Some(router) => agent_reply(router.stop_agent(&task_id)),
                None => app.stop_agent_task_local(&task_id),
            })
            .await
            .map_err(|error| RpcError::internal(format!("停止委派的任务失败：{error}")))?
            .map_err(RpcError::from)?;
            ok(&stopped)
        }
        R::McpServers => ok(&app.mcp_servers()?),
        R::SwitchMcpServers { names, on, all } => ok(&app.switch_mcp_servers(names, on, all)?),
        R::TestMcpServer { name } => {
            // 要起进程、等握手（HTTP 的还要 block_on），不能占异步 worker。
            let app = app.clone();
            let tested = tokio::task::spawn_blocking(move || app.test_mcp_server(&name))
                .await
                .map_err(|error| {
                    RpcError::internal(format!("试起 MCP server 的任务失败：{error}"))
                })?
                .map_err(RpcError::from)?;
            ok(&tested)
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
            let state = match daemon {
                Some(context) => {
                    let router = context.router.clone();
                    app.set_planning_mode_with_cleanup(&profile.id, mode, move |root| {
                        router
                            .terminate_workspace(root)
                            .map_err(|error| error.to_string())
                    })
                    .await?
                }
                None => app.set_planning_mode_local(&profile.id, mode).await?,
            };
            ok(&state)
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
        R::ServedTools => {
            let Some((build, tools)) = gld_host::hub_runtime::served_tools().await else {
                return Err(RpcError::from(gld_core::AppError::Message(
                    "服务没在跑，没有正在给客户端的工具表；`gld start` 之后再查".into(),
                )));
            };
            Ok(serde_json::json!({
                "build": build,
                "surface": gld_core::tools::registry::surface_digest(&tools),
                "tools": tools
            }))
        }
        R::CallTool { target, name, args } => {
            let profile = resolve(app, &target)?;
            // 工具内核是同步 API，内部用 block_on 驱动子进程，不能占用异步 worker 线程。
            let app = app.clone();
            let id = profile.id;
            let router = daemon.map(|context| context.router.clone());
            tokio::task::spawn_blocking(move || match router {
                Some(router) => {
                    // 与旧 App 入口相同，在阻塞线程里重读当前目录，不沿用更早的 selector 快照。
                    let root = app.tool_workspace_root(&id)?;
                    router
                        .local_tool(&id, &root, &name, &args)
                        .map_err(|error| gld_core::AppError::Message(error.to_string()))
                }
                None => app.call_tool_local(&id, &name, args),
            })
            .await
            .map_err(|error| RpcError::internal(format!("工具调用任务失败：{error}")))?
            .map_err(RpcError::from)
        }

        // ---- 观察 ----
        R::Doctor => {
            let mut diagnosis = app.doctor(listeners).await?;
            // 直连模式跑在命令行进程里，查出来的不是守护进程的会话和授权。
            if daemon.is_some() && app.uses_screen_capture()? {
                diagnosis.checks.push(probe_desktop().await?.check);
            }
            ok(&diagnosis)
        }
        R::DesktopProbe { only_if_used } => {
            if only_if_used && !app.uses_screen_capture()? {
                return ok(&Value::Null);
            }
            ok(&probe_desktop().await?)
        }
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
            ok(&app.usage_stats(listeners, &profile.id)?)
        }
        R::AgentContext { target } => {
            let profile = resolve(app, &target)?;
            ok(&app.agent_context(&profile.id)?)
        }
        R::GlobalAgentContext => ok(&app.global_agent_context()),

        // ---- 升级 ----
        R::UpgradeStatus => upgrade(daemon, |router| Ok(router.status())).await,
        R::UpgradePrepare { exe, timeout_secs } => {
            upgrade(daemon, move |router| {
                router.prepare(&exe, std::time::Duration::from_secs(timeout_secs))
            })
            .await
        }
        R::UpgradeSwitch => upgrade(daemon, |router| router.switch()).await,
        R::UpgradeAbort => upgrade(daemon, |router| router.abort()).await,
        // ---- 软件 ----
    }
}

fn agent_reply(
    reply: Result<Value, gld_core::generation::CallError>,
) -> gld_core::AppResult<gld_core::agents::store::AgentTask> {
    let value = reply.map_err(|error| gld_core::AppError::Message(error.to_string()))?;
    Ok(serde_json::from_value(value)?)
}

/// 这些操作要等子进程应答，放阻塞线程。
async fn upgrade(
    daemon: Option<&DaemonContext>,
    run: impl FnOnce(&gld_host::generation::Router) -> Result<Value, String> + Send + 'static,
) -> Result<Value, RpcError> {
    let Some(context) = daemon else {
        return Err(RpcError::protocol("当前不是守护进程，没有执行代可操作"));
    };
    let router = context.router.clone();
    tokio::task::spawn_blocking(move || run(&router))
        .await
        .map_err(|error| RpcError::internal(format!("升级操作失败：{error}")))?
        .map_err(RpcError::app)
}

async fn probe_desktop() -> Result<gld_core::app::DesktopStatus, RpcError> {
    let executable = std::env::current_exe()
        .map_err(|error| RpcError::internal(format!("找不到自己的路径：{error}")))?;
    tokio::task::spawn_blocking(move || gld_core::app::desktop_status(&executable))
        .await
        .map_err(|error| RpcError::internal(format!("截屏探测失败：{error}")))
}

async fn change_node_credential(
    machine_id: String,
    credential: Option<String>,
) -> Result<Value, RpcError> {
    let changed = tokio::task::spawn_blocking(move || match credential {
        Some(credential) => gld_core::nodes::config::rotate_credential(&machine_id, &credential),
        None => gld_core::nodes::config::revoke_machine(&machine_id),
    })
    .await
    .map_err(|_| RpcError::internal("节点凭据更新任务失败"))?
    .map_err(RpcError::app)?;
    close_stale_connection(changed.machine_id.clone(), "节点凭据已更新").await?;
    ok(&changed)
}

async fn remove_node(machine: String) -> Result<Value, RpcError> {
    let removed =
        tokio::task::spawn_blocking(move || gld_core::nodes::config::remove_machine(&machine))
            .await
            .map_err(|_| RpcError::internal("节点登记删除任务失败"))?
            .map_err(RpcError::app)?;
    // 撤销时收连接失败、当时没管的话，连接还挂在这台机器上，这是最后的机会。
    close_stale_connection(removed.machine_id.clone(), "节点登记已删除").await?;
    let mut result = ok(&removed)?;
    result["removed"] = true.into();
    Ok(result)
}

/// 登记已经落盘之后才调，所以失败时要说清楚变更本身已经生效。
async fn close_stale_connection(machine_id: String, done: &str) -> Result<(), RpcError> {
    if let Some(nodes) = gld_host::hub_runtime::nodes().await {
        tokio::task::spawn_blocking(move || nodes.close_stale(&machine_id))
            .await
            .map_err(|_| RpcError::internal(format!("{done}，但旧连接收回任务失败")))?
            .map_err(|error| RpcError::app(format!("{done}，但旧连接收回失败：{error}")))?;
    }
    Ok(())
}

fn resolve(app: &App, target: &WorkspaceTarget) -> Result<WorkspaceProfile, RpcError> {
    app.resolve_workspace(target).map_err(RpcError::from)
}

fn ok<T: Serialize>(value: &T) -> Result<Value, RpcError> {
    serde_json::to_value(value).map_err(|error| RpcError::internal(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::agent_reply;
    use gld_core::generation::CallError;
    use gld_core::AppError;

    #[test]
    fn routed_agent_replies_preserve_transport_and_json_errors() {
        let lost = agent_reply(Err(CallError::Lost)).expect_err("transport error");
        assert!(matches!(&lost, AppError::Message(_)));
        assert_eq!(lost.to_string(), CallError::Lost.to_string());
        let malformed = agent_reply(Ok(serde_json::json!({}))).expect_err("invalid task");
        assert!(matches!(&malformed, AppError::Json(_)));
        assert!(malformed.to_string().starts_with("json error: "));
    }
}

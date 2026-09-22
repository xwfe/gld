//! 项目自己的 GPT Actions（自定义 GPT 导入 OpenAPI 用）。
//!
//! RFC-0004 之后 MCP 只有一个服务，Actions 是唯一还按项目起的线路：聚合入口没有
//! OpenAPI 版，自定义 GPT 导入的也是一个项目的文档。命令行里它挂在
//! `start` / `stop` / `restart` / `share` / `logs` / `health` 的 `-s actions` 上。

use gld_core::app::{WorkspaceTarget, WorkspaceUpdate};
use gld_core::runtime::ServiceKind;
use gld_core::tunnel::{TunnelServiceKind, TunnelStatus};
use gld_core::workspace::{RuntimeStatusDto, WorkspaceProfile};
use gld_daemon::Request;
use serde_json::{json, Value};

use super::{share, Ctx};
use crate::cli::{ServiceArg, StartArgs, TunnelService};
use crate::error::{CliError, CliResult};
use crate::output::{mask, or_dash};

/// 起这个项目的 Actions；`-s actions` 时 `--port` / `--tunnel` 也归它。
pub async fn start(ctx: &mut Ctx, profile: &WorkspaceProfile, args: &StartArgs) -> CliResult {
    let target = WorkspaceTarget::selector(profile.id.clone());
    // `-s all` 时端口和公网入口给的是服务，只有 `-s actions` 才归这条线路。
    let own = matches!(args.service, Some(ServiceArg::Actions));
    if let (true, Some(port)) = (own, args.port) {
        if profile.actions.local_port != port {
            let update: WorkspaceUpdate = ctx
                .backend
                .call_typed(Request::SetWorkspaceFields {
                    target: target.clone(),
                    assignments: vec![("actions.port".into(), port.to_string())],
                })
                .await?;
            if let Some(failure) = update.restart_failures.first() {
                return Err(CliError::new(format!(
                    "端口配置已保存，但服务重启失败：{}",
                    failure.error
                )));
            }
        }
    }
    let tunnel = if own { args.tunnel.as_ref() } else { None };
    if let Some(spec) = tunnel {
        share::configure(
            ctx,
            &target,
            profile,
            spec,
            args.subdomain.as_deref(),
            args.tunnel_token.as_deref(),
            TunnelService::Actions,
        )
        .await?;
    }
    let status: RuntimeStatusDto = ctx
        .backend
        .call_typed(Request::StartService {
            target: target.clone(),
            kind: ServiceKind::Actions,
        })
        .await?;
    if let Some(spec) = tunnel {
        share::ensure_tunnel_up(ctx, &target, spec, TunnelService::Actions).await?;
    } else {
        share::verify_named_public(ctx, &target, TunnelService::Actions).await?;
    }
    if !ctx.out.json_or(&status) {
        print_status(ctx, &profile.name, &status);
    }
    Ok(())
}

pub async fn stop(ctx: &mut Ctx, profile: &WorkspaceProfile) -> CliResult {
    let status: RuntimeStatusDto = ctx
        .backend
        .call_typed(Request::StopService {
            target: WorkspaceTarget::selector(profile.id.clone()),
            kind: ServiceKind::Actions,
        })
        .await?;
    if !ctx.out.json_or(&status) {
        print_status(ctx, &profile.name, &status);
    }
    Ok(())
}

pub async fn restart(ctx: &mut Ctx, profile: &WorkspaceProfile) -> CliResult {
    let status: RuntimeStatusDto = ctx
        .backend
        .call_typed(Request::RestartService {
            target: WorkspaceTarget::selector(profile.id.clone()),
            kind: ServiceKind::Actions,
        })
        .await?;
    if !ctx.out.json_or(&status) {
        print_status(ctx, &profile.name, &status);
    }
    Ok(())
}

fn print_status(ctx: &Ctx, name: &str, status: &RuntimeStatusDto) {
    ctx.out.line(format!(
        "{name} GPT Actions  {}  {}",
        ctx.out.state(&status.state),
        status.local_message
    ));
    if matches!(status.state.as_str(), "running" | "starting") {
        if !status.local_endpoint.is_empty() {
            ctx.out.line(format!("  本地 {}", status.local_endpoint));
        }
        if !is_loopback_url(&status.public_endpoint) && !status.public_endpoint.is_empty() {
            ctx.out
                .line(format!("  OpenAPI {}", status.public_endpoint));
        }
    }
}

/// Actions 没配公网地址时会回退成本地地址；在"公网"那栏里显示它只会误导。
fn is_loopback_url(url: &str) -> bool {
    url.starts_with("http://127.0.0.1") || url.starts_with("http://localhost")
}

/// 这个项目的 Actions 在不在用、用的什么地址和凭据。没在用（没在跑、也没配过
/// 公网入口）是 `null`——只用 MCP 的人不该在每个项目下面看到一块与他无关的东西。
pub async fn detail(ctx: &mut Ctx, profile: &WorkspaceProfile, reveal: bool) -> CliResult<Value> {
    let target = WorkspaceTarget::selector(profile.id.clone());
    let status: RuntimeStatusDto = ctx
        .backend
        .call_typed(Request::ServiceStatus {
            target: target.clone(),
            kind: ServiceKind::Actions,
        })
        .await?;
    let actions = &profile.actions;
    let configured = actions.tunnel_type != "none"
        || !actions.public_url.trim().is_empty()
        || actions.use_global_gateway;
    if status.state == "stopped" && !configured {
        return Ok(Value::Null);
    }
    let tunnel: TunnelStatus = ctx
        .backend
        .call_typed(Request::TunnelStatus {
            target: target.clone(),
            kind: TunnelServiceKind::Actions,
        })
        .await?;
    let key = if actions.auth_type == "api_key" {
        let request = if actions.use_shared_secrets {
            Request::SharedSecret {
                key: "actions_api_key".into(),
            }
        } else {
            Request::WorkspaceSecret {
                target,
                key: "actions_api_key".into(),
            }
        };
        let value: Option<String> = ctx.backend.call_typed(request).await?;
        value.map(|value| if reveal { value } else { mask(&value) })
    } else {
        None
    };
    Ok(json!({
        "state": status.state,
        "localUrl": status.local_endpoint,
        "openapiUrl": status.public_endpoint,
        "auth": actions.auth_type,
        "apiKey": key,
        "tunnel": {
            "config": tunnel_label(profile),
            "state": tunnel.state,
            "pid": tunnel.tunnel_pid,
        },
    }))
}

pub fn print_detail(ctx: &Ctx, detail: &Value) {
    if detail.is_null() {
        return;
    }
    let text = |key: &str| detail[key].as_str().unwrap_or_default().to_string();
    let out = ctx.out;
    out.line("");
    out.line(out.bold("GPT Actions（自定义 GPT 导入 OpenAPI）"));
    let openapi = text("openapiUrl");
    out.kv(&[
        ("  状态", out.state(&text("state"))),
        ("  本地地址", or_dash(&text("localUrl"))),
        (
            "  OpenAPI 地址",
            if is_loopback_url(&openapi) {
                format!("{openapi}（未配置公网，仅本机）")
            } else {
                or_dash(&openapi)
            },
        ),
        (
            "  隧道",
            format!(
                "{}  {}",
                detail["tunnel"]["config"].as_str().unwrap_or_default(),
                out.state(detail["tunnel"]["state"].as_str().unwrap_or_default())
            ),
        ),
        ("  认证方式", text("auth")),
        (
            "  API Key",
            detail["apiKey"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| "-".into()),
        ),
    ]);
}

/// 配置里的 Actions 公网入口长什么样（不含运行状态）。
fn tunnel_label(profile: &WorkspaceProfile) -> String {
    let actions = &profile.actions;
    if actions.use_global_gateway {
        return "全局入口（/w/<id>/actions）".into();
    }
    match actions.tunnel_type.as_str() {
        "frp" => format!("frp（子域名 {}）", or_dash(&actions.frp_subdomain)),
        "cloudflare" => format!("cloudflare（{}）", actions.cloudflare_mode),
        _ if !actions.public_url.trim().is_empty() => "固定地址（自建入口）".into(),
        _ => "none".into(),
    }
}

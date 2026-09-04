use gld_core::app::{ServiceOverview, WorkspaceTarget};
use gld_core::runtime::ServiceKind;
use gld_core::workspace::{RuntimeStatusDto, WorkspaceProfile};
use gld_daemon::lifecycle::{self, DaemonProbe};
use gld_daemon::Request;
use serde_json::json;

use super::Ctx;
use crate::cli::{ConnectArgs, ServiceArg, ServiceArgs};
use crate::error::CliResult;
use crate::output::{human_duration, mask, or_dash};

fn kinds(args: &ServiceArgs, default: ServiceArg) -> Vec<ServiceKind> {
    match args.service.unwrap_or(default) {
        ServiceArg::Mcp => vec![ServiceKind::Mcp],
        ServiceArg::Actions => vec![ServiceKind::Actions],
        ServiceArg::All => vec![ServiceKind::Mcp, ServiceKind::Actions],
    }
}

pub async fn start(ctx: &mut Ctx, args: ServiceArgs) -> CliResult {
    let mut results = Vec::new();
    for kind in kinds(&args, ServiceArg::Mcp) {
        let status: RuntimeStatusDto = ctx
            .backend
            .call_typed(Request::StartService {
                target: ctx.target.clone(),
                kind,
            })
            .await?;
        if !ctx.out.json {
            print_service_status(ctx, kind, &status);
        }
        results.push(json!({ "service": kind, "status": status }));
    }
    if !ctx.out.json_or(&results) {
        ctx.out
            .note("守护进程在后台持有服务；`gld connect` 查看连接信息，`gld stop` 停止。");
    }
    Ok(())
}

pub async fn stop(ctx: &mut Ctx, args: ServiceArgs) -> CliResult {
    if !ctx.backend.is_remote() {
        if !ctx.out.json_or(&Vec::<serde_json::Value>::new()) {
            ctx.out.line("守护进程未运行，没有需要停止的服务。");
        }
        return Ok(());
    }
    let mut results = Vec::new();
    for kind in kinds(&args, ServiceArg::All) {
        let current: RuntimeStatusDto = ctx
            .backend
            .call_typed(Request::ServiceStatus {
                target: ctx.target.clone(),
                kind,
            })
            .await?;
        if current.state == "stopped" && args.service.is_none() {
            continue;
        }
        let status: RuntimeStatusDto = ctx
            .backend
            .call_typed(Request::StopService {
                target: ctx.target.clone(),
                kind,
            })
            .await?;
        if !ctx.out.json {
            print_service_status(ctx, kind, &status);
        }
        results.push(json!({ "service": kind, "status": status }));
    }
    if !ctx.out.json_or(&results) && results.is_empty() {
        ctx.out.line("该工作区没有正在运行的服务。");
    }
    Ok(())
}

pub async fn restart(ctx: &mut Ctx, args: ServiceArgs) -> CliResult {
    let selected = kinds(&args, ServiceArg::All);
    let mut targets = Vec::new();
    if args.service.is_none() && ctx.backend.is_remote() {
        // 默认只重启正在运行的；什么都没在跑就按 start 的默认（MCP）处理。
        for kind in &selected {
            let current: RuntimeStatusDto = ctx
                .backend
                .call_typed(Request::ServiceStatus {
                    target: ctx.target.clone(),
                    kind: *kind,
                })
                .await?;
            if current.state != "stopped" {
                targets.push(*kind);
            }
        }
        if targets.is_empty() {
            targets.push(ServiceKind::Mcp);
        }
    } else if args.service.is_none() {
        targets.push(ServiceKind::Mcp);
    } else {
        targets = selected;
    }
    let mut results = Vec::new();
    for kind in targets {
        let status: RuntimeStatusDto = ctx
            .backend
            .call_typed(Request::RestartService {
                target: ctx.target.clone(),
                kind,
            })
            .await?;
        if !ctx.out.json {
            print_service_status(ctx, kind, &status);
        }
        results.push(json!({ "service": kind, "status": status }));
    }
    ctx.out.json_or(&results);
    Ok(())
}

fn print_service_status(ctx: &Ctx, kind: ServiceKind, status: &RuntimeStatusDto) {
    let label = match kind {
        ServiceKind::Mcp => "MCP",
        ServiceKind::Actions => "Actions",
    };
    ctx.out.line(format!(
        "{label:8} {}  {}",
        ctx.out.state(&status.state),
        status.local_message
    ));
    if !matches!(status.state.as_str(), "running" | "starting") {
        return;
    }
    if !status.local_endpoint.is_empty() {
        ctx.out
            .line(format!("{:8} 本地 {}", "", status.local_endpoint));
    }
    if !status.public_endpoint.is_empty() && !is_loopback_url(&status.public_endpoint) {
        ctx.out
            .line(format!("{:8} 公网 {}", "", status.public_endpoint));
    }
}

/// Actions 没配公网地址时会回退成本地地址；在“公网”栏里显示它只会误导。
fn is_loopback_url(url: &str) -> bool {
    url.starts_with("http://127.0.0.1") || url.starts_with("http://localhost")
}

pub async fn status(ctx: &mut Ctx) -> CliResult {
    let daemon = lifecycle::probe(ctx.backend.paths()).await;
    let overview: Vec<ServiceOverview> = ctx.backend.call_typed(Request::Overview).await?;

    if ctx.explicit_workspace {
        let profile: WorkspaceProfile = ctx
            .backend
            .call_typed(Request::ResolveWorkspace {
                target: ctx.target.clone(),
            })
            .await?;
        let item = overview.iter().find(|item| item.workspace.id == profile.id);
        if ctx
            .out
            .json_or(&json!({ "daemon": daemon_json(&daemon), "workspace": item }))
        {
            return Ok(());
        }
        print_daemon_line(ctx, &daemon);
        if let Some(item) = item {
            print_workspace_detail(ctx, item);
        }
        return Ok(());
    }

    if ctx
        .out
        .json_or(&json!({ "daemon": daemon_json(&daemon), "workspaces": overview }))
    {
        return Ok(());
    }
    print_daemon_line(ctx, &daemon);
    if overview.is_empty() {
        ctx.out
            .line("还没有工作区。执行 `gld workspace add <项目目录>` 添加一个。");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = overview
        .iter()
        .map(|item| {
            vec![
                item.workspace.name.clone(),
                format!(
                    "{} :{}",
                    ctx.out.state(&item.mcp.state),
                    item.workspace.runtime.local_port
                ),
                tunnel_cell(ctx, &item.mcp_tunnel.state, &item.mcp.public_endpoint),
                format!(
                    "{} :{}",
                    ctx.out.state(&item.actions.state),
                    item.workspace.actions.local_port
                ),
                tunnel_cell(
                    ctx,
                    &item.actions_tunnel.state,
                    &item.actions.public_endpoint,
                ),
            ]
        })
        .collect();
    ctx.out.table(
        &["工作区", "MCP", "MCP 公网", "Actions", "Actions 公网"],
        &rows,
    );
    ctx.out.line("");
    ctx.out.line(
        ctx.out
            .dim("详情：gld status -w <工作区>；连接信息：gld connect -w <工作区>"),
    );
    Ok(())
}

pub async fn ps(ctx: &mut Ctx) -> CliResult {
    let overview: Vec<ServiceOverview> = ctx.backend.call_typed(Request::Overview).await?;
    let mut rows = Vec::new();
    let mut items = Vec::new();
    for item in &overview {
        for (kind, status, tunnel) in [
            ("mcp", &item.mcp, &item.mcp_tunnel),
            ("actions", &item.actions, &item.actions_tunnel),
        ] {
            if status.state == "stopped" {
                continue;
            }
            items.push(json!({ "workspace": item.workspace.id, "name": item.workspace.name, "service": kind, "status": status, "tunnel": tunnel }));
            rows.push(vec![
                item.workspace.name.clone(),
                kind.to_string(),
                ctx.out.state(&status.state),
                status.local_endpoint.clone(),
                if is_loopback_url(&status.public_endpoint) {
                    "-".into()
                } else {
                    or_dash(&status.public_endpoint)
                },
                tunnel.state.clone(),
            ]);
        }
    }
    if ctx.out.json_or(&items) {
        return Ok(());
    }
    if rows.is_empty() {
        ctx.out.line("没有正在运行的服务。");
        return Ok(());
    }
    ctx.out.table(
        &["工作区", "服务", "状态", "本地地址", "公网地址", "隧道"],
        &rows,
    );
    Ok(())
}

fn tunnel_cell(ctx: &Ctx, tunnel_state: &str, public_endpoint: &str) -> String {
    if public_endpoint.is_empty() || is_loopback_url(public_endpoint) {
        return ctx.out.dim("-");
    }
    match tunnel_state {
        "running" => public_endpoint.to_string(),
        other => format!("{} {}", ctx.out.dim(&format!("({other})")), public_endpoint),
    }
}

fn daemon_json(probe: &DaemonProbe) -> serde_json::Value {
    match probe {
        DaemonProbe::Running(info) => {
            json!({ "running": true, "pid": info.pid, "uptime_secs": info.uptime_secs })
        }
        DaemonProbe::Unresponsive(record) => {
            json!({ "running": false, "unresponsive": true, "pid": record.pid })
        }
        _ => json!({ "running": false }),
    }
}

fn print_daemon_line(ctx: &Ctx, probe: &DaemonProbe) {
    match probe {
        DaemonProbe::Running(info) => ctx.out.line(format!(
            "守护进程 {}  pid {}  已运行 {}",
            ctx.out.green("运行中"),
            info.pid,
            human_duration(info.uptime_secs)
        )),
        DaemonProbe::Unresponsive(record) => ctx.out.line(format!(
            "守护进程 {}  pid {}（存在但不响应）",
            ctx.out.red("异常"),
            record.pid
        )),
        _ => ctx.out.line(format!(
            "守护进程 {}  （所有服务均已停止；gld start 会自动拉起）",
            ctx.out.dim("未运行")
        )),
    }
    ctx.out.line("");
}

fn print_workspace_detail(ctx: &Ctx, item: &ServiceOverview) {
    let ws = &item.workspace;
    ctx.out
        .line(ctx.out.bold(&format!("{}  ({})", ws.name, ws.id)));
    ctx.out.line(format!("路径  {}", ws.path));
    ctx.out.line("");
    for (label, status, tunnel, port) in [
        ("MCP", &item.mcp, &item.mcp_tunnel, ws.runtime.local_port),
        (
            "Actions",
            &item.actions,
            &item.actions_tunnel,
            ws.actions.local_port,
        ),
    ] {
        ctx.out.line(ctx.out.bold(label));
        ctx.out.kv(&[
            (
                "  状态",
                format!("{}  {}", ctx.out.state(&status.state), status.local_message),
            ),
            ("  端口", port.to_string()),
            ("  本地地址", or_dash(&status.local_endpoint)),
            ("  公网地址", or_dash(&status.public_endpoint)),
            (
                "  隧道",
                format!(
                    "{}{}",
                    tunnel.state,
                    tunnel
                        .tunnel_pid
                        .map(|pid| format!("（pid {pid}）"))
                        .unwrap_or_default()
                ),
            ),
        ]);
        ctx.out.line("");
    }
}

/// 给 AI 客户端用的连接信息。
pub async fn connect(ctx: &mut Ctx, args: ConnectArgs) -> CliResult {
    let profile: WorkspaceProfile = ctx
        .backend
        .call_typed(Request::ResolveWorkspace {
            target: ctx.target.clone(),
        })
        .await?;
    let target = WorkspaceTarget::selector(profile.id.clone());
    let mcp: RuntimeStatusDto = ctx
        .backend
        .call_typed(Request::ServiceStatus {
            target: target.clone(),
            kind: ServiceKind::Mcp,
        })
        .await?;
    let actions: RuntimeStatusDto = ctx
        .backend
        .call_typed(Request::ServiceStatus {
            target: target.clone(),
            kind: ServiceKind::Actions,
        })
        .await?;

    let mut credentials: Vec<(&str, String)> = Vec::new();
    match profile.auth.auth_type.as_str() {
        "oauth" => {
            let client_id = if profile.auth.use_shared_secrets {
                secret(ctx, &target, "oauth_client_id", true).await?
            } else {
                Some(profile.auth.oauth_client_id.clone())
            };
            let password = secret(
                ctx,
                &target,
                "oauth_password",
                profile.auth.use_shared_secrets,
            )
            .await?;
            credentials.push(("OAuth Client ID", client_id.unwrap_or_default()));
            credentials.push(("授权口令 (oauth_password)", password.unwrap_or_default()));
        }
        "bearer" => {
            let token = secret(
                ctx,
                &target,
                "bearer_token",
                profile.auth.use_shared_secrets,
            )
            .await?;
            credentials.push(("Bearer Token", token.unwrap_or_default()));
        }
        _ => {}
    }
    let actions_key = if profile.actions.auth_type == "api_key" {
        secret(
            ctx,
            &target,
            "actions_api_key",
            profile.actions.use_shared_secrets,
        )
        .await?
    } else {
        None
    };

    let payload = json!({
        "workspace": { "id": profile.id, "name": profile.name, "path": profile.path },
        "mcp": {
            "state": mcp.state,
            "auth": profile.auth.auth_type,
            "local_url": mcp.local_endpoint,
            "public_url": mcp.public_endpoint,
            "credentials": credentials.iter().map(|(k, v)| json!({ "label": k, "value": if args.reveal { v.clone() } else { mask(v) } })).collect::<Vec<_>>(),
        },
        "actions": {
            "state": actions.state,
            "auth": profile.actions.auth_type,
            "local_url": actions.local_endpoint,
            "openapi_url": actions.public_endpoint,
            "api_key": actions_key.as_ref().map(|v| if args.reveal { v.clone() } else { mask(v) }),
        }
    });
    if ctx.out.json_or(&payload) {
        return Ok(());
    }

    let out = ctx.out;
    out.line(out.bold(&format!("工作区 {}  ({})", profile.name, profile.path)));
    out.line("");
    out.line(out.bold("MCP（ChatGPT 连接器 / 其他 MCP 客户端）"));
    let mut rows = vec![
        ("  状态", out.state(&mcp.state)),
        ("  本地地址", or_dash(&mcp.local_endpoint)),
        ("  公网地址", or_dash(&mcp.public_endpoint)),
        ("  认证方式", profile.auth.auth_type.clone()),
    ];
    for (label, value) in &credentials {
        let shown = if args.reveal {
            value.clone()
        } else {
            mask(value)
        };
        rows.push((label, shown));
    }
    out.kv(&rows);
    out.line("");
    out.line(out.bold("GPT Actions（自定义 GPT 导入 OpenAPI）"));
    out.kv(&[
        ("  状态", out.state(&actions.state)),
        ("  本地地址", or_dash(&actions.local_endpoint)),
        (
            "  OpenAPI 地址",
            if is_loopback_url(&actions.public_endpoint) {
                format!("{}（未配置公网，仅本机）", actions.public_endpoint)
            } else {
                or_dash(&actions.public_endpoint)
            },
        ),
        ("  认证方式", profile.actions.auth_type.clone()),
        (
            "  API Key",
            actions_key
                .map(|v| if args.reveal { v } else { mask(&v) })
                .unwrap_or_else(|| "-".into()),
        ),
    ]);
    out.line("");
    if mcp.public_endpoint.is_empty() {
        out.line(out.yellow("还没有公网地址：ChatGPT 需要 HTTPS 公网 /mcp。配置 FRP 或 Cloudflare 后执行 gld tunnel start。"));
    } else {
        out.line("ChatGPT：设置 → 开发人员模式 → 插件 → 新建 MCP，粘贴上面的公网地址，认证方式与此一致。");
    }
    if !args.reveal {
        out.line(out.dim("凭据已脱敏，--reveal 显示明文。"));
    }
    Ok(())
}

async fn secret(
    ctx: &mut Ctx,
    target: &WorkspaceTarget,
    key: &str,
    shared: bool,
) -> CliResult<Option<String>> {
    let request = if shared {
        Request::SharedSecret { key: key.into() }
    } else {
        Request::WorkspaceSecret {
            target: target.clone(),
            key: key.into(),
        }
    };
    ctx.backend.call_typed(request).await
}

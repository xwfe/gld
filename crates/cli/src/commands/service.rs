use std::path::Path;

use gld_core::app::{EnsuredWorkspace, ServiceOverview, WorkspaceCreateOptions, WorkspaceTarget};
use gld_core::runtime::ServiceKind;
use gld_core::tunnel::{TunnelServiceKind, TunnelStatus};
use gld_core::workspace::{RuntimeStatusDto, WorkspaceProfile};
use gld_daemon::lifecycle::{self, DaemonProbe};
use gld_daemon::Request;
use serde_json::json;

use super::{share, Ctx};
use crate::cli::{LsArgs, ServiceArg, ServiceArgs, StartArgs, TunnelService};
use crate::error::{CliError, CliResult};
use crate::output::{human_duration, mask, or_dash, yes_no};

fn kinds(service: Option<ServiceArg>, default: ServiceArg) -> Vec<ServiceKind> {
    match service.unwrap_or(default) {
        ServiceArg::Mcp => vec![ServiceKind::Mcp],
        ServiceArg::Actions => vec![ServiceKind::Actions],
        ServiceArg::All => vec![ServiceKind::Mcp, ServiceKind::Actions],
    }
}

/// 定位工作区；目录没登记过就当场登记。
///
/// `gld start ~/code/x` 和 `gld start`（在还没登记的目录里）都走这里。以前
/// 必须先 `workspace add` 再 `start`——第一条命令唯一的作用就是让第二条别报
/// "当前目录不属于任何工作区"，读者却要先读懂"工作区"这个概念才敢往下走。
///
/// 写了 `-w` 却找不到时不自动登记：那是名字拼错了，凭空建一个新工作区
/// 只会让人对着两个空工作区更迷惑。
pub async fn resolve_or_register(
    ctx: &mut Ctx,
    path: Option<&Path>,
    options: WorkspaceCreateOptions,
) -> CliResult<WorkspaceProfile> {
    if let Some(path) = path {
        if ctx.explicit_workspace {
            return Err(CliError::new(format!(
                "同时给了目录 {} 和 -w {}，不知道该听哪个。去掉其中一个。",
                path.display(),
                ctx.target.selector.clone().unwrap_or_default()
            )));
        }
        return register(ctx, path.to_path_buf(), options).await;
    }
    if ctx.explicit_workspace {
        // -w 写了却没解析到 = 名字拼错了，把原始报错（带候选列表）给用户。
        return ctx
            .backend
            .call_typed(Request::ResolveWorkspace {
                target: ctx.target.clone(),
            })
            .await;
    }
    register(ctx, std::env::current_dir()?, options).await
}

async fn register(
    ctx: &mut Ctx,
    path: std::path::PathBuf,
    options: WorkspaceCreateOptions,
) -> CliResult<WorkspaceProfile> {
    let ensured: EnsuredWorkspace = ctx
        .backend
        .call_typed(Request::EnsureWorkspace { path, options })
        .await?;
    if ensured.created {
        // 自动登记必须让人看见：否则在随手进的目录里敲 gld start，
        // 工作区悄悄多了一个，而用户以为自己起的是别的项目。
        ctx.out.line(format!(
            "已登记工作区「{}」（{}），MCP 端口 {}。",
            ensured.profile.name, ensured.profile.path, ensured.profile.runtime.local_port
        ));
        ctx.out
            .note("登记错了：gld workspace remove -w <名称>（不会动项目文件）。");
    }
    Ok(ensured.profile)
}

pub async fn start(ctx: &mut Ctx, args: StartArgs) -> CliResult {
    let actions_side = matches!(args.service, Some(ServiceArg::Actions));
    // 新登记的工作区直接用指定端口建；已经存在的走下面的 set。
    let options = WorkspaceCreateOptions {
        name: None,
        mcp_port: (!actions_side).then_some(args.port).flatten(),
        actions_port: actions_side.then_some(args.port).flatten(),
    };
    let profile = resolve_or_register(ctx, args.path.as_deref(), options).await?;
    let target = WorkspaceTarget::selector(profile.id.clone());
    let tunnel_service = if actions_side {
        TunnelService::Actions
    } else {
        TunnelService::Mcp
    };

    if let Some(port) = args.port {
        let key = if actions_side {
            "actions.port"
        } else {
            "mcp.port"
        };
        let current = if actions_side {
            profile.actions.local_port
        } else {
            profile.runtime.local_port
        };
        if current != port {
            let _: gld_core::app::WorkspaceUpdate = ctx
                .backend
                .call_typed(Request::SetWorkspaceFields {
                    target: target.clone(),
                    assignments: vec![(key.into(), port.to_string())],
                })
                .await?;
        }
    }

    // 先配公网入口再启动：反过来的话服务刚起来就要为了新配置重启一次。
    if let Some(spec) = &args.tunnel {
        share::configure(
            ctx,
            &target,
            &profile,
            spec,
            args.subdomain.as_deref(),
            tunnel_service,
        )
        .await?;
    } else if let Some(sub) = &args.subdomain {
        return Err(CliError::new(format!(
            "--subdomain {sub} 要配合 --tunnel frp:<配置名> 一起用。"
        )));
    }

    let mut results = Vec::new();
    for kind in kinds(args.service, ServiceArg::Mcp) {
        let status: RuntimeStatusDto = ctx
            .backend
            .call_typed(Request::StartService {
                target: target.clone(),
                kind,
            })
            .await?;
        if !ctx.out.json {
            print_service_status(ctx, kind, &status);
        }
        results.push(json!({ "service": kind, "status": status }));
    }

    if let Some(spec) = &args.tunnel {
        share::ensure_tunnel_up(ctx, &target, spec, tunnel_service).await?;
        // 配了公网入口就是奔着"连上去"来的，直接把地址和凭据摆出来。
        // （`--json` 下这里是唯一一份输出，上面的服务状态不再单独打印。）
        return show_detail(ctx, &target, LsArgs::default()).await;
    }
    if !ctx.out.json_or(&results) {
        ctx.out
            .note("守护进程在后台持有服务；`gld ls` 查看连接信息，`gld stop` 停止。");
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
    for kind in kinds(args.service, ServiceArg::All) {
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
    let selected = kinds(args.service, ServiceArg::All);
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
                status_public_cell(ctx, &item.mcp_tunnel.state, &item.mcp.public_endpoint),
                format!(
                    "{} :{}",
                    ctx.out.state(&item.actions.state),
                    item.workspace.actions.local_port
                ),
                status_public_cell(
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
            .dim("详情：gld status -w <工作区>；连接信息：gld ls -w <工作区>"),
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

/// `gld status` 的"公网"格：地址为主，隧道没跑起来时在前面标出来。
fn status_public_cell(ctx: &Ctx, tunnel_state: &str, public_endpoint: &str) -> String {
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

/// `gld ls`：一个工作区就看详情，多个就先列出来。
///
/// 不指定工作区时的两种情形读者要的东西不一样：在项目目录里敲，想要的是
/// "这个项目的地址和凭据"；在别处敲，想要的是"我都有哪些工作区、谁在跑"。
pub async fn ls(ctx: &mut Ctx, args: LsArgs) -> CliResult {
    if args.all {
        return show_list(ctx).await;
    }
    if ctx.explicit_workspace {
        let target = ctx.target.clone();
        return show_detail(ctx, &target, args).await;
    }
    let resolved: CliResult<WorkspaceProfile> = ctx
        .backend
        .call_typed(Request::ResolveWorkspace {
            target: ctx.target.clone(),
        })
        .await;
    match resolved {
        Ok(profile) => show_detail(ctx, &WorkspaceTarget::selector(profile.id), args).await,
        Err(_) => show_list(ctx).await,
    }
}

/// 所有工作区一览：一行一条正在用的线路，带隧道与认证方式。
async fn show_list(ctx: &mut Ctx) -> CliResult {
    let overview: Vec<ServiceOverview> = ctx.backend.call_typed(Request::Overview).await?;
    if ctx.out.json_or(&overview) {
        return Ok(());
    }
    if overview.is_empty() {
        ctx.out
            .line("还没有工作区。在项目目录里执行 `gld start` 就会自动登记并启动。");
        return Ok(());
    }
    let mut rows = Vec::new();
    for item in &overview {
        let ws = &item.workspace;
        rows.push(vec![
            ws.name.clone(),
            "mcp".into(),
            ctx.out.state(&item.mcp.state),
            or_dash(&item.mcp.local_endpoint),
            public_cell(&item.mcp.public_endpoint),
            tunnel_cell(
                ctx,
                &ws.tunnel.tunnel_type,
                ws.tunnel.use_global_gateway,
                &tunnel_config_label(
                    &ws.tunnel.tunnel_type,
                    &ws.tunnel.cloudflare_mode,
                    &ws.tunnel.frp_subdomain,
                    &ws.tunnel.public_url,
                    ws.tunnel.use_global_gateway,
                ),
                &item.mcp_tunnel.state,
            ),
            ws.auth.auth_type.clone(),
        ]);
        // Actions 是可选的第二条线路，没在跑就不占一行。
        if item.actions.state != "stopped" {
            rows.push(vec![
                ws.name.clone(),
                "actions".into(),
                ctx.out.state(&item.actions.state),
                or_dash(&item.actions.local_endpoint),
                public_cell(&item.actions.public_endpoint),
                tunnel_cell(
                    ctx,
                    &ws.actions.tunnel_type,
                    ws.actions.use_global_gateway,
                    &tunnel_config_label(
                        &ws.actions.tunnel_type,
                        &ws.actions.cloudflare_mode,
                        &ws.actions.frp_subdomain,
                        &ws.actions.public_url,
                        ws.actions.use_global_gateway,
                    ),
                    &item.actions_tunnel.state,
                ),
                ws.actions.auth_type.clone(),
            ]);
        }
    }
    ctx.out.table(
        &[
            "工作区",
            "服务",
            "状态",
            "本地地址",
            "公网地址",
            "隧道",
            "认证",
        ],
        &rows,
    );
    ctx.out.line("");
    ctx.out.line(
        ctx.out
            .dim("详情与凭据：gld ls -w <工作区>；换公网入口：gld upgrade --tunnel <地址>"),
    );
    Ok(())
}

/// 一个工作区的连接信息：地址、认证、凭据、隧道。
pub async fn show_detail(ctx: &mut Ctx, target: &WorkspaceTarget, args: LsArgs) -> CliResult {
    let profile: WorkspaceProfile = ctx
        .backend
        .call_typed(Request::ResolveWorkspace {
            target: target.clone(),
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

    // 隧道状态和服务状态是两回事：服务好好跑着、隧道断了，公网地址照样贴不出去。
    let mcp_tunnel: TunnelStatus = ctx
        .backend
        .call_typed(Request::TunnelStatus {
            target: target.clone(),
            kind: TunnelServiceKind::Mcp,
        })
        .await?;
    let actions_tunnel: TunnelStatus = ctx
        .backend
        .call_typed(Request::TunnelStatus {
            target: target.clone(),
            kind: TunnelServiceKind::Actions,
        })
        .await?;
    let mcp_tunnel_label = tunnel_config_label(
        &profile.tunnel.tunnel_type,
        &profile.tunnel.cloudflare_mode,
        &profile.tunnel.frp_subdomain,
        &profile.tunnel.public_url,
        profile.tunnel.use_global_gateway,
    );
    let actions_tunnel_label = tunnel_config_label(
        &profile.actions.tunnel_type,
        &profile.actions.cloudflare_mode,
        &profile.actions.frp_subdomain,
        &profile.actions.public_url,
        profile.actions.use_global_gateway,
    );

    let payload = json!({
        "workspace": { "id": profile.id, "name": profile.name, "path": profile.path },
        "mcp": {
            "state": mcp.state,
            "auth": profile.auth.auth_type,
            "local_url": mcp.local_endpoint,
            "public_url": mcp.public_endpoint,
            "credentials": credentials.iter().map(|(k, v)| json!({ "label": k, "value": if args.reveal { v.clone() } else { mask(v) } })).collect::<Vec<_>>(),
            "tunnel": { "config": mcp_tunnel_label, "state": mcp_tunnel.state, "pid": mcp_tunnel.tunnel_pid },
        },
        "actions": {
            "state": actions.state,
            "auth": profile.actions.auth_type,
            "local_url": actions.local_endpoint,
            "openapi_url": actions.public_endpoint,
            "api_key": actions_key.as_ref().map(|v| if args.reveal { v.clone() } else { mask(v) }),
            "tunnel": { "config": actions_tunnel_label, "state": actions_tunnel.state, "pid": actions_tunnel.tunnel_pid },
        }
    });
    if ctx.out.json_or(&payload) {
        return Ok(());
    }

    let out = ctx.out;
    out.line(out.bold(&format!("工作区 {}  ({})", profile.name, profile.path)));
    out.line("");
    out.line(out.bold("MCP（ChatGPT 连接器 / 其他 MCP 客户端）"));
    let mut rows: Vec<(String, String)> = vec![
        ("  状态".into(), out.state(&mcp.state)),
        ("  本地地址".into(), or_dash(&mcp.local_endpoint)),
        ("  公网地址".into(), or_dash(&mcp.public_endpoint)),
        (
            "  隧道".into(),
            tunnel_detail(
                ctx,
                &profile.tunnel.tunnel_type,
                profile.tunnel.use_global_gateway,
                &mcp_tunnel_label,
                &mcp_tunnel,
            ),
        ),
        ("  认证方式".into(), profile.auth.auth_type.clone()),
    ];
    for (label, value) in &credentials {
        let shown = if args.reveal {
            value.clone()
        } else {
            mask(value)
        };
        rows.push((format!("  {label}"), shown));
    }
    rows.push((
        "  共享密钥池".into(),
        yes_no(profile.auth.use_shared_secrets).to_string(),
    ));
    rows.push(("  工具集".into(), profile.runtime.tool_profile.clone()));
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
        (
            "  隧道",
            tunnel_detail(
                ctx,
                &profile.actions.tunnel_type,
                profile.actions.use_global_gateway,
                &actions_tunnel_label,
                &actions_tunnel,
            ),
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
        out.line(out.yellow("还没有公网地址：ChatGPT 只能连公网 HTTPS。一条命令拿一个：gld share"));
    } else {
        out.line("ChatGPT：设置 → 开发人员模式 → 插件 → 新建 MCP，粘贴上面的公网地址，认证方式与此一致。");
    }
    if !args.reveal {
        out.line(out.dim("凭据已脱敏，--reveal 显示明文。"));
    }
    Ok(())
}

/// 配置里的公网入口长什么样（不含运行状态）。
///
/// `gld ls`、`gld workspace show` 都用它，措辞只有一处。这里不重复地址本身——
/// 它就在同一屏的"公网地址"那一行 / 那一列里。
pub fn tunnel_config_label(
    kind: &str,
    cloudflare_mode: &str,
    subdomain: &str,
    public_url: &str,
    gateway: bool,
) -> String {
    if gateway {
        return "全局入口（/w/<id>）".into();
    }
    match kind {
        "frp" => format!("frp（子域名 {}）", or_dash(subdomain)),
        "cloudflare" => format!("cloudflare（{cloudflare_mode}）"),
        _ if !public_url.trim().is_empty() => "固定地址（自建入口）".into(),
        _ => "none".into(),
    }
}

/// 详情里的隧道一行：配置 + 运行状态 + pid。
///
/// 只有 frp / cloudflare 在本机开进程。自建反代（固定地址）和全局入口不开，
/// 给它们标一个 `stopped` 只会让人以为哪里坏了，跑去查一个根本不存在的进程。
fn tunnel_detail(
    ctx: &Ctx,
    kind: &str,
    gateway: bool,
    label: &str,
    status: &TunnelStatus,
) -> String {
    if gateway {
        return format!("{label}  {}", ctx.out.dim("（进程属于全局入口）"));
    }
    match kind {
        "frp" | "cloudflare" => {
            let pid = status
                .tunnel_pid
                .map(|pid| format!("，pid {pid}"))
                .unwrap_or_default();
            format!("{label}  {}{pid}", ctx.out.state(&status.state))
        }
        _ if label == "none" => ctx.out.dim("none（没有公网入口）"),
        // 固定地址：label 已经说清了，不必再补一句状态。
        _ => label.to_string(),
    }
}

/// 列表里的隧道一格：配置 + 状态，没配就一个横杠。
///
/// 状态只对本机真的开着进程的隧道有意义，理由同 [`tunnel_detail`]。
fn tunnel_cell(ctx: &Ctx, kind: &str, gateway: bool, label: &str, state: &str) -> String {
    if label == "none" {
        return ctx.out.dim("-");
    }
    if gateway || !matches!(kind, "frp" | "cloudflare") {
        return label.to_string();
    }
    format!("{label} {}", ctx.out.state(state))
}

/// Actions 没配公网地址时会回退成本地地址；把它放进"公网地址"栏只会误导。
fn public_cell(endpoint: &str) -> String {
    if endpoint.is_empty() || is_loopback_url(endpoint) {
        return "-".into();
    }
    endpoint.to_string()
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

//! 服务的起停与查看（RFC-0004：只有一个 MCP 服务，项目都挂在它下面）。
//!
//! 服务在内部仍叫 hub（`Request::Hub*`），命令行里不再出现这个词。项目自己的
//! GPT Actions 还是一个项目一个，那一半在 [`super::actions`]。

use std::path::Path;

use gld_core::app::{
    EnsuredWorkspace, HubMemberDto, HubMembershipChange, HubStatusDto, ServiceOverview,
    WorkspaceCreateOptions, WorkspaceTarget,
};
use gld_core::runtime::ServiceKind;
use gld_core::settings::HubConfig;
use gld_core::workspace::{RuntimeStatusDto, WorkspaceProfile};
use gld_daemon::lifecycle::{self, DaemonProbe};
use gld_daemon::Request;
use serde_json::json;

use super::{actions, share, Ctx};
use crate::cli::{ListArgs, ServiceArg, ServiceArgs, StartArgs, StopArgs};
use crate::error::{CliError, CliResult};
use crate::output::{human_duration, mask, or_dash, yes_no};

/// `-s` 选的是服务、项目的 Actions，还是两个都要。
fn wants(service: Option<ServiceArg>, default: ServiceArg) -> (bool, bool) {
    match service.unwrap_or(default) {
        ServiceArg::Mcp => (true, false),
        ServiceArg::Actions => (false, true),
        ServiceArg::All => (true, true),
    }
}

/// `start` / `share` 没给目录时要不要加当前目录，给了目录就一定加。
///
/// 没给目录时只有两种情况加当前目录：它本来就是（或在）一个项目里，或者一个项目
/// 都还没有（第一次用）。其余情况只起服务——只剩一个服务之后，`gld start` 也是
/// "把服务拉起来"的那条命令，在主目录里随手敲一下不该把整个主目录登记成项目。
///
/// 写了 `-w` 却找不到时不自动登记：那是名字拼错了，凭空建一个新项目只会更迷惑。
pub async fn pick_project(
    ctx: &mut Ctx,
    path: Option<&Path>,
) -> CliResult<Option<WorkspaceProfile>> {
    pick_project_quietly(ctx, path, false).await
}

/// 同 [`pick_project`]；`quiet` 时当前目录没加进来也不提示——`share` 要的是公网
/// 地址，不是加项目，那句提示对它只是噪音。
pub async fn pick_project_quietly(
    ctx: &mut Ctx,
    path: Option<&Path>,
    quiet: bool,
) -> CliResult<Option<WorkspaceProfile>> {
    if let Some(path) = path {
        if ctx.explicit_workspace {
            return Err(CliError::new(format!(
                "同时给了目录 {} 和 -w {}，不知道该听哪个。去掉其中一个。",
                path.display(),
                ctx.target.selector.clone().unwrap_or_default()
            )));
        }
        return register(ctx, super::absolutize(path)?, None)
            .await
            .map(|ensured| Some(ensured.profile));
    }
    if ctx.explicit_workspace {
        // -w 写了却没解析到 = 名字拼错了，把原始报错（带候选列表）给用户。
        return ctx
            .backend
            .call_typed(Request::ResolveWorkspace {
                target: ctx.target.clone(),
            })
            .await
            .map(Some);
    }
    let cwd = std::env::current_dir()?;
    // 登记时存的是规范化过的路径（macOS 上 /var 其实是 /private/var），比之前先对齐。
    let canonical = cwd.canonicalize().unwrap_or_else(|_| cwd.clone());
    let profiles: Vec<WorkspaceProfile> = ctx.backend.call_typed(Request::ListWorkspaces).await?;
    let inside = profiles
        .iter()
        .any(|profile| canonical.starts_with(&profile.path));
    if inside || profiles.is_empty() {
        return register(ctx, cwd, None)
            .await
            .map(|ensured| Some(ensured.profile));
    }
    if !quiet {
        ctx.out.note(format!(
            "当前目录 {} 不是项目，没加进来。要加：gld add .",
            cwd.display()
        ));
    }
    Ok(None)
}

/// 按目录登记（登记即加入服务）；已经登记过就返回那一个。
pub async fn register(
    ctx: &mut Ctx,
    path: std::path::PathBuf,
    name: Option<String>,
) -> CliResult<EnsuredWorkspace> {
    let ensured: EnsuredWorkspace = ctx
        .backend
        .call_typed(Request::EnsureWorkspace {
            path,
            options: WorkspaceCreateOptions {
                name,
                ..WorkspaceCreateOptions::default()
            },
        })
        .await?;
    if ensured.created {
        // 自动登记必须让人看见：否则在随手进的目录里敲 gld start，
        // 项目悄悄多了一个，而用户以为自己起的是别的。
        ctx.out.line(format!(
            "已加入项目「{}」（{}）。",
            ensured.profile.name, ensured.profile.path
        ));
        // 措辞是「不想要它的话」而不是「登记错了」：后者读起来像在报错，
        // 而这一步其实是成功的——用户反馈说「为啥总提示登记错了，但又成功了」。
        ctx.out.note(format!(
            "不想要它的话：gld rm {}（只删 gld 这边的配置，项目文件不动）。",
            ensured.profile.name
        ));
    }
    Ok(ensured)
}

/// 老数据里登记了但不在服务里的项目，这次加进来，并逐个说出来。
async fn join_leftovers(ctx: &mut Ctx) -> CliResult {
    let joined: Vec<HubMemberDto> = ctx.backend.call_typed(Request::HubJoinAll).await?;
    if !joined.is_empty() {
        ctx.out.line(format!(
            "这些项目以前登记了但不在服务里，现在加进来了：{}",
            names(&joined)
        ));
    }
    Ok(())
}

pub async fn start(ctx: &mut Ctx, args: StartArgs) -> CliResult {
    let (service, actions_too) = wants(args.service, ServiceArg::Mcp);
    if !service {
        // 只起项目的 Actions：这条线路还是一个项目一个。
        let profile = match pick_project(ctx, args.path.as_deref()).await? {
            Some(profile) => profile,
            None => resolve_current(ctx).await?,
        };
        return actions::start(ctx, &profile, &args).await;
    }

    let project = pick_project(ctx, args.path.as_deref()).await?;
    join_leftovers(ctx).await?;
    if let Some(port) = args.port {
        update_service(ctx, |config| {
            config.local_port = port;
            Ok(())
        })
        .await?;
    }
    // 先配公网入口再启动：反过来的话服务刚起来就要为了新配置重启一次。
    if let Some(spec) = &args.tunnel {
        share::configure_service(
            ctx,
            spec,
            args.subdomain.as_deref(),
            args.tunnel_token.as_deref(),
        )
        .await?;
    } else if let Some(sub) = &args.subdomain {
        return Err(CliError::new(format!(
            "--subdomain {sub} 要配合 --tunnel frp:<配置名> 一起用。"
        )));
    }

    // 已经在跑就不碰它：重复敲 start 不该掉客户端连接，临时地址也不该换。
    // （上面改了端口或公网入口的话，那一步已经按新配置重启过。）
    let status: HubStatusDto = ctx.backend.call_typed(Request::HubEnsureStarted).await?;
    if actions_too {
        if let Some(profile) = &project {
            actions::start(ctx, profile, &args).await?;
        }
    }
    if args.tunnel.is_some() {
        share::ensure_service_public(ctx, &status).await?;
        // 配了公网入口就是奔着"连上去"来的，直接把地址和凭据摆出来。
        return show_service(ctx, false).await;
    }
    share::verify_named_service_public(ctx, &status).await?;
    if ctx.out.json_or(&status) {
        return Ok(());
    }
    print_endpoints(ctx, &status);
    if !status.tunnel_error.is_empty() {
        ctx.out.line(format!(
            "{} 公网入口没起来：{}",
            ctx.out.red("✗"),
            status.tunnel_error
        ));
    }
    ctx.out
        .note("守护进程在后台持有服务；`gld ls` 看地址和凭据，`gld stop` 停止。");
    Ok(())
}

pub async fn stop(ctx: &mut Ctx, args: StopArgs) -> CliResult {
    if !ctx.backend.is_remote() {
        if !ctx.out.json_or(&Vec::<serde_json::Value>::new()) {
            ctx.out.line("守护进程未运行，没有需要停止的服务。");
        }
        return Ok(());
    }
    let (service, actions_too) = wants(args.service, ServiceArg::All);
    if !service {
        let profile = resolve_current(ctx).await?;
        return actions::stop(ctx, &profile).await;
    }
    let status: HubStatusDto = ctx.backend.call_typed(Request::HubStop).await?;
    let mut stopped = vec![json!({ "service": "mcp", "status": status })];
    if !ctx.out.json {
        ctx.out
            .line(format!("MCP 服务  {}", ctx.out.state(&status.state)));
    }
    if actions_too {
        // 各项目自己的线路：GPT Actions，以及 RFC-0004 之前起的、还在跑的单项目 MCP。
        let overview: Vec<ServiceOverview> = ctx.backend.call_typed(Request::Overview).await?;
        for item in &overview {
            for (kind, current) in [
                (ServiceKind::Mcp, &item.mcp),
                (ServiceKind::Actions, &item.actions),
            ] {
                if current.state == "stopped" {
                    continue;
                }
                let status: RuntimeStatusDto = ctx
                    .backend
                    .call_typed(Request::StopService {
                        target: WorkspaceTarget::selector(item.workspace.id.clone()),
                        kind,
                    })
                    .await?;
                if !ctx.out.json {
                    ctx.out.line(format!(
                        "{} {}  {}",
                        item.workspace.name,
                        line_label(kind),
                        ctx.out.state(&status.state)
                    ));
                }
                stopped.push(json!({
                    "workspace": item.workspace.id,
                    "name": item.workspace.name,
                    "service": kind,
                    "status": status,
                }));
            }
        }
    }
    ctx.out.json_or(&stopped);
    Ok(())
}

pub async fn restart(ctx: &mut Ctx, args: ServiceArgs) -> CliResult {
    let (service, actions_too) = wants(args.service, ServiceArg::Mcp);
    if actions_too {
        let profile = resolve_current(ctx).await?;
        actions::restart(ctx, &profile).await?;
    }
    if service {
        let status: HubStatusDto = ctx.backend.call_typed(Request::HubStart).await?;
        if !ctx.out.json_or(&status) {
            print_endpoints(ctx, &status);
        }
    }
    Ok(())
}

pub async fn status(ctx: &mut Ctx) -> CliResult {
    let daemon = lifecycle::probe(ctx.backend.paths()).await;
    let hub: HubStatusDto = ctx.backend.call_typed(Request::HubStatus).await?;
    let overview: Vec<ServiceOverview> = ctx.backend.call_typed(Request::Overview).await?;
    let lines: Vec<(&ServiceOverview, ServiceKind, &RuntimeStatusDto)> = overview
        .iter()
        .flat_map(|item| {
            [
                (item, ServiceKind::Mcp, &item.mcp),
                (item, ServiceKind::Actions, &item.actions),
            ]
        })
        .filter(|(_, _, status)| status.state != "stopped")
        .collect();
    if ctx.out.json_or(&json!({
        "daemon": daemon_json(&daemon),
        "service": hub,
        "projectLines": lines.iter().map(|(item, kind, status)| json!({
            "workspace": item.workspace.id,
            "name": item.workspace.name,
            "service": kind,
            "status": status,
        })).collect::<Vec<_>>(),
    })) {
        return Ok(());
    }
    print_daemon_line(ctx, &daemon);
    print_endpoints(ctx, &hub);
    if !hub.tunnel_error.is_empty() {
        ctx.out.line(format!(
            "{} 公网入口没起来：{}",
            ctx.out.red("✗"),
            hub.tunnel_error
        ));
    }
    if !lines.is_empty() {
        ctx.out.line("");
        ctx.out.line(ctx.out.bold("项目自己的线路"));
        let rows: Vec<Vec<String>> = lines
            .iter()
            .map(|(item, kind, status)| {
                vec![
                    item.workspace.name.clone(),
                    line_label(*kind).to_string(),
                    ctx.out.state(&status.state),
                    or_dash(&status.local_endpoint),
                ]
            })
            .collect();
        ctx.out.table(&["项目", "线路", "状态", "本地地址"], &rows);
        if lines.iter().any(|(_, kind, _)| *kind == ServiceKind::Mcp) {
            ctx.out.line(ctx.out.dim(
                "「单项目 MCP」是旧版本起的，现在只用上面那一个服务；gld stop 会把它一起停掉。",
            ));
        }
    }
    ctx.out.line("");
    ctx.out.line(
        ctx.out
            .dim("地址和凭据：gld ls；某个项目的配置：gld ls <项目>"),
    );
    Ok(())
}

fn line_label(kind: ServiceKind) -> &'static str {
    match kind {
        ServiceKind::Mcp => "单项目 MCP",
        ServiceKind::Actions => "GPT Actions",
    }
}

/// `gld ls`：不给项目就是服务的连接信息和项目表，给了就是那个项目的配置。
pub async fn list(ctx: &mut Ctx, args: ListArgs) -> CliResult {
    if let Some(project) = &args.project {
        if ctx.explicit_workspace {
            return Err(CliError::new(format!(
                "同时给了 {project} 和 -w {}，不知道该听哪个。去掉其中一个。",
                ctx.target.selector.clone().unwrap_or_default()
            )));
        }
        let target = WorkspaceTarget::new(Some(project.clone()), ctx.target.cwd.clone());
        return show_project(ctx, target, args.reveal).await;
    }
    if ctx.explicit_workspace {
        let target = ctx.target.clone();
        return show_project(ctx, target, args.reveal).await;
    }
    show_service(ctx, args.reveal).await
}

/// 服务的连接信息：地址、公网入口、认证、凭据，和项目表。
pub async fn show_service(ctx: &mut Ctx, reveal: bool) -> CliResult {
    let status: HubStatusDto = ctx.backend.call_typed(Request::HubStatus).await?;
    let mut credentials: Vec<(&str, String)> = Vec::new();
    for &(label, key) in credential_keys(&status.config.auth_type) {
        let value: String = ctx
            .backend
            .call_typed(Request::HubSecret { key: key.into() })
            .await?;
        credentials.push((label, if reveal { value } else { mask(&value) }));
    }
    let payload = json!({
        "service": status,
        "credentials": credentials
            .iter()
            .map(|(label, value)| json!({ "label": label, "value": value }))
            .collect::<Vec<_>>(),
    });
    if ctx.out.json_or(&payload) {
        return Ok(());
    }

    let out = ctx.out;
    let state = format!("{}  {}", out.state(&status.state), status.detail);
    let mut rows: Vec<(&str, String)> = vec![
        ("状态", state.trim_end().to_string()),
        ("本地地址", status.local_endpoint.clone()),
        ("公网地址", or_dash(&status.public_endpoint)),
        ("公网入口", status.tunnel_label.clone()),
        ("认证方式", status.config.auth_type.clone()),
    ];
    rows.extend(credentials);
    rows.push((
        "工具集",
        format!(
            "{}（项目自己的工具集照样生效，取交集）",
            status.config.tool_profile
        ),
    ));
    out.line(out.bold("MCP 服务"));
    out.kv(&rows);
    if !status.tunnel_error.is_empty() {
        out.line(format!(
            "{} 公网入口没起来：{}",
            out.red("✗"),
            status.tunnel_error
        ));
    }
    // 登记了却不在服务里的（RFC-0004 之前的老数据）也列出来：看不见的话，用户只会
    // 以为它丢了。`gld start` 会把它们加进来。
    let profiles: Vec<WorkspaceProfile> = ctx.backend.call_typed(Request::ListWorkspaces).await?;
    let outside: Vec<&WorkspaceProfile> = profiles
        .iter()
        .filter(|profile| !status.members.iter().any(|member| member.id == profile.id))
        .collect();
    out.line("");
    if status.members.is_empty() && outside.is_empty() {
        out.line("项目：还没有。gld add <项目目录> 把项目加进来。");
    } else {
        out.line(out.bold(&format!("项目（{}）", status.members.len() + outside.len())));
        let rows: Vec<Vec<String>> = status
            .members
            .iter()
            .map(|member| {
                // 远端项目没有本机路径和工具集，那两列给的是它在对面的位置
                // 和访问上限——远端的 root 由 ccnm 自己解析，gld 不知道。
                let (profile, location) = if member.kind == "remote" {
                    (
                        format!("{}（远端）", member.mode),
                        format!("{}:{}", member.node, member.workspace),
                    )
                } else {
                    (member.tool_profile.clone(), member.path.clone())
                };
                vec![
                    member.name.clone(),
                    gld_core::short_id(&member.id).to_string(),
                    profile,
                    location,
                ]
            })
            .chain(outside.iter().map(|profile| {
                vec![
                    profile.name.clone(),
                    gld_core::short_id(&profile.id).to_string(),
                    out.yellow("不在服务里"),
                    profile.path.clone(),
                ]
            }))
            .collect();
        out.table(&["名称", "ID", "工具集 / 模式", "路径 / 位置"], &rows);
        if !outside.is_empty() {
            out.line(out.yellow(
                "标了「不在服务里」的 AI 看不见（以前的版本登记的）。gld start 会把它们加进来。",
            ));
        }
    }
    out.line("");
    out.line(out.dim(
        "客户端里只配上面这一条地址。AI 每次调用都要带 workspace 参数（项目名称或 id），说明见 docs/concepts.md",
    ));
    if status.public_endpoint.is_empty() {
        out.line(out.yellow("还没有公网地址：ChatGPT 只能连公网 HTTPS。一条命令拿一个：gld share"));
    }
    if !reveal && !status.config.auth_type.eq("noauth") {
        out.line(out.dim("凭据已脱敏，--reveal 显示明文。"));
    }
    Ok(())
}

/// 每种认证方式下，客户端要填的凭据。
pub fn credential_keys(auth_type: &str) -> &'static [(&'static str, &'static str)] {
    match auth_type {
        "oauth" => &[
            ("OAuth Client ID", "oauth_client_id"),
            ("授权口令 (oauth_password)", "oauth_password"),
        ],
        "bearer" => &[("Bearer Token", "bearer_token")],
        _ => &[],
    }
}

/// 一个项目的配置：它是谁、在不在服务里、AI 在它里面能做什么，以及它的 GPT Actions。
pub async fn show_project(ctx: &mut Ctx, target: WorkspaceTarget, reveal: bool) -> CliResult {
    let profile: WorkspaceProfile = ctx
        .backend
        .call_typed(Request::ResolveWorkspace { target })
        .await?;
    let hub: HubStatusDto = ctx.backend.call_typed(Request::HubStatus).await?;
    let in_service = hub.members.iter().any(|member| member.id == profile.id);
    let actions = actions::detail(ctx, &profile, reveal).await?;
    if ctx.out.json_or(&json!({
        "project": profile,
        "inService": in_service,
        "actions": actions,
    })) {
        return Ok(());
    }
    let out = ctx.out;
    out.line(out.bold(&format!(
        "项目 {}  ({})",
        profile.name,
        gld_core::short_id(&profile.id)
    )));
    let runtime = &profile.runtime;
    out.kv(&[
        ("路径", profile.path.clone()),
        (
            "在服务里",
            if in_service {
                "是".to_string()
            } else {
                "否（AI 看不见它；gld start 会把它加进来）".to_string()
            },
        ),
        ("工具集", runtime.tool_profile.clone()),
        ("权限模式", runtime.permission_mode.clone()),
        ("只读工作区内", yes_no(runtime.confine_reads).to_string()),
        ("追加命令", or_dash(&runtime.allowed_commands)),
        ("历史记录", yes_no(runtime.history_recording).to_string()),
    ]);
    actions::print_detail(ctx, &actions);
    out.line("");
    out.line(out.dim(&format!(
        "改它：gld set {} <字段>=<值>（字段见 gld fields）",
        profile.name
    )));
    Ok(())
}

/// 当前目录 / `-w` 对应的项目，不登记。
pub async fn resolve_current(ctx: &mut Ctx) -> CliResult<WorkspaceProfile> {
    ctx.backend
        .call_typed(Request::ResolveWorkspace {
            target: ctx.target.clone(),
        })
        .await
}

/// 读服务配置 → 改 → 整体发回；服务在跑且配置真的变了会自动重启。
pub async fn update_service(
    ctx: &mut Ctx,
    change: impl FnOnce(&mut HubConfig) -> CliResult,
) -> CliResult<HubStatusDto> {
    let current: HubStatusDto = ctx.backend.call_typed(Request::HubStatus).await?;
    let mut config = current.config;
    change(&mut config)?;
    ctx.backend
        .call_typed(Request::SetHubConfig { config })
        .await
}

pub fn print_endpoints(ctx: &Ctx, status: &HubStatusDto) {
    ctx.out.kv(&[
        (
            "MCP 服务",
            format!("{}  {}", ctx.out.state(&status.state), status.detail)
                .trim_end()
                .to_string(),
        ),
        ("本地地址", status.local_endpoint.clone()),
        ("公网地址", or_dash(&status.public_endpoint)),
        ("认证方式", status.config.auth_type.clone()),
        ("项目", status.members.len().to_string()),
    ]);
}

pub fn names(members: &[HubMemberDto]) -> String {
    members
        .iter()
        .map(|member| {
            if member.name.is_empty() {
                // 项目已经不在了、只剩 id 的成员。
                member.id.clone()
            } else {
                member.name.clone()
            }
        })
        .collect::<Vec<_>>()
        .join("、")
}

/// 加入 / 移出服务的结果。
pub fn print_change(
    ctx: &Ctx,
    change: &HubMembershipChange,
    changed: &str,
    unchanged: &str,
) -> CliResult {
    if ctx.out.json_or(change) {
        return Ok(());
    }
    if !change.changed.is_empty() {
        ctx.out
            .line(format!("{changed}：{}", names(&change.changed)));
    }
    if !change.unchanged.is_empty() {
        ctx.out
            .line(format!("{unchanged}：{}", names(&change.unchanged)));
    }
    // 项目表是服务每次请求现读的。不说清楚的话，用户会顺手 gld restart 一遍，
    // 白白掉一次客户端连接。
    if change.status.state == "running" {
        ctx.out
            .line(ctx.out.dim("服务正在运行，下一次调用就生效，不用重启。"));
    }
    Ok(())
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
            "守护进程 {}  （服务已停止；gld start 会自动拉起）",
            ctx.out.dim("未运行")
        )),
    }
    ctx.out.line("");
}

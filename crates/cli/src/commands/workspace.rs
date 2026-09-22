//! 项目的增删改：`gld add` / `gld rm` / `gld set` / `gld fields`，以及旧的 `gld ws …`。
//!
//! 看（`gld ls`）在 [`super::service`]：项目表和服务的连接信息是同一屏。

use std::io::{BufRead, IsTerminal, Write};

use gld_core::app::{
    workspace_field_catalog, HubMemberDto, HubMembershipChange, HubStatusDto, WorkspaceTarget,
    WorkspaceUpdate,
};
use gld_core::runtime::ServiceKind;
use gld_core::workspace::WorkspaceProfile;
use gld_daemon::Request;

use super::{service, Ctx};
use crate::cli::{AddArgs, ListArgs, RemoveArgs, SetArgs, WorkspaceCmd};
use crate::error::{CliError, CliResult};

/// 旧的 `gld ws …`：照新语义走（`ws add` 也会加入服务）。
pub async fn run(ctx: &mut Ctx, command: WorkspaceCmd) -> CliResult {
    match command {
        WorkspaceCmd::Add {
            path,
            name,
            mcp_port: _,
            actions_port,
        } => {
            add(
                ctx,
                AddArgs {
                    paths: vec![path],
                    name,
                },
            )
            .await?;
            if let Some(port) = actions_port {
                let target = ctx.target.clone();
                set_fields(ctx, target, vec![("actions.port".into(), port.to_string())]).await?;
            }
            Ok(())
        }
        WorkspaceCmd::List => service::list(ctx, ListArgs::default()).await,
        WorkspaceCmd::Show => {
            let profile = service::resolve_current(ctx).await?;
            service::show_project(ctx, WorkspaceTarget::selector(profile.id), false).await
        }
        WorkspaceCmd::Remove { yes } => {
            remove(
                ctx,
                RemoveArgs {
                    projects: Vec::new(),
                    all: false,
                    yes,
                },
            )
            .await
        }
        WorkspaceCmd::Set { assignments } => set(ctx, SetArgs { args: assignments }).await,
        WorkspaceCmd::Fields { all } => fields(ctx, all),
        WorkspaceCmd::Use => {
            let profile: WorkspaceProfile = ctx
                .backend
                .call_typed(Request::UseWorkspace {
                    target: ctx.target.clone(),
                })
                .await?;
            if !ctx.out.json_or(&profile) {
                ctx.out.line(format!(
                    "已记住最近使用的项目：{}（{}）",
                    profile.name,
                    gld_core::short_id(&profile.id)
                ));
            }
            Ok(())
        }
    }
}

/// `gld add`：登记即加入服务。已经登记过的，确认它在服务里。
pub async fn add(ctx: &mut Ctx, args: AddArgs) -> CliResult {
    let paths = if args.paths.is_empty() {
        vec![std::path::PathBuf::from(".")]
    } else {
        args.paths
    };
    if args.name.is_some() && paths.len() > 1 {
        return Err(CliError::new(
            "--name 只能配一个目录：一次给了好几个的话，名字该给谁说不清。",
        ));
    }
    let mut added = Vec::new();
    for path in paths {
        let absolute = super::absolutize(&path)?;
        let ensured = service::register(ctx, absolute, args.name.clone()).await?;
        let profile = ensured.profile;
        // 已经登记过、但不在服务里（RFC-0004 之前的老数据）：补加进去。
        let change: HubMembershipChange = ctx
            .backend
            .call_typed(Request::HubAddMembers {
                targets: vec![WorkspaceTarget::selector(profile.id.clone())],
            })
            .await?;
        if !ctx.out.json {
            if !change.changed.is_empty() {
                ctx.out
                    .line(format!("「{}」以前登记过，现在加进服务了。", profile.name));
            } else if !ensured.created {
                // 刚登记的 register 已经说过"已加入"，这里只管本来就在的。
                ctx.out.line(format!(
                    "「{}」本来就在（{}）。",
                    profile.name, profile.path
                ));
            }
        }
        added.push(profile);
    }
    if ctx.out.json_or(&added) {
        return Ok(());
    }
    let status: HubStatusDto = ctx.backend.call_typed(Request::HubStatus).await?;
    if status.state == "running" {
        // 项目表是服务每次请求现读的。不说清楚的话，用户会顺手 gld restart 一遍，
        // 白白掉一次客户端连接。
        ctx.out
            .line(ctx.out.dim("服务正在运行，下一次调用就生效，不用重启。"));
    } else {
        ctx.out.note("服务没在跑：gld start");
    }
    Ok(())
}

/// `gld rm`：从服务里拿掉，并删掉它在 gld 这边的配置和记账。项目文件不动。
///
/// 删掉的是"gld 这边关于这个项目的一切"：配置、它自己的密钥、历史与 Planning 的
/// 记账。服务的凭据不受影响。默认要确认一次，`-y` 是给脚本用的。
pub async fn remove(ctx: &mut Ctx, args: RemoveArgs) -> CliResult {
    let remotes: Vec<HubMemberDto> = remote_members(ctx).await?;
    let mut locals: Vec<WorkspaceProfile> = Vec::new();
    let mut remote_victims: Vec<HubMemberDto> = Vec::new();
    if args.all {
        locals = ctx.backend.call_typed(Request::ListWorkspaces).await?;
    } else if args.projects.is_empty() {
        locals.push(service::resolve_current(ctx).await?);
    } else {
        if ctx.explicit_workspace {
            return Err(CliError::new(format!(
                "同时给了 {} 和 -w {}，不知道该听哪个。去掉其中一个。",
                args.projects.join(" "),
                ctx.target.selector.clone().unwrap_or_default()
            )));
        }
        for selector in &args.projects {
            // 远端项目按名字或 id 认：它们没有本机路径，也不在工作区表里。
            if let Some(remote) = remotes
                .iter()
                .find(|remote| remote.id == *selector || remote.name.eq_ignore_ascii_case(selector))
            {
                remote_victims.push(remote.clone());
                continue;
            }
            // 带上当前目录：selector 可以是相对路径（`gld rm ../ccnm`），
            // 而守护进程的工作目录是数据目录，它自己解析会指到别处。
            let profile: WorkspaceProfile = ctx
                .backend
                .call_typed(Request::ResolveWorkspace {
                    target: WorkspaceTarget::new(Some(selector.clone()), ctx.target.cwd.clone()),
                })
                .await?;
            locals.push(profile);
        }
    }

    if locals.is_empty() && remote_victims.is_empty() {
        if !ctx.out.json_or(&Vec::<WorkspaceProfile>::new()) {
            ctx.out.line("没有项目可删。");
        }
        return Ok(());
    }

    if !args.yes {
        // 一次列清楚要删谁：--all 的时候尤其重要，名字看着眼熟不代表就是它。
        ctx.out.line(format!(
            "将删除 {} 个项目（它们经服务起的命令会先停掉，项目文件不动）：",
            locals.len() + remote_victims.len()
        ));
        for profile in &locals {
            ctx.out
                .line(format!("  {}  {}", profile.name, profile.path));
        }
        for remote in &remote_victims {
            ctx.out.line(format!(
                "  {}  远端 {}:{}",
                remote.name, remote.node, remote.workspace
            ));
        }
        if !confirm("确认删除？项目在 gld 这边的配置和记账会被删掉，且无法恢复。")?
        {
            ctx.out.line("已取消。");
            return Ok(());
        }
    }

    let mut removed = Vec::new();
    for profile in locals {
        let gone: WorkspaceProfile = ctx
            .backend
            .call_typed(Request::DeleteWorkspace {
                target: WorkspaceTarget::selector(profile.id.clone()),
            })
            .await?;
        if !ctx.out.json {
            ctx.out.line(format!("已删除项目「{}」。", gone.name));
        }
        removed.push(serde_json::json!({ "id": gone.id, "name": gone.name, "kind": "local" }));
    }
    for remote in remote_victims {
        let _: HubMembershipChange = ctx
            .backend
            .call_typed(Request::HubRemoveRemote {
                selector: remote.id.clone(),
            })
            .await?;
        if !ctx.out.json {
            ctx.out.line(format!("已删除远端项目「{}」。", remote.name));
        }
        removed.push(serde_json::json!({ "id": remote.id, "name": remote.name, "kind": "remote" }));
    }
    ctx.out.json_or(&removed);
    Ok(())
}

async fn remote_members(ctx: &mut Ctx) -> CliResult<Vec<HubMemberDto>> {
    let status: HubStatusDto = ctx.backend.call_typed(Request::HubStatus).await?;
    Ok(status
        .members
        .into_iter()
        .filter(|member| member.kind == "remote")
        .collect())
}

/// `gld set [项目] key=value…`：第一个不带 `=` 的是项目。
pub async fn set(ctx: &mut Ctx, args: SetArgs) -> CliResult {
    let mut words = args.args.into_iter().peekable();
    let target = match words.peek() {
        Some(first) if !first.contains('=') => {
            let selector = words.next().unwrap_or_default();
            if ctx.explicit_workspace {
                return Err(CliError::new(format!(
                    "同时给了 {selector} 和 -w {}，不知道该听哪个。去掉其中一个。",
                    ctx.target.selector.clone().unwrap_or_default()
                )));
            }
            WorkspaceTarget::new(Some(selector), ctx.target.cwd.clone())
        }
        _ => ctx.target.clone(),
    };
    let mut pairs = Vec::new();
    for item in words {
        let Some((key, value)) = item.split_once('=') else {
            return Err(CliError::new(format!(
                "格式应为 KEY=VALUE，收到「{item}」。项目只能写在最前面；`gld fields` 查看字段。"
            )));
        };
        pairs.push((key.trim().to_string(), value.to_string()));
    }
    if pairs.is_empty() {
        return Err(CliError::new(
            "没说要改什么。写法：gld set <项目> tool-profile=read-only（字段见 gld fields）",
        ));
    }
    set_fields(ctx, target, pairs).await
}

async fn set_fields(
    ctx: &mut Ctx,
    target: WorkspaceTarget,
    pairs: Vec<(String, String)>,
) -> CliResult {
    let update: WorkspaceUpdate = ctx
        .backend
        .call_typed(Request::SetWorkspaceFields {
            target,
            assignments: pairs,
        })
        .await?;
    if ctx.out.json_or(&update) {
        return Ok(());
    }
    ctx.out
        .line(format!("已更新项目「{}」。", update.profile.name));
    report_restarts(ctx, &update)
}

/// 把"为了让新配置生效做了什么"讲清楚。
///
/// 服务每次调用都重新读项目配置，所以 MCP 那一半改完就生效，不用重启；只有项目
/// 自己的 GPT Actions 要重启——重启成功要确认已生效，失败最要紧（配置存下了，
/// 但它现在是停的）。
fn report_restarts(ctx: &Ctx, update: &WorkspaceUpdate) -> CliResult {
    if !update.restart_failures.is_empty() {
        for failure in &update.restart_failures {
            ctx.out.line(format!(
                "{} {} 重启失败：{}",
                ctx.out.red("✗"),
                service_label(failure.service),
                failure.error
            ));
        }
        return Err(CliError::new(
            "新配置已经保存，但服务没能用它起来——现在是停的。按上面的错误修好后 `gld restart -s actions`。",
        ));
    }
    if update.restarted.is_empty() {
        ctx.out.line(ctx.out.dim("下一次调用就生效，不用重启。"));
    } else {
        ctx.out.line(format!(
            "已重启 {}，新配置已生效。",
            update
                .restarted
                .iter()
                .map(|kind| service_label(*kind))
                .collect::<Vec<_>>()
                .join(" 和 ")
        ));
    }
    Ok(())
}

fn service_label(kind: ServiceKind) -> &'static str {
    match kind {
        ServiceKind::Mcp => "单项目 MCP",
        ServiceKind::Actions => "GPT Actions",
    }
}

/// `gld fields`：set 能改的项目字段。
pub fn fields(ctx: &Ctx, all: bool) -> CliResult {
    let catalog = workspace_field_catalog();
    // --json 永远给完整表：脚本要的是全集，不是给人看的精简版。
    if ctx.out.json_or(&catalog) {
        return Ok(());
    }
    let shown = catalog
        .iter()
        .filter(|f| all || !f.key.starts_with("actions."));
    let rows: Vec<Vec<String>> = shown
        .map(|f| {
            vec![
                // 省掉 mcp. 前缀显示，因为敲的时候也可以省。
                f.key.strip_prefix("mcp.").unwrap_or(f.key).to_string(),
                f.value.to_string(),
                f.description.to_string(),
            ]
        })
        .collect();
    ctx.out.table(&["字段", "取值", "说明"], &rows);
    ctx.out.line("");
    ctx.out
        .line("用法：gld set <项目> tool-profile=read-only allowed-commands=rg,gh");
    ctx.out.line(ctx.out.dim(
        "服务本身的端口、认证、公网入口不在这里：gld upgrade --port / --auth，gld share --tunnel",
    ));
    if !all {
        ctx.out.line(format!(
            "GPT Actions（自定义 GPT）那条线路另有 {} 个字段，写成 actions.<字段>：gld fields --all",
            gld_core::app::actions_field_suffixes().len()
        ));
    }
    Ok(())
}

fn confirm(question: &str) -> CliResult<bool> {
    if !std::io::stdin().is_terminal() {
        return Err(CliError::new("非交互环境，请加 -y 确认。"));
    }
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

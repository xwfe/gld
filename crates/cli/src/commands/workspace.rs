use std::io::{BufRead, IsTerminal, Write};

use gld_core::app::{
    workspace_field_catalog, WorkspaceCreateOptions, WorkspaceTarget, WorkspaceUpdate,
};
use gld_core::runtime::ServiceKind;
use gld_core::workspace::WorkspaceProfile;
use gld_daemon::Request;

use super::Ctx;
use crate::cli::{DestroyArgs, WorkspaceCmd};
use crate::error::{CliError, CliResult};
use crate::output::yes_no;

pub async fn run(ctx: &mut Ctx, command: WorkspaceCmd) -> CliResult {
    match command {
        WorkspaceCmd::Add {
            path,
            name,
            mcp_port,
            actions_port,
        } => {
            let profile: WorkspaceProfile = ctx
                .backend
                .call_typed(Request::CreateWorkspace {
                    path: super::absolutize(&path)?,
                    options: WorkspaceCreateOptions {
                        name,
                        mcp_port,
                        actions_port,
                    },
                })
                .await?;
            if !ctx.out.json_or(&profile) {
                ctx.out.line(format!("已添加工作区「{}」", profile.name));
                show_profile(ctx, &profile);
                ctx.out.line("");
                ctx.out.line(format!(
                    "下一步：gld start -w {}   （或进入该目录后直接 gld start）",
                    profile.name
                ));
            }
            Ok(())
        }
        WorkspaceCmd::List => {
            let profiles: Vec<WorkspaceProfile> =
                ctx.backend.call_typed(Request::ListWorkspaces).await?;
            if ctx.out.json_or(&profiles) {
                return Ok(());
            }
            if profiles.is_empty() {
                ctx.out
                    .line("还没有工作区。在项目目录里执行 `gld start` 就会自动登记并启动。");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = profiles
                .iter()
                .map(|p| {
                    vec![
                        p.id.chars().take(8).collect(),
                        p.name.clone(),
                        p.runtime.local_port.to_string(),
                        p.actions.local_port.to_string(),
                        p.auth.auth_type.clone(),
                        p.tunnel.tunnel_type.clone(),
                        p.path.clone(),
                    ]
                })
                .collect();
            ctx.out.table(
                &[
                    "ID",
                    "名称",
                    "MCP端口",
                    "Actions端口",
                    "认证",
                    "隧道",
                    "路径",
                ],
                &rows,
            );
            Ok(())
        }
        WorkspaceCmd::Show => {
            let profile: WorkspaceProfile = ctx
                .backend
                .call_typed(Request::ResolveWorkspace {
                    target: ctx.target.clone(),
                })
                .await?;
            if !ctx.out.json_or(&profile) {
                show_profile(ctx, &profile);
            }
            Ok(())
        }
        WorkspaceCmd::Remove { yes } => {
            // 和 `gld destroy` 是同一件事，只是入口不同。
            destroy(
                ctx,
                DestroyArgs {
                    workspace: None,
                    all: false,
                    yes,
                },
            )
            .await
        }
        WorkspaceCmd::Set { assignments } => {
            let mut pairs = Vec::with_capacity(assignments.len());
            for item in &assignments {
                let Some((key, value)) = item.split_once('=') else {
                    return Err(CliError::new(format!(
                        "格式应为 KEY=VALUE，收到「{item}」。`gld workspace fields` 查看字段。"
                    )));
                };
                pairs.push((key.trim().to_string(), value.to_string()));
            }
            let update: WorkspaceUpdate = ctx
                .backend
                .call_typed(Request::SetWorkspaceFields {
                    target: ctx.target.clone(),
                    assignments: pairs,
                })
                .await?;
            if ctx.out.json_or(&update) {
                return Ok(());
            }
            ctx.out
                .line(format!("已更新工作区「{}」。", update.profile.name));
            show_profile(ctx, &update.profile);
            report_restarts(ctx, &update)
        }
        WorkspaceCmd::Fields { all } => {
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
                .line("用法：gld workspace set port=30000 auth=bearer");
            if !all {
                ctx.out.line(format!(
                    "Actions 那条线路有同名的 {} 个字段，前缀写成 actions.：{}",
                    gld_core::app::actions_field_suffixes().len(),
                    gld_core::app::actions_field_suffixes().join(" / ")
                ));
                ctx.out
                    .line(ctx.out.dim("完整列表：gld workspace fields --all"));
            }
            Ok(())
        }
        WorkspaceCmd::Use => {
            let profile: WorkspaceProfile = ctx
                .backend
                .call_typed(Request::UseWorkspace {
                    target: ctx.target.clone(),
                })
                .await?;
            if !ctx.out.json_or(&profile) {
                ctx.out.line(format!(
                    "已记住最近使用的工作区：{}（{}）",
                    profile.name, profile.id
                ));
            }
            Ok(())
        }
    }
}

/// `gld destroy`：停服务、停隧道、删配置与密钥。项目文件不动。
///
/// 删掉的是"gld 这边关于这个项目的一切"：端口、认证方式、密钥、隧道配置、
/// 历史与 Planning 的记账。密钥没有备份，客户端里存着的 token / 口令随之失效，
/// 所以默认要确认一次，`-y` 是给脚本用的。
pub async fn destroy(ctx: &mut Ctx, args: DestroyArgs) -> CliResult {
    let victims = if args.all {
        ctx.backend.call_typed(Request::ListWorkspaces).await?
    } else {
        let target = match &args.workspace {
            Some(selector) => {
                if ctx.explicit_workspace {
                    return Err(CliError::new(format!(
                        "同时给了 {selector} 和 -w {}，不知道该听哪个。去掉其中一个。",
                        ctx.target.selector.clone().unwrap_or_default()
                    )));
                }
                // 带上当前目录：selector 可以是相对路径（`gld destroy ../ccnm`），
                // 而守护进程的工作目录是数据目录，它自己解析会指到别处。
                WorkspaceTarget::new(Some(selector.clone()), ctx.target.cwd.clone())
            }
            None => ctx.target.clone(),
        };
        let profile: WorkspaceProfile = ctx
            .backend
            .call_typed(Request::ResolveWorkspace { target })
            .await?;
        vec![profile]
    };

    if victims.is_empty() {
        if !ctx.out.json_or(&Vec::<WorkspaceProfile>::new()) {
            ctx.out.line("没有工作区可销毁。");
        }
        return Ok(());
    }

    if !args.yes {
        // 一次列清楚要销毁谁：--all 的时候尤其重要，名字看着眼熟不代表就是它。
        ctx.out.line(format!(
            "将销毁 {} 个工作区（服务和隧道会先停掉，项目文件不动）：",
            victims.len()
        ));
        for profile in &victims {
            ctx.out
                .line(format!("  {}  {}", profile.name, profile.path));
        }
        if !confirm("确认销毁？配置和密钥会被删除，且无法恢复。")? {
            ctx.out.line("已取消。");
            return Ok(());
        }
    }

    let mut removed = Vec::with_capacity(victims.len());
    for profile in victims {
        let gone: WorkspaceProfile = ctx
            .backend
            .call_typed(Request::DeleteWorkspace {
                target: WorkspaceTarget::selector(profile.id.clone()),
            })
            .await?;
        if !ctx.out.json {
            ctx.out.line(format!("已销毁工作区「{}」。", gone.name));
        }
        removed.push(gone);
    }
    ctx.out.json_or(&removed);
    Ok(())
}

/// 把"为了让新配置生效做了什么"讲清楚。
///
/// 三种情况读者关心的东西完全不同：重启成功要确认已生效；没在跑要知道
/// 下次 start 会带上；重启失败最要紧——配置存下了，但服务现在是停的。
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
            "新配置已经保存，但服务没能用它起来——现在是停的。按上面的错误修好后 `gld restart`。",
        ));
    }
    match update.restarted.as_slice() {
        [] => ctx
            .out
            .note("服务没在跑，新配置会在下次 `gld start` 时生效。"),
        kinds => ctx.out.line(format!(
            "已重启 {}，新配置已生效。",
            kinds
                .iter()
                .map(|kind| service_label(*kind))
                .collect::<Vec<_>>()
                .join(" 和 ")
        )),
    }
    Ok(())
}

fn service_label(kind: ServiceKind) -> &'static str {
    match kind {
        ServiceKind::Mcp => "MCP",
        ServiceKind::Actions => "Actions",
    }
}

pub fn show_profile(ctx: &Ctx, p: &WorkspaceProfile) {
    ctx.out.kv(&[
        ("名称", p.name.clone()),
        ("ID", p.id.clone()),
        ("路径", p.path.clone()),
        ("MCP 端口", p.runtime.local_port.to_string()),
        (
            "MCP 认证",
            format!(
                "{}（共享密钥：{}）",
                p.auth.auth_type,
                yes_no(p.auth.use_shared_secrets)
            ),
        ),
        ("MCP 工具集", p.runtime.tool_profile.clone()),
        (
            "MCP 隧道",
            super::service::tunnel_config_label(
                &p.tunnel.tunnel_type,
                &p.tunnel.cloudflare_mode,
                &p.tunnel.frp_subdomain,
                &p.tunnel.public_url,
                p.tunnel.use_global_gateway,
            ),
        ),
        ("Actions 端口", p.actions.local_port.to_string()),
        (
            "Actions 认证",
            format!(
                "{}（共享密钥：{}）",
                p.actions.auth_type,
                yes_no(p.actions.use_shared_secrets)
            ),
        ),
        (
            "Actions 隧道",
            super::service::tunnel_config_label(
                &p.actions.tunnel_type,
                &p.actions.cloudflare_mode,
                &p.actions.frp_subdomain,
                &p.actions.public_url,
                p.actions.use_global_gateway,
            ),
        ),
        ("历史记录", yes_no(p.runtime.history_recording).to_string()),
    ]);
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

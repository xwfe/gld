//! `gld upgrade`：改常用配置，并让它当场生效。
//!
//! 这些改动以前要拆成两三步：`gld ws set public-url=…`（还得知道字段叫什么）、
//! 隧道要不要重连、服务要不要重启。而用户想的只有一句"换个地址"。
//!
//! 和 `gld workspace set` 的分工：那条命令是全字段入口（33 个），
//! 这条只管最常改的五项，用参数而不是 `key=value` 表达。

use gld_core::app::{WorkspaceTarget, WorkspaceUpdate};
use gld_core::workspace::WorkspaceProfile;
use gld_daemon::Request;

use super::{service, share, Ctx};
use crate::cli::{ListArgs, TunnelSpec, UpgradeArgs};
use crate::error::{CliError, CliResult};

pub async fn run(ctx: &mut Ctx, args: UpgradeArgs) -> CliResult {
    let spec = match (args.off, args.tunnel.clone()) {
        (true, _) => Some(TunnelSpec::Off),
        (false, spec) => spec,
    };
    if spec.is_none()
        && args.path.is_none()
        && args.name.is_none()
        && args.port.is_none()
        && args.actions_port.is_none()
        && args.auth.is_none()
    {
        return Err(CliError::new(
            "没说要改什么。可改的：--path 项目目录、--tunnel 公网入口、--off 关公网、\n\
             --name 名称、--port MCP 端口、--actions-port、--auth 认证方式。\n\
             更多字段：gld workspace fields",
        ));
    }

    let profile = resolve(ctx, args.workspace.as_deref()).await?;
    let target = WorkspaceTarget::selector(profile.id.clone());

    // 先改公网入口：它和别的字段走同一个 set，但要按服务分前缀，
    // 而且换模式时得把上一种模式的残值清掉（share 那边已经处理好了）。
    if let Some(spec) = &spec {
        share::configure(
            ctx,
            &target,
            &profile,
            spec,
            args.subdomain.as_deref(),
            args.service,
        )
        .await?;
    } else if let Some(sub) = &args.subdomain {
        return Err(CliError::new(format!(
            "--subdomain {sub} 要配合 --tunnel frp:<配置名> 一起用。"
        )));
    }

    let mut assignments: Vec<(String, String)> = Vec::new();
    if let Some(path) = &args.path {
        assignments.push(("path".into(), path.to_string_lossy().into_owned()));
    }
    if let Some(name) = &args.name {
        assignments.push(("name".into(), name.clone()));
    }
    if let Some(port) = args.port {
        assignments.push(("mcp.port".into(), port.to_string()));
    }
    if let Some(port) = args.actions_port {
        assignments.push(("actions.port".into(), port.to_string()));
    }
    if let Some(auth) = &args.auth {
        assignments.push(("mcp.auth".into(), auth.clone()));
    }

    if !assignments.is_empty() {
        let update: WorkspaceUpdate = ctx
            .backend
            .call_typed(Request::SetWorkspaceFields {
                target: target.clone(),
                assignments,
            })
            .await?;
        for failure in &update.restart_failures {
            ctx.out.line(format!(
                "{} {} 重启失败：{}",
                ctx.out.red("✗"),
                match failure.service {
                    gld_core::runtime::ServiceKind::Mcp => "MCP",
                    gld_core::runtime::ServiceKind::Actions => "Actions",
                },
                failure.error
            ));
        }
        if !update.restart_failures.is_empty() {
            return Err(CliError::new(
                "新配置已经保存，但服务没能用它起来——现在是停的。按上面的错误修好后 `gld restart`。",
            ));
        }
    }

    // 换了隧道模式就得真的把新隧道拉起来，否则配置是新的、跑着的还是旧的，
    // 而 ls 会照着配置显示一个还没生效的地址。
    if let Some(spec) = &spec {
        let running = ctx
            .backend
            .call_typed::<gld_core::workspace::RuntimeStatusDto>(Request::ServiceStatus {
                target: target.clone(),
                kind: share::service_kind(args.service),
            })
            .await?
            .state
            != "stopped";
        if running {
            share::ensure_tunnel_up(ctx, &target, spec, args.service).await?;
        }
    }

    service::show_detail(ctx, &target, ListArgs::default()).await
}

/// 位置参数给了就按它找，否则按当前目录 / `-w` 推断。
///
/// 这里不自动登记：`upgrade` 是"改一个已有工作区"，目录没登记过时凭空
/// 建一个再改它，等于把 `start` 的语义偷偷塞进来。
async fn resolve(ctx: &mut Ctx, selector: Option<&str>) -> CliResult<WorkspaceProfile> {
    if let Some(selector) = selector {
        if ctx.explicit_workspace {
            return Err(CliError::new(format!(
                "同时给了 {selector} 和 -w {}，不知道该听哪个。去掉其中一个。",
                ctx.target.selector.clone().unwrap_or_default()
            )));
        }
        return ctx
            .backend
            .call_typed(Request::ResolveWorkspace {
                target: WorkspaceTarget::selector(selector),
            })
            .await;
    }
    ctx.backend
        .call_typed(Request::ResolveWorkspace {
            target: ctx.target.clone(),
        })
        .await
}

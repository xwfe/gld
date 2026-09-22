//! `gld upgrade`：改常用配置，并让它当场生效。
//!
//! 这些改动以前要拆成两三步：改字段（还得知道字段叫什么）、隧道要不要重连、
//! 服务要不要重启。而用户想的只有一句"换个地址"。
//!
//! RFC-0004 之后分两半：端口、认证、工具集、公网入口是**服务**的；目录、名称、
//! Actions 端口是**某个项目**的。项目的其余字段走 `gld set`。

use gld_core::app::{WorkspaceTarget, WorkspaceUpdate};
use gld_core::workspace::WorkspaceProfile;
use gld_daemon::Request;

use super::{service, share, Ctx};
use crate::cli::{TunnelService, TunnelSpec, UpgradeArgs};
use crate::error::{CliError, CliResult};

pub async fn run(ctx: &mut Ctx, args: UpgradeArgs) -> CliResult {
    let spec = match (args.off, args.tunnel.clone()) {
        (true, _) => Some(TunnelSpec::Off),
        (false, spec) => spec,
    };
    let actions_line = matches!(args.service, TunnelService::Actions);
    let project_change =
        args.path.is_some() || args.name.is_some() || args.actions_port.is_some() || actions_line;
    let service_change = !actions_line
        && (spec.is_some()
            || args.port.is_some()
            || args.auth.is_some()
            || args.tool_profile.is_some());
    if !project_change && !service_change {
        let selector = args
            .workspace
            .as_deref()
            .or(ctx.target.selector.as_deref())
            .unwrap_or_default();
        return Err(CliError::new(format!(
            "{}没说要改什么。服务的：--port 端口、--auth 认证、--tool-profile 工具集、\n\
             --tunnel 公网入口、--off 关公网；项目的：--path 目录、--name 名称。\n\
             项目的其余字段：gld fields",
            path_selector_hint(selector)
        )));
    }
    if spec.is_none() {
        if let Some(sub) = &args.subdomain {
            return Err(CliError::new(format!(
                "--subdomain {sub} 要配合 --tunnel frp:<配置名> 一起用。"
            )));
        }
    }

    if project_change {
        let profile = resolve(ctx, args.workspace.as_deref()).await?;
        let target = WorkspaceTarget::selector(profile.id.clone());
        // 项目的 GPT Actions 公网入口：`-s actions --tunnel …`。
        if let (true, Some(spec)) = (actions_line, &spec) {
            share::configure(
                ctx,
                &target,
                &profile,
                spec,
                args.subdomain.as_deref(),
                args.tunnel_token.as_deref(),
                TunnelService::Actions,
            )
            .await?;
        }
        let mut assignments: Vec<(String, String)> = Vec::new();
        if let Some(path) = &args.path {
            let absolute = super::absolutize(path)?;
            assignments.push(("path".into(), absolute.to_string_lossy().into_owned()));
        }
        if let Some(name) = &args.name {
            assignments.push(("name".into(), name.clone()));
        }
        if let Some(port) = args.actions_port {
            assignments.push(("actions.port".into(), port.to_string()));
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
                    "{} GPT Actions 重启失败：{}",
                    ctx.out.red("✗"),
                    failure.error
                ));
            }
            if !update.restart_failures.is_empty() {
                return Err(CliError::new(
                    "新配置已经保存，但 GPT Actions 没能用它起来——现在是停的。按上面的错误修好后 `gld restart -s actions`。",
                ));
            }
        }
        if let (true, Some(spec)) = (actions_line, &spec) {
            share::ensure_tunnel_up(ctx, &target, spec, TunnelService::Actions).await?;
        }
        if !service_change {
            return service::show_project(ctx, target, false).await;
        }
    }

    if let Some(spec) = &spec {
        share::configure_service(
            ctx,
            spec,
            args.subdomain.as_deref(),
            args.tunnel_token.as_deref(),
        )
        .await?;
    }
    let status = service::update_service(ctx, |config| {
        if let Some(port) = args.port {
            config.local_port = port;
        }
        if let Some(auth) = &args.auth {
            config.auth_type = auth.clone();
        }
        if let Some(profile) = &args.tool_profile {
            config.tool_profile = profile.clone();
        }
        Ok(())
    })
    .await?;
    // 换了公网入口就得确认新入口真的起来了，否则配置是新的、跑着的还是旧的，
    // 而 ls 会照着配置显示一个还没生效的地址。
    if status.state == "running" {
        if spec.is_some() && !matches!(spec, Some(TunnelSpec::Off)) {
            share::ensure_service_public(ctx, &status).await?;
        } else {
            share::verify_named_service_public(ctx, &status).await?;
        }
    }
    service::show_service(ctx, false).await
}

/// 用路径挑了项目、却一个要改的字段都没给时，先说清那个路径是干什么的。
///
/// `-w <路径>` 和 `--path <路径>` 都吃路径，方向却相反：一个是"改哪个项目"，
/// 一个是"把目录改成这个"。光列出 `--path 目录`，刚给过一个路径的人只会
/// 想"我不是已经给了吗"。只在选择器看着像路径时说——`-w api` 这种没有歧义，
/// 多一句反而是噪音。
fn path_selector_hint(selector: &str) -> String {
    let looks_like_path = selector.contains('/')
        || selector.contains('\\')
        || matches!(selector, "." | "..")
        || selector.starts_with('~');
    if !looks_like_path {
        return String::new();
    }
    format!(
        "「{selector}」是在挑要改哪个项目，不是要把目录改成它。\n\
         真要换项目目录：gld upgrade --path {selector}\n"
    )
}

/// 位置参数给了就按它找，否则按当前目录 / `-w` 推断。
///
/// 这里不自动登记：`upgrade` 是"改一个已有项目"，目录没登记过时凭空
/// 建一个再改它，等于把 `add` 的语义偷偷塞进来。
async fn resolve(ctx: &mut Ctx, selector: Option<&str>) -> CliResult<WorkspaceProfile> {
    if let Some(selector) = selector {
        if ctx.explicit_workspace {
            return Err(CliError::new(format!(
                "同时给了 {selector} 和 -w {}，不知道该听哪个。去掉其中一个。",
                ctx.target.selector.clone().unwrap_or_default()
            )));
        }
        // 带上当前目录：selector 可以是相对路径，而守护进程的工作目录是数据目录。
        let target = WorkspaceTarget::new(Some(selector.to_string()), ctx.target.cwd.clone());
        return ctx
            .backend
            .call_typed(Request::ResolveWorkspace { target })
            .await;
    }
    service::resolve_current(ctx).await
}

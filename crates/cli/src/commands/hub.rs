//! 远端项目（`gld remote`），以及旧的 `gld hub …`。
//!
//! RFC-0004 之后"hub"就是那个唯一的服务，它的起停、查看、改配置都挪到了顶层
//! （start / stop / ls / upgrade）。`gld hub …` 还认，照新语义转过去。

use gld_core::app::{CcnmMemberSpec, HubMembershipChange, HubStatusDto};
use gld_daemon::Request;

use super::{secret, service, Ctx};
use crate::cli::{HubCmd, HubSetArgs, RemoteCmd, StartArgs};
use crate::error::{CliError, CliResult};

pub async fn run(ctx: &mut Ctx, command: HubCmd) -> CliResult {
    match command {
        HubCmd::List { reveal } => service::show_service(ctx, reveal).await,
        HubCmd::Add { workspaces } => {
            let targets = targets(ctx, workspaces);
            let change: HubMembershipChange = ctx
                .backend
                .call_typed(Request::HubAddMembers { targets })
                .await?;
            service::print_change(ctx, &change, "已加入", "本来就在服务里")
        }
        // 只把项目移出服务、登记留着——RFC-0004 之前的语义。现在的 `gld rm`
        // 是连登记一起删，这条旧命令不偷偷改成那样。
        HubCmd::Remove { workspaces } => {
            let targets = targets(ctx, workspaces);
            let change: HubMembershipChange = ctx
                .backend
                .call_typed(Request::HubRemoveMembers { targets })
                .await?;
            service::print_change(ctx, &change, "已移出", "本来就不在服务里")
        }
        HubCmd::Set(args) => {
            let status = service::update_service(ctx, |config| apply(config, args)).await?;
            if !ctx.out.json_or(&status) {
                service::print_endpoints(ctx, &status);
            }
            Ok(())
        }
        HubCmd::Start => {
            service::start(
                ctx,
                StartArgs {
                    path: None,
                    tunnel: None,
                    tunnel_token: None,
                    subdomain: None,
                    port: None,
                    service: None,
                },
            )
            .await
        }
        HubCmd::Stop => {
            let status: HubStatusDto = ctx.backend.call_typed(Request::HubStop).await?;
            if !ctx.out.json_or(&status) {
                ctx.out
                    .line(format!("MCP 服务  {}", ctx.out.state(&status.state)));
            }
            Ok(())
        }
        HubCmd::Regenerate { key } => secret::regenerate_service(ctx, key).await,
        HubCmd::Remote(command) => remote(ctx, command).await,
    }
}

pub async fn remote(ctx: &mut Ctx, command: RemoteCmd) -> CliResult {
    match command {
        RemoteCmd::Add {
            name,
            node,
            remote_workspace,
            ccnm,
            mode,
        } => {
            let spec = CcnmMemberSpec {
                name,
                node,
                workspace: remote_workspace,
                ccnm_bin: ccnm.unwrap_or_default(),
                mode: mode.unwrap_or_default(),
            };
            let change: HubMembershipChange = ctx
                .backend
                .call_typed(Request::HubAddRemote { spec })
                .await?;
            service::print_change(ctx, &change, "已加入", "本来就在服务里")?;
            // 这条命令只写配置，不去连。真连要等第一次调用，那时才知道对面
            // 通不通——先说清楚，免得以为"加成功了"就等于"连得上"。
            ctx.out.note(
                "只写了配置，还没连过。验一下：gld start 之后让客户端调 remote_workspace_info，\
                 或者先在本机跑 ccnm mcp bridge <workspace> --node <node> --mode read 看看通不通。",
            );
            Ok(())
        }
        RemoteCmd::Remove { selector } => {
            let change: HubMembershipChange = ctx
                .backend
                .call_typed(Request::HubRemoveRemote { selector })
                .await?;
            service::print_change(ctx, &change, "已删除", "本来就不在")
        }
    }
}

/// 给了项目就逐个解析；一个没给就和其他命令一样，按 -w / 当前目录推断。
///
/// 相对路径形式的项目要按调用方目录解析，所以每个 target 都带上 cwd。
fn targets(ctx: &Ctx, workspaces: Vec<String>) -> Vec<gld_core::app::WorkspaceTarget> {
    if workspaces.is_empty() {
        return vec![ctx.target.clone()];
    }
    workspaces
        .into_iter()
        .map(|selector| gld_core::app::WorkspaceTarget::new(Some(selector), ctx.target.cwd.clone()))
        .collect()
}

fn apply(config: &mut gld_core::settings::HubConfig, args: HubSetArgs) -> CliResult {
    if let Some(port) = args.port {
        if port == 0 {
            return Err(CliError::new("端口必须在 1-65535"));
        }
        config.local_port = port;
    }
    if let Some(auth) = args.auth {
        config.auth_type = auth;
    }
    if let Some(profile) = args.tool_profile {
        config.tool_profile = profile;
    }
    if let Some(url) = args.public_url {
        config.public_url = url;
    }
    if let Some(use_gateway) = args.global_gateway {
        config.use_global_gateway = use_gateway;
    }
    Ok(())
}

use gld_core::global_gateway::{GatewayHealthItem, GlobalGatewayStatusDto};
use gld_core::settings::GlobalGatewayConfig;
use gld_daemon::Request;
use serde_json::json;

use super::Ctx;
use crate::cli::{GatewayCmd, GatewaySetArgs};
use crate::error::{CliError, CliResult};
use crate::output::{or_dash, yes_no};

pub async fn run(ctx: &mut Ctx, command: GatewayCmd) -> CliResult {
    match command {
        GatewayCmd::Show => {
            let config: GlobalGatewayConfig =
                ctx.backend.call_typed(Request::GatewayConfig).await?;
            let status: GlobalGatewayStatusDto =
                ctx.backend.call_typed(Request::GatewayStatus).await?;
            if ctx
                .out
                .json_or(&json!({ "config": config, "status": status }))
            {
                return Ok(());
            }
            ctx.out.kv(&[
                ("启用", yes_no(config.enabled).to_string()),
                (
                    "状态",
                    format!("{}  {}", ctx.out.state(&status.state), status.detail),
                ),
                ("本地地址", status.local_url.clone()),
                ("公网地址", or_dash(&status.public_url)),
                ("端口", config.local_port.to_string()),
                ("隧道", config.tunnel_type.clone()),
                ("FRP 配置", or_dash(&config.frp_profile_id)),
                ("FRP 子域名", or_dash(&config.frp_subdomain)),
                ("使用代理", yes_no(config.use_proxy).to_string()),
            ]);
            ctx.out.line("");
            ctx.out.line(ctx.out.dim("工作区通过 gld ws set mcp.global-gateway=true 接入，公网路径为 <公网地址>/w/<工作区id>/mcp"));
            Ok(())
        }
        GatewayCmd::Set(args) => {
            let mut config: GlobalGatewayConfig =
                ctx.backend.call_typed(Request::GatewayConfig).await?;
            apply(&mut config, args)?;
            let saved: GlobalGatewayConfig = ctx
                .backend
                .call_typed(Request::SetGatewayConfig { config })
                .await?;
            if !ctx.out.json_or(&saved) {
                ctx.out.line(
                    "已保存全局入口配置。若它正在运行，执行 `gld gateway start` 应用新配置。",
                );
            }
            Ok(())
        }
        GatewayCmd::Start => {
            let status: GlobalGatewayStatusDto =
                ctx.backend.call_typed(Request::GatewayStart).await?;
            print_status(ctx, &status)
        }
        GatewayCmd::Stop => {
            let status: GlobalGatewayStatusDto =
                ctx.backend.call_typed(Request::GatewayStop).await?;
            print_status(ctx, &status)
        }
        GatewayCmd::Health => {
            let items: Vec<GatewayHealthItem> =
                ctx.backend.call_typed(Request::GatewayHealth).await?;
            if ctx.out.json_or(&items) {
                return Ok(());
            }
            for item in &items {
                ctx.out.line(format!(
                    "{} {}  {}",
                    ctx.out.ok_mark(item.ok),
                    item.label,
                    item.detail
                ));
            }
            Ok(())
        }
    }
}

fn apply(config: &mut GlobalGatewayConfig, args: GatewaySetArgs) -> CliResult {
    if let Some(enabled) = args.enabled {
        config.enabled = enabled;
    }
    if let Some(port) = args.port {
        if port == 0 {
            return Err(CliError::new("端口必须在 1-65535"));
        }
        config.local_port = port;
    }
    if let Some(tunnel) = args.tunnel {
        let tunnel = tunnel.to_ascii_lowercase();
        if !matches!(tunnel.as_str(), "none" | "cloudflare" | "frp") {
            return Err(CliError::new("隧道类型只能是 none | cloudflare | frp"));
        }
        config.tunnel_type = tunnel;
    }
    if let Some(url) = args.public_url {
        config.public_url = url.trim().trim_end_matches('/').to_string();
    }
    if let Some(profile) = args.frp_profile {
        config.frp_profile_id = profile;
    }
    if let Some(subdomain) = args.frp_subdomain {
        config.frp_subdomain = subdomain;
    }
    if let Some(use_proxy) = args.use_proxy {
        config.use_proxy = use_proxy;
    }
    Ok(())
}

fn print_status(ctx: &Ctx, status: &GlobalGatewayStatusDto) -> CliResult {
    if ctx.out.json_or(status) {
        return Ok(());
    }
    ctx.out.kv(&[
        (
            "状态",
            format!("{}  {}", ctx.out.state(&status.state), status.detail),
        ),
        ("本地地址", status.local_url.clone()),
        ("公网地址", or_dash(&status.public_url)),
    ]);
    Ok(())
}

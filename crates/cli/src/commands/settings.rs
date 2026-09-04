use gld_core::app::GlobalRuntimeSettingsDto;
use gld_core::settings::{GlobalGatewayConfig, ProxyConfig};
use gld_daemon::Request;
use serde_json::json;

use super::{split_list, Ctx};
use crate::cli::{RuntimeSetArgs, SettingsCmd};
use crate::error::CliResult;
use crate::output::{or_dash, yes_no};

pub async fn run(ctx: &mut Ctx, command: SettingsCmd) -> CliResult {
    match command {
        SettingsCmd::Show => {
            let proxy: ProxyConfig = ctx.backend.call_typed(Request::Proxy).await?;
            let runtime: GlobalRuntimeSettingsDto =
                ctx.backend.call_typed(Request::RuntimeSettings).await?;
            let gateway: GlobalGatewayConfig =
                ctx.backend.call_typed(Request::GatewayConfig).await?;
            let home = ctx.backend.paths().home.clone();
            if ctx.out.json_or(
                &json!({ "home": home, "proxy": proxy, "runtime": runtime, "gateway": gateway }),
            ) {
                return Ok(());
            }
            ctx.out.kv(&[
                ("数据目录", home.display().to_string()),
                ("代理", describe_proxy(&proxy.mode, &proxy.url)),
                ("局域网访问", yes_no(runtime.allow_lan_access).into()),
                (
                    "启动时恢复服务",
                    yes_no(runtime.restore_runtime_state_on_launch).into(),
                ),
                ("全局可执行路径", or_dash(&runtime.executable_paths)),
                ("全局 Agent 说明", or_dash(&runtime.ai_instructions)),
                ("说明来源", or_dash(&runtime.instruction_sources.join(","))),
                ("Skill 来源", or_dash(&runtime.skill_sources.join(","))),
                (
                    "全局入口",
                    format!(
                        "{}（端口 {}，隧道 {}）",
                        yes_no(gateway.enabled),
                        gateway.local_port,
                        gateway.tunnel_type
                    ),
                ),
            ]);
            Ok(())
        }
        SettingsCmd::Proxy { mode, url } => {
            let mut proxy: ProxyConfig = ctx.backend.call_typed(Request::Proxy).await?;
            if mode.is_none() && url.is_none() {
                if !ctx.out.json_or(&proxy) {
                    ctx.out.line(describe_proxy(&proxy.mode, &proxy.url));
                }
                return Ok(());
            }
            if let Some(mode) = mode {
                proxy.mode = mode;
            }
            if let Some(url) = url {
                proxy.url = url;
            }
            let saved: ProxyConfig = ctx.backend.call_typed(Request::SetProxy { proxy }).await?;
            if !ctx.out.json_or(&saved) {
                ctx.out.line(format!(
                    "代理已设置：{}",
                    describe_proxy(&saved.mode, &saved.url)
                ));
            }
            Ok(())
        }
        SettingsCmd::Runtime(args) => {
            let mut runtime: GlobalRuntimeSettingsDto =
                ctx.backend.call_typed(Request::RuntimeSettings).await?;
            if !apply_runtime(&mut runtime, args) {
                if !ctx.out.json_or(&runtime) {
                    ctx.out.kv(&[
                        ("局域网访问", yes_no(runtime.allow_lan_access).into()),
                        (
                            "启动时恢复服务",
                            yes_no(runtime.restore_runtime_state_on_launch).into(),
                        ),
                        ("全局可执行路径", or_dash(&runtime.executable_paths)),
                        ("全局 Agent 说明", or_dash(&runtime.ai_instructions)),
                        ("说明来源", or_dash(&runtime.instruction_sources.join(","))),
                        ("Skill 来源", or_dash(&runtime.skill_sources.join(","))),
                        ("自定义说明路径", or_dash(&runtime.custom_instruction_paths)),
                        ("自定义 Skill 路径", or_dash(&runtime.custom_skill_paths)),
                    ]);
                }
                return Ok(());
            }
            let saved: GlobalRuntimeSettingsDto = ctx
                .backend
                .call_typed(Request::SetRuntimeSettings { runtime })
                .await?;
            if !ctx.out.json_or(&saved) {
                ctx.out
                    .line("运行时设置已保存。局域网访问等监听相关项需要 gld restart 才生效。");
                if saved.allow_lan_access {
                    ctx.out.line(ctx.out.yellow(
                        "已允许局域网访问：服务会监听 0.0.0.0，请确认认证方式不是 noauth。",
                    ));
                }
            }
            Ok(())
        }
    }
}

fn apply_runtime(runtime: &mut GlobalRuntimeSettingsDto, args: RuntimeSetArgs) -> bool {
    let mut changed = false;
    if let Some(value) = args.lan_access {
        runtime.allow_lan_access = value;
        changed = true;
    }
    if let Some(value) = args.restore_on_launch {
        runtime.restore_runtime_state_on_launch = value;
        changed = true;
    }
    if let Some(value) = args.executable_paths {
        runtime.executable_paths = value;
        changed = true;
    }
    if let Some(value) = args.ai_instructions {
        runtime.ai_instructions = value;
        changed = true;
    }
    if let Some(value) = args.instruction_sources {
        runtime.instruction_sources = split_list(&value);
        changed = true;
    }
    if let Some(value) = args.skill_sources {
        runtime.skill_sources = split_list(&value);
        changed = true;
    }
    if let Some(value) = args.custom_instruction_paths {
        runtime.custom_instruction_paths = value;
        changed = true;
    }
    if let Some(value) = args.custom_skill_paths {
        runtime.custom_skill_paths = value;
        changed = true;
    }
    changed
}

fn describe_proxy(mode: &str, url: &str) -> String {
    match mode {
        "manual" => format!("manual（{}）", or_dash(url)),
        "" => "system".into(),
        other => other.to_string(),
    }
}

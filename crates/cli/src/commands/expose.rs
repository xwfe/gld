//! `gld expose`：一条命令拿到公网地址。
//!
//! 这个命令不做任何别处做不到的事，它只是把最常走的那条路合成一步。
//! 以前接 ChatGPT 要敲：
//!
//! ```text
//! gld ws set mcp.tunnel=cloudflare mcp.cloudflare-mode=quick
//! gld restart          # 忘了这步就是"配了没反应"
//! gld connect          # 才看得到公网地址
//! ```
//!
//! 三条里有两条是"仪式"：用户想要的只是那个地址。

use gld_core::app::{WorkspaceTarget, WorkspaceUpdate};
use gld_core::runtime::ServiceKind;
use gld_core::tunnel::{TunnelServiceKind, TunnelStatus};
use gld_core::workspace::{RuntimeStatusDto, WorkspaceProfile};
use gld_daemon::Request;

use super::{service, Ctx};
use crate::cli::{ConnectArgs, ExposeArgs, TunnelService};
use crate::error::{CliError, CliResult};

pub async fn run(ctx: &mut Ctx, args: ExposeArgs) -> CliResult {
    let profile: WorkspaceProfile = ctx
        .backend
        .call_typed(Request::ResolveWorkspace {
            target: ctx.target.clone(),
        })
        .await?;
    // 后面每一步都锁死同一个工作区：中途按目录重新推断的话，
    // 配置和启动有可能落到两个工作区上。
    let target = WorkspaceTarget::selector(profile.id.clone());
    let (service_kind, tunnel_kind, prefix) = match args.service {
        TunnelService::Mcp => (ServiceKind::Mcp, TunnelServiceKind::Mcp, "mcp."),
        TunnelService::Actions => (ServiceKind::Actions, TunnelServiceKind::Actions, "actions."),
    };

    let assignments = assignments(&args, &profile, prefix)?;
    let update: WorkspaceUpdate = ctx
        .backend
        .call_typed(Request::SetWorkspaceFields {
            target: target.clone(),
            assignments,
        })
        .await?;
    for failure in &update.restart_failures {
        ctx.out
            .line(format!("{} {}", ctx.out.red("✗"), failure.error));
    }
    if !update.restart_failures.is_empty() {
        return Err(CliError::new(
            "隧道配置已保存，但服务没能重启起来。按上面的错误修好后重试。",
        ));
    }

    if args.off {
        return report_off(ctx, &target, service_kind).await;
    }

    // 服务没在跑就先起来：隧道是把本地端口转出去的，本地没人听，
    // 公网地址拿到了也只会 502。
    let status: RuntimeStatusDto = ctx
        .backend
        .call_typed(Request::ServiceStatus {
            target: target.clone(),
            kind: service_kind,
        })
        .await?;
    if status.state == "stopped" {
        ctx.backend
            .call(Request::StartService {
                target: target.clone(),
                kind: service_kind,
            })
            .await?;
    }

    // --url 是"我自己已经有公网地址了"，没有隧道要起。
    if args.url.is_none() {
        // start 里也会顺带起一次隧道，但那次失败只写日志（不能让隧道问题
        // 把服务一起拖垮）。这里显式再起一次，为的是把真正的报错拿到手上——
        // 没装 cloudflared 之类的问题，用户需要当场看到，而不是去翻日志。
        let tunnel: TunnelStatus = ctx
            .backend
            .call_typed(Request::TunnelStart {
                target: target.clone(),
                kind: tunnel_kind,
            })
            .await?;
        if tunnel.public_url.is_empty() {
            return Err(CliError::new(format!(
                "隧道状态是 {}，但没拿到公网地址。`gld logs -n 30` 看隧道输出。",
                tunnel.state
            )));
        }
    }

    // 地址、认证方式、凭据统一由 connect 渲染：两处各印一份迟早对不上。
    service::connect(ctx, ConnectArgs { reveal: false }).await
}

/// 把命令行选项翻译成 `ws set` 的字段。
///
/// 每一种模式都会显式写全三件事（隧道类型、公网地址、走不走全局入口），
/// 否则从别的模式切过来会留下上一次的残值——例如从 frp 换到 cloudflare，
/// 旧的 public_url 还挂在那里，`gld connect` 会显示一个已经失效的地址。
fn assignments(
    args: &ExposeArgs,
    profile: &WorkspaceProfile,
    prefix: &str,
) -> CliResult<Vec<(String, String)>> {
    let field = |name: &str, value: String| (format!("{prefix}{name}"), value);
    let mut pairs = vec![field("global-gateway", "false".into())];

    if args.off {
        pairs.push(field("tunnel", "none".into()));
        pairs.push(field("public-url", String::new()));
        return Ok(pairs);
    }
    if let Some(url) = &args.url {
        pairs.push(field("tunnel", "none".into()));
        pairs.push(field("public-url", url.clone()));
        return Ok(pairs);
    }
    if let Some(frp) = &args.frp {
        let subdomain = match &args.subdomain {
            Some(value) => value.clone(),
            None => slugify(&profile.name).ok_or_else(|| {
                CliError::new(format!(
                    "工作区名「{}」里没有可用作子域名的字符，请显式给一个：\n  \
                     gld expose --frp {frp} --subdomain <小写字母/数字/连字符>",
                    profile.name
                ))
            })?,
        };
        pairs.push(field("tunnel", "frp".into()));
        pairs.push(field("frp-profile", frp.clone()));
        pairs.push(field("frp-subdomain", subdomain));
        pairs.push(field("public-url", String::new()));
        return Ok(pairs);
    }

    pairs.push(field("tunnel", "cloudflare".into()));
    pairs.push(field(
        "cloudflare-mode",
        if args.named { "named" } else { "quick" }.into(),
    ));
    pairs.push(field("public-url", String::new()));
    Ok(pairs)
}

async fn report_off(ctx: &mut Ctx, target: &WorkspaceTarget, kind: ServiceKind) -> CliResult {
    let status: RuntimeStatusDto = ctx
        .backend
        .call_typed(Request::ServiceStatus {
            target: target.clone(),
            kind,
        })
        .await?;
    if ctx.out.json_or(&status) {
        return Ok(());
    }
    ctx.out.line("已关闭公网入口。");
    if !status.local_endpoint.is_empty() {
        ctx.out
            .line(format!("本地地址仍然可用：{}", status.local_endpoint));
    }
    Ok(())
}

/// 工作区名 → 子域名。非法字符换成连字符，连续的合并成一个。
///
/// 中文名（`我的项目`）会被整段吃掉，返回 None，这时让用户自己给一个，
/// 而不是拼出一个空子域名让 frps 去报错。
fn slugify(name: &str) -> Option<String> {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = trim_dashes(&out);
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn trim_dashes(value: &str) -> &str {
    value.trim_matches('-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_makes_a_usable_subdomain_or_gives_up() {
        assert_eq!(slugify("my-project").as_deref(), Some("my-project"));
        assert_eq!(slugify("My Project").as_deref(), Some("my-project"));
        assert_eq!(slugify("a__b..c").as_deref(), Some("a-b-c"));
        assert_eq!(slugify("_edge_").as_deref(), Some("edge"));
        // 纯中文名拼不出子域名，得让用户自己给，而不是拼个空串。
        assert_eq!(slugify("我的项目"), None);
        assert_eq!(slugify(""), None);
    }
}

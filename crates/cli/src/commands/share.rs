//! `gld share`：一条命令拿到公网地址。
//!
//! 这个命令不做任何别处做不到的事，它只是把最常走的那条路合成一步。
//! 以前接 ChatGPT 要敲：
//!
//! ```text
//! gld ws set mcp.tunnel=cloudflare mcp.cloudflare-mode=quick
//! gld restart          # 忘了这步就是"配了没反应"
//! gld ls               # 才看得到公网地址
//! ```
//!
//! 三条里有两条是"仪式"：用户想要的只是那个地址。
//!
//! 这里的 `--tunnel` 解析和套用逻辑也被 `gld start --tunnel` 和
//! `gld upgrade --tunnel` 复用——三个命令说的是同一件事，不该有三套规则。

use gld_core::app::{WorkspaceTarget, WorkspaceUpdate};
use gld_core::runtime::ServiceKind;
use gld_core::tunnel::{TunnelServiceKind, TunnelStatus};
use gld_core::workspace::{RuntimeStatusDto, WorkspaceProfile};
use gld_daemon::Request;

use super::{service, Ctx};
use crate::cli::{ListArgs, ShareArgs, TunnelService, TunnelSpec};
use crate::error::{CliError, CliResult};

pub async fn run(ctx: &mut Ctx, args: ShareArgs) -> CliResult {
    let profile = service::resolve_or_register(
        ctx,
        args.path.as_deref(),
        gld_core::app::WorkspaceCreateOptions::default(),
    )
    .await?;
    // 后面每一步都锁死同一个工作区：中途按目录重新推断的话，
    // 配置和启动有可能落到两个工作区上。
    let target = WorkspaceTarget::selector(profile.id.clone());
    let spec = match (args.off, args.tunnel) {
        (true, _) => TunnelSpec::Off,
        (false, Some(spec)) => spec,
        // 不带参数就是"给我个能贴进 ChatGPT 的地址"——Cloudflare 临时隧道零配置。
        (false, None) => TunnelSpec::Cloudflare {
            named: false,
            domain: None,
        },
    };

    configure(
        ctx,
        &target,
        &profile,
        &spec,
        args.subdomain.as_deref(),
        args.tunnel_token.as_deref(),
        args.service,
    )
    .await?;

    let service_kind = service_kind(args.service);
    if matches!(spec, TunnelSpec::Off) {
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

    ensure_tunnel_up(ctx, &target, &spec, args.service).await?;

    // 地址、认证方式、凭据统一由 list 渲染：两处各印一份迟早对不上。
    service::show_detail(ctx, &target, ListArgs::default()).await
}

pub fn service_kind(service: TunnelService) -> ServiceKind {
    match service {
        TunnelService::Mcp => ServiceKind::Mcp,
        TunnelService::Actions => ServiceKind::Actions,
    }
}

fn tunnel_kind(service: TunnelService) -> TunnelServiceKind {
    match service {
        TunnelService::Mcp => TunnelServiceKind::Mcp,
        TunnelService::Actions => TunnelServiceKind::Actions,
    }
}

/// 把 `--tunnel` 写进工作区配置（正在跑的服务会被顺带重启）。
pub async fn configure(
    ctx: &mut Ctx,
    target: &WorkspaceTarget,
    profile: &WorkspaceProfile,
    spec: &TunnelSpec,
    subdomain: Option<&str>,
    token: Option<&str>,
    service: TunnelService,
) -> CliResult<()> {
    // token 必须赶在写配置之前落实：配置一写进去，正在跑的服务就会带着新的
    // named 模式重启，而那一步没有 token 是起不来的。
    ensure_cloudflare_token(ctx, target, spec, token, service).await?;
    let prefix = match service {
        TunnelService::Mcp => "mcp.",
        TunnelService::Actions => "actions.",
    };
    let assignments = assignments(spec, subdomain, profile, prefix, service)?;
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
    Ok(())
}

/// Cloudflare 固定域名要用的 Tunnel Token：命令行给了就存下来，没给也没配过就当场问。
///
/// 以前这个值只能事先 `gld secret set cloudflare_token <token>`，忘了的话
/// `gld start <目录> --tunnel cf:<域名>` 会一路成功到最后一步才炸：工作区登记了、
/// 配置写了、服务起来了，然后"错误：Cloudflare 命名隧道模式需要填写 Tunnel Token"。
/// 前面每一步都打的是成功，用户只会以为整条命令失败了，于是原样再跑一遍。
async fn ensure_cloudflare_token(
    ctx: &mut Ctx,
    target: &WorkspaceTarget,
    spec: &TunnelSpec,
    token: Option<&str>,
    service: TunnelService,
) -> CliResult<()> {
    let named = matches!(spec, TunnelSpec::Cloudflare { named: true, .. });
    if !named {
        if token.is_some() {
            return Err(CliError::new(
                "--tunnel-token 是 Cloudflare 固定域名用的，配合 --tunnel cf:<域名>。\n  \
                 临时地址（--tunnel cf）不需要它；FRP 的 token 配在 gld frp add --token 里。",
            ));
        }
        return Ok(());
    }

    let key = match service {
        TunnelService::Mcp => "cloudflare_token",
        TunnelService::Actions => "actions_cloudflare_token",
    };
    let value = match token {
        Some(given) => given.trim().to_string(),
        None => {
            let saved: Option<String> = ctx
                .backend
                .call_typed(Request::WorkspaceSecret {
                    target: target.clone(),
                    key: key.into(),
                })
                .await?;
            if saved.is_some_and(|value| !value.trim().is_empty()) {
                return Ok(());
            }
            crate::prompt::secret(
                "Cloudflare Tunnel Token（Zero Trust 后台复制，输入不显示）：",
                &format!(
                    "Cloudflare 固定域名要 Tunnel Token。非交互环境这样给：\n  \
                     gld secret set {key} <token>\n  \
                     或者在命令里带上：--tunnel-token <token>"
                ),
            )?
        }
    };
    if value.is_empty() {
        return Err(CliError::new(
            "Tunnel Token 是空的，没往下走。临时地址不需要 token：--tunnel cf",
        ));
    }

    ctx.backend
        .call(Request::SetWorkspaceSecret {
            target: target.clone(),
            key: key.into(),
            value,
        })
        .await?;
    Ok(())
}

/// 显式起一次隧道，把真正的报错拿到手上。
///
/// `start` 内部也会顺带起隧道，但那次失败只写日志（不能让隧道问题把服务
/// 一起拖垮）。没装 cloudflared 之类的问题，用户需要当场看到，
/// 而不是对着一句"启动成功"去翻日志找为什么没有地址。
pub async fn ensure_tunnel_up(
    ctx: &mut Ctx,
    target: &WorkspaceTarget,
    spec: &TunnelSpec,
    service: TunnelService,
) -> CliResult<()> {
    // Url 是"我自己已经有公网地址了"，Off 是不要地址，两种都没有隧道要起。
    if matches!(spec, TunnelSpec::Url(_) | TunnelSpec::Off) {
        return Ok(());
    }
    let tunnel: TunnelStatus = ctx
        .backend
        .call_typed(Request::TunnelStart {
            target: target.clone(),
            kind: tunnel_kind(service),
        })
        .await?;
    if tunnel.public_url.is_empty() {
        return Err(CliError::new(format!(
            "隧道状态是 {}，但没拿到公网地址。`gld logs -n 30` 看隧道输出。",
            tunnel.state
        )));
    }
    Ok(())
}

/// 把 `--tunnel` 翻译成 `ws set` 的字段。
///
/// 每一种模式都会显式写全三件事（隧道类型、公网地址、走不走全局入口），
/// 否则从别的模式切过来会留下上一次的残值——例如从 frp 换到 cloudflare，
/// 旧的 public_url 还挂在那里，`gld list` 会显示一个已经失效的地址。
fn assignments(
    spec: &TunnelSpec,
    subdomain: Option<&str>,
    profile: &WorkspaceProfile,
    prefix: &str,
    service: TunnelService,
) -> CliResult<Vec<(String, String)>> {
    let field = |name: &str, value: String| (format!("{prefix}{name}"), value);
    let mut pairs = vec![field("global-gateway", "false".into())];

    if let (Some(sub), false) = (subdomain, matches!(spec, TunnelSpec::Frp { .. })) {
        return Err(CliError::new(format!(
            "--subdomain {sub} 只对 FRP 有意义。要固定域名：--tunnel frp:<配置名> --subdomain {sub}"
        )));
    }

    match spec {
        TunnelSpec::Off => {
            pairs.push(field("tunnel", "none".into()));
            pairs.push(field("public-url", String::new()));
        }
        TunnelSpec::Url(url) => {
            pairs.push(field("tunnel", "none".into()));
            pairs.push(field("public-url", public_base(url, service)));
        }
        TunnelSpec::Frp { profile: frp } => {
            let sub = match subdomain {
                Some(value) => value.to_string(),
                None => slugify(&profile.name).ok_or_else(|| {
                    CliError::new(format!(
                        "工作区名「{}」里没有可用作子域名的字符，请显式给一个：\n  \
                         gld share --tunnel frp:{frp} --subdomain <小写字母/数字/连字符>",
                        profile.name
                    ))
                })?,
            };
            pairs.push(field("tunnel", "frp".into()));
            pairs.push(field("frp-profile", frp.clone()));
            pairs.push(field("frp-subdomain", sub));
            pairs.push(field("public-url", String::new()));
        }
        TunnelSpec::Cloudflare { named: false, .. } => {
            pairs.push(field("tunnel", "cloudflare".into()));
            pairs.push(field("cloudflare-mode", "quick".into()));
            // quick 的地址是每次启动现拿的，留着上一种模式的固定地址只会让
            // `gld list` 显示一个早就失效的域名。
            pairs.push(field("public-url", String::new()));
        }
        TunnelSpec::Cloudflare {
            named: true,
            domain,
        } => {
            pairs.push(field("tunnel", "cloudflare".into()));
            pairs.push(field("cloudflare-mode", "named".into()));
            match domain {
                // cf:<域名>：这次顺手把它定下来。
                Some(domain) => pairs.push(field("public-url", public_base(domain, service))),
                // cf:named：沿用已经配好的那个，绝不能清掉——named 模式没有
                // 对外地址就起不来（cloudflared 要拿它建 ingress，OAuth 元数据
                // 和 OpenAPI 文档里也要写它）。
                None if current_public_url(profile, service).is_empty() => {
                    return Err(CliError::new(
                        "Cloudflare 固定域名模式需要一个对外域名。连域名一起给：\n  \
                         gld share --tunnel cf:mcp.example.com\n\
                         临时地址（每次重启都会变）用：gld share --tunnel cf",
                    ))
                }
                None => {}
            }
        }
    }
    Ok(pairs)
}

/// 这条线路当前配着的固定公网地址。
fn current_public_url(profile: &WorkspaceProfile, service: TunnelService) -> &str {
    match service {
        TunnelService::Mcp => profile.tunnel.public_url.trim(),
        TunnelService::Actions => profile.actions.public_url.trim(),
    }
}

/// 用户贴过来的地址 → 配置里要存的"基地址"。
///
/// 存的是基地址，具体端点是拼出来的（MCP 拼 `/mcp`，Actions 拼 `/openapi.json`）。
/// 而用户手里那个地址往往就是从客户端里复制的完整端点，直接存下去会变成
/// `https://x.com/mcp/mcp`——客户端 404，且看不出哪里错了。
fn public_base(url: &str, service: TunnelService) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    // `cf:mcp.example.com` 里的域名是裸的。配置字段要求带协议头（不带的话
    // 客户端连不上而且没有任何提示），这里补上——公网入口一律按 https 算。
    let with_scheme = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let suffix = match service {
        TunnelService::Mcp => "/mcp",
        TunnelService::Actions => "/openapi.json",
    };
    with_scheme
        .strip_suffix(suffix)
        .unwrap_or(&with_scheme)
        .trim_end_matches('/')
        .to_string()
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
    use std::str::FromStr;

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

    #[test]
    fn tunnel_spec_reads_every_documented_form() {
        let parse = TunnelSpec::from_str;
        assert_eq!(
            parse("https://x.com/mcp").unwrap(),
            TunnelSpec::Url("https://x.com/mcp".into())
        );
        assert_eq!(
            parse("cf").unwrap(),
            TunnelSpec::Cloudflare {
                named: false,
                domain: None
            }
        );
        // 光写 named = 沿用工作区里已经配好的域名。
        assert_eq!(
            parse("cf:named").unwrap(),
            TunnelSpec::Cloudflare {
                named: true,
                domain: None
            }
        );
        // cf:<域名> = 固定域名模式，顺手把域名定下来。
        assert_eq!(
            parse("cf:mcp.example.com").unwrap(),
            TunnelSpec::Cloudflare {
                named: true,
                domain: Some("mcp.example.com".into())
            }
        );
        // 带协议头的写法也认；域名里的大写要原样留着。
        assert_eq!(
            parse("cf:https://MCP.Example.com").unwrap(),
            TunnelSpec::Cloudflare {
                named: true,
                domain: Some("https://MCP.Example.com".into())
            }
        );
        // 字段表里写的是 tunnel=cloudflare，全称也得认，否则两处对不上。
        assert_eq!(
            parse("cloudflare").unwrap(),
            TunnelSpec::Cloudflare {
                named: false,
                domain: None
            }
        );
        assert_eq!(
            parse("frp:公司").unwrap(),
            TunnelSpec::Frp {
                profile: "公司".into()
            }
        );
        assert_eq!(parse("off").unwrap(), TunnelSpec::Off);
        assert_eq!(parse("none").unwrap(), TunnelSpec::Off);

        // 光写 frp 没法知道用哪台服务器，要当场说清楚怎么补。
        assert!(parse("frp").unwrap_err().contains("frp:公司"));
        // 手滑拼错的模式名不能被当成域名默默吃下去——它不含点，不像域名。
        let error = parse("cf:nmaed").unwrap_err();
        assert!(error.contains("quick、named 或一个固定域名"), "{error}");
        // 忘了协议头的地址不能被当成模式名默默吃掉。
        assert!(parse("mcp.example.com").unwrap_err().contains("看不懂"));
    }

    /// 用户手里的地址通常是从客户端复制的完整端点，存下去前要去掉端点后缀。
    #[test]
    fn a_pasted_endpoint_is_stored_as_a_base_url() {
        assert_eq!(
            public_base("https://x.com/mcp", TunnelService::Mcp),
            "https://x.com"
        );
        assert_eq!(
            public_base("https://x.com/gld/mcp/", TunnelService::Mcp),
            "https://x.com/gld"
        );
        // 不是端点后缀的就别动它。
        assert_eq!(
            public_base("https://x.com/", TunnelService::Mcp),
            "https://x.com"
        );
        assert_eq!(
            public_base("https://x.com/openapi.json", TunnelService::Actions),
            "https://x.com"
        );
        // MCP 侧不该去动 Actions 的后缀，反之亦然。
        assert_eq!(
            public_base("https://x.com/mcp", TunnelService::Actions),
            "https://x.com/mcp"
        );
    }

    #[test]
    fn a_subdomain_without_frp_is_rejected_instead_of_silently_dropped() {
        let profile = WorkspaceProfile::new("/tmp/x".into(), Some("api".into()));
        let error = assignments(
            &TunnelSpec::Cloudflare {
                named: false,
                domain: None,
            },
            Some("demo"),
            &profile,
            "mcp.",
            TunnelService::Mcp,
        )
        .unwrap_err()
        .message;
        assert!(error.contains("只对 FRP 有意义"), "{error}");
    }

    /// 换模式必须把上一种模式的残值清掉，否则 ls 会显示一个已经失效的地址。
    #[test]
    fn every_mode_writes_all_three_fields() {
        let profile = WorkspaceProfile::new("/tmp/x".into(), Some("api".into()));
        for spec in [
            TunnelSpec::Off,
            TunnelSpec::Url("https://x.com/mcp".into()),
            TunnelSpec::Cloudflare {
                named: false,
                domain: None,
            },
            TunnelSpec::Cloudflare {
                named: true,
                domain: Some("mcp.example.com".into()),
            },
            TunnelSpec::Frp {
                profile: "office".into(),
            },
        ] {
            let pairs = assignments(&spec, None, &profile, "mcp.", TunnelService::Mcp)
                .expect("assignments");
            let keys: Vec<&str> = pairs.iter().map(|(key, _)| key.as_str()).collect();
            assert!(keys.contains(&"mcp.global-gateway"), "{spec:?} → {keys:?}");
            assert!(keys.contains(&"mcp.tunnel"), "{spec:?} → {keys:?}");
            assert!(keys.contains(&"mcp.public-url"), "{spec:?} → {keys:?}");
        }
    }

    fn public_url_of(pairs: &[(String, String)]) -> Option<&str> {
        pairs
            .iter()
            .find(|(key, _)| key == "mcp.public-url")
            .map(|(_, value)| value.as_str())
    }

    /// `cf:<域名>` 一步把固定域名定下来；裸域名要补上协议头。
    ///
    /// 不补的话字段校验会以"要带协议头"拒绝，而用户只是照着 `--tunnel cf:<域名>`
    /// 的提示写的。
    #[test]
    fn cf_with_a_domain_stores_it_with_a_scheme() {
        let profile = WorkspaceProfile::new("/tmp/x".into(), Some("api".into()));
        for written in ["mcp.example.com", "https://mcp.example.com/mcp"] {
            let pairs = assignments(
                &TunnelSpec::Cloudflare {
                    named: true,
                    domain: Some(written.into()),
                },
                None,
                &profile,
                "mcp.",
                TunnelService::Mcp,
            )
            .expect("assignments");
            assert_eq!(
                public_url_of(&pairs),
                Some("https://mcp.example.com"),
                "{written}"
            );
        }
    }

    /// `cf:named` 沿用已经配好的域名——绝不能像别的模式那样清掉它。
    ///
    /// 这里曾经无条件写空：切到固定域名模式的同时把域名抹了，隧道必然起不来，
    /// 报的还是"命名隧道模式需要填写固定公网地址"——而用户刚刚才配过它。
    #[test]
    fn cf_named_keeps_the_domain_that_is_already_configured() {
        let mut profile = WorkspaceProfile::new("/tmp/x".into(), Some("api".into()));
        profile.tunnel.public_url = "https://mcp.example.com".into();

        let pairs = assignments(
            &TunnelSpec::Cloudflare {
                named: true,
                domain: None,
            },
            None,
            &profile,
            "mcp.",
            TunnelService::Mcp,
        )
        .expect("assignments");

        assert_eq!(public_url_of(&pairs), None, "不该去动已经配好的域名");
    }

    /// 一个域名都没有时当场说清楚怎么给，而不是等 cloudflared 报一句没头没脑的错。
    #[test]
    fn cf_named_without_any_domain_says_how_to_give_one() {
        let profile = WorkspaceProfile::new("/tmp/x".into(), Some("api".into()));
        let error = assignments(
            &TunnelSpec::Cloudflare {
                named: true,
                domain: None,
            },
            None,
            &profile,
            "mcp.",
            TunnelService::Mcp,
        )
        .unwrap_err()
        .message;
        assert!(error.contains("cf:mcp.example.com"), "{error}");
    }
}

use gld_core::app::{HubMemberDto, HubMembershipChange, HubStatusDto, WorkspaceTarget};
use gld_core::settings::HubConfig;
use gld_daemon::Request;
use serde_json::json;

use super::Ctx;
use crate::cli::{HubCmd, HubSetArgs};
use crate::error::{CliError, CliResult};
use crate::output::{mask, or_dash, yes_no};

pub async fn run(ctx: &mut Ctx, command: HubCmd) -> CliResult {
    match command {
        HubCmd::Show { reveal } => show(ctx, reveal).await,
        HubCmd::Add { workspaces } => {
            let targets = targets(ctx, workspaces);
            let change: HubMembershipChange = ctx
                .backend
                .call_typed(Request::HubAddMembers { targets })
                .await?;
            print_change(ctx, &change, "已加入", "本来就在 hub 里")
        }
        HubCmd::Remove { workspaces } => {
            let targets = targets(ctx, workspaces);
            let change: HubMembershipChange = ctx
                .backend
                .call_typed(Request::HubRemoveMembers { targets })
                .await?;
            print_change(ctx, &change, "已移出", "本来就不在 hub 里")
        }
        HubCmd::Set(args) => {
            let current: HubStatusDto = ctx.backend.call_typed(Request::HubStatus).await?;
            let mut config = current.config;
            apply(&mut config, args)?;
            let status: HubStatusDto = ctx
                .backend
                .call_typed(Request::SetHubConfig { config })
                .await?;
            if ctx.out.json_or(&status) {
                return Ok(());
            }
            if status.state == "running" {
                ctx.out
                    .line("已保存聚合入口配置，hub 正在运行，已按新配置重启。");
            } else {
                ctx.out
                    .line("已保存聚合入口配置。hub 没在运行，下次 gld hub start 时生效。");
            }
            print_endpoints(ctx, &status);
            Ok(())
        }
        HubCmd::Start => {
            let status: HubStatusDto = ctx.backend.call_typed(Request::HubStart).await?;
            if ctx.out.json_or(&status) {
                return Ok(());
            }
            print_endpoints(ctx, &status);
            if status.members.is_empty() {
                ctx.out.note(
                    "hub 还没有成员，客户端连上来也什么都访问不了。先 gld hub add <工作区>（立即生效，不用重启）。",
                );
            }
            ctx.out
                .line(ctx.out.dim("凭据和成员：gld hub show（--reveal 显示明文）"));
            Ok(())
        }
        HubCmd::Stop => {
            let status: HubStatusDto = ctx.backend.call_typed(Request::HubStop).await?;
            if ctx.out.json_or(&status) {
                return Ok(());
            }
            ctx.out.kv(&[("状态", ctx.out.state(&status.state))]);
            Ok(())
        }
        HubCmd::Regenerate { key } => {
            let value: String = ctx
                .backend
                .call_typed(Request::RegenerateHubSecret { key: key.clone() })
                .await?;
            if ctx.out.json_or(&json!({ "key": key, "value": value })) {
                return Ok(());
            }
            ctx.out.kv(&[(key.as_str(), value)]);
            // 换不同的凭据，客户端那边要做的事完全不一样。笼统地说"记得更新客户端"的话，
            // OAuth 用户会去找一个根本不存在的配置项。
            ctx.out.note(match key.as_str() {
                "bearer_token" => "客户端里配的 Bearer Token 要换成上面的新值，旧值已经失效。",
                "oauth_token_secret" => {
                    "已发出的 OAuth 令牌全部失效：每个连着 hub 的客户端都要重新授权一次。"
                }
                "oauth_password" => {
                    "已经授权过的客户端不受影响；下次授权时输入新口令。要把已授权的踢下线，换 oauth_token_secret。"
                }
                _ => "只影响手填了静态 Client ID 的客户端；ChatGPT 这类自动注册的不受影响。",
            });
            Ok(())
        }
    }
}

async fn show(ctx: &mut Ctx, reveal: bool) -> CliResult {
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
        "status": status,
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
        ("认证方式", status.config.auth_type.clone()),
    ];
    rows.extend(credentials);
    rows.push((
        "工具集",
        format!(
            "{}（成员自己的工具集照样生效，取交集）",
            status.config.tool_profile
        ),
    ));
    rows.push((
        "经全局入口",
        yes_no(status.config.use_global_gateway).to_string(),
    ));
    out.kv(&rows);
    out.line("");
    if status.members.is_empty() {
        out.line("成员：还没有。gld hub add <工作区> 把工作区加进来。");
    } else {
        out.line(out.bold(&format!("成员（{}）", status.members.len())));
        let rows: Vec<Vec<String>> = status
            .members
            .iter()
            .map(|member| {
                vec![
                    member.name.clone(),
                    gld_core::short_id(&member.id).to_string(),
                    member.tool_profile.clone(),
                    member.path.clone(),
                ]
            })
            .collect();
        out.table(&["名称", "ID", "工具集", "路径"], &rows);
    }
    out.line("");
    out.line(out.dim(
        "客户端里只配上面这一条地址。AI 每次调用都要带 workspace 参数（成员名称或 id），说明见 docs/concepts.md",
    ));
    Ok(())
}

/// 每种认证方式下，客户端要填的凭据。标签和 `gld list` 保持一致。
fn credential_keys(auth_type: &str) -> &'static [(&'static str, &'static str)] {
    match auth_type {
        "oauth" => &[
            ("OAuth Client ID", "oauth_client_id"),
            ("授权口令 (oauth_password)", "oauth_password"),
        ],
        "bearer" => &[("Bearer Token", "bearer_token")],
        _ => &[],
    }
}

/// 给了工作区就逐个解析；一个没给就和其他命令一样，按 -w / 当前目录推断。
///
/// 相对路径形式的工作区要按调用方目录解析，所以每个 target 都带上 cwd。
fn targets(ctx: &Ctx, workspaces: Vec<String>) -> Vec<WorkspaceTarget> {
    if workspaces.is_empty() {
        return vec![ctx.target.clone()];
    }
    workspaces
        .into_iter()
        .map(|selector| WorkspaceTarget::new(Some(selector), ctx.target.cwd.clone()))
        .collect()
}

fn print_change(
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
    let members = if change.status.members.is_empty() {
        "无".to_string()
    } else {
        names(&change.status.members)
    };
    ctx.out.line(format!("当前成员：{members}"));
    // 成员表是 hub 每次请求现读的。不说清楚的话，用户会顺手 gld hub start 重启一遍，
    // 白白掉一次客户端连接。
    if change.status.state == "running" {
        ctx.out
            .line(ctx.out.dim("hub 正在运行，下一次调用就生效，不用重启。"));
    }
    Ok(())
}

fn names(members: &[HubMemberDto]) -> String {
    members
        .iter()
        .map(|member| {
            if member.name.is_empty() {
                // 工作区已经不在了、只剩 id 的成员。
                member.id.clone()
            } else {
                member.name.clone()
            }
        })
        .collect::<Vec<_>>()
        .join("、")
}

fn print_endpoints(ctx: &Ctx, status: &HubStatusDto) {
    ctx.out.kv(&[
        (
            "状态",
            format!("{}  {}", ctx.out.state(&status.state), status.detail)
                .trim_end()
                .to_string(),
        ),
        ("本地地址", status.local_endpoint.clone()),
        ("公网地址", or_dash(&status.public_endpoint)),
        ("认证方式", status.config.auth_type.clone()),
        ("成员", status.members.len().to_string()),
    ]);
}

fn apply(config: &mut HubConfig, args: HubSetArgs) -> CliResult {
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

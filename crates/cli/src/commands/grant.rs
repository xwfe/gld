//! `gld grant`：只开部分项目的凭据（RFC-0007）。

use gld_core::app::{
    BrowserGrantCreatedDto, BrowserGrantSpec, GrantDto, GrantRemoved, GrantSpec, HubStatusDto,
};
use gld_daemon::Request;

use super::Ctx;
use crate::cli::GrantCmd;
use crate::error::{CliError, CliResult};
use crate::output::mask;

pub async fn run(ctx: &mut Ctx, command: GrantCmd) -> CliResult {
    match command {
        GrantCmd::Add {
            name,
            projects,
            write,
        } => add(ctx, name, projects, write).await,
        GrantCmd::Browser {
            name,
            projects,
            all,
            ttl,
            origin,
            write,
        } => {
            let projects = if all {
                all_members(ctx).await?
            } else {
                projects
            };
            browser(ctx, name, projects, ttl, origin, write).await
        }
        GrantCmd::List { reveal } => list(ctx, reveal).await,
        GrantCmd::Remove { name } => remove(ctx, name).await,
    }
}

async fn add(ctx: &mut Ctx, name: String, projects: Vec<String>, writable: bool) -> CliResult {
    let grant: GrantDto = ctx
        .backend
        .call_typed(Request::AddGrant {
            spec: GrantSpec {
                name,
                workspaces: projects,
                writable,
            },
        })
        .await?;
    if ctx.out.json_or(&grant) {
        return Ok(());
    }
    let status: HubStatusDto = ctx.backend.call_typed(Request::HubStatus).await?;
    ctx.out.line(format!(
        "已建 grant「{}」：只开 {}，{}。",
        grant.name,
        projects_text(&grant),
        access_text(&grant)
    ));
    let endpoint = if status.public_endpoint.is_empty() {
        status.local_endpoint.clone()
    } else {
        status.public_endpoint.clone()
    };
    let bearer = status.config.auth_type == "bearer";
    ctx.out.kv(&[
        ("地址（和服务一样）", endpoint),
        if bearer {
            ("Bearer 令牌", grant.bearer_token.clone())
        } else {
            ("授权口令", grant.oauth_password.clone())
        },
    ]);
    ctx.out.note(if bearer {
        "客户端的 Authorization 头带 Bearer <上面的令牌>。服务自己的令牌照旧管全部项目。"
    } else {
        "客户端连这个地址，OAuth 授权页填上面的口令（不是服务口令）。服务自己的口令照旧管全部项目。"
    });
    if !grant.read_only {
        ctx.out.note(
            "这把能跑命令：它能以你的身份读到 gld 数据目录里的服务口令，等于全权。只发给你愿意把服务口令交给的人。",
        );
    }
    ctx.out
        .note("要再看口令和令牌：gld grant ls --reveal。作废：gld grant rm <名字>。");
    Ok(())
}

async fn browser(
    ctx: &mut Ctx,
    name: String,
    projects: Vec<String>,
    ttl_seconds: u64,
    allowed_origins: Vec<String>,
    writable: bool,
) -> CliResult {
    let created: BrowserGrantCreatedDto = ctx
        .backend
        .call_typed(Request::AddBrowserGrant {
            spec: BrowserGrantSpec {
                name,
                workspaces: projects,
                writable,
                ttl_seconds,
                allowed_origins,
            },
        })
        .await?;
    if ctx.out.json_or(&created) {
        return Ok(());
    }
    ctx.out.line(format!(
        "已建浏览器 grant「{}」：只开 {}，{}。",
        created.grant.name,
        projects_text(&created.grant),
        access_text(&created.grant)
    ));
    let mut fields = vec![
        ("本地连接地址", created.local_url),
        ("到期", expiry_text(created.expires_at_unix_secs)),
    ];
    if let Some(url) = created.public_url {
        fields.push(("公网连接地址", url));
    }
    ctx.out.kv(&fields);
    ctx.out
        .note("SuperAssistant 选择 Streamable HTTP，粘贴完整地址。同一台电脑用本地地址。");
    ctx.out.note("完整 URL 就是凭据，只显示这一次；不要贴进对话、截图或访问日志。到期后重新创建并替换扩展里的 URL。");
    if !created.grant.read_only {
        ctx.out.note("这把能跑命令：它能以你的身份读到 gld 数据目录里的服务口令，等于全权。只给你信得过的客户端。");
    }
    ctx.out
        .note("查看授权和到期：gld grant ls。立即作废并停掉它起的命令：gld grant rm <名字>。");
    Ok(())
}

/// `--all` 在命令行这边展开成服务现有成员的 id：守护进程照旧逐个校验，IPC 不变，
/// 在跑的守护进程不用重启就能用。展开的是此刻的名单，之后新加的项目不会自动进来。
async fn all_members(ctx: &mut Ctx) -> CliResult<Vec<String>> {
    let status: HubStatusDto = ctx.backend.call_typed(Request::HubStatus).await?;
    if status.members.is_empty() {
        return Err(CliError::new(
            "服务里还没有项目，先 gld add <目录> 登记一个",
        ));
    }
    Ok(status.members.into_iter().map(|member| member.id).collect())
}

async fn list(ctx: &mut Ctx, reveal: bool) -> CliResult {
    let mut grants: Vec<GrantDto> = ctx.backend.call_typed(Request::ListGrants).await?;
    if !reveal {
        for grant in &mut grants {
            grant.oauth_password = mask(&grant.oauth_password);
            grant.bearer_token = mask(&grant.bearer_token);
        }
    }
    if ctx.out.json_or(&grants) {
        return Ok(());
    }
    if grants.is_empty() {
        ctx.out.line(
            "还没有 grant：服务的凭据管全部项目。要给别人只开几个：gld grant add <名字> <项目>…",
        );
        return Ok(());
    }
    let rows: Vec<Vec<String>> = grants
        .iter()
        .map(|grant| {
            vec![
                grant.name.clone(),
                projects_text(grant),
                access_text(grant).to_string(),
                if grant.browser.is_some() {
                    "浏览器 URL"
                } else {
                    "OAuth / bearer"
                }
                .to_string(),
                grant
                    .browser
                    .as_ref()
                    .map(|browser| expiry_text(browser.expires_at_unix_secs))
                    .unwrap_or_else(|| "不限期".into()),
                grant.oauth_password.clone(),
                grant.bearer_token.clone(),
            ]
        })
        .collect();
    ctx.out.table(
        &[
            "名称",
            "项目",
            "权限",
            "类型",
            "到期",
            "授权口令",
            "Bearer 令牌",
        ],
        &rows,
    );
    if !reveal {
        ctx.out
            .note("已脱敏，--reveal 显示普通 grant 的口令和令牌明文。");
    }
    if grants.iter().any(|grant| grant.browser.is_some()) {
        ctx.out
            .note("浏览器 URL 只在创建时显示；--reveal 也不能恢复。到期后删除并重新创建。");
    }
    Ok(())
}

async fn remove(ctx: &mut Ctx, name: String) -> CliResult {
    let removed: GrantRemoved = ctx
        .backend
        .call_typed(Request::RemoveGrant { selector: name })
        .await?;
    if ctx.out.json_or(&removed) {
        return Ok(());
    }
    if removed.grant.browser.is_some() {
        ctx.out.line(format!(
            "已作废「{}」：它的浏览器连接地址下一次请求起就不能用。",
            removed.grant.name
        ));
    } else {
        ctx.out.line(format!(
            "已作废「{}」：它的令牌下一次请求起就不能用，刷新也换不来新的。",
            removed.grant.name
        ));
    }
    if removed.stopped_commands > 0 {
        ctx.out.line(format!(
            "停掉了它起的 {} 条命令。",
            removed.stopped_commands
        ));
    }
    ctx.out
        .note("用这把凭据的客户端会开始报 401，把那边的连接删掉就行。");
    Ok(())
}

fn projects_text(grant: &GrantDto) -> String {
    grant
        .workspaces
        .iter()
        .map(|member| {
            if member.name.is_empty() {
                format!("{}（已不在服务里）", gld_core::short_id(&member.id))
            } else {
                member.name.clone()
            }
        })
        .collect::<Vec<_>>()
        .join("、")
}

fn access_text(grant: &GrantDto) -> &'static str {
    if grant.read_only {
        "只读"
    } else {
        "能写能跑命令（等于全权）"
    }
}

fn expiry_text(expires_at: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    let remaining = expires_at.saturating_sub(now);
    if remaining == 0 {
        "已过期".into()
    } else {
        format!(
            "{}小时{}分{}秒后",
            remaining / 3600,
            remaining % 3600 / 60,
            remaining % 60
        )
    }
}

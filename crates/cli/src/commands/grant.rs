//! `gld grant`：只开部分项目的凭据（RFC-0007）。

use gld_core::app::{GrantDto, GrantRemoved, GrantSpec, HubStatusDto};
use gld_daemon::Request;

use super::Ctx;
use crate::cli::GrantCmd;
use crate::error::CliResult;
use crate::output::mask;

pub async fn run(ctx: &mut Ctx, command: GrantCmd) -> CliResult {
    match command {
        GrantCmd::Add {
            name,
            projects,
            write,
        } => add(ctx, name, projects, write).await,
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
                grant.oauth_password.clone(),
                grant.bearer_token.clone(),
            ]
        })
        .collect();
    ctx.out
        .table(&["名称", "项目", "权限", "授权口令", "Bearer 令牌"], &rows);
    if !reveal {
        ctx.out.note("已脱敏，--reveal 显示明文。");
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
    ctx.out.line(format!(
        "已作废「{}」：它的令牌下一次请求起就不能用，刷新也换不来新的。",
        removed.grant.name
    ));
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

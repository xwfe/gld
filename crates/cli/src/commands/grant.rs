//! Grant 元数据管理。新签发入口在 D18-B/D 接通；退休入口不再创建旧凭据。

use gld_core::app::{GrantDto, GrantRemoved};
use gld_core::auth::{CredentialKind, GrantStatus, ProjectScope};
use gld_daemon::Request;

use super::Ctx;
use crate::cli::GrantCmd;
use crate::error::{CliError, CliResult};

pub async fn run(ctx: &mut Ctx, command: GrantCmd) -> CliResult {
    match command {
        GrantCmd::Add { .. } | GrantCmd::Browser { .. } => Err(CliError::new(
            "AUTH_NOT_READY：旧授权创建入口已移除；统一 token/oauth/url 签发尚未接通。",
        )),
        GrantCmd::List { reveal } => list(ctx, reveal).await,
        GrantCmd::Remove { name } => remove(ctx, name).await,
    }
}

async fn list(ctx: &mut Ctx, reveal: bool) -> CliResult {
    if reveal {
        return Err(CliError::new(
            "AUTH_UNSUPPORTED：新授权存储不保存凭据明文，不能通过 --reveal 恢复。",
        ));
    }
    let grants: Vec<GrantDto> = ctx.backend.call_typed(Request::ListGrants).await?;
    if ctx.out.json_or(&grants) {
        return Ok(());
    }
    if grants.is_empty() {
        ctx.out
            .line("新授权存储中没有 grant；网络 MCP 不会因此获得任何权限。");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = grants
        .iter()
        .map(|grant| {
            vec![
                grant.id.clone(),
                grant.name.clone(),
                match grant.credential.kind {
                    CredentialKind::OAuth => "OAuth",
                    CredentialKind::Token => "API token",
                    CredentialKind::Url => "URL",
                }
                .to_string(),
                projects_text(grant),
                access_text(grant),
                match grant.status {
                    GrantStatus::Active => "有效",
                    GrantStatus::Revoked => "已撤销",
                }
                .to_string(),
                grant.credential.revision.to_string(),
                grant
                    .credential
                    .expires_at_unix_secs
                    .map(expiry_text)
                    .unwrap_or_else(|| "不限期".into()),
            ]
        })
        .collect();
    ctx.out.table(
        &["ID", "名称", "类型", "项目", "权限", "状态", "修订", "到期"],
        &rows,
    );
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
        "已撤销「{}」；旧凭据不能再次授权。",
        removed.grant.name
    ));
    if removed.stopped_commands > 0 {
        ctx.out.line(format!(
            "停掉了它起的 {} 条命令。",
            removed.stopped_commands
        ));
    }
    if removed.stopped_agent_tasks > 0 {
        ctx.out.line(format!(
            "停掉了它委派的 {} 个 Agent 任务。",
            removed.stopped_agent_tasks
        ));
    }
    Ok(())
}

fn projects_text(grant: &GrantDto) -> String {
    match &grant.scope {
        ProjectScope::AllProjects => "当前及以后登记的全部项目".into(),
        ProjectScope::Selected(ids) => ids.join("、"),
    }
}

fn access_text(grant: &GrantDto) -> String {
    let mut permissions = vec![if grant.permissions.write {
        "可写"
    } else {
        "只读"
    }];
    let service = &grant.permissions.service;
    if service.user_prompts {
        permissions.push("用户 Prompt");
    }
    if service.machine_mcp {
        permissions.push("本机 MCP");
    }
    if service.managed_workspaces {
        permissions.push("托管空间");
    }
    permissions.join(" / ")
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

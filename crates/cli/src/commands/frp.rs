use gld_core::app::FrpProfileDto;
use gld_core::settings::FrpProfile;
use gld_daemon::Request;
use serde_json::json;

use super::Ctx;
use crate::cli::FrpCmd;
use crate::error::{CliError, CliResult};
use crate::output::yes_no;

pub async fn run(ctx: &mut Ctx, command: FrpCmd) -> CliResult {
    match command {
        FrpCmd::List => {
            let profiles: Vec<FrpProfileDto> =
                ctx.backend.call_typed(Request::ListFrpProfiles).await?;
            if ctx.out.json_or(&profiles) {
                return Ok(());
            }
            if profiles.is_empty() {
                ctx.out.line("还没有 FRP 服务器配置。gld frp add --name 公司 --server frp.example.com --token xxx");
                return Ok(());
            }
            let rows = profiles
                .iter()
                .map(|p| {
                    vec![
                        gld_core::short_id(&p.id).to_string(),
                        p.name.clone(),
                        format!("{}:{}", p.server, p.server_port),
                        yes_no(p.has_token).into(),
                    ]
                })
                .collect::<Vec<_>>();
            ctx.out
                .table(&["ID", "名称", "服务器", "已设 token"], &rows);
            Ok(())
        }
        FrpCmd::Add {
            name,
            server,
            port,
            token,
        } => {
            let saved: FrpProfileDto = ctx
                .backend
                .call_typed(Request::SaveFrpProfile {
                    profile: FrpProfile::new(name, server, port),
                    token,
                })
                .await?;
            if !ctx.out.json_or(&saved) {
                ctx.out.line(format!(
                    "已添加 FRP 配置「{}」，id {}",
                    saved.name,
                    gld_core::short_id(&saved.id)
                ));
                // 接入时写名称就够（`mcp.frp-profile=` 认名称、完整 id 和 ≥4 位前缀），
                // 名称是用户自己起的，比 id 好认也好记。
                ctx.out.line(format!(
                    "工作区接入：gld ws set mcp.tunnel=frp mcp.frp-profile={} mcp.frp-subdomain=<子域名>",
                    saved.name
                ));
            }
            Ok(())
        }
        FrpCmd::Update {
            id,
            name,
            server,
            port,
            token,
        } => {
            let profiles: Vec<FrpProfileDto> =
                ctx.backend.call_typed(Request::ListFrpProfiles).await?;
            let current = profiles
                .into_iter()
                .find(|p| p.id == id || p.name == id)
                .ok_or_else(|| CliError::new(format!("FRP 配置不存在：{id}")))?;
            let profile = FrpProfile {
                id: current.id.clone(),
                name: name.unwrap_or(current.name),
                server: server.unwrap_or(current.server),
                server_port: port.unwrap_or(current.server_port),
            };
            let saved: FrpProfileDto = ctx
                .backend
                .call_typed(Request::SaveFrpProfile { profile, token })
                .await?;
            if !ctx.out.json_or(&saved) {
                ctx.out.line(format!(
                    "已更新 FRP 配置「{}」。使用它的隧道需要 gld tunnel restart。",
                    saved.name
                ));
            }
            Ok(())
        }
        FrpCmd::Remove { id, force } => {
            ctx.backend
                .call(Request::DeleteFrpProfile {
                    id: id.clone(),
                    force,
                })
                .await?;
            if !ctx.out.json_or(&json!({ "deleted": id })) {
                ctx.out.line("已删除。");
            }
            Ok(())
        }
    }
}

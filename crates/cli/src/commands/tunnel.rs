use gld_core::app::TunnelTestResult;
use gld_core::tunnel::{TunnelServiceKind, TunnelStatus};
use gld_daemon::Request;

use super::Ctx;
use crate::cli::{TunnelArgs, TunnelCmd, TunnelService};
use crate::error::CliResult;
use crate::output::or_dash;

fn kind(args: &TunnelArgs) -> TunnelServiceKind {
    match args.service {
        TunnelService::Mcp => TunnelServiceKind::Mcp,
        TunnelService::Actions => TunnelServiceKind::Actions,
    }
}

pub async fn run(ctx: &mut Ctx, command: TunnelCmd) -> CliResult {
    let target = ctx.target.clone();
    match command {
        TunnelCmd::Start(args) => {
            let status: TunnelStatus = ctx
                .backend
                .call_typed(Request::TunnelStart {
                    target,
                    kind: kind(&args),
                })
                .await?;
            print_status(ctx, &status)
        }
        TunnelCmd::Stop(args) => {
            let status: TunnelStatus = ctx
                .backend
                .call_typed(Request::TunnelStop {
                    target,
                    kind: kind(&args),
                })
                .await?;
            print_status(ctx, &status)
        }
        TunnelCmd::Restart(args) => {
            let status: TunnelStatus = ctx
                .backend
                .call_typed(Request::TunnelRestart {
                    target,
                    kind: kind(&args),
                })
                .await?;
            print_status(ctx, &status)
        }
        TunnelCmd::Status(args) => {
            let status: TunnelStatus = ctx
                .backend
                .call_typed(Request::TunnelStatus {
                    target,
                    kind: kind(&args),
                })
                .await?;
            print_status(ctx, &status)
        }
        TunnelCmd::Test(args) => {
            let result: TunnelTestResult = ctx
                .backend
                .call_typed(Request::TunnelTest {
                    target,
                    kind: kind(&args),
                })
                .await?;
            if !ctx.out.json_or(&result) {
                ctx.out.line(format!(
                    "{} {}",
                    ctx.out.ok_mark(result.success),
                    result.message
                ));
                if !result.public_url.is_empty() {
                    ctx.out.line(format!("  公网地址  {}", result.public_url));
                }
            }
            if result.success {
                Ok(())
            } else {
                Err(crate::error::CliError::new(""))
            }
        }
        TunnelCmd::Snippet { service, reveal } => {
            let snippet: String = ctx
                .backend
                .call_typed(Request::FrpSnippet {
                    target,
                    kind: kind(&service),
                    reveal,
                })
                .await?;
            if !ctx.out.json_or(&snippet) {
                println!("{snippet}");
            }
            Ok(())
        }
    }
}

fn print_status(ctx: &Ctx, status: &TunnelStatus) -> CliResult {
    if ctx.out.json_or(status) {
        return Ok(());
    }
    ctx.out.kv(&[
        ("状态", ctx.out.state(&status.state)),
        ("公网地址", or_dash(&status.public_url)),
        (
            "隧道进程",
            status
                .tunnel_pid
                .map(|pid| pid.to_string())
                .unwrap_or_else(|| "-".into()),
        ),
    ]);
    Ok(())
}

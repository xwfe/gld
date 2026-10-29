//! 预置节点及凭据管理入口；邀请配对与自启动由后续切片提供。

use std::io::{IsTerminal, Read};
use std::path::Path;

use gld_daemon::Request;
use serde_json::Value;

use super::Ctx;
use crate::cli::NodeCmd;
use crate::error::{CliError, CliResult};

pub async fn run(ctx: &mut Ctx, command: NodeCmd) -> CliResult {
    let (request, note) = match command {
        NodeCmd::Register {
            name,
            id,
            credential_stdin,
        } => (
            Request::NodeRegister {
                name,
                id,
                credential: read_credential(credential_stdin)?,
            },
            "已登记专用凭据。目标机用私有配置文件运行 gld node run --config；当前只提供项目读取。",
        ),
        NodeCmd::Revoke { machine_id } => (
            Request::NodeRevoke { machine_id },
            "已撤销凭据并关闭旧连接，保留机器身份和项目目录。",
        ),
        NodeCmd::RotateCredential {
            machine_id,
            credential_stdin,
        } => (
            Request::NodeRotateCredential {
                machine_id,
                credential: read_credential(credential_stdin)?,
            },
            "新凭据已生效，旧连接已关闭。更新目标机的私有配置后重新运行节点；机器与项目句柄保持。",
        ),
        NodeCmd::Run { .. } => unreachable!("node run is handled before Backend"),
    };
    let result: Value = ctx.backend.call_typed(request).await?;
    if !ctx.out.json_or(&result) {
        ctx.out.line(format!(
            "Hub ID：{}\n机器 ID：{}\n名称：{}",
            result["hub_id"].as_str().unwrap_or(""),
            result["machine_id"].as_str().unwrap_or(""),
            result["name"].as_str().unwrap_or("")
        ));
        ctx.out.note(note);
    }
    Ok(())
}

fn read_credential(enabled: bool) -> Result<String, CliError> {
    if !enabled {
        return Err(CliError::new("节点凭据须使用 --credential-stdin。"));
    }
    if std::io::stdin().is_terminal() {
        return Err(CliError::new(
            "--credential-stdin 需要管道或文件输入，不在终端回显凭据。",
        ));
    }
    let mut credential = String::new();
    std::io::stdin()
        .take(259)
        .read_to_string(&mut credential)
        .map_err(|_| CliError::new("无法读取节点凭据。"))?;
    if credential.len() > 258 {
        return Err(CliError::new("节点凭据输入过长。"));
    }
    Ok(credential.trim_end_matches(['\r', '\n']).to_string())
}

pub async fn run_config(path: &Path) -> CliResult {
    let config = gld_core::nodes::config::NodeConfig::load(path).map_err(CliError::new)?;
    tokio::task::spawn_blocking(move || gld_core::nodes::client::run(config))
        .await
        .map_err(|_| CliError::new("节点进程异常结束。"))?
        .map_err(CliError::new)
}

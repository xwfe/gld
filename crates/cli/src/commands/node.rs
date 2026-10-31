//! 本机操作员的节点邀请及凭据管理入口。

use std::io::{IsTerminal, Read};
use std::path::Path;

use gld_daemon::Request;
use serde_json::Value;

use super::Ctx;
use crate::cli::{AutostartCmd, NodeCmd};
use crate::error::{CliError, CliResult};
use crate::output::Output;

pub async fn run(ctx: &mut Ctx, command: NodeCmd) -> CliResult {
    let (request, note) = match command {
        NodeCmd::Invite { name } => {
            let result: Value = ctx.backend.call_typed(Request::NodeInvite { name }).await?;
            if !ctx.out.json_or(&result) {
                ctx.out.line(format!(
                    "Hub ID：{}\n机器名称：{}\n邀请：{}\n有效期：十分钟",
                    result["hub_id"].as_str().unwrap_or(""),
                    result["name"].as_str().unwrap_or(""),
                    result["invitation"].as_str().unwrap_or(""),
                ));
                ctx.out.note("在目标机用 gld join --invite-stdin 接收邀请；通过终端确认共享项目。相同名称的新邀请会使旧邀请失效。");
            }
            return Ok(());
        }
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
            "已登记专用凭据。把 Hub 公钥原样填进目标机私有配置的 hub_public_key，再运行 gld node run --config；项目默认只读，文件写入须在目标机显式授权。",
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
        NodeCmd::Run { .. }
        | NodeCmd::Shares { .. }
        | NodeCmd::Doctor { .. }
        | NodeCmd::Autostart(_) => {
            unreachable!("local node commands are handled before Backend")
        }
    };
    let result: Value = ctx.backend.call_typed(request).await?;
    if !ctx.out.json_or(&result) {
        ctx.out.line(format!(
            "Hub ID：{}\nHub 公钥：{}\n机器 ID：{}\n名称：{}",
            result["hub_id"].as_str().unwrap_or(""),
            result["hub_public_key"].as_str().unwrap_or(""),
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

pub async fn run_config(path: &Path, service: bool) -> CliResult {
    if service {
        gld_core::nodes::service::detach_console();
        gld_core::nodes::service::log_to(path);
    }
    // 拿不到锁时退出码非 0：前台那个停了之后，服务管理器一分钟内再拉起这个就接上了。
    let storage = gld_core::nodes::local_config::LocalConfig::lock(path).map_err(|error| {
        if service {
            gld_core::nodes::service::report(&error);
        }
        CliError::new(error)
    })?;
    let config = match load_config(&storage) {
        Ok(config) => config,
        // 配置坏了或项目被删，拉起多少次都一样。退出码 0，服务管理器不再反复拉起，原因在日志里。
        Err(error) if service => {
            gld_core::nodes::service::report(&format!("{error}; not starting"));
            return Ok(());
        }
        Err(error) => return Err(CliError::new(error)),
    };
    tokio::task::spawn_blocking(move || {
        let _configuration_lock = storage;
        if service {
            gld_core::nodes::service::run(config)
        } else {
            gld_core::nodes::client::run(config)
        }
    })
    .await
    .map_err(|_| CliError::new("节点进程异常结束。"))?
    .map_err(CliError::new)
}

fn load_config(
    storage: &gld_core::nodes::local_config::LocalConfig,
) -> Result<gld_core::nodes::config::NodeConfig, String> {
    let config = storage.load()?;
    let projects = gld_core::nodes::join::resolve_workspaces(&config.workspaces)?;
    gld_core::nodes::local_config::validate_config_location(
        storage.path(),
        &projects
            .into_iter()
            .map(|project| project.path)
            .collect::<Vec<_>>(),
    )?;
    Ok(config)
}

pub fn autostart(command: &AutostartCmd, out: Output) -> CliResult {
    let find = |config: &Option<std::path::PathBuf>| {
        gld_core::nodes::local_config::find(config.as_deref()).map_err(CliError::new)
    };
    match command {
        AutostartCmd::Enable { config } => {
            let status =
                gld_core::nodes::autostart::enable(&find(config)?).map_err(CliError::new)?;
            print_autostart(&status, out);
        }
        AutostartCmd::Disable { config } => {
            let removed =
                gld_core::nodes::autostart::disable(&find(config)?).map_err(CliError::new)?;
            if !out.json_or(&serde_json::json!({ "removed": removed })) {
                out.line(if removed {
                    "已停止节点并删除自启动配置。节点配置和项目文件没动，要连 Hub 就 gld node run。"
                } else {
                    "这份配置没配过自启动，什么都没动。"
                });
            }
        }
        AutostartCmd::Status { config } => {
            let status =
                gld_core::nodes::autostart::status(&find(config)?).map_err(CliError::new)?;
            print_autostart(&status, out);
        }
    }
    Ok(())
}

pub fn print_autostart(status: &gld_core::nodes::autostart::Status, out: Output) {
    if out.json_or(status) {
        return;
    }
    out.kv(&[
        (
            "自启动",
            if status.installed {
                format!("已配置（{}）", status.manager)
            } else {
                "没配置".to_string()
            },
        ),
        (
            "节点进程",
            if status.running { "在跑" } else { "没跑" }.to_string(),
        ),
        ("服务名", status.label.clone()),
        ("日志", status.log.display().to_string()),
    ]);
    for note in &status.notes {
        out.note(note);
    }
}

pub fn shares(
    path: &Path,
    workspaces: &[String],
    writable_workspaces: &[String],
    runnable_workspaces: &[String],
    out: Output,
) -> CliResult {
    let changed = !workspaces.is_empty();
    let view = if changed {
        gld_core::nodes::sharing::replace(
            path,
            workspaces,
            writable_workspaces,
            runnable_workspaces,
        )
    } else {
        gld_core::nodes::sharing::show(path)
    }
    .map_err(CliError::new)?;
    if !out.json_or(&view) {
        out.kv(&[
            ("Hub", view.hub_url),
            ("Hub ID", view.hub_id),
            ("机器 ID", view.machine_id),
            ("配置", view.config_path.display().to_string()),
        ]);
        let runs_commands = view.workspaces.iter().any(|workspace| workspace.commands);
        let rows: Vec<_> = view
            .workspaces
            .into_iter()
            .map(|workspace| {
                vec![
                    workspace.id,
                    workspace.name.unwrap_or_else(|| "—".into()),
                    workspace.path.unwrap_or_else(|| "—".into()),
                    if workspace.available {
                        "可共享".into()
                    } else {
                        "缺失或未限制读取".into()
                    },
                    if workspace.commands {
                        "文件写入、跑命令".into()
                    } else if workspace.read_only {
                        "只读".into()
                    } else {
                        "文件写入".into()
                    },
                ]
            })
            .collect();
        out.table(&["项目 ID", "名称", "路径", "本机登记", "权限"], &rows);
        if changed {
            out.note("已替换共享名单及权限。重新运行节点并连接 Hub 后生效；管理工具不开放。");
            if runs_commands {
                out.note("能跑命令的项目不是沙箱：命令以当前账号执行，能调用这个 Hub 的人等于拿到这个账号。");
            }
        }
    }
    Ok(())
}

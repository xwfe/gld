//! 子命令实现。每个文件对应 `cli.rs` 里的一组命令，只做三件事：
//! 组装 `Request`、调用后端、渲染结果。

mod daemon;
mod doctor;
mod frp;
mod gateway;
mod inspect;
mod logs;
mod planning;
mod secret;
mod service;
mod settings;
mod share;
mod tool;
mod tunnel;
mod upgrade;
mod workspace;

use std::time::Duration;

use clap::CommandFactory;
use gld_core::app::WorkspaceTarget;

use crate::backend::Backend;
use crate::cli::{Cli, Command};
use crate::error::CliResult;
use crate::output::Output;

/// 一次命令执行共享的上下文。
pub struct Ctx {
    pub backend: Backend,
    pub out: Output,
    pub target: WorkspaceTarget,
    /// 用户是否显式指定了工作区（决定 `status` 显示总览还是详情）。
    pub explicit_workspace: bool,
}

pub async fn run(cli: Cli) -> CliResult {
    let out = Output::new(cli.global.json, cli.global.no_color);

    // 守护进程命令不经过 Backend：它们要在守护进程不响应 / 版本不一致时也能工作。
    if let Command::Daemon(command) = &cli.command {
        return daemon::run(command, out).await;
    }
    if let Command::Completions { shell } = &cli.command {
        clap_complete::generate(*shell, &mut Cli::command(), "gld", &mut std::io::stdout());
        return Ok(());
    }

    let backend = Backend::connect(
        !cli.global.no_autostart,
        cli.global.timeout.map(Duration::from_secs),
        out,
    )
    .await?;
    let target = WorkspaceTarget::new(cli.global.workspace.clone(), std::env::current_dir().ok());
    let mut ctx = Ctx {
        backend,
        out,
        explicit_workspace: cli.global.workspace.is_some(),
        target,
    };

    match cli.command {
        Command::Daemon(_) | Command::Completions { .. } => unreachable!("handled above"),
        Command::Workspace(command) => workspace::run(&mut ctx, command).await,
        Command::Start(args) => service::start(&mut ctx, args).await,
        Command::Stop(args) => service::stop(&mut ctx, args).await,
        Command::Restart(args) => service::restart(&mut ctx, args).await,
        Command::Status => service::status(&mut ctx).await,
        Command::Ps => service::ps(&mut ctx).await,
        Command::Logs(args) => logs::workspace_logs(&mut ctx, args).await,
        Command::Ls(args) => service::ls(&mut ctx, args).await,
        Command::Share(args) => share::run(&mut ctx, args).await,
        Command::Upgrade(args) => upgrade::run(&mut ctx, args).await,
        Command::Health => inspect::health(&mut ctx).await,
        Command::Doctor => doctor::run(&mut ctx).await,
        Command::Tool(command) => tool::run(&mut ctx, command).await,
        Command::Tunnel(command) => tunnel::run(&mut ctx, command).await,
        Command::Gateway(command) => gateway::run(&mut ctx, command).await,
        Command::Secret(command) => secret::run(&mut ctx, command).await,
        Command::Frp(command) => frp::run(&mut ctx, command).await,
        Command::Settings(command) => settings::run(&mut ctx, command).await,
        Command::Planning(command) => planning::run(&mut ctx, command).await,
        Command::History => inspect::history(&mut ctx).await,
        Command::Usage => inspect::usage(&mut ctx).await,
        Command::Context(args) => inspect::context(&mut ctx, args).await,
    }
}

/// 逗号分隔列表 → Vec，去空白与空项。
pub fn split_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

//! 配对在目标账号本地完成，不连接或自动启动本机 daemon。

use std::io::{BufRead, IsTerminal, Read, Write};

use gld_core::nodes::join::{JoinOptions, PreparedJoin};

use crate::cli::JoinArgs;
use crate::error::{CliError, CliResult};
use crate::output::Output;

pub async fn run(args: &JoinArgs, out: Output) -> CliResult {
    let invitation = read_invitation(args.invite_stdin)?;
    let prepared = PreparedJoin::prepare(
        JoinOptions {
            hub_url: args.hub.clone(),
            config_path: args.config.clone(),
            workspaces: args.share.clone(),
            ca_certificate: args.ca_certificate.clone(),
        },
        invitation,
    )
    .map_err(CliError::new)?;
    let preview = serde_json::to_string_pretty(prepared.preview())
        .map_err(|_| CliError::new("无法显示节点配对范围。"))?;
    out.note(format!(
        "将以当前执行账号共享以下只读项目，桌面访问关闭：\n{preview}"
    ));
    if !args.yes {
        confirm()?;
    }
    let joined = prepared.execute().await.map_err(CliError::new)?;
    if !out.json_or(&joined) {
        out.line("配对成功，私有节点配置已保存。");
        out.note("使用 gld node run --config <配置文件> 在前台连接 Hub。");
    }
    Ok(())
}

fn read_invitation(enabled: bool) -> Result<String, CliError> {
    if !enabled || std::io::stdin().is_terminal() {
        return Err(CliError::new(
            "--invite-stdin 需要管道或文件输入，不在终端回显邀请。",
        ));
    }
    let mut invitation = String::new();
    std::io::stdin()
        .take(259)
        .read_to_string(&mut invitation)
        .map_err(|_| CliError::new("无法读取配对邀请。"))?;
    if invitation.len() > 258 {
        return Err(CliError::new("配对邀请输入过长。"));
    }
    Ok(invitation.trim_end_matches(['\r', '\n']).into())
}

fn confirm() -> CliResult {
    if !std::io::stderr().is_terminal() {
        return Err(CliError::new(
            "非交互配对需要显式 --yes 确认以上 Hub、账号和共享项目。",
        ));
    }
    #[cfg(unix)]
    let terminal = std::fs::File::open("/dev/tty");
    #[cfg(windows)]
    let terminal = std::fs::File::open("CONIN$");
    #[cfg(not(any(unix, windows)))]
    let terminal: std::io::Result<std::fs::File> = Err(std::io::Error::other("no terminal"));
    let terminal =
        terminal.map_err(|_| CliError::new("无法打开确认终端；非交互调用请使用 --yes。"))?;
    eprint!("继续配对？输入 yes 确认：");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::BufReader::new(terminal)
        .take(16)
        .read_line(&mut answer)?;
    if answer.trim() != "yes" {
        return Err(CliError::new("已取消配对，未发送邀请或创建节点配置。"));
    }
    Ok(())
}

//! 配对在目标账号本地完成，不连接或自动启动本机 daemon。

use std::io::{BufRead, IsTerminal, Read, Write};

use gld_core::app::{App, WorkspaceCreateOptions};
use gld_core::nodes::join::{JoinOptions, PreparedJoin};

use crate::cli::{JoinArgs, LeaveArgs};
use crate::error::{CliError, CliResult};
use crate::output::Output;

pub async fn run(args: &JoinArgs, out: Output) -> CliResult {
    let invitation = read_invitation(args.invite_stdin)?;
    let mut projects = SharedDirectories::default();
    let workspaces = projects.selectors(&args.share, &out)?;
    let writable_workspaces = projects.selectors(&args.writable_workspaces, &out)?;
    let runnable_workspaces = projects.selectors(&args.runnable_workspaces, &out)?;
    let prepared = PreparedJoin::prepare(
        JoinOptions {
            hub_url: args.hub.clone(),
            config_path: args.config.clone(),
            workspaces,
            writable_workspaces,
            runnable_workspaces,
            ca_certificate: args.ca_certificate.clone(),
        },
        invitation,
    )
    .map_err(CliError::new)?;
    let preview = serde_json::to_string_pretty(prepared.preview())
        .map_err(|_| CliError::new("无法显示节点配对范围。"))?;
    out.note(format!(
        "将以当前执行账号共享以下项目；read_only 为 false 的项目允许文件写入，commands 为 true 的项目能以这个账号跑命令（不是沙箱，能调用 Hub 的人等于拿到这个账号）；管理工具和桌面访问关闭：\n{preview}"
    ));
    if !args.yes {
        confirm(
            "继续配对？输入 yes 确认：",
            "非交互配对需要显式 --yes 确认以上 Hub、账号和共享项目。",
            "已取消配对，未发送邀请或创建节点配置。",
        )?;
    }
    let joined = prepared.execute().await.map_err(CliError::new)?;
    if !out.json_or(&joined) {
        out.line("配对成功，私有节点配置已保存。");
        out.note(format!(
            "配置在 {}。前台连接 Hub：gld node run --config <上面的路径>；只加入过一个 Hub 时可以不写 --config。",
            joined.config_path.display()
        ));
    }
    Ok(())
}

/// `--share` 能直接写目录，目标机不用先学 `gld add`。登记写本机数据文件，不拉起守护进程。
#[derive(Default)]
struct SharedDirectories {
    app: Option<App>,
}

impl SharedDirectories {
    fn selectors(&mut self, selectors: &[String], out: &Output) -> Result<Vec<String>, CliError> {
        selectors
            .iter()
            .map(|selector| {
                let absolute = super::absolutize(std::path::Path::new(selector))?;
                // 不是目录就仍当名称或 ID 查，查不到由后面统一报错，不在这里猜用户的意思。
                if !looks_like_directory(selector) || !absolute.is_dir() {
                    return Ok(selector.clone());
                }
                let app = match &mut self.app {
                    Some(app) => app,
                    empty => empty.insert(App::load()?),
                };
                let ensured = app.ensure_workspace(&absolute, WorkspaceCreateOptions::default())?;
                if ensured.created {
                    out.note(format!(
                        "已登记本机项目「{}」（{}）。这一步只写本机记录，配对取消也不会撤销；不要了就 gld rm {}。",
                        ensured.profile.name, ensured.profile.path, ensured.profile.name
                    ));
                }
                Ok(ensured.profile.id)
            })
            .collect()
    }
}

/// 只把明显是路径的写法当目录，`--share api` 不会因为当前目录下碰巧有个 api 就变成登记新项目。
fn looks_like_directory(selector: &str) -> bool {
    selector.contains(['/', '\\']) || matches!(selector, "." | "..")
}

pub fn leave(args: &LeaveArgs, out: Output) -> CliResult {
    let config =
        gld_core::nodes::local_config::find(args.config.as_deref()).map_err(CliError::new)?;
    if !args.yes {
        out.note(format!(
            "将删除 {} 及其未完成的配对文件，这台机器之后连不上 Hub，要回来得重新邀请和配对。项目文件和运行记录不动。",
            config.display()
        ));
        confirm(
            "继续退出？输入 yes 确认：",
            "非交互退出需要显式 --yes。",
            "已取消，节点配置没动。",
        )?;
    }
    let left = gld_core::nodes::sharing::leave(&config).map_err(CliError::new)?;
    if !out.json_or(&left) {
        out.line("已删除节点配置和凭据，项目文件没动。");
        out.note(format!(
            "Hub 上的登记还在，显示离线。不再用的话，在 Hub 上执行 gld node revoke {}",
            left.machine_id
        ));
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

fn confirm(prompt: &str, needs_yes: &str, cancelled: &str) -> CliResult {
    if !std::io::stderr().is_terminal() {
        return Err(CliError::new(needs_yes));
    }
    #[cfg(unix)]
    let terminal = std::fs::File::open("/dev/tty");
    #[cfg(windows)]
    let terminal = std::fs::File::open("CONIN$");
    #[cfg(not(any(unix, windows)))]
    let terminal: std::io::Result<std::fs::File> = Err(std::io::Error::other("no terminal"));
    let terminal =
        terminal.map_err(|_| CliError::new("无法打开确认终端；非交互调用请使用 --yes。"))?;
    eprint!("{prompt}");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::BufReader::new(terminal)
        .take(16)
        .read_line(&mut answer)?;
    if answer.trim() != "yes" {
        return Err(CliError::new(cancelled));
    }
    Ok(())
}

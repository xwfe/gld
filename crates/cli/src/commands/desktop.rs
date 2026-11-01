//! `gld desktop authorize`：趁用户在电脑前把录屏授权办好，别等 AI 用的时候才弹框没人点。

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use gld_core::app::{DesktopStatus, GRAPHICAL_SESSION};
use gld_daemon::Request;

use super::Ctx;
use crate::cli::DesktopCmd;
use crate::error::{CliError, CliResult};

const SETTINGS_PANE: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture";
const POLL_EVERY: Duration = Duration::from_secs(2);

pub async fn run(ctx: &mut Ctx, command: DesktopCmd) -> CliResult {
    let DesktopCmd::Authorize { wait } = command;
    let out = ctx.out;
    if !cfg!(target_os = "macos") {
        out.line("这个系统不用单独授权，这条命令只对 macOS 有用。");
        return Ok(());
    }
    // 不自动拉起：从这个终端拉起的，会话类型跟着这个终端，授权办好了也可能白办。
    if !ctx.backend.is_remote() {
        return Err(CliError::new(
            "守护进程没在跑。授权要给跑着的那个守护进程，先把它交给 launchd 拉起，步骤见 docs/daemon.md 的“开机自启”。",
        ));
    }
    let mut status = probe(ctx).await?;
    if !status.can_capture && status.session != GRAPHICAL_SESSION {
        return Err(CliError::new(format!(
            "{}\n{}",
            status.check.detail, status.check.fix
        )));
    }
    if !status.can_capture {
        let _ = Command::new("open")
            .arg(SETTINGS_PANE)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        out.note("系统设置已打开：隐私与安全性 → 录屏与系统录音。");
        out.note(format!("要授权的文件：{}", status.executable.display()));
        out.note(
            "· 弹了授权框：按提示打开设置，把它的开关打开。系统说要重启它，选“稍后”，不用重启。",
        );
        out.note(
            "· 列表里已经有它：开关没用，换过二进制后那条已经失效。选中点“−”删掉，这边会重新弹框。",
        );
        out.note("· 屏幕锁着也截不了，先解锁。");
        out.note(format!("这边等到能截为止，最多 {wait} 秒……"));
        let deadline = Instant::now() + Duration::from_secs(wait);
        while !status.can_capture && Instant::now() < deadline {
            tokio::time::sleep(POLL_EVERY).await;
            status = probe(ctx).await?;
        }
    }
    if !status.can_capture {
        return Err(CliError::new(format!(
            "等了 {wait} 秒还是截不了。{}",
            status.check.detail
        )));
    }
    if !out.json_or(&status) {
        out.line(format!(
            "{} 能截屏了（{} 会话）。换了二进制要再跑一次这条命令。",
            out.ok_mark(true),
            status.session
        ));
    }
    Ok(())
}

async fn probe(ctx: &mut Ctx) -> CliResult<DesktopStatus> {
    ctx.backend
        .call_typed(Request::DesktopProbe {
            only_if_used: false,
        })
        .await
}

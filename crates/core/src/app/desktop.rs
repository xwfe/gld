//! 守护进程自己能不能截屏。`gld doctor --desktop`、`gld desktop authorize` 用。
//!
//! 必须在守护进程里查：AI 的命令是守护进程的子孙，会话类型和录屏授权都看它，命令行所在的
//! 终端能截不代表 AI 能截。没放行 `screencapture` 的项目不主动查：没授权时一试就可能弹系统
//! 授权框，不用桌面的人不该碰到。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::{App, DoctorCheck};
use crate::error::AppResult;

pub const DESKTOP_SCOPE: &str = "桌面";
const LABEL: &str = "截屏";
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(5);
const CAPTURE_COMMAND: &str = "screencapture";

/// `launchctl managername` 里的图形会话。
pub const GRAPHICAL_SESSION: &str = "Aqua";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopStatus {
    pub session: String,
    pub can_capture: bool,
    /// 系统授权认的是拉起来的这个文件，不是命令行那个，授权时要把它加进设置。
    pub executable: PathBuf,
    pub check: DoctorCheck,
}

impl App {
    /// 有项目放行了 `screencapture`，才算这台机器在用桌面截屏。
    pub fn uses_screen_capture(&self) -> AppResult<bool> {
        Ok(self
            .list_workspaces()?
            .iter()
            .any(|profile| allows_screen_capture(&profile.runtime.allowed_commands)))
    }
}

/// `allowed-commands` 的值里有没有 `screencapture`。`only:` 前缀见 `tools/policy.rs`。
pub fn allows_screen_capture(configured: &str) -> bool {
    let configured = configured.trim();
    configured
        .strip_prefix("only:")
        .unwrap_or(configured)
        .split(',')
        .any(|name| name.trim() == CAPTURE_COMMAND)
}

/// 真截 1×1 像素再看结果。比问系统“有没有授权”可靠：会话、授权、锁屏任何一层不通都会失败，
/// 而且一个像素看不到屏幕内容。没有这一条记录的二进制，这一下还会让系统登记并弹授权框。
pub fn desktop_status(executable: &Path) -> DesktopStatus {
    let (session, capture) = if cfg!(target_os = "macos") {
        (session_type(), try_capture())
    } else {
        ("未知".to_string(), Err("这项目前只查 macOS".to_string()))
    };
    let can_capture = capture.is_ok();
    let check = if cfg!(target_os = "macos") {
        capture_check(&session, capture, executable)
    } else {
        DoctorCheck::ok(DESKTOP_SCOPE, LABEL, "这项目前只查 macOS")
    };
    DesktopStatus {
        session,
        can_capture,
        executable: executable.to_path_buf(),
        check,
    }
}

/// `launchctl managername`：`Aqua` 才是图形会话。这个值继承自拉起守护进程的进程，`setsid` 改不了它。
fn session_type() -> String {
    Command::new("launchctl")
        .arg("managername")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "未知".to_string())
}

fn try_capture() -> Result<(), String> {
    let shot = std::env::temp_dir().join(format!("gld-desktop-probe-{}.png", std::process::id()));
    let result = run_capture(&shot);
    let _ = std::fs::remove_file(&shot);
    result
}

fn run_capture(shot: &Path) -> Result<(), String> {
    let mut child = Command::new("/usr/sbin/screencapture")
        .args(["-x", "-R0,0,1,1"])
        .arg(shot)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("起不了 screencapture：{error}"))?;
    let started = Instant::now();
    while child
        .try_wait()
        .map_err(|error| error.to_string())?
        .is_none()
    {
        if started.elapsed() >= CAPTURE_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "screencapture {} 秒没结束",
                CAPTURE_TIMEOUT.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child
        .wait_with_output()
        .map_err(|error| error.to_string())?;
    if output.status.success() && shot.is_file() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(match stderr.trim() {
        "" => format!("screencapture 失败（{}）", output.status),
        message => message.to_string(),
    })
}

pub fn capture_check(session: &str, capture: Result<(), String>, executable: &Path) -> DoctorCheck {
    let error = match capture {
        Ok(()) => {
            return DoctorCheck::ok(
                DESKTOP_SCOPE,
                LABEL,
                format!("守护进程能截屏（{session} 会话）"),
            );
        }
        Err(error) => error,
    };
    if session != GRAPHICAL_SESSION {
        return DoctorCheck::warn(
            DESKTOP_SCOPE,
            LABEL,
            format!(
                "截不了（{error}）：守护进程在 {session} 会话里，没有图形界面，开录屏权限也没用。\
                 会话类型继承自拉起它的进程，SSH 和后台工具拉起的都这样"
            ),
            "交给 launchd 拉起，步骤见 docs/daemon.md 的“开机自启”，之后再查一次",
        );
    }
    DoctorCheck::warn(
        DESKTOP_SCOPE,
        LABEL,
        format!(
            "截不了（{error}）：在图形会话里，多半是 {} 没有有效的录屏授权，或者屏幕锁着",
            executable.display()
        ),
        "gld desktop authorize",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::DoctorLevel;

    fn check(session: &str, capture: Result<(), String>) -> DoctorCheck {
        capture_check(session, capture, Path::new("/opt/bin/gld"))
    }

    #[test]
    fn a_capture_that_worked_is_ok_whatever_the_session_is_called() {
        assert_eq!(check("Aqua", Ok(())).level, DoctorLevel::Ok);
        assert_eq!(check("未知", Ok(())).level, DoctorLevel::Ok);
    }

    /// 两种原因的修法完全不同，报错又是同一句：后台会话里开权限没用，图形会话里才要动权限。
    #[test]
    fn the_same_system_error_gets_a_different_fix_per_session() {
        let error = || Err("could not create image from display".to_string());

        let background = check("Background", error());
        assert_eq!(background.level, DoctorLevel::Warn);
        assert!(background.detail.contains("Background"));
        assert!(background.fix.contains("launchd"));
        assert!(!background.fix.contains("录屏"));

        let graphical = check("Aqua", error());
        assert_eq!(graphical.level, DoctorLevel::Warn);
        assert!(graphical.detail.contains("could not create image"));
        assert!(graphical.detail.contains("/opt/bin/gld"));
        assert!(graphical.fix.contains("desktop authorize"));
        assert!(!graphical.fix.contains("launchd"));
    }

    #[test]
    fn only_a_project_that_allows_the_command_counts_as_using_it() {
        assert!(allows_screen_capture("rg, screencapture"));
        assert!(allows_screen_capture("only:screencapture,git"));
        assert!(!allows_screen_capture(""));
        assert!(!allows_screen_capture("rg,screencapture-helper"));
    }

    /// 真的跑一次，只看探测文件有没有收拾干净；能不能截到取决于跑测试的环境，不断言。
    #[cfg(target_os = "macos")]
    #[test]
    fn probing_leaves_no_screenshot_behind() {
        let _ = try_capture();
        let shot =
            std::env::temp_dir().join(format!("gld-desktop-probe-{}.png", std::process::id()));
        assert!(!shot.exists());
    }
}

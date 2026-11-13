//! 操作系统的进程组查询与停止。
//!
//! 不保留句柄或裸指针；unsafe 只覆盖 libc / Win32 系统调用。

#![allow(unsafe_code)]

/// 以这个 pid 为组号的进程组现在有没有进程。命令起来时自成一组（组号 = 它的 pid）。
#[cfg(unix)]
pub(super) fn process_group_exists(pid: u32) -> Option<bool> {
    let pid = libc::pid_t::try_from(pid).ok()?;
    // 发 0 号信号只查不发。EPERM 说明有这个组、只是不归我们管，也算在。
    // SAFETY: signal 0 只查询经过范围检查的进程组，不读写用户态指针。
    let alive = unsafe { libc::kill(-pid, 0) } == 0
        || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
    Some(alive)
}

#[cfg(not(unix))]
pub(super) fn process_group_exists(_pid: u32) -> Option<bool> {
    None
}

/// Signal the command and everything it started.
///
/// exec_command puts each command in its own process group whose id is the
/// command's pid, so the group is signalled first. A process that was not
/// started that way has no such group (the kernel does not hand out a pid
/// while a group with that id exists), and gets the signal alone. A child
/// that moved itself to another group or session on purpose is out of reach.
#[cfg(unix)]
pub(super) fn signal_process_tree(pid: u32, signal: &str) {
    let sig = match signal {
        "KILL" => libc::SIGKILL,
        "INT" => libc::SIGINT,
        _ => libc::SIGTERM,
    };
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return;
    };
    // SAFETY: pid 已转换为平台 pid_t；kill 只接收进程号和固定信号值。
    unsafe {
        if libc::kill(-pid, sig) != 0 {
            libc::kill(pid, sig);
        }
    }
}

/// `taskkill /T` walks the child list; TerminateProcess alone would leave
/// the grandchildren. The signal name has no Windows equivalent: it is
/// always a forced stop. Not run on Windows in this change, only compiled
/// by CI.
#[cfg(windows)]
pub(super) fn signal_process_tree(pid: u32, _signal: &str) {
    use std::os::windows::process::CommandExt;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};

    const CREATE_NO_WINDOW: u32 = 0x08000000;
    let tree = std::process::Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status();
    if tree.is_ok_and(|status| status.success()) {
        return;
    }
    // SAFETY: OpenProcess 返回的有效句柄只用于终止该进程，并在此处关闭一次。
    unsafe {
        if let Ok(handle) = OpenProcess(PROCESS_TERMINATE, false, pid) {
            let _ = TerminateProcess(handle, 1);
            let _ = CloseHandle(handle);
        }
    }
}

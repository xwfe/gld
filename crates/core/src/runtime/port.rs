//! 端口占用检测与释放等待。
//!
//! MCP / Actions 监听器停止后，操作系统不一定立刻释放端口（socket 在 tokio
//! 事件循环上异步关闭）。重启同一端口前必须等它真正空出来，否则新监听器
//! 会和旧的并发存在，表现为“启动成功但请求打到旧实例”。

use std::time::{Duration, Instant};

use crate::async_rt::JoinHandle;
use crate::platform::platform;

/// 占用端口的进程是否就是当前进程（守护进程自己上一次的监听器）。
pub fn is_own_process(pid: u32) -> bool {
    pid == std::process::id()
}

/// Windows 上可以强制回收本进程持有的 TCP 监听项；其他平台总是返回 false。
pub fn try_reclaim_own_port(port: u16) -> bool {
    let Ok(Some(pid)) = platform().find_pid_listening_on_port(port) else {
        return false;
    };
    if !is_own_process(pid) {
        return false;
    }

    match platform().reclaim_listening_port(port) {
        Ok(true) => platform()
            .find_pid_listening_on_port(port)
            .ok()
            .flatten()
            .is_none(),
        Ok(false) => false,
        Err(error) => {
            eprintln!("reclaim_listening_port({port}) failed: {error}");
            false
        }
    }
}

/// 异步等待端口释放；只要占用者是别的进程就立即放弃。
pub async fn wait_for_port_free(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match platform().find_pid_listening_on_port(port) {
            Ok(None) => return true,
            Ok(Some(pid)) if is_own_process(pid) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Ok(Some(_)) => return false,
            Err(_) => return false,
        }
    }

    if try_reclaim_own_port(port) {
        return true;
    }

    platform()
        .find_pid_listening_on_port(port)
        .ok()
        .flatten()
        .is_none()
}

/// [`wait_for_port_free`] 的阻塞版本，供同步调用方使用。
pub fn wait_for_port_free_blocking(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match platform().find_pid_listening_on_port(port) {
            Ok(None) => return true,
            Ok(Some(pid)) if is_own_process(pid) => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(Some(_)) => return false,
            Err(_) => return false,
        }
    }

    if try_reclaim_own_port(port) {
        return true;
    }

    platform()
        .find_pid_listening_on_port(port)
        .ok()
        .flatten()
        .is_none()
}

/// 等待监听器任务退出并确认端口已释放；超过 3 秒仍未退出则强制 abort。
pub async fn await_listener_shutdown(handle: Option<JoinHandle<()>>, port: u16) {
    if let Some(handle) = handle {
        let mut handle = handle;
        tokio::select! {
            _ = &mut handle => {}
            _ = tokio::time::sleep(Duration::from_secs(3)) => {
                handle.abort();
                let _ = handle.await;
            }
        }
    }

    if !wait_for_port_free(port, Duration::from_secs(2)).await {
        let _ = try_reclaim_own_port(port);
    }
}

/// [`await_listener_shutdown`] 的阻塞版本。
///
/// `begin_stop` 已经发送了优雅退出信号。这里必须等待端口真正释放，
/// 不能只把等待任务丢到异步运行时后立即返回，否则 restart 会与旧监听器并发启动。
pub fn await_listener_shutdown_blocking(handle: Option<JoinHandle<()>>, port: u16) {
    if let Some(handle) = handle {
        let port_free = wait_for_port_free_blocking(port, Duration::from_secs(3));
        if !port_free {
            handle.abort();
        }
        crate::async_rt::spawn(async move {
            let _ = handle.await;
        });
    } else if !wait_for_port_free_blocking(port, Duration::from_secs(5)) {
        let _ = try_reclaim_own_port(port);
    }
}

/// 面向用户的端口占用提示，区分“本进程残留”与“别的程序占用”。
pub fn port_busy_message(port: u16, service_label: &str, pid: u32) -> String {
    let image = platform()
        .process_image_path(pid)
        .ok()
        .flatten()
        .unwrap_or_else(|| format!("pid {pid}"));

    if is_own_process(pid) {
        format!(
            "{service_label}端口 {port} 仍被本进程上一次的服务占用（{image}），请先停止服务或稍后再试"
        )
    } else {
        format!("{service_label}端口 {port} 已被占用：{image}（pid {pid}）")
    }
}

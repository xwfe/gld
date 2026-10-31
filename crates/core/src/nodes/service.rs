//! 交给系统服务管理器跑的节点。和前台的差别只有两点：写日志文件（没有终端可看），
//! 以及错误怎么处理，见 [`run`]。

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::hub::Hub;

use super::client::{self, NodeEnd};
use super::config::NodeConfig;

const RETRY_AFTER: Duration = Duration::from_secs(60);
const RETRY_AFTER_REFUSED: Duration = Duration::from_secs(600);

static LOG_FILE: OnceLock<PathBuf> = OnceLock::new();

/// 配置旁边的 `<配置>.log`，诊断命令也读它。
pub fn log_path(config: &Path) -> PathBuf {
    let mut name = config.as_os_str().to_os_string();
    name.push(".log");
    PathBuf::from(name)
}

/// 之后 [`report`] 写这个文件。只调一次；一个进程只跑一个节点。
pub fn log_to(config: &Path) {
    let _ = LOG_FILE.set(log_path(config));
}

/// 前台写终端，服务模式写日志文件。两边都写的话 journal 和文件里每行各有两份。
pub fn report(message: &str) {
    match LOG_FILE.get() {
        Some(path) => {
            crate::logs::append_line(path, &format!("{} {message}", crate::logs::utc_timestamp()))
        }
        None => eprintln!("{message}"),
    }
}

/// 只有服务模式要记的事。前台能直接看连接现状，多这几行只是噪音。
pub(super) fn note(message: &str) {
    if LOG_FILE.get().is_some() {
        report(message);
    }
}

/// 笔记本在酒店 Wi-Fi 上醒来，认证页面会劫持 TLS；反代更新时节点入口会短暂 404。前台进程退出让人去看，
/// 服务模式没有人看，退出就一直离线到下次开机，所以这些错误隔一分钟再试，进程不退。
/// 只有 Hub 身份对不上才停，退出码 0，服务管理器不再拉起：节点凭据在对方证明身份之前就发出去了，不能反复送给冒充者。
/// 401 不停：多半是凭据被撤销，但也可能是反代自己的认证，响应没签名分不清，退出的话反代修好了节点也回不来。
/// 所以先按撤销处理、停掉这里起的命令，再每十分钟试一次。
pub fn run(config: NodeConfig) -> Result<(), String> {
    config.validate()?;
    let hub = Arc::new(Hub::for_node(config.access())?);
    crate::async_rt::block_on(async move {
        let shutdown = client::stop_signal();
        tokio::pin!(shutdown);
        loop {
            match client::run_until(
                config.clone(),
                hub.clone(),
                &mut shutdown,
                client::Backoff::expecting_old_connection(),
            )
            .await
            {
                Ok(()) => {
                    report("stopped by the service manager");
                    break;
                }
                Err(error) if error.end == NodeEnd::WrongHub => {
                    report(&format!(
                        "{}; not retrying until this machine joins the Hub again",
                        error.message
                    ));
                    break;
                }
                Err(error) => {
                    let wait = if error.end == NodeEnd::Refused {
                        // 停命令要阻塞等子进程收尾，在异步上下文里直接调会 panic。
                        let _ = tokio::task::spawn_blocking(
                            crate::tools::workspace_runtime::terminate_all_sessions_everywhere,
                        )
                        .await;
                        report(&format!(
                            "{}; stopped this node's commands. If the credential was revoked, join the Hub again",
                            error.message
                        ));
                        RETRY_AFTER_REFUSED
                    } else {
                        report(&error.message);
                        RETRY_AFTER
                    };
                    report(&format!("trying again in {}s", wait.as_secs()));
                    tokio::select! {
                        biased;
                        _ = &mut shutdown => break,
                        _ = tokio::time::sleep(wait) => {}
                    }
                }
            }
        }
    });
    crate::tools::workspace_runtime::terminate_all_sessions_everywhere();
    Ok(())
}

/// 计划任务起的控制台程序会弹一个黑窗口，用户关掉它节点就死了。尽早脱离，日志已经写文件了。
#[cfg(windows)]
pub fn detach_console() {
    extern "system" {
        fn FreeConsole() -> i32;
    }
    unsafe {
        FreeConsole();
    }
}

#[cfg(not(windows))]
pub fn detach_console() {}

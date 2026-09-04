use std::time::Duration;

use gld_daemon::lifecycle::{self, DaemonPaths, DaemonProbe, StopOutcome};
use gld_daemon::server::{self, ServerOptions};
use serde_json::json;

use crate::backend::{check_version, current_executable};
use crate::cli::DaemonCmd;
use crate::error::{CliError, CliResult};
use crate::output::{human_duration, Output};

pub async fn run(command: &DaemonCmd, out: Output) -> CliResult {
    let paths = DaemonPaths::resolve()?;
    match command {
        DaemonCmd::Start => start(&paths, out).await,
        DaemonCmd::Stop { force, wait } => {
            stop(&paths, out, *force, Duration::from_secs(*wait)).await
        }
        DaemonCmd::Restart { force } => {
            stop(&paths, out, *force, Duration::from_secs(20)).await?;
            start(&paths, out).await
        }
        DaemonCmd::Status => status(&paths, out).await,
        DaemonCmd::Run { no_restore } => server::run(ServerOptions {
            paths,
            restore_runtime_state: !no_restore,
        })
        .await
        .map_err(CliError::from),
        DaemonCmd::Logs { lines, follow } => {
            super::logs::tail_files(
                out,
                &[("daemon.log".to_string(), paths.log.clone())],
                *lines,
                *follow,
            )
            .await
        }
    }
}

async fn start(paths: &DaemonPaths, out: Output) -> CliResult {
    match lifecycle::probe(paths).await {
        DaemonProbe::Running(info) => {
            if let Err(error) = check_version(&info) {
                return Err(CliError::new(format!(
                    "{error}（守护进程已在运行，pid {}）",
                    info.pid
                ))
                .with_exit(error.exit));
            }
            if !out.json_or(&json!({ "started": false, "pid": info.pid })) {
                out.line(format!(
                    "守护进程已在运行（pid {}，已运行 {}）。",
                    info.pid,
                    human_duration(info.uptime_secs)
                ));
            }
            return Ok(());
        }
        DaemonProbe::Unresponsive(record) => {
            return Err(CliError::new(format!(
                "守护进程（pid {}）存在但不响应，先执行 `gld daemon stop --force`。日志：{}",
                record.pid,
                paths.log.display()
            )));
        }
        DaemonProbe::Stale(_) => lifecycle::cleanup_stale(paths),
        DaemonProbe::NotRunning => {}
    }
    let pid = lifecycle::spawn_detached(paths, &current_executable()?)?;
    let info = lifecycle::wait_until_ready(paths, Duration::from_secs(15)).await?;
    check_version(&info)?;
    if !out.json_or(&json!({ "started": true, "pid": info.pid })) {
        out.line(format!("守护进程已启动（pid {pid}）。"));
        out.line(format!("  socket  {}", info.socket.display()));
        out.line(format!("  日志    {}", info.log_file.display()));
        if info.running_services > 0 {
            out.line(format!(
                "  已恢复 {} 个服务，`gld ps` 查看。",
                info.running_services
            ));
        }
    }
    Ok(())
}

async fn stop(paths: &DaemonPaths, out: Output, force: bool, wait: Duration) -> CliResult {
    let outcome = lifecycle::request_stop(paths, wait, force).await?;
    let (message, pid) = match outcome {
        StopOutcome::Stopped(pid) => (
            format!("守护进程已退出（pid {pid}），所有服务已停止。"),
            Some(pid),
        ),
        StopOutcome::Killed(pid) => (
            format!("守护进程未在超时内退出，已强制结束（pid {pid}）。"),
            Some(pid),
        ),
        StopOutcome::WasStale(pid) => (
            format!("守护进程早已不在（记录的 pid {pid}），已清理残留文件。"),
            None,
        ),
        StopOutcome::NotRunning => ("守护进程未运行。".to_string(), None),
    };
    if !out.json_or(&json!({ "outcome": format!("{outcome:?}"), "pid": pid })) {
        out.line(message);
    }
    Ok(())
}

async fn status(paths: &DaemonPaths, out: Output) -> CliResult {
    let probe = lifecycle::probe(paths).await;
    match probe {
        DaemonProbe::Running(info) => {
            let version_ok = check_version(&info).is_ok();
            if !out.json_or(
                &json!({ "running": true, "info": info, "version_matches_cli": version_ok }),
            ) {
                out.kv(&[
                    ("状态", out.green("运行中")),
                    ("pid", info.pid.to_string()),
                    (
                        "版本",
                        format!("{}（协议 {}）", info.version, info.protocol),
                    ),
                    ("运行时长", human_duration(info.uptime_secs)),
                    ("运行中的服务", info.running_services.to_string()),
                    ("数据目录", info.data_home.display().to_string()),
                    ("socket", info.socket.display().to_string()),
                    ("日志", info.log_file.display().to_string()),
                ]);
                if !version_ok {
                    out.line(out.yellow(&format!(
                        "命令行版本 {} 与守护进程不一致，请执行 gld daemon restart。",
                        gld_core::VERSION
                    )));
                }
            }
            Ok(())
        }
        DaemonProbe::Unresponsive(record) => {
            if !out.json_or(&json!({ "running": false, "unresponsive": true, "record": record })) {
                out.line(out.yellow(&format!(
                    "守护进程（pid {}）存在但不响应。日志：{}",
                    record.pid,
                    paths.log.display()
                )));
            }
            Err(CliError::daemon_not_running(""))
        }
        DaemonProbe::Stale(record) => {
            if !out.json_or(&json!({ "running": false, "stale_record": record })) {
                out.line(format!(
                    "守护进程未运行（上次 pid {} 的记录已失效，`gld daemon start` 会自动清理）。",
                    record.pid
                ));
            }
            Err(CliError::daemon_not_running(""))
        }
        DaemonProbe::NotRunning => {
            if !out.json_or(&json!({ "running": false })) {
                out.line("守护进程未运行。执行 `gld start` 或 `gld daemon start` 会拉起它。");
                out.line(format!("  数据目录  {}", paths.home.display()));
            }
            Err(CliError::daemon_not_running(""))
        }
    }
}

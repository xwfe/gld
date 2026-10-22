use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gld_core::data::DataStore;
use gld_core::tools::runs;
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
            if !force {
                refuse_while_commands_run(&paths, out).await?;
            }
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

/// 守护进程起的命令还在跑就不重启：重启会把它们（包括开着的 dev server）一起停掉。
///
/// 以前这一步靠人记着先查，忘了就把正跑着的测试、服务停了。看的是盘上的运行记录，不问
/// 守护进程：换完二进制、重启之前，守护进程还是旧版，新命令行跟它的协议不一定对得上。
/// 守护进程不在、不响应时不拦：前者没东西可停，后者本来就要靠重启救。
async fn refuse_while_commands_run(paths: &DaemonPaths, out: Output) -> CliResult {
    let DaemonProbe::Running(info) = lifecycle::probe(paths).await else {
        return Ok(());
    };
    let commands = runs::running_under(&paths.home.join("runs"), info.pid);
    // 委派出去的 Agent 也是守护进程起的，重启一样会停掉（RFC-0009）。
    let delegated = gld_core::agents::running_under(&paths.home, info.pid);
    if commands.is_empty() && delegated.is_empty() {
        return Ok(());
    }
    let names = project_names();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64);
    // 每一行：(项目, 在跑什么, 跑了几秒, pid, JSON)。
    let mut rows: Vec<(String, String, u64, Option<u32>, serde_json::Value)> = Vec::new();
    for (project, record) in &commands {
        let name = names
            .get(project)
            .cloned()
            .unwrap_or_else(|| project.clone());
        let secs = now.saturating_sub(record.started_at_ms) / 1000;
        let what = match record.service_port {
            Some(port) => format!("{}（服务，端口 {port}）", record.command),
            None => record.command.clone(),
        };
        let json = json!({
            "kind": "command",
            "project": name,
            "session_id": record.session_id,
            "command": record.command,
            "pid": record.pid,
            "service_port": record.service_port,
            "running_secs": secs,
        });
        rows.push((name, what, secs, record.pid, json));
    }
    for task in &delegated {
        let secs = now.saturating_sub(task.started_at_ms.unwrap_or(task.created_at_ms)) / 1000;
        let preview: String = task.prompt_preview.chars().take(40).collect();
        let what = format!("委派给 {}：{}", task.agent, preview.replace('\n', " "));
        let json = json!({
            "kind": "agent",
            "project": task.workspace_name,
            "task_id": task.task_id,
            "agent": task.agent,
            "pid": task.pid,
            "running_secs": secs,
        });
        rows.push((task.workspace_name.clone(), what, secs, task.pid, json));
    }
    let advice = "等命令跑完再重启；确定要停掉就加 --force：gld daemon restart --force";
    let json_rows: Vec<_> = rows.iter().map(|row| row.4.clone()).collect();
    if out.json_or(&json!({ "restarted": false, "running_commands": json_rows })) {
        return Err(CliError::new(format!(
            "有 {} 条命令在跑，没重启。{advice}",
            rows.len()
        )));
    }
    let mut message = format!(
        "有 {} 条命令在跑，重启会连命令一起停掉，这次没重启：",
        rows.len()
    );
    for (name, what, secs, pid, _) in &rows {
        let pid = pid.map(|pid| format!("，pid {pid}")).unwrap_or_default();
        message.push_str(&format!(
            "\n  {name}  {what}  已跑 {}{pid}",
            human_duration(*secs)
        ));
    }
    message.push('\n');
    message.push_str(advice);
    Err(CliError::new(message))
}

/// 项目 id → 项目名。读不出来就空着，列表里退回显示 id，不因此拦不住重启。
fn project_names() -> HashMap<String, String> {
    DataStore::read_file(|data| Ok(data.profiles.clone()))
        .unwrap_or_default()
        .into_iter()
        .map(|profile| {
            let root = Path::new(&profile.path);
            let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
            (runs::project_id(&root), profile.name)
        })
        .collect()
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
                        "命令行版本 {} 与守护进程不一致，守护进程还在跑旧代码。换完二进制要 gld daemon restart：服务会断开片刻；有命令在跑时它先列出来、不重启，见 docs/install.md 的「升级」。",
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

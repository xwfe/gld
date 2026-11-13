//! 子进程、stdin 与后台结束监视。

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::command_spec::CommandSpec;
use crate::session::{ExecSession, SessionStore};
use crate::workspace::WorkspaceError;

use super::platform::{command_for_program, platform_command_path};
use super::resolve::parse_and_resolve;
use super::{merge_exec_result, withheld_env_names, Execution, StdinPlan};

/// 按计划把初始输入写进去，该关的关掉。
async fn apply_stdin_plan(
    session: &std::sync::Arc<ExecSession>,
    plan: &StdinPlan,
) -> Result<(), WorkspaceError> {
    use tokio::io::AsyncWriteExt;
    let text = match plan {
        StdinPlan::CloseImmediately => "",
        StdinPlan::Once(text) | StdinPlan::Interactive(text) => text.as_str(),
    };
    let mut stdin_guard = session.stdin.lock().await;
    if let Some(stdin) = stdin_guard.as_mut() {
        if !text.is_empty() {
            stdin
                .write_all(text.as_bytes())
                .await
                .map_err(|_| WorkspaceError::Tool {
                    code: "SESSION_CLOSED",
                    message: "Failed to write stdin.".into(),
                    category: "runtime",
                    retryable: false,
                })?;
            stdin.flush().await.ok();
        }
        if !plan.keeps_stdin_open() {
            let _ = stdin.shutdown().await;
        }
    }
    if !plan.keeps_stdin_open() {
        *stdin_guard = None;
        drop(stdin_guard);
        session.mark_stdin_closed();
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_command(
    execution: &Execution<'_>,
    sessions: &Arc<SessionStore>,
    spec: &CommandSpec,
    cwd: &Path,
    limit: Duration,
    yield_time: Duration,
    max_output: usize,
    stdin_plan: &StdinPlan,
    service_port: Option<u16>,
) -> Result<Value, WorkspaceError> {
    let tty = stdin_plan.keeps_stdin_open();
    let cmd = spec.display.as_str();
    let search_path = execution.search_path.as_deref();
    let (program, args) = parse_and_resolve(
        spec,
        cwd,
        execution.workspace.root(),
        &execution.executables,
        search_path,
    )?;
    let start = Instant::now();

    let mut command = command_for_program(&program, &args);
    if let Some(path) = search_path {
        command.env("PATH", path);
    }
    // 不把 gld 自己的数据目录交给项目命令。守护进程起的时候设了 GLD_HOME，不去掉的话子进程
    // 全都继承：项目里跑 gld 自己的测试（D10 拿 gld 当项目），测试的隔离函数看到 GLD_HOME
    // 已设就照用，往真实的 ~/.config/gld 里写了几百个临时项目的任务记录和锁文件。项目命令也
    // 本来就不该知道 gld 的凭据放在哪。
    for name in execution.remove_env {
        command.env_remove(name);
    }
    // 名字像密钥的环境变量也不给。守护进程从哪儿起就带着哪儿的环境：2026-10-08 本机那个是从 Claude Code
    // 会话里起的，带着 CLAUDE_CODE_MESSAGING_TOKEN，ChatGPT 经 gld 跑一句
    // `python3 -c "import os; print(os.environ)"` 就读得到。只在这里去掉、守护进程自己留着：本机 MCP 配置里的
    // `${GITHUB_TOKEN}` 要靠它展开。名字看着不像的（`DATABASE_URL` 里带口令）挡不住。
    for name in withheld_env_names() {
        command.env_remove(name);
    }
    command
        .current_dir(platform_command_path(cwd))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    #[cfg(windows)]
    command
        .env("PYTHONUTF8", "1")
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONLEGACYWINDOWSSTDIO", "0");

    // Its own process group, so a timeout or kill_session reaches whatever
    // the command started in the background too, not only the command.
    #[cfg(unix)]
    command.process_group(0);

    let child = command.spawn().map_err(|e| WorkspaceError::ToolDetails {
        code: "COMMAND_SPAWN_FAILED",
        message: format!("Failed to start command: {e}"),
        category: "runtime",
        retryable: true,
        details: json!({
            "termination_reason": "spawn_failed",
            "recoverable": true,
            "suggestion": "检查命令路径、权限和运行时环境后重试"
        }),
    })?;

    let session = sessions.insert(
        ExecSession::new_with_mode(
            child,
            tty,
            cmd.to_string(),
            execution.runtime.write_counter(),
        )
        .with_service_port(service_port),
    );
    session.spawn_readers().await;
    let deadline = start + limit;

    // **stdin 先安排好，再考虑要不要立刻返回。**顺序反过来的后果是
    // `yield_time_ms: 0` 那一路把初始输入整个丢掉：命令拿到一个开着、却永远
    // 不会有数据的 stdin，于是挂到超时——而调用方明明传了 stdin（审查 X03、
    // 方案 E "先设置输入/EOF 语义，再进行 yield"）。
    if let Err(error) = apply_stdin_plan(&session, stdin_plan).await {
        // 命令已经起来了：不交给监视任务的话，没人在它结束时记下结局（运行记录一直停在
        // running），到了 timeout_ms 也没人停它。
        spawn_timeout_monitor(sessions.clone(), session.clone(), deadline);
        return Err(error);
    }

    if yield_time.is_zero() {
        let snapshot = session.snapshot(max_output);
        spawn_timeout_monitor(sessions.clone(), session.clone(), deadline);
        return Ok(merge_exec_result(snapshot, start, cmd, cwd, true));
    }

    loop {
        session.refresh_status().await;
        if session.has_exited() {
            session.wait_for_readers().await;
            let snapshot = session.snapshot(max_output);
            // The result hands out output_refs (and says when it cut the
            // output), so they must stay readable for a while, like after a
            // timeout; removing the session here made every one of them
            // SESSION_NOT_FOUND.
            schedule_session_eviction(sessions.clone(), session.session_id.clone());
            return Ok(merge_exec_result(snapshot, start, cmd, cwd, false));
        }
        if !tty && Instant::now() >= deadline {
            session.mark_termination_reason("timeout");
            session.kill_and_wait().await;
            session.refresh_status().await;
            session.wait_for_readers().await;
            let snapshot = session.snapshot(max_output);
            // Snapshot is embedded; schedule eviction so abandoned timeouts do not linger.
            schedule_session_eviction(sessions.clone(), session.session_id.clone());
            return Err(WorkspaceError::ToolDetails {
                code: "TIMEOUT",
                message: "Command timed out.".into(),
                category: "runtime",
                retryable: true,
                details: json!({
                    "termination_reason": "timeout",
                    "recoverable": true,
                    "suggestion": "读取 output_refs，调整 timeout_ms 后重试",
                    "session": snapshot
                }),
            });
        }
        // 服务一答话就返回，不等满 yield_time。
        let ready = service_port.is_some_and(|port| !crate::service::answering(port).is_empty());
        if ready || Instant::now() - start >= yield_time || tty {
            let snapshot = session.snapshot(max_output);
            spawn_timeout_monitor(sessions.clone(), session.clone(), deadline);
            return Ok(merge_exec_result(snapshot, start, cmd, cwd, true));
        }
        // 等服务时每次都要连一下端口，问得稀一点。
        let poll = if service_port.is_some() { 100 } else { 20 };
        tokio::time::sleep(Duration::from_millis(poll)).await;
    }
}

/// 隔多久问一次转了后台的命令结束没有。
///
/// 以前没人问：后台命令结束了，要等有人 `read_output` 或者到了 `timeout_ms` 才知道。
/// 运行记录要在结束那一刻落终态——之后 gld 重启了，那份记录就是唯一知道结局的地方
/// （审查 D09）。一次询问是一个不阻塞的 `try_wait`，200 毫秒一次不痛不痒。
const EXIT_POLL: Duration = Duration::from_millis(200);

fn spawn_timeout_monitor(
    sessions: Arc<SessionStore>,
    session: Arc<ExecSession>,
    deadline: Instant,
) {
    crate::async_rt::spawn(async move {
        loop {
            session.refresh_status().await;
            if session.has_exited() {
                break;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                session.mark_termination_reason("timeout");
                session.kill_and_wait().await;
                session.refresh_status().await;
                session.wait_for_readers().await;
                break;
            }
            tokio::time::sleep(remaining.min(EXIT_POLL)).await;
        }
        // Keep the session briefly so clients can still read_output / probe status.
        schedule_session_eviction(sessions, session.session_id.clone());
    });
}

/// 到保留期就把会话从表里去掉，把那两个 1 MiB 的缓冲还回去。
///
/// **判据不在这里**，在 `SessionStore::sweep`：这个定时器只是让内存早点还，
/// 睡过头或者根本没跑（进程被 SIGKILL）也不影响正确性，下一次 get / insert
/// 会扫到它。保留期统一从进程结束那一刻算（审查 X02）。
fn schedule_session_eviction(sessions: Arc<SessionStore>, session_id: String) {
    crate::async_rt::spawn(async move {
        tokio::time::sleep(sessions.retention()).await;
        sessions.remove(&session_id);
    });
}

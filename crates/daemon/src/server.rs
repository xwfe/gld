//! 守护进程主循环。
//!
//! 启动顺序（任一步失败都直接退出，错误打到 stderr，也就是 `daemon.log`）：
//!
//! 1. 拿 `daemon.lock` 排他锁（同一数据目录只允许一个实例）；
//! 2. 清掉上次残留的 socket 文件，绑定新的；
//! 3. 加载 [`App`]（读 `data/profiles.json`）；
//! 4. 写 `daemon.json`；
//! 5. 若设置了“启动时恢复”，把上次运行的 MCP / Actions 拉起来；
//! 6. 进入 accept 循环，每个连接一个任务；
//! 7. 收到 `shutdown` 请求或 SIGTERM / SIGINT 后：停掉所有服务、隧道、全局入口，
//!    删除 socket 与 `daemon.json`，释放锁，退出。

use std::sync::Arc;
use std::time::Duration;

use gld_core::app::App;
use gld_core::{AppError, AppResult};
use tokio::io::BufReader;
use tokio::sync::watch;

use crate::dispatch::{dispatch, DaemonContext};
use crate::ipc;
use crate::lifecycle::{acquire_instance_lock, now_unix, DaemonPaths, DaemonRecord};
use crate::logging;
use crate::protocol::{DaemonInfo, Request, Response, RpcError, PROTOCOL_VERSION};

/// 读取一条请求最多等这么久；本机连接正常情况下是毫秒级。
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub paths: DaemonPaths,
    /// 是否按设置里的“启动时恢复”清单拉起服务。
    pub restore_runtime_state: bool,
}

pub async fn run(options: ServerOptions) -> AppResult<()> {
    let paths = options.paths;
    let _lock = acquire_instance_lock(&paths)?;

    if ipc::is_listening(&paths.socket).await {
        return Err(AppError::Message(format!(
            "{} 已有守护进程在监听，本实例退出。",
            paths.socket.display()
        )));
    }
    ipc::cleanup(&paths.socket);

    let app = Arc::new(App::load()?);
    let listener = ipc::bind(&paths.socket).map_err(|error| {
        AppError::Message(format!("绑定 {} 失败：{error}", paths.socket.display()))
    })?;

    let started_at = now_unix();
    let record = DaemonRecord {
        pid: std::process::id(),
        version: gld_core::VERSION.to_string(),
        protocol: PROTOCOL_VERSION,
        started_at_unix: started_at,
        socket: paths.socket.clone(),
        log: paths.log.clone(),
    };
    record.write(&paths.record)?;
    logging::info(format!(
        "daemon v{} pid {} listening on {} (home {})",
        record.version,
        record.pid,
        paths.socket.display(),
        paths.home.display()
    ));

    gld_core::tunnel::ensure_frp_health_loop();

    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    spawn_signal_handler(shutdown_tx.clone());

    let context = Arc::new(DaemonContext {
        info: {
            let app = app.clone();
            let paths = paths.clone();
            Box::new(move || DaemonInfo {
                pid: std::process::id(),
                version: gld_core::VERSION.to_string(),
                protocol: PROTOCOL_VERSION,
                started_at_unix: started_at,
                uptime_secs: now_unix().saturating_sub(started_at),
                data_home: paths.home.clone(),
                socket: paths.socket.clone(),
                log_file: paths.log.clone(),
                running_services: app.running_services().map(|s| s.len()).unwrap_or(0),
            })
        },
        request_shutdown: {
            let shutdown_tx = shutdown_tx.clone();
            Box::new(move || {
                let _ = shutdown_tx.send(true);
            })
        },
    });

    if options.restore_runtime_state {
        match app.restore_runtime_state().await {
            Ok(restored) if restored.is_empty() => {}
            Ok(restored) => logging::info(format!(
                "restored {} service(s): {}",
                restored.len(),
                restored
                    .iter()
                    .map(|s| format!("{}:{}", s.kind, s.workspace_id))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            Err(error) => logging::warn(format!("restore runtime state failed: {error}")),
        }
    }

    loop {
        tokio::select! {
            accepted = ipc::accept(&listener) => match accepted {
                Ok(stream) => {
                    let app = app.clone();
                    let context = context.clone();
                    tokio::spawn(async move {
                        handle_connection(app, context, stream).await;
                    });
                }
                Err(error) => {
                    logging::warn(format!("accept failed: {error}"));
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow() {
                    break;
                }
            }
        }
    }

    logging::info("shutdown requested; stopping services");
    drop(listener);
    app.shutdown_all().await;
    ipc::cleanup(&paths.socket);
    let _ = std::fs::remove_file(&paths.record);
    logging::info("daemon exited cleanly");
    Ok(())
}

async fn handle_connection(app: Arc<App>, context: Arc<DaemonContext>, mut stream: ipc::Stream) {
    let (read_half, mut write_half) = tokio::io::split(&mut stream);
    let mut reader = BufReader::new(read_half);

    let line = match tokio::time::timeout(REQUEST_READ_TIMEOUT, ipc::read_line(&mut reader)).await {
        Ok(Ok(Some(line))) => line,
        Ok(Ok(None)) => return,
        Ok(Err(error)) => {
            let _ = reply(&mut write_half, Err(RpcError::protocol(error.to_string()))).await;
            return;
        }
        Err(_) => {
            let _ = reply(&mut write_half, Err(RpcError::protocol("读取请求超时"))).await;
            return;
        }
    };

    let request: Request = match serde_json::from_str(&line) {
        Ok(request) => request,
        Err(error) => {
            logging::warn(format!("malformed request: {error}"));
            let _ = reply(
                &mut write_half,
                Err(RpcError::protocol(format!("请求无法解析：{error}"))),
            )
            .await;
            return;
        }
    };

    let op = request.op_name();
    let started = std::time::Instant::now();
    // 单独 spawn 一层：业务代码 panic 只影响这条连接，并且能转成错误回给客户端。
    let outcome = tokio::spawn({
        let app = app.clone();
        let context = context.clone();
        async move { dispatch(&app, Some(&context), request).await }
    })
    .await
    .unwrap_or_else(|join_error| {
        Err(RpcError::internal(format!(
            "处理请求时守护进程内部出错：{join_error}"
        )))
    });

    match &outcome {
        Ok(_) => logging::info(format!("{op} ok ({} ms)", started.elapsed().as_millis())),
        Err(error) => logging::warn(format!(
            "{op} failed ({} ms): {}",
            started.elapsed().as_millis(),
            error.message
        )),
    }
    let _ = reply(&mut write_half, outcome).await;
}

async fn reply<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    outcome: Result<serde_json::Value, RpcError>,
) -> std::io::Result<()> {
    let response: Response = outcome.into();
    let text = serde_json::to_string(&response).unwrap_or_else(|_| {
        r#"{"status":"error","error":{"code":"internal","message":"serialize failed"}}"#.into()
    });
    ipc::write_line(writer, &text).await
}

fn spawn_signal_handler(shutdown_tx: watch::Sender<bool>) {
    tokio::spawn(async move {
        let name = wait_for_signal().await;
        logging::info(format!("received {name}"));
        let _ = shutdown_tx.send(true);
    });
}

#[cfg(unix)]
async fn wait_for_signal() -> &'static str {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = match signal(SignalKind::terminate()) {
        Ok(stream) => stream,
        Err(_) => return std::future::pending().await,
    };
    let mut int = match signal(SignalKind::interrupt()) {
        Ok(stream) => stream,
        Err(_) => return std::future::pending().await,
    };
    let mut hup = match signal(SignalKind::hangup()) {
        Ok(stream) => stream,
        Err(_) => return std::future::pending().await,
    };
    tokio::select! {
        _ = term.recv() => "SIGTERM",
        _ = int.recv() => "SIGINT",
        _ = hup.recv() => "SIGHUP",
    }
}

#[cfg(windows)]
async fn wait_for_signal() -> &'static str {
    let _ = tokio::signal::ctrl_c().await;
    "Ctrl-C"
}

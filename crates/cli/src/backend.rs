//! 后端选择：请求发给守护进程，还是在当前进程里直接执行。
//!
//! 规则（见 `Request::needs_daemon`）：
//!
//! 1. 守护进程在跑 → 一律转发给它（它持有内存里的运行状态，且是数据文件的唯一写入者）；
//! 2. 没在跑、请求只是改配置 / 看日志 → 进程内直连执行；
//! 3. 没在跑、请求要启动 / 停止服务 → 先在后台拉起守护进程，再转发。
//!    `--no-autostart` 可关掉第 3 条，此时报错退出码 3。
//!
//! 转发前会核对守护进程的版本与协议版本；升级二进制后旧进程还在跑时，
//! 直接报错让用户 `gld daemon restart`，而不是发它不认识的请求。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gld_core::app::App;
use gld_daemon::lifecycle::{self, DaemonPaths, DaemonProbe};
use gld_daemon::{Client, ClientError, Request, PROTOCOL_VERSION};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::{CliError, CliResult, ExitCode};
use crate::output::Output;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const SLOW_TIMEOUT: Duration = Duration::from_secs(180);
const AUTOSTART_READY_TIMEOUT: Duration = Duration::from_secs(15);

pub struct Backend {
    paths: DaemonPaths,
    mode: Mode,
    autostart: bool,
    timeout_override: Option<Duration>,
    out: Output,
}

enum Mode {
    Remote(Client),
    Local(Option<Arc<App>>),
}

impl Backend {
    pub async fn connect(
        autostart: bool,
        timeout_override: Option<Duration>,
        out: Output,
    ) -> CliResult<Self> {
        let paths = DaemonPaths::resolve()?;
        let mode = match lifecycle::probe(&paths).await {
            DaemonProbe::Running(info) => {
                check_version(&info)?;
                Mode::Remote(Client::new(paths.clone()))
            }
            DaemonProbe::Unresponsive(record) => {
                return Err(CliError::new(format!(
                    "守护进程（pid {}）存在但不响应。查看日志 {} ，或执行 `gld daemon stop --force` 后重试。",
                    record.pid,
                    paths.log.display()
                )));
            }
            DaemonProbe::Stale(_) => {
                lifecycle::cleanup_stale(&paths);
                Mode::Local(None)
            }
            DaemonProbe::NotRunning => Mode::Local(None),
        };
        Ok(Self {
            paths,
            mode,
            autostart,
            timeout_override,
            out,
        })
    }

    pub fn paths(&self) -> &DaemonPaths {
        &self.paths
    }

    pub fn is_remote(&self) -> bool {
        matches!(self.mode, Mode::Remote(_))
    }

    /// 命令行要退出了：直连模式下停掉这个进程里还在跑的命令，返回停掉的条数。
    ///
    /// 没有守护进程时，`gld tool call` 起的命令是这个命令行进程的子进程。命令行一退，还在跑
    /// 的就成了孤儿：下一条命令是新进程，停不掉它，它以你的身份一直跑到自己的 timeout。停掉的
    /// 记成 `interrupted`，下一条命令从运行记录读得到它停之前的输出（审查 D09）。经守护进程的
    /// 不归这里管，守护进程退出时自己收。
    ///
    /// 停命令要阻塞等子进程收尾，不能在异步运行时里直接调（会 panic），放到阻塞线程里。
    pub async fn finish(&self) -> usize {
        match self.mode {
            Mode::Local(Some(_)) => tokio::task::spawn_blocking(
                gld_core::tools::workspace_runtime::terminate_all_sessions_everywhere,
            )
            .await
            .unwrap_or(0),
            _ => 0,
        }
    }

    pub async fn call(&mut self, request: Request) -> CliResult<Value> {
        if !self.is_remote() && request.needs_daemon() {
            self.autostart_daemon().await?;
        }
        let timeout = self.timeout_override.unwrap_or(if request.is_slow() {
            SLOW_TIMEOUT
        } else {
            DEFAULT_TIMEOUT
        });
        match &mut self.mode {
            Mode::Remote(client) => {
                let client = client.clone().with_timeout(timeout);
                client.call(&request).await.map_err(map_client_error)
            }
            Mode::Local(app) => {
                let app = match app {
                    Some(app) => app.clone(),
                    None => {
                        let loaded = Arc::new(App::load()?);
                        *app = Some(loaded.clone());
                        loaded
                    }
                };
                gld_daemon::dispatch::dispatch(&app, None, request)
                    .await
                    .map_err(|error| CliError::new(error.message))
            }
        }
    }

    pub async fn call_typed<T: DeserializeOwned>(&mut self, request: Request) -> CliResult<T> {
        let value = self.call(request).await?;
        serde_json::from_value(value)
            .map_err(|error| CliError::new(format!("响应格式不符合预期：{error}")))
    }

    async fn autostart_daemon(&mut self) -> CliResult<()> {
        if !self.autostart {
            return Err(CliError::daemon_not_running(
                "守护进程未运行，且已指定 --no-autostart。请先执行 `gld daemon start`。",
            ));
        }
        let executable = current_executable()?;
        let pid = lifecycle::spawn_detached(&self.paths, &executable)?;
        self.out.note(format!(
            "守护进程未运行，已在后台启动（pid {pid}），等待就绪…"
        ));
        let info = lifecycle::wait_until_ready(&self.paths, AUTOSTART_READY_TIMEOUT).await?;
        check_version(&info)?;
        self.mode = Mode::Remote(Client::new(self.paths.clone()));
        Ok(())
    }
}

pub fn current_executable() -> CliResult<PathBuf> {
    std::env::current_exe()
        .map_err(|error| CliError::new(format!("无法确定当前可执行文件路径：{error}")))
}

pub fn check_version(info: &gld_daemon::DaemonInfo) -> CliResult<()> {
    if info.protocol != PROTOCOL_VERSION || info.version != gld_core::VERSION {
        return Err(CliError::new(format!(
            "守护进程版本 {}（协议 {}）与命令行版本 {}（协议 {}）不一致，请执行 `gld daemon restart`。",
            info.version,
            info.protocol,
            gld_core::VERSION,
            PROTOCOL_VERSION
        ))
        .with_exit(ExitCode::VersionMismatch));
    }
    Ok(())
}

fn map_client_error(error: ClientError) -> CliError {
    match error {
        ClientError::Rpc(rpc) => CliError::new(rpc.message),
        ClientError::NotRunning(io) => {
            CliError::daemon_not_running(format!("守护进程在处理过程中失去响应（{io}）"))
        }
        other => CliError::new(other.to_string()),
    }
}

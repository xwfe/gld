//! 命令行错误与退出码。
//!
//! | 退出码 | 含义 |
//! | --- | --- |
//! | 0 | 成功 |
//! | 1 | 一般错误（参数合法，但操作失败：端口冲突、工作区不存在……） |
//! | 2 | 用法错误（clap 自动产生） |
//! | 3 | 守护进程未运行（`gld daemon status` 等探测类命令用，方便脚本判断） |
//! | 4 | 守护进程版本与命令行不一致，需要 `gld daemon restart` |

use gld_daemon::ClientError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    Failure,
    DaemonNotRunning,
    VersionMismatch,
}

impl ExitCode {
    pub fn code(self) -> i32 {
        match self {
            ExitCode::Failure => 1,
            ExitCode::DaemonNotRunning => 3,
            ExitCode::VersionMismatch => 4,
        }
    }
}

#[derive(Debug)]
pub struct CliError {
    pub message: String,
    pub exit: ExitCode,
}

impl CliError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            exit: ExitCode::Failure,
        }
    }

    pub fn with_exit(mut self, exit: ExitCode) -> Self {
        self.exit = exit;
        self
    }

    pub fn daemon_not_running(message: impl Into<String>) -> Self {
        Self::new(message).with_exit(ExitCode::DaemonNotRunning)
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CliError {}

impl From<gld_core::AppError> for CliError {
    fn from(error: gld_core::AppError) -> Self {
        Self::new(error.to_string())
    }
}

impl From<ClientError> for CliError {
    fn from(error: ClientError) -> Self {
        match error {
            ClientError::NotRunning(_) => Self::daemon_not_running(error.to_string()),
            other => Self::new(other.to_string()),
        }
    }
}

impl From<serde_json::Error> for CliError {
    fn from(error: serde_json::Error) -> Self {
        Self::new(format!("JSON 处理失败：{error}"))
    }
}

impl From<std::io::Error> for CliError {
    fn from(error: std::io::Error) -> Self {
        Self::new(error.to_string())
    }
}

pub type CliResult<T = ()> = Result<T, CliError>;

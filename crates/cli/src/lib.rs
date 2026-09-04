//! `gld` 命令行。
//!
//! 职责只有三件：解析参数、选择后端（守护进程 / 进程内直连）、把结果打印出来。
//! 所有业务逻辑都在 `gld-core` 的 `app` 层，命令行不直接碰状态。
//!
//! 做成 lib 是为了让集成测试能拿到 clap 定义——例如校验
//! `gld doctor` 给出的每条修复命令是否真的存在。二进制入口在 `main.rs`。

pub mod backend;
pub mod cli;
pub mod commands;
pub mod error;
pub mod output;

pub use cli::Cli;
pub use commands::run;
pub use error::{CliError, CliResult, ExitCode};

//! `gld-core`：Workspace-first 的 MCP / GPT Actions 运行时内核。
//!
//! 这一层不知道命令行、守护进程或任何 UI 的存在。它只提供：
//!
//! - [`tools`]：统一工具内核（文件 / Patch / 命令 / Git / History / Planning）；
//! - [`mcp`] 与 [`actions`]：两条 HTTP transport，最终都进入 `tools::call_tool`；
//! - [`auth`]：Bearer 与 OAuth（Authorization Code + PKCE + DCR + Refresh Token）；
//! - [`tunnel`] 与 [`global_gateway`]：FRP / Cloudflare 隧道与共享公网入口；
//! - [`runtime`]：单个进程内 MCP / Actions 监听器的生命周期；
//! - [`app`]：面向调用方的应用服务层（工作区、密钥、设置、启停），
//!   命令行与守护进程都只通过它操作状态。
//!
//! 所有持久化数据都放在 [`home::data_home`] 返回的目录下（默认 `~/.config/gld`，
//! 可用环境变量 `GLD_HOME` 覆盖）。

pub mod actions;
pub mod agent_context;
pub mod app;
pub mod async_rt;
pub mod auth;
pub mod data;
pub mod error;
pub mod global_gateway;
pub mod harness;
pub mod health;
pub mod home;
pub mod local_network;
pub mod logs;
pub mod mcp;
pub mod planning;
pub mod platform;
pub mod runtime;
pub mod secret;
pub mod settings;
pub mod tools;
pub mod tunnel;
pub mod usage;
pub mod workspace;

pub use error::{AppError, AppResult};

/// 当前 crate 版本；MCP `serverInfo.version` 与命令行 `--version` 共用。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

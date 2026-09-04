//! `gld-daemon`：把 [`gld_core::app::App`] 放进一个常驻后台进程，并给命令行
//! 提供一条本机 IPC 通道。
//!
//! 为什么需要守护进程：MCP / Actions 监听器和 frpc / cloudflared 子进程都
//! 活在启动它们的进程里。命令行一执行完就退出，服务也就没了。所以真正
//! 持有服务的是这里的守护进程，命令行只是它的遥控器。
//!
//! 分层：
//!
//! ```text
//! gld (CLI) ──Request──▶ ipc ──▶ server ──▶ dispatch ──▶ gld_core::app::App
//!            ◀─Response──      ◀──        ◀──
//! ```
//!
//! - [`protocol`]：请求 / 响应类型，JSON 编码，一行一条；
//! - [`ipc`]：Unix domain socket（macOS / Linux）或命名管道（Windows）；
//! - [`dispatch`]：把每个请求翻译成一次 `App` 调用，是协议与业务的唯一交界；
//! - [`server`]：接受连接、并发处理、处理信号与优雅退出；
//! - [`client`]：命令行侧的调用封装；
//! - [`lifecycle`]：socket / 锁 / pid 文件路径、后台拉起、探活、停止。

pub mod client;
pub mod dispatch;
pub mod ipc;
pub mod lifecycle;
pub mod logging;
pub mod protocol;
pub mod server;

pub use client::{Client, ClientError};
pub use lifecycle::{DaemonPaths, DaemonProbe, DaemonRecord};
pub use protocol::{DaemonInfo, Request, Response, RpcError, PROTOCOL_VERSION};

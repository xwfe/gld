//! 进程内 MCP / Actions 监听器的生命周期管理。

mod port;
mod supervisor;

pub use port::{await_listener_shutdown, is_own_process, port_busy_message, wait_for_port_free};
pub use supervisor::{RuntimeSupervisor, ServiceKind};

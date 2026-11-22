//! 进程内 MCP / Actions 监听器的生命周期管理。

mod host;
mod manager;
mod port;
mod workspace;

pub use gld_protocol::service::ServiceKind;
pub use host::ListenerHost;
#[cfg(test)]
pub(crate) use manager::test_listener_manager;
pub use manager::ListenerManager;
pub use port::{
    await_listener_shutdown, await_listener_shutdown_blocking, is_own_process, port_busy_message,
    wait_for_port_free, wait_for_port_free_blocking,
};
pub use workspace::WorkspaceListeners;

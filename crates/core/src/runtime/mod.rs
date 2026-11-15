//! 进程内 MCP / Actions 监听器的生命周期管理。

mod binding;
mod manager;
mod port;
mod supervisor;

pub use binding::{BoundListener, ListenerBinder, ShutdownSender};
pub use manager::ListenerManager;
pub use port::{await_listener_shutdown, is_own_process, port_busy_message, wait_for_port_free};
pub use supervisor::{RuntimeSupervisor, ServiceKind};

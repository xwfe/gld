//! 原生节点客户端、共享存储/配对与传输；宿主负责接受和登记连接。
//! 复用 bridge::peer 与现有 handle_request，不维护第二套 JSON-RPC 或消息恢复协议。

pub mod autostart;
pub mod client;
pub mod config;
pub mod doctor;
pub mod hub_key;
pub mod identity;
pub mod join;
pub mod local_config;
mod member;
pub mod pairing;
pub mod proxy;
pub mod service;
pub mod sharing;
pub mod transport;

pub use member::NativeMember;

#[doc(hidden)]
pub mod registry {
    pub use super::NativeMember;
}

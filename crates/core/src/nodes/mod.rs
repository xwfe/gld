//! 原生节点的鉴权、单连接登记与项目读取路由；邀请配对由后续切片接入。
//! 复用 bridge::peer 与现有 handle_request，不维护第二套 JSON-RPC 或消息恢复协议。

pub mod client;
pub mod config;
pub mod connection;
pub mod endpoint;
pub mod identity;
pub mod registry;
pub mod transport;

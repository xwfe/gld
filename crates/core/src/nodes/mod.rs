//! 原生节点的邀请配对、鉴权、单连接登记与项目共享路由。
//! 复用 bridge::peer 与现有 handle_request，不维护第二套 JSON-RPC 或消息恢复协议。

pub mod autostart;
pub mod client;
pub mod config;
pub mod connection;
pub mod doctor;
pub mod endpoint;
pub mod hub_key;
pub mod identity;
pub mod join;
pub mod local_config;
pub mod pairing;
pub mod proxy;
pub mod registry;
pub mod service;
pub mod sharing;
pub mod transport;

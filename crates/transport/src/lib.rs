//! MCP 对话与 stdio 传输，供 CCNM bridge 和原生节点共用。
//!
//! 这里只负责消息交换、超时、能力检查及子进程通道；
//! 身份校验、项目授权、连接登记与会话回收由调用方负责。

#![forbid(unsafe_code)]

pub mod capabilities;
pub mod peer;

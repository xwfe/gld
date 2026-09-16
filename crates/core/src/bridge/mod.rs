//! 接 ccnm 的远端 workspace（跨仓计划 toexec 的 V2-H，gld RFC-0002 第 5 节）。
//!
//! 这里只放「怎么跟 `ccnm mcp bridge` 说话」。hub 的成员分型、静态远端工具
//! 和连接生命周期在后续切片里加，**不在这一层**——这一层不认识 workspace、
//! 不读配置、不碰鉴权。

pub mod member;
pub mod peer;

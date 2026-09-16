//! 接 ccnm 的远端 workspace（跨仓计划 toexec 的 V2-H，gld RFC-0002 第 5 节）。
//!
//! 这里只放「怎么跟 `ccnm mcp bridge` 说话」和连接活多久，**不管路由**：
//! 这一层不认识 hub 的成员名单，不读数据文件，不碰鉴权。哪次调用落到哪个
//! 成员是 [`crate::hub`] 的事。
//!
//! | 模块 | 管什么 |
//! | --- | --- |
//! | [`peer`] | MCP stdio 客户端：握手、列工具、调用、ping、关闭 |
//! | [`member`] | 一个远端成员的配置，以及由它推出的 bridge argv |
//! | [`tools`] | hub 对外暴露的那几个 `remote_*` 工具（静态名单） |
//! | [`session`] | 连接池：按需开、一个成员一条、空闲回收 |

pub mod member;
pub mod peer;
pub mod session;
pub mod tools;

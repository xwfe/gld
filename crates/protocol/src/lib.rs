//! GLD 配置与进程间协议的数据契约。
//!
//! 这里只定义数据、默认值和纯转换。身份验证、进程探测、存储与 I/O 由调用侧负责。

#![forbid(unsafe_code)]

pub mod config;
pub mod daemon;
pub mod generation;
pub mod planning;
pub mod service;
pub mod tool_args;

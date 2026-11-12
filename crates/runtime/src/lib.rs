//! 工作区路径、文件读取与事务补丁执行。
//!
//! 调用侧传入保护目录，并负责授权、提交锁和会话账本；本层不读取应用配置，
//! 不绑定 HTTP、MCP、Agent 或后台命令会话。这是代码依赖边界，不是操作系统沙箱。

#![forbid(unsafe_code)]

pub mod file;
mod file_types;
pub mod image_tool;
pub mod notebook;
pub mod patch;
mod patch_diag;
pub mod workspace;

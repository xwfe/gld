//! 工作区文件、命令、会话与输出执行。
//!
//! 调用侧传入工作区、保护目录、调用主体及存储位置，并负责授权、任务账本、
//! Agent 编排与换代组合。本层持有执行资源，不读取应用配置或绑定服务入口。
//! 这是代码依赖边界，不是操作系统沙箱。

// 进程控制的系统调用只在专用子模块局部放行；其余执行代码禁止 unsafe。
#![deny(unsafe_code)]

pub mod async_rt;
pub mod command_spec;
pub mod exec;
pub mod file;
mod file_types;
pub mod image_tool;
pub mod notebook;
pub mod patch;
mod patch_diag;
pub mod redaction;
pub mod runs;
pub mod service;
pub mod session;
pub mod workspace;
pub mod workspace_runtime;

//! Agent 档案、任务记录与委派进程生命周期。
//!
//! 调用侧决定谁能委派、是否需审批，以及正在运行的任务如何影响工作区操作。

#![deny(unsafe_code)]

pub mod profile;
pub mod run;
pub mod store;

pub const DEFAULT_TIMEOUT_MINUTES: u64 = 30;

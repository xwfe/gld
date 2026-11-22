//! 执行代入口与双方共用的身份、工作区归属和私有管道适配。
//!
//! 子进程只经管道接收宿主已验证的请求，不拥有执行代路由器。

pub mod child;
mod service;
pub mod wire;

#[cfg(test)]
mod tests;

use std::path::Path;

pub use gld_protocol::generation::{
    BuildIdentity, GENERATION_PROTOCOL, GENERATION_PROTOCOL_MIN, STATE_FORMAT,
};
pub use service::{CallError, McpReply};

// 归属键仍由双方共用；Router 的实现已归 gld-host。
pub mod router {
    pub use super::{root_key, CallError, McpReply};
}

/// 工作区所有权的键：和 [`crate::tools::workspace_runtime::runtime_for`] 一样规范化路径，
/// 同一个目录不管从哪个入口进来都是同一个主人。
pub fn root_key(root: &Path) -> String {
    root.canonicalize()
        .unwrap_or_else(|_| root.to_path_buf())
        .display()
        .to_string()
}

/// 读取当前进程的构建和身份；协议模型只保存结果。
pub fn current_build_identity() -> BuildIdentity {
    BuildIdentity {
        version: crate::VERSION.to_string(),
        build_commit: crate::BUILD_COMMIT.map(str::to_string),
        generation_protocol: GENERATION_PROTOCOL,
        state_format: STATE_FORMAT,
        pid: std::process::id(),
        exe: std::env::current_exe().ok(),
    }
}

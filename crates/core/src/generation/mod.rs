//! 稳定宿主和可替换执行代（RFC-0008 §3.4）。
//!
//! `gld daemon run` 是宿主：对外监听、隧道、认证、配置写入都在它手里，升级工具实现时不动。
//! 工具真正在子进程 `gld daemon generation` 里跑，这个子进程就是“一代”：工具上下文、
//! 命令会话、写入序号、委派的 Agent 都归它。
//!
//! 这个模块分三块：
//!
//! - [`wire`]：两边走的消息，一行一条 JSON，走子进程的 stdin/stdout；
//! - [`child`]：子进程这头，收消息、跑工具；
//! - [`host`] 和 [`router`]：宿主这头，管子进程、决定请求进哪一代、切换、报状态。
//!
//! 没装路由器（[`router`] 返回 `None`）时一切照旧在本进程跑：直连模式的 `gld tool call`、
//! `gld node run` 和 core 的单元测试都是这样，它们本来就没有“对外监听活得比工具久”这个需求。

pub mod child;
mod host;
pub mod router;
pub mod wire;

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};

pub use router::{CallError, McpReply, Router};

/// 宿主和执行代之间消息的版本。形状有不兼容改动时加一；同时要决定 [`GENERATION_PROTOCOL_MIN`]。
/// 只加新操作不用加一：老二进制读不懂时回错误，不会执行，调用的一方自己要接得住（比如工作区搬不走就留着）。
pub const GENERATION_PROTOCOL: u32 = 1;
/// 这个宿主还能带的最老执行代协议。范围之外的二进制一律不让上线，失败发生在动路由之前。
pub const GENERATION_PROTOCOL_MIN: u32 = 1;
/// 盘上状态的格式版本：`profiles.json`、运行记录、任务和委派记录。
///
/// 两代要同时读同一份数据目录，所以必须相等。改了这些文件的不兼容格式就加一，
/// 不加的话新代会把旧代读不懂的东西写到盘上，而这种坏法回退也救不回来。
pub const STATE_FORMAT: u32 = 1;
/// 同时活着的代数上限（含准备好还没切过去的）。到了就拒绝再准备新的，绝不为了腾位置杀旧代。
pub const MAX_LIVE_GENERATIONS: usize = 3;

/// 一代是什么构建。每个二进制启动时自报，宿主据此判断能不能用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildIdentity {
    pub version: String,
    /// 构建提交；不知道就是 `None`，不拿版本号顶替。
    pub build_commit: Option<String>,
    pub generation_protocol: u32,
    pub state_format: u32,
    pub pid: u32,
    pub exe: Option<PathBuf>,
}

impl BuildIdentity {
    pub fn current() -> Self {
        Self {
            version: crate::VERSION.to_string(),
            build_commit: crate::BUILD_COMMIT.map(str::to_string),
            generation_protocol: GENERATION_PROTOCOL,
            state_format: STATE_FORMAT,
            pid: std::process::id(),
            exe: std::env::current_exe().ok(),
        }
    }

    /// 这个宿主不能用它的原因；能用是 `None`。
    pub fn incompatibility(&self) -> Option<String> {
        if !(GENERATION_PROTOCOL_MIN..=GENERATION_PROTOCOL).contains(&self.generation_protocol) {
            return Some(format!(
                "执行代协议 {} 不在这个守护进程支持的 {GENERATION_PROTOCOL_MIN}–{GENERATION_PROTOCOL} 范围内",
                self.generation_protocol
            ));
        }
        if self.state_format != STATE_FORMAT {
            return Some(format!(
                "盘上状态格式 {} 和这个守护进程的 {STATE_FORMAT} 不一样",
                self.state_format
            ));
        }
        None
    }
}

static ROUTER: OnceLock<Arc<Router>> = OnceLock::new();

/// 宿主进程启动时调一次。之后本进程的工具调用都发给执行代，不再在本进程跑。
pub fn install(router: Arc<Router>) {
    let _ = ROUTER.set(router);
}

pub fn router() -> Option<Arc<Router>> {
    ROUTER.get().cloned()
}

/// `gld grant rm`、切 plan 模式这些操作要停的命令可能不在本进程，这里统一找到它们。
pub mod exec {
    use std::path::Path;

    use super::router;

    /// 停掉一个工作区里所有在跑的命令，返回条数。
    pub fn terminate_workspace_sessions(root: &Path) -> Result<usize, String> {
        match router() {
            Some(router) => router.terminate_workspace(root).map_err(|e| e.to_string()),
            None => Ok(crate::tools::workspace_runtime::runtime_for(root).terminate_all_sessions()),
        }
    }

    /// 撤销 grant 时停它起的命令和委派，返回（命令条数，任务个数）。
    pub fn terminate_grant(grant_id: &str, data_home: Option<&Path>) -> (usize, usize) {
        match router() {
            Some(router) => router.revoke_grant(grant_id),
            None => (
                crate::tools::workspace_runtime::terminate_grant_sessions(grant_id),
                data_home.map_or(0, |home| crate::agents::stop_for_grant(home, grant_id)),
            ),
        }
    }

    /// 宿主退出的第一步：先把命令和 Agent 收了，记成 `interrupted`。阻塞，放 `spawn_blocking` 里调。
    pub fn interrupt_everything() {
        match router() {
            Some(router) => router.interrupt_all(),
            None => {
                crate::tools::workspace_runtime::terminate_all_sessions_everywhere();
                crate::agents::interrupt_all(std::time::Duration::from_secs(15));
            }
        }
    }
}

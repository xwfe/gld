//! 异步运行时垫片。
//!
//! 工具内核（`tools::exec` / `tools::session`）是同步 API，但内部要驱动
//! tokio 子进程和管道。原先依赖 `tauri::async_runtime` 提供“任何线程都能
//! spawn / block_on”的能力，这里用一个全局 tokio 运行时复刻同样的语义：
//!
//! - 已经在 tokio 运行时里（有 `Handle`）→ 直接借用当前运行时；
//! - 不在运行时里（例如同步测试、命令行直接调用）→ 懒初始化一个全局运行时。
//!
//! 注意：`block_on` 不能在 tokio 的异步 worker 线程里调用，会 panic
//! （tokio 的硬性限制）。调用方必须先 `spawn_blocking` 再进来，MCP /
//! Actions 监听器已经这样做了。

use std::future::Future;
use std::sync::OnceLock;

use tokio::runtime::{Handle, Runtime};

pub use tokio::task::JoinHandle;

static GLOBAL: OnceLock<Runtime> = OnceLock::new();

fn global_runtime() -> &'static Runtime {
    GLOBAL.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("gld-async")
            .build()
            .expect("failed to build the global tokio runtime")
    })
}

/// 当前可用的运行时句柄：优先当前线程所在的运行时，否则退回全局运行时。
pub fn handle() -> Handle {
    Handle::try_current().unwrap_or_else(|_| global_runtime().handle().clone())
}

/// 在可用的运行时上 spawn 一个任务。
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    handle().spawn(future)
}

/// 同步等待一个 future 完成。
///
/// 从异步 worker 线程调用会 panic（tokio 限制）；同步入口（工具内核）
/// 总是运行在 `spawn_blocking` 线程或普通线程上，因此不会触发。
pub fn block_on<F: Future>(future: F) -> F::Output {
    match Handle::try_current() {
        Ok(current) => current.block_on(future),
        Err(_) => global_runtime().block_on(future),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_on_works_without_an_ambient_runtime() {
        assert_eq!(block_on(async { 41 + 1 }), 42);
    }

    #[test]
    fn spawn_works_without_an_ambient_runtime() {
        let task = spawn(async { "done" });
        assert_eq!(block_on(task).expect("join"), "done");
    }

    #[tokio::test]
    async fn spawn_reuses_the_ambient_runtime() {
        let task = spawn(async { 7 });
        assert_eq!(task.await.expect("join"), 7);
    }
}

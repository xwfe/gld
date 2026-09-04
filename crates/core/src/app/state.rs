use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use crate::data::DataStore;
use crate::error::{AppError, AppResult};
use crate::runtime::RuntimeSupervisor;
use crate::settings::AppSettings;
use crate::tools::ToolContext;
use crate::workspace::WorkspaceProfile;

/// 进程内的应用状态：持久化数据 + 正在运行的监听器 + 命令行工具调用的上下文。
///
/// 三把锁都是 `std::sync::Mutex`，临界区只做内存读写和一次同步落盘，
/// 不在持锁期间 `await`。异步的重启串行化由 [`App::restart_gate`] 负责。
pub struct App {
    data: Mutex<DataStore>,
    runtime: Mutex<RuntimeSupervisor>,
    /// `gld tool call` 用的工具上下文，按工作区缓存。
    ///
    /// 缓存是为了让连续两次命令行调用共享 exec 会话（先 `exec_command`
    /// 再 `read_output`）。改了工作区配置必须失效，否则会继续用旧的
    /// 工具集和命令白名单，见 [`App::invalidate_tool_context`]。
    tool_contexts: Mutex<HashMap<String, Arc<ToolContext>>>,
    /// 串行化 MCP / Actions 的 stop→start，避免密钥保存与表单保存同时拆同一个监听器。
    pub(super) restart_gate: tokio::sync::Mutex<()>,
    pub(super) startup_restore_attempted: AtomicBool,
}

impl App {
    /// 从磁盘加载数据并补齐共享密钥。
    pub fn load() -> AppResult<Self> {
        let mut store = DataStore::load()?;
        store.init_shared_secrets()?;
        Ok(Self {
            data: Mutex::new(store),
            runtime: Mutex::new(RuntimeSupervisor::default()),
            tool_contexts: Mutex::new(HashMap::new()),
            restart_gate: tokio::sync::Mutex::new(()),
            startup_restore_attempted: AtomicBool::new(false),
        })
    }

    pub(super) fn with_tool_contexts<R>(
        &self,
        f: impl FnOnce(&mut HashMap<String, Arc<ToolContext>>) -> AppResult<R>,
    ) -> AppResult<R> {
        let mut guard = self
            .tool_contexts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut guard)
    }

    /// 工作区配置变化后丢弃缓存的工具上下文，下次调用会按新配置重建。
    pub(super) fn invalidate_tool_context(&self, id: &str) {
        let _ = self.with_tool_contexts(|contexts| {
            contexts.remove(id);
            Ok(())
        });
    }

    pub(super) fn with_data<R>(
        &self,
        f: impl FnOnce(&mut DataStore) -> AppResult<R>,
    ) -> AppResult<R> {
        let mut guard = self
            .data
            .lock()
            .map_err(|_| AppError::Message("data store poisoned".into()))?;
        f(&mut guard)
    }

    pub(super) fn with_runtime<R>(
        &self,
        f: impl FnOnce(&mut RuntimeSupervisor) -> AppResult<R>,
    ) -> AppResult<R> {
        let mut guard = self
            .runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut guard)
    }

    /// 当前设置快照。
    pub fn settings(&self) -> AppResult<AppSettings> {
        self.with_data(|store| Ok(store.settings()))
    }

    /// 读-改-写一次设置。
    pub(super) fn update_settings(
        &self,
        mutate: impl FnOnce(&mut AppSettings) -> AppResult<()>,
    ) -> AppResult<()> {
        self.with_data(|store| {
            let mut settings = store.settings();
            mutate(&mut settings)?;
            store.update_settings(settings)
        })
    }

    pub(super) fn profile_by_id(&self, id: &str) -> AppResult<WorkspaceProfile> {
        self.with_data(|store| {
            store
                .get(id)
                .cloned()
                .ok_or_else(|| AppError::Message(format!("workspace not found: {id}")))
        })
    }

    pub(super) fn ensure_workspace_exists(&self, id: &str) -> AppResult<()> {
        self.profile_by_id(id).map(|_| ())
    }
}

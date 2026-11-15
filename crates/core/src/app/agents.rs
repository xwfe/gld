//! `gld agent`：在本机看、放行、拒绝、停委派任务（RFC-0009）。
//!
//! 起委派只有 AI 经 `agent_manage` 能做；这里提供操作员的记录访问、本地动作和根目录查询。
//! 守护进程将放行/停止送到持有任务的执行代，不能在宿主另起一份 LIVE 表。

use std::path::PathBuf;

use crate::agents::{self, store::AgentTask, AgentError, TaskDetail};
use crate::error::{AppError, AppResult};

use super::App;

impl App {
    pub fn list_agent_tasks(&self) -> AppResult<Vec<AgentTask>> {
        Ok(agents::list(&crate::home::data_home()?, None))
    }

    pub fn agent_task(&self, task_id: &str) -> AppResult<TaskDetail> {
        let (dir, task) = agents::get(&crate::home::data_home()?, task_id).map_err(convert)?;
        Ok(agents::detail(&dir, task))
    }

    /// 按任务记录里的工作区目录选择执行代，不读取报告或 diff。
    pub fn agent_task_root(&self, task_id: &str) -> AppResult<PathBuf> {
        let (_, task) = agents::get(&crate::home::data_home()?, task_id).map_err(convert)?;
        Ok(task.workspace_root.into())
    }

    /// 在本进程放行任务，可以同时调整 Agent、模型、推理强度。
    pub fn approve_agent_task_local(
        &self,
        task_id: &str,
        agent: Option<&str>,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> AppResult<AgentTask> {
        agents::approve_with(
            &crate::home::data_home()?,
            task_id,
            agents::ApprovalChange {
                agent,
                model,
                effort,
            },
        )
        .map_err(convert)
    }

    pub fn deny_agent_task(&self, task_id: &str) -> AppResult<AgentTask> {
        agents::deny(&crate::home::data_home()?, task_id).map_err(convert)
    }

    pub fn stop_agent_task_local(&self, task_id: &str) -> AppResult<AgentTask> {
        agents::stop(&crate::home::data_home()?, task_id).map_err(convert)
    }
}

fn convert(error: AgentError) -> AppError {
    AppError::Message(format!("{}（{}）", error.message, error.code))
}

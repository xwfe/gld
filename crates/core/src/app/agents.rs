//! `gld agent`：在本机看、放行、拒绝、停委派任务（RFC-0009）。
//!
//! 起委派只有 AI 经 `agent_manage` 能做；这里是操作员这一侧。放行和停止要在守护进程里做
//! （进程和"在跑的表"在那儿），列和看直接读盘。

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

    /// 放行，可以顺手换 Agent、模型、推理强度（都不给就照 AI 的提议）。
    pub fn approve_agent_task(
        &self,
        task_id: &str,
        agent: Option<&str>,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> AppResult<AgentTask> {
        // Agent 进程和它的 LIVE 记录要在这个工作区所属的执行代里，
        // 那一代的写入和命令拦截才看得见它（RFC-0009 第 5 节第 3 条）。
        if let Some(router) = crate::generation::router() {
            let (_, task) = agents::get(&crate::home::data_home()?, task_id).map_err(convert)?;
            let reply = router
                .approve_agent(
                    std::path::Path::new(&task.workspace_root),
                    task_id,
                    agent,
                    model,
                    effort,
                )
                .map_err(|error| AppError::Message(error.to_string()))?;
            return Ok(serde_json::from_value(reply)?);
        }
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

    pub fn stop_agent_task(&self, task_id: &str) -> AppResult<AgentTask> {
        if let Some(router) = crate::generation::router() {
            let reply = router
                .stop_agent(task_id)
                .map_err(|error| AppError::Message(error.to_string()))?;
            return Ok(serde_json::from_value(reply)?);
        }
        agents::stop(&crate::home::data_home()?, task_id).map_err(convert)
    }
}

fn convert(error: AgentError) -> AppError {
    AppError::Message(format!("{}（{}）", error.message, error.code))
}

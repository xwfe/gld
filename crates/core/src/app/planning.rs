use std::path::{Path, PathBuf};

use super::App;
use crate::error::{AppError, AppResult};
use crate::planning::{
    Goal, GoalStatus, Plan, PlanStatus, PlanningMode, PlanningService, PlanningState,
};

pub use gld_protocol::planning::PlanStepUpdate;

impl App {
    pub fn planning_state(&self, id: &str) -> AppResult<PlanningState> {
        self.planning_service(id)?.state()
    }

    pub async fn set_planning_mode_local(
        &self,
        id: &str,
        mode: PlanningMode,
    ) -> AppResult<PlanningState> {
        self.set_planning_mode_with_cleanup(id, mode, |path| {
            Ok(crate::tools::workspace_runtime::runtime_for(path).terminate_all_sessions())
        })
        .await
    }

    /// 先保存模式；进入 Plan 模式时，再清理仍在运行的命令会话。
    pub async fn set_planning_mode_with_cleanup(
        &self,
        id: &str,
        mode: PlanningMode,
        cleanup: impl FnOnce(&Path) -> Result<usize, String> + Send + 'static,
    ) -> AppResult<PlanningState> {
        let path = self.workspace_root(id)?;
        let state = PlanningService::new(&path).set_mode(mode)?;
        if mode == PlanningMode::Plan {
            let failed = |error: String| {
                AppError::Message(format!(
                    "已切到 plan 模式，但停止工作区里还在跑的命令时出错：{error}"
                ))
            };
            // 清理会同步等待进程退出，不能在 Tokio 异步线程上阻塞。
            tokio::task::spawn_blocking(move || cleanup(&path))
                .await
                .map_err(|error| failed(error.to_string()))?
                .map_err(failed)?;
        }
        Ok(state)
    }

    pub fn create_goal(
        &self,
        id: &str,
        title: &str,
        objective: &str,
        success_criteria: Vec<String>,
        constraints: Vec<String>,
    ) -> AppResult<Goal> {
        self.planning_service(id)?
            .create_goal(title, objective, success_criteria, constraints)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_goal(
        &self,
        id: &str,
        goal_id: &str,
        title: Option<String>,
        objective: Option<String>,
        status: Option<GoalStatus>,
        constraints: Option<Vec<String>>,
        completed_criteria_ids: Option<Vec<String>>,
        focus: Option<bool>,
    ) -> AppResult<Goal> {
        self.planning_service(id)?.update_goal(
            goal_id,
            title,
            objective,
            status,
            constraints,
            completed_criteria_ids,
            focus,
        )
    }

    pub fn create_plan(
        &self,
        id: &str,
        goal_id: Option<String>,
        title: &str,
        objective: &str,
        steps: Vec<String>,
    ) -> AppResult<Plan> {
        self.planning_service(id)?
            .create_plan(goal_id, title, objective, steps)
    }

    pub fn update_plan(
        &self,
        id: &str,
        plan_id: &str,
        status: Option<PlanStatus>,
        step_updates: Vec<PlanStepUpdate>,
        focus: Option<bool>,
    ) -> AppResult<Plan> {
        let updates = step_updates
            .into_iter()
            .map(|update| (update.step_id, update.status, update.notes))
            .collect();
        self.planning_service(id)?
            .update_plan(plan_id, status, updates, focus)
    }

    pub fn accept_goal_review(&self, id: &str, goal_id: &str) -> AppResult<Goal> {
        self.planning_service(id)?.accept_goal_review(goal_id)
    }

    pub fn reject_goal_review(
        &self,
        id: &str,
        goal_id: &str,
        feedback: Option<String>,
    ) -> AppResult<Goal> {
        self.planning_service(id)?
            .reject_goal_review(goal_id, feedback)
    }

    pub fn accept_plan_review(&self, id: &str, plan_id: &str) -> AppResult<Plan> {
        self.planning_service(id)?.accept_plan_review(plan_id)
    }

    pub fn reject_plan_review(
        &self,
        id: &str,
        plan_id: &str,
        feedback: Option<String>,
    ) -> AppResult<Plan> {
        self.planning_service(id)?
            .reject_plan_review(plan_id, feedback)
    }

    fn planning_service(&self, id: &str) -> AppResult<PlanningService> {
        Ok(PlanningService::new(&self.workspace_root(id)?))
    }

    pub(super) fn workspace_root(&self, id: &str) -> AppResult<PathBuf> {
        let profile = self.profile_by_id(id)?;
        PathBuf::from(&profile.path)
            .canonicalize()
            .map_err(|error| {
                AppError::Message(format!("工作区目录不可用：{}（{error}）", profile.path))
            })
    }
}

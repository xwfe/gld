use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::App;
use crate::error::{AppError, AppResult};
use crate::planning::{
    Goal, GoalStatus, Plan, PlanStatus, PlanStepStatus, PlanningMode, PlanningService,
    PlanningState,
};

/// 对计划中一个步骤的状态更新。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStepUpdate {
    pub step_id: String,
    pub status: PlanStepStatus,
    #[serde(default)]
    pub notes: Option<String>,
}

impl App {
    pub fn planning_state(&self, id: &str) -> AppResult<PlanningState> {
        self.planning_service(id)?.state()
    }

    /// 切换 Direct / Plan / Goal 模式；进入 Plan 模式时杀掉工作区里仍在运行的命令会话。
    pub fn set_planning_mode(&self, id: &str, mode: PlanningMode) -> AppResult<PlanningState> {
        let path = self.workspace_root(id)?;
        let state = PlanningService::new(&path).set_mode(mode)?;
        if mode == PlanningMode::Plan {
            crate::tools::session::kill_workspace_sessions(&path);
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

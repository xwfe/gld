//! IPC 与计划状态共用的枚举和步骤更新数据；账本与持久化归应用侧。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanningMode {
    #[default]
    Direct,
    Plan,
    Goal,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    #[default]
    Active,
    Paused,
    Completed,
    AwaitingAcceptance,
    Archived,
    Cancelled,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    #[default]
    Draft,
    Active,
    Paused,
    Completed,
    AwaitingAcceptance,
    Archived,
    Cancelled,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStepStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
    Blocked,
    Skipped,
}

/// 对计划中一个步骤的状态更新。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStepUpdate {
    pub step_id: String,
    pub status: PlanStepStatus,
    #[serde(default)]
    pub notes: Option<String>,
}

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityStatus {
    pub status: String,
    pub reason: String,
    pub recoverable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessStatus {
    pub schema_version: u32,
    pub workspace_id: String,
    pub task_id: Option<String>,
    pub task_state: Option<TaskStatus>,
    pub task_updated_at: Option<String>,
    pub writable: bool,
    pub reason: String,
    pub recoverable: bool,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub baseline_matches: Option<bool>,
    /// 指纹覆盖了工作区里的每个文件吗。`false` 时 `unreadable_paths` 列出没读到的
    /// （权限、遍历出错）——那些文件变了也看不出来。没算指纹时是 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_complete: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unreadable_paths: Vec<String>,
    /// Harness 数据目录里读不出来的任务文件（最多列 20 个）。找到了没结束的任务时它们
    /// 不挡路，只列在这里；没找到时 status 直接报 `STORE_CORRUPT`。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unreadable_task_files: Vec<String>,
    pub capabilities: HashMap<String, CapabilityStatus>,
    pub next_actions: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Active,
    Paused,
    Verifying,
    Failed,
    Completed,
    CompletedUnverified,
    RolledBack,
}

impl TaskStatus {
    /// 还没结束，占着这个工作区的任务位：此时不能 start 下一个任务。
    pub fn is_open(self) -> bool {
        matches!(
            self,
            Self::Active | Self::Paused | Self::Verifying | Self::Failed
        )
    }

    /// 能不能写文件、跑命令。和 `is_open` 分开是因为 Paused 占着位却不该放行写入：
    /// 以前两件事共用一个判断，暂停的任务照样能打补丁，"暂停"就只剩个名字（审查 D01）。
    /// Verifying 放行：验收要在这个状态下跑测试，之后的写入会让已有证据的指纹对不上，
    /// 不会被误收。
    pub fn accepts_writes(self) -> bool {
        matches!(self, Self::Active | Self::Verifying | Self::Failed)
    }

    /// 状态图本身允许的迁移。进 `Completed` 还要证据，那一关在
    /// `Harness::finish_task`，`Harness::transition` 不放行它。
    pub fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Active, Self::Paused | Self::Verifying | Self::Failed)
                | (Self::Active, Self::CompletedUnverified)
                | (Self::Paused, Self::Active)
                | (
                    Self::Verifying,
                    Self::Completed | Self::CompletedUnverified | Self::Failed
                )
                | (Self::Failed, Self::Active | Self::RolledBack)
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineEntry {
    pub path: String,
    pub exists: bool,
    pub is_binary: bool,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectBaseline {
    pub branch: Option<String>,
    pub head: Option<String>,
    pub worktree_fingerprint: String,
    pub entries: Vec<BaselineEntry>,
    /// 遍历或读取失败、因而不在 `entries` 里的路径。不进指纹：它们的内容本来就不知道。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unreadable: Vec<String>,
    pub captured_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSession {
    pub id: String,
    pub workspace_id: String,
    pub objective: String,
    pub status: TaskStatus,
    pub baseline: ProjectBaseline,
    pub expected_fingerprint: String,
    #[serde(default)]
    pub completed_steps: Vec<String>,
    #[serde(default)]
    pub pending_steps: Vec<String>,
    pub latest_change_id: Option<String>,
    pub latest_verification_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasonRecord {
    pub text: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChangeRecord {
    pub path: String,
    pub status: String,
    pub before_sha256: Option<String>,
    pub after_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessEvent {
    pub id: String,
    pub task_id: String,
    pub operation_id: String,
    pub kind: String,
    pub tool_name: Option<String>,
    pub input_summary: Value,
    pub result_summary: Value,
    pub reason: Option<ReasonRecord>,
    #[serde(default)]
    pub affected_files: Vec<FileChangeRecord>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationRecord {
    pub id: String,
    pub workspace_id: String,
    pub task_id: Option<String>,
    pub tool: String,
    pub kind: String,
    pub input_summary: Value,
    pub result_summary: Value,
    pub reason: Option<String>,
    #[serde(default)]
    pub affected_files: Vec<FileChangeRecord>,
    pub created_at: String,
}

/// 一条命令会话的状态：还在跑，或者怎么结束的。
///
/// exec_command / read_output / write_stdin / kill_session 的返回里都有这几格。
/// 台账和验收证据只认它，不认工具调用顶层的 `ok`——命令退出 7 时 `ok` 也是 true。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandOutcome {
    pub session_id: Option<String>,
    /// running / exited / timeout / killed / spawn_failed / server_restart ……
    pub status: String,
    pub exit_code: Option<i64>,
    pub command_ok: Option<bool>,
    /// 命令起来之后 gld 自己往工作区落过几次盘。不是 0，命令读到的就不是一份稳定的文件树。
    pub workspace_writes_since_start: Option<u64>,
}

impl CommandOutcome {
    pub fn is_running(&self) -> bool {
        self.status == "running"
    }

    pub fn passed(&self) -> bool {
        self.status == "exited" && self.exit_code == Some(0)
    }
}

/// 一条被接受的验收证据：哪条命令、在哪份内容上、跑出了什么。
///
/// `fingerprint` / `head` 是命令结束时的工作区；完成任务时要和当时的工作区一致，
/// 之后再改文件，这条证据就不算数了。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationRecord {
    pub id: String,
    pub task_id: String,
    pub command: String,
    pub category: String,
    pub exit_code: Option<i32>,
    pub passed: bool,
    pub change_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub fingerprint: String,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub head: Option<String>,
    /// 命令自己跑的时候改掉的文件（缓存、快照之类）。不拒收，但要说出来。
    #[serde(default)]
    pub changed_during_run: Vec<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeSet {
    pub id: String,
    pub task_id: String,
    pub objective: String,
    pub reason: ReasonRecord,
    #[serde(default)]
    pub files: Vec<FileChangeRecord>,
    #[serde(default)]
    pub command_ids: Vec<String>,
    #[serde(default)]
    pub verification_ids: Vec<String>,
    #[serde(default)]
    pub risks: Vec<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectFileState {
    pub path: String,
    pub status: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectState {
    pub schema_version: u32,
    pub workspace_id: String,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub clean: bool,
    pub files: Vec<ProjectFileState>,
    pub total_files: usize,
    pub truncated: bool,
    pub active_task_id: Option<String>,
    pub task: Option<TaskSession>,
    pub recent_events: usize,
    /// 这次扫描没读到的文件；它们的状态未知，不在 `files` 里。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unreadable_paths: Vec<String>,
}

/// 工作区在某一刻的位置：分支、HEAD、文件树指纹。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorktreePosition {
    pub branch: Option<String>,
    pub head: Option<String>,
    pub fingerprint: String,
}

/// `refresh_baseline` 的结果：任务上次记账的位置、现在的位置、中间变了哪些文件。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineReview {
    pub task_id: String,
    pub baseline_matches: bool,
    pub expected: WorktreePosition,
    pub current: WorktreePosition,
    /// `last_recorded_state`：和任务上次记账时的逐文件清单比；
    /// `task_start`：升级前开的任务没有那份清单，只能和任务开始时比，列出来的会多。
    pub compared_with: String,
    pub changes: Vec<ProjectFileState>,
    pub total_changes: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unreadable_paths: Vec<String>,
    /// 这次调用有没有把当前工作区接纳为新的基线。
    pub accepted: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceHarnessState {
    pub schema_version: u32,
    pub active_task_id: Option<String>,
    #[serde(default)]
    pub recent_task_ids: Vec<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HarnessIndex {
    pub schema_version: u32,
    #[serde(default)]
    pub workspaces: HashMap<String, WorkspaceHarnessState>,
}

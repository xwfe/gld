use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::model::{
    BaselineEntry, BaselineReview, CapabilityStatus, FileChangeRecord, HarnessEvent, HarnessStatus,
    OperationRecord, ProjectFileState, ProjectState, TaskSession, TaskStatus,
    WorkspaceHarnessState, WorktreePosition, SCHEMA_VERSION,
};
pub use super::scan::capture_baseline;
use super::scan::{changed_files, file_states, git_position, scan_worktree, timestamp};
use super::store::{HarnessError, HarnessResult, HarnessStore};

/// 状态、错误里最多列几个没读到的文件。完整名单在 `project_state` / `refresh_baseline`。
const UNREADABLE_SAMPLE: usize = 20;

/// 基线复核一次最多列几个改动文件；超过看 `total_changes`。
const REVIEW_FILES: usize = 200;

#[derive(Debug, Clone)]
pub struct Harness {
    pub(super) workspace_root: PathBuf,
    pub(super) workspace_id: String,
    pub(super) store: HarnessStore,
}

/// 一次记账（把当前工作区记成任务的预期状态）的结果。
pub struct ExpectedRefresh {
    pub position: WorktreePosition,
    /// 和上一次记账相比变了的文件。
    pub changed: Vec<String>,
}

/// 这个任务从开始到现在改了什么，以及算的时候工作区在哪。
pub struct TaskChanges {
    pub files: Vec<ProjectFileState>,
    pub position: WorktreePosition,
    pub unreadable: Vec<String>,
}

impl Harness {
    pub fn new(workspace_root: PathBuf, harness_root: PathBuf) -> HarnessResult<Self> {
        let workspace_root = workspace_root
            .canonicalize()
            .map_err(|e| HarnessError::new("WORKSPACE_UNAVAILABLE", e.to_string()))?;
        let workspace_id = workspace_id(&workspace_root);
        Ok(Self {
            workspace_root,
            workspace_id,
            store: HarnessStore::new(harness_root)?,
        })
    }

    pub fn default_root() -> HarnessResult<PathBuf> {
        crate::home::harness_root()
            .map_err(|error| HarnessError::new("STORE_UNAVAILABLE", error.to_string()))
    }

    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    pub fn store_root(&self) -> &Path {
        self.store.root()
    }

    pub fn start_task(&self, objective: &str) -> HarnessResult<TaskSession> {
        if objective.trim().is_empty() {
            return Err(HarnessError::new("INVALID_ARGUMENT", "任务目标不能为空"));
        }
        if let Some(task) = self.current_task()? {
            return Err(HarnessError::new(
                "TASK_ALREADY_ACTIVE",
                format!(
                    "工作区已有未结束的任务 {}（{:?}）；先 finish 它，或确认放弃验收后带 allow_unverified=true 收尾",
                    task.id, task.status
                ),
            ));
        }
        let baseline = capture_baseline(&self.workspace_root);
        let now = timestamp();
        let task = TaskSession {
            id: Uuid::new_v4().simple().to_string(),
            workspace_id: self.workspace_id.clone(),
            objective: objective.trim().to_string(),
            status: TaskStatus::Active,
            expected_fingerprint: baseline.worktree_fingerprint.clone(),
            baseline,
            completed_steps: Vec::new(),
            pending_steps: Vec::new(),
            latest_change_id: None,
            latest_verification_id: None,
            created_at: now.clone(),
            updated_at: now,
        };
        self.store.save_task(&task)?;
        self.save_workspace_state(Some(&task.id), &task.updated_at)?;
        self.record_event(
            &task.id,
            "task_started",
            None,
            json!({}),
            json!({"ok": true}),
        )?;
        Ok(task)
    }

    /// 占着这个工作区任务位的任务（还没结束的那个）。
    pub fn current_task(&self) -> HarnessResult<Option<TaskSession>> {
        Ok(self
            .store
            .list_tasks(&self.workspace_id)?
            .into_iter()
            .find(|task| task.status.is_open()))
    }

    /// 同 `current_task`，但先看工作区状态文件里记的 id，只读那一个任务文件。
    ///
    /// `current_task` 要把这个工作区所有任务文件读一遍（每个都带逐文件基线），
    /// 给 read_output 这种会被反复调的读工具用太重。
    pub fn active_task(&self) -> HarnessResult<Option<TaskSession>> {
        let Some(state) = self.store.load_workspace_state(&self.workspace_id)? else {
            return self.current_task();
        };
        let Some(task_id) = state.active_task_id else {
            return Ok(None);
        };
        match self.task(&task_id) {
            Ok(task) if task.status.is_open() => Ok(Some(task)),
            _ => self.current_task(),
        }
    }

    pub fn task(&self, task_id: &str) -> HarnessResult<TaskSession> {
        self.store.load_task(&self.workspace_id, task_id)
    }

    /// 暂停、恢复、标记失败这类不需要证据的迁移。进 `Completed` 只能走
    /// `finish_task` 并带上验收证据。
    pub fn transition(&self, task_id: &str, next: TaskStatus) -> HarnessResult<TaskSession> {
        if next == TaskStatus::Completed {
            return Err(HarnessError::new(
                "VERIFICATION_REQUIRED",
                "completed 只能经 finish 带验收证据（evidence_session_ids）进入",
            ));
        }
        let task = self.task(task_id)?;
        self.set_status(task, next)
    }

    /// 改状态、落盘、记事件。迁移合不合法在这里查；证据由调用方负责。
    pub(super) fn set_status(
        &self,
        mut task: TaskSession,
        next: TaskStatus,
    ) -> HarnessResult<TaskSession> {
        if !task.status.can_transition_to(next) {
            return Err(HarnessError::new(
                "INVALID_TASK_TRANSITION",
                format!("不允许从 {:?} 转换到 {:?}", task.status, next),
            ));
        }
        task.status = next;
        task.updated_at = timestamp();
        self.store.save_task(&task)?;
        if !task.status.is_open() {
            self.save_workspace_state(None, &task.updated_at)?;
        }
        self.record_event(
            &task.id,
            "task_status_changed",
            None,
            json!({"status": next}),
            json!({"ok": true}),
        )?;
        Ok(task)
    }

    pub fn update_steps(
        &self,
        task_id: &str,
        completed_steps: Option<Vec<String>>,
        pending_steps: Option<Vec<String>>,
    ) -> HarnessResult<TaskSession> {
        let mut task = self.task(task_id)?;
        if let Some(steps) = completed_steps {
            task.completed_steps = steps;
        }
        if let Some(steps) = pending_steps {
            task.pending_steps = steps;
        }
        task.updated_at = timestamp();
        self.store.save_task(&task)?;
        self.record_event(
            task_id,
            "task_updated",
            None,
            json!({
                "completed_steps": task.completed_steps,
                "pending_steps": task.pending_steps
            }),
            json!({"ok": true}),
        )?;
        Ok(task)
    }

    /// 写文件、跑命令之前的检查：任务在不在能写的状态，工作区是不是上次记账的样子。
    pub fn check_baseline(&self, task_id: &str) -> HarnessResult<()> {
        let task = self.task(task_id)?;
        if !task.status.accepts_writes() {
            return Err(HarnessError::new(
                "TASK_PAUSED",
                format!(
                    "任务 {} 处于 {:?}，不接受写入和执行；task_manage action=resume 之后再做",
                    task.id, task.status
                ),
            ));
        }
        let (branch, head) = git_position(&self.workspace_root);
        if branch != task.baseline.branch || head != task.baseline.head {
            return Err(HarnessError::new(
                "BASELINE_STALE",
                "Git 分支或 HEAD 已发生变化；用 task_manage action=refresh_baseline 查看并接纳",
            ));
        }
        if scan_worktree(&self.workspace_root).fingerprint() != task.expected_fingerprint {
            return Err(HarnessError::new(
                "FILE_CHANGED_EXTERNALLY",
                "工作区存在 Harness 未记录的外部文件变化；用 task_manage action=refresh_baseline 查看是哪些文件，确认归属后再接纳",
            ));
        }
        Ok(())
    }

    /// 把当前工作区记成任务的预期状态。gld 自己写完文件、跑完命令之后调。
    pub fn refresh_expected_state(&self, task_id: &str) -> HarnessResult<ExpectedRefresh> {
        let mut task = self.task(task_id)?;
        let previous = self.expected_entries(&task)?.0;
        let scan = scan_worktree(&self.workspace_root);
        let changed = changed_files(&previous, &scan.entries)
            .into_iter()
            .map(|file| file.path)
            .collect();
        let (branch, head) = git_position(&self.workspace_root);
        let position = WorktreePosition {
            branch,
            head,
            fingerprint: scan.fingerprint(),
        };
        task.expected_fingerprint = position.fingerprint.clone();
        task.updated_at = timestamp();
        self.store.save_task(&task)?;
        self.store
            .save_expected_entries(&self.workspace_id, task_id, &scan.entries)?;
        Ok(ExpectedRefresh { position, changed })
    }

    /// 任务此刻记账的位置。写前检查通过之后，工作区就是这个样子。
    pub fn expected_position(&self, task: &TaskSession) -> WorktreePosition {
        WorktreePosition {
            branch: task.baseline.branch.clone(),
            head: task.baseline.head.clone(),
            fingerprint: task.expected_fingerprint.clone(),
        }
    }

    /// 任务上次记账时的逐文件清单，以及它是哪一份。
    pub(super) fn expected_entries(
        &self,
        task: &TaskSession,
    ) -> HarnessResult<(Vec<BaselineEntry>, &'static str)> {
        Ok(
            match self
                .store
                .load_expected_entries(&self.workspace_id, &task.id)?
            {
                Some(entries) => (entries, "last_recorded_state"),
                None => (task.baseline.entries.clone(), "task_start"),
            },
        )
    }

    /// 外部改动之后的恢复入口（审查 D02）。
    ///
    /// 不带 `accept_fingerprint`：只看，列出从上次记账到现在变了哪些文件，什么都不改。
    /// 带上：确认看过的就是现在这份（指纹一致），写明 `reason`，把当前工作区接纳为
    /// 新基线。两步分开是为了不静默吞下外部改动——看过之后工作区又变了，接纳会被拒，
    /// 得重新看。
    pub fn refresh_baseline(
        &self,
        task_id: &str,
        accept_fingerprint: Option<&str>,
        reason: Option<&str>,
    ) -> HarnessResult<BaselineReview> {
        let mut task = self.task(task_id)?;
        if !task.status.is_open() {
            return Err(HarnessError::new(
                "TASK_STATE_REQUIRED",
                format!(
                    "任务 {} 已经结束（{:?}），没有基线可刷新",
                    task.id, task.status
                ),
            ));
        }
        let (previous, compared_with) = self.expected_entries(&task)?;
        let scan = scan_worktree(&self.workspace_root);
        let (branch, head) = git_position(&self.workspace_root);
        let changes = changed_files(&previous, &scan.entries);
        let expected = self.expected_position(&task);
        let current = WorktreePosition {
            branch,
            head,
            fingerprint: scan.fingerprint(),
        };
        let mut review = BaselineReview {
            task_id: task.id.clone(),
            baseline_matches: expected == current,
            total_changes: changes.len(),
            changes: changes.into_iter().take(REVIEW_FILES).collect(),
            expected,
            current,
            compared_with: compared_with.into(),
            unreadable_paths: scan.unreadable.clone(),
            accepted: false,
        };
        let Some(accept_fingerprint) = accept_fingerprint else {
            return Ok(review);
        };
        let reason = reason.map(str::trim).filter(|reason| !reason.is_empty());
        let Some(reason) = reason else {
            return Err(HarnessError::new(
                "INVALID_ARGUMENT",
                "接纳新基线必须写 reason：这些改动是谁做的、为什么可以算进任务",
            ));
        };
        if accept_fingerprint != review.current.fingerprint {
            return Err(HarnessError::new(
                "BASELINE_CHANGED_SINCE_REVIEW",
                "你看过之后工作区又变了；先不带 accept_fingerprint 再调一次，看清新的改动再接纳",
            ));
        }
        task.expected_fingerprint = review.current.fingerprint.clone();
        task.baseline.branch = review.current.branch.clone();
        task.baseline.head = review.current.head.clone();
        task.updated_at = timestamp();
        self.store.save_task(&task)?;
        self.store
            .save_expected_entries(&self.workspace_id, &task.id, &scan.entries)?;
        self.record_event(
            &task.id,
            "baseline_refreshed",
            None,
            json!({"reason": reason, "compared_with": review.compared_with}),
            json!({
                "ok": true,
                "before": review.expected,
                "after": review.current,
                "changes": review.changes,
                "total_changes": review.total_changes,
                "unreadable_paths": review.unreadable_paths
            }),
        )?;
        review.baseline_matches = true;
        review.accepted = true;
        Ok(review)
    }

    pub fn record_event(
        &self,
        task_id: &str,
        kind: &str,
        tool_name: Option<&str>,
        input_summary: serde_json::Value,
        result_summary: serde_json::Value,
    ) -> HarnessResult<HarnessEvent> {
        let event = HarnessEvent {
            id: Uuid::new_v4().simple().to_string(),
            task_id: task_id.to_string(),
            operation_id: Uuid::new_v4().simple().to_string(),
            kind: kind.to_string(),
            tool_name: tool_name.map(str::to_string),
            input_summary: json!({"workspace_id": self.workspace_id, "payload": input_summary}),
            result_summary,
            reason: None,
            affected_files: Vec::<FileChangeRecord>::new(),
            created_at: timestamp(),
        };
        self.store
            .append_event_for_workspace(&self.workspace_id, &event)?;
        Ok(event)
    }

    pub fn list_events(
        &self,
        task_id: &str,
        offset: usize,
        limit: usize,
    ) -> HarnessResult<Vec<HarnessEvent>> {
        self.store
            .list_events(&self.workspace_id, task_id, offset, limit)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_operation(
        &self,
        operation_id: Option<&str>,
        task_id: Option<&str>,
        tool: &str,
        kind: &str,
        input_summary: serde_json::Value,
        result_summary: serde_json::Value,
    ) -> HarnessResult<OperationRecord> {
        let reason = input_summary
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let operation = OperationRecord {
            id: operation_id
                .map(str::to_string)
                .unwrap_or_else(|| Uuid::new_v4().simple().to_string()),
            workspace_id: self.workspace_id.clone(),
            task_id: task_id.map(str::to_string),
            tool: tool.to_string(),
            kind: kind.to_string(),
            input_summary,
            result_summary,
            reason,
            affected_files: Vec::new(),
            created_at: timestamp(),
        };
        self.store
            .append_operation(&self.workspace_id, &operation)?;
        Ok(operation)
    }

    pub fn list_operations(
        &self,
        offset: usize,
        limit: usize,
    ) -> HarnessResult<Vec<OperationRecord>> {
        self.store
            .list_operations(&self.workspace_id, offset, limit)
    }

    pub fn project_state(&self, max_files: usize) -> HarnessResult<ProjectState> {
        let current = capture_baseline(&self.workspace_root);
        let task = self.current_task()?;
        let files = file_states(
            task.as_ref().map(|t| t.baseline.entries.as_slice()),
            &current.entries,
        );
        let total_files = files.len();
        // clean 要在截断之前判：改动排在 max_files 之后也得算不干净。
        let clean = files.iter().all(|f| f.status == "unchanged");
        let truncated = files.len() > max_files.max(1);
        let files = files.into_iter().take(max_files.max(1)).collect::<Vec<_>>();
        let active_task_id = task.as_ref().map(|t| t.id.clone());
        let recent_events = task
            .as_ref()
            .and_then(|t| self.list_events(&t.id, 0, 100).ok())
            .map(|events| events.len())
            .unwrap_or(0);
        Ok(ProjectState {
            schema_version: SCHEMA_VERSION,
            workspace_id: self.workspace_id.clone(),
            branch: current.branch,
            head: current.head,
            clean,
            files,
            total_files,
            truncated,
            active_task_id,
            task,
            recent_events,
            unreadable_paths: current.unreadable,
        })
    }

    /// 这个任务开始以来改过的文件，全量、按路径排序。只拿它自己的基线比，不管
    /// 它现在是不是活动任务——finish_task 算摘要时任务已经关了，拿活动任务的
    /// 基线就是拿空基线，每个文件都成了 added。
    pub fn task_changes(&self, task: &TaskSession) -> TaskChanges {
        let scan = scan_worktree(&self.workspace_root);
        let (branch, head) = git_position(&self.workspace_root);
        TaskChanges {
            files: changed_files(&task.baseline.entries, &scan.entries),
            position: WorktreePosition {
                branch,
                head,
                fingerprint: scan.fingerprint(),
            },
            unreadable: scan.unreadable,
        }
    }

    pub fn status(&self) -> HarnessResult<HarnessStatus> {
        // 先比分支和 HEAD，对得上才扫整棵树算指纹：扫描要把每个文件读一遍算 SHA-256，
        // 而 status 挂在每一次失败的工具调用后面——以前工作区里有个 511 MB 的文件，
        // 读一个不存在的路径就要多等 2 秒。没有任务时指纹根本没人用。
        let (branch, head) = git_position(&self.workspace_root);
        let task = self.current_task()?;
        let mut baseline_complete = None;
        let mut unreadable_paths = Vec::new();
        let (task_id, task_state, task_updated_at, writable, baseline_matches, reason) = match task
            .as_ref()
        {
            Some(task) => {
                let position_matches = task.baseline.branch == branch && task.baseline.head == head;
                let matches = position_matches && {
                    let scan = scan_worktree(&self.workspace_root);
                    baseline_complete = Some(scan.is_complete());
                    unreadable_paths = scan
                        .unreadable
                        .iter()
                        .take(UNREADABLE_SAMPLE)
                        .cloned()
                        .collect();
                    task.expected_fingerprint == scan.fingerprint()
                };
                let accepts = task.status.accepts_writes();
                let reason = if !matches {
                    "工作区基线已变化，写入和执行已暂停"
                } else if !accepts {
                    "任务已暂停，resume 之后才能写入和执行"
                } else if task.status == TaskStatus::Verifying {
                    "任务等待验收：带 evidence_session_ids 调 finish 完成"
                } else {
                    "任务可继续执行"
                };
                (
                    Some(task.id.clone()),
                    Some(task.status),
                    Some(task.updated_at.clone()),
                    matches && accepts,
                    Some(matches),
                    reason.to_string(),
                )
            }
            None => (
                None,
                None,
                None,
                true,
                None,
                "当前没有活动任务，工作区采用无任务模式；修改不会进入任务事件流".to_string(),
            ),
        };
        let denied_reason = if baseline_matches == Some(false) {
            "工作区和任务上次记账时不一致；先查看并接纳（refresh_baseline）"
        } else {
            "任务已暂停；resume 之后才能写入"
        };

        let mut capabilities = HashMap::new();
        capabilities.insert(
            "read".into(),
            CapabilityStatus {
                status: "available".into(),
                reason: "工作区读取不依赖活动任务".into(),
                recoverable: true,
            },
        );
        for (name, no_task_reason) in [
            (
                "write",
                "无任务模式允许直接修改，建议需要长期追踪时调用 start_task",
            ),
            (
                "exec",
                "无任务模式允许直接执行，建议需要长期追踪时调用 start_task",
            ),
        ] {
            capabilities.insert(
                name.into(),
                CapabilityStatus {
                    status: if writable { "available" } else { "denied" }.into(),
                    reason: if !writable {
                        denied_reason
                    } else if task_id.is_some() {
                        "活动任务和工作区基线有效"
                    } else {
                        no_task_reason
                    }
                    .into(),
                    recoverable: true,
                },
            );
        }
        capabilities.insert(
            "git".into(),
            CapabilityStatus {
                status: if branch.is_some() && head.is_some() {
                    "available"
                } else {
                    "degraded"
                }
                .into(),
                reason: if branch.is_some() && head.is_some() {
                    "已读取当前分支和 HEAD"
                } else {
                    "当前工作区不是可读取 Git 状态的仓库"
                }
                .into(),
                recoverable: true,
            },
        );
        capabilities.insert(
            "network".into(),
            CapabilityStatus {
                status: "managed_by_policy".into(),
                reason: "网络权限由工具策略控制，不由 Harness 任务状态决定".into(),
                recoverable: true,
            },
        );

        let mut next_actions = Vec::new();
        if task_id.is_none() {
            next_actions.push("start_task".into());
        } else if baseline_matches == Some(false) {
            next_actions.push("refresh_baseline".into());
            next_actions.push("git_diff".into());
        } else if !writable {
            next_actions.push("resume_task".into());
        } else if task_state == Some(TaskStatus::Verifying) {
            next_actions.push("finish_task".into());
        }
        next_actions.push("read_file".into());
        next_actions.push("git_status".into());

        Ok(HarnessStatus {
            schema_version: SCHEMA_VERSION,
            workspace_id: self.workspace_id.clone(),
            task_id,
            task_state,
            task_updated_at,
            writable,
            reason,
            recoverable: true,
            branch,
            head,
            baseline_matches,
            baseline_complete,
            unreadable_paths,
            capabilities,
            next_actions,
        })
    }

    fn save_workspace_state(
        &self,
        active_task_id: Option<&str>,
        updated_at: &str,
    ) -> HarnessResult<()> {
        self.store.save_workspace_state(
            &self.workspace_id,
            &WorkspaceHarnessState {
                schema_version: SCHEMA_VERSION,
                active_task_id: active_task_id.map(str::to_string),
                recent_task_ids: self
                    .store
                    .list_tasks(&self.workspace_id)?
                    .into_iter()
                    .take(20)
                    .map(|t| t.id)
                    .collect(),
                updated_at: updated_at.to_string(),
            },
        )
    }
}

fn workspace_id(root: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(root.to_string_lossy().as_bytes());
    format!("{:x}", hasher.finalize())[..32].to_string()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use tempfile::tempdir;

    fn harness(workspace: &Path, root: &Path) -> Harness {
        Harness::new(workspace.to_path_buf(), root.to_path_buf()).expect("harness")
    }

    #[test]
    fn status_keeps_read_available_without_task() {
        let workspace = tempdir().expect("workspace");
        let harness_root = tempdir().expect("harness");
        fs::write(workspace.path().join("main.rs"), "fn main() {}\n").expect("file");
        let harness = harness(workspace.path(), harness_root.path());

        let status = harness.status().expect("status");
        assert!(status.writable);
        assert_eq!(status.capabilities["read"].status, "available");
        assert_eq!(status.capabilities["write"].status, "available");
        assert!(status.next_actions.contains(&"start_task".to_string()));
    }

    #[test]
    fn starting_task_does_not_create_workspace_copies() {
        let workspace = tempdir().expect("workspace");
        let harness_root = tempdir().expect("harness");
        fs::write(workspace.path().join("main.rs"), "fn main() {}\n").expect("file");
        let harness = harness(workspace.path(), harness_root.path());

        harness.start_task("测试任务").expect("start task");
        assert!(!harness
            .store_root()
            .join("workspaces")
            .join(harness.workspace_id())
            .join("snapshots")
            .exists());
    }

    /// 暂停占着任务位，但不放行写入：以前暂停的任务照样能打补丁、跑命令。
    #[test]
    fn a_paused_task_refuses_writes_until_resumed() {
        let workspace = tempdir().expect("workspace");
        let harness_root = tempdir().expect("harness");
        fs::write(workspace.path().join("main.rs"), "fn main() {}\n").expect("file");
        let harness = harness(workspace.path(), harness_root.path());
        let task = harness.start_task("暂停").expect("start");

        harness
            .transition(&task.id, TaskStatus::Paused)
            .expect("pause");
        let status = harness.status().expect("status");
        assert!(!status.writable, "{status:?}");
        assert_eq!(status.capabilities["write"].status, "denied");
        assert!(status.next_actions.contains(&"resume_task".to_string()));
        let error = harness.check_baseline(&task.id).expect_err("暂停时不能写");
        assert_eq!(error.code(), "TASK_PAUSED");
        assert!(
            harness.start_task("另一个").is_err(),
            "暂停的任务仍然占着任务位"
        );

        harness
            .transition(&task.id, TaskStatus::Active)
            .expect("resume");
        harness.check_baseline(&task.id).expect("恢复之后可以写");
    }

    #[test]
    fn transition_never_reaches_completed_without_evidence() {
        let workspace = tempdir().expect("workspace");
        let harness_root = tempdir().expect("harness");
        let harness = harness(workspace.path(), harness_root.path());
        let task = harness.start_task("验收").expect("start");
        harness
            .transition(&task.id, TaskStatus::Verifying)
            .expect("verifying");
        let error = harness
            .transition(&task.id, TaskStatus::Completed)
            .expect_err("没有证据不能直接 completed");
        assert_eq!(error.code(), "VERIFICATION_REQUIRED");
    }

    /// 基线恢复：先看（列出外部改动、不改任何东西），再按看到的指纹接纳，并写明原因。
    #[test]
    fn refresh_baseline_lists_external_changes_then_accepts_what_was_reviewed() {
        let workspace = tempdir().expect("workspace");
        let harness_root = tempdir().expect("harness");
        fs::write(workspace.path().join("a.txt"), "a\n").expect("file");
        fs::write(workspace.path().join("b.txt"), "b\n").expect("file");
        let harness = harness(workspace.path(), harness_root.path());
        let task = harness.start_task("恢复").expect("start");

        // gld 自己的写入记了账，之后的外部改动只该列出 b.txt。
        fs::write(workspace.path().join("a.txt"), "a2\n").expect("file");
        harness.refresh_expected_state(&task.id).expect("refresh");
        fs::write(workspace.path().join("b.txt"), "外部\n").expect("external");

        let review = harness
            .refresh_baseline(&task.id, None, None)
            .expect("review");
        assert!(!review.baseline_matches);
        assert!(!review.accepted);
        assert_eq!(review.compared_with, "last_recorded_state");
        let changed: Vec<_> = review
            .changes
            .iter()
            .map(|f| (f.path.as_str(), f.status.as_str()))
            .collect();
        assert_eq!(changed, [("b.txt", "modified")]);
        assert_eq!(
            harness
                .check_baseline(&task.id)
                .expect_err("只看不接纳")
                .code(),
            "FILE_CHANGED_EXTERNALLY"
        );

        let missing_reason = harness
            .refresh_baseline(&task.id, Some(&review.current.fingerprint), Some("  "))
            .expect_err("没有 reason 不接纳");
        assert_eq!(missing_reason.code(), "INVALID_ARGUMENT");

        fs::write(workspace.path().join("b.txt"), "又改了\n").expect("external again");
        let moved = harness
            .refresh_baseline(
                &task.id,
                Some(&review.current.fingerprint),
                Some("用户手改"),
            )
            .expect_err("看过之后又变了");
        assert_eq!(moved.code(), "BASELINE_CHANGED_SINCE_REVIEW");

        let review = harness
            .refresh_baseline(&task.id, None, None)
            .expect("review again");
        let accepted = harness
            .refresh_baseline(
                &task.id,
                Some(&review.current.fingerprint),
                Some("用户手改 b.txt"),
            )
            .expect("accept");
        assert!(accepted.accepted && accepted.baseline_matches);
        harness.check_baseline(&task.id).expect("接纳之后可以写");
        let events = harness.list_events(&task.id, 0, 100).expect("events");
        let refreshed = events
            .iter()
            .find(|event| event.kind == "baseline_refreshed")
            .expect("接纳要留事件");
        assert_eq!(
            refreshed.input_summary["payload"]["reason"],
            "用户手改 b.txt"
        );
    }
}

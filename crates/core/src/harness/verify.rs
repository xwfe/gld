//! 任务验收：拿命令的真实终态当证据，决定任务能不能进 completed（审查 D01）。
//!
//! 证据从哪来：任务期间用 exec_command 起的命令，dispatch 拿到结果时把命令终态、
//! 起跑时和结束时的工作区写进任务事件；转了后台的，之后 read_output /
//! write_stdin / kill_session 第一次看到它结束时再补一条（[`Harness::observe_command`]）。
//! 验收只认这些事件，不认调用方自己说的"测试过了"。
//!
//! 能证明什么、不能证明什么：能证明"这条命令在现在这份文件上跑过、退出 0"；
//! 不能证明这条命令测的东西有意义——`true` 也会退出 0。记录里留着命令原文，
//! 让看的人判断。

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::model::{
    CommandOutcome, HarnessEvent, TaskSession, TaskStatus, VerificationRecord, WorktreePosition,
};
use super::scan::{changed_files, git_position, scan_worktree, timestamp};
use super::state::Harness;
use super::store::{HarnessError, HarnessResult};

/// 事件里记的一条命令证据。放在事件 `result_summary.evidence` 下。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandEvidence {
    pub command: CommandOutcome,
    /// 命令原文（已脱敏）。只有起命令的那条事件有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_text: Option<String>,
    /// 命令起来时的工作区。写前检查保证它就是任务当时记账的样子。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<WorktreePosition>,
    /// 看到命令终态时的工作区；还在跑时没有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<WorktreePosition>,
    /// 从起跑到看到终态之间变了的文件。命令自己写的缓存、快照会出现在这里。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_during_run: Vec<String>,
}

/// 一条被拒收的证据，以及为什么。
#[derive(Debug, Clone, Serialize)]
pub struct EvidenceProblem {
    pub session_id: String,
    pub code: &'static str,
    pub message: String,
}

/// 现在就能拿来验收的命令：跑完了、退出 0、之后工作区没变。
#[derive(Debug, Clone, Serialize)]
pub struct EvidenceCandidate {
    pub session_id: String,
    pub command: Option<String>,
}

pub struct FinishResult {
    pub task: TaskSession,
    pub verification: Vec<VerificationRecord>,
    /// 非空时任务状态没动。
    pub rejected: Vec<EvidenceProblem>,
    pub candidates: Vec<EvidenceCandidate>,
    pub warnings: Vec<String>,
}

/// 候选最多列几条。
const CANDIDATES: usize = 10;

impl Harness {
    /// 收尾。三条路：
    ///
    /// - 带 `evidence_session_ids`：逐条核对，全过才进 `Completed`；有一条不过就整个
    ///   拒收，任务状态不动，`rejected` 里说清每条为什么。
    /// - `allow_unverified`：明确放弃正式验收，进 `CompletedUnverified`。
    /// - 都不带：进 `Verifying`（已在 Verifying 就留着），`candidates` 列出现在能用的证据。
    ///   以前只有这一条路，Verifying 之后除了 allow_unverified 没有出口（审查 D01）。
    pub fn finish_task(
        &self,
        task_id: &str,
        evidence_session_ids: &[String],
        allow_unverified: bool,
    ) -> HarnessResult<FinishResult> {
        let _lock = self.store.lock(&self.workspace_id)?;
        let task = self.task(task_id)?;
        if !matches!(task.status, TaskStatus::Active | TaskStatus::Verifying) {
            return Err(HarnessError::new(
                "INVALID_TASK_TRANSITION",
                format!(
                    "{:?} 的任务不能 finish：Paused / Failed 先 resume",
                    task.status
                ),
            ));
        }
        if allow_unverified && !evidence_session_ids.is_empty() {
            return Err(HarnessError::new(
                "INVALID_ARGUMENT",
                "evidence_session_ids 和 allow_unverified 只能给一个",
            ));
        }
        if allow_unverified {
            let task = self.set_status(task, TaskStatus::CompletedUnverified)?;
            return Ok(FinishResult {
                task,
                verification: Vec::new(),
                rejected: Vec::new(),
                candidates: Vec::new(),
                warnings: Vec::new(),
            });
        }

        let log = self.list_events(&task.id, 0, usize::MAX)?;
        let unreadable_lines = log.unreadable.len();
        let events = log.into_items();
        let scan = scan_worktree(&self.workspace_root);
        let (branch, head) = git_position(&self.workspace_root);
        let current = WorktreePosition {
            branch,
            head,
            fingerprint: scan.fingerprint(),
        };
        if evidence_session_ids.is_empty() {
            let candidates = candidates(&events, &current);
            let task = if task.status == TaskStatus::Active {
                self.set_status(task, TaskStatus::Verifying)?
            } else {
                task
            };
            return Ok(FinishResult {
                task,
                verification: Vec::new(),
                rejected: Vec::new(),
                candidates,
                warnings: Vec::new(),
            });
        }

        if current != self.expected_position(&task) {
            return Err(HarnessError::new(
                "BASELINE_STALE",
                "工作区和任务上次记账时不一致，有没认领的改动；先 task_manage action=refresh_baseline 看清并接纳，再验收",
            ));
        }

        let mut seen = HashSet::new();
        let mut records = Vec::new();
        let mut rejected = Vec::new();
        for session_id in evidence_session_ids {
            if !seen.insert(session_id.as_str()) {
                continue;
            }
            match accept_evidence(&task, &events, session_id, &current) {
                Ok(record) => records.push(record),
                Err(problem) => rejected.push(problem),
            }
        }
        if !rejected.is_empty() {
            return Ok(FinishResult {
                candidates: candidates(&events, &current),
                task,
                verification: Vec::new(),
                rejected,
                warnings: Vec::new(),
            });
        }

        let mut warnings = Vec::new();
        for record in &records {
            self.record_event(
                &task.id,
                "verification_recorded",
                None,
                json!({"session_id": record.session_id}),
                json!({"ok": true, "verification": record}),
            )?;
            if !record.changed_during_run.is_empty() {
                warnings.push(format!(
                    "命令 {} 运行期间改了 {} 个文件（{}）：它测的是改之前的内容；如果改的不只是缓存，重跑一次再验收",
                    record.command,
                    record.changed_during_run.len(),
                    record.changed_during_run.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
                ));
            }
        }
        if !scan.is_complete() {
            warnings.push(format!(
                "有 {} 个文件没读到（{}），证据不覆盖它们",
                scan.unreadable.len(),
                scan.unreadable
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if unreadable_lines > 0 {
            // 坏行只会让证据变少（找不到、看不到终态都是拒收），不会让不该过的过；
            // 但看记录的人得知道记录不全。
            warnings.push(format!(
                "任务事件里有 {unreadable_lines} 行读不出来，那几行记的是什么已经不知道；行号见 task_manage action=events 的 unreadable_lines"
            ));
        }
        let mut task = if task.status == TaskStatus::Active {
            self.set_status(task, TaskStatus::Verifying)?
        } else {
            task
        };
        task.latest_verification_id = records.last().map(|record| record.id.clone());
        let task = self.set_status(task, TaskStatus::Completed)?;
        Ok(FinishResult {
            task,
            verification: records,
            rejected: Vec::new(),
            candidates: Vec::new(),
            warnings,
        })
    }

    /// 后续调用第一次看到一条后台命令结束：把终态和当时的工作区记进任务事件。
    ///
    /// 只管当前任务期间用 exec_command 起的会话；同一条会话的终态只记一次。
    /// 返回有没有记。
    pub fn observe_command(
        &self,
        tool: &str,
        command: &CommandOutcome,
        state: &str,
    ) -> HarnessResult<bool> {
        let Some(session_id) = command.session_id.as_deref() else {
            return Ok(false);
        };
        if command.is_running() {
            return Ok(false);
        }
        let Some(task) = self.active_task()? else {
            return Ok(false);
        };
        let events = self.list_events(&task.id, 0, usize::MAX)?.into_items();
        match find_evidence(&events, session_id) {
            Some(known) if known.end.is_none() => {}
            _ => return Ok(false),
        }
        let scan = scan_worktree(&self.workspace_root);
        let (branch, head) = git_position(&self.workspace_root);
        let (expected, _) = self.expected_entries(&task)?;
        let evidence = CommandEvidence {
            command: command.clone(),
            command_text: None,
            start: None,
            end: Some(WorktreePosition {
                branch,
                head,
                fingerprint: scan.fingerprint(),
            }),
            changed_during_run: changed_files(&expected, &scan.entries)
                .into_iter()
                .map(|file| file.path)
                .collect(),
        };
        self.record_event(
            &task.id,
            "command_observed",
            Some(tool),
            json!({"session_id": session_id}),
            json!({"ok": true, "tool": tool, "state": state, "evidence": evidence}),
        )?;
        Ok(true)
    }
}

/// 任务事件里被接受的验收记录，按时间先后。
pub fn verification_records(events: &[HarnessEvent]) -> Vec<VerificationRecord> {
    events
        .iter()
        .filter(|event| event.kind == "verification_recorded")
        .filter_map(|event| {
            serde_json::from_value(event.result_summary.get("verification")?.clone()).ok()
        })
        .collect()
}

/// 一条会话在这个任务里留下的证据，合并成最新的样子。
///
/// 起点必须是这个任务里 exec_command 的那条事件：别的任务、没开任务时起的命令、
/// 手写的 session_id 都找不到。
fn find_evidence(events: &[HarnessEvent], session_id: &str) -> Option<CommandEvidence> {
    let mut found: Option<CommandEvidence> = None;
    for event in events {
        let Some(evidence) = event
            .result_summary
            .get("evidence")
            .and_then(|value| serde_json::from_value::<CommandEvidence>(value.clone()).ok())
        else {
            continue;
        };
        if evidence.command.session_id.as_deref() != Some(session_id) {
            continue;
        }
        let started_here = event.kind == "operation_finished"
            && event.tool_name.as_deref() == Some("exec_command");
        match found.as_mut() {
            None if started_here => found = Some(evidence),
            None => {}
            Some(known) => {
                known.command = evidence.command;
                if evidence.end.is_some() {
                    known.end = evidence.end;
                    known.changed_during_run = evidence.changed_during_run;
                }
            }
        }
    }
    found
}

fn accept_evidence(
    task: &TaskSession,
    events: &[HarnessEvent],
    session_id: &str,
    current: &WorktreePosition,
) -> Result<VerificationRecord, EvidenceProblem> {
    let problem = |code, message: String| EvidenceProblem {
        session_id: session_id.to_string(),
        code,
        message,
    };
    let Some(evidence) = find_evidence(events, session_id) else {
        return Err(problem(
            "EVIDENCE_NOT_FOUND",
            "不是这个任务期间用 exec_command 起的命令（别的任务、没开任务时起的，或 id 写错了）"
                .into(),
        ));
    };
    let command = &evidence.command;
    if command.is_running() {
        return Err(problem(
            "EVIDENCE_NOT_FINISHED",
            "命令还没结束，或结束之后还没人读到；先 read_output 读到它结束".into(),
        ));
    }
    if !command.passed() {
        let how = match (command.status.as_str(), command.exit_code) {
            ("exited", Some(code)) => format!("退出码 {code}"),
            (status, _) => status.to_string(),
        };
        return Err(problem(
            "EVIDENCE_FAILED",
            format!("命令没有通过（{how}），失败、超时、被杀的命令不能当验收证据"),
        ));
    }
    match command.workspace_writes_since_start {
        Some(0) => {}
        Some(writes) => {
            return Err(problem(
                "EVIDENCE_STALE",
                format!("命令运行期间 gld 往工作区写过 {writes} 次，它测到的不是一份稳定的文件；改完之后重跑"),
            ))
        }
        None => {
            return Err(problem(
                "EVIDENCE_STALE",
                "没有记录命令运行期间工作区有没有被写过".into(),
            ))
        }
    }
    let Some(end) = evidence.end.as_ref() else {
        return Err(problem(
            "EVIDENCE_STALE",
            "没有记录命令结束时的工作区".into(),
        ));
    };
    if end != current {
        return Err(problem(
            "EVIDENCE_STALE",
            "命令结束之后工作区又变了，它测的不是现在的内容；重跑一次".into(),
        ));
    }
    Ok(VerificationRecord {
        id: Uuid::new_v4().simple().to_string(),
        task_id: task.id.clone(),
        command: evidence.command_text.clone().unwrap_or_default(),
        category: "command".into(),
        exit_code: command.exit_code.map(|code| code as i32),
        passed: true,
        change_id: None,
        session_id: Some(session_id.to_string()),
        fingerprint: end.fingerprint.clone(),
        branch: end.branch.clone(),
        head: end.head.clone(),
        changed_during_run: evidence.changed_during_run.clone(),
        created_at: timestamp(),
    })
}

fn candidates(events: &[HarnessEvent], current: &WorktreePosition) -> Vec<EvidenceCandidate> {
    let mut sessions: Vec<&str> = Vec::new();
    for event in events {
        if event.kind != "operation_finished" || event.tool_name.as_deref() != Some("exec_command")
        {
            continue;
        }
        if let Some(session_id) = event.result_summary.pointer("/evidence/command/session_id") {
            if let Some(session_id) = session_id.as_str() {
                sessions.push(session_id);
            }
        }
    }
    sessions
        .into_iter()
        .rev()
        .filter_map(|session_id| {
            let evidence = find_evidence(events, session_id)?;
            let fresh = evidence.command.passed()
                && evidence.command.workspace_writes_since_start == Some(0)
                && evidence.end.as_ref() == Some(current);
            fresh.then(|| EvidenceCandidate {
                session_id: session_id.to_string(),
                command: evidence.command_text,
            })
        })
        .take(CANDIDATES)
        .collect()
}

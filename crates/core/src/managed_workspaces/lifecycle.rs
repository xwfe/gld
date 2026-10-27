//! 可重试收尾与持久资源账目。命令发出前记意图，未得到终态的记录不因日志到期而消失。

use super::*;
use crate::tools::{runs, session::Found, workspace_runtime, Caller};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingCommand {
    operation: String,
    session: Option<String>,
}

pub(super) fn validate_record(record: &Record) -> Result<()> {
    if record.idle_retention_ms.is_some() != record.expires_at_ms.is_some()
        || record
            .idle_retention_ms
            .is_some_and(|ttl| !(60 * 60 * 1000..=8760 * 60 * 60 * 1000).contains(&ttl))
    {
        return Err(error(
            "WORK_SESSION_REGISTRY_INVALID",
            "Invalid retention policy",
            "storage",
        ));
    }
    if record.pending_commands.len() > 64
        || record.pending_commands.iter().any(|p| {
            Uuid::parse_str(&p.operation).is_err()
                || p.session
                    .as_ref()
                    .is_some_and(|id| Uuid::parse_str(id).is_err())
        })
        || (matches!(record.state, State::Closing | State::Closed) && record.finish_paths.is_none())
    {
        return Err(error(
            "WORK_SESSION_REGISTRY_INVALID",
            "Invalid close receipt or command resources",
            "storage",
        ));
    }
    if let Some(paths) = &record.finish_paths {
        if paths.len() > 32 || paths.iter().any(|p| artifacts::relative_parts(p).is_err()) {
            return Err(error(
                "WORK_SESSION_REGISTRY_INVALID",
                "Invalid retained-file plan",
                "storage",
            ));
        }
    }
    Ok(())
}

fn terminal(status: &str) -> bool {
    matches!(
        status,
        "exited" | "killed" | "timeout" | "interrupted" | "spawn_failed"
    )
}

impl Operation {
    pub fn begin_command(&mut self) -> Result<String> {
        self.require_active()?;
        if self.record.pending_commands.len() >= 64 {
            return Err(error(
                "WORK_SESSION_RESOURCE_LIMIT",
                "Too many unresolved commands; inspect them before starting more",
                "resource",
            ));
        }
        let id = Uuid::new_v4().to_string();
        self.record.pending_commands.push(PendingCommand {
            operation: id.clone(),
            session: None,
        });
        self.persist()?;
        Ok(id)
    }

    pub fn observe_command_result(&mut self, operation: &str, output: &Value) -> Result<()> {
        let Some(index) = self
            .record
            .pending_commands
            .iter()
            .position(|p| p.operation == operation)
        else {
            return Ok(());
        };
        let outcome = crate::tools::outcome::command_outcome(output);
        let did_not_start = output.pointer("/error/details/executed") == Some(&Value::Bool(false))
            || matches!(
                output.pointer("/error/category").and_then(Value::as_str),
                Some("policy" | "permission" | "validation" | "conflict")
            );
        if did_not_start || outcome.as_ref().is_some_and(|c| terminal(&c.status)) {
            self.record.pending_commands.remove(index);
        } else if let Some(session) = output.get("session_id").and_then(Value::as_str) {
            self.record.pending_commands[index].session = Some(session.into());
        }
        self.persist()
    }

    /// current_caller 用于读本进程内存会话；没有它时只采用持久终态，不靠 PID 猜。
    pub fn resources(&mut self, current_caller: Option<&Caller>) -> Result<Value> {
        let runtime = workspace_runtime::runtime_for(&self.record.root);
        let running = runtime
            .running_commands(current_caller.unwrap_or(&Caller::local()))
            .total();
        let log = runs::RunLog::new(
            self.home.join("runs"),
            &runs::project_id(&self.record.root),
            &self.record.owner,
        );
        let mut pending = Vec::new();
        let mut unknown = 0;
        for item in &self.record.pending_commands {
            let status = item.session.as_deref().and_then(|id| {
                if let Some(caller) = current_caller {
                    if let Ok(found) = runtime.sessions_for(caller).lookup(id) {
                        return Some(match found {
                            Found::Live(session) => {
                                crate::async_rt::block_on(session.refresh_status());
                                if crate::async_rt::block_on(session.is_running()) {
                                    "running".into()
                                } else {
                                    session.termination().0
                                }
                            }
                            Found::Recorded(record) => record.status,
                        });
                    }
                }
                log.load(id).map(|r| r.status)
            });
            if !status.as_deref().is_some_and(terminal) {
                if status.as_deref() != Some("running") {
                    unknown += 1;
                }
                pending.push(item.clone());
            }
        }
        if pending.len() != self.record.pending_commands.len() {
            self.record.pending_commands = pending;
            self.persist()?;
        }
        // 委派默认关闭；若出现任务资料，宁可保留，不能把读坏的任务当成已经结束。
        let agent_root =
            crate::agents::store::root(&self.home).join(runs::project_id(&self.record.root));
        let agents = match fs::read_dir(&agent_root) {
            Ok(entries) => entries
                .map(|e| e.map_err(storage))
                .collect::<Result<Vec<_>>>()?
                .iter()
                .filter(|e| {
                    crate::agents::store::read(&e.path()).is_none_or(|t| !t.state.is_finished())
                })
                .count(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(e) => return Err(storage(e)),
        };
        Ok(json!({
            "running_commands":running,
            "unresolved_commands":self.record.pending_commands.len(),
            "unknown_commands":unknown,
            "unfinished_or_unreadable_agents":agents,
            "legacy_history_unverified":!self.record.resource_tracking,
            "cleanup_blocked":running > 0 || !self.record.pending_commands.is_empty() || agents > 0 || !self.record.resource_tracking,
            "scope":"gld-managed commands only; machine MCP providers and arbitrary detached processes are not covered"
        }))
    }

    pub fn finish(&mut self, paths: &[String], caller: Option<&Caller>) -> Result<Value> {
        if paths.len() > 32 {
            return Err(WorkspaceError::invalid_argument("Select at most 32 files"));
        }
        for path in paths {
            artifacts::relative_parts(path)?;
        }
        let mut paths = paths.to_vec();
        paths.sort();
        paths.dedup();
        if self
            .record
            .finish_paths
            .as_ref()
            .is_some_and(|saved| *saved != paths)
        {
            return Err(error(
                "WORK_SESSION_FINISH_CONFLICT",
                "Retry finish with the same keep_paths; the saved close plan cannot be changed",
                "conflict",
            ));
        }
        if self.record.state == State::Closed {
            self.verify_artifacts()?;
            return Ok(
                json!({"closed":true,"already_closed":true,"artifacts":self.record.artifacts}),
            );
        }
        let runtime = workspace_runtime::runtime_for(&self.record.root);
        let guard = runtime
            .lock_commits_within(Duration::from_secs(2))
            .ok_or_else(workspace_runtime::write_lock_busy)?;
        if !guard.is_cross_process() {
            return Err(error(
                "WORK_SESSION_LOCK_UNAVAILABLE",
                "Cross-process exclusion could not be established; files were kept",
                "security",
            ));
        }
        let resources = self.resources(caller)?;
        if resources["cleanup_blocked"] == true {
            return Err(WorkspaceError::ToolDetails { code:"WORK_SESSION_RESOURCES_BUSY", message:"Commands are running or their end state is unverified; no source files were deleted".into(), category:"conflict",retryable:false,details:resources });
        }
        if self.record.state == State::Active {
            self.preserve(&paths)?;
            self.record.finish_paths = Some(paths);
            self.record.state = State::Closing;
            self.persist()?;
        } else if self.record.state != State::Closing {
            return Err(error(
                "WORK_SESSION_NOT_ACTIVE",
                "Creation is incomplete; retry start first",
                "conflict",
            ));
        }
        // 已保存的完整性是每次继续删除的前提，不能只相信上次的成功回包。
        self.verify_artifacts()?;
        if normal_dir(&self.record.root, false)? {
            verify_root(&self.home, &self.record)?;
            fs::remove_dir_all(&self.record.root).map_err(storage)?;
        }
        self.record.state = State::Closed;
        self.persist()?;
        runtime.record_write();
        Ok(
            json!({"closed":true,"already_closed":false,"artifacts":self.record.artifacts,"removed_root":self.record.root}),
        )
    }
}

#[cfg(test)]
mod tests;

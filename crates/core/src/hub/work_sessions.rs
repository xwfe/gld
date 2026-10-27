//! Hub 级非项目入口，不选择隐式默认工作区；已登记目录复用现有文件/命令工具。

use super::*;
use crate::managed_workspaces::{Record, Store};

pub(super) const TOOL: &str = "work_session";

impl Hub {
    pub(super) fn work_store(&self) -> Result<Store, WorkspaceError> {
        #[cfg(test)]
        if let Some(home) = &self.managed_home {
            return Ok(Store::new(home.clone()));
        }
        crate::home::data_home()
            .map(Store::new)
            .map_err(|e| WorkspaceError::Tool {
                code: "WORK_SESSION_STORAGE",
                message: e.to_string(),
                category: "storage",
                retryable: false,
            })
    }

    pub(super) fn work_sessions_offered(
        &self,
        settings: &AppSettings,
        grant: Option<&Grant>,
    ) -> bool {
        settings.hub.work_sessions
            && self.auth.auth_type != "noauth"
            && self.tool_profile != "read-only"
            && grant.is_none()
    }

    fn check_work_access(
        &self,
        auth: &AuthContext,
        settings: &AppSettings,
    ) -> Result<(), WorkspaceError> {
        if !auth.is_authenticated() || auth.grant().is_some() {
            return Err(WorkspaceError::Tool {
                code: "WORK_SESSION_REQUIRES_FULL_AUTH",
                message: "Managed workspaces require full authenticated service credentials, not noauth or a project grant".into(),
                category: "permission", retryable: false,
            });
        }
        if !self.work_sessions_offered(settings, None) {
            return Err(WorkspaceError::Tool {
                code: "WORK_SESSIONS_DISABLED",
                message: "Managed workspaces are disabled or the service is read-only. The operator can enable them with gld upgrade --work-sessions true".into(),
                category: "permission", retryable: false,
            });
        }
        Ok(())
    }

    pub(super) fn call_work_session(
        &self,
        auth: &AuthContext,
        args: &Value,
        settings: &AppSettings,
    ) -> Value {
        let result = (|| {
            self.check_work_access(auth, settings)?;
            let object = args
                .as_object()
                .ok_or_else(|| WorkspaceError::invalid_argument("work_session takes an object"))?;
            let action = text(args, "action")?;
            let allowed: &[&str] = match action {
                "start" => &["action", "client_request_id", "purpose"],
                "status" => &["action", "workspace"],
                "list" => &["action", "cursor", "limit"],
                _ => return Err(WorkspaceError::invalid_argument("action must be start, list or status; artifact preservation and cleanup are not implemented in W0")),
            };
            if object.keys().any(|key| !allowed.contains(&key.as_str())) {
                return Err(WorkspaceError::invalid_argument(format!(
                    "{action} accepts only {}",
                    allowed.join(", ")
                )));
            }
            let store = self.work_store()?;
            let owner = auth.tag();
            match action {
                "start" => {
                    let purpose = args
                        .get("purpose")
                        .map(|_| text_allow_empty(args, "purpose"))
                        .transpose()?
                        .unwrap_or("");
                    let (record, created) =
                        store.start(&owner, text(args, "client_request_id")?, purpose)?;
                    Ok(tool_ok(json!({
                        "workspace": record.id, "created": created, "session": summary(&record),
                        "next": "Call workspace_context with this workspace ID, then reuse it with read_file, apply_patch or exec_command. No user project or Git repository was created."
                    })))
                }
                "status" => {
                    let record = store.get(&owner, text(args, "workspace")?, false)?;
                    Ok(tool_ok(json!({"session": summary(&record)})))
                }
                "list" => {
                    let limit = match args.get("limit") {
                        None => 20,
                        Some(value) => value
                            .as_u64()
                            .filter(|n| (1..=100).contains(n))
                            .ok_or_else(|| {
                                WorkspaceError::invalid_argument("limit must be 1..=100")
                            })? as usize,
                    };
                    let cursor = args
                        .get("cursor")
                        .map(|_| text(args, "cursor"))
                        .transpose()?;
                    if cursor.is_some_and(|id| !crate::managed_workspaces::is_id(id)) {
                        return Err(WorkspaceError::invalid_argument(
                            "cursor must be a next_cursor from work_session list",
                        ));
                    }
                    let records = store.owned(&owner)?;
                    let page: Vec<_> = records
                        .iter()
                        .filter(|r| cursor.is_none_or(|c| r.id.as_str() > c))
                        .take(limit + 1)
                        .collect();
                    let more = page.len() > limit;
                    let selected = &page[..page.len().min(limit)];
                    Ok(tool_ok(json!({
                        "sessions": selected.iter().map(|r| summary(r)).collect::<Vec<_>>(),
                        "count": selected.len(),
                        "next_cursor": if more { selected.last().map(|r| r.id.as_str()) } else { None },
                    })))
                }
                _ => unreachable!("validated action"),
            }
        })();
        let mut structured = result.unwrap_or_else(tool_err);
        structured["operation_id"] = json!(uuid::Uuid::new_v4().simple().to_string());
        plain_result(structured)
    }

    pub(super) fn call_managed_workspace(
        &self,
        auth: &AuthContext,
        tool: &str,
        id: &str,
        args: &Value,
        settings: &AppSettings,
        routed: &mut Option<Routed>,
    ) -> Result<Value, Value> {
        let record = match self
            .check_work_access(auth, settings)
            .and_then(|()| self.work_store()?.get(&auth.tag(), id, true))
        {
            Ok(record) => record,
            Err(error) => return Ok(plain_result(tool_err(error))),
        };
        if remote_tools::find(tool).is_some() || remote_tools::is_session_tool(tool) {
            return Ok(plain_result(tool_err(WorkspaceError::invalid_argument(
                "Managed workspaces are local; remote_* tools do not operate on them",
            ))));
        }
        // 构造内部 profile 只为复用 context，不写入普通项目表、不分配端口/凭据。
        let mut profile = WorkspaceProfile::new(
            record.root.to_string_lossy().into_owned(),
            Some(format!("通用任务 {}", &record.id[5..13])),
        );
        profile.id = record.id;
        profile.runtime.history_recording = false;
        profile.runtime.ai_instructions = "This is a managed non-project work directory. Keep temporary files here; use explicit paths for user-requested outputs. Do not assume a Git repository or another project's rules. Guarded execution is policy-only, not an OS sandbox. W0 does not implement finish, artifact retention or automatic cleanup; report retained paths truthfully.".into();
        self.call_local(auth, tool, &profile, args, settings, routed)
    }
}

fn text<'a>(args: &'a Value, key: &str) -> Result<&'a str, WorkspaceError> {
    let value = text_allow_empty(args, key)?;
    if value.trim().is_empty() {
        return Err(WorkspaceError::invalid_argument(format!(
            "{key} must not be empty"
        )));
    }
    Ok(value)
}

fn text_allow_empty<'a>(args: &'a Value, key: &str) -> Result<&'a str, WorkspaceError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument(format!("{key} must be a string")))
}

fn summary(record: &Record) -> Value {
    json!({
        "workspace": record.id, "kind": "managed", "root": record.root,
        "purpose": record.purpose, "state": record.state,
        "created_at_ms": record.created_at_ms, "last_used_at_ms": record.last_used_at_ms,
        "policy": {"confine_reads": true, "command_policy": "guarded", "agent_delegation": "off", "sandbox_enforced": false},
        "cleanup_supported": false,
        "note": "W0 retains this directory and its records; finish/artifact/retain and automatic cleanup are not implemented. This is metadata, not a claim that processes are idle or files are delivered.",
    })
}

pub(super) fn definition() -> Value {
    json!({
        "name": TOOL,
        "title": "Managed work sessions",
        "description": "Create or inspect a non-project work directory managed by gld. start needs client_request_id (idempotent per caller) and optional purpose; list only shows this caller's sessions; status needs the returned workspace ID. No project registration or Git is needed. Every subsequent workspace tool still needs the returned workspace ID. W0 does not clean files or preserve artifacts automatically.",
        "inputSchema": {"type":"object", "properties": {
            "action": {"type":"string", "enum":["start","list","status"]},
            "client_request_id": {"type":"string","minLength":1,"maxLength":128},
            "purpose": {"type":"string","maxLength":256},
            "workspace": {"type":"string","minLength":1},
            "cursor": {"type":"string","minLength":1},
            "limit": {"type":"integer","minimum":1,"maximum":100,"default":20}
        },"required":["action"],"additionalProperties":false},
        "annotations": {"readOnlyHint":false,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}
    })
}

#[cfg(test)]
mod tests;

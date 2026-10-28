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
                "retain" => &[
                    "action",
                    "workspace",
                    "keep_paths",
                    "retention_hours",
                    "retain_forever",
                ],
                "artifact" => &["action", "artifact_id", "include_content"],
                "finish" => &["action", "workspace", "keep_paths", "outcome"],
                "sweep" => &["action", "cursor", "limit"],
                "capacity" => &["action"],
                _ => {
                    return Err(WorkspaceError::invalid_argument(
                        "action must be start, list, status, retain, artifact, finish, sweep or capacity",
                    ))
                }
            };
            if object.keys().any(|key| !allowed.contains(&key.as_str())) {
                return Err(WorkspaceError::invalid_argument(format!(
                    "{action} accepts only {}",
                    allowed.join(", ")
                )));
            }
            let store = self
                .work_store()?
                .with_limits(settings.hub.work_session_limits.clone());
            let owner = auth.tag();
            match action {
                "capacity" => Ok(tool_ok(store.capacity()?)),
                "start" => {
                    let purpose = args
                        .get("purpose")
                        .map(|_| text_allow_empty(args, "purpose"))
                        .transpose()?
                        .unwrap_or("");
                    let (record, created) =
                        store.start(&owner, text(args, "client_request_id")?, purpose)?;
                    let mut operation = store.operation(&owner, &record.id)?;
                    if operation.record.state == crate::managed_workspaces::State::Active {
                        operation.touch()?;
                    }
                    let record = operation.record.clone();
                    drop(operation);
                    let sweep = store
                        .sweep(&owner, None, 8, Some(&Caller::from_auth(auth)))
                        .unwrap_or_else(|e| json!({"error":e.message()}));
                    Ok(tool_ok(json!({
                        "workspace": record.id, "created": created, "session": summary(&record),
                        "expiry_sweep":sweep,
                        "next": "Call workspace_context with this workspace ID, then reuse it with read_file, apply_patch or exec_command. No user project or Git repository was created."
                    })))
                }
                "status" => {
                    let mut operation = store.operation(&owner, text(args, "workspace")?)?;
                    let resources = operation.resources(Some(&Caller::from_auth(auth)))?;
                    Ok(tool_ok(
                        json!({"session": summary(&operation.record), "resources":resources}),
                    ))
                }
                "finish" => {
                    let paths = keep_paths(args)?;
                    let outcome = args
                        .get("outcome")
                        .map(|_| text(args, "outcome"))
                        .transpose()?
                        .unwrap_or("success");
                    if !["success", "failed"].contains(&outcome) {
                        return Err(WorkspaceError::invalid_argument(
                            "outcome must be success or failed",
                        ));
                    }
                    let mut operation = store.operation(&owner, text(args, "workspace")?)?;
                    let receipt = if outcome == "failed" {
                        let artifacts = operation.preserve(&paths)?;
                        operation.set_retention(Some(24))?;
                        json!({"closed":false,"source_files_kept":true,"artifacts":artifacts,"session":summary(&operation.record),"note":"Failed task retained for 24 idle hours; busy or unknown resources still block expiry cleanup."})
                    } else {
                        operation.finish(&paths, Some(&Caller::from_auth(auth)))?
                    };
                    drop(operation);
                    let mut result = tool_ok(receipt);
                    result["expiry_sweep"] = store
                        .sweep(&owner, None, 8, Some(&Caller::from_auth(auth)))
                        .unwrap_or_else(|e| json!({"error":e.message()}));
                    Ok(result)
                }
                "retain" => {
                    let retention = retention_arg(args)?;
                    let paths = if args.get("keep_paths").is_some() {
                        keep_paths(args)?
                    } else if retention.is_some() {
                        Vec::new()
                    } else {
                        return Err(WorkspaceError::invalid_argument(
                            "retain needs keep_paths, retention_hours or retain_forever=true",
                        ));
                    };
                    let mut operation = store.operation(&owner, text(args, "workspace")?)?;
                    let runtime =
                        crate::tools::workspace_runtime::runtime_for(&operation.record.root);
                    let _guard = runtime
                        .lock_commits()
                        .ok_or_else(crate::tools::workspace_runtime::write_lock_busy)?;
                    let artifacts = operation.preserve(&paths)?;
                    if let Some(hours) = retention {
                        operation.set_retention(hours)?;
                    } else {
                        operation.touch()?;
                    }
                    Ok(tool_ok(
                        json!({"artifacts": artifacts, "source_files_kept": true, "session":summary(&operation.record)}),
                    ))
                }
                "artifact" => {
                    let id = text(args, "artifact_id")?;
                    let workspace = crate::managed_workspaces::artifact_workspace(id)?;
                    let include_content = match args.get("include_content") {
                        None => false,
                        Some(value) => value.as_bool().ok_or_else(|| {
                            WorkspaceError::invalid_argument("include_content must be boolean")
                        })?,
                    };
                    let operation = store.operation(&owner, &workspace)?;
                    Ok(tool_ok(operation.artifact(id, include_content)?))
                }
                "sweep" => {
                    let cursor = args
                        .get("cursor")
                        .map(|_| text(args, "cursor"))
                        .transpose()?;
                    let limit = match args.get("limit") {
                        None => 8,
                        Some(v) => v.as_u64().filter(|v| (1..=16).contains(v)).ok_or_else(|| {
                            WorkspaceError::invalid_argument("sweep limit must be 1..=16")
                        })? as usize,
                    };
                    Ok(tool_ok(store.sweep(
                        &owner,
                        cursor,
                        limit,
                        Some(&Caller::from_auth(auth)),
                    )?))
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
        let mut operation = match self
            .check_work_access(auth, settings)
            .and_then(|()| self.work_store()?.operation(&auth.tag(), id))
        {
            Ok(operation) => operation,
            Err(error) => return Ok(plain_result(tool_err(error))),
        };
        if let Err(error) = operation.touch() {
            return Ok(plain_result(tool_err(error)));
        }
        let record = &operation.record;
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
        profile.id = record.id.clone();
        profile.runtime.history_recording = false;
        profile.runtime.ai_instructions = "This is a managed non-project work directory. Keep temporary files here. Use retain to save verified artifacts; finish with explicit keep_paths saves selected files then deletes the temporary directory only after resources are confirmed stopped. Artifact retrieval remains available after closing. Do not assume a Git repository or another project's rules. Guarded execution is policy-only, not an OS sandbox. Report saved and removed files separately.".into();
        let started = if tool == "exec_command" {
            match operation.begin_command() {
                Ok(id) => Some(id),
                Err(error) => return Ok(plain_result(tool_err(error))),
            }
        } else {
            None
        };
        let mut result = self.call_local(auth, tool, &profile, args, settings, routed)?;
        let recorded = match started {
            Some(id) => operation.observe_command_result(&id, &result["structuredContent"]),
            None => Ok(()),
        }
        .and_then(|()| {
            operation
                .resources(Some(&Caller::from_auth(auth)))
                .map(|_| ())
        });
        if let Err(error) = recorded {
            // 命令可能已经完成；不把资源记账失败伪装成命令没执行，让模型重跑。
            result["structuredContent"]["cleanup_warning"] = json!(error.message());
            if let Some(content) = result["content"].as_array_mut() {
                content.push(json!({"type":"text","text":format!("Resource bookkeeping could not be confirmed; do not repeat the command. Cleanup remains conservative: {}",error.message())}));
            }
        }
        Ok(result)
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

fn keep_paths(args: &Value) -> Result<Vec<String>, WorkspaceError> {
    let values = args.get("keep_paths").and_then(Value::as_array)
        .filter(|v| v.len() <= 32).ok_or_else(|| WorkspaceError::invalid_argument("keep_paths must explicitly list at most 32 relative files (an empty array keeps none)"))?;
    values
        .iter()
        .map(|v| {
            v.as_str().map(str::to_string).ok_or_else(|| {
                WorkspaceError::invalid_argument("Each keep_paths entry must be a string")
            })
        })
        .collect()
}

fn text_allow_empty<'a>(args: &'a Value, key: &str) -> Result<&'a str, WorkspaceError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument(format!("{key} must be a string")))
}

fn retention_arg(args: &Value) -> Result<Option<Option<u64>>, WorkspaceError> {
    match (args.get("retention_hours"), args.get("retain_forever")) {
        (Some(value), None) => Ok(Some(Some(
            value
                .as_u64()
                .filter(|v| (1..=8760).contains(v))
                .ok_or_else(|| {
                    WorkspaceError::invalid_argument("retention_hours must be 1..=8760")
                })?,
        ))),
        (None, Some(Value::Bool(true))) => Ok(Some(None)),
        (None, None) => Ok(None),
        _ => Err(WorkspaceError::invalid_argument(
            "Use either retention_hours or retain_forever=true, not both",
        )),
    }
}

fn summary(record: &Record) -> Value {
    json!({
        "workspace": record.id, "kind": "managed", "root": record.root,
        "purpose": record.purpose, "state": record.state,
        "created_at_ms": record.created_at_ms, "last_used_at_ms": record.last_used_at_ms,
        "policy": {"confine_reads": true, "command_policy": "guarded", "agent_delegation": "off", "sandbox_enforced": false},
        "cleanup_supported": true,
        "artifacts": record.artifacts,
        "unresolved_commands":record.pending_commands.len(),
        "expires_at_ms":record.expires_at_ms,
        "idle_retention_ms":record.idle_retention_ms,
        "note": "finish requires an explicit keep_paths list and verified idle resources. Artifacts survive closure. Query status for resource checks; active alone does not imply idle.",
    })
}

pub(super) fn definition() -> Value {
    json!({
        "name": TOOL,
        "title": "Managed work sessions",
        "description": "Manage owned non-project work directories. start is idempotent, including archived closed requests; status checks resources. capacity reports aggregate limits and usage, not an OS hard quota. retain saves keep_paths or sets retention_hours/retain_forever. finish with explicit keep_paths verifies artifacts and idle resources before removing the temporary directory and archiving its receipt; outcome=failed retains it for 24 idle hours. artifact retrieves saved results after closure. Bounded expiry sweeps run on start/finish or explicit sweep; busy/unknown resources and saved artifacts are never evicted for capacity.",
        "inputSchema": {"type":"object", "properties": {
            "action": {"type":"string", "enum":["start","list","status","retain","artifact","finish","sweep","capacity"]},
            "client_request_id": {"type":"string","minLength":1,"maxLength":128},
            "purpose": {"type":"string","maxLength":256},
            "workspace": {"type":"string","minLength":1},
            "cursor": {"type":"string","minLength":1},
            "keep_paths": {"type":"array","maxItems":32,"items":{"type":"string"}},
            "artifact_id": {"type":"string","minLength":1},
            "include_content": {"type":"boolean","default":false},
            "retention_hours": {"type":"integer","minimum":1,"maximum":8760},
            "retain_forever": {"type":"boolean"},
            "outcome": {"type":"string","enum":["success","failed"],"default":"success"},
            "limit": {"type":"integer","minimum":1,"maximum":100,"default":20}
        },"required":["action"],"additionalProperties":false},
        "annotations": {"readOnlyHint":false,"destructiveHint":true,"idempotentHint":true,"openWorldHint":false}
    })
}

#[cfg(test)]
mod tests;

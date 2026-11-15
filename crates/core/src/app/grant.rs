//! 统一授权的应用入口。凭据发行在认证接入完成前明确拒绝，管理只使用新存储。

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::App;
use crate::auth::{AccessRecord, CredentialKind, GrantPermissions, GrantStatus, ProjectScope};
use crate::error::{AppError, AppResult};

pub use gld_protocol::daemon::{BrowserGrantSpec, GrantSpec};

/// 仅管理元数据，不能用于重新交付或验证秘密。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantCredentialDto {
    pub id: String,
    pub kind: CredentialKind,
    pub revision: u64,
    pub issued_at_unix_secs: u64,
    pub expires_at_unix_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantDto {
    pub id: String,
    pub name: String,
    pub scope: ProjectScope,
    pub permissions: GrantPermissions,
    pub status: GrantStatus,
    pub credential: GrantCredentialDto,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantRemoved {
    pub grant: GrantDto,
    pub stopped_commands: usize,
    pub stopped_agent_tasks: usize,
}

impl App {
    pub fn list_grants(&self) -> AppResult<Vec<GrantDto>> {
        let current = self.access_reader()?.snapshot()?;
        Ok(current.records().iter().map(grant_dto).collect())
    }

    pub fn add_grant(&self, _spec: GrantSpec) -> AppResult<Value> {
        Err(issuance_not_ready())
    }

    pub fn add_browser_grant(&self, _spec: BrowserGrantSpec) -> AppResult<Value> {
        Err(issuance_not_ready())
    }

    pub fn remove_grant_local(&self, selector: &str) -> AppResult<GrantRemoved> {
        self.remove_grant_with_cleanup(selector, |grant_id, data_home| {
            (
                crate::tools::workspace_runtime::terminate_grant_sessions(grant_id),
                data_home.map_or(0, |home| crate::agents::stop_for_grant(home, grant_id)),
            )
        })
    }

    /// 提交撤销后才回收任务，失败的保存不能伪装成撤销成功。
    pub fn remove_grant_with_cleanup(
        &self,
        selector: &str,
        cleanup: impl Fn(&str, Option<&Path>) -> (usize, usize) + Send + 'static,
    ) -> AppResult<GrantRemoved> {
        let selector = selector.trim();
        let store = self.access_store()?;
        let current = store.snapshot()?;
        let record = current
            .by_grant_id(selector)
            .or_else(|| current.by_name(selector))
            .ok_or_else(|| {
                AppError::Message(format!(
                    "没有叫「{selector}」的 grant。看看有哪些：gld grant ls"
                ))
            })?;
        let grant_id = record.grant.id.clone();
        let mut dto = grant_dto(record);
        dto.status = store.revoke(&grant_id)?.status;
        drop(current);

        let data_home = crate::home::data_home().ok();
        let (stopped_commands, stopped_agent_tasks) = cleanup(&grant_id, data_home.as_deref());
        // 已接受的调用可能仍在等待执行锁；沿用现有二次回收，始终绑定原 GrantId。
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(35));
            cleanup(&grant_id, data_home.as_deref());
        });
        Ok(GrantRemoved {
            grant: dto,
            stopped_commands,
            stopped_agent_tasks,
        })
    }
}

fn issuance_not_ready() -> AppError {
    AppError::Message("AUTH_NOT_READY: 当前版本尚未启用统一凭据签发，旧授权创建入口已停用。".into())
}

fn grant_dto(record: &AccessRecord) -> GrantDto {
    GrantDto {
        id: record.grant.id.clone(),
        name: record.grant.name.clone(),
        scope: record.grant.scope.clone(),
        permissions: record.grant.permissions,
        status: record.grant.status,
        credential: GrantCredentialDto {
            id: record.credential.id.clone(),
            kind: record.credential.kind,
            revision: record.credential.revision,
            issued_at_unix_secs: record.credential.issued_at_unix_secs,
            expires_at_unix_secs: record.credential.expires_at_unix_secs,
        },
    }
}

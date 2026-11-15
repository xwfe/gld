//! 统一授权的应用入口；创建默认值只作用于显式新建，不解释旧授权。

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::App;
use crate::auth::{
    AccessRecord, CredentialKind, CredentialTtl, GrantCreation, GrantPermissions, GrantStatus,
    IssuedAccess, IssuedSecret, ProjectScope, ServiceCapabilities,
};
use crate::error::{AppError, AppResult};
use crate::hub::Hub;

pub use gld_protocol::daemon::{BrowserGrantSpec, GrantSpec};

/// 可信应用入口的创建参数；wire/CLI 适配不在此处猜测缺失权限。
#[derive(Debug)]
pub struct GrantCreate {
    pub name: String,
    pub projects: Vec<String>,
    pub all: bool,
    pub via: CredentialKind,
    pub write: bool,
    pub service_access: bool,
    pub ttl: Option<CredentialTtl>,
    pub allowed_origins: Vec<String>,
}

impl GrantCreate {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            projects: Vec::new(),
            all: false,
            via: CredentialKind::Token,
            write: false,
            service_access: false,
            ttl: None,
            allowed_origins: Vec::new(),
        }
    }
}

/// 只由成功创建/轮换返回；列表和存储没有恢复该秘密的接口。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantIssued {
    pub grant: GrantDto,
    pub secret: IssuedSecret,
}

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
    pub fn create_grant(&self, spec: GrantCreate, hub: &Hub) -> AppResult<GrantIssued> {
        let scope = hub.grant_scope(&spec.projects, spec.all)?;
        let permissions = GrantPermissions {
            write: spec.write,
            service: ServiceCapabilities {
                user_prompts: spec.service_access,
                machine_mcp: spec.service_access,
                managed_workspaces: spec.service_access,
            },
        };
        // 首次创建不经过重建提示：这是从旧授权切换后建立新授权的入口。
        let issued = self.access_store()?.create_grant(
            GrantCreation {
                name: spec.name,
                scope,
                permissions,
                kind: spec.via,
                ttl: spec.ttl,
                allowed_origins: spec.allowed_origins,
            },
            now_unix_secs()?,
        )?;
        Ok(issued.into())
    }

    pub fn rotate_grant(
        &self,
        selector: &str,
        ttl: Option<CredentialTtl>,
    ) -> AppResult<GrantIssued> {
        let store = self.access_store()?;
        self.require_current_authorization(&store)?;
        Ok(store.rotate_grant(selector, ttl, now_unix_secs()?)?.into())
    }

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
        self.require_current_authorization(&store)?;
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
    AppError::Message("AUTH_NOT_READY: 旧授权创建入口已停用，统一命令接口尚未接通。".into())
}

fn now_unix_secs() -> AppResult<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| AppError::Message("系统时钟早于 Unix 起点，无法签发凭据。".into()))
}

impl From<IssuedAccess> for GrantIssued {
    fn from(issued: IssuedAccess) -> Self {
        Self {
            grant: grant_dto(&issued.record),
            secret: issued.secret,
        }
    }
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

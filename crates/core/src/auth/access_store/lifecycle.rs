//! 创建与轮换共用材料规则；秘密只随成功提交的结果离开写入用例。

use gld_policy::{CredentialKind, Grant, GrantPermissions, GrantStatus, ProjectScope};

use super::{invalid, AccessRecord, AccessStore, CredentialRecord};
use crate::auth::credential::{credential_lifetime, generate_secret, CredentialTtl, IssuedSecret};
use crate::error::AppResult;

/// 已在应用层解析项目选择器的创建请求，不从存储缺省推导权限。
#[derive(Debug)]
pub struct GrantCreation {
    pub name: String,
    pub scope: ProjectScope,
    pub permissions: GrantPermissions,
    pub kind: CredentialKind,
    pub ttl: Option<CredentialTtl>,
    pub allowed_origins: Vec<String>,
}

/// 不能序列化整条存储记录；应用层将元数据与一次性交付秘密分别投影。
#[derive(Debug)]
pub struct IssuedAccess {
    pub record: AccessRecord,
    pub secret: IssuedSecret,
}

impl AccessStore {
    pub fn create_grant(&self, spec: GrantCreation, now_unix_secs: u64) -> AppResult<IssuedAccess> {
        let grant = Grant {
            id: uuid::Uuid::new_v4().simple().to_string(),
            name: spec.name.trim().to_string(),
            scope: spec.scope,
            permissions: spec.permissions,
            status: GrantStatus::Active,
        };
        grant
            .validate()
            .map_err(|error| invalid(&error.to_string()))?;
        let lifetime =
            credential_lifetime(spec.kind, spec.ttl, None).map_err(|error| invalid(&error))?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let secret = generate_secret(spec.kind, &id).map_err(|error| invalid(&error))?;
        let record = AccessRecord {
            grant,
            credential: CredentialRecord::prepare(
                id,
                spec.kind,
                secret.as_str(),
                1,
                now_unix_secs,
                lifetime,
                spec.allowed_origins,
            )
            .map_err(|error| invalid(&error))?,
        };
        record.validate().map_err(|error| invalid(&error))?;
        // insert 的原子保存失败时，record 与 secret 都只在本次栈上，不返回给调用方。
        self.insert(record.clone())?;
        Ok(IssuedAccess { record, secret })
    }

    pub fn rotate_grant(
        &self,
        selector: &str,
        ttl: Option<CredentialTtl>,
        now_unix_secs: u64,
    ) -> AppResult<IssuedAccess> {
        let selector = selector.trim();
        self.change(|records| {
            let index = records
                .iter()
                .position(|record| record.grant.id == selector)
                .or_else(|| {
                    records.iter().position(|record| {
                        record.grant.status == GrantStatus::Active
                            && record.grant.name.eq_ignore_ascii_case(selector)
                    })
                })
                .ok_or_else(|| invalid("Grant not found; use gld grant ls"))?;
            let record = &mut records[index];
            if record.grant.status == GrantStatus::Revoked {
                return Err(invalid("A revoked grant cannot be rotated"));
            }
            let current = &record.credential;
            let revision = current
                .revision
                .checked_add(1)
                .ok_or_else(|| invalid("Credential revision overflow"))?;
            let lifetime = credential_lifetime(current.kind, ttl, Some(current))
                .map_err(|error| invalid(&error))?;
            let secret =
                generate_secret(current.kind, &current.id).map_err(|error| invalid(&error))?;
            record.credential = CredentialRecord::prepare(
                current.id.clone(),
                current.kind,
                secret.as_str(),
                revision,
                now_unix_secs,
                lifetime,
                current.allowed_origins.clone(),
            )
            .map_err(|error| invalid(&error))?;
            // 只替换锁内读到的当前材料，不回写过时的名字、范围或权限。
            Ok(IssuedAccess {
                record: record.clone(),
                secret,
            })
        })
    }
}

#[cfg(test)]
mod tests;

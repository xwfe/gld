//! 创建与轮换共用材料规则；秘密只随成功提交的结果离开写入用例。

use gld_policy::{CredentialKind, Grant, GrantPermissions, GrantStatus, ProjectScope};

use super::{invalid, AccessRecord, AccessStore, CredentialRecord, TunnelBinding};
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

    /// 把一条隧道绑定到一个有效的 token 类型 grant，记下此刻的凭据修订号。
    /// `tunnel` 是 [`super::openai_tunnel_name`] 生成的标识，`selector` 是 GrantId 或有效 grant 的名字。
    /// 这条隧道已绑同一个 grant 时只更新修订号、保留绑定 ID（轮换后重绑，调用方身份不变）；
    /// 改绑别的 grant 时换新 ID。OAuth、URL 用途和已撤销的 grant 都不能绑。
    pub fn bind_tunnel(
        &self,
        tunnel: &str,
        selector: &str,
        now_unix_secs: u64,
    ) -> AppResult<TunnelBinding> {
        let selector = selector.trim();
        self.change_all(|records, bindings| {
            let record = records
                .iter()
                .find(|record| record.grant.id == selector)
                .or_else(|| {
                    records.iter().find(|record| {
                        record.grant.status == GrantStatus::Active
                            && record.grant.name.eq_ignore_ascii_case(selector)
                    })
                })
                .ok_or_else(|| invalid("Grant not found; use gld grant ls"))?;
            if record.grant.status == GrantStatus::Revoked {
                return Err(invalid("A revoked grant cannot be bound to a tunnel"));
            }
            if record.credential.kind != CredentialKind::Token {
                return Err(invalid("Only a token grant can be bound to a tunnel"));
            }
            let previous = bindings.iter().position(|old| old.tunnel == tunnel);
            let id = previous
                .map(|index| &bindings[index])
                .filter(|old| old.grant_id == record.grant.id)
                .map(|old| old.id.clone())
                .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
            let binding = TunnelBinding {
                id,
                tunnel: tunnel.to_string(),
                grant_id: record.grant.id.clone(),
                credential_revision: record.credential.revision,
                bound_at_unix_secs: now_unix_secs,
            };
            binding.validate().map_err(|error| invalid(&error))?;
            match previous {
                Some(index) => bindings[index] = binding.clone(),
                None => bindings.push(binding.clone()),
            }
            Ok(binding)
        })
    }

    /// 解除一条隧道的绑定；本来就没有绑定时返回 `false`，不写文件以外的状态。
    pub fn unbind_tunnel(&self, tunnel: &str) -> AppResult<bool> {
        self.change_all(|_, bindings| {
            let before = bindings.len();
            bindings.retain(|binding| binding.tunnel != tunnel);
            Ok(bindings.len() != before)
        })
    }
}

#[cfg(test)]
mod tests;

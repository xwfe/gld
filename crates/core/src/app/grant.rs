//! `gld grant`：发一把只开部分项目的凭据、列出来、作废（RFC-0007）。
//!
//! 范围怎么在请求里生效在 [`crate::hub`]，钥匙怎么验在 [`crate::auth`]。这里只管登记。

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::App;
use crate::auth::{Grant, GrantRecord};
use crate::error::{AppError, AppResult};

/// `gld grant add` 收到的参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantSpec {
    pub name: String,
    /// 服务里的项目：id、名称或 id 前缀（≥4 位），本地和远端都行。
    pub workspaces: Vec<String>,
    /// 能写能跑命令。默认 false（只读）：能跑命令就能以你的身份读到数据目录里的服务
    /// 口令，拿到全权，所以要显式要（`gld grant add --write`）。
    #[serde(default)]
    pub writable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantMemberDto {
    pub id: String,
    /// 项目已经不在服务里时是空串：grant 里还记着它的 id，但它不会被看到。
    pub name: String,
}

/// 一把 grant。钥匙是明文，由命令行决定脱不脱敏（和 `gld secret ls` 一样）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantDto {
    pub id: String,
    pub name: String,
    pub workspaces: Vec<GrantMemberDto>,
    pub read_only: bool,
    pub created_at: String,
    /// OAuth 授权页上填的口令。
    pub oauth_password: String,
    /// bearer 认证时客户端带的令牌。
    pub bearer_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantRemoved {
    pub grant: GrantDto,
    /// 它起的、被一起停掉的命令会话数。
    pub stopped_commands: usize,
}

impl App {
    pub fn list_grants(&self) -> AppResult<Vec<GrantDto>> {
        let records = self.with_data(|store| Ok(store.grants().to_vec()))?;
        let members = self.hub_member_names()?;
        Ok(records
            .iter()
            .map(|record| grant_dto(record, &members))
            .collect())
    }

    /// 发一把新的。口令和令牌当场随机生成，两样都给：服务换认证方式时不用重发。
    pub fn add_grant(&self, spec: GrantSpec) -> AppResult<GrantDto> {
        let name = spec.name.trim().to_string();
        if name.is_empty() || name.chars().count() > 64 {
            return Err(AppError::Message(
                "grant 要有个名字（64 个字以内），用来在 gld grant ls / rm 里认它".into(),
            ));
        }
        // noauth 下谁连上都是全权，没有"用哪把钥匙进来"这回事：建了也挡不住任何人。
        if self.settings()?.hub.auth_type == "noauth" {
            return Err(AppError::Message(
                "服务的认证是 noauth：谁连上都是全权，按凭据分范围没有意义。先 gld upgrade --auth oauth（或 bearer）".into(),
            ));
        }
        if spec.workspaces.is_empty() {
            return Err(AppError::Message(
                "至少开一个项目：gld grant add <名字> api web".into(),
            ));
        }
        let members = self.hub_member_names()?;
        let profiles = self.list_workspaces()?;
        let mut workspaces: Vec<String> = Vec::new();
        for selector in &spec.workspaces {
            let id = resolve_member(&members, selector)?;
            // 关了 confine-reads 的项目，读工具能读整台机器；服务也会拒 grant 用它，
            // 这里先拦下，免得发出去一把用不了的。
            if let Some(open) = profiles
                .iter()
                .find(|profile| profile.id == id && !profile.runtime.confine_reads)
            {
                return Err(AppError::Message(format!(
                    "项目「{}」关了 confine-reads：经它能读到项目目录外面，不能开给 grant。要开就先 gld set {} confine-reads=true",
                    open.name, open.name
                )));
            }
            if !workspaces.contains(&id) {
                workspaces.push(id);
            }
        }
        let record = GrantRecord {
            grant: Grant {
                id: uuid::Uuid::new_v4().simple().to_string(),
                name: name.clone(),
                workspaces,
                read_only: !spec.writable,
            },
            oauth_password: crate::data::random_secret(),
            bearer_token: crate::data::random_secret(),
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_millis().to_string())
                .unwrap_or_else(|_| "0".into()),
        };
        // 查重和写入在同一次 with_data 里：两条并发的 add 不会建出两个同名的。
        self.with_data(|store| {
            if let Some(clash) = store
                .grants()
                .iter()
                .find(|item| item.grant.name.eq_ignore_ascii_case(&name))
            {
                return Err(AppError::Message(format!(
                    "已经有一个叫「{}」的 grant 了。换个名字，或者先 gld grant rm {} 再建",
                    clash.grant.name, clash.grant.name
                )));
            }
            store.add_grant(record.clone())
        })?;
        Ok(grant_dto(&record, &members))
    }

    /// 作废一把：下一次请求起它的令牌 401，刷新也换不来新的；它起的命令一起停掉。
    pub fn remove_grant(&self, selector: &str) -> AppResult<GrantRemoved> {
        let selector = selector.trim();
        let removed = self.with_data(|store| {
            let found = store
                .grants()
                .iter()
                .find(|item| item.grant.id == selector)
                .or_else(|| {
                    store
                        .grants()
                        .iter()
                        .find(|item| item.grant.name.eq_ignore_ascii_case(selector))
                })
                .map(|item| item.grant.id.clone());
            match found {
                Some(id) => store.remove_grant(&id),
                None => Ok(None),
            }
        })?;
        let Some(record) = removed else {
            return Err(AppError::Message(format!(
                "没有叫「{selector}」的 grant。看看有哪些：gld grant ls"
            )));
        };
        // 先删记录再停命令：反过来的话，停完到删掉之间它还能再起一条。
        let grant_id = record.grant.id.clone();
        let stopped_commands = crate::tools::workspace_runtime::terminate_grant_sessions(&grant_id);
        // 删之前已经过了鉴权的那一次调用还在跑：它要起命令的话，可能正排在写锁上（最多等
        // 30 秒），起来时已经在上面那次清理之后了，没人读得到、停得掉，一直跑到它自己的
        // timeout（上限 10 分钟）。35 秒后再清一次。
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(35));
            crate::tools::workspace_runtime::terminate_grant_sessions(&grant_id);
        });
        let members = self.hub_member_names()?;
        Ok(GrantRemoved {
            grant: grant_dto(&record, &members),
            stopped_commands,
        })
    }

    /// 服务里的成员（本地和远端），按加入顺序：(id, 名称)。
    fn hub_member_names(&self) -> AppResult<Vec<(String, String)>> {
        let ids = self.settings()?.hub.members;
        let profiles = self.list_workspaces()?;
        let remotes = self.ccnm_members()?;
        Ok(ids
            .iter()
            .filter_map(|id| {
                profiles
                    .iter()
                    .find(|profile| &profile.id == id)
                    .map(|profile| (profile.id.clone(), profile.name.clone()))
                    .or_else(|| {
                        remotes
                            .iter()
                            .find(|remote| &remote.id == id)
                            .map(|remote| (remote.id.clone(), remote.name.clone()))
                    })
            })
            .collect())
    }
}

/// 在服务成员里找：id、名称（不分大小写）、id 前缀（≥4 位）。只认服务里的——grant
/// 开的是"经服务访问"的范围，登记了但不在服务里的项目经服务本来就进不去。
fn resolve_member(members: &[(String, String)], selector: &str) -> AppResult<String> {
    let selector = selector.trim();
    if let Some((id, _)) = members.iter().find(|(id, _)| id == selector) {
        return Ok(id.clone());
    }
    let by_name: Vec<&(String, String)> = members
        .iter()
        .filter(|(_, name)| name.eq_ignore_ascii_case(selector))
        .collect();
    let matched = if by_name.is_empty() && selector.len() >= 4 {
        members
            .iter()
            .filter(|(id, _)| id.starts_with(selector))
            .collect()
    } else {
        by_name
    };
    match matched.as_slice() {
        [(id, _)] => Ok(id.clone()),
        [] => Err(AppError::Message(format!(
            "「{selector}」不在服务里。gld ls 看有哪些项目；没加进来的先 gld add"
        ))),
        several => Err(AppError::Message(format!(
            "「{selector}」对得上好几个项目：{}。改用完整 id",
            several
                .iter()
                .map(|(id, name)| format!("{name}（{}）", crate::short_id(id)))
                .collect::<Vec<_>>()
                .join("、")
        ))),
    }
}

fn grant_dto(record: &GrantRecord, members: &[(String, String)]) -> GrantDto {
    GrantDto {
        id: record.grant.id.clone(),
        name: record.grant.name.clone(),
        workspaces: record
            .grant
            .workspaces
            .iter()
            .map(|id| GrantMemberDto {
                id: id.clone(),
                name: members
                    .iter()
                    .find(|(member, _)| member == id)
                    .map(|(_, name)| name.clone())
                    .unwrap_or_default(),
            })
            .collect(),
        read_only: record.grant.read_only,
        created_at: record.created_at.clone(),
        oauth_password: record.oauth_password.clone(),
        bearer_token: record.bearer_token.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn members() -> Vec<(String, String)> {
        vec![
            ("a1b2c3d4e5".into(), "api".into()),
            ("f6a7b8c9d0".into(), "web".into()),
            ("remote-prod".into(), "prod".into()),
        ]
    }

    #[test]
    fn members_resolve_by_id_name_or_prefix_and_nothing_else() {
        let members = members();
        assert_eq!(resolve_member(&members, "API").unwrap(), "a1b2c3d4e5");
        assert_eq!(resolve_member(&members, "f6a7").unwrap(), "f6a7b8c9d0");
        assert_eq!(
            resolve_member(&members, "remote-prod").unwrap(),
            "remote-prod"
        );
        assert!(resolve_member(&members, "f6a").is_err(), "前缀太短不猜");
        let missing = resolve_member(&members, "outsider")
            .unwrap_err()
            .to_string();
        assert!(missing.contains("不在服务里"), "{missing}");
    }
}

use super::App;
use crate::error::{AppError, AppResult};
use crate::runtime::ServiceKind;
use crate::workspace::WorkspaceProfile;

/// 工作区级密钥的合法 key。
pub const WORKSPACE_SECRET_KEYS: &[&str] = &[
    "oauth_client_secret",
    "oauth_password",
    "oauth_token_secret",
    "bearer_token",
    "cloudflare_token",
    "actions_cloudflare_token",
    "actions_api_key",
    "actions_oauth_client_secret",
    "actions_oauth_password",
    "actions_oauth_token_secret",
    "frp_token",
    "actions_frp_token",
];

/// 共享密钥池的合法 key。
pub const SHARED_SECRET_KEYS: &[&str] = &[
    "oauth_client_id",
    "bearer_token",
    "oauth_client_secret",
    "oauth_password",
    "oauth_token_secret",
    "actions_api_key",
    "actions_oauth_client_secret",
    "actions_oauth_password",
    "actions_oauth_token_secret",
];

const MCP_KEYS: &[&str] = &[
    "oauth_client_id",
    "bearer_token",
    "oauth_client_secret",
    "oauth_password",
    "oauth_token_secret",
];

const ACTIONS_KEYS: &[&str] = &[
    "actions_api_key",
    "actions_oauth_client_secret",
    "actions_oauth_password",
    "actions_oauth_token_secret",
];

/// 这个工作区的这个密钥，服务启动时是从共享池读、还是读工作区自己那份。
///
/// 勾了 shared-secrets 之后，工作区里存的那份就完全不参与了。谁想告诉用户
/// "你的凭据是什么"，都得先问这里——否则给出去的值客户端用不了，
/// 表现成一直 401，而 `gld connect` 又显示着另一个值，两边对不上。
pub fn reads_from_shared_pool(profile: &WorkspaceProfile, key: &str) -> bool {
    if !SHARED_SECRET_KEYS.contains(&key) {
        return false;
    }
    if ACTIONS_KEYS.contains(&key) {
        profile.actions.use_shared_secrets
    } else if MCP_KEYS.contains(&key) {
        profile.auth.use_shared_secrets
    } else {
        false
    }
}

impl App {
    pub fn workspace_secret(&self, id: &str, key: &str) -> AppResult<Option<String>> {
        validate_workspace_key(key)?;
        self.ensure_workspace_exists(id)?;
        self.with_data(|store| store.get_workspace_secret(id, key))
    }

    /// 设置工作区密钥，并重启正在使用它的服务。
    pub async fn set_workspace_secret(&self, id: &str, key: &str, value: &str) -> AppResult<()> {
        validate_workspace_key(key)?;
        if value.is_empty() {
            return Err(AppError::Message("密钥不能为空。".into()));
        }
        self.ensure_workspace_exists(id)?;
        self.with_data(|store| store.set_workspace_secret(id, key, value))?;
        let profile = self.profile_by_id(id)?;
        self.restart_services_using_key(&[profile], key, false)
            .await;
        Ok(())
    }

    pub async fn regenerate_workspace_secret(&self, id: &str, key: &str) -> AppResult<String> {
        validate_workspace_key(key)?;
        self.ensure_workspace_exists(id)?;
        let value = self.with_data(|store| store.regenerate_workspace_secret(id, key))?;
        let profile = self.profile_by_id(id)?;
        self.restart_services_using_key(&[profile], key, false)
            .await;
        Ok(value)
    }

    pub fn shared_secret(&self, key: &str) -> AppResult<Option<String>> {
        validate_shared_key(key)?;
        self.with_data(|store| Ok(store.get_shared_secret(key)))
    }

    pub async fn set_shared_secret(&self, key: &str, value: &str) -> AppResult<()> {
        validate_shared_key(key)?;
        if value.is_empty() {
            return Err(AppError::Message("密钥不能为空。".into()));
        }
        let changed = self.with_data(|store| {
            if store.get_shared_secret(key).as_deref() == Some(value) {
                return Ok(false);
            }
            store.set_shared_secret(key, value)?;
            Ok(true)
        })?;
        if changed {
            let workspaces = self.list_workspaces()?;
            self.restart_services_using_key(&workspaces, key, true)
                .await;
        }
        Ok(())
    }

    pub async fn regenerate_shared_secret(&self, key: &str) -> AppResult<String> {
        validate_shared_key(key)?;
        let value = self.with_data(|store| store.regenerate_shared_secret(key))?;
        let workspaces = self.list_workspaces()?;
        self.restart_services_using_key(&workspaces, key, true)
            .await;
        Ok(value)
    }

    /// 只重启确实在运行、并且使用了这组密钥的服务。
    ///
    /// 密钥变更后监听器里缓存的还是旧值，不重启客户端就会一直 401。
    /// 重启在这里统一处理，调用方不需要再额外 restart。
    async fn restart_services_using_key(
        &self,
        profiles: &[WorkspaceProfile],
        key: &str,
        shared: bool,
    ) {
        for profile in profiles {
            // 改的是共享池就只重启用池子的，改的是工作区那份就只重启不用池子的：
            // 两边判断都走 reads_from_shared_pool，免得跟 `secret show` 的口径分家。
            let mcp_affected = MCP_KEYS.contains(&key)
                && reads_from_shared_pool(profile, key) == shared
                && self
                    .is_service_running(&profile.id, ServiceKind::Mcp)
                    .unwrap_or(false);
            if mcp_affected {
                if let Err(error) = self.restart_service(&profile.id, ServiceKind::Mcp).await {
                    eprintln!("密钥变更后重启 MCP 失败（{}）：{error}", profile.id);
                }
            }

            let actions_affected = ACTIONS_KEYS.contains(&key)
                && reads_from_shared_pool(profile, key) == shared
                && self
                    .is_service_running(&profile.id, ServiceKind::Actions)
                    .unwrap_or(false);
            if actions_affected {
                if let Err(error) = self
                    .restart_service(&profile.id, ServiceKind::Actions)
                    .await
                {
                    eprintln!("密钥变更后重启 Actions 失败（{}）：{error}", profile.id);
                }
            }
        }
    }
}

fn validate_workspace_key(key: &str) -> AppResult<()> {
    if WORKSPACE_SECRET_KEYS.contains(&key) {
        Ok(())
    } else {
        Err(AppError::Message(format!(
            "无效的密钥名「{key}」。可用：{}",
            WORKSPACE_SECRET_KEYS.join(", ")
        )))
    }
}

fn validate_shared_key(key: &str) -> AppResult<()> {
    if SHARED_SECRET_KEYS.contains(&key) {
        Ok(())
    } else {
        Err(AppError::Message(format!(
            "无效的共享密钥名「{key}」。可用：{}",
            SHARED_SECRET_KEYS.join(", ")
        )))
    }
}

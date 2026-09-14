//! 聚合入口（hub）的用例：配置、成员、凭据、起停。
//!
//! 路由和隔离规则在 [`crate::hub`]；这里管"改了什么要不要重启"和凭据落盘。
//!
//! 哪些改动要重启 hub：端口、认证、工具集、公网地址、凭据——它们在监听器起来时就定死了。
//! 哪些不用：增删成员、成员自己改配置——hub 每次请求都重新读，重启反而会掉客户端连接。

use serde::{Deserialize, Serialize};

use super::runtime::{ensure_port_available, wait_until_answering, READY_PROBE_BUDGET};
use super::workspace_fields::{parse_choice, MCP_AUTH_CHOICES, TOOL_PROFILE_CHOICES};
use super::{App, WorkspaceTarget};
use crate::error::{AppError, AppResult};
use crate::global_gateway;
use crate::hub::runtime::HubState;
use crate::hub::{self, HubSecrets, HUB_SCOPE, HUB_SECRET_KEYS};
use crate::logs::append_profile_log;
use crate::runtime::ServiceKind;
use crate::settings::{AppSettings, HubConfig};
use crate::workspace::WorkspaceProfile;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubMemberDto {
    pub id: String,
    pub name: String,
    pub path: String,
    pub tool_profile: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubStatusDto {
    /// running | stopped | error
    pub state: String,
    /// 给人看的下一步；跑得好好的时候是空串。
    pub detail: String,
    pub config: HubConfig,
    pub local_endpoint: String,
    /// 公网地址（带 `/mcp`）；没有公网入口时为空串。
    pub public_endpoint: String,
    pub members: Vec<HubMemberDto>,
}

/// `gld hub add` / `remove` 的结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubMembershipChange {
    /// 这次真的加进来 / 移出去的。
    pub changed: Vec<HubMemberDto>,
    /// 本来就在（add）或本来就不在（remove）的。原样报回去，免得以为命令没生效。
    pub unchanged: Vec<HubMemberDto>,
    pub status: HubStatusDto,
}

impl App {
    pub fn hub_config(&self) -> AppResult<HubConfig> {
        Ok(self.settings()?.hub)
    }

    pub async fn hub_status(&self) -> AppResult<HubStatusDto> {
        let config = self.settings()?.hub;
        let profiles = self.list_workspaces()?;
        let (state, detail) = match hub::runtime::state().await {
            HubState::Running { .. } => ("running", String::new()),
            HubState::Stopped if config.members.is_empty() => (
                "stopped",
                "还没有成员：先 gld hub add 把工作区加进来，再 gld hub start".to_string(),
            ),
            HubState::Stopped => ("stopped", "未启动：gld hub start".to_string()),
            HubState::Exited { .. } => (
                "error",
                format!(
                    "监听器意外退出，原因见 {}；修好后 gld hub start 重新拉起",
                    crate::logs::log_dir_for_profile(HUB_SCOPE)
                        .join("stderr.log")
                        .display()
                ),
            ),
        };
        // 公网地址从磁盘读：全局入口拿到临时地址后是直接写文件的，内存里这份可能还是旧的。
        let public_base = hub::public_base_url(&AppSettings::load_or_default());
        let members = config
            .members
            .iter()
            .filter_map(|id| profiles.iter().find(|profile| &profile.id == id))
            .map(member_dto)
            .collect();
        Ok(HubStatusDto {
            state: state.into(),
            detail,
            local_endpoint: format!("http://127.0.0.1:{}/mcp", config.local_port),
            public_endpoint: if public_base.is_empty() {
                String::new()
            } else {
                format!("{public_base}/mcp")
            },
            members,
            config,
        })
    }

    /// 改 hub 配置；正在跑就按新配置重启。
    ///
    /// 成员和恢复标记不在这里改，传进来的值一律忽略：命令行是"读旧配置 → 改 → 整体发回"，
    /// 两条命令并发时，认传进来的成员表就会把另一条刚加的成员冲掉。
    pub async fn set_hub_config(&self, config: HubConfig) -> AppResult<HubStatusDto> {
        if config.local_port == 0 {
            return Err(AppError::Message("端口必须在 1-65535".into()));
        }
        let auth_type = parse_choice(&config.auth_type, MCP_AUTH_CHOICES)?;
        let tool_profile = parse_choice(&config.tool_profile, TOOL_PROFILE_CHOICES)?;
        let before = self.settings()?.hub;
        if config.local_port != before.local_port {
            self.validate_hub_port(config.local_port)?;
        }
        let after = HubConfig {
            auth_type,
            tool_profile,
            public_url: config.public_url.trim().trim_end_matches('/').to_string(),
            members: before.members.clone(),
            restore_on_launch: before.restore_on_launch,
            ..config
        };
        reject_public_noauth(&after)?;
        if after != before {
            let saved = after.clone();
            self.update_settings(|settings| {
                settings.hub = saved;
                Ok(())
            })?;
            if hub::runtime::state().await != HubState::Stopped {
                self.start_hub_listener().await?;
            }
        }
        self.hub_status().await
    }

    /// 把工作区加进 hub。不重启：hub 每次请求都重新读成员表。
    pub async fn add_hub_members(
        &self,
        targets: &[WorkspaceTarget],
    ) -> AppResult<HubMembershipChange> {
        let profiles = targets
            .iter()
            .map(|target| self.resolve_workspace(target))
            .collect::<AppResult<Vec<_>>>()?;
        let mut changed = Vec::new();
        let mut unchanged = Vec::new();
        self.update_settings(|settings| {
            for profile in &profiles {
                if settings.hub.members.contains(&profile.id) {
                    unchanged.push(member_dto(profile));
                } else {
                    settings.hub.members.push(profile.id.clone());
                    changed.push(member_dto(profile));
                }
            }
            Ok(())
        })?;
        Ok(HubMembershipChange {
            changed,
            unchanged,
            status: self.hub_status().await?,
        })
    }

    /// 把工作区移出 hub。下一次调用起就访问不到，它经 hub 起的命令在那时被结束。
    pub async fn remove_hub_members(
        &self,
        targets: &[WorkspaceTarget],
    ) -> AppResult<HubMembershipChange> {
        let members = self.settings()?.hub.members;
        let mut removing = Vec::new();
        for target in targets {
            match self.resolve_workspace(target) {
                Ok(profile) => removing.push(member_dto(&profile)),
                Err(error) => {
                    // 工作区已经不在了、id 却还留在成员表里（手工改过数据文件才会这样）：
                    // 认原样的 id，否则这条悬空记录没有命令能清掉。
                    let selector = target.selector.as_deref().unwrap_or_default().trim();
                    if !members.iter().any(|id| id == selector) {
                        return Err(error);
                    }
                    removing.push(HubMemberDto {
                        id: selector.to_string(),
                        name: String::new(),
                        path: String::new(),
                        tool_profile: String::new(),
                    });
                }
            }
        }
        let mut changed = Vec::new();
        let mut unchanged = Vec::new();
        self.update_settings(|settings| {
            for member in removing {
                if settings.hub.members.contains(&member.id) {
                    settings.hub.members.retain(|id| id != &member.id);
                    changed.push(member);
                } else {
                    unchanged.push(member);
                }
            }
            Ok(())
        })?;
        Ok(HubMembershipChange {
            changed,
            unchanged,
            status: self.hub_status().await?,
        })
    }

    /// 启动（已在跑就按当前配置重启），并记住下次守护进程启动时恢复。
    pub async fn start_hub(&self) -> AppResult<HubStatusDto> {
        self.start_hub_listener().await?;
        self.update_settings(|settings| {
            settings.hub.restore_on_launch = true;
            Ok(())
        })?;
        self.hub_status().await
    }

    pub async fn stop_hub(&self) -> AppResult<HubStatusDto> {
        hub::runtime::stop().await;
        self.update_settings(|settings| {
            settings.hub.restore_on_launch = false;
            Ok(())
        })?;
        self.hub_status().await
    }

    /// 取 hub 的一项凭据；还没生成过就当场生成。
    pub fn hub_secret(&self, key: &str) -> AppResult<String> {
        validate_hub_key(key)?;
        self.with_data(|store| store.get_or_create_app_secret(HUB_SCOPE, key))
    }

    /// 重新生成一项凭据；hub 在跑就重启，否则监听器里还是旧值。
    pub async fn regenerate_hub_secret(&self, key: &str) -> AppResult<String> {
        validate_hub_key(key)?;
        let value = self.with_data(|store| store.regenerate_app_secret(HUB_SCOPE, key))?;
        if hub::runtime::state().await != HubState::Stopped {
            self.start_hub_listener().await?;
        }
        Ok(value)
    }

    /// 按当前配置（重）起监听器，不动恢复标记。守护进程启动恢复时也走这里。
    pub(super) async fn start_hub_listener(&self) -> AppResult<()> {
        let settings = self.settings()?;
        let config = settings.hub.clone();
        self.validate_hub_port(config.local_port)?;
        // 保存时已经拦过一道；这里再拦是防手工改过数据文件、或老配置恢复起来。
        reject_public_noauth(&config)?;
        if config.use_global_gateway {
            if !settings.global_gateway.enabled {
                return Err(AppError::Message(
                    "hub 设了经全局入口暴露，但全局入口没启用。\
                     先配好并启用它（gld gateway set --enabled true），\
                     或改回直连：gld hub set --global-gateway false"
                        .into(),
                ));
            }
            global_gateway::ensure_started().await?;
        }
        let secrets = self.with_data(|store| {
            Ok(HubSecrets {
                bearer_token: store.get_or_create_app_secret(HUB_SCOPE, "bearer_token")?,
                oauth_client_id: store.get_or_create_app_secret(HUB_SCOPE, "oauth_client_id")?,
                oauth_password: store.get_or_create_app_secret(HUB_SCOPE, "oauth_password")?,
                oauth_token_secret: store
                    .get_or_create_app_secret(HUB_SCOPE, "oauth_token_secret")?,
            })
        })?;

        hub::runtime::stop().await;
        ensure_port_available(config.local_port, "聚合入口 ").await?;
        let public_base = hub::public_base_url(&AppSettings::load_or_default());
        hub::runtime::start(&config, public_base, secrets).await?;
        // 和工作区服务一样，报 running 之前确认它真的开始应答，理由见 wait_until_answering。
        if !wait_until_answering(config.local_port, ServiceKind::Mcp, READY_PROBE_BUDGET).await {
            append_profile_log(
                HUB_SCOPE,
                "stderr.log",
                &format!(
                    "[hub] 端口 {} 已绑定，但 {} 秒内没有应答就绪探测；\
                     start 照常返回，紧接着的第一个请求可能会失败",
                    config.local_port,
                    READY_PROBE_BUDGET.as_secs()
                ),
            );
        }
        Ok(())
    }

    /// 端口已经分给了别的 gld 服务就当场拒。
    ///
    /// 不拦的话要等到两边都启动时才撞，而且报错里只有"被本进程占用"，看不出是谁。
    fn validate_hub_port(&self, port: u16) -> AppResult<()> {
        if self.settings()?.global_gateway.local_port == port {
            return Err(AppError::Message(format!(
                "端口 {port} 已经是全局入口的本地端口，给 hub 换一个：gld hub set --port <端口>"
            )));
        }
        if let Some(profile) = self.list_workspaces()?.iter().find(|profile| {
            profile.runtime.local_port == port || profile.actions.local_port == port
        }) {
            return Err(AppError::Message(format!(
                "端口 {port} 已经分给了工作区「{}」，给 hub 换一个：gld hub set --port <端口>",
                profile.name
            )));
        }
        Ok(())
    }
}

/// hub 挂了公网入口就不许 noauth。
///
/// 工作区那边 noauth + 公网只是 `gld doctor` 报 ✗，这里直接拒：hub 的暴露面是全部成员，
/// 一个无认证的公网地址等于把这几个项目的"执行任意命令"一起开放给整个互联网，
/// 而且谁连上来先调 list_workspaces 就知道有哪几个。
fn reject_public_noauth(config: &HubConfig) -> AppResult<()> {
    let public = config.use_global_gateway || !config.public_url.trim().is_empty();
    if config.auth_type == "noauth" && public {
        return Err(AppError::Message(
            "hub 挂了公网入口（经全局入口或手动公网地址），不能用 noauth：\
             那等于把全部成员的执行权限开放给整个互联网。\
             改用认证：gld hub set --auth oauth；确实只在本机用就先撤掉公网入口再改 noauth。"
                .into(),
        ));
    }
    Ok(())
}

fn member_dto(profile: &WorkspaceProfile) -> HubMemberDto {
    HubMemberDto {
        id: profile.id.clone(),
        name: profile.name.clone(),
        path: profile.path.clone(),
        tool_profile: crate::tools::registry::normalize_tool_profile(&profile.runtime.tool_profile)
            .to_string(),
    }
}

fn validate_hub_key(key: &str) -> AppResult<()> {
    if HUB_SECRET_KEYS.contains(&key) {
        Ok(())
    } else {
        Err(AppError::Message(format!(
            "无效的 hub 凭据名「{key}」。可用：{}",
            HUB_SECRET_KEYS.join(", ")
        )))
    }
}

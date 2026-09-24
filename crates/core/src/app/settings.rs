use serde::{Deserialize, Serialize};

use super::App;
use crate::error::{AppError, AppResult};
use crate::runtime::ServiceKind;
use crate::settings::{FrpProfile, ProxyConfig};

/// FRP 服务器配置（不含 token 明文，只报告是否已设置）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrpProfileDto {
    pub id: String,
    pub name: String,
    pub server: String,
    pub server_port: u16,
    pub has_token: bool,
}

/// 全局运行时设置（影响所有工作区）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalRuntimeSettingsDto {
    #[serde(default)]
    pub executable_paths: String,
    #[serde(default)]
    pub ai_instructions: String,
    #[serde(default)]
    pub instruction_sources: Vec<String>,
    #[serde(default)]
    pub skill_sources: Vec<String>,
    #[serde(default)]
    pub custom_instruction_paths: String,
    #[serde(default)]
    pub custom_skill_paths: String,
    #[serde(default)]
    pub hidden_skills: Vec<String>,
    #[serde(default)]
    pub allow_lan_access: bool,
    #[serde(default)]
    pub restore_runtime_state_on_launch: bool,
}

impl App {
    // ---- FRP 服务器配置 -------------------------------------------------

    pub fn list_frp_profiles(&self) -> AppResult<Vec<FrpProfileDto>> {
        self.with_data(|store| {
            Ok(store
                .data()
                .frp_profiles
                .iter()
                .map(|profile| FrpProfileDto {
                    id: profile.id.clone(),
                    name: profile.name.clone(),
                    server: profile.server.clone(),
                    server_port: profile.server_port,
                    has_token: store
                        .get_app_secret("frp_profile_token", &profile.id)
                        .is_some_and(|value| !value.trim().is_empty()),
                })
                .collect())
        })
    }

    /// 新建或更新（id 非空且已存在时）一个 FRP 服务器配置。
    pub fn save_frp_profile(
        &self,
        profile: FrpProfile,
        token: Option<String>,
    ) -> AppResult<FrpProfileDto> {
        if profile.name.trim().is_empty() || profile.server.trim().is_empty() {
            return Err(AppError::Message("FRP 配置名称和服务器不能为空。".into()));
        }
        let mut saved = profile;
        saved.name = saved.name.trim().to_string();
        saved.server = saved.server.trim().to_string();
        if saved.id.trim().is_empty() {
            saved.id = uuid::Uuid::new_v4().to_string().replace('-', "");
        }

        self.with_data(|store| {
            let mut settings = store.settings();
            if let Some(existing) = settings
                .frp_profiles
                .iter_mut()
                .find(|item| item.id == saved.id)
            {
                *existing = saved.clone();
            } else {
                settings.frp_profiles.push(saved.clone());
            }
            store.update_settings(settings)?;
            if let Some(token) = token.filter(|value| !value.trim().is_empty()) {
                store.set_app_secret("frp_profile_token", &saved.id, token.trim())?;
            }
            let has_token = store
                .get_app_secret("frp_profile_token", &saved.id)
                .is_some_and(|value| !value.trim().is_empty());
            Ok(FrpProfileDto {
                id: saved.id.clone(),
                name: saved.name.clone(),
                server: saved.server.clone(),
                server_port: saved.server_port,
                has_token,
            })
        })
    }

    /// 删除一个 FRP 服务器配置。
    ///
    /// 还有人指着它时默认拒绝（服务的公网入口、项目的 GPT Actions、全局入口）：
    /// 删掉之后它们不会有任何变化，直到某次 `gld start` 报「引用的 FRP 配置 xxx
    /// 不存在」——那时手里只剩一个 id，已经查不出它原来是哪台服务器了。确实要删就加
    /// `force`，报错里会告诉你有哪些地方受影响。
    pub fn delete_frp_profile(&self, id: &str, force: bool) -> AppResult<()> {
        self.with_data(|store| {
            let mut settings = store.settings();
            let Some(position) = settings.frp_profiles.iter().position(|item| item.id == id) else {
                return Err(AppError::Message(format!("FRP 配置不存在：{id}")));
            };
            if !force {
                let mut users = frp_profile_users(store.list(), id);
                if settings.hub.tunnel_type == "frp" && settings.hub.frp_profile_id == id {
                    users.insert(0, "  MCP 服务的公网入口（gld ls）".to_string());
                }
                if settings.global_gateway.frp_profile_id == id {
                    users.push("  全局入口（gld gateway ls）".to_string());
                }
                if !users.is_empty() {
                    let name = settings.frp_profiles[position].name.clone();
                    return Err(AppError::Message(format!(
                        "FRP 配置「{name}」还在被这些地方用着：\n{}\n\
                         先把它们改到别的配置（服务：gld share --tunnel frp:<名称>；\
                         项目的 GPT Actions：gld set <项目> actions.frp-profile=<名称>）\
                         或收回公网入口（gld share --off）；确定要留下悬空引用就加 --force。",
                        users.join("\n")
                    )));
                }
            }
            settings.frp_profiles.remove(position);
            store.update_settings(settings)?;
            store.delete_app_secret("frp_profile_token", id)
        })
    }

    // ---- 代理与下载 -----------------------------------------------------

    pub fn proxy(&self) -> AppResult<ProxyConfig> {
        Ok(self.settings()?.proxy)
    }

    pub fn set_proxy(&self, proxy: ProxyConfig) -> AppResult<()> {
        validate_proxy_mode(&proxy.mode, &proxy.url)?;
        self.update_settings(|settings| {
            settings.proxy = proxy;
            Ok(())
        })
    }

    // ---- 全局运行时设置 -------------------------------------------------

    pub fn global_runtime_settings(&self) -> AppResult<GlobalRuntimeSettingsDto> {
        let settings = self.settings()?;
        Ok(GlobalRuntimeSettingsDto {
            executable_paths: settings.global_executable_paths,
            ai_instructions: settings.global_ai_instructions,
            instruction_sources: settings.global_instruction_sources,
            skill_sources: settings.global_skill_sources,
            custom_instruction_paths: settings.global_custom_instruction_paths,
            custom_skill_paths: settings.global_custom_skill_paths,
            hidden_skills: settings.global_hidden_skills,
            allow_lan_access: settings.allow_lan_access,
            restore_runtime_state_on_launch: settings.restore_runtime_state_on_launch,
        })
    }

    pub fn set_global_runtime_settings(&self, runtime: GlobalRuntimeSettingsDto) -> AppResult<()> {
        // 刚打开“启动时恢复”时，把当前正在跑的服务当作恢复清单的初始值。
        let should_capture_running = runtime.restore_runtime_state_on_launch
            && !self.settings()?.restore_runtime_state_on_launch;
        let running_snapshot = if should_capture_running {
            Some(self.with_runtime(|supervisor| {
                Ok((
                    supervisor.running_workspace_ids(ServiceKind::Mcp),
                    supervisor.running_workspace_ids(ServiceKind::Actions),
                ))
            })?)
        } else {
            None
        };

        self.update_settings(|settings| {
            settings.global_executable_paths = runtime.executable_paths.trim().to_string();
            settings.global_ai_instructions = runtime.ai_instructions.trim().to_string();
            settings.global_instruction_sources = runtime.instruction_sources;
            settings.global_skill_sources = runtime.skill_sources;
            settings.global_custom_instruction_paths =
                runtime.custom_instruction_paths.trim().to_string();
            settings.global_custom_skill_paths = runtime.custom_skill_paths.trim().to_string();
            settings.global_hidden_skills = runtime.hidden_skills;
            settings.allow_lan_access = runtime.allow_lan_access;
            settings.restore_runtime_state_on_launch = runtime.restore_runtime_state_on_launch;
            if let Some((mcp_ids, actions_ids)) = running_snapshot {
                settings.restore_mcp_workspace_ids = mcp_ids;
                settings.restore_actions_workspace_ids = actions_ids;
            }
            Ok(())
        })?;
        // 可执行路径、全局说明、Skill 来源都进工具上下文，而 `gld tool call` 的上下文按项目缓存
        // 在守护进程里。不清的话新值要等守护进程重启才用上：配了路径还是 Program not found。
        // 清掉不影响正在跑的命令，它们在按目录的执行资源里，不在上下文里。
        self.with_tool_contexts(|contexts| {
            contexts.clear();
            Ok(())
        })
    }
}

fn validate_proxy_mode(mode: &str, url: &str) -> AppResult<()> {
    match mode.trim() {
        "none" | "system" => Ok(()),
        "manual" => {
            if url.trim().is_empty() {
                Err(AppError::Message(
                    "代理模式为 manual 时必须填写代理地址。".into(),
                ))
            } else {
                Ok(())
            }
        }
        other => Err(AppError::Message(format!(
            "代理模式无效：{other}（可选 none | system | manual）"
        ))),
    }
}

/// 哪些工作区的哪条线路正引用着这个 FRP 配置，写成可读的一行一条。
///
/// 全局入口（gateway）不在这里查：它的配置在 settings 里，由调用方另外判断。
/// 哪些项目的 GPT Actions 指着这个 FRP 配置。
///
/// 项目自己那个 MCP 服务的引用不算（RFC-0004 之后没有它了，那个字段也改不了）：
/// 算进来的话，删配置会被一个谁也改不动的引用挡住。
fn frp_profile_users(profiles: &[crate::workspace::WorkspaceProfile], id: &str) -> Vec<String> {
    profiles
        .iter()
        .filter(|profile| profile.actions.frp_profile_id == id)
        .map(|profile| format!("  项目「{}」的 GPT Actions", profile.name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::WorkspaceProfile;

    fn workspace(name: &str, mcp: &str, actions: &str) -> WorkspaceProfile {
        let mut profile = WorkspaceProfile::new("/tmp/x".into(), Some(name.into()));
        profile.tunnel.frp_profile_id = mcp.into();
        profile.actions.frp_profile_id = actions.into();
        profile
    }

    #[test]
    fn frp_profile_users_counts_only_the_actions_lines() {
        let profiles = vec![
            workspace("api", "p1", ""),
            workspace("web", "p2", "p1"),
            workspace("both", "p1", "p1"),
        ];
        let users = frp_profile_users(&profiles, "p1");
        // 项目自己那个 MCP 服务的引用不算：它没有了，那个字段也改不动。
        assert_eq!(users.len(), 2, "{users:#?}");
        assert!(users[0].contains("web") && users[0].contains("Actions"));
        assert!(users[1].contains("both"));
        assert!(frp_profile_users(&profiles, "p3").is_empty());
    }
}

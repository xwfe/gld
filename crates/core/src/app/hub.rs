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
use crate::bridge::member::{CcnmMember, Mode};
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
    /// `local` = 本机的一个工作区目录，`remote` = 另一台机器上由 ccnm 管着的
    /// workspace。两者的工具、能力和配置都不一样，见 [`crate::hub`]。
    pub kind: String,
    /// 本机根目录。**远端成员是空串**——那是对面机器上的路径，gld 不知道，
    /// 编一个比留空更糟（RFC-0002 5.1）。
    pub path: String,
    /// 本地成员的工具集；远端成员是空串（它的工具由访问模式决定）。
    pub tool_profile: String,
    /// 远端成员：ccnm 配置里的 node 别名。本地成员是空串。
    #[serde(default)]
    pub node: String,
    /// 远端成员：ccnm 配置里的 workspace 名。本地成员是空串。
    #[serde(default)]
    pub workspace: String,
    /// 远端成员的访问上限：`read` 或 `coding`。本地成员是空串。
    #[serde(default)]
    pub mode: String,
}

/// `gld hub remote add` 收到的参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CcnmMemberSpec {
    /// 给人看的名字，也是调用时 `workspace` 参数可以用的值。
    pub name: String,
    /// ccnm 配置里的 node 别名（一台机器的名字），不是 host 也不是 user。
    pub node: String,
    /// ccnm 配置里的 workspace 名，不是路径。
    pub workspace: String,
    /// 本机 `ccnm` 可执行程序；空串表示用 PATH 里的 `ccnm`。
    #[serde(default)]
    pub ccnm_bin: String,
    /// 访问上限：`read`（默认）或 `coding`。
    #[serde(default)]
    pub mode: String,
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
        let remotes = self.ccnm_members()?;
        // 按加入顺序，本地和远端混在一条名单里——hub 路由看到的就是这一份。
        let members = config
            .members
            .iter()
            .filter_map(|id| {
                profiles
                    .iter()
                    .find(|profile| &profile.id == id)
                    .map(member_dto)
                    .or_else(|| {
                        remotes
                            .iter()
                            .find(|remote| &remote.id == id)
                            .map(remote_dto)
                    })
            })
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

    /// 当前登记的远端 ccnm 成员（不管在不在 hub 里）。
    pub fn ccnm_members(&self) -> AppResult<Vec<CcnmMember>> {
        self.with_data(|store| Ok(store.ccnm_members().to_vec()))
    }

    /// 登记一个远端 ccnm workspace 并加进 hub。
    ///
    /// 字段全部由操作员给：node 和 workspace 都是 **ccnm 那边配置里的名字**，
    /// 不是 host、不是路径。gld 不解析 SSH 凭据，也不知道对面的根目录在哪
    /// （RFC-0002 5.1）。
    pub async fn add_ccnm_member(&self, spec: CcnmMemberSpec) -> AppResult<HubMembershipChange> {
        let member = build_ccnm_member(spec)?;
        let existing = self.ccnm_members()?;
        if let Some(clash) = existing
            .iter()
            .find(|item| item.name.eq_ignore_ascii_case(&member.name))
        {
            return Err(AppError::Message(format!(
                "已经有一个叫「{}」的远端成员了（id {}）。换个名字，或者先 gld hub remote rm {} 再加。",
                clash.name,
                crate::short_id(&clash.id),
                clash.name
            )));
        }
        // 和本地工作区重名也不行：调用时 `workspace` 参数按名字找，重名会报
        // WORKSPACE_AMBIGUOUS，模型只能改用 id——那就白起名字了。
        if let Some(clash) = self
            .list_workspaces()?
            .iter()
            .find(|profile| profile.name.eq_ignore_ascii_case(&member.name))
        {
            return Err(AppError::Message(format!(
                "「{}」已经是本地工作区的名字了。远端成员换一个名字，不然调用时按名字分不清是哪个。",
                clash.name
            )));
        }
        let dto = remote_dto(&member);
        let id = member.id.clone();
        self.with_data(|store| store.upsert_ccnm_member(member))?;
        self.update_settings(|settings| {
            if !settings.hub.members.contains(&id) {
                settings.hub.members.push(id);
            }
            Ok(())
        })?;
        Ok(HubMembershipChange {
            changed: vec![dto],
            unchanged: Vec::new(),
            status: self.hub_status().await?,
        })
    }

    /// 删掉一个远端成员：从 hub 名单和登记表里一起去掉。
    ///
    /// 和本地成员不同，本地的"移出 hub"只是不再暴露，工作区本身还在；远端
    /// 成员除了这份配置没有别的东西，所以是真的删掉。
    pub async fn remove_ccnm_member(&self, selector: &str) -> AppResult<HubMembershipChange> {
        let selector = selector.trim();
        let members = self.ccnm_members()?;
        let found = members
            .iter()
            .find(|item| item.id == selector)
            .or_else(|| {
                members
                    .iter()
                    .find(|item| item.name.eq_ignore_ascii_case(selector))
            })
            .or_else(|| {
                (selector.len() >= 4)
                    .then(|| members.iter().find(|item| item.id.starts_with(selector)))
                    .flatten()
            });
        let Some(member) = found.cloned() else {
            return Err(AppError::Message(format!(
                "没有叫「{selector}」的远端成员。看看有哪些：gld hub show"
            )));
        };
        let dto = remote_dto(&member);
        let id = member.id.clone();
        self.update_settings(|settings| {
            settings.hub.members.retain(|item| item != &id);
            Ok(())
        })?;
        self.with_data(|store| store.remove_ccnm_member(&id))?;
        Ok(HubMembershipChange {
            changed: vec![dto],
            unchanged: Vec::new(),
            status: self.hub_status().await?,
        })
    }

    /// 把工作区移出 hub。下一次调用起就访问不到，它经 hub 起的命令在那时被结束。
    pub async fn remove_hub_members(
        &self,
        targets: &[WorkspaceTarget],
    ) -> AppResult<HubMembershipChange> {
        let members = self.settings()?.hub.members;
        let remotes = self.ccnm_members()?;
        let mut removing = Vec::new();
        for target in targets {
            let selector = target.selector.as_deref().unwrap_or_default().trim();
            // 远端成员用 gld hub remote rm，这条命令只管本地——不然"移出 hub"
            // 会让那条登记记录悬着，没有命令能清掉它。
            if remotes
                .iter()
                .any(|remote| remote.id == selector || remote.name.eq_ignore_ascii_case(selector))
            {
                return Err(AppError::Message(format!(
                    "「{selector}」是远端 ccnm 成员，用 gld hub remote rm {selector} 删它。"
                )));
            }
            match self.resolve_workspace(target) {
                Ok(profile) => removing.push(member_dto(&profile)),
                Err(error) => {
                    // 工作区已经不在了、id 却还留在成员表里（手工改过数据文件才会这样）：
                    // 认原样的 id，否则这条悬空记录没有命令能清掉。
                    if !members.iter().any(|id| id == selector) {
                        return Err(error);
                    }
                    removing.push(HubMemberDto {
                        id: selector.to_string(),
                        name: String::new(),
                        kind: "local".into(),
                        path: String::new(),
                        tool_profile: String::new(),
                        node: String::new(),
                        workspace: String::new(),
                        mode: String::new(),
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
        kind: "local".into(),
        path: profile.path.clone(),
        tool_profile: crate::tools::registry::normalize_tool_profile(&profile.runtime.tool_profile)
            .to_string(),
        node: String::new(),
        workspace: String::new(),
        mode: String::new(),
    }
}

fn remote_dto(member: &CcnmMember) -> HubMemberDto {
    HubMemberDto {
        id: member.id.clone(),
        name: member.name.clone(),
        kind: "remote".into(),
        path: String::new(),
        tool_profile: String::new(),
        node: member.node.clone(),
        workspace: member.workspace.clone(),
        mode: member.max_mode.as_str().to_string(),
    }
}

/// 把命令行给的参数变成一条成员记录，顺手把能当场发现的错挡掉。
///
/// 都是"填错了会在很久以后才炸"的那种：名字空着的话，调用时按名字找不到，
/// 只能用 id；node 或 workspace 填成路径的话，要等真的去连 bridge 才报错，
/// 而那时的报错来自 ccnm，看着像网络问题。
fn build_ccnm_member(spec: CcnmMemberSpec) -> AppResult<CcnmMember> {
    let name = spec.name.trim();
    let node = spec.node.trim();
    let workspace = spec.workspace.trim();
    for (label, value) in [("名字", name), ("--node", node), ("--workspace", workspace)] {
        if value.is_empty() {
            return Err(AppError::Message(format!("{label} 不能是空的")));
        }
    }
    for (label, value) in [("--node", node), ("--workspace", workspace)] {
        if value.contains('/') || value.contains('\\') {
            return Err(AppError::Message(format!(
                "{label} 要填 ccnm 配置里的名字，不是路径（给的是「{value}」）。\
                 在那台机器上跑 ccnm workspace list 看有哪些。"
            )));
        }
    }
    let max_mode = match spec.mode.trim() {
        "" | "read" => Mode::Read,
        "coding" => Mode::Coding,
        other => {
            return Err(AppError::Message(format!(
                "访问模式只能是 read 或 coding，给的是「{other}」"
            )))
        }
    };
    let ccnm_bin = match spec.ccnm_bin.trim() {
        "" => "ccnm".to_string(),
        path => path.to_string(),
    };
    Ok(CcnmMember {
        // 和本地工作区一个格式：32 位十六进制。这样 hub 那边按 id 前缀
        // （≥4 位）找成员的规则对两种成员完全一样。
        id: uuid::Uuid::new_v4().to_string().replace('-', ""),
        name: name.to_string(),
        ccnm_bin,
        node: node.to_string(),
        workspace: workspace.to_string(),
        max_mode,
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> CcnmMemberSpec {
        CcnmMemberSpec {
            name: "prod".into(),
            node: "work".into(),
            workspace: "server".into(),
            ccnm_bin: String::new(),
            mode: String::new(),
        }
    }

    #[test]
    fn a_member_defaults_to_read_only_and_ccnm_on_path() {
        let member = build_ccnm_member(spec()).expect("建成员");
        assert_eq!(member.max_mode, Mode::Read, "默认必须是只读");
        assert_eq!(member.ccnm_bin, "ccnm");
        assert_eq!(
            member.id.len(),
            32,
            "id 要和本地工作区一个格式：{}",
            member.id
        );
        assert!(member.id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// 最常见的填错：把 --node / --remote-workspace 当成路径填。
    /// 不当场拦的话，要等第一次调用连 bridge 才炸，那时的报错来自 ccnm，
    /// 看着像网络问题。
    #[test]
    fn a_path_where_a_ccnm_name_belongs_is_refused_right_away() {
        for (field, value) in [("node", "/Users/bing/code"), ("workspace", "~/code/api")] {
            let mut spec = spec();
            match field {
                "node" => spec.node = value.into(),
                _ => spec.workspace = value.into(),
            }
            let error = build_ccnm_member(spec).expect_err("该拒绝");
            let text = error.to_string();
            assert!(text.contains("不是路径"), "{field}: {text}");
            assert!(text.contains("ccnm workspace list"), "得说去哪儿查：{text}");
        }
    }

    #[test]
    fn empty_fields_are_named_one_by_one() {
        for (label, mut spec) in [
            (
                "名字",
                CcnmMemberSpec {
                    name: "  ".into(),
                    ..spec()
                },
            ),
            (
                "--node",
                CcnmMemberSpec {
                    node: String::new(),
                    ..spec()
                },
            ),
            (
                "--remote-workspace",
                CcnmMemberSpec {
                    workspace: String::new(),
                    ..spec()
                },
            ),
        ] {
            spec.mode = String::new();
            let error = build_ccnm_member(spec).expect_err("该拒绝").to_string();
            // --remote-workspace 在 app 层叫 --workspace，报错里认前缀就行。
            let expected = label.trim_start_matches("--remote-");
            assert!(error.contains(expected), "{label}: {error}");
        }
    }

    #[test]
    fn an_unknown_mode_is_refused_instead_of_falling_back_to_read() {
        let error = build_ccnm_member(CcnmMemberSpec {
            mode: "admin".into(),
            ..spec()
        })
        .expect_err("该拒绝")
        .to_string();
        assert!(error.contains("admin"), "{error}");
        assert!(
            error.contains("read") && error.contains("coding"),
            "{error}"
        );
    }

    #[test]
    fn coding_is_accepted_as_a_ceiling_even_though_it_is_not_implemented_yet() {
        let member = build_ccnm_member(CcnmMemberSpec {
            mode: "coding".into(),
            ..spec()
        })
        .expect("建成员");
        assert_eq!(member.max_mode, Mode::Coding);
    }

    /// 远端成员在名单里的样子：没有本机路径，有它在对面的位置。
    #[test]
    fn the_remote_row_has_no_local_path() {
        let member = build_ccnm_member(spec()).expect("建成员");
        let dto = remote_dto(&member);
        assert_eq!(dto.kind, "remote");
        assert!(dto.path.is_empty(), "远端不该有本机路径");
        assert!(dto.tool_profile.is_empty());
        assert_eq!(dto.node, "work");
        assert_eq!(dto.workspace, "server");
        assert_eq!(dto.mode, "read");
    }
}

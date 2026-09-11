use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::App;
use crate::error::{AppError, AppResult};
use crate::logs::append_profile_log;
use crate::runtime::ServiceKind;
use crate::tunnel::drop_workspace as drop_tunnel_workspace;
use crate::workspace::resources::{
    assign_free_workspace_ports, validate_workspace_resources, validate_workspace_resources_update,
};
use crate::workspace::WorkspaceProfile;

/// 调用方对“哪个工作区”的描述。
///
/// - `selector`：用户显式给出的 id / id 前缀 / 名称 / 路径；
/// - `cwd`：调用方所在目录，用于在没有 selector 时按目录归属推断。
///
/// 解析规则见 [`App::resolve_workspace`]。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceTarget {
    #[serde(default)]
    pub selector: Option<String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
}

impl WorkspaceTarget {
    pub fn selector(selector: impl Into<String>) -> Self {
        Self {
            selector: Some(selector.into()),
            cwd: None,
        }
    }

    pub fn new(selector: Option<String>, cwd: Option<PathBuf>) -> Self {
        Self { selector, cwd }
    }
}

/// [`App::ensure_workspace`] 的结果：目录对应的工作区，以及它是不是刚建出来的。
///
/// `created` 是给调用方提示用的——自动登记必须让人看见，否则在一个随手进的
/// 目录里敲 `gld start`，工作区就悄悄多了一个，而用户以为自己启动的是别的项目。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnsuredWorkspace {
    pub profile: WorkspaceProfile,
    pub created: bool,
}

/// 创建工作区时可选的覆盖项。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceCreateOptions {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub mcp_port: Option<u16>,
    #[serde(default)]
    pub actions_port: Option<u16>,
}

/// `ws set` 的结果：改完的配置，以及为了让它生效做了什么。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceUpdate {
    pub profile: WorkspaceProfile,
    /// 已经重启并跑起来的服务。
    pub restarted: Vec<ServiceKind>,
    /// 重启失败的服务——配置已经存下了，但这条服务此刻是停的。
    pub restart_failures: Vec<RestartFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestartFailure {
    pub service: ServiceKind,
    pub error: String,
}

impl App {
    pub fn list_workspaces(&self) -> AppResult<Vec<WorkspaceProfile>> {
        self.with_data(|store| Ok(store.list().to_vec()))
    }

    /// 解析目标工作区；找不到或有歧义时返回带候选列表的错误。
    pub fn resolve_workspace(&self, target: &WorkspaceTarget) -> AppResult<WorkspaceProfile> {
        let profiles = self.list_workspaces()?;
        if let Some(selector) = target.selector.as_deref().map(str::trim) {
            if !selector.is_empty() {
                return resolve_by_selector(&profiles, selector, target.cwd.as_deref());
            }
        }
        if let Some(cwd) = target.cwd.as_deref() {
            if let Ok(profile) = resolve_by_dir(&profiles, cwd) {
                return Ok(profile);
            }
        }
        // 只配置了一个工作区时没有歧义，直接用它，单项目用户不必每次都写 -w。
        if let [only] = profiles.as_slice() {
            return Ok(only.clone());
        }
        if profiles.is_empty() {
            return Err(AppError::Message(
                "还没有任何工作区。先执行 `gld workspace add <项目目录>`。".into(),
            ));
        }
        let cwd_hint = target
            .cwd
            .as_deref()
            .map(|cwd| format!("当前目录 {} 不属于任何工作区。", cwd.display()))
            .unwrap_or_default();
        Err(AppError::Message(format!(
            "{cwd_hint}请用 --workspace/-w 指定 id、名称或路径（`gld workspace list` 查看）。"
        )))
    }

    pub fn create_workspace(
        &self,
        path: &Path,
        options: WorkspaceCreateOptions,
    ) -> AppResult<WorkspaceProfile> {
        let root = normalize_workspace_root(path)?;
        self.with_data(|store| {
            if let Some(existing) = store
                .list()
                .iter()
                .find(|profile| same_path(&profile.path, &root))
            {
                return Err(AppError::Message(format!(
                    "该目录已经是工作区「{}」（id {}）",
                    existing.name,
                    crate::short_id(&existing.id)
                )));
            }
            let mut profile =
                WorkspaceProfile::new(root.to_string_lossy().into_owned(), options.name.clone());
            // 命令行没有向导引导填 FRP 服务器，默认不开隧道；否则每次 start 都会报
            // “FRP 模式需要选择全局配置”。需要公网时用 gld ws set mcp.tunnel=frp 打开。
            profile.tunnel.tunnel_type = "none".into();
            profile.actions.tunnel_type = "none".into();
            // 创建不该因为默认端口被占而失败：先挑空闲端口，再套用显式覆盖并校验冲突。
            assign_free_workspace_ports(store.list(), &mut profile)?;
            if let Some(port) = options.mcp_port {
                profile.runtime.local_port = port;
            }
            if let Some(port) = options.actions_port {
                profile.actions.local_port = port;
            }
            if options.mcp_port.is_some() || options.actions_port.is_some() {
                validate_workspace_resources(store.list(), &profile)?;
            }
            store.init_workspace_secrets(&profile.id)?;
            store.add(profile.clone())?;
            Ok(profile)
        })
    }

    /// 按目录拿工作区：这个目录（或它的上层）已经登记过就返回它，否则当场登记。
    ///
    /// `gld start ~/code/x` 走这条路。以前必须先 `workspace add` 再 `start`，
    /// 两条命令之间没有任何判断——第一条的唯一作用就是让第二条别报
    /// "当前目录不属于任何工作区"。
    ///
    /// 归属判断用的是和 [`Self::resolve_workspace`] 同一套目录规则（在
    /// `~/code/x/src/` 里也算 `~/code/x`），但**不带**"只有一个工作区就用它"
    /// 那条回退：在一个全新的目录里敲 `gld start`，要的是这个目录，
    /// 而不是碰巧唯一的那个别的项目。
    pub fn ensure_workspace(
        &self,
        path: &Path,
        options: WorkspaceCreateOptions,
    ) -> AppResult<EnsuredWorkspace> {
        let root = normalize_workspace_root(path)?;
        if let Ok(existing) = resolve_by_dir(&self.list_workspaces()?, &root) {
            return Ok(EnsuredWorkspace {
                profile: existing,
                created: false,
            });
        }
        Ok(EnsuredWorkspace {
            profile: self.create_workspace(&root, options)?,
            created: true,
        })
    }

    /// 按 `key=value` 修改若干字段后保存；任一字段非法则整体不写。
    ///
    /// 字段表见 [`super::workspace_field_catalog`]。
    ///
    /// 改完会重启受影响、且此刻正在运行的服务，调用方不需要再 restart 一次。
    /// 这和 `gld secret set` 是同一套行为：以前只有密钥会自动重启，改配置要
    /// 自己记得 `gld restart`，同一个心智两套规矩，而"忘了重启"的表现是
    /// 改了没反应——看不出跟没重启有关。
    ///
    /// 只重启配置真的变了的那一侧：`mcp.*` 只动 MCP，`actions.*` 只动 Actions，
    /// 值没变（`auth=oauth` 设成本来就是 oauth）则一个都不重启，免得白白掐断客户端。
    pub async fn set_workspace_fields(
        &self,
        id: &str,
        assignments: &[(String, String)],
    ) -> AppResult<WorkspaceUpdate> {
        let before = self.profile_by_id(id)?;
        let mut profile = before.clone();
        let ctx = self.field_context()?;
        for (key, value) in assignments {
            super::workspace_fields::apply_workspace_field(&mut profile, key, value, &ctx)?;
        }
        let saved = self.update_workspace(profile)?;

        let mut restarted = Vec::new();
        let mut restart_failures = Vec::new();
        for kind in [ServiceKind::Mcp, ServiceKind::Actions] {
            if !service_config_changed(&before, &saved, kind) {
                continue;
            }
            if !self.is_service_running(id, kind).unwrap_or(false) {
                continue;
            }
            match self.restart_service(id, kind).await {
                Ok(_) => restarted.push(kind),
                Err(error) => restart_failures.push(RestartFailure {
                    service: kind,
                    error: error.to_string(),
                }),
            }
        }
        Ok(WorkspaceUpdate {
            profile: saved,
            restarted,
            restart_failures,
        })
    }

    /// 校验字段取值时要看的全局数据（目前只有 FRP 配置表）。
    fn field_context(&self) -> AppResult<super::workspace_fields::FieldContext> {
        Ok(super::workspace_fields::FieldContext {
            frp_profiles: self.settings()?.frp_profiles,
        })
    }

    /// 整体替换一个工作区配置（端口 / 子域名冲突会被拒绝）。
    pub fn update_workspace(&self, profile: WorkspaceProfile) -> AppResult<WorkspaceProfile> {
        let saved = self.with_data(|store| {
            let current = store
                .get(&profile.id)
                .cloned()
                .ok_or_else(|| AppError::Message(format!("workspace not found: {}", profile.id)))?;
            validate_workspace_resources_update(store.list(), &current, &profile)?;
            validate_unique_path(store.list(), &profile)?;
            store.update(profile.clone())?;
            Ok(profile)
        })?;
        // 缓存的工具上下文带着旧的工具集和命令白名单，配置一变就必须丢弃。
        self.invalidate_tool_context(&saved.id);
        Ok(saved)
    }

    /// 删除工作区：先停掉它的服务和隧道，再移除配置与密钥。
    pub async fn delete_workspace(&self, id: &str) -> AppResult<WorkspaceProfile> {
        let profile = self.profile_by_id(id)?;
        for kind in [ServiceKind::Mcp, ServiceKind::Actions] {
            if self.with_runtime(|runtime| Ok(runtime.is_running(id, kind)))? {
                let _ = self.stop_service(id, kind).await;
            }
        }
        drop_tunnel_workspace(id).await?;
        self.with_runtime(|runtime| {
            runtime.drop_workspace(&profile);
            Ok(())
        })?;
        // 已授权客户端的注册表跟着工作区一起走。留着没用：工作区 id 不会复用，
        // 里面的条目永远不会再被命中，只是把 client_secret 留在磁盘上。
        for scope in [id.to_string(), format!("{id}-actions")] {
            if let Err(error) = crate::auth::remove_client_registry(&scope) {
                append_profile_log(
                    id,
                    "stderr.log",
                    &format!("[oauth] 清不掉客户端注册表 {scope}：{error}"),
                );
            }
        }
        self.with_data(|store| {
            store.remove(id)?;
            store.remove_workspace_secrets(id)?;
            let mut settings = store.settings();
            settings
                .restore_mcp_workspace_ids
                .retain(|workspace_id| workspace_id != id);
            settings
                .restore_actions_workspace_ids
                .retain(|workspace_id| workspace_id != id);
            if settings.last_workspace_id == id {
                settings.last_workspace_id.clear();
            }
            store.update_settings(settings)
        })?;
        self.invalidate_tool_context(id);
        Ok(profile)
    }

    /// 记住“最近使用的工作区”，供 `gld workspace use` 与无参命令回退。
    pub fn set_last_workspace(&self, id: &str) -> AppResult<()> {
        self.ensure_workspace_exists(id)?;
        self.update_settings(|settings| {
            settings.last_workspace_id = id.to_string();
            Ok(())
        })
    }

    pub fn last_workspace_id(&self) -> AppResult<Option<String>> {
        let id = self.settings()?.last_workspace_id;
        Ok((!id.is_empty()).then_some(id))
    }
}

/// 两个工作区指向同一个目录会让人分不清自己连的是谁：`gld ls` 里两行
/// 路径一模一样，历史档案和 Planning 台账还会互相覆盖。创建时已经拦了一道，
/// 改 `path` 是另一条能撞上的路。
fn validate_unique_path(
    profiles: &[WorkspaceProfile],
    candidate: &WorkspaceProfile,
) -> AppResult<()> {
    let Ok(root) = Path::new(&candidate.path).canonicalize() else {
        return Ok(());
    };
    for profile in profiles {
        if profile.id == candidate.id {
            continue;
        }
        if same_path(&profile.path, &root) {
            return Err(AppError::Message(format!(
                "目录 {} 已经是工作区「{}」（id {}）。同一个目录只能属于一个工作区。",
                root.display(),
                profile.name,
                profile.id
            )));
        }
    }
    Ok(())
}

/// 这一侧的配置有没有真的变。
///
/// 用 JSON 比而不是 `PartialEq`：这些结构体没派生 Eq，而且字段还在长，
/// 漏比一个新字段的后果是"改了不重启"，比多派生一个 trait 难查得多。
/// `name` 不属于任何一侧，改名不重启——它不影响服务怎么监听。
/// `path` 相反：两侧服务都在那个目录上跑工具，换了目录不重启的话，
/// 配置里写着新路径，Agent 读到的还是旧仓库。
fn service_config_changed(
    before: &WorkspaceProfile,
    after: &WorkspaceProfile,
    kind: ServiceKind,
) -> bool {
    let snapshot = |profile: &WorkspaceProfile| match kind {
        // MCP 那条线路由 runtime（端口 / 工具集 / 策略）、auth（认证）和
        // tunnel（公网入口）三段共同决定。
        //
        // name 也算：它会进 serverInfo.name，也就是客户端服务器列表里显示的那个名字。
        // 不重启的话，改完名 `gld ws show` 是新的、服务自报的还是旧的，
        // 而这种不一致只有连上客户端才看得见。
        ServiceKind::Mcp => serde_json::to_value((
            &profile.path,
            &profile.name,
            &profile.runtime,
            &profile.auth,
            &profile.tunnel,
        )),
        ServiceKind::Actions => serde_json::to_value((&profile.path, &profile.actions)),
    };
    match (snapshot(before), snapshot(after)) {
        (Ok(left), Ok(right)) => left != right,
        // 序列化不该失败；真失败了就当"变了"，重启一次总比漏掉强。
        _ => true,
    }
}

fn normalize_workspace_root(path: &Path) -> AppResult<PathBuf> {
    let expanded = if path.as_os_str().is_empty() {
        std::env::current_dir()?
    } else {
        path.to_path_buf()
    };
    let canonical = expanded.canonicalize().map_err(|error| {
        AppError::Message(format!(
            "目录不存在或无法访问：{}（{error}）",
            expanded.display()
        ))
    })?;
    if !canonical.is_dir() {
        return Err(AppError::Message(format!(
            "不是目录：{}",
            canonical.display()
        )));
    }
    Ok(canonical)
}

fn same_path(stored: &str, candidate: &Path) -> bool {
    Path::new(stored)
        .canonicalize()
        .map(|path| path == candidate)
        .unwrap_or(false)
}

/// 按 selector 找工作区。`base` 是调用方所在目录，用来解析相对路径形式的 selector。
fn resolve_by_selector(
    profiles: &[WorkspaceProfile],
    selector: &str,
    base: Option<&Path>,
) -> AppResult<WorkspaceProfile> {
    if let Some(profile) = profiles.iter().find(|profile| profile.id == selector) {
        return Ok(profile.clone());
    }

    let by_name: Vec<_> = profiles
        .iter()
        .filter(|profile| profile.name == selector)
        .collect();
    if let Some(unique) = unique(&by_name) {
        return Ok(unique.clone());
    }
    if by_name.len() > 1 {
        return Err(ambiguous(selector, &by_name));
    }

    let by_name_ci: Vec<_> = profiles
        .iter()
        .filter(|profile| profile.name.eq_ignore_ascii_case(selector))
        .collect();
    if let Some(unique) = unique(&by_name_ci) {
        return Ok(unique.clone());
    }

    if let Some(canonical) = selector_as_path(selector, base) {
        if let Some(profile) = profiles
            .iter()
            .find(|profile| same_path(&profile.path, &canonical))
        {
            return Ok(profile.clone());
        }
    }

    if selector.len() >= 4 {
        let by_prefix: Vec<_> = profiles
            .iter()
            .filter(|profile| profile.id.starts_with(selector))
            .collect();
        if let Some(unique) = unique(&by_prefix) {
            return Ok(unique.clone());
        }
        if by_prefix.len() > 1 {
            return Err(ambiguous(selector, &by_prefix));
        }
    }

    Err(AppError::Message(format!(
        "未找到工作区「{selector}」。可用 `gld workspace list` 查看，selector 支持 id、id 前缀（≥4 位）、名称或路径。"
    )))
}

/// 把 selector 当路径解析。相对路径按**调用方**目录算，不是当前进程的。
///
/// 守护进程的工作目录是数据目录（`~/.config/gld`），拿它去解析 `-w ../ccnm`
/// 会得到 `~/.config/ccnm`——那儿碰巧有目录的话就指到一个毫不相干的地方去了。
/// 拿不到调用方目录时（内部按 id 调用的场景）直接放弃路径匹配：猜一个基准
/// 目录只会错得更隐蔽。
fn selector_as_path(selector: &str, base: Option<&Path>) -> Option<PathBuf> {
    let path = Path::new(selector);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base?.join(path)
    };
    absolute.canonicalize().ok()
}

fn resolve_by_dir(profiles: &[WorkspaceProfile], cwd: &Path) -> AppResult<WorkspaceProfile> {
    let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let mut best: Option<(&WorkspaceProfile, usize)> = None;
    for profile in profiles {
        let Ok(root) = Path::new(&profile.path).canonicalize() else {
            continue;
        };
        if !cwd.starts_with(&root) {
            continue;
        }
        let depth = root.components().count();
        if best.map(|(_, d)| depth > d).unwrap_or(true) {
            best = Some((profile, depth));
        }
    }
    best.map(|(profile, _)| profile.clone()).ok_or_else(|| {
        AppError::Message(format!(
            "当前目录 {} 不属于任何工作区。请用 --workspace/-w 指定，或先执行 `gld workspace add <目录>`。",
            cwd.display()
        ))
    })
}

fn unique<'a>(items: &[&'a WorkspaceProfile]) -> Option<&'a WorkspaceProfile> {
    match items {
        [single] => Some(single),
        _ => None,
    }
}

fn ambiguous(selector: &str, matches: &[&WorkspaceProfile]) -> AppError {
    let listing = matches
        .iter()
        .map(|profile| format!("  {}  {}  {}", profile.id, profile.name, profile.path))
        .collect::<Vec<_>>()
        .join("\n");
    AppError::Message(format!(
        "「{selector}」匹配到多个工作区，请改用完整 id：\n{listing}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(id: &str, name: &str, path: &str) -> WorkspaceProfile {
        let mut profile = WorkspaceProfile::new(path.into(), Some(name.into()));
        profile.id = id.into();
        profile
    }

    /// 改名要让 MCP 跟着重启：那个名字会进 serverInfo.name。
    ///
    /// 漏了这条的表现很隐蔽：`gld ws show` 显示新名字，服务自报的还是旧的，
    /// 只有连上客户端看服务器列表才发现对不上。Actions 不用工作区名
    /// （它的 OpenAPI title 是固定的），别跟着白重启一次。
    #[test]
    fn renaming_a_workspace_restarts_mcp_but_not_actions() {
        let before = profile("abcd1234", "old", "/tmp/does-not-exist-a");
        let mut after = before.clone();
        after.name = "new".into();

        assert!(service_config_changed(&before, &after, ServiceKind::Mcp));
        assert!(!service_config_changed(
            &before,
            &after,
            ServiceKind::Actions
        ));
    }

    #[test]
    fn selector_prefers_exact_id_then_name_then_prefix() {
        let profiles = vec![
            profile("abcd1234", "api", "/tmp/does-not-exist-a"),
            profile("abcd9999", "web", "/tmp/does-not-exist-b"),
        ];
        assert_eq!(
            resolve_by_selector(&profiles, "abcd1234", None)
                .unwrap()
                .name,
            "api"
        );
        assert_eq!(
            resolve_by_selector(&profiles, "web", None).unwrap().id,
            "abcd9999"
        );
        assert_eq!(
            resolve_by_selector(&profiles, "WEB", None).unwrap().id,
            "abcd9999"
        );
        assert_eq!(
            resolve_by_selector(&profiles, "abcd99", None).unwrap().name,
            "web"
        );
        let ambiguous = resolve_by_selector(&profiles, "abcd", None)
            .unwrap_err()
            .to_string();
        assert!(ambiguous.contains("多个工作区"));
        assert!(resolve_by_selector(&profiles, "nope", None).is_err());
    }

    #[test]
    fn selector_matches_a_real_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let profiles = vec![profile("id1", "one", temp.path().to_str().unwrap())];
        let found = resolve_by_selector(&profiles, temp.path().to_str().unwrap(), None).unwrap();
        assert_eq!(found.id, "id1");
    }

    /// 相对路径要按调用方目录算，不是按当前进程的工作目录。
    ///
    /// 守护进程的工作目录是数据目录，用它解析 `-w ../ccnm` 会指到
    /// `~/.config/ccnm`——那儿刚好有目录的话就静默匹配到别人家去了。
    #[test]
    fn a_relative_selector_resolves_against_the_callers_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp.path().join("ccnm");
        std::fs::create_dir_all(&project).unwrap();
        let profiles = vec![profile("id1", "one", project.to_str().unwrap())];

        assert_eq!(
            resolve_by_selector(&profiles, "ccnm", Some(temp.path()))
                .unwrap()
                .id,
            "id1"
        );
        // 没有调用方目录就不做路径匹配：猜一个基准只会错得更隐蔽。
        assert!(resolve_by_selector(&profiles, "ccnm", None).is_err());
    }

    #[test]
    fn cwd_resolution_picks_the_deepest_matching_workspace() {
        let temp = tempfile::tempdir().expect("tempdir");
        let nested = temp.path().join("inner");
        let deeper = nested.join("src");
        std::fs::create_dir_all(&deeper).unwrap();
        let profiles = vec![
            profile("outer", "outer", temp.path().to_str().unwrap()),
            profile("inner", "inner", nested.to_str().unwrap()),
        ];
        assert_eq!(resolve_by_dir(&profiles, &deeper).unwrap().id, "inner");
        assert_eq!(resolve_by_dir(&profiles, temp.path()).unwrap().id, "outer");
        assert!(resolve_by_dir(&profiles, Path::new("/")).is_err());
    }
}

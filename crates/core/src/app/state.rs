use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::auth::{AccessReader, AccessStore};
use crate::data::DataStore;
use crate::error::{AppError, AppResult};
use crate::settings::AppSettings;
use crate::tools::ToolContext;
use crate::workspace::WorkspaceProfile;

/// 进程内的应用状态：持久化数据与本地工具上下文。
///
/// 同步锁不跨 await；监听状态及重启/恢复管理由宿主另行创建并显式传入。
pub struct App {
    data: Mutex<DataStore>,
    /// 普通项目配置操作不打开授权文件；监听器和授权用例共享同一个写者。
    access: Mutex<Option<Arc<AccessStore>>>,
    /// `gld tool call` 用的工具上下文，按工作区缓存。
    ///
    /// 缓存是为了让连续两次命令行调用共享 exec 会话（先 `exec_command`
    /// 再 `read_output`）。改了工作区配置必须失效，否则会继续用旧的
    /// 工具集和命令白名单，见 [`App::invalidate_tool_context`]。
    tool_contexts: Mutex<HashMap<String, Arc<ToolContext>>>,
}

impl App {
    /// 从磁盘加载数据并补齐共享密钥。
    pub fn load() -> AppResult<Self> {
        let mut store = DataStore::load()?;
        store.init_shared_secrets()?;
        Ok(Self {
            data: Mutex::new(store),
            access: Mutex::new(None),
            tool_contexts: Mutex::new(HashMap::new()),
        })
    }

    pub fn access_reader(&self) -> AppResult<AccessReader> {
        let store = self.access_store()?;
        self.require_current_authorization(&store)?;
        Ok(store.reader())
    }

    pub(super) fn require_current_authorization(&self, store: &AccessStore) -> AppResult<()> {
        if store.snapshot()?.is_initialized() {
            return Ok(());
        }
        let requires_rebuild = self.with_data(|data| Ok(data.data().grants.requires_rebuild()))?;
        // 检查旧配置期间首次创建可能已提交；已初始化的新授权优先。
        if requires_rebuild && !store.snapshot()?.is_initialized() {
            return Err(AppError::Message(
                "AUTH_REBUILD_REQUIRED: 检测到旧授权，新授权存储尚未初始化。请用 gld grant add 新建 grant，并让客户端重新授权。旧授权不会自动转换。".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn access_store(&self) -> AppResult<Arc<AccessStore>> {
        let mut access = self
            .access
            .lock()
            .map_err(|_| AppError::Message("authorization store lock poisoned".into()))?;
        if let Some(store) = access.as_ref() {
            return Ok(store.clone());
        }
        let store = Arc::new(AccessStore::open()?);
        *access = Some(store.clone());
        Ok(store)
    }

    pub(super) fn with_tool_contexts<R>(
        &self,
        f: impl FnOnce(&mut HashMap<String, Arc<ToolContext>>) -> AppResult<R>,
    ) -> AppResult<R> {
        let mut guard = self
            .tool_contexts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut guard)
    }

    /// 工作区配置变化后丢弃缓存的工具上下文，下次调用会按新配置重建。
    pub(super) fn invalidate_tool_context(&self, id: &str) {
        let _ = self.with_tool_contexts(|contexts| {
            contexts.remove(id);
            Ok(())
        });
    }

    pub(super) fn with_data<R>(
        &self,
        f: impl FnOnce(&mut DataStore) -> AppResult<R>,
    ) -> AppResult<R> {
        let mut guard = self
            .data
            .lock()
            .map_err(|_| AppError::Message("data store poisoned".into()))?;
        // 整段读-改-写都拿着文件锁，并且先重读磁盘：内存里这份只是缓存，文件才是准的。
        let _file = guard.lock_latest()?;
        f(&mut guard)
    }

    /// 当前设置快照。
    pub fn settings(&self) -> AppResult<AppSettings> {
        self.with_data(|store| Ok(store.settings()))
    }

    /// 读-改-写一次设置。
    pub(super) fn update_settings(
        &self,
        mutate: impl FnOnce(&mut AppSettings) -> AppResult<()>,
    ) -> AppResult<()> {
        self.with_data(|store| {
            let mut settings = store.settings();
            mutate(&mut settings)?;
            store.update_settings(settings)
        })
    }

    pub(super) fn profile_by_id(&self, id: &str) -> AppResult<WorkspaceProfile> {
        self.with_data(|store| {
            store
                .get(id)
                .cloned()
                .ok_or_else(|| AppError::Message(format!("workspace not found: {id}")))
        })
    }

    pub(super) fn ensure_workspace_exists(&self, id: &str) -> AppResult<()> {
        self.profile_by_id(id).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::SecretStore;

    /// 守护进程里不止 `App` 一个写数据文件的：全局入口拿到临时公网地址会回写，
    /// Actions 起服务时发现缺 OAuth 密钥会当场补一个，用户也可能手工改文件。
    /// 以前 `App` 拿启动时读的内存副本整份覆盖，这些值会在下一次任意保存时被悄悄冲掉。
    #[test]
    fn a_value_written_straight_to_the_file_survives_the_next_app_save() {
        crate::home::isolate_for_tests();
        let app = App::load().expect("load");
        let workspace_id = uuid::Uuid::new_v4().simple().to_string();

        SecretStore::set(
            &workspace_id,
            "actions_oauth_password",
            "written-behind-app",
        )
        .expect("write straight to the file");
        app.update_settings(|settings| {
            settings.last_workspace_id = workspace_id.clone();
            Ok(())
        })
        .expect("app save");

        assert_eq!(
            SecretStore::get(&workspace_id, "actions_oauth_password")
                .expect("read")
                .as_deref(),
            Some("written-behind-app"),
            "App 保存时把别人写进文件的密钥冲掉了"
        );
        // App 自己读到的也得是这个值，否则展示凭据的命令会报没有。
        assert_eq!(
            app.with_data(
                |store| store.get_workspace_secret(&workspace_id, "actions_oauth_password")
            )
            .expect("read through app")
            .as_deref(),
            Some("written-behind-app")
        );
    }
}

#[cfg(test)]
mod access_tests {
    use super::*;
    use crate::app::{BrowserGrantSpec, GrantCreate, GrantSpec};
    use crate::auth::{
        AccessRecord, CredentialKind, CredentialRecord, Grant, GrantPermissions, GrantStatus,
        ProjectScope,
    };
    use serde_json::json;

    fn record(id: &str, name: &str, secret: &str) -> AccessRecord {
        AccessRecord {
            grant: Grant {
                id: id.into(),
                name: name.into(),
                scope: ProjectScope::AllProjects,
                permissions: GrantPermissions::default(),
                status: GrantStatus::Active,
            },
            credential: CredentialRecord::prepare(
                format!("credential-{id}"),
                CredentialKind::Token,
                secret,
                1,
                100,
                Some(3600),
                Vec::new(),
            )
            .unwrap(),
        }
    }

    fn app_with_store() -> (tempfile::TempDir, App, Arc<AccessStore>) {
        crate::home::isolate_for_tests();
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(AccessStore::open_in(home.path()).unwrap());
        let app = App::load().unwrap();
        *app.access.lock().unwrap() = Some(store.clone());
        (home, app, store)
    }

    fn with_legacy(value: Option<serde_json::Value>, run: impl FnOnce()) {
        crate::home::isolate_for_tests();
        // GLD_HOME 是进程共享的；整个案例持有现有数据文件锁，避免污染并行测试。
        let mut locked_data = DataStore::load().unwrap();
        let _file_lock = locked_data.lock_latest().unwrap();
        let previous = DataStore::update_file(|data| {
            let previous = data.grants.clone();
            data.grants = match value {
                Some(value) => serde_json::from_value(value)?,
                None => Default::default(),
            };
            Ok(previous)
        })
        .unwrap();
        run();
        DataStore::update_file(|data| {
            data.grants = previous;
            Ok(())
        })
        .unwrap();
    }

    fn creation() -> crate::auth::access_store::GrantCreation {
        crate::auth::access_store::GrantCreation {
            name: "rebuilt-client".into(),
            scope: ProjectScope::AllProjects,
            permissions: GrantPermissions::default(),
            kind: CredentialKind::Token,
            ttl: None,
            allowed_origins: vec![],
        }
    }

    struct NoNodes;

    impl crate::hub::NodeBridge for NoNodes {
        fn members(&self) -> Result<Vec<crate::nodes::NativeMember>, String> {
            Ok(vec![])
        }

        fn call(
            &self,
            _: &crate::auth::AuthContext,
            _: &crate::nodes::NativeMember,
            _: &str,
            _: serde_json::Value,
        ) -> serde_json::Value {
            panic!("grant creation must not call a node")
        }
    }

    fn with_hub(
        profiles: Vec<WorkspaceProfile>,
        members: Vec<String>,
        run: impl FnOnce(&crate::hub::Hub),
    ) {
        // 调用方持有 with_legacy 的数据文件锁，整个真实 DataFile 快照测试串行。
        let previous = DataStore::update_file(|data| {
            let previous = (
                data.profiles.clone(),
                data.hub.clone(),
                data.ccnm_members.clone(),
            );
            data.profiles = profiles;
            data.hub = crate::settings::HubConfig::default();
            data.hub.members = members;
            data.ccnm_members.clear();
            Ok(previous)
        })
        .unwrap();
        let hub = crate::hub::Hub::new(&crate::settings::HubConfig::default(), Arc::new(NoNodes));
        run(&hub);
        DataStore::update_file(|data| {
            data.profiles = previous.0;
            data.hub = previous.1;
            data.ccnm_members = previous.2;
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn missing_current_store_only_requires_rebuild_for_nonempty_legacy() {
        for (legacy, expected) in [
            (None, false),
            (Some(json!(null)), false),
            (Some(json!([])), false),
            (Some(json!({})), false),
            (Some(json!("")), false),
            (Some(json!([{"unknown": "synthetic-old-secret"}])), true),
            (Some(json!(false)), true),
        ] {
            with_legacy(legacy, || {
                let (_home, app, store) = app_with_store();
                assert!(!store.snapshot().unwrap().is_initialized());
                if expected {
                    let error = match app.access_reader() {
                        Err(error) => error.to_string(),
                        Ok(_) => panic!("legacy authorization must require rebuilding"),
                    };
                    assert!(error.contains("AUTH_REBUILD_REQUIRED"));
                    assert!(error.contains("新建 grant"));
                    assert!(error.contains("客户端重新授权"));
                    assert!(!error.contains("synthetic-old-secret"));
                } else {
                    assert!(app.access_reader().is_ok());
                }
            });
        }
    }

    #[test]
    fn first_creation_recovers_reading_without_changing_legacy_json() {
        let legacy = json!({"unknown": ["synthetic-old-secret", null, 7]});
        with_legacy(Some(legacy.clone()), || {
            with_hub(vec![], vec![], |hub| {
                let (_home, app, store) = app_with_store();
                assert!(app.access_reader().is_err());
                let issued = app
                    .create_grant(GrantCreate::new("rebuilt-client"), hub)
                    .unwrap();
                assert!(store.snapshot().unwrap().is_initialized());
                assert_eq!(issued.grant.scope, ProjectScope::AllProjects);
                assert_eq!(issued.grant.credential.kind, CredentialKind::Token);
                assert_eq!(issued.grant.permissions, GrantPermissions::default());
                assert!(app
                    .access_reader()
                    .unwrap()
                    .snapshot()
                    .unwrap()
                    .by_grant_id(&issued.grant.id)
                    .is_some());
                let created_json = serde_json::to_value(&issued).unwrap();
                let old_id = issued.grant.credential.id.clone();
                let old_secret = issued.secret.into_string();
                assert_eq!(created_json["secret"], old_secret);
                let reader = app.access_reader().unwrap();
                let before = reader
                    .authenticate_token(
                        "test-hub",
                        &old_id,
                        &old_secret,
                        issued.grant.credential.issued_at_unix_secs,
                    )
                    .unwrap();
                let rotated = app.rotate_grant("rebuilt-client", None).unwrap();
                assert_eq!(rotated.grant.id, issued.grant.id);
                assert_eq!(rotated.grant.credential.revision, 2);
                let rotated_json = serde_json::to_value(&rotated).unwrap();
                let new_id = rotated.grant.credential.id.clone();
                let new_secret = rotated.secret.into_string();
                assert_eq!(rotated_json["secret"], new_secret);
                assert_ne!(old_secret, new_secret);
                let now = rotated.grant.credential.issued_at_unix_secs;
                assert!(reader
                    .authenticate_token("test-hub", &old_id, &old_secret, now)
                    .is_err());
                let after = reader
                    .authenticate_token("test-hub", &new_id, &new_secret, now)
                    .unwrap();
                assert_eq!(before.id(), after.id());
                let listed = serde_json::to_value(app.list_grants().unwrap()).unwrap();
                assert!(listed[0].get("secret").is_none());
                let listed_text = listed.to_string();
                assert!(!listed_text.contains(&old_secret));
                assert!(!listed_text.contains(&new_secret));
                assert!(!listed_text.contains("secret_sha256"));
                DataStore::read_file(|data| {
                    assert_eq!(serde_json::to_value(data)?["grants"], legacy);
                    Ok(())
                })
                .unwrap();
            });
        });
    }

    #[test]
    fn app_selected_creation_resolves_stable_ids_and_rejects_invalid_scope_atomically() {
        with_legacy(None, || {
            let mut api = WorkspaceProfile::new("/synthetic/api".into(), Some("API".into()));
            api.id = "abcd1111".into();
            api.runtime.confine_reads = true;
            let mut unsafe_project = api.clone();
            unsafe_project.id = "unsafe-id".into();
            unsafe_project.name = "unsafe".into();
            unsafe_project.runtime.confine_reads = false;
            let mut outsider = api.clone();
            outsider.id = "outside-id".into();
            outsider.name = "outside".into();
            with_hub(
                vec![api, unsafe_project, outsider],
                vec!["abcd1111".into(), "unsafe-id".into()],
                |hub| {
                    let (_home, app, store) = app_with_store();
                    let mut spec = GrantCreate::new("selected");
                    spec.projects = vec!["API".into(), "abcd".into(), "abcd1111".into()];
                    spec.write = true;
                    let issued = app.create_grant(spec, hub).unwrap();
                    assert_eq!(
                        issued.grant.scope,
                        ProjectScope::Selected(vec!["abcd1111".into()])
                    );
                    assert!(issued.grant.permissions.write);
                    let before = store.snapshot().unwrap();
                    for (project, all, service) in [
                        ("unsafe", false, false),
                        ("outside", false, false),
                        ("API", true, false),
                        ("API", false, true),
                    ] {
                        let mut spec = GrantCreate::new("rejected");
                        spec.projects = vec![project.into()];
                        spec.all = all;
                        spec.service_access = service;
                        assert!(app.create_grant(spec, hub).is_err());
                        assert!(Arc::ptr_eq(&before, &store.snapshot().unwrap()));
                    }
                    assert_eq!(app.list_grants().unwrap().len(), 1);
                },
            );
        });
    }

    #[test]
    fn app_service_access_is_explicit_for_all_token_and_oauth_and_rejects_url() {
        with_legacy(None, || {
            with_hub(vec![], vec![], |hub| {
                let (_home, app, store) = app_with_store();
                for (kind, name) in [
                    (CredentialKind::Token, "service-token"),
                    (CredentialKind::OAuth, "service-oauth"),
                ] {
                    let mut spec = GrantCreate::new(name);
                    spec.via = kind;
                    spec.all = true;
                    spec.service_access = true;
                    let issued = app.create_grant(spec, hub).unwrap();
                    assert_eq!(issued.grant.scope, ProjectScope::AllProjects);
                    let service = issued.grant.permissions.service;
                    assert!(
                        service.user_prompts && service.machine_mcp && service.managed_workspaces
                    );
                    assert!(!issued.grant.permissions.write);
                }
                let before = store.snapshot().unwrap();
                let mut spec = GrantCreate::new("service-url");
                spec.via = CredentialKind::Url;
                spec.service_access = true;
                assert!(app.create_grant(spec, hub).is_err());
                assert!(Arc::ptr_eq(&before, &store.snapshot().unwrap()));
                assert_eq!(app.list_grants().unwrap().len(), 2);
            })
        });
    }

    #[test]
    fn failed_first_commit_keeps_rebuild_required() {
        with_legacy(Some(json!([{"unknown": true}])), || {
            let (home, app, store) = app_with_store();
            let path = home.path().join("access/v1/grants.json");
            std::fs::create_dir(&path).unwrap();
            assert!(store.create_grant(creation(), 100).is_err());
            assert!(!store.snapshot().unwrap().is_initialized());
            let error = app
                .require_current_authorization(&store)
                .unwrap_err()
                .to_string();
            assert!(error.contains("AUTH_REBUILD_REQUIRED"));
        });
    }

    #[test]
    fn initialized_empty_store_takes_precedence_and_corruption_never_falls_back() {
        with_legacy(Some(json!([{"unknown": true}])), || {
            let home = tempfile::tempdir().unwrap();
            // open_in 建立私有父目录；写入明确的空 schema，区分缺文件与合法空文件。
            drop(AccessStore::open_in(home.path()).unwrap());
            let path = home.path().join("access/v1/grants.json");
            std::fs::write(&path, br#"{"schema_version":1,"records":[]}"#).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
            let store = Arc::new(AccessStore::open_in(home.path()).unwrap());
            assert!(store.snapshot().unwrap().is_initialized());
            let app = App::load().unwrap();
            *app.access.lock().unwrap() = Some(store.clone());
            assert!(app
                .access_reader()
                .unwrap()
                .snapshot()
                .unwrap()
                .records()
                .is_empty());
            *app.access.lock().unwrap() = None;
            drop(store);
            std::fs::write(&path, "{broken").unwrap();
            assert!(AccessStore::open_in(home.path()).is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "{broken");
            DataStore::read_file(|data| {
                assert_eq!(
                    serde_json::to_value(data)?["grants"],
                    json!([{"unknown": true}])
                );
                Ok(())
            })
            .unwrap();
        });
    }

    #[test]
    fn access_store_is_lazy_and_shared_by_app_readers() {
        crate::home::isolate_for_tests();
        let app = App::load().unwrap();
        assert!(app.access.lock().unwrap().is_none());
        let first = app.access_store().unwrap();
        let second = app.access_store().unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert!(Arc::ptr_eq(
            &app.access_reader().unwrap().snapshot().unwrap(),
            &first.snapshot().unwrap(),
        ));
    }

    #[test]
    fn ordinary_app_save_preserves_opaque_legacy_authorization() {
        crate::home::isolate_for_tests();
        let mut locked_data = DataStore::load().unwrap();
        let _file_lock = locked_data.lock_latest().unwrap();
        let app = App::load().unwrap();
        let legacy = json!({
            "unknown_shape": [{"bearer_token": "opaque-old-secret", "arbitrary": [null, 7]}]
        });
        let previous = DataStore::update_file(|data| {
            let previous = data.grants.clone();
            data.grants = serde_json::from_value(legacy.clone())?;
            Ok(previous)
        })
        .unwrap();
        app.update_settings(|settings| {
            settings.last_workspace_id = "opaque-preservation-test".into();
            Ok(())
        })
        .unwrap();
        DataStore::read_file(|data| {
            assert_eq!(serde_json::to_value(data)?["grants"], legacy);
            assert!(!format!("{data:?}").contains("opaque-old-secret"));
            Ok(())
        })
        .unwrap();
        DataStore::update_file(|data| {
            data.grants = previous;
            Ok(())
        })
        .unwrap();
        assert!(app.access.lock().unwrap().is_none());
    }

    #[test]
    fn grant_list_contains_only_current_metadata_and_issuance_stays_closed() {
        let (_home, app, store) = app_with_store();
        let one = record("one", "Client", "new-secret-once");
        let digest = serde_json::to_value(&one.credential).unwrap()["secret_sha256"]
            .as_str()
            .unwrap()
            .to_string();
        store.insert(one).unwrap();
        let before = app.access_reader().unwrap().snapshot().unwrap();
        let listed = app.list_grants().unwrap();
        let json = serde_json::to_string(&listed).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].credential.id, "credential-one");
        assert_eq!(listed[0].credential.revision, 1);
        assert!(!json.contains("new-secret-once"));
        assert!(!json.contains(&digest));
        assert!(!json.contains("oauthPassword"));
        assert!(!json.contains("bearerToken"));
        assert!(app
            .add_grant(GrantSpec {
                name: "disabled".into(),
                workspaces: Vec::new(),
                writable: false,
            })
            .unwrap_err()
            .to_string()
            .contains("AUTH_NOT_READY"));
        assert!(app
            .add_browser_grant(BrowserGrantSpec {
                name: "disabled-url".into(),
                workspaces: Vec::new(),
                writable: false,
                ttl_seconds: 3600,
                allowed_origins: Vec::new(),
            })
            .unwrap_err()
            .to_string()
            .contains("AUTH_NOT_READY"));
        assert!(Arc::ptr_eq(
            &before,
            &app.access_reader().unwrap().snapshot().unwrap(),
        ));
    }

    #[test]
    fn removal_commits_before_cleaning_only_the_selected_grant() {
        let (_home, app, store) = app_with_store();
        store
            .insert(record("one", "First", "first-secret"))
            .unwrap();
        store
            .insert(record("two", "Second", "second-secret"))
            .unwrap();
        let reader = app.access_reader().unwrap();
        let observed = reader.clone();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let events = calls.clone();
        let removed = app
            .remove_grant_with_cleanup("FIRST", move |grant_id, _| {
                let current = observed.snapshot().unwrap();
                assert!(matches!(
                    current.by_grant_id("one").unwrap().grant.status,
                    GrantStatus::Revoked,
                ));
                assert!(matches!(
                    current.by_grant_id("two").unwrap().grant.status,
                    GrantStatus::Active,
                ));
                events.lock().unwrap().push(grant_id.to_string());
                (2, 1)
            })
            .unwrap();
        assert_eq!(removed.grant.id, "one");
        assert!(matches!(removed.grant.status, GrantStatus::Revoked));
        assert_eq!(removed.stopped_commands, 2);
        assert_eq!(removed.stopped_agent_tasks, 1);
        assert_eq!(*calls.lock().unwrap(), vec!["one"]);
        assert!(reader.snapshot().unwrap().by_name("Second").is_some());
    }
}

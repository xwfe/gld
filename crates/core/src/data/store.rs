use std::cell::Cell;
use std::sync::{Mutex, MutexGuard};

use crate::auth::GrantRecord;
use crate::bridge::member::CcnmMember;
use crate::error::{AppError, AppResult};
use crate::settings::AppSettings;
use crate::workspace::WorkspaceProfile;

use super::migrate::{
    data_file_path, load_existing, load_or_migrate, maybe_backup_legacy_files, save,
};
use super::model::AppData;

static DATA_FILE_LOCK: Mutex<()> = Mutex::new(());

thread_local! {
    /// 当前线程是否已经拿着 [`DATA_FILE_LOCK`]。
    ///
    /// `App` 会在整段"重读 → 改 → 保存"期间拿着锁，而保存（以及闭包里可能调到的
    /// `AppSettings::load_or_default`）自己也要拿锁。`std::sync::Mutex` 不可重入，
    /// 没有这个标记就是同一线程自己等自己，整个守护进程卡死。
    static HOLDING_DATA_FILE_LOCK: Cell<bool> = const { Cell::new(false) };
}

/// 数据文件锁。同一线程里嵌套拿锁时，里层拿到的是个空壳，由最外层负责解锁。
///
/// 里面是 `MutexGuard`，所以不能跨 `.await` 持有（编译器会拒绝把它送到别的线程），
/// 这正好保证了线程标记不会错位。
pub(crate) struct DataFileGuard(Option<MutexGuard<'static, ()>>);

impl Drop for DataFileGuard {
    fn drop(&mut self) {
        if self.0.is_some() {
            HOLDING_DATA_FILE_LOCK.with(|holding| holding.set(false));
        }
    }
}

const SHARED_KEYS: &[&str] = &[
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

#[derive(Debug)]
pub struct DataStore {
    data: AppData,
}

impl DataStore {
    pub fn load() -> AppResult<Self> {
        let _guard = lock_data_file()?;
        let path = data_file_path()?;
        let existed_before = path.exists();
        let data = load_or_migrate()?;
        let store = Self { data };
        if !existed_before {
            store.persist_unlocked()?;
        }
        if !existed_before {
            maybe_backup_legacy_files(&path)?;
        }
        Ok(store)
    }

    pub fn read_file<R>(f: impl FnOnce(&AppData) -> AppResult<R>) -> AppResult<R> {
        let _guard = lock_data_file()?;
        let data = load_or_migrate()?;
        f(&data)
    }

    pub fn update_file<R>(f: impl FnOnce(&mut AppData) -> AppResult<R>) -> AppResult<R> {
        let _guard = lock_data_file()?;
        let mut data = load_or_migrate()?;
        let result = f(&mut data)?;
        save(&data)?;
        Ok(result)
    }

    /// 拿住数据文件锁，并把内存副本换成磁盘上的最新内容。
    ///
    /// 内存副本保存时是整份覆盖文件的。守护进程里还有别的代码直接改文件（全局入口
    /// 回写公网地址、Actions 补 OAuth 密钥），用户也可能手工改。不先重读，这些值会在
    /// 下一次任意保存时被悄悄冲掉：密钥变了、连接器突然 401，还查不到是谁改的。
    ///
    /// 文件不在了就沿用内存里的（下次保存会写回去）。当成空配置的话，误删一个文件
    /// 就会让守护进程把所有工作区和密钥一起抹掉。文件坏了则报错，不覆盖它。
    pub(crate) fn lock_latest(&mut self) -> AppResult<DataFileGuard> {
        let guard = lock_data_file()?;
        if let Some(data) = load_existing()? {
            self.data = data;
        }
        Ok(guard)
    }

    pub fn data(&self) -> &AppData {
        &self.data
    }

    pub fn save(&self) -> AppResult<()> {
        let _guard = lock_data_file()?;
        self.persist_unlocked()
    }

    fn persist_unlocked(&self) -> AppResult<()> {
        save(&self.data)
    }

    pub fn settings(&self) -> AppSettings {
        AppSettings::from_data(&self.data)
    }

    pub fn update_settings(&mut self, settings: AppSettings) -> AppResult<()> {
        settings.apply_to(&mut self.data);
        self.save()
    }

    pub fn list(&self) -> &[WorkspaceProfile] {
        &self.data.profiles
    }

    pub fn get(&self, id: &str) -> Option<&WorkspaceProfile> {
        self.data.profiles.iter().find(|profile| profile.id == id)
    }

    pub fn add(&mut self, profile: WorkspaceProfile) -> AppResult<()> {
        self.data.profiles.push(profile);
        self.save()
    }

    pub fn update(&mut self, profile: WorkspaceProfile) -> AppResult<()> {
        let Some(index) = self
            .data
            .profiles
            .iter()
            .position(|item| item.id == profile.id)
        else {
            return Err(AppError::Message(format!(
                "workspace not found: {}",
                profile.id
            )));
        };
        self.data.profiles[index] = profile;
        self.save()
    }

    pub fn remove(&mut self, id: &str) -> AppResult<Option<WorkspaceProfile>> {
        let Some(index) = self.data.profiles.iter().position(|item| item.id == id) else {
            return Ok(None);
        };
        let removed = self.data.profiles.remove(index);
        self.data.workspace_secrets.remove(id);
        self.save()?;
        Ok(Some(removed))
    }

    /// 远端 ccnm 成员。和 [`list`](Self::list) 是两份名单，不混在一起——
    /// 远端成员没有本机根目录、隧道和 Planning（RFC-0002 5.1）。
    pub fn ccnm_members(&self) -> &[CcnmMember] {
        &self.data.ccnm_members
    }

    /// 按 id 加一个远端成员，已有同 id 就整条换掉。
    pub fn upsert_ccnm_member(&mut self, member: CcnmMember) -> AppResult<()> {
        match self
            .data
            .ccnm_members
            .iter()
            .position(|item| item.id == member.id)
        {
            Some(index) => self.data.ccnm_members[index] = member,
            None => self.data.ccnm_members.push(member),
        }
        self.save()
    }

    pub fn remove_ccnm_member(&mut self, id: &str) -> AppResult<Option<CcnmMember>> {
        let Some(index) = self.data.ccnm_members.iter().position(|item| item.id == id) else {
            return Ok(None);
        };
        let removed = self.data.ccnm_members.remove(index);
        self.save()?;
        Ok(Some(removed))
    }

    /// 只开部分项目的凭据（RFC-0007），连同它们的钥匙。
    pub fn grants(&self) -> &[GrantRecord] {
        &self.data.grants
    }

    pub fn add_grant(&mut self, record: GrantRecord) -> AppResult<()> {
        self.data.grants.push(record);
        self.save()
    }

    pub fn remove_grant(&mut self, id: &str) -> AppResult<Option<GrantRecord>> {
        let Some(index) = self.data.grants.iter().position(|item| item.grant.id == id) else {
            return Ok(None);
        };
        let removed = self.data.grants.remove(index);
        self.save()?;
        Ok(Some(removed))
    }

    pub fn init_workspace_secrets(&mut self, profile_id: &str) -> AppResult<()> {
        // oauth_client_secret is optional for MCP OAuth (ChatGPT PKCE); not auto-generated.
        self.set_workspace_secret(profile_id, "oauth_password", &random_secret())?;
        self.set_workspace_secret(profile_id, "oauth_token_secret", &random_secret())?;
        self.set_workspace_secret(profile_id, "bearer_token", &random_secret())?;
        self.set_workspace_secret(profile_id, "actions_api_key", &random_secret())?;
        self.set_workspace_secret(profile_id, "actions_oauth_client_secret", &random_secret())?;
        self.set_workspace_secret(profile_id, "actions_oauth_password", &random_secret())?;
        self.set_workspace_secret(profile_id, "actions_oauth_token_secret", &random_secret())?;
        Ok(())
    }

    pub fn init_shared_secrets(&mut self) -> AppResult<()> {
        let mut changed = false;
        for key in SHARED_KEYS {
            if !self.data.shared_secrets.contains_key(*key) {
                self.data
                    .shared_secrets
                    .insert(key.to_string(), shared_value_for_key(key));
                changed = true;
            }
        }
        if changed {
            self.save()?;
        }
        Ok(())
    }

    pub fn get_workspace_secret(&self, profile_id: &str, key: &str) -> AppResult<Option<String>> {
        Ok(self
            .data
            .workspace_secrets
            .get(profile_id)
            .and_then(|secrets| secrets.get(key))
            .filter(|value| !value.is_empty())
            .cloned())
    }

    pub fn set_workspace_secret(
        &mut self,
        profile_id: &str,
        key: &str,
        value: &str,
    ) -> AppResult<()> {
        self.data
            .workspace_secrets
            .entry(profile_id.to_string())
            .or_default()
            .insert(key.to_string(), value.to_string());
        self.save()
    }

    pub fn regenerate_workspace_secret(
        &mut self,
        profile_id: &str,
        key: &str,
    ) -> AppResult<String> {
        let value = shared_value_for_key(key);
        self.set_workspace_secret(profile_id, key, &value)?;
        Ok(value)
    }

    pub fn remove_workspace_secrets(&mut self, profile_id: &str) -> AppResult<()> {
        self.data.workspace_secrets.remove(profile_id);
        self.save()
    }

    pub fn get_shared_secret(&self, key: &str) -> Option<String> {
        self.data.shared_secrets.get(key).cloned()
    }

    pub fn set_shared_secret(&mut self, key: &str, value: &str) -> AppResult<()> {
        self.data
            .shared_secrets
            .insert(key.to_string(), value.to_string());
        self.save()
    }

    pub fn regenerate_shared_secret(&mut self, key: &str) -> AppResult<String> {
        let value = random_secret();
        self.set_shared_secret(key, &value)?;
        Ok(value)
    }

    pub fn get_app_secret(&self, scope: &str, item_id: &str) -> Option<String> {
        self.data
            .app_secrets
            .get(scope)
            .and_then(|items| items.get(item_id))
            .filter(|value| !value.is_empty())
            .cloned()
    }

    pub fn set_app_secret(&mut self, scope: &str, item_id: &str, value: &str) -> AppResult<()> {
        self.data
            .app_secrets
            .entry(scope.to_string())
            .or_default()
            .insert(item_id.to_string(), value.to_string());
        self.save()
    }

    /// 取应用级密钥，没有就当场生成一个存下。
    ///
    /// 给聚合入口这种"第一次用到才需要凭据"的地方用。查和写在同一次 `App::with_data`
    /// 里完成，两条并发请求不会各生成一个、后写的把先写的盖掉。
    pub fn get_or_create_app_secret(&mut self, scope: &str, item_id: &str) -> AppResult<String> {
        match self.get_app_secret(scope, item_id) {
            Some(value) => Ok(value),
            None => self.regenerate_app_secret(scope, item_id),
        }
    }

    pub fn regenerate_app_secret(&mut self, scope: &str, item_id: &str) -> AppResult<String> {
        let value = shared_value_for_key(item_id);
        self.set_app_secret(scope, item_id, &value)?;
        Ok(value)
    }

    pub fn delete_app_secret(&mut self, scope: &str, item_id: &str) -> AppResult<()> {
        if let Some(items) = self.data.app_secrets.get_mut(scope) {
            items.remove(item_id);
            if items.is_empty() {
                self.data.app_secrets.remove(scope);
            }
        }
        self.save()
    }
}

fn lock_data_file() -> AppResult<DataFileGuard> {
    if HOLDING_DATA_FILE_LOCK.with(Cell::get) {
        return Ok(DataFileGuard(None));
    }
    let guard = DATA_FILE_LOCK
        .lock()
        .map_err(|_| AppError::Message("data file lock poisoned".into()))?;
    HOLDING_DATA_FILE_LOCK.with(|holding| holding.set(true));
    Ok(DataFileGuard(Some(guard)))
}

pub(crate) fn random_secret() -> String {
    format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4()).replace('-', "")
}

fn shared_value_for_key(key: &str) -> String {
    if key == "oauth_client_id" {
        format!("chatgpt-client-{}", &uuid::Uuid::new_v4().to_string()[..12])
    } else {
        random_secret()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_secret_roundtrip() {
        crate::home::isolate_for_tests();
        let id = uuid::Uuid::new_v4().to_string().replace('-', "");
        let mut store = DataStore::load().expect("load");
        store
            .set_workspace_secret(&id, "oauth_client_secret", "roundtrip-secret")
            .expect("set");
        let loaded = store
            .get_workspace_secret(&id, "oauth_client_secret")
            .expect("get");
        assert_eq!(loaded.as_deref(), Some("roundtrip-secret"));
        store.remove_workspace_secrets(&id).expect("remove");
    }

    #[test]
    fn shared_oauth_client_id_uses_client_id_format() {
        let value = shared_value_for_key("oauth_client_id");
        assert!(value.starts_with("chatgpt-client-"));
        assert_eq!(value.len(), "chatgpt-client-".len() + 12);
    }
}

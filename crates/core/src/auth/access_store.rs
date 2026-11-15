//! D18 授权状态：独立格式、一个写者、提交后发布只读索引。
//! 原 AppData 不是数据源；读取损坏的新文件也不回退到旧授权。

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use fs2::FileExt;
use gld_policy::{Grant, GrantStatus};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::nodes::local_config::{check_private_file, create_private_file};

mod authorize;
mod model;
pub use model::{AccessRecord, CredentialRecord, MAX_URL_TTL_SECONDS};

#[cfg(test)]
mod tests;

const SCHEMA_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AccessFile {
    schema_version: u32,
    records: Vec<AccessRecord>,
}

/// 一次已提交的不可变状态。请求通过 AccessReader 取得当前值，不单独缓存认证结果。
#[derive(Debug)]
pub struct AccessSnapshot {
    records: Vec<AccessRecord>,
    grants: HashMap<String, usize>,
    credentials: HashMap<String, usize>,
    names: HashMap<String, usize>,
}

impl AccessSnapshot {
    fn build(records: Vec<AccessRecord>) -> AppResult<Self> {
        let mut snapshot = Self {
            records,
            grants: HashMap::new(),
            credentials: HashMap::new(),
            names: HashMap::new(),
        };
        for (index, record) in snapshot.records.iter().enumerate() {
            record
                .validate()
                .map_err(|_| invalid("Invalid authorization record"))?;
            if snapshot
                .grants
                .insert(record.grant.id.clone(), index)
                .is_some()
            {
                return Err(invalid("Duplicate grant ID"));
            }
            if snapshot
                .credentials
                .insert(record.credential.id.clone(), index)
                .is_some()
            {
                return Err(invalid("Duplicate credential ID"));
            }
            if matches!(record.grant.status, GrantStatus::Active)
                && snapshot
                    .names
                    .insert(record.grant.name.to_ascii_lowercase(), index)
                    .is_some()
            {
                return Err(invalid("Duplicate active grant name"));
            }
        }
        Ok(snapshot)
    }

    pub fn records(&self) -> &[AccessRecord] {
        &self.records
    }

    /// 管理查询包含撤销记录；找到记录不表示已经通过认证。
    pub fn by_grant_id(&self, id: &str) -> Option<&AccessRecord> {
        self.grants.get(id).map(|&index| &self.records[index])
    }

    /// 只定位材料；认证入口仍需校验 grant 状态、用途、修订号、期限及限制。
    pub fn by_credential_id(&self, id: &str) -> Option<&AccessRecord> {
        self.credentials.get(id).map(|&index| &self.records[index])
    }

    /// 撤销的记录不占用名字；它们的 ID 仍保留，不能被新授权重新使用。
    pub fn by_name(&self, name: &str) -> Option<&AccessRecord> {
        self.names
            .get(&name.to_ascii_lowercase())
            .map(|&index| &self.records[index])
    }
}

// fork 到 exec 之间仍会继承 CLOEXEC 描述符，dup 也会延长同一 open file description。
// 最后一个 reader 释放时主动解锁，不能只等 File::drop 关闭最后一份 OS 引用。
struct WriterLock {
    file: File,
}

impl Drop for WriterLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

struct Shared {
    current: RwLock<Arc<AccessSnapshot>>,
    // 旧监听器仍持有 reader 时不释放写者锁，避免它继续接受已经过时的授权。
    _lock: WriterLock,
}

#[derive(Clone)]
pub struct AccessReader {
    shared: Arc<Shared>,
}

impl AccessReader {
    pub fn snapshot(&self) -> AppResult<Arc<AccessSnapshot>> {
        self.shared
            .current
            .read()
            .map(|value| Arc::clone(&value))
            .map_err(|_| invalid("Authorization snapshot lock is poisoned"))
    }
}

pub struct AccessStore {
    path: PathBuf,
    writer: Mutex<()>,
    shared: Arc<Shared>,
}

impl AccessStore {
    pub fn open() -> AppResult<Self> {
        Self::open_in(&crate::home::data_home()?)
    }

    pub fn open_in(data_home: &Path) -> AppResult<Self> {
        let path = data_home.join("access").join("v1").join("grants.json");
        let parent = path.parent().expect("authorization path has parent");
        create_directory(parent)?;
        let lock = open_lock(&path.with_extension("lock"))?;
        let records = read_file(&path)?;
        let snapshot = Arc::new(AccessSnapshot::build(records)?);
        Ok(Self {
            path,
            writer: Mutex::new(()),
            shared: Arc::new(Shared {
                current: RwLock::new(snapshot),
                _lock: lock,
            }),
        })
    }

    pub fn reader(&self) -> AccessReader {
        AccessReader {
            shared: self.shared.clone(),
        }
    }

    pub fn snapshot(&self) -> AppResult<Arc<AccessSnapshot>> {
        self.reader().snapshot()
    }

    pub fn insert(&self, record: AccessRecord) -> AppResult<()> {
        self.change(|records| {
            if records.iter().any(|old| old.grant.id == record.grant.id) {
                return Err(invalid("Grant ID already exists"));
            }
            records.push(record);
            Ok(())
        })
    }

    pub fn update(&self, record: AccessRecord) -> AppResult<()> {
        self.change(|records| {
            let old = records
                .iter_mut()
                .find(|old| old.grant.id == record.grant.id)
                .ok_or_else(|| invalid("Grant ID does not exist"))?;
            if matches!(old.grant.status, GrantStatus::Revoked) {
                return Err(invalid("A revoked grant cannot be updated"));
            }
            if std::mem::discriminant(&old.credential.kind)
                != std::mem::discriminant(&record.credential.kind)
            {
                return Err(invalid("A grant cannot change credential kind"));
            }
            if record.credential.revision < old.credential.revision
                || (record.credential.revision == old.credential.revision
                    && !record.credential.same_material(&old.credential))
            {
                return Err(invalid("Changed credentials require a newer revision"));
            }
            *old = record;
            Ok(())
        })
    }

    pub fn revoke(&self, grant_id: &str) -> AppResult<Grant> {
        self.change(|records| {
            let record = records
                .iter_mut()
                .find(|record| record.grant.id == grant_id)
                .ok_or_else(|| invalid("Grant ID does not exist"))?;
            record.grant.status = GrantStatus::Revoked;
            Ok(record.grant.clone())
        })
    }

    fn change<R>(
        &self,
        mutate: impl FnOnce(&mut Vec<AccessRecord>) -> AppResult<R>,
    ) -> AppResult<R> {
        let _writer = self
            .writer
            .lock()
            .map_err(|_| invalid("Authorization writer lock is poisoned"))?;
        let mut records = self.snapshot()?.records.clone();
        let result = mutate(&mut records)?;
        let candidate = Arc::new(AccessSnapshot::build(records)?);
        let bytes = serde_json::to_vec_pretty(&AccessFile {
            schema_version: SCHEMA_VERSION,
            records: candidate.records.clone(),
        })
        .map_err(|_| invalid("Cannot serialize authorization data"))?;
        // 所有可能失败的检查和锁获取都在 rename 前；提交后只替换已分配好的 Arc。
        let mut current = self
            .shared
            .current
            .write()
            .map_err(|_| invalid("Authorization snapshot lock is poisoned"))?;
        replace_file(&self.path, &bytes)?;
        *current = candidate;
        Ok(result)
    }
}

fn invalid(message: &str) -> AppError {
    AppError::Message(message.into())
}

fn create_directory(path: &Path) -> AppResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
    }
    #[cfg(not(unix))]
    crate::home::create_data_dir(path)?;
    Ok(())
}

fn open_lock(path: &Path) -> AppResult<WriterLock> {
    let file = match create_private_file(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            open_existing(path, true)?
        }
        Err(_) => return Err(invalid("Cannot create authorization writer lock")),
    };
    check_private_file(&file).map_err(|_| invalid("Authorization lock must be a private file"))?;
    FileExt::try_lock_exclusive(&file)
        .map_err(|_| invalid("Authorization store already has a writer or cannot be locked"))?;
    // 仅成功取得锁后才构造守卫；open_in 后续读文件或解析失败也会显式解锁。
    Ok(WriterLock { file })
}

fn open_existing(path: &Path, writable: bool) -> AppResult<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(writable);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, SECURITY_ANONYMOUS,
        };
        options
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
            .security_qos_flags(SECURITY_ANONYMOUS.0);
    }
    let file = options
        .open(path)
        .map_err(|_| invalid("Cannot open authorization data"))?;
    check_private_file(&file).map_err(|_| invalid("Authorization data must be a private file"))?;
    Ok(file)
}

fn read_file(path: &Path) -> AppResult<Vec<AccessRecord>> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(invalid("Cannot inspect authorization data")),
        Ok(_) => {}
    }
    let mut bytes = Vec::new();
    open_existing(path, false)?
        .read_to_end(&mut bytes)
        .map_err(|_| invalid("Cannot read authorization data"))?;
    let saved: AccessFile = serde_json::from_slice(&bytes)
        .map_err(|_| invalid("Invalid authorization schema; the original file was kept"))?;
    if saved.schema_version != SCHEMA_VERSION {
        return Err(invalid("Unsupported authorization schema version"));
    }
    Ok(saved.records)
}

fn replace_file(path: &Path, bytes: &[u8]) -> AppResult<()> {
    let temp = path.with_file_name(format!(".gld-access-{}.tmp", uuid::Uuid::new_v4().simple()));
    // 只清理本次成功创建的临时文件，不删除创建失败时原已存在的路径。
    let mut file = create_private_file(&temp)
        .map_err(|_| invalid("Cannot create private authorization data"))?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    // rename 是提交点。之后不能因另一次可失败 I/O 而继续使用旧快照。
    let result = written.and_then(|()| fs::rename(&temp, path));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(|_| invalid("Cannot save authorization data; no new state was published"))
}

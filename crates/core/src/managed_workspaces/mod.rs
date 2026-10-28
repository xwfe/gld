//! 服务管理的非项目执行目录。登记在工作目录之外；同主体同请求只创建一次。

mod archive;
mod artifacts;
mod capacity;
mod lifecycle;
mod operation;
mod retention;
pub(crate) mod startup;
pub(crate) use artifacts::artifact_workspace;
pub(crate) use artifacts::Artifact;
pub(crate) use lifecycle::PendingCommand;
pub(crate) use operation::Operation;

use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::tools::workspace::WorkspaceError;

type Result<T> = std::result::Result<T, WorkspaceError>;
const MAX_RECORDS: usize = crate::settings::WorkSessionLimits::MAX_RECORDS as usize;
const MAX_RECORD_BYTES: u64 = 64 * 1024;
const REGISTRY: &str = "managed-workspaces";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum State {
    Creating,
    Active,
    Closing,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    pub schema_version: u32,
    pub id: String,
    pub root: PathBuf,
    pub owner: String,
    pub client_request_id: String,
    pub purpose: String,
    pub state: State,
    pub created_at_ms: u64,
    pub last_used_at_ms: u64,
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
    #[serde(default)]
    pub finish_paths: Option<Vec<String>>,
    #[serde(default)]
    pub pending_commands: Vec<PendingCommand>,
    /// 没有这一字段的 W0 登记不能证明历史命令已经结束，不直接清理。
    #[serde(default)]
    pub resource_tracking: bool,
    #[serde(default)]
    pub idle_retention_ms: Option<u64>,
    #[serde(default)]
    pub expires_at_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct Store {
    home: PathBuf,
    limits: crate::settings::WorkSessionLimits,
}

impl Store {
    pub fn new(home: PathBuf) -> Self {
        Self {
            home,
            limits: crate::settings::WorkSessionLimits::default(),
        }
    }

    pub fn with_limits(mut self, limits: crate::settings::WorkSessionLimits) -> Self {
        self.limits = limits;
        self
    }

    /// 不含凭据；由 hub 传入已经验过身份的 owner，客户端不能指定 owner 或路径。
    pub fn start(&self, owner: &str, request: &str, purpose: &str) -> Result<(Record, bool)> {
        validate_text(owner, "owner", 1024, false)?;
        validate_text(request, "client_request_id", 128, false)?;
        validate_text(purpose, "purpose", 256, true)?;
        let guard = self.lock(true)?.expect("start creates registry");
        let records = self.read_records(&guard.home)?;
        // 旧版本的 closed 回执还在主目录。先归档，不丢请求标识，也不删除产物。
        for record in records.iter().filter(|r| r.state == State::Closed) {
            archive::close(&guard.home, record)?;
        }
        let count = records.iter().filter(|r| r.state != State::Closed).count();
        if let Some(mut record) = records
            .into_iter()
            .find(|record| record.owner == owner && record.client_request_id == request)
        {
            if record.purpose != purpose {
                return Err(error("WORK_SESSION_REQUEST_CONFLICT", "The request ID already belongs to a different purpose; reuse the original request or choose a new ID", "conflict"));
            }
            if record.state == State::Creating {
                self.activate(&guard.home, &mut record)?;
            } else if record.state == State::Active {
                verify_root(&guard.home, &record)?;
            }
            return Ok((record, false));
        }
        if let Some(record) = archive::by_request(&guard.home, owner, request)? {
            if record.purpose != purpose {
                return Err(error(
                    "WORK_SESSION_REQUEST_CONFLICT",
                    "The archived request belongs to a different purpose; choose a new request ID",
                    "conflict",
                ));
            }
            return Ok((record, false));
        }
        self.limits
            .validate()
            .map_err(WorkspaceError::invalid_argument)?;
        if count >= self.limits.max_active as usize
            || count + archive::count(&guard.home)? >= self.limits.max_records as usize
        {
            return Err(error("WORK_SESSION_LIMIT", "Managed workspace capacity reached; closed receipts do not use active slots, but total records remain bounded. No files or receipts were deleted", "resource"));
        }
        capacity::require_space(&guard.home, self.limits.max_bytes, 0)?;
        let id = format!("work-{}", Uuid::new_v4().simple());
        let now = now_ms();
        let mut record = Record {
            schema_version: 2,
            root: guard.home.join("work").join(&id),
            id,
            owner: owner.into(),
            client_request_id: request.into(),
            purpose: purpose.into(),
            state: State::Creating,
            created_at_ms: now,
            last_used_at_ms: now,
            artifacts: Vec::new(),
            finish_paths: None,
            pending_commands: Vec::new(),
            resource_tracking: true,
            idle_retention_ms: Some(7 * 24 * 60 * 60 * 1000),
            expires_at_ms: Some(now + 7 * 24 * 60 * 60 * 1000),
        };
        // 先登记再建目录：中断后同请求能完成 creating，prune 也能保护其记录。
        save(&guard.home, &record)?;
        self.activate(&guard.home, &mut record)?;
        Ok((record, true))
    }

    /// 用于 hub 的完整存活集合与 prune；目录暂时丢失也保留登记，不能当孤儿删除。
    pub fn all(&self) -> Result<Vec<Record>> {
        let Some(guard) = self.lock(false)? else {
            return Ok(Vec::new());
        };
        let mut records = self.read_records(&guard.home)?;
        archive::append(&guard.home, &mut records)?;
        Ok(records)
    }

    /// 逐次路由和到期扫描不反复加载历史回执；prune 与显式列表仍使用完整 all。
    pub fn live(&self) -> Result<Vec<Record>> {
        let Some(guard) = self.lock(false)? else {
            return Ok(Vec::new());
        };
        self.read_records(&guard.home)
    }

    pub fn owned(&self, owner: &str) -> Result<Vec<Record>> {
        Ok(self
            .all()?
            .into_iter()
            .filter(|r| r.owner == owner)
            .collect())
    }

    /// ID 不存在与属于别人是同一个错误。访问目录前每次重新核对路径，而非只信缓存。
    #[cfg(test)]
    pub fn get(&self, owner: &str, id: &str, touch: bool) -> Result<Record> {
        if !is_id(id) {
            return Err(not_found());
        }
        let Some(guard) = self.lock(false)? else {
            return Err(not_found());
        };
        let mut record = self
            .read_records(&guard.home)?
            .into_iter()
            .find(|r| r.id == id && r.owner == owner)
            .ok_or_else(not_found)?;
        if record.state != State::Active {
            return Err(error("WORK_SESSION_INCOMPLETE", "Creation is incomplete; retry work_session start with the original client_request_id", "storage"));
        }
        verify_root(&guard.home, &record)?;
        if touch {
            record.last_used_at_ms = now_ms();
            save(&guard.home, &record)?;
        }
        Ok(record)
    }

    fn activate(&self, home: &Path, record: &mut Record) -> Result<()> {
        if record.state == State::Creating {
            normal_dir(&home.join("work"), true)?;
            normal_dir(&record.root, true)?;
        }
        verify_root(home, record)?;
        record.state = State::Active;
        record.last_used_at_ms = now_ms();
        save(home, record)
    }

    fn lock(&self, create: bool) -> Result<Option<RegistryGuard>> {
        // GLD_HOME 是操作员指定的根，本来就允许迁移后以链接保留旧位置；只对
        // 其下的托管目录拒绝链接，避免功能关闭时也影响已有服务的普通调用。
        let selected = if self.home.is_symlink() {
            self.home.canonicalize().map_err(storage)?
        } else {
            self.home.clone()
        };
        if !normal_dir(&selected, create)? {
            return Ok(None);
        }
        let home = selected.canonicalize().map_err(storage)?;
        if !normal_dir(&home.join(REGISTRY), create)? {
            return Ok(None);
        }
        let lock_path = home.join(REGISTRY).join(".lock");
        regular_or_absent(&lock_path)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(lock_path).map_err(storage)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match FileExt::try_lock_exclusive(&file) {
                Ok(()) => return Ok(Some(RegistryGuard { home, _file: file })),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(error(
                            "WORK_SESSION_BUSY",
                            "Managed workspace registry is busy; retry after the current operation",
                            "conflict",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => return Err(storage(e)),
            }
        }
    }

    fn read_records(&self, home: &Path) -> Result<Vec<Record>> {
        let mut records = Vec::new();
        for entry in fs::read_dir(home.join(REGISTRY)).map_err(storage)? {
            let entry = entry.map_err(storage)?;
            let path = entry.path();
            if path.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            if records.len() >= MAX_RECORDS {
                return Err(error("WORK_SESSION_REGISTRY_INVALID", "Too many managed workspace records; no cleanup is allowed until the registry is checked", "storage"));
            }
            let record = read_record(home, &path)?.ok_or_else(not_found)?;
            if path.file_stem().and_then(|x| x.to_str()) != Some(record.id.as_str()) {
                return Err(error(
                    "WORK_SESSION_REGISTRY_INVALID",
                    "A managed workspace record has an invalid identity, version or root",
                    "storage",
                ));
            }
            if records.iter().any(|r: &Record| {
                r.owner == record.owner && r.client_request_id == record.client_request_id
            }) {
                return Err(error(
                    "WORK_SESSION_REGISTRY_INVALID",
                    "Duplicate owner/request entries in the managed workspace registry",
                    "storage",
                ));
            }
            records.push(record);
        }
        records.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(records)
    }
}

struct RegistryGuard {
    home: PathBuf,
    _file: File,
}

fn read_record(home: &Path, path: &Path) -> Result<Option<Record>> {
    regular_or_absent(path)?;
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(storage(e)),
    };
    let mut bytes = Vec::new();
    file.take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(storage)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(error(
            "WORK_SESSION_REGISTRY_INVALID",
            "A record exceeds its size limit",
            "storage",
        ));
    }
    let record: Record = serde_json::from_slice(&bytes).map_err(|_| {
        error(
            "WORK_SESSION_REGISTRY_INVALID",
            "A record cannot be decoded; files were left untouched",
            "storage",
        )
    })?;
    if !matches!(record.schema_version, 1 | 2)
        || !is_id(&record.id)
        || record.root != home.join("work").join(&record.id)
        || record.owner.is_empty()
        || record.client_request_id.is_empty()
    {
        return Err(error(
            "WORK_SESSION_REGISTRY_INVALID",
            "Invalid record identity, version or root",
            "storage",
        ));
    }
    artifacts::validate_manifest(&record)?;
    lifecycle::validate_record(&record)?;
    Ok(Some(record))
}

pub(crate) fn is_id(value: &str) -> bool {
    value.strip_prefix("work-").is_some_and(|id| {
        id.len() == 32
            && id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn verify_root(home: &Path, record: &Record) -> Result<()> {
    let work = home.join("work");
    if !normal_dir(&work, false)? || !normal_dir(&record.root, false)? {
        return Err(error(
            "WORK_SESSION_DIRECTORY_UNAVAILABLE",
            "The registered work directory is missing; it was not recreated or replaced",
            "runtime",
        ));
    }
    if record.root != work.join(&record.id)
        || record.root.canonicalize().map_err(storage)? != record.root
    {
        return Err(error(
            "WORK_SESSION_PATH_INVALID",
            "The work directory resolves outside its registered location",
            "security",
        ));
    }
    Ok(())
}

fn normal_dir(path: &Path, create: bool) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => Err(error(
            "WORK_SESSION_PATH_INVALID",
            "A managed directory is a symlink or not a directory",
            "security",
        )),
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !create => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(path).map_err(storage)?;
            normal_dir(path, false)
        }
        Err(e) => Err(storage(e)),
    }
}

fn regular_or_absent(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.file_type().is_file() => Err(error(
            "WORK_SESSION_PATH_INVALID",
            "Managed registry files must be regular files, not links",
            "security",
        )),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(storage(e)),
    }
}

fn save(home: &Path, record: &Record) -> Result<()> {
    let path = home.join(REGISTRY).join(format!("{}.json", record.id));
    regular_or_absent(&path)?;
    let temp = path.with_extension(format!("tmp-{}", Uuid::new_v4().simple()));
    let bytes = serde_json::to_vec(record).map_err(storage)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(error(
            "WORK_SESSION_REGISTRY_LIMIT",
            "Record size limit reached; the previous metadata and source files were kept",
            "resource",
        ));
    }
    let result = (|| {
        toexec_fs::write_durable(&temp, &bytes, None)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temp, fs::Permissions::from_mode(0o600))?;
        }
        toexec_fs::replace(&temp, &path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(storage)?;
    if record.state == State::Closed {
        archive::close(home, record)?;
    }
    Ok(())
}

fn validate_text(value: &str, field: &str, max: usize, empty: bool) -> Result<()> {
    if (!empty && value.trim().is_empty())
        || value.len() > max
        || value.chars().any(char::is_control)
    {
        return Err(WorkspaceError::invalid_argument(format!(
            "{field} must be {}text of at most {max} bytes without control characters",
            if empty { "" } else { "non-empty " }
        )));
    }
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn not_found() -> WorkspaceError {
    error(
        "WORK_SESSION_NOT_FOUND",
        "No managed workspace with that ID belongs to this caller",
        "not_found",
    )
}

fn storage(error_value: impl std::fmt::Display) -> WorkspaceError {
    error(
        "WORK_SESSION_STORAGE",
        &format!("Managed workspace storage failed: {error_value}"),
        "storage",
    )
}

fn error(code: &'static str, message: &str, category: &'static str) -> WorkspaceError {
    WorkspaceError::Tool {
        code,
        message: message.into(),
        category,
        retryable: false,
    }
}

#[cfg(test)]
mod tests;

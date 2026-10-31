//! 目标机配置的互斥及私有原子写入。锁文件稳定保留，避免删锁导致两个进程各持一把锁。

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use fs2::FileExt;

use super::config::{NodeConfig, MAX_CONFIG_BYTES};

#[cfg(windows)]
#[path = "windows_private.rs"]
mod windows_private;

#[cfg(test)]
#[path = "local_config_tests.rs"]
mod tests;

/// 不写 `--config` 时的位置：数据目录下 `nodes/<Hub ID>.json`，按 Hub 分开，一台机器能接多个 Hub。
pub fn default_path(hub_id: &str) -> Result<PathBuf, String> {
    let dir = nodes_dir()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .map_err(|_| "Cannot create the node configuration directory")?;
    }
    #[cfg(not(unix))]
    fs::create_dir_all(&dir).map_err(|_| "Cannot create the node configuration directory")?;
    Ok(dir.join(format!("{hub_id}.json")))
}

/// 查看、运行、退出都用它定位配置：写了路径就用，没写就找默认目录里唯一的一份。
pub fn find(explicit: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }
    find_in(&nodes_dir()?)
}

fn nodes_dir() -> Result<PathBuf, String> {
    crate::home::data_home()
        .map(|home| home.join("nodes"))
        .map_err(|_| "Cannot locate gld's control data directory".to_string())
}

fn find_in(dir: &Path) -> Result<PathBuf, String> {
    let mut found: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(_) => return Err("Cannot read the node configuration directory".into()),
    };
    found.sort();
    match found.len() {
        0 => Err("This machine has not joined a Hub; run gld join first, or pass --config".into()),
        1 => Ok(found.remove(0)),
        _ => Err(format!(
            "This machine has {} node configurations; pick one with --config: {}",
            found.len(),
            found
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

const IN_USE: &str =
    "Node configuration is in use; stop the node or wait for the other configuration command";

pub struct LocalConfig {
    path: PathBuf,
    pending: PathBuf,
    _lock: File,
}

impl LocalConfig {
    pub fn lock(path: &Path) -> Result<Self, String> {
        let filename = path
            .file_name()
            .ok_or("Node configuration needs a file name")?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent = parent
            .canonicalize()
            .map_err(|_| "Node configuration directory must already exist")?;
        let path = parent.join(filename);
        let mut lock_name = filename.to_os_string();
        lock_name.push(".lock");
        let lock_path = parent.join(lock_name);
        let lock = match create_private_file(&lock_path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                open_private_file(&lock_path, true)?
            }
            Err(_) => return Err("Cannot create node configuration lock".into()),
        };
        check_private_file(&lock)?;
        lock.try_lock_exclusive().map_err(|_| IN_USE)?;
        let mut pending_name = filename.to_os_string();
        pending_name.push(".pending");
        Ok(Self {
            path,
            pending: parent.join(pending_name),
            _lock: lock,
        })
    }

    /// 节点进程全程持锁，所以这就是“节点在不在跑”，不管它是前台起的还是服务管理器起的。
    pub fn in_use(path: &Path) -> Result<bool, String> {
        match Self::lock(path) {
            Ok(_) => Ok(false),
            Err(error) if error == IN_USE => Ok(true),
            Err(error) => Err(error),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn exists(&self) -> Result<bool, String> {
        path_exists(&self.path)
    }

    pub fn load(&self) -> Result<NodeConfig, String> {
        read_config(&self.path)
    }

    pub fn load_pending(&self) -> Result<Option<NodeConfig>, String> {
        if path_exists(&self.pending)? {
            read_config(&self.pending).map(Some)
        } else {
            Ok(None)
        }
    }

    pub fn save_pending(&self, config: &NodeConfig) -> Result<(), String> {
        config.validate()?;
        if self.exists()? {
            return Err("Node configuration already exists; it was not replaced".into());
        }
        if let Some(saved) = self.load_pending()? {
            if same_config(&saved, config) {
                return Ok(());
            }
            return Err(
                "Pending node pairing belongs to different settings; it was not replaced".into(),
            );
        }
        write_new(&self.pending, config)
    }

    pub fn finish(&self, config: &NodeConfig) -> Result<(), String> {
        let pending = self
            .load_pending()?
            .ok_or("Pending node pairing is missing")?;
        if !same_config(&pending, config) {
            return Err("Pending node pairing changed; configuration was not replaced".into());
        }
        // 同目录 hard_link 提供无覆盖发布：目标已存在时保留两边，而不是先检查再 rename 覆盖。
        fs::hard_link(&self.pending, &self.path).map_err(|_| "Cannot save paired node configuration without replacing an existing file; pending state was kept")?;
        fs::remove_file(&self.pending).map_err(|_| {
            "Paired node configuration was saved, but its pending file could not be removed"
        })?;
        sync_parent(&self.path)
    }

    /// 删掉配置和没配对完的 pending，返回被删的配置。锁文件留着：删了锁，别的进程可能各持一把。
    /// 先读出来校验，免得 `--config` 写错路径时把不相干的文件删了。
    pub fn remove(&self) -> Result<NodeConfig, String> {
        let config = if self.exists()? {
            self.load()?
        } else {
            self.load_pending()?
                .ok_or("No node configuration or pending pairing here")?
        };
        for path in [&self.path, &self.pending] {
            if path_exists(path)? {
                fs::remove_file(path).map_err(|_| "Cannot remove the node configuration")?;
            }
        }
        sync_parent(&self.path)?;
        Ok(config)
    }

    /// 调用方持有同一把锁，校验配置后整份原子替换。
    pub fn replace(&self, config: &NodeConfig) -> Result<(), String> {
        self.load()?;
        config.validate()?;
        let temp = write_temp(&self.path, config)?;
        if fs::rename(&temp, &self.path).is_err() {
            let _ = fs::remove_file(&temp);
            return Err(
                "Cannot replace node configuration; previous configuration was kept".into(),
            );
        }
        sync_parent(&self.path)
    }
}

fn same_config(a: &NodeConfig, b: &NodeConfig) -> bool {
    a.hub_url == b.hub_url
        && a.hub_id == b.hub_id
        && a.hub_public_key == b.hub_public_key
        && a.machine_id == b.machine_id
        && a.credential == b.credential
        && a.workspaces == b.workspaces
        && a.writable_workspaces == b.writable_workspaces
        && a.runnable_workspaces == b.runnable_workspaces
        && a.ca_certificate == b.ca_certificate
}

/// 私有配置及同目录 pending/lock 不得放进共享项目，避免凭据泄露或远端改变下一次启动的范围。
pub fn validate_config_location(path: &Path, project_paths: &[String]) -> Result<(), String> {
    let data_home = crate::home::data_home()
        .map_err(|_| "Cannot locate gld's control data directory".to_string())?;
    validate_config_location_in(path, project_paths, &data_home)
}

fn validate_config_location_in(
    path: &Path,
    project_paths: &[String],
    data_home: &Path,
) -> Result<(), String> {
    let filename = path
        .file_name()
        .ok_or("Node configuration needs a file name")?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()
        .map_err(|_| "Node configuration directory must already exist")?;
    let config_path = parent.join(filename);
    let data_home = data_home
        .canonicalize()
        .map_err(|_| "Cannot resolve gld's control data directory")?;
    if !data_home.is_dir() {
        return Err("The control data path for this node must be an existing directory".into());
    }
    for project_path in project_paths {
        let root = Path::new(project_path).canonicalize().map_err(|_| {
            "Cannot resolve a shared project directory for node configuration safety"
        })?;
        if !root.is_dir() {
            return Err("Every shared project path must be an existing directory".into());
        }
        if root.starts_with(&data_home) || data_home.starts_with(&root) {
            return Err(
                "Node shared projects must be separate from gld's control data directory".into(),
            );
        }
        if config_path.starts_with(&root) {
            return Err(
                "Private node configuration must be outside every shared project directory".into(),
            );
        }
    }
    Ok(())
}

fn path_exists(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err("Cannot inspect node configuration path".into()),
    }
}

pub fn read_config(path: &Path) -> Result<NodeConfig, String> {
    let file = open_private_file(path, false)?;
    check_private_file(&file)?;
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read private node configuration")?;
    if bytes.len() > MAX_CONFIG_BYTES as usize {
        return Err("Node configuration exceeds 64 KiB".into());
    }
    let mut config: NodeConfig =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid private node configuration JSON")?;
    if let Some(ca) = &mut config.ca_certificate {
        if ca.is_relative() {
            *ca = path.parent().expect("configuration has parent").join(&*ca);
        }
    }
    config.validate()?;
    Ok(config)
}

fn open_private_file(path: &Path, writable: bool) -> Result<File, String> {
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
    options
        .open(path)
        .map_err(|_| "Cannot open private node configuration".into())
}

#[cfg(windows)]
pub(crate) fn create_private_file(path: &Path) -> std::io::Result<File> {
    windows_private::create_new_private(path)
}

#[cfg(not(windows))]
pub(crate) fn create_private_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    options.open(path)
}

pub(crate) fn check_private_file(file: &File) -> Result<(), String> {
    #[cfg(windows)]
    windows_private::check_private(file).map_err(|_| {
        "Node configuration requires a protected ACL granting access only to this account"
    })?;
    let metadata = file
        .metadata()
        .map_err(|_| "Cannot inspect private node configuration")?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Node configuration must be a regular private file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o077 != 0 || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(
                "Node configuration must be owned by this account and private (chmod 600)".into(),
            );
        }
    }
    Ok(())
}

fn write_temp(target: &Path, config: &NodeConfig) -> Result<PathBuf, String> {
    let bytes =
        serde_json::to_vec_pretty(config).map_err(|_| "Cannot serialize node configuration")?;
    if bytes.len() > MAX_CONFIG_BYTES as usize {
        return Err("Node configuration exceeds 64 KiB".into());
    }
    let temp = target.with_file_name(format!(".gld-node-{}.tmp", uuid::Uuid::new_v4().simple()));
    let mut file =
        create_private_file(&temp).map_err(|_| "Cannot create private node configuration")?;
    let result = file.write_all(&bytes).and_then(|()| file.sync_all());
    drop(file);
    if result.is_err() {
        let _ = fs::remove_file(&temp);
        return Err("Cannot persist private node configuration".into());
    }
    Ok(temp)
}

fn write_new(target: &Path, config: &NodeConfig) -> Result<(), String> {
    let temp = write_temp(target, config)?;
    let result = fs::hard_link(&temp, target);
    let _ = fs::remove_file(&temp);
    result.map_err(|_| "Cannot save node pairing state without replacing an existing file")?;
    sync_parent(target)
}

fn sync_parent(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    File::open(path.parent().expect("configuration has parent"))
        .and_then(|file| file.sync_all())
        .map_err(|_| {
            "Node configuration was written, but its directory could not be synchronized"
        })?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

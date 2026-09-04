//! 守护进程的文件约定与生命周期：在哪监听、怎么判断它活着、怎么拉起和停掉。
//!
//! 数据目录下的三个文件：
//!
//! | 文件 | 作用 |
//! | --- | --- |
//! | `daemon.sock` | IPC 入口（Windows 用命名管道，此路径只作为名字来源） |
//! | `daemon.lock` | `flock` 排他锁，保证同一数据目录只有一个守护进程 |
//! | `daemon.json` | pid / 版本 / 启动时间，socket 没响应时靠它判断“僵尸还是没跑” |
//!
//! 判断“守护进程是否在跑”只信 socket：能连上并回应 `ping` 才算活。
//! pid 文件只用于给用户看和在 socket 失联时补充信息。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use gld_core::platform::platform;
use gld_core::{AppError, AppResult};
use serde::{Deserialize, Serialize};

use crate::client::Client;
use crate::protocol::{DaemonInfo, Request};

/// Unix socket 路径在 macOS 上最多约 104 字节；更长就退到临时目录。
///
/// Windows 走命名管道，没有这个长度限制，也就用不到这个常量——
/// 不加 cfg 的话 Windows 上是 dead_code，而 CI 开着 `-D warnings`。
#[cfg(not(windows))]
const MAX_UNIX_SOCKET_PATH: usize = 100;

#[derive(Debug, Clone)]
pub struct DaemonPaths {
    pub home: PathBuf,
    pub socket: PathBuf,
    pub lock: PathBuf,
    pub record: PathBuf,
    pub log: PathBuf,
}

impl DaemonPaths {
    /// 按当前 `GLD_HOME` 解析。
    pub fn resolve() -> AppResult<Self> {
        Self::for_home(gld_core::home::data_home()?)
    }

    pub fn for_home(home: PathBuf) -> AppResult<Self> {
        Ok(Self {
            socket: socket_path(&home),
            lock: home.join("daemon.lock"),
            record: home.join("daemon.json"),
            log: home.join("logs").join("daemon.log"),
            home,
        })
    }
}

fn socket_path(home: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        return PathBuf::from(format!(r"\\.\pipe\gld-{}", short_hash(home)));
    }
    #[cfg(not(windows))]
    {
        let preferred = home.join("daemon.sock");
        if preferred.as_os_str().len() <= MAX_UNIX_SOCKET_PATH {
            preferred
        } else {
            std::env::temp_dir().join(format!("gld-{}.sock", short_hash(home)))
        }
    }
}

fn short_hash(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(path.to_string_lossy().as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// `daemon.json` 的内容。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonRecord {
    pub pid: u32,
    pub version: String,
    pub protocol: u32,
    pub started_at_unix: u64,
    pub socket: PathBuf,
    pub log: PathBuf,
}

impl DaemonRecord {
    pub fn write(&self, path: &Path) -> AppResult<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(path, format!("{text}\n"))?;
        Ok(())
    }

    pub fn read(path: &Path) -> Option<Self> {
        let raw = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&raw).ok()
    }
}

/// 探活结果。
#[derive(Debug, Clone)]
pub enum DaemonProbe {
    /// socket 能连、ping 有回应。
    Running(DaemonInfo),
    /// 有 pid 记录且进程还活着，但 socket 不响应（启动中或卡死）。
    Unresponsive(DaemonRecord),
    /// 有残留记录但进程已不在。
    Stale(DaemonRecord),
    NotRunning,
}

impl DaemonProbe {
    pub fn is_running(&self) -> bool {
        matches!(self, DaemonProbe::Running(_))
    }
}

pub async fn probe(paths: &DaemonPaths) -> DaemonProbe {
    let client = Client::new(paths.clone()).with_timeout(Duration::from_secs(3));
    if let Ok(info) = client.daemon_info().await {
        return DaemonProbe::Running(info);
    }
    match DaemonRecord::read(&paths.record) {
        Some(record) if platform().is_process_alive(record.pid) => {
            DaemonProbe::Unresponsive(record)
        }
        Some(record) => DaemonProbe::Stale(record),
        None => DaemonProbe::NotRunning,
    }
}

/// 拿单实例锁。返回的 `File` 必须活到进程退出，drop 即释放。
pub fn acquire_instance_lock(paths: &DaemonPaths) -> AppResult<File> {
    std::fs::create_dir_all(&paths.home)?;
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&paths.lock)?;
    file.try_lock_exclusive().map_err(|_| {
        let hint = DaemonRecord::read(&paths.record)
            .map(|record| format!("（pid {}）", record.pid))
            .unwrap_or_default();
        AppError::Message(format!(
            "已有另一个守护进程持有 {}{hint}。若确认它已死，删除该文件后重试。",
            paths.lock.display()
        ))
    })?;
    Ok(file)
}

/// 在后台拉起 `<exe> daemon run`，stdout / stderr 追加到 `logs/daemon.log`。
///
/// 子进程脱离当前终端会话（Unix 走 `setsid`，Windows 走 `DETACHED_PROCESS`），
/// 关掉终端不会把它一起带走。
pub fn spawn_detached(paths: &DaemonPaths, executable: &Path) -> AppResult<u32> {
    std::fs::create_dir_all(paths.log.parent().unwrap_or(&paths.home))?;
    // 守护进程的 stdout / stderr 是启动时重定向过去的，进程内没法在写入时插手，
    // 只能趁每次启动前检查一下大小。
    gld_core::logs::rotate_if_oversized(&paths.log);
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&paths.log)?;
    writeln!(
        log,
        "{} INFO  spawning daemon from {}",
        crate::logging::timestamp(),
        executable.display()
    )?;
    let stdout = log.try_clone()?;
    let stderr = log;

    let mut command = Command::new(executable);
    command
        .args(["daemon", "run"])
        .env(gld_core::home::HOME_ENV, &paths.home)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .current_dir(&paths.home);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // 新会话 + 新进程组：既不接收终端的 SIGHUP，也不会被 Ctrl-C 波及。
        unsafe {
            command.pre_exec(|| {
                if libc_setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }

    let child = command
        .spawn()
        .map_err(|error| AppError::Message(format!("无法启动守护进程：{error}")))?;
    Ok(child.id())
}

#[cfg(unix)]
fn libc_setsid() -> i32 {
    extern "C" {
        fn setsid() -> i32;
    }
    unsafe { setsid() }
}

/// 等待守护进程开始响应，超时返回 `Err`。
pub async fn wait_until_ready(paths: &DaemonPaths, timeout: Duration) -> AppResult<DaemonInfo> {
    let deadline = Instant::now() + timeout;
    let client = Client::new(paths.clone()).with_timeout(Duration::from_secs(2));
    loop {
        if let Ok(info) = client.daemon_info().await {
            return Ok(info);
        }
        if Instant::now() >= deadline {
            return Err(AppError::Message(format!(
                "守护进程在 {} 秒内没有就绪，请查看日志：{}",
                timeout.as_secs(),
                paths.log.display()
            )));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// 请求守护进程退出，并等它真正消失。`force` 时超时后直接杀进程树。
pub async fn request_stop(
    paths: &DaemonPaths,
    timeout: Duration,
    force: bool,
) -> AppResult<StopOutcome> {
    let probe = probe(paths).await;
    let pid = match &probe {
        DaemonProbe::Running(info) => info.pid,
        DaemonProbe::Unresponsive(record) => record.pid,
        DaemonProbe::Stale(record) => {
            cleanup_stale(paths);
            return Ok(StopOutcome::WasStale(record.pid));
        }
        DaemonProbe::NotRunning => return Ok(StopOutcome::NotRunning),
    };

    if probe.is_running() {
        let client = Client::new(paths.clone()).with_timeout(Duration::from_secs(5));
        let _ = client.call(&Request::Shutdown).await;
    }

    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !platform().is_process_alive(pid) {
            cleanup_stale(paths);
            return Ok(StopOutcome::Stopped(pid));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    if force {
        platform().terminate_process_tree(pid)?;
        tokio::time::sleep(Duration::from_millis(300)).await;
        cleanup_stale(paths);
        return Ok(StopOutcome::Killed(pid));
    }
    Err(AppError::Message(format!(
        "守护进程（pid {pid}）在 {} 秒内没有退出。可加 --force 强制结束。",
        timeout.as_secs()
    )))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    Stopped(u32),
    Killed(u32),
    WasStale(u32),
    NotRunning,
}

/// 清理上次异常退出留下的 socket / 记录文件。
pub fn cleanup_stale(paths: &DaemonPaths) {
    crate::ipc::cleanup(&paths.socket);
    let _ = std::fs::remove_file(&paths.record);
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_falls_back_when_home_is_too_deep() {
        let short = DaemonPaths::for_home(PathBuf::from("/tmp/gld")).unwrap();
        // "路径太长要退到临时目录"是 Unix domain socket 独有的问题，
        // Windows 命名管道没有它，所以那个用例只在非 Windows 上构造，
        // 否则 Windows 上是个未使用变量（CI 开着 `-D warnings`）。
        #[cfg(not(windows))]
        {
            let deep =
                DaemonPaths::for_home(PathBuf::from(format!("/tmp/{}", "x".repeat(150)))).unwrap();
            assert_eq!(short.socket, PathBuf::from("/tmp/gld/daemon.sock"));
            assert!(deep.socket.as_os_str().len() <= MAX_UNIX_SOCKET_PATH + 20);
            assert_ne!(deep.socket.parent(), Some(deep.home.as_path()));
        }
        #[cfg(windows)]
        {
            assert!(short.socket.to_string_lossy().starts_with(r"\\.\pipe\gld-"));
        }
    }

    #[test]
    fn instance_lock_is_exclusive() {
        let temp = tempfile::tempdir().unwrap();
        let paths = DaemonPaths::for_home(temp.path().to_path_buf()).unwrap();
        let first = acquire_instance_lock(&paths).unwrap();
        assert!(acquire_instance_lock(&paths).is_err());
        drop(first);
        assert!(acquire_instance_lock(&paths).is_ok());
    }
}

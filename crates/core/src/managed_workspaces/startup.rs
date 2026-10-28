//! 每次真实监听器启动后做一轮到期检查；不依赖客户端请求、不重放命令，也不是定时器。

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const LIMIT: usize = 16;
const REPORT: &str = ".startup-sweep";

pub(crate) struct StartupSweep {
    cancelled: Arc<AtomicBool>,
    task: crate::async_rt::JoinHandle<()>,
}

impl StartupSweep {
    pub fn launch(config: &crate::settings::HubConfig) -> Option<Self> {
        if !config.work_sessions
            || config.auth_type == "noauth"
            || config.tool_profile == "read-only"
        {
            return None;
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&cancelled);
        let limits = config.work_session_limits.clone();
        let task = crate::async_rt::handle().spawn_blocking(move || {
            let result = crate::home::data_home()
                .map_err(storage)
                .and_then(|home| run_once(&Store::new(home).with_limits(limits), &stop, LIMIT));
            match result {
                Ok(report) => eprintln!(
                    "通用任务启动检查：检查 {}、清理 {}、阻塞 {}、取消 {}。",
                    report.examined, report.removed, report.blocked, report.cancelled
                ),
                // 不输出任务路径、owner、请求正文；具体任务可由所属主体查询 status。
                Err(error) => eprintln!(
                    "通用任务启动检查未完成（{}），保留未核实目录。",
                    error.code()
                ),
            }
        });
        Some(Self { cancelled, task })
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub async fn join(self) {
        if self.task.await.is_err() {
            eprintln!("通用任务启动检查异常退出；下次启动重新核实，不重放命令。");
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Report {
    version: u32,
    cursor: Option<String>,
    completed_at_ms: u64,
    examined: usize,
    removed: usize,
    blocked: usize,
    cancelled: bool,
    error_codes: Vec<String>,
}

impl Default for Report {
    fn default() -> Self {
        Self {
            version: 1,
            cursor: None,
            completed_at_ms: 0,
            examined: 0,
            removed: 0,
            blocked: 0,
            cancelled: false,
            error_codes: Vec::new(),
        }
    }
}

fn run_once(store: &Store, cancelled: &AtomicBool, limit: usize) -> Result<Report> {
    if cancelled.load(Ordering::Acquire) {
        return Ok(Report {
            cancelled: true,
            ..Report::default()
        });
    }
    let Some(registry) = store.lock(false)? else {
        return Ok(Report::default());
    };
    let home = registry.home.clone();
    drop(registry);
    // 独立于 registry/逐空间锁，防止同一数据目录被两个服务同时做启动检查。
    let lock_path = home.join(REGISTRY).join(".startup-lock");
    regular_or_absent(&lock_path)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let lock = options.open(lock_path).map_err(storage)?;
    FileExt::try_lock_exclusive(&lock).map_err(|_| {
        error(
            "WORK_SESSION_BUSY",
            "A startup sweep already owns this data directory",
            "conflict",
        )
    })?;
    let path = home.join(REGISTRY).join(REPORT);
    let previous = read_report(&path)?;
    let records = store.live()?;
    let now = now_ms();
    let eligible =
        |r: &&Record| r.state != State::Closed && r.expires_at_ms.is_some_and(|at| at <= now);
    // 游标在成功、受阻两种情况下都推进；下一轮绕回，避免固定的前16项饿死后续目录。
    let cursor = previous.cursor.as_deref();
    let selected = records
        .iter()
        .filter(eligible)
        .filter(|r| cursor.is_none_or(|id| r.id.as_str() > id))
        .chain(
            records
                .iter()
                .filter(eligible)
                .filter(|r| cursor.is_some_and(|id| r.id.as_str() <= id)),
        )
        .take(limit.min(LIMIT));
    let mut report = Report {
        cursor: previous.cursor.clone(),
        ..Report::default()
    };
    for record in selected {
        if cancelled.load(Ordering::Acquire) {
            report.cancelled = true;
            break;
        }
        report.examined += 1;
        // owner 只能取服务登记，启动检查不接受任何客户端指定的其他主体身份。
        match store.expire(record, None, now) {
            Ok(true) => report.removed += 1,
            Ok(false) => {}
            Err(error) => {
                report.blocked += 1;
                let code = error.code().to_string();
                if !report.error_codes.contains(&code) {
                    report.error_codes.push(code);
                }
            }
        }
        report.cursor = Some(record.id.clone());
    }
    report.cancelled |= cancelled.load(Ordering::Acquire);
    report.completed_at_ms = now_ms();
    write_report(&path, &report)?;
    Ok(report)
}

fn read_report(path: &Path) -> Result<Report> {
    regular_or_absent(path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Report::default()),
        Err(e) => return Err(storage(e)),
    };
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes).map_err(storage)?;
    let invalid = || {
        error(
            "WORK_SESSION_SWEEP_STATE_INVALID",
            "Startup sweep report is invalid; no cleanup started",
            "storage",
        )
    };
    if bytes.len() > 4096 {
        return Err(invalid());
    }
    let report: Report = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if report.version != 1 || report.cursor.as_deref().is_some_and(|id| !is_id(id)) {
        return Err(invalid());
    }
    Ok(report)
}

fn write_report(path: &Path, report: &Report) -> Result<()> {
    regular_or_absent(path)?;
    let temp = path.with_extension(format!("tmp-{}", Uuid::new_v4().simple()));
    let bytes = serde_json::to_vec(report).map_err(storage)?;
    let result = (|| {
        toexec_fs::write_durable(&temp, &bytes, None)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temp, fs::Permissions::from_mode(0o600))?;
        }
        toexec_fs::replace(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(storage)
}

#[cfg(test)]
mod tests;

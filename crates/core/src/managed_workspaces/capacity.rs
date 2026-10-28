//! 预算只控制 gld 的新增目录和产物复制；不宣称能限制任意子进程的写入。

use super::*;
use serde_json::{json, Value};

const MAX_USAGE_ENTRIES: usize = 100_000;

fn usage(home: &Path) -> Result<u64> {
    let mut bytes = 0u64;
    let mut entries = 0;
    for name in ["work", "artifacts"] {
        let root = home.join(name);
        if !normal_dir(&root, false)? {
            continue;
        }
        for item in walkdir::WalkDir::new(root).follow_links(false) {
            let item = item.map_err(|e| {
                error(
                    "WORK_SESSION_CAPACITY_UNVERIFIED",
                    &format!("Cannot measure task storage: {e}; no data was deleted"),
                    "storage",
                )
            })?;
            entries += 1;
            if entries > MAX_USAGE_ENTRIES {
                return Err(error(
                    "WORK_SESSION_CAPACITY_UNVERIFIED",
                    "Storage scan exceeded 100000 entries; usage is unknown, not zero",
                    "resource",
                ));
            }
            if item.file_type().is_file() {
                bytes = bytes
                    .checked_add(item.metadata().map_err(storage)?.len())
                    .ok_or_else(|| {
                        error(
                            "WORK_SESSION_CAPACITY_UNVERIFIED",
                            "Storage size overflow",
                            "resource",
                        )
                    })?;
            }
        }
    }
    Ok(bytes)
}

pub(super) fn require_space(home: &Path, maximum: u64, additional: u64) -> Result<()> {
    let used = usage(home)?;
    if used
        .checked_add(additional)
        .is_none_or(|total| total > maximum)
        || (additional == 0 && used >= maximum)
    {
        return Err(WorkspaceError::ToolDetails {
            code: "WORK_SESSION_STORAGE_LIMIT",
            message: "Task storage budget exceeded; existing files and receipts were kept. Read/export results or finish disposable work, then retry or adjust the limit locally".into(),
            category: "resource", retryable: false,
            details: json!({"used_bytes":used,"additional_bytes":additional,"max_bytes":maximum}),
        });
    }
    Ok(())
}

/// 将服务直接执行的两次保存串行化；复制与盘上登记完成前不能释放。
pub(super) fn copy_guard(home: &Path) -> Result<File> {
    let path = home.join(REGISTRY).join(".capacity.lock");
    regular_or_absent(&path)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path).map_err(storage)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(file),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                return Err(error(
                    "WORK_SESSION_CAPACITY_BUSY",
                    "Another artifact save holds the storage budget; retry after it completes",
                    "conflict",
                ));
            }
            Err(e) => return Err(storage(e)),
        }
    }
}

impl Store {
    pub fn capacity(&self) -> Result<Value> {
        self.limits
            .validate()
            .map_err(WorkspaceError::invalid_argument)?;
        let (active, closed, bytes) = match self.lock(false)? {
            None => (0, 0, usage(&self.home)?),
            Some(guard) => {
                let records = self.read_records(&guard.home)?;
                let active = records.iter().filter(|r| r.state != State::Closed).count();
                let archived = archive::count(&guard.home)?;
                (
                    active,
                    records.len() - active + archived,
                    usage(&guard.home)?,
                )
            }
        };
        Ok(json!({
            "limits":self.limits,
            "usage":{"active":active,"closed":closed,"records":active+closed,"bytes":bytes},
            "scope":"aggregate for this GLD_HOME; logical regular-file bytes under work/ and artifacts/ only",
            "hard_filesystem_quota":false,
            "note":"Admission and artifact-copy budget, not a limit on arbitrary command writes. Reads, retries and cleanup remain available; no retained results are evicted."
        }))
    }
}

#[cfg(test)]
mod tests;

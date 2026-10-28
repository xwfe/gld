//! 每个托管目录的操作锁。文件调用、保存及后续删除用同一把锁，关闭不能越过正在开始的命令。

use super::*;

pub(crate) struct Operation {
    pub record: Record,
    pub(super) home: PathBuf,
    pub(super) store: Store,
    _file: File,
}

impl Store {
    pub fn inspect(&self, owner: &str, id: &str) -> Result<Record> {
        if !is_id(id) {
            return Err(not_found());
        }
        let guard = self.lock(false)?.ok_or_else(not_found)?;
        let record = match read_record(
            &guard.home,
            &guard.home.join(REGISTRY).join(format!("{id}.json")),
        )? {
            Some(record) if record.id == id => Some(record),
            Some(_) => {
                return Err(error(
                    "WORK_SESSION_REGISTRY_INVALID",
                    "Record ID does not match its path",
                    "storage",
                ))
            }
            None => archive::by_id(&guard.home, id)?,
        };
        record.filter(|r| r.owner == owner).ok_or_else(not_found)
    }

    pub fn operation(&self, owner: &str, id: &str) -> Result<Operation> {
        // 先验归属，不能靠猜 ID 建锁文件或探测别人的目录。
        self.inspect(owner, id)?;
        let home = self.home.canonicalize().map_err(storage)?;
        let root = home.join(REGISTRY).join(".operations");
        normal_dir(&root, true)?;
        let path = root.join(format!("{id}.lock"));
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
                Ok(()) => break,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    return Err(error("WORK_SESSION_BUSY", "Another operation still owns this work directory; retry after it completes", "conflict"));
                }
                Err(e) => return Err(storage(e)),
            }
        }
        Ok(Operation {
            record: self.inspect(owner, id)?,
            home,
            store: self.clone(),
            _file: file,
        })
    }
}

impl Operation {
    pub fn require_active(&self) -> Result<()> {
        if self.record.state != State::Active {
            return Err(error("WORK_SESSION_NOT_ACTIVE", "This work directory is closing or closed; saved results remain available through work_session artifact", "conflict"));
        }
        verify_root(&self.home, &self.record)
    }

    pub fn touch(&mut self) -> Result<()> {
        self.require_active()?;
        self.record.last_used_at_ms = now_ms();
        if let Some(ttl) = self.record.idle_retention_ms {
            self.record.expires_at_ms = Some(self.record.last_used_at_ms.saturating_add(ttl));
        }
        self.persist()
    }

    pub(super) fn persist(&self) -> Result<()> {
        let guard = self.store.lock(false)?.ok_or_else(not_found)?;
        save(&guard.home, &self.record)
    }
}

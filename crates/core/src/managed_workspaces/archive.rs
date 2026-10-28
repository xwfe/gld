//! 关闭回执按 owner/request 的摘要落盘；ID 索引只负责定位，不是第二份事实。
//! 先落索引再原子移动回执，中断时回执仍在旧位置或新位置，不会重新执行原请求。

use super::*;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

const CLOSED: &str = "closed";
const IDS: &str = "closed-ids";

fn key(owner: &str, request: &str) -> String {
    // 元组编码保留字段边界，不用可产生歧义的字符串拼接。
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(owner, request)).expect("string tuple"))
    )
}

fn invalid(message: &str) -> WorkspaceError {
    error("WORK_SESSION_ARCHIVE_INVALID", message, "storage")
}

fn directory(home: &Path, name: &str, create: bool) -> Result<Option<PathBuf>> {
    let path = home.join(REGISTRY).join(name);
    Ok(normal_dir(&path, create)?.then_some(path))
}

fn entries(home: &Path) -> Result<Vec<PathBuf>> {
    let Some(root) = directory(home, CLOSED, false)? else {
        return Ok(Vec::new());
    };
    let mut paths = Vec::new();
    for entry in fs::read_dir(root).map_err(storage)? {
        let entry = entry.map_err(storage)?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            if paths.len() >= MAX_RECORDS {
                return Err(invalid("Too many archived receipts; none were removed"));
            }
            regular_or_absent(&path)?;
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

pub(super) fn count(home: &Path) -> Result<usize> {
    Ok(entries(home)?.len())
}

fn validate(record: &Record, digest: &str) -> Result<()> {
    if record.state != State::Closed
        || record.finish_paths.is_none()
        || !record.resource_tracking
        || !record.pending_commands.is_empty()
        || key(&record.owner, &record.client_request_id) != digest
    {
        return Err(invalid(
            "An archived receipt is not closed or does not match its request",
        ));
    }
    Ok(())
}

fn read_index(home: &Path, id: &str) -> Result<Option<String>> {
    let Some(root) = directory(home, IDS, false)? else {
        return Ok(None);
    };
    let path = root.join(id);
    regular_or_absent(&path)?;
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(storage(e)),
    };
    let mut value = String::new();
    file.take(65).read_to_string(&mut value).map_err(storage)?;
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid(
            "Invalid closed receipt index; it was not overwritten",
        ));
    }
    Ok(Some(value))
}

fn ensure_index(home: &Path, record: &Record) -> Result<()> {
    let digest = key(&record.owner, &record.client_request_id);
    if let Some(existing) = read_index(home, &record.id)? {
        if existing != digest {
            return Err(invalid("Closed receipt index points at another request"));
        }
        return Ok(());
    }
    let root = directory(home, IDS, true)?.expect("created index directory");
    let target = root.join(&record.id);
    let temp = root.join(format!(".index-{}", Uuid::new_v4().simple()));
    let result = (|| {
        toexec_fs::write_durable(&temp, digest.as_bytes(), None)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temp, fs::Permissions::from_mode(0o600))?;
        }
        toexec_fs::replace(&temp, &target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(storage)
}

pub(super) fn by_request(home: &Path, owner: &str, request: &str) -> Result<Option<Record>> {
    let Some(root) = directory(home, CLOSED, false)? else {
        return Ok(None);
    };
    let digest = key(owner, request);
    let Some(record) = read_record(home, &root.join(format!("{digest}.json")))? else {
        return Ok(None);
    };
    validate(&record, &digest)?;
    if record.owner != owner || record.client_request_id != request {
        return Err(invalid("Archived request identity does not match"));
    }
    // 索引缺失时可由完整回执恢复；不能因索引丢失创建另一个同请求的任务。
    ensure_index(home, &record)?;
    Ok(Some(record))
}

pub(super) fn by_id(home: &Path, id: &str) -> Result<Option<Record>> {
    let Some(digest) = read_index(home, id)? else {
        return Ok(None);
    };
    let root = directory(home, CLOSED, false)?
        .ok_or_else(|| invalid("Archived receipt directory is missing"))?;
    let record = read_record(home, &root.join(format!("{digest}.json")))?
        .ok_or_else(|| invalid("Closed receipt index has no matching receipt"))?;
    validate(&record, &digest)?;
    if record.id != id {
        return Err(invalid("Closed receipt ID does not match its index"));
    }
    Ok(Some(record))
}

/// 调用方持有登记锁；这里只移动已 closed 的元数据，不删除任务文件或保留产物。
pub(super) fn close(home: &Path, record: &Record) -> Result<()> {
    let digest = key(&record.owner, &record.client_request_id);
    validate(record, &digest)?;
    match fs::symlink_metadata(&record.root) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(storage(e)),
        Ok(_) => {
            return Err(invalid(
                "Closed task directory still exists; it was not hidden by archiving",
            ))
        }
    }
    let root = directory(home, CLOSED, true)?.expect("created archive");
    let target = root.join(format!("{digest}.json"));
    let source = home.join(REGISTRY).join(format!("{}.json", record.id));
    if let Some(existing) = read_record(home, &target)? {
        if existing != *record {
            return Err(invalid(
                "Conflicting archived receipt; neither copy was overwritten",
            ));
        }
        ensure_index(home, record)?;
        if let Some(live) = read_record(home, &source)? {
            if live != *record {
                return Err(invalid("Conflicting unarchived receipt"));
            }
            fs::remove_file(source).map_err(storage)?;
        }
        return Ok(());
    }
    ensure_index(home, record)?;
    // 与主登记处于同一数据目录。跨文件系统或权限失败时源回执保留，重试可继续。
    fs::rename(source, target).map_err(storage)
}

/// 显式列表和 prune 才读完整历史；同一次原子迁移留下的相同副本只计一条。
pub(super) fn append(home: &Path, records: &mut Vec<Record>) -> Result<()> {
    let mut ids: HashMap<String, usize> = records
        .iter()
        .enumerate()
        .map(|(i, r)| (r.id.clone(), i))
        .collect();
    let mut requests: HashSet<String> = records
        .iter()
        .map(|r| key(&r.owner, &r.client_request_id))
        .collect();
    for path in entries(home)? {
        let digest = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| invalid("Invalid archive name"))?;
        let record =
            read_record(home, &path)?.ok_or_else(|| invalid("Archived receipt disappeared"))?;
        validate(&record, digest)?;
        if read_index(home, &record.id)?.as_deref() != Some(digest) {
            return Err(invalid("Archived receipt index is missing or inconsistent"));
        }
        if let Some(index) = ids.get(&record.id) {
            if records[*index] != record {
                return Err(invalid("Conflicting active and archived records"));
            }
            continue;
        }
        if !requests.insert(digest.into()) {
            return Err(invalid("Duplicate archived request"));
        }
        if records.len() >= MAX_RECORDS {
            return Err(invalid("Total receipt scan limit exceeded"));
        }
        ids.insert(record.id.clone(), records.len());
        records.push(record);
    }
    records.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(())
}

#[cfg(test)]
mod tests;

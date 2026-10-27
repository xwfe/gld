//! 只保存明确选中的普通文件：复制、读回核验、登记，原文件始终留到 finish 决定删除。

use super::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Write;

const MAX_ARTIFACTS: usize = 32;
const MAX_ARTIFACT_BYTES: u64 = 256 * 1024 * 1024;
const MAX_TEXT_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Artifact {
    pub id: String,
    pub source: String,
    pub bytes: u64,
    pub sha256: String,
    pub mime_type: String,
}

pub(super) fn validate_manifest(record: &Record) -> Result<()> {
    if record.artifacts.len() > MAX_ARTIFACTS {
        return Err(error(
            "WORK_SESSION_REGISTRY_INVALID",
            "Too many artifacts in record",
            "storage",
        ));
    }
    let mut ids = std::collections::HashSet::new();
    for item in &record.artifacts {
        if relative_parts(&item.source).is_err()
            || item.bytes > MAX_ARTIFACT_BYTES
            || item.sha256.len() != 64
            || !item.sha256.bytes().all(|b| b.is_ascii_hexdigit())
            || item.id != artifact_id(&record.id, &item.source, &item.sha256)
            || !ids.insert(&item.id)
        {
            return Err(error(
                "WORK_SESSION_REGISTRY_INVALID",
                "Invalid artifact identity or manifest",
                "storage",
            ));
        }
    }
    Ok(())
}

pub(super) fn relative_parts(path: &str) -> Result<Vec<&str>> {
    if path.len() > 512 || path.contains(['\\', ':']) || path.chars().any(char::is_control) {
        return Err(WorkspaceError::invalid_argument(
            "Artifact paths must be relative UTF-8 file paths of at most 512 bytes",
        ));
    }
    let parts: Vec<_> = path.split('/').collect();
    if parts.iter().any(|p| p.is_empty() || p.starts_with('.')) {
        return Err(WorkspaceError::invalid_argument(
            "Absolute, hidden and parent-relative artifact paths are not accepted",
        ));
    }
    Ok(parts)
}

fn source_file(root: &Path, path: &str) -> Result<PathBuf> {
    let mut selected = root.to_path_buf();
    let parts = relative_parts(path)?;
    for (index, part) in parts.iter().enumerate() {
        selected.push(part);
        let meta = fs::symlink_metadata(&selected).map_err(storage)?;
        if meta.file_type().is_symlink()
            || (index + 1 < parts.len() && !meta.is_dir())
            || (index + 1 == parts.len() && !meta.is_file())
        {
            return Err(error("ARTIFACT_PATH_INVALID", "Only ordinary files inside the task directory can be saved; links and directories are refused", "security"));
        }
    }
    if !selected.canonicalize().map_err(storage)?.starts_with(root) {
        return Err(error(
            "ARTIFACT_PATH_INVALID",
            "Artifact source escaped the task directory",
            "security",
        ));
    }
    Ok(selected)
}

fn open_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        let file = options.open(path).map_err(storage)?;
        let meta = file.metadata().map_err(storage)?;
        if !meta.is_file() || meta.nlink() != 1 {
            return Err(error(
                "ARTIFACT_PATH_INVALID",
                "Artifact must be an ordinary, non-hardlinked file",
                "security",
            ));
        }
        Ok(file)
    }
    #[cfg(not(unix))]
    {
        regular_or_absent(path)?;
        let file = options.open(path).map_err(storage)?;
        if !file.metadata().map_err(storage)?.is_file() {
            return Err(error(
                "ARTIFACT_PATH_INVALID",
                "Artifact must be a regular file",
                "security",
            ));
        }
        Ok(file)
    }
}

fn digest(path: &Path) -> Result<(u64, String)> {
    let file = open_file(path)?;
    if file.metadata().map_err(storage)?.len() > MAX_ARTIFACT_BYTES {
        return Err(error(
            "ARTIFACT_TOO_LARGE",
            "An artifact exceeds 256 MiB; source was kept",
            "resource",
        ));
    }
    let mut source = file.take(MAX_ARTIFACT_BYTES + 1);
    let mut hash = Sha256::new();
    let mut bytes = 0;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = source.read(&mut buffer).map_err(storage)?;
        if count == 0 {
            break;
        }
        bytes += count as u64;
        if bytes > MAX_ARTIFACT_BYTES {
            return Err(error(
                "ARTIFACT_TOO_LARGE",
                "An artifact exceeds 256 MiB; source was kept",
                "resource",
            ));
        }
        hash.update(&buffer[..count]);
    }
    Ok((bytes, format!("{:x}", hash.finalize())))
}

fn artifact_id(workspace: &str, source: &str, sha: &str) -> String {
    let fingerprint = format!("{source}\0{sha}");
    format!(
        "art-{}-{:x}",
        &workspace[5..],
        Sha256::digest(fingerprint.as_bytes())
    )
}

pub(crate) fn artifact_workspace(id: &str) -> Result<String> {
    if !id.is_ascii()
        || id.len() != 101
        || !id.starts_with("art-")
        || id.as_bytes()[36] != b'-'
        || !id[4..36]
            .bytes()
            .chain(id[37..].bytes())
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(WorkspaceError::invalid_argument(
            "artifact_id must be an ID returned by retain or finish",
        ));
    }
    Ok(format!("work-{}", &id[4..36]))
}

impl Operation {
    fn artifact_root(&self, create: bool) -> Result<PathBuf> {
        let base = self.home.join("artifacts");
        if !normal_dir(&base, create)? {
            return Err(error(
                "ARTIFACT_NOT_FOUND",
                "Artifact storage is missing",
                "not_found",
            ));
        }
        let root = base.join(&self.record.id);
        if !normal_dir(&root, create)? || root.canonicalize().map_err(storage)? != root {
            return Err(error(
                "ARTIFACT_PATH_INVALID",
                "Artifact directory is missing or redirected",
                "security",
            ));
        }
        Ok(root)
    }

    pub fn preserve(&mut self, paths: &[String]) -> Result<Vec<Artifact>> {
        self.require_active()?;
        if paths.len() > MAX_ARTIFACTS {
            return Err(WorkspaceError::invalid_argument(
                "Select at most 32 artifact files",
            ));
        }
        // 先检查整组路径：不要保存到一半才发现最后一个是越界路径。
        let sources = paths
            .iter()
            .map(|p| source_file(&self.record.root, p))
            .collect::<Result<Vec<_>>>()?;
        let root = self.artifact_root(true)?;
        let mut kept = Vec::new();
        for (relative, source) in paths.iter().zip(sources) {
            let (bytes, sha256) = digest(&source)?;
            let id = artifact_id(&self.record.id, relative, &sha256);
            let exists = self.record.artifacts.iter().any(|a| a.id == id);
            if !exists && self.record.artifacts.len() >= MAX_ARTIFACTS {
                return Err(error(
                    "ARTIFACT_LIMIT",
                    "This task already has 32 retained file versions; nothing was deleted",
                    "resource",
                ));
            }
            let target = root.join(&id);
            regular_or_absent(&target)?;
            if target.exists() {
                if digest(&target)? != (bytes, sha256.clone()) {
                    return Err(error(
                        "ARTIFACT_INTEGRITY",
                        "Existing artifact failed verification; it was not overwritten",
                        "storage",
                    ));
                }
            } else {
                let temp = root.join(format!(".copy-{}", Uuid::new_v4().simple()));
                let copied = (|| {
                    let mut input = open_file(&source)?.take(MAX_ARTIFACT_BYTES + 1);
                    let mut options = OpenOptions::new();
                    options.write(true).create_new(true);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::OpenOptionsExt;
                        options.mode(0o600);
                    }
                    let mut output = options.open(&temp).map_err(storage)?;
                    let written = std::io::copy(&mut input, &mut output).map_err(storage)?;
                    output.flush().map_err(storage)?;
                    output.sync_all().map_err(storage)?;
                    drop(output);
                    if written != bytes
                        || digest(&temp)? != (bytes, sha256.clone())
                        || digest(&source)? != (bytes, sha256.clone())
                    {
                        return Err(error(
                            "ARTIFACT_CHANGED",
                            "Source changed while saving; original files were kept",
                            "conflict",
                        ));
                    }
                    toexec_fs::replace(&temp, &target).map_err(storage)
                })();
                if copied.is_err() {
                    let _ = fs::remove_file(&temp);
                }
                copied?;
            }
            let item = Artifact {
                id,
                source: relative.clone(),
                bytes,
                sha256,
                mime_type: mime(relative).into(),
            };
            if !exists {
                self.record.artifacts.push(item.clone());
                self.persist()?;
            }
            kept.push(item);
        }
        self.verify_artifacts()?;
        Ok(kept)
    }

    pub fn artifact(&self, id: &str, include_content: bool) -> Result<Value> {
        let item = self
            .record
            .artifacts
            .iter()
            .find(|a| a.id == id)
            .ok_or_else(|| {
                error(
                    "ARTIFACT_NOT_FOUND",
                    "No such artifact belongs to this caller",
                    "not_found",
                )
            })?;
        let path = self.artifact_root(false)?.join(id);
        regular_or_absent(&path)?;
        if digest(&path)? != (item.bytes, item.sha256.clone()) {
            return Err(error(
                "ARTIFACT_INTEGRITY",
                "Artifact does not match its saved checksum",
                "storage",
            ));
        }
        let mut out = json!({"artifact": item, "workspace": self.record.id, "saved_path": path, "verified": true, "retention": "explicit; not subject to workspace expiry"});
        if include_content {
            if item.bytes > MAX_TEXT_BYTES {
                return Err(error(
                    "ARTIFACT_CONTENT_TOO_LARGE",
                    "Inline text is limited to 64 KiB; query metadata or export the saved file",
                    "resource",
                ));
            }
            let mut content = String::new();
            open_file(&path)?.take(MAX_TEXT_BYTES + 1).read_to_string(&mut content).map_err(|_|
                WorkspaceError::invalid_argument("This artifact is not UTF-8 text; use metadata to retrieve its saved location"))?;
            if content.len() as u64 != item.bytes
                || format!("{:x}", Sha256::digest(content.as_bytes())) != item.sha256
            {
                return Err(error(
                    "ARTIFACT_INTEGRITY",
                    "Artifact changed during reading",
                    "storage",
                ));
            }
            out["content"] = json!(content);
        }
        Ok(out)
    }

    pub(super) fn verify_artifacts(&self) -> Result<()> {
        for item in &self.record.artifacts {
            self.artifact(&item.id, false)?;
        }
        Ok(())
    }
}

fn mime(path: &str) -> &'static str {
    match path
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "txt" | "md" | "rs" | "py" => "text/plain",
        "csv" => "text/csv",
        "json" => "application/json",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests;

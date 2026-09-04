use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::App;
use crate::error::{AppError, AppResult};
use crate::logs::log_dir_for_profile;
use crate::runtime::ServiceKind;
use crate::workspace::WorkspaceProfile;

/// 单个日志文件的尾部内容。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogChunk {
    pub name: String,
    pub path: PathBuf,
    pub content: String,
    /// 文件比返回的内容更长（前面被截掉了）。
    pub truncated: bool,
}

impl App {
    /// 读取某个服务相关日志文件的尾部，每个文件最多 `max_bytes` 字节。
    pub fn workspace_logs(
        &self,
        id: &str,
        kind: ServiceKind,
        max_bytes: usize,
    ) -> AppResult<Vec<LogChunk>> {
        let profile = self.profile_by_id(id)?;
        let log_dir = log_dir_for_profile(&profile.id);
        let mut chunks = Vec::new();
        for name in log_file_names(&profile, kind) {
            let path = log_dir.join(name);
            if !path.exists() {
                continue;
            }
            let (content, truncated) = read_log_tail(&path, max_bytes)?;
            chunks.push(LogChunk {
                name: name.to_string(),
                path,
                content,
                truncated,
            });
        }
        Ok(chunks)
    }

    /// 工作区日志目录（给 `-f` 跟随模式直接 tail 文件用）。
    pub fn workspace_log_dir(&self, id: &str) -> AppResult<PathBuf> {
        self.ensure_workspace_exists(id)?;
        Ok(log_dir_for_profile(id))
    }
}

fn log_file_names(profile: &WorkspaceProfile, kind: ServiceKind) -> Vec<&'static str> {
    match kind {
        ServiceKind::Mcp => {
            let mut names = vec!["mcp-requests.log", "stderr.log", "stdout.log"];
            if profile.tunnel.tunnel_type == "cloudflare" {
                names.insert(0, "cloudflared.log");
            }
            if profile.tunnel.tunnel_type == "frp" {
                names.insert(0, "frpc-mcp.log");
            }
            names
        }
        ServiceKind::Actions => {
            let mut names = vec!["actions-stderr.log", "actions-stdout.log"];
            if profile.actions.tunnel_type == "cloudflare" {
                names.insert(0, "actions-cloudflared.log");
            }
            if profile.actions.tunnel_type == "frp" {
                names.insert(0, "frpc-actions.log");
            }
            names
        }
    }
}

/// 从文件末尾读取最多 `max_bytes`，并对齐到第一个完整行，避免半个 UTF-8 字符。
pub fn read_log_tail(path: &Path, max_bytes: usize) -> AppResult<(String, bool)> {
    let mut file = File::open(path)
        .map_err(|error| AppError::Message(format!("无法打开日志 {}：{error}", path.display())))?;
    let size = file.seek(SeekFrom::End(0))?;
    let start = size.saturating_sub(max_bytes as u64);
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    let truncated = start > 0;
    let mut text = String::from_utf8_lossy(&buf).into_owned();
    if truncated {
        if let Some(newline) = text.find('\n') {
            text.drain(..=newline);
        }
    }
    Ok((text, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_drops_the_partial_first_line_when_truncated() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("a.log");
        std::fs::write(&path, "line one\nline two\nline three\n").unwrap();
        let (text, truncated) = read_log_tail(&path, 12).unwrap();
        assert!(truncated);
        assert_eq!(text, "line three\n");
        let (full, truncated) = read_log_tail(&path, 1024).unwrap();
        assert!(!truncated);
        assert!(full.starts_with("line one"));
    }
}

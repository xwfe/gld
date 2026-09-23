//! 工作区扫描：每个文件的 SHA-256、整棵树的指纹、以及哪些文件没读到。
//!
//! 任务基线、写前检查、验收证据都拿这里的结果比。

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use super::model::{BaselineEntry, ProjectBaseline, ProjectFileState};

/// 一次扫描的结果。
pub struct WorktreeScan {
    pub entries: Vec<BaselineEntry>,
    /// 遍历出错或打不开、读到一半失败的路径（相对工作区根）。以前这些一律当作
    /// "不存在"悄悄跳过，于是一个没权限读的源文件被改了，指纹照样对得上，也没有
    /// 任何地方说"这次比对不完整"（审查 D02）。
    pub unreadable: Vec<String>,
}

impl WorktreeScan {
    pub fn fingerprint(&self) -> String {
        fingerprint_of(&self.entries)
    }

    pub fn is_complete(&self) -> bool {
        self.unreadable.is_empty()
    }
}

pub fn capture_baseline(root: &Path) -> ProjectBaseline {
    let scan = scan_worktree(root);
    let (branch, head) = git_position(root);
    ProjectBaseline {
        branch,
        head,
        worktree_fingerprint: scan.fingerprint(),
        entries: scan.entries,
        unreadable: scan.unreadable,
        captured_at: timestamp(),
    }
}

/// 只要指纹时用。不要拿它跟 `ProjectBaseline::worktree_fingerprint` 以外的东西比。
pub(super) fn fingerprint_of(entries: &[BaselineEntry]) -> String {
    let mut fingerprint = Sha256::new();
    for entry in entries {
        fingerprint.update(entry.path.as_bytes());
        fingerprint.update(entry.sha256.as_bytes());
        fingerprint.update(entry.bytes.to_le_bytes());
    }
    format!("{:x}", fingerprint.finalize())
}

pub(super) fn git_position(root: &Path) -> (Option<String>, Option<String>) {
    (
        git_value(root, &["rev-parse", "--abbrev-ref", "HEAD"]),
        git_value(root, &["rev-parse", "HEAD"]),
    )
}

/// 工作区里每个文件的路径、大小和 SHA-256，按路径排好序；读不到的单列。
///
/// 跳过的目录在遍历时就不进去。以前是走到每个文件再判断要不要跳，`node_modules`、
/// `target` 里几十万个文件照样要逐个 stat 一遍。
/// 文件内容按固定缓冲流式算哈希：以前整个读进内存，工作区里一个 511 MB 的文件就让
/// 峰值内存涨到 517 MB。
pub(super) fn scan_worktree(root: &Path) -> WorktreeScan {
    let mut entries = Vec::new();
    let mut unreadable = Vec::new();
    let walker = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|item| {
            item.depth() == 0 || !(item.file_type().is_dir() && is_skipped_dir(item.file_name()))
        });
    for item in walker {
        let item = match item {
            Ok(item) => item,
            Err(error) => {
                // 没权限进的目录、遍历中途消失的条目：内容不知道，得说出来。
                if let Some(path) = error.path() {
                    unreadable.push(relative(root, path));
                }
                continue;
            }
        };
        if !item.file_type().is_file() {
            continue;
        }
        let path = item.path();
        let rel = relative(root, path);
        match hash_file(path) {
            Some((sha256, is_binary, bytes)) => entries.push(BaselineEntry {
                path: rel,
                exists: true,
                is_binary,
                sha256,
                bytes,
            }),
            None => unreadable.push(rel),
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    unreadable.sort();
    unreadable.dedup();
    WorktreeScan {
        entries,
        unreadable,
    }
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// 流式算 (sha256, 是否含 0 字节, 字节数)。打不开或读到一半失败是 `None`。
fn hash_file(path: &Path) -> Option<(String, bool, u64)> {
    use std::io::Read;
    let mut file = fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut is_binary = false;
    let mut total = 0u64;
    loop {
        let read = match file.read(&mut buf) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        };
        let chunk = &buf[..read];
        is_binary = is_binary || chunk.contains(&0);
        hasher.update(chunk);
        total += read as u64;
    }
    Some((format!("{:x}", hasher.finalize()), is_binary, total))
}

/// 指纹要盯的是"用户的工作区内容"，不包括 gld 自己在项目里的状态目录。
///
/// `.gld/`（老项目里是 `.coding-tools/`）存的是 Planning 状态，而它由 dispatch
/// 的公共路径 load-or-create——也就是**每一次工具调用**都可能写它。算进指纹的话
/// 就是自己把自己锁死：
///
///   task_manage start   → 记下 baseline，同时创建 .gld/planning/state.json
///   exec_command        → 指纹对不上 → FILE_CHANGED_EXTERNALLY，写操作被拒
///
/// 开了任务反而什么都干不了，而报错说的是"外部文件变化"——去查外部改了什么，
/// 永远查不到。
///
/// 其余是构建产物、依赖缓存，以及工作区指到用户目录或磁盘根时才会碰到的巨型系统目录
/// （macOS 的 `Library` 下挂着 iCloud / OneDrive，Windows 的 `AppData`）。它们不计入指纹
/// 的代价是：有人在这些目录里改东西，任务不会报 FILE_CHANGED_EXTERNALLY。
///
/// **只对目录生效**，路径里任意一级都算。以前文件也按名字跳：一个叫 `build` 的脚本、
/// 一个叫 `dist` 的配置被改了，任务照样认为没变（审查 D02）。
fn is_skipped_dir(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(
            ".git"
                | ".gld"
                | ".coding-tools"
                | ".mcp-probe-kit"
                | "node_modules"
                | "target"
                | "dist"
                | "build"
                | ".svelte-kit"
                | ".next"
                | ".turbo"
                | ".cache"
                | "__pycache__"
                | ".venv"
                | "venv"
                | "coverage"
                | "Library"
                | "AppData"
                | "$Recycle.Bin"
                | "System Volume Information"
        )
    )
}

/// 每个文件相对某份清单的状态，两边的路径取并集、按路径排序。
/// 没有参照（没有任务）时一律是 added。
pub(super) fn file_states(
    baseline: Option<&[BaselineEntry]>,
    current: &[BaselineEntry],
) -> Vec<ProjectFileState> {
    let baseline_map: HashMap<_, _> = baseline
        .map(|entries| entries.iter().map(|e| (e.path.as_str(), e)).collect())
        .unwrap_or_default();
    let current_map: HashMap<_, _> = current.iter().map(|e| (e.path.as_str(), e)).collect();
    let mut paths: Vec<&str> = baseline_map
        .keys()
        .chain(current_map.keys())
        .copied()
        .collect();
    paths.sort_unstable();
    paths.dedup();
    paths
        .into_iter()
        .map(|path| {
            let before = baseline_map.get(path);
            let entry = current_map.get(path);
            let status = match (before, entry) {
                (Some(before), Some(entry)) if before.sha256 == entry.sha256 => "unchanged",
                (Some(_), Some(_)) => "modified",
                (Some(_), None) => "deleted",
                (None, Some(_)) => "added",
                (None, None) => "unknown",
            };
            ProjectFileState {
                path: path.to_string(),
                status: status.to_string(),
                sha256: entry.map(|e| e.sha256.clone()).unwrap_or_default(),
                bytes: entry.map(|e| e.bytes).unwrap_or(0),
            }
        })
        .collect()
}

/// 只留有变化的那些。
pub(super) fn changed_files(
    baseline: &[BaselineEntry],
    current: &[BaselineEntry],
) -> Vec<ProjectFileState> {
    file_states(Some(baseline), current)
        .into_iter()
        .filter(|file| file.status != "unchanged")
        .collect()
}

fn git_value(root: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(root).args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!value.is_empty()).then_some(value)
}

pub(super) fn timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().to_string())
        .unwrap_or_else(|_| "0".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// 流式哈希要跟整份读进来算的一样，否则升级后老任务的指纹全部对不上，
    /// 一写就报 FILE_CHANGED_EXTERNALLY。0 字节故意放在第二块里。
    #[test]
    fn scan_skips_generated_dirs_and_hashes_like_a_whole_read() {
        let workspace = tempdir().expect("workspace");
        let root = workspace.path();
        let mut big = vec![b'a'; 200 * 1024];
        big[100_000] = 0;
        for (path, content) in [
            ("src/main.rs", b"fn main() {}\n".to_vec()),
            ("big.bin", big.clone()),
            ("node_modules/pkg/index.js", b"x".to_vec()),
            (".venv/lib/site.py", b"x".to_vec()),
            ("docs/Library/cache.db", b"x".to_vec()),
        ] {
            let full = root.join(path);
            fs::create_dir_all(full.parent().unwrap()).expect("dir");
            fs::write(full, content).expect("file");
        }

        let scan = scan_worktree(root);
        let paths: Vec<&str> = scan.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["big.bin", "src/main.rs"]);
        assert!(scan.is_complete());

        let big_entry = &scan.entries[0];
        assert_eq!(big_entry.bytes, big.len() as u64);
        assert!(big_entry.is_binary, "0 字节在第二块也得认出是二进制");
        assert_eq!(big_entry.sha256, format!("{:x}", Sha256::digest(&big)));
        assert_eq!(
            scan.fingerprint(),
            capture_baseline(root).worktree_fingerprint
        );
    }

    /// 名单是给目录的。叫 `build` 的脚本、叫 `dist` 的文件是源码，改了要看得见。
    #[test]
    fn files_that_share_a_skipped_directory_name_are_still_tracked() {
        let workspace = tempdir().expect("workspace");
        let root = workspace.path();
        for path in ["scripts/build", "dist", "build/out.js"] {
            let full = root.join(path);
            fs::create_dir_all(full.parent().unwrap()).expect("dir");
            fs::write(full, "x").expect("file");
        }
        let paths: Vec<String> = scan_worktree(root)
            .entries
            .into_iter()
            .map(|e| e.path)
            .collect();
        assert_eq!(paths, ["dist", "scripts/build"], "build/ 目录照旧跳过");
    }

    /// 读不到的文件不能当它不存在：那样它被改了指纹也对得上，还没有任何提示。
    #[cfg(unix)]
    #[test]
    fn unreadable_files_are_reported_instead_of_silently_skipped() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempdir().expect("workspace");
        let root = workspace.path();
        fs::write(root.join("ok.txt"), "x").expect("file");
        let secret = root.join("secret.txt");
        fs::write(&secret, "x").expect("file");
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o000)).expect("chmod");
        // root 跑测试时权限挡不住，这条用例没法说明问题。
        if fs::File::open(&secret).is_ok() {
            return;
        }

        let scan = scan_worktree(root);
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o644)).expect("chmod back");
        assert_eq!(scan.unreadable, ["secret.txt"]);
        assert!(!scan.is_complete());
        let paths: Vec<&str> = scan.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["ok.txt"]);
    }
}

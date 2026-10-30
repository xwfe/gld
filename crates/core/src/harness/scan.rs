//! 工作区扫描：每个文件的 SHA-256、整棵树的指纹、以及哪些文件没读到。
//!
//! 任务基线、写前检查、验收证据都拿这里的结果比。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
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
///
/// 项目是 Git 仓库时，被 `.gitignore` 忽略、又没被跟踪的文件和目录也不进，见 [`git_ignored`]。
pub(super) fn scan_worktree(root: &Path) -> WorktreeScan {
    let mut entries = Vec::new();
    let mut unreadable = Vec::new();
    let ignored = git_ignored(root);
    let walker = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|item| {
            if item.depth() == 0 {
                return true;
            }
            if item.file_type().is_dir() {
                !is_skipped_dir(item.file_name())
                    && !ignored.dirs.contains(&relative(root, item.path()))
            } else {
                !ignored.files.contains(&relative(root, item.path()))
            }
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
                // Playwright 的默认输出目录。不排除的话，后台跑一次 E2E 写出的截图和报告会让下一次
                // apply_patch 报 FILE_CHANGED_EXTERNALLY（D10 Web 场景实测，那次是自定义的目录名，
                // 照样要 refresh_baseline）。
                | "test-results"
                | "playwright-report"
                | "blob-report"
                | "Library"
                | "AppData"
                | "$Recycle.Bin"
                | "System Volume Information"
        )
    )
}

/// 被 Git 忽略、又没被跟踪的路径，相对工作区根、用 `/` 分隔。
#[derive(Default)]
struct IgnoredPaths {
    files: HashSet<String>,
    /// 整个目录都被忽略的（git 只报目录本身，不展开）。
    dirs: HashSet<String>,
}

/// 问 git 哪些文件和目录被忽略了（`.gitignore`、`.git/info/exclude`、全局 excludes 都算）。
///
/// 为什么不算进指纹：测试转到后台继续跑时，回包之后才写出、删掉的文件算不到那条命令头上，
/// 下一次写就报 `FILE_CHANGED_EXTERNALLY`。2026-10-18 到 10-24 的 13 次 `refresh_baseline` 里
/// 11 次改动全是 `.data/tests/…`、`.pytest_cache/…` 这类被忽略的测试产物（审查 §72）。项目自己
/// 声明了不要的东西，和上面那张目录名单一个道理。代价也一样：任务期间有人改了被忽略的文件
/// （比如本地 `.env`），gld 不再报外部改动，任务的改动清单里也没有它们。
///
/// 已经被跟踪的文件照样算，哪怕它也匹配忽略规则（`git add -f` 进去的）。项目在别的仓库里面时
/// 按那个仓库的规则算。不是 Git 仓库、没装 git、git 报错都是空的：照旧全扫，不会因此漏判。
fn git_ignored(root: &Path) -> IgnoredPaths {
    let Some(output) = git_output(
        root,
        &[
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
        ],
    ) else {
        return IgnoredPaths::default();
    };
    if !output.status.success() {
        return IgnoredPaths::default();
    }
    let mut ignored = IgnoredPaths::default();
    for raw in output.stdout.split(|byte| *byte == 0) {
        let path = String::from_utf8_lossy(raw).replace('\\', "/");
        match path.strip_suffix('/') {
            Some(dir) if !dir.is_empty() => {
                ignored.dirs.insert(dir.to_string());
            }
            Some(_) => {}
            None if !path.is_empty() => {
                ignored.files.insert(path);
            }
            None => {}
        }
    }
    ignored
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

/// `descendant` 是不是从 `ancestor` 往前走出来的（同一个提交也算）。git 报错算不是。
pub(super) fn git_is_ancestor(root: &Path, ancestor: &str, descendant: &str) -> bool {
    git_output(root, &["merge-base", "--is-ancestor", ancestor, descendant])
        .is_some_and(|output| output.status.success())
}

fn git_value(root: &Path, args: &[&str]) -> Option<String> {
    let output = git_output(root, args)?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn git_output(root: &Path, args: &[&str]) -> Option<std::process::Output> {
    let mut cmd = crate::tools::git::git_command(root).ok()?;
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    cmd.output().ok()
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
            ("test-results/list-adds/test-finished-1.png", b"x".to_vec()),
            ("playwright-report/index.html", b"x".to_vec()),
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

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.email=t@example.com", "-c", "user.name=t"])
            .args(args)
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    }

    fn write(root: &Path, path: &str, content: &str) {
        let full = root.join(path);
        fs::create_dir_all(full.parent().unwrap()).expect("dir");
        fs::write(full, content).expect("file");
    }

    fn scanned(root: &Path) -> Vec<String> {
        scan_worktree(root)
            .entries
            .into_iter()
            .map(|e| e.path)
            .collect()
    }

    /// 被忽略、没被跟踪的不算：测试在后台写出的 `.data/`、`.pytest_cache/` 不该让下一次写报外部改动
    /// （审查 §72）。被跟踪的哪怕匹配忽略规则也照算；没被忽略的新文件照样看得见。
    #[test]
    fn a_git_project_does_not_count_what_it_ignores() {
        let workspace = tempdir().expect("workspace");
        let root = workspace.path();
        git(root, &["init", "-q"]);
        write(root, ".gitignore", ".data/\n*.log\n.pytest_cache/\n");
        write(root, "src/app.py", "print(1)\n");
        write(root, "pinned.log", "tracked on purpose\n");
        git(root, &["add", ".gitignore", "src/app.py"]);
        git(root, &["add", "-f", "pinned.log"]);
        git(root, &["commit", "-q", "-m", "init"]);

        let before = scan_worktree(root).fingerprint();
        // 测试跑出来的东西：整目录忽略的、按后缀忽略的、目录里一层层往下的。
        write(root, ".data/tests/case-1/rag.sqlite3", "db");
        write(root, ".pytest_cache/v/cache/lastfailed", "{}");
        write(root, "src/debug.log", "noise");
        assert_eq!(scan_worktree(root).fingerprint(), before);
        assert_eq!(
            scanned(root),
            [".gitignore", "pinned.log", "src/app.py"],
            "被跟踪的 pinned.log 照算"
        );

        // 没被忽略的新文件、被跟踪文件的改动，照样让指纹变。
        write(root, "notes.md", "x");
        let with_new_file = scan_worktree(root).fingerprint();
        assert_ne!(with_new_file, before);
        write(root, "pinned.log", "changed\n");
        assert_ne!(scan_worktree(root).fingerprint(), with_new_file);
    }

    /// 项目是大仓库里的一个子目录：按那个仓库的规则算，路径照旧相对项目根。
    #[test]
    fn a_project_inside_a_repository_uses_that_repositorys_rules() {
        let repo = tempdir().expect("repo");
        git(repo.path(), &["init", "-q"]);
        write(repo.path(), ".gitignore", "out/\n");
        let root = repo.path().join("packages/web");
        write(&root, "index.js", "x");
        write(&root, "out/bundle.js", "x");
        assert_eq!(scanned(&root), ["index.js"]);
    }

    /// 不是 Git 仓库：`.gitignore` 只是个普通文件，里面写的规则不生效，照旧全扫。
    #[test]
    fn without_git_every_file_still_counts() {
        let workspace = tempdir().expect("workspace");
        let root = workspace.path();
        write(root, ".gitignore", ".data/\n");
        write(root, ".data/x.db", "x");
        assert_eq!(scanned(root), [".data/x.db", ".gitignore"]);
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

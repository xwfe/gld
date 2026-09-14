use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use walkdir::WalkDir;

use super::model::{
    BaselineEntry, CapabilityStatus, FileChangeRecord, HarnessEvent, HarnessStatus,
    OperationRecord, ProjectBaseline, ProjectFileState, ProjectState, TaskSession, TaskStatus,
    WorkspaceHarnessState, SCHEMA_VERSION,
};
use super::store::{HarnessError, HarnessResult, HarnessStore};

#[derive(Debug, Clone)]
pub struct Harness {
    workspace_root: PathBuf,
    workspace_id: String,
    store: HarnessStore,
}

impl Harness {
    pub fn new(workspace_root: PathBuf, harness_root: PathBuf) -> HarnessResult<Self> {
        let workspace_root = workspace_root
            .canonicalize()
            .map_err(|e| HarnessError::new("WORKSPACE_UNAVAILABLE", e.to_string()))?;
        let workspace_id = workspace_id(&workspace_root);
        Ok(Self {
            workspace_root,
            workspace_id,
            store: HarnessStore::new(harness_root)?,
        })
    }

    pub fn default_root() -> HarnessResult<PathBuf> {
        crate::home::harness_root()
            .map_err(|error| HarnessError::new("STORE_UNAVAILABLE", error.to_string()))
    }

    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    pub fn store_root(&self) -> &Path {
        self.store.root()
    }

    pub fn start_task(&self, objective: &str) -> HarnessResult<TaskSession> {
        if objective.trim().is_empty() {
            return Err(HarnessError::new("INVALID_ARGUMENT", "任务目标不能为空"));
        }
        if let Some(task) = self.current_task()? {
            return Err(HarnessError::new(
                "TASK_ALREADY_ACTIVE",
                format!("工作区已有活动任务 {}", task.id),
            ));
        }
        let baseline = capture_baseline(&self.workspace_root);
        let now = timestamp();
        let task = TaskSession {
            id: Uuid::new_v4().simple().to_string(),
            workspace_id: self.workspace_id.clone(),
            objective: objective.trim().to_string(),
            status: TaskStatus::Active,
            expected_fingerprint: baseline.worktree_fingerprint.clone(),
            baseline,
            completed_steps: Vec::new(),
            pending_steps: Vec::new(),
            latest_change_id: None,
            latest_verification_id: None,
            created_at: now.clone(),
            updated_at: now,
        };
        self.store.save_task(&task)?;
        self.save_workspace_state(Some(&task.id), &task.updated_at)?;
        self.record_event(
            &task.id,
            "task_started",
            None,
            json!({}),
            json!({"ok": true}),
        )?;
        Ok(task)
    }

    pub fn current_task(&self) -> HarnessResult<Option<TaskSession>> {
        Ok(self
            .store
            .list_tasks(&self.workspace_id)?
            .into_iter()
            .find(|task| task.status.is_writable()))
    }

    pub fn task(&self, task_id: &str) -> HarnessResult<TaskSession> {
        self.store.load_task(&self.workspace_id, task_id)
    }

    pub fn transition(&self, task_id: &str, next: TaskStatus) -> HarnessResult<TaskSession> {
        let mut task = self.task(task_id)?;
        if !task.status.can_transition_to(next) {
            return Err(HarnessError::new(
                "INVALID_TASK_TRANSITION",
                format!("不允许从 {:?} 转换到 {:?}", task.status, next),
            ));
        }
        task.status = next;
        task.updated_at = timestamp();
        self.store.save_task(&task)?;
        if !task.status.is_writable() {
            self.save_workspace_state(None, &task.updated_at)?;
        }
        self.record_event(
            task_id,
            "task_status_changed",
            None,
            json!({"status": next}),
            json!({"ok": true}),
        )?;
        Ok(task)
    }

    pub fn update_steps(
        &self,
        task_id: &str,
        completed_steps: Option<Vec<String>>,
        pending_steps: Option<Vec<String>>,
    ) -> HarnessResult<TaskSession> {
        let mut task = self.task(task_id)?;
        if let Some(steps) = completed_steps {
            task.completed_steps = steps;
        }
        if let Some(steps) = pending_steps {
            task.pending_steps = steps;
        }
        task.updated_at = timestamp();
        self.store.save_task(&task)?;
        self.record_event(
            task_id,
            "task_updated",
            None,
            json!({
                "completed_steps": task.completed_steps,
                "pending_steps": task.pending_steps
            }),
            json!({"ok": true}),
        )?;
        Ok(task)
    }

    pub fn check_baseline(&self, task_id: &str) -> HarnessResult<()> {
        let task = self.task(task_id)?;
        let (branch, head) = git_position(&self.workspace_root);
        if branch != task.baseline.branch || head != task.baseline.head {
            return Err(HarnessError::new(
                "BASELINE_STALE",
                "Git 分支或 HEAD 已发生变化",
            ));
        }
        if worktree_fingerprint(&self.workspace_root) != task.expected_fingerprint {
            return Err(HarnessError::new(
                "FILE_CHANGED_EXTERNALLY",
                "工作区存在 Harness 未记录的外部文件变化",
            ));
        }
        Ok(())
    }

    pub fn refresh_expected_state(&self, task_id: &str) -> HarnessResult<TaskSession> {
        let mut task = self.task(task_id)?;
        task.expected_fingerprint = worktree_fingerprint(&self.workspace_root);
        task.updated_at = timestamp();
        self.store.save_task(&task)?;
        Ok(task)
    }

    pub fn record_event(
        &self,
        task_id: &str,
        kind: &str,
        tool_name: Option<&str>,
        input_summary: serde_json::Value,
        result_summary: serde_json::Value,
    ) -> HarnessResult<HarnessEvent> {
        let event = HarnessEvent {
            id: Uuid::new_v4().simple().to_string(),
            task_id: task_id.to_string(),
            operation_id: Uuid::new_v4().simple().to_string(),
            kind: kind.to_string(),
            tool_name: tool_name.map(str::to_string),
            input_summary: json!({"workspace_id": self.workspace_id, "payload": input_summary}),
            result_summary,
            reason: None,
            affected_files: Vec::<FileChangeRecord>::new(),
            created_at: timestamp(),
        };
        self.store
            .append_event_for_workspace(&self.workspace_id, &event)?;
        Ok(event)
    }

    pub fn list_events(
        &self,
        task_id: &str,
        offset: usize,
        limit: usize,
    ) -> HarnessResult<Vec<HarnessEvent>> {
        self.store
            .list_events(&self.workspace_id, task_id, offset, limit)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_operation(
        &self,
        operation_id: Option<&str>,
        task_id: Option<&str>,
        tool: &str,
        kind: &str,
        input_summary: serde_json::Value,
        result_summary: serde_json::Value,
    ) -> HarnessResult<OperationRecord> {
        let reason = input_summary
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let operation = OperationRecord {
            id: operation_id
                .map(str::to_string)
                .unwrap_or_else(|| Uuid::new_v4().simple().to_string()),
            workspace_id: self.workspace_id.clone(),
            task_id: task_id.map(str::to_string),
            tool: tool.to_string(),
            kind: kind.to_string(),
            input_summary,
            result_summary,
            reason,
            affected_files: Vec::new(),
            created_at: timestamp(),
        };
        self.store
            .append_operation(&self.workspace_id, &operation)?;
        Ok(operation)
    }

    pub fn list_operations(
        &self,
        offset: usize,
        limit: usize,
    ) -> HarnessResult<Vec<OperationRecord>> {
        self.store
            .list_operations(&self.workspace_id, offset, limit)
    }

    pub fn project_state(&self, max_files: usize) -> HarnessResult<ProjectState> {
        let current = capture_baseline(&self.workspace_root);
        let task = self.current_task()?;
        let baseline_map = task
            .as_ref()
            .map(|t| {
                t.baseline
                    .entries
                    .iter()
                    .map(|e| (e.path.clone(), e))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        let current_map: HashMap<_, _> = current
            .entries
            .iter()
            .map(|e| (e.path.clone(), e))
            .collect();
        let mut paths: Vec<String> = baseline_map
            .keys()
            .chain(current_map.keys())
            .cloned()
            .collect();
        paths.sort();
        paths.dedup();
        let total_files = paths.len();
        let files = paths
            .into_iter()
            .map(|path| {
                let before = baseline_map.get(&path).map(|e| e.sha256.clone());
                let entry = current_map.get(&path);
                let status = match (before, entry) {
                    (Some(before), Some(entry)) if before == entry.sha256 => "unchanged",
                    (Some(_), Some(_)) => "modified",
                    (Some(_), None) => "deleted",
                    (None, Some(_)) => "added",
                    (None, None) => "unknown",
                };
                ProjectFileState {
                    path,
                    status: status.to_string(),
                    sha256: entry.map(|e| e.sha256.clone()).unwrap_or_default(),
                    bytes: entry.map(|e| e.bytes).unwrap_or(0),
                }
            })
            .collect::<Vec<_>>();
        let truncated = files.len() > max_files.max(1);
        let files = files.into_iter().take(max_files.max(1)).collect::<Vec<_>>();
        let active_task_id = task.as_ref().map(|t| t.id.clone());
        let recent_events = task
            .as_ref()
            .and_then(|t| self.list_events(&t.id, 0, 100).ok())
            .map(|events| events.len())
            .unwrap_or(0);
        Ok(ProjectState {
            schema_version: SCHEMA_VERSION,
            workspace_id: self.workspace_id.clone(),
            branch: current.branch,
            head: current.head,
            clean: files.iter().all(|f| f.status == "unchanged"),
            files,
            total_files,
            truncated,
            active_task_id,
            task,
            recent_events,
        })
    }

    pub fn status(&self) -> HarnessResult<HarnessStatus> {
        // 这里只拿分支和 HEAD。整棵树的指纹要把工作区每个文件读一遍算 SHA-256，而 status
        // 挂在每一次失败的工具调用后面——以前工作区里有个 511 MB 的文件，读一个不存在的
        // 路径就要多等 2 秒。没有活动任务时指纹根本没人用；有任务时也先比分支和 HEAD，
        // 对得上才算指纹。
        let (branch, head) = git_position(&self.workspace_root);
        let task = self.current_task()?;
        let (task_id, task_state, task_updated_at, writable, baseline_matches, reason) =
            match task.as_ref() {
                Some(task) => {
                    let matches = task.baseline.branch == branch
                        && task.baseline.head == head
                        && task.expected_fingerprint == worktree_fingerprint(&self.workspace_root);
                    let reason = if matches {
                        "任务可继续执行"
                    } else {
                        "工作区基线已变化，写入和执行已暂停"
                    };
                    (
                        Some(task.id.clone()),
                        Some(task.status),
                        Some(task.updated_at.clone()),
                        matches && task.status.is_writable(),
                        Some(matches),
                        reason.to_string(),
                    )
                }
                None => (
                    None,
                    None,
                    None,
                    true,
                    None,
                    "当前没有活动任务，工作区采用无任务模式；修改不会进入任务事件流".to_string(),
                ),
            };

        let mut capabilities = HashMap::new();
        capabilities.insert(
            "read".into(),
            CapabilityStatus {
                status: "available".into(),
                reason: "工作区读取不依赖活动任务".into(),
                recoverable: true,
            },
        );
        capabilities.insert(
            "write".into(),
            CapabilityStatus {
                status: if writable { "available" } else { "denied" }.into(),
                reason: if writable {
                    if task_id.is_some() {
                        "活动任务和工作区基线有效"
                    } else {
                        "无任务模式允许直接修改，建议需要长期追踪时调用 start_task"
                    }
                } else {
                    "需要活动任务且工作区基线必须匹配"
                }
                .into(),
                recoverable: true,
            },
        );
        capabilities.insert(
            "exec".into(),
            CapabilityStatus {
                status: if writable { "available" } else { "denied" }.into(),
                reason: if writable {
                    if task_id.is_some() {
                        "活动任务和工作区基线有效"
                    } else {
                        "无任务模式允许直接执行，建议需要长期追踪时调用 start_task"
                    }
                } else {
                    "需要活动任务且工作区基线必须匹配"
                }
                .into(),
                recoverable: true,
            },
        );
        capabilities.insert(
            "git".into(),
            CapabilityStatus {
                status: if branch.is_some() && head.is_some() {
                    "available"
                } else {
                    "degraded"
                }
                .into(),
                reason: if branch.is_some() && head.is_some() {
                    "已读取当前分支和 HEAD"
                } else {
                    "当前工作区不是可读取 Git 状态的仓库"
                }
                .into(),
                recoverable: true,
            },
        );
        capabilities.insert(
            "network".into(),
            CapabilityStatus {
                status: "managed_by_policy".into(),
                reason: "网络权限由工具策略控制，不由 Harness 任务状态决定".into(),
                recoverable: true,
            },
        );

        let mut next_actions = Vec::new();
        if task_id.is_none() {
            next_actions.push("start_task".into());
        } else if baseline_matches == Some(false) {
            next_actions.push("project_state".into());
            next_actions.push("git_diff".into());
            next_actions.push("refresh_baseline".into());
        } else if !writable {
            next_actions.push("resume_task".into());
        }
        next_actions.push("read_file".into());
        next_actions.push("git_status".into());

        Ok(HarnessStatus {
            schema_version: SCHEMA_VERSION,
            workspace_id: self.workspace_id.clone(),
            task_id,
            task_state,
            task_updated_at,
            writable,
            reason,
            recoverable: true,
            branch,
            head,
            baseline_matches,
            capabilities,
            next_actions,
        })
    }

    fn save_workspace_state(
        &self,
        active_task_id: Option<&str>,
        updated_at: &str,
    ) -> HarnessResult<()> {
        self.store.save_workspace_state(
            &self.workspace_id,
            &WorkspaceHarnessState {
                schema_version: SCHEMA_VERSION,
                active_task_id: active_task_id.map(str::to_string),
                recent_task_ids: self
                    .store
                    .list_tasks(&self.workspace_id)?
                    .into_iter()
                    .take(20)
                    .map(|t| t.id)
                    .collect(),
                updated_at: updated_at.to_string(),
            },
        )
    }
}

pub fn capture_baseline(root: &Path) -> ProjectBaseline {
    let entries = worktree_entries(root);
    let (branch, head) = git_position(root);
    ProjectBaseline {
        branch,
        head,
        worktree_fingerprint: fingerprint_of(&entries),
        entries,
        captured_at: timestamp(),
    }
}

fn worktree_fingerprint(root: &Path) -> String {
    fingerprint_of(&worktree_entries(root))
}

fn fingerprint_of(entries: &[BaselineEntry]) -> String {
    let mut fingerprint = Sha256::new();
    for entry in entries {
        fingerprint.update(entry.path.as_bytes());
        fingerprint.update(entry.sha256.as_bytes());
        fingerprint.update(entry.bytes.to_le_bytes());
    }
    format!("{:x}", fingerprint.finalize())
}

fn git_position(root: &Path) -> (Option<String>, Option<String>) {
    (
        git_value(root, &["rev-parse", "--abbrev-ref", "HEAD"]),
        git_value(root, &["rev-parse", "HEAD"]),
    )
}

/// 工作区里每个文件的路径、大小和 SHA-256，按路径排好序。
///
/// 跳过的目录在遍历时就不进去。以前是走到每个文件再判断要不要跳，`node_modules`、
/// `target` 里几十万个文件照样要逐个 stat 一遍。
/// 文件内容按固定缓冲流式算哈希：以前整个读进内存，工作区里一个 511 MB 的文件就让
/// 峰值内存涨到 517 MB。
fn worktree_entries(root: &Path) -> Vec<BaselineEntry> {
    let mut entries = Vec::new();
    let walker = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|item| item.depth() == 0 || !is_skipped_name(item.file_name()));
    for item in walker.filter_map(Result::ok) {
        if !item.file_type().is_file() {
            continue;
        }
        let path = item.path();
        let Some((sha256, is_binary, bytes)) = hash_file(path) else {
            continue;
        };
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        entries.push(BaselineEntry {
            path: rel,
            exists: true,
            is_binary,
            sha256,
            bytes,
        });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    entries
}

/// 流式算 (sha256, 是否含 0 字节, 字节数)。读失败就当这个文件不存在，跟以前一样。
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
/// 名字匹配的是路径里任意一级，文件名也算（一个叫 `build` 的脚本同样被跳过）。
fn is_skipped_name(name: &std::ffi::OsStr) -> bool {
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

fn workspace_id(root: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(root.to_string_lossy().as_bytes());
    format!("{:x}", hasher.finalize())[..32].to_string()
}

fn timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().to_string())
        .unwrap_or_else(|_| "0".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn status_keeps_read_available_without_task() {
        let workspace = tempdir().expect("workspace");
        let harness_root = tempdir().expect("harness");
        fs::write(workspace.path().join("main.rs"), "fn main() {}\n").expect("file");
        let harness = Harness::new(
            workspace.path().to_path_buf(),
            harness_root.path().to_path_buf(),
        )
        .expect("harness");

        let status = harness.status().expect("status");
        assert!(status.writable);
        assert_eq!(status.capabilities["read"].status, "available");
        assert_eq!(status.capabilities["write"].status, "available");
        assert!(status.next_actions.contains(&"start_task".to_string()));
    }

    #[test]
    fn starting_task_does_not_create_workspace_copies() {
        let workspace = tempdir().expect("workspace");
        let harness_root = tempdir().expect("harness");
        fs::write(workspace.path().join("main.rs"), "fn main() {}\n").expect("file");
        let harness = Harness::new(
            workspace.path().to_path_buf(),
            harness_root.path().to_path_buf(),
        )
        .expect("harness");

        harness.start_task("测试任务").expect("start task");
        assert!(!harness
            .store_root()
            .join("workspaces")
            .join(harness.workspace_id())
            .join("snapshots")
            .exists());
    }

    /// 流式哈希要跟整份读进来算的一样，否则升级后老任务的指纹全部对不上，
    /// 一写就报 FILE_CHANGED_EXTERNALLY。0 字节故意放在第二块里。
    #[test]
    fn worktree_entries_skip_generated_dirs_and_hash_like_a_whole_read() {
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
            ("scripts/build", b"#!/bin/sh\n".to_vec()),
        ] {
            let full = root.join(path);
            fs::create_dir_all(full.parent().unwrap()).expect("dir");
            fs::write(full, content).expect("file");
        }

        let entries = worktree_entries(root);
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["big.bin", "src/main.rs"]);

        let big_entry = &entries[0];
        assert_eq!(big_entry.bytes, big.len() as u64);
        assert!(big_entry.is_binary, "0 字节在第二块也得认出是二进制");
        assert_eq!(big_entry.sha256, format!("{:x}", Sha256::digest(&big)));
        assert_eq!(
            worktree_fingerprint(root),
            capture_baseline(root).worktree_fingerprint
        );
    }
}

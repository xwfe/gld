use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use super::model::{
    BaselineEntry, HarnessEvent, OperationRecord, TaskSession, WorkspaceHarnessState,
};

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct HarnessError {
    code: &'static str,
    message: String,
}

impl HarnessError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn code(&self) -> &'static str {
        self.code
    }
}

pub type HarnessResult<T> = Result<T, HarnessError>;

#[derive(Debug, Clone)]
pub struct HarnessStore {
    root: PathBuf,
}

impl HarnessStore {
    pub fn new(root: PathBuf) -> HarnessResult<Self> {
        fs::create_dir_all(&root)
            .map_err(|e| HarnessError::new("STORE_UNAVAILABLE", e.to_string()))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn workspace_dir(&self, workspace_id: &str) -> PathBuf {
        self.root.join("workspaces").join(workspace_id)
    }

    fn tasks_dir(&self, workspace_id: &str) -> PathBuf {
        self.workspace_dir(workspace_id).join("tasks")
    }

    fn events_dir(&self, workspace_id: &str) -> PathBuf {
        self.workspace_dir(workspace_id).join("events")
    }

    fn operations_path(&self, workspace_id: &str) -> PathBuf {
        self.workspace_dir(workspace_id).join("operations.jsonl")
    }

    /// 不放 `tasks/` 下：`list_tasks` 会把那里每个 `.json` 都当任务解析。
    fn expected_entries_path(&self, workspace_id: &str, task_id: &str) -> PathBuf {
        self.workspace_dir(workspace_id)
            .join("expected")
            .join(format!("{task_id}.json"))
    }

    /// 任务上一次记账时的逐文件清单。
    ///
    /// 任务里只存了那一刻的指纹，指纹只能说"变了"，说不出"哪几个文件变了"。
    /// 基线复核要把外部改动逐个列出来让人认领（审查 D02），验收要说出命令自己跑的
    /// 时候改了什么，都得有这份清单。
    pub fn save_expected_entries(
        &self,
        workspace_id: &str,
        task_id: &str,
        entries: &[BaselineEntry],
    ) -> HarnessResult<()> {
        let path = self.expected_entries_path(workspace_id, task_id);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(io_error)?;
        }
        atomic_write_json(&path, &entries)
    }

    /// 没存过（任务刚开始，或者是升级前开的任务）是 `None`。
    pub fn load_expected_entries(
        &self,
        workspace_id: &str,
        task_id: &str,
    ) -> HarnessResult<Option<Vec<BaselineEntry>>> {
        let path = self.expected_entries_path(workspace_id, task_id);
        if !path.exists() {
            return Ok(None);
        }
        read_json(&path).map(Some)
    }

    pub fn load_workspace_state(
        &self,
        workspace_id: &str,
    ) -> HarnessResult<Option<WorkspaceHarnessState>> {
        let path = self.workspace_dir(workspace_id).join("state.json");
        if !path.exists() {
            return Ok(None);
        }
        read_json(&path).map(Some)
    }

    pub fn save_task(&self, task: &TaskSession) -> HarnessResult<()> {
        let dir = self.tasks_dir(&task.workspace_id);
        fs::create_dir_all(&dir).map_err(io_error)?;
        atomic_write_json(&dir.join(format!("{}.json", task.id)), task)
    }

    pub fn load_task(&self, workspace_id: &str, task_id: &str) -> HarnessResult<TaskSession> {
        read_json(&self.tasks_dir(workspace_id).join(format!("{task_id}.json")))
    }

    pub fn list_tasks(&self, workspace_id: &str) -> HarnessResult<Vec<TaskSession>> {
        let dir = self.tasks_dir(workspace_id);
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut tasks: Vec<TaskSession> = Vec::new();
        for entry in fs::read_dir(dir).map_err(io_error)? {
            let path = entry.map_err(io_error)?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            if let Ok(task) = read_json(&path) {
                tasks.push(task);
            }
        }
        tasks.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(tasks)
    }

    pub fn save_workspace_state(
        &self,
        workspace_id: &str,
        state: &WorkspaceHarnessState,
    ) -> HarnessResult<()> {
        let dir = self.workspace_dir(workspace_id);
        fs::create_dir_all(&dir).map_err(io_error)?;
        atomic_write_json(&dir.join("state.json"), state)
    }

    pub fn append_event_for_workspace(
        &self,
        workspace_id: &str,
        event: &HarnessEvent,
    ) -> HarnessResult<()> {
        let dir = self.events_dir(workspace_id);
        fs::create_dir_all(&dir).map_err(io_error)?;
        let path = dir.join(format!("{}.jsonl", event.task_id));
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(io_error)?;
        let line = serde_json::to_string(event)
            .map_err(|e| HarnessError::new("STORE_SERIALIZE_FAILED", e.to_string()))?;
        writeln!(file, "{line}").map_err(io_error)
    }

    pub fn append_operation(
        &self,
        workspace_id: &str,
        operation: &OperationRecord,
    ) -> HarnessResult<()> {
        let dir = self.workspace_dir(workspace_id);
        fs::create_dir_all(&dir).map_err(io_error)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.operations_path(workspace_id))
            .map_err(io_error)?;
        let line = serde_json::to_string(operation)
            .map_err(|e| HarnessError::new("STORE_SERIALIZE_FAILED", e.to_string()))?;
        writeln!(file, "{line}").map_err(io_error)
    }

    pub fn list_operations(
        &self,
        workspace_id: &str,
        offset: usize,
        limit: usize,
    ) -> HarnessResult<Vec<OperationRecord>> {
        let path = self.operations_path(workspace_id);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let file = File::open(path).map_err(io_error)?;
        let mut operations = Vec::new();
        for line in BufReader::new(file).lines().skip(offset).take(limit.max(1)) {
            let line = line.map_err(io_error)?;
            match serde_json::from_str(&line) {
                Ok(operation) => operations.push(operation),
                Err(_) => break,
            }
        }
        Ok(operations)
    }

    pub fn list_events(
        &self,
        workspace_id: &str,
        task_id: &str,
        offset: usize,
        limit: usize,
    ) -> HarnessResult<Vec<HarnessEvent>> {
        let path = self
            .events_dir(workspace_id)
            .join(format!("{task_id}.jsonl"));
        if !path.exists() {
            return Ok(Vec::new());
        }
        let file = File::open(path).map_err(io_error)?;
        let mut events = Vec::new();
        for line in BufReader::new(file).lines().skip(offset).take(limit.max(1)) {
            let line = line.map_err(io_error)?;
            match serde_json::from_str(&line) {
                Ok(event) => events.push(event),
                Err(_) => break,
            }
        }
        Ok(events)
    }
}

fn io_error(error: std::io::Error) -> HarnessError {
    HarnessError::new("STORE_IO_FAILED", error.to_string())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> HarnessResult<T> {
    let bytes = fs::read(path).map_err(io_error)?;
    serde_json::from_slice(&bytes)
        .map_err(|e| HarnessError::new("STORE_CORRUPT", format!("{}: {e}", path.display())))
}

fn atomic_write_json<T: serde::Serialize>(path: &Path, value: &T) -> HarnessResult<()> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| HarnessError::new("STORE_SERIALIZE_FAILED", e.to_string()))?;
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, bytes).map_err(io_error)?;
    fs::rename(&temp, path).map_err(io_error)
}

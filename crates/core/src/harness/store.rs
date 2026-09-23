//! Harness 的数据目录：每个工作区一格，里面是任务文件（整份 JSON）、任务事件和操作日志
//! （一行一条的 JSONL），外加两份从任务推出来的索引（`state.json`、`expected/`）。
//!
//! 坏了怎么办（审查 D11）：
//!
//! - **任务文件、日志是记录**，坏了不跳过、不覆盖：任务文件报 `STORE_CORRUPT` 带路径，
//!   日志里的坏行跳过但逐行报出来（[`LogPage::unreadable`]），后面的行照读。
//! - **索引坏了按任务文件重算**，下次记账时重写（见 `Harness::active_task`、
//!   `Harness::expected_entries`）。
//! - 读改写任务之前拿 [`HarnessStore::lock`]，两个连接同时开任务、改任务不会互相覆盖。

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fs2::FileExt;
use serde::Serialize;

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

/// 读不出来的一条记录：一个任务文件，或者日志里的一行。
#[derive(Debug, Clone, Serialize)]
pub struct Unreadable {
    pub path: String,
    /// 日志里的第几行，从 1 数，和编辑器、`sed -n 3p` 里的行号一致。任务文件没有。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    pub error: String,
}

/// 一个工作区的全部任务文件。
pub struct TaskListing {
    /// 按最近更新排在前面。
    pub tasks: Vec<TaskSession>,
    pub unreadable: Vec<Unreadable>,
}

/// 从一份 JSONL 日志里读出来的一页。
pub struct LogPage<T> {
    /// 每条记录和它所在的行（从 0 数）。行号就是从这条开始翻页要给的 cursor。
    pub records: Vec<(usize, T)>,
    /// 下一页从哪一行开始。坏行也占一行，所以不一定是 `offset + records.len()`。
    pub next_offset: usize,
    /// 读到文件尾了（不是因为凑够 `limit` 停下的）。
    pub exhausted: bool,
    pub unreadable: Vec<Unreadable>,
}

impl<T> LogPage<T> {
    pub fn into_items(self) -> Vec<T> {
        self.records.into_iter().map(|(_, item)| item).collect()
    }
}

/// 读改写一个工作区的任务期间握着，丢掉就释放。
///
/// 是 OS 文件锁（flock）：同一个进程里的两个连接、两个 gld 进程都挡得住。每次加锁都
/// 新开一个文件描述符，所以**同一个线程里不能套着拿**，第二次会一直等第一次——只在
/// `Harness` 的对外入口拿，内部函数不拿。进程死了内核会放锁，不留残留。
#[must_use = "丢掉就等于放开了锁"]
pub struct HarnessLock {
    file: File,
}

impl Drop for HarnessLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

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

    /// 一个工作区一把。实测不加锁时 8 个连接同时 start，4–7 个都开成了任务。
    pub fn lock(&self, workspace_id: &str) -> HarnessResult<HarnessLock> {
        let dir = self.workspace_dir(workspace_id);
        fs::create_dir_all(&dir).map_err(io_error)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join("lock"))
            .map_err(io_error)?;
        FileExt::lock_exclusive(&file).map_err(io_error)?;
        Ok(HarnessLock { file })
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

    /// 只认 `.json`：写到一半的临时文件（`x.json.tmp.<pid>.<n>`）、人挪开的坏文件
    /// （`x.json.corrupt`）都不算任务。
    pub fn list_tasks(&self, workspace_id: &str) -> HarnessResult<TaskListing> {
        let mut listing = TaskListing {
            tasks: Vec::new(),
            unreadable: Vec::new(),
        };
        let dir = self.tasks_dir(workspace_id);
        if !dir.exists() {
            return Ok(listing);
        }
        for entry in fs::read_dir(dir).map_err(io_error)? {
            let path = entry.map_err(io_error)?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            match read_json(&path) {
                Ok(task) => listing.tasks.push(task),
                Err(error) => listing.unreadable.push(Unreadable {
                    path: path.display().to_string(),
                    line: None,
                    error: error.to_string(),
                }),
            }
        }
        listing
            .tasks
            .sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(listing)
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
        append_line(&dir.join(format!("{}.jsonl", event.task_id)), event)
    }

    pub fn append_operation(
        &self,
        workspace_id: &str,
        operation: &OperationRecord,
    ) -> HarnessResult<()> {
        let dir = self.workspace_dir(workspace_id);
        fs::create_dir_all(&dir).map_err(io_error)?;
        append_line(&self.operations_path(workspace_id), operation)
    }

    pub fn list_operations(
        &self,
        workspace_id: &str,
        offset: usize,
        limit: usize,
    ) -> HarnessResult<LogPage<OperationRecord>> {
        read_log(&self.operations_path(workspace_id), offset, limit)
    }

    pub fn list_events(
        &self,
        workspace_id: &str,
        task_id: &str,
        offset: usize,
        limit: usize,
    ) -> HarnessResult<LogPage<HarnessEvent>> {
        read_log(
            &self
                .events_dir(workspace_id)
                .join(format!("{task_id}.jsonl")),
            offset,
            limit,
        )
    }
}

fn io_error(error: std::io::Error) -> HarnessError {
    HarnessError::new("STORE_IO_FAILED", error.to_string())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> HarnessResult<T> {
    let bytes = fs::read(path).map_err(io_error)?;
    serde_json::from_slice(&bytes).map_err(|e| {
        HarnessError::new(
            "STORE_CORRUPT",
            format!(
                "{} 读不出来（{e}），gld 没有改动它；修好，或者挪开（比如改名加 .corrupt）之后重试",
                path.display()
            ),
        )
    })
}

/// 先写临时文件、落盘，再改名换上去：读的人看到的要么是旧的一整份，要么是新的一整份。
fn atomic_write_json<T: serde::Serialize>(path: &Path, value: &T) -> HarnessResult<()> {
    // 临时文件名带进程号和序号。以前固定叫 `x.json.tmp`，两个写者写的是同一个临时
    // 文件，一个先改名走了，另一个改名时报 "No such file or directory"——并发 start /
    // update 实测就是这么失败的。
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| HarnessError::new("STORE_SERIALIZE_FAILED", e.to_string()))?;
    let temp = path.with_extension(format!(
        "json.tmp.{}.{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| {
        let mut file = File::create(&temp)?;
        file.write_all(&bytes)?;
        // 不先落盘，断电或系统崩溃后可能出现"改名生效了、内容还没写下去"，
        // 原来那份好的已经被换掉，留下的是空文件或半截。
        file.sync_all()?;
        fs::rename(&temp, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written.map_err(io_error)
}

/// 往 JSONL 末尾追加一行，一次 write 写完（记录和换行不分开写）。
fn append_line<T: serde::Serialize>(path: &Path, value: &T) -> HarnessResult<()> {
    let mut line = serde_json::to_vec(value)
        .map_err(|e| HarnessError::new("STORE_SERIALIZE_FAILED", e.to_string()))?;
    line.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(path)
        .map_err(io_error)?;
    // 上一次写到半行进程就没了（被杀、断电、磁盘满），文件结尾是没有换行的半行。直接
    // 接着写，新记录会粘在半行后面，两条一起读不出来。先补个换行把半行隔开：它成了
    // 一条读得到行号的坏行，新记录完好。两个写者同时补，多出来的是一个空行，读的时候跳过。
    if file.metadata().map_err(io_error)?.len() > 0 {
        let mut last = [0u8; 1];
        file.seek(SeekFrom::End(-1)).map_err(io_error)?;
        file.read_exact(&mut last).map_err(io_error)?;
        if last[0] != b'\n' {
            line.insert(0, b'\n');
        }
    }
    file.write_all(&line).map_err(io_error)
}

/// 从第 `offset` 行起读最多 `limit` 行。坏行跳过、记下行号，接着往后读。
///
/// 最后一行没有换行的不算：可能是别的线程正写到一半，也可能是写的时候进程没了。
/// 下一次追加会先把它隔开（见 [`append_line`]），那时它才作为坏行报出来。
fn read_log<T: serde::de::DeserializeOwned>(
    path: &Path,
    offset: usize,
    limit: usize,
) -> HarnessResult<LogPage<T>> {
    let mut page = LogPage {
        records: Vec::new(),
        next_offset: offset,
        exhausted: true,
        unreadable: Vec::new(),
    };
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(page),
        Err(error) => return Err(io_error(error)),
    };
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut index = 0usize;
    loop {
        line.clear();
        // 按字节读：一行不是 UTF-8 也只是这一行坏，不能让整次读失败。
        if reader.read_until(b'\n', &mut line).map_err(io_error)? == 0
            || line.last() != Some(&b'\n')
        {
            break;
        }
        if index >= offset {
            if index - offset >= limit.max(1) {
                page.exhausted = false;
                break;
            }
            let text = line.trim_ascii();
            if !text.is_empty() {
                match serde_json::from_slice(text) {
                    Ok(record) => page.records.push((index, record)),
                    Err(error) => page.unreadable.push(Unreadable {
                        path: path.display().to_string(),
                        line: Some(index + 1),
                        error: error.to_string(),
                    }),
                }
            }
            page.next_offset = index + 1;
        }
        index += 1;
    }
    Ok(page)
}

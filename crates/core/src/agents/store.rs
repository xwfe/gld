//! 委派任务的记录：`GLD_HOME/agents/<项目 id>/<任务 id>/`。
//!
//! - `task.json`：状态和结果，见 [`AgentTask`]。先写临时文件再改名，读的人看到的要么是旧的一整份，要么是新的一整份。
//! - `prompt.md`：交给 Agent 的提示全文（含 gld 加的尾巴）。ask 档要等本机放行才起，所以得先存下来。
//! - `stdout.log` / `stderr.log`：Agent 的原样输出，不脱敏；目录 0700、文件 0600。
//! - `result.txt`：结果从文件取的档案（`{result_file}`）让 Agent 写在这儿。
//!
//! 不自动清：委派一次一次是有数的，审核意见也要留着查。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::tools::runs::{create_private_dir, create_private_file};

pub const TASK_FILE: &str = "task.json";
pub const PROMPT_FILE: &str = "prompt.md";
pub const STDOUT_FILE: &str = "stdout.log";
pub const STDERR_FILE: &str = "stderr.log";
pub const RESULT_FILE: &str = "result.txt";
/// 这次任务自己的完整 diff：跑前跑后两棵工作区树之间的（见 `run::snapshot`）。
pub const DIFF_FILE: &str = "task.diff";

/// 一次委派走到哪了。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// ask 档：等用户在本机 `gld agent approve`。
    WaitingLocalApproval,
    Running,
    /// Agent 跑完了，交回审核。
    AwaitingReview,
    Accepted,
    Rejected,
    /// 退出码不是 0，或者取不到结果。改动照样列出来，照样可以审。
    Failed,
    /// 总时长到了或者太久没输出，被 gld 停的。
    Timeout,
    /// 用户或 ChatGPT 叫停的。
    Stopped,
    /// gld 正常退出时停掉的。
    Interrupted,
    /// 起它的 gld 进程没来得及记就没了（被 kill -9、崩溃）：命令可能还在跑，gld 不猜。
    Unknown,
    /// ask 档被拒。
    Denied,
    /// ask 档 24 小时没人管。
    Expired,
}

impl State {
    /// Agent 已经停了、改动也定了：可以审，也可以重新起一个。
    pub fn is_finished(self) -> bool {
        !matches!(self, State::WaitingLocalApproval | State::Running)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            State::WaitingLocalApproval => "waiting_local_approval",
            State::Running => "running",
            State::AwaitingReview => "awaiting_review",
            State::Accepted => "accepted",
            State::Rejected => "rejected",
            State::Failed => "failed",
            State::Timeout => "timeout",
            State::Stopped => "stopped",
            State::Interrupted => "interrupted",
            State::Unknown => "unknown",
            State::Denied => "denied",
            State::Expired => "expired",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    /// accept / reject
    pub verdict: String,
    pub note: String,
    pub at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTask {
    pub task_id: String,
    pub project_id: String,
    pub workspace_root: String,
    pub workspace_name: String,
    pub agent: String,
    /// 发起人（[`crate::tools::caller::Caller::key`]），不含令牌。
    pub caller: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_request_id: Option<String>,
    /// 用户给的提示，脱敏后最多 2 KB，给人看。全文在 `prompt.md`。
    pub prompt_preview: String,
    pub state: State,
    pub created_at_ms: u64,
    /// 总时长上限。ask 档等放行之后才起，所以要记下来。
    #[serde(default = "default_timeout_minutes")]
    pub timeout_minutes: u64,
    #[serde(default)]
    pub started_at_ms: Option<u64>,
    #[serde(default)]
    pub finished_at_ms: Option<u64>,
    #[serde(default)]
    pub pid: Option<u32>,
    /// 起它的 gld 进程：实例 id（判活用，见 `runs::owner_alive`）和 pid（给人看、`gld daemon restart` 认）。
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub owner_pid: Option<u32>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub agent_version: Option<String>,
    /// 版本不在档案的 `verified` 里。
    #[serde(default)]
    pub version_unverified: bool,
    /// Agent 的最后一条消息，脱敏、最多 32 KB。
    #[serde(default)]
    pub recap: Option<String>,
    #[serde(default)]
    pub failure: Option<String>,
    /// 跑前跑后两棵工作区树之间改了的文件。不是 git 仓库时为空，`changes_unknown` 为真。
    #[serde(default)]
    pub changed_files: Vec<String>,
    /// 每个文件增删了几行，和 `changed_files` 同样的文件、同样的顺序。
    #[serde(default)]
    pub file_stats: Vec<FileStat>,
    #[serde(default)]
    pub changed_files_truncated: bool,
    #[serde(default)]
    pub changes_unknown: bool,
    /// [`DIFF_FILE`] 多少字节。没算出来（不是 git 仓库）时为 None。
    #[serde(default)]
    pub diff_bytes: Option<u64>,
    /// 跑前、跑后把整个工作区写成的两棵 git 树（见 `run::snapshot`），`git diff <前> <后>` 就是 [`DIFF_FILE`]。
    /// 没人引用，过期后 `git gc` 会清掉，所以 diff 另存了一份。记不下来（不是 git 仓库）时为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_tree: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_tree: Option<String>,
    /// 跑的过程中 HEAD 动了：Agent 被叮嘱别提交，还是提交了。改动照样都在 diff 里。
    #[serde(default)]
    pub head_moved: bool,
    #[serde(default)]
    pub review: Option<Review>,
}

/// 一个文件这次增删了几行（`git diff --numstat`）。二进制文件数不了行，两个都是 None。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStat {
    pub path: String,
    pub added: Option<u64>,
    pub deleted: Option<u64>,
}

fn default_timeout_minutes() -> u64 {
    super::DEFAULT_TIMEOUT_MINUTES
}

/// `GLD_HOME/agents`
pub fn root(data_home: &Path) -> PathBuf {
    data_home.join("agents")
}

pub fn task_dir(root: &Path, project_id: &str, task_id: &str) -> PathBuf {
    root.join(project_id).join(task_id)
}

/// 任务 id 是 gld 自己生成的 uuid；调用方传进来的要先过这一关，不然 `../x` 就拼到目录外面去了。
pub fn valid_id(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok() && !id.contains(['/', '\\', '.'])
}

pub fn read(dir: &Path) -> Option<AgentTask> {
    let bytes = fs::read(dir.join(TASK_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn write(dir: &Path, task: &AgentTask) -> std::io::Result<()> {
    create_private_dir(dir)?;
    let bytes = serde_json::to_vec_pretty(task).map_err(std::io::Error::other)?;
    let temp = dir.join(format!("{TASK_FILE}.tmp.{}", std::process::id()));
    let written = create_private_file(&temp)
        .and_then(|mut file| file.write_all(&bytes))
        .and_then(|()| fs::rename(&temp, dir.join(TASK_FILE)));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

pub fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    create_private_file(path).and_then(|mut file| file.write_all(text.as_bytes()))
}

/// 按任务 id 找它的目录：任务 id 全局唯一，调用方不用知道是哪个项目的。
pub fn find(root: &Path, task_id: &str) -> Option<PathBuf> {
    if !valid_id(task_id) {
        return None;
    }
    fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|project| project.path().join(task_id))
        .find(|dir| dir.join(TASK_FILE).is_file())
}

/// 全部任务（`project_id` 给了就只看这个项目的），按创建时间从新到旧。
pub fn list(root: &Path, project_id: Option<&str>) -> Vec<(PathBuf, AgentTask)> {
    let Ok(projects) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut tasks: Vec<(PathBuf, AgentTask)> = projects
        .flatten()
        .filter(|project| {
            project_id.is_none_or(|wanted| project.file_name().to_str() == Some(wanted))
        })
        .filter_map(|project| fs::read_dir(project.path()).ok())
        .flat_map(|entries| entries.flatten())
        .filter(|entry| entry.file_name().to_str().is_some_and(valid_id))
        .filter_map(|entry| {
            let dir = entry.path();
            read(&dir).map(|task| (dir, task))
        })
        .collect();
    tasks.sort_by(|a, b| (b.1.created_at_ms, &b.1.task_id).cmp(&(a.1.created_at_ms, &a.1.task_id)));
    tasks
}

/// 文件的最后 `max` 字节，按 UTF-8 边界切，给"最后几行输出"用。
pub fn tail(path: &Path, max: usize) -> String {
    let Ok(bytes) = fs::read(path) else {
        return String::new();
    };
    let start = bytes.len().saturating_sub(max);
    let mut text = String::from_utf8_lossy(&bytes[start..]).into_owned();
    if start > 0 {
        // 从中间切开的那一行不要。
        if let Some(newline) = text.find('\n') {
            text.drain(..=newline);
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, project: &str, created: u64) -> AgentTask {
        AgentTask {
            task_id: id.into(),
            project_id: project.into(),
            workspace_root: "/w".into(),
            workspace_name: "w".into(),
            agent: "codex".into(),
            caller: "local".into(),
            client_request_id: None,
            prompt_preview: "fix".into(),
            state: State::Running,
            created_at_ms: created,
            timeout_minutes: 30,
            started_at_ms: None,
            finished_at_ms: None,
            pid: None,
            owner: None,
            owner_pid: None,
            exit_code: None,
            agent_version: None,
            version_unverified: false,
            recap: None,
            failure: None,
            changed_files: Vec::new(),
            file_stats: Vec::new(),
            changed_files_truncated: false,
            changes_unknown: false,
            diff_bytes: None,
            before_tree: None,
            after_tree: None,
            head_moved: false,
            review: None,
        }
    }

    #[test]
    fn tasks_round_trip_and_are_found_by_id_across_projects() {
        let home = tempfile::tempdir().expect("home");
        let root = root(home.path());
        let (a, b) = (
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        );
        write(&task_dir(&root, "p1", &a), &task(&a, "p1", 1)).expect("写");
        write(&task_dir(&root, "p2", &b), &task(&b, "p2", 2)).expect("写");

        let found = find(&root, &b).expect("找得到");
        assert_eq!(read(&found).expect("读").project_id, "p2");
        let all: Vec<String> = list(&root, None)
            .into_iter()
            .map(|(_, t)| t.task_id)
            .collect();
        assert_eq!(all, vec![b.clone(), a.clone()], "新的在前");
        assert_eq!(list(&root, Some("p1")).len(), 1);
        for bad in ["../p1", "x", "", "a/b"] {
            assert!(find(&root, bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn tail_keeps_whole_lines_from_the_end() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("out");
        fs::write(&path, "first line\nsecond line\nthird\n").expect("write");
        assert_eq!(tail(&path, 1000), "first line\nsecond line\nthird\n");
        assert_eq!(tail(&path, 14), "third\n");
        assert_eq!(tail(&dir.path().join("missing"), 10), "");
    }
}

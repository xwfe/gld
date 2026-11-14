//! 委派结果的有界读取与应用回包，审批和运行状态由父模块提供。

use std::fs;
use std::path::Path;

use super::store::{self, AgentTask, State};
use super::{handoff_dir, not_in_state, AgentError, REPORT_MAX_BYTES};

/// 给人看、给 ChatGPT 看的一份：任务本身，加交接目录、Agent 写的报告、这次任务自己的 diff、没收尾时最后一段输出。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TaskDetail {
    #[serde(flatten)]
    pub task: AgentTask,
    pub handoff_dir: String,
    #[serde(default)]
    pub report: Option<String>,
    /// 这次任务自己改了什么（跑前跑后两棵工作区树之间），脱敏、最多 [`DIFF_MAX_BYTES`]。
    /// 跟 HEAD 比会混进之前没提交的改动，审核看这个。给 AI 的回包里超过一页就不附（`tools::agent::present`），
    /// 本机 `gld agent show` 照样打这一页。
    #[serde(default)]
    pub diff: Option<String>,
    #[serde(default)]
    pub diff_truncated: bool,
    /// 截断了才有：剩下的从这个字节位置起，用 `agent_manage action=diff offset=` 接着读。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_next_offset: Option<u64>,
    /// 没收尾（在跑、失败、超时）时才有：最后一段 stdout / stderr。
    #[serde(default)]
    pub output_tail: Option<OutputTail>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OutputTail {
    pub stdout: String,
    pub stderr: String,
}

/// 收尾之后（失败、超时、被停）最后一段输出最多回多少字节：要看它死在哪。
pub const OUTPUT_TAIL_BYTES: usize = 4096;
/// 在跑时每次 `wait` 都回一段，只要看得出还在动就行。Codex 的 `--json` 输出是事件 JSON，
/// 套进回包再转义一层就翻倍：以前给 4 KB，一次 `wait` 回包 14 KB（审查 §44）。
pub const RUNNING_TAIL_BYTES: usize = 768;
/// 这次任务自己的 diff 一次最多回多少字节（`status` / `wait` 回包里的、`action=diff` 每一页），全文在任务目录的 `task.diff`。
pub const DIFF_MAX_BYTES: usize = 64 * 1024;

pub fn detail(dir: &Path, task: AgentTask) -> TaskDetail {
    let tail_bytes = if task.state == State::Running {
        RUNNING_TAIL_BYTES
    } else {
        OUTPUT_TAIL_BYTES
    };
    let output_tail = matches!(
        task.state,
        State::Running | State::Failed | State::Timeout | State::Stopped | State::Interrupted
    )
    .then(|| {
        let (mut stdout, mut stderr) = output_tail(dir, tail_bytes);
        crate::tools::history::redact_text(&mut stdout);
        crate::tools::history::redact_text(&mut stderr);
        OutputTail { stdout, stderr }
    });
    // 还没收尾或者没算出来，就没有这个文件。
    let (diff, diff_next_offset) =
        match read_diff(&dir.join(store::DIFF_FILE), 0, u64::MAX, DIFF_MAX_BYTES) {
            Ok((text, next)) => (Some(text), next),
            Err(_) => (None, None),
        };
    TaskDetail {
        handoff_dir: handoff_dir(Path::new(&task.workspace_root), &task.task_id)
            .display()
            .to_string(),
        report: report(&task),
        diff,
        diff_truncated: diff_next_offset.is_some(),
        diff_next_offset,
        output_tail,
        task,
    }
}

/// `action=diff` 的一页。
#[derive(Debug, Clone, serde::Serialize)]
pub struct DiffPage {
    pub task_id: String,
    /// 只读一个文件时是那个文件。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// 这一页从 `task.diff` 的第几个字节起。给了 `path` 也是整份 diff 里的位置。
    pub offset: u64,
    /// 下一页从哪起；读完了（给了 `path` 就是读完那个文件）是 None。
    pub next_offset: Option<u64>,
    /// `task.diff` 一共多少字节。
    pub total_bytes: u64,
    pub diff: String,
}

/// 跑完的任务的 diff，从 `offset` 字节起一页最多 `max` 字节（调用方限在 1 KB 到 [`DIFF_MAX_BYTES`]）。
/// 回包里的 diff 截断了，剩下的靠它一页页读：AI 读不到 gld 数据目录里的 `task.diff`（审查 §47）。
///
/// 给了 `path` 只读那个文件的一段，`offset` 默认是它的开头、页到它的结尾为止：大 diff 里先看源代码，
/// 不用先读完前面一百多 KB 的生成数据（审查 §50）。`offset` 一直是整份 diff 里的位置，给了 `path` 也一样。
pub fn diff_page(
    dir: &Path,
    task: &AgentTask,
    offset: Option<u64>,
    path: Option<&str>,
    max: usize,
) -> Result<DiffPage, AgentError> {
    if !task.state.is_finished() {
        return Err(not_in_state(task, "已经跑完"));
    }
    let diff_file = dir.join(store::DIFF_FILE);
    let Ok(meta) = fs::metadata(&diff_file) else {
        let why = match task.state {
            State::Denied | State::Expired => "这个任务没跑过",
            State::Unknown => "起它的 gld 进程没来得及收尾，没算 diff",
            _ if task.changes_unknown => "项目不是 git 仓库（或者跑前跑后记不下工作区），没算 diff",
            _ => "任务目录里没有 task.diff：这个任务早于 gld 存 diff 的版本，或者当时没写进去",
        };
        return Err(AgentError::new(
            "AGENT_DIFF_UNAVAILABLE",
            format!("任务 {} 没有 diff：{why}", task.task_id),
        ));
    };
    let total_bytes = meta.len();
    let (start, end) = match path {
        Some(path) => file_section(task, path)?,
        None => (0, total_bytes),
    };
    let offset = offset.unwrap_or(start);
    if offset < start || offset > end {
        let message = match path {
            Some(path) => format!("offset {offset} 不在 {path} 那一段（{start}–{end} 字节）里"),
            None => format!("offset {offset} 超过了 diff 的总长 {total_bytes} 字节"),
        };
        return Err(AgentError::new("INVALID_ARGUMENT", message));
    }
    let (diff, next_offset) = read_diff(&diff_file, offset, end, max.max(1)).map_err(|error| {
        AgentError::new(
            "AGENT_STATE_UNAVAILABLE",
            format!("读不了 {}：{error}", diff_file.display()),
        )
    })?;
    Ok(DiffPage {
        task_id: task.task_id.clone(),
        path: path.map(str::to_string),
        offset,
        next_offset,
        total_bytes,
        diff,
    })
}

/// 一个文件那一段在 `task.diff` 里的起止字节。
fn file_section(task: &AgentTask, path: &str) -> Result<(u64, u64), AgentError> {
    let unrecorded = || {
        AgentError::new(
            "AGENT_DIFF_UNAVAILABLE",
            format!(
                "任务 {} 没记每个文件在 diff 里的位置（早于 gld 记位置的版本），只能按 offset 读",
                task.task_id
            ),
        )
    };
    match task.file_stats.iter().find(|stat| stat.path == path) {
        Some(stat) => match (stat.diff_offset, stat.diff_bytes) {
            (Some(offset), Some(bytes)) => Ok((offset, offset + bytes)),
            _ => Err(unrecorded()),
        },
        None if task.changed_files.iter().any(|changed| changed == path) => Err(unrecorded()),
        None => {
            let more = if task.changed_files_truncated {
                "；文件列表只记了前 200 个，后面的只能按 offset 读"
            } else {
                ""
            };
            Err(AgentError::new(
                "INVALID_ARGUMENT",
                format!("这次任务没改 {path}，改了的文件见 file_stats{more}"),
            ))
        }
    }
}

/// diff 文件从 `offset` 起、读到 `end` 为止最多 `max` 字节，脱敏。返回这一段和下一段从哪起（读到 `end` 了是 None）。
///
/// 截在整行上：截在半行上看着像改了半行，切成两半的密钥也可能认不出来、漏过脱敏。一行本身就比 `max` 长（压缩过的文件）
/// 才在行中间切，切在 UTF-8 字符边界上，下一页从同一个字符接着读。只读要用的这一段，diff 再大也不整个读进内存。
fn read_diff(
    path: &Path,
    offset: u64,
    end: u64,
    max: usize,
) -> std::io::Result<(String, Option<u64>)> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = fs::File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    // 剩下的不止一页时多读一个字节：读得到就说明后面还有，也用它判断切点是不是字符边界。
    let want = end.saturating_sub(offset).min(max as u64 + 1);
    let mut bytes = Vec::with_capacity(want as usize);
    file.take(want).read_to_end(&mut bytes)?;
    let mut next = None;
    if bytes.len() > max {
        let end = match bytes[..max].iter().rposition(|byte| *byte == b'\n') {
            Some(newline) => newline + 1,
            None => (1..=max)
                .rev()
                .find(|&end| bytes[end] & 0xC0 != 0x80)
                .unwrap_or(max),
        };
        bytes.truncate(end);
        next = Some(offset + end as u64);
    }
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    crate::tools::history::redact_text(&mut text);
    Ok((text, next))
}

/// Agent 写的详细报告，脱敏、最多 [`REPORT_MAX_BYTES`]。没有就是 None。
pub fn report(task: &AgentTask) -> Option<String> {
    let path = handoff_dir(Path::new(&task.workspace_root), &task.task_id).join("report.md");
    let bytes = fs::read(path).ok()?;
    let mut text =
        String::from_utf8_lossy(&bytes[..bytes.len().min(REPORT_MAX_BYTES)]).into_owned();
    if bytes.len() > REPORT_MAX_BYTES {
        text.push_str("\n…[截断]");
    }
    crate::tools::history::redact_text(&mut text);
    Some(text)
}

/// 在跑的输出最后一段，给 `status` / `wait` 看进度。
pub fn output_tail(dir: &Path, max: usize) -> (String, String) {
    (
        store::tail(&dir.join(store::STDOUT_FILE), max),
        store::tail(&dir.join(store::STDERR_FILE), max),
    )
}

#[cfg(test)]
mod tests {
    use super::read_diff;

    /// 一行比一页还长（压缩过的文件）就在行中间切，切在字符边界上；一页页接起来不丢不重。
    #[test]
    fn pages_cut_inside_an_overlong_line_on_a_char_boundary() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("task.diff");
        for (text, max) in [("aéééb", 4), ("ab\ncdefgh\n", 5), ("", 4)] {
            std::fs::write(&path, text).expect("write");
            let (mut whole, mut offset) = (String::new(), 0);
            loop {
                let (page, next) = read_diff(&path, offset, u64::MAX, max).expect("读");
                assert!(page.len() <= max && !page.contains('\u{FFFD}'), "{page:?}");
                whole.push_str(&page);
                match next {
                    Some(next) => {
                        assert!(next > offset, "要往前走");
                        offset = next;
                    }
                    None => break,
                }
            }
            assert_eq!(whole, text);
        }
    }
}

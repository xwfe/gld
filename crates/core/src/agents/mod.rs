//! 委派：把一段活交给本机的编码 Agent（Codex、Claude Code……）用命令行跑，做完交回审核。
//!
//! 设计和取舍见 [RFC-0009](../../../../docs/rfc/0009-agent-delegation.md)。这里是 D1：
//!
//! - 档案表（[`profile`]）：每种 Agent 怎么调，内置 codex、claude，`agents.toml` 可改可加；
//! - 任务记录（[`store`]）：`GLD_HOME/agents/<项目 id>/<任务 id>/`；
//! - 运行器（[`run`]）：起进程、两道超时、停整组、收尾时比 git 快照算改动。
//!
//! Agent 一律全权跑（用户定的：不限制在沙箱，做完交回审核）。所以把关在起之前（项目开关、
//! ask 档本机点头、只有服务主凭据能发起，见 `tools::agent`）和做完之后（`awaiting_review`
//! → `review`，验收仍只认 gld 自己跑的命令）。

pub mod profile;
pub mod run;
pub mod store;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use profile::AgentProfile;
use store::{AgentTask, Review, State};

pub use run::{interrupt_all, running_in};

/// 提示最长多少字节。再长的放 `brief` 里。
pub const PROMPT_MAX_BYTES: usize = 64 * 1024;
/// 交接报告最多回多少字节。
pub const REPORT_MAX_BYTES: usize = 64 * 1024;
pub const DEFAULT_TIMEOUT_MINUTES: u64 = 30;
pub const MAX_TIMEOUT_MINUTES: u64 = 120;
/// ask 档等多久没人放行就作废。
pub const APPROVAL_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentError {
    pub code: &'static str,
    pub message: String,
}

impl AgentError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

/// 项目开关 `agent-delegation` 的三档。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approval {
    /// 每次要用户在本机 `gld agent approve`。
    Ask,
    /// 直接起。
    Auto,
}

pub struct StartRequest<'a> {
    pub data_home: &'a Path,
    pub workspace_root: &'a Path,
    pub workspace_name: &'a str,
    pub caller: &'a str,
    pub agent: &'a str,
    pub prompt: &'a str,
    pub brief: Option<&'a str>,
    pub client_request_id: Option<&'a str>,
    pub approval: Approval,
    /// 项目的 `agent-approval`：all | self。
    pub agent_approval: &'a str,
    /// AI 提议的模型、推理强度；不给就用档案默认。
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
    pub timeout_minutes: Option<u64>,
}

pub fn agents_toml(data_home: &Path) -> PathBuf {
    data_home.join("agents.toml")
}

pub fn profiles(
    data_home: &Path,
) -> Result<std::collections::BTreeMap<String, AgentProfile>, AgentError> {
    profile::load(&agents_toml(data_home))
        .map_err(|message| AgentError::new("AGENT_PROFILES_INVALID", message))
}

fn find_profile(data_home: &Path, agent: &str) -> Result<AgentProfile, AgentError> {
    let mut profiles = profiles(data_home)?;
    profiles.remove(agent).ok_or_else(|| {
        AgentError::new(
            "AGENT_UNKNOWN",
            format!(
                "没有叫「{agent}」的 Agent 档案。有：{}（在 {} 里可以加）",
                profiles.keys().cloned().collect::<Vec<_>>().join("、"),
                agents_toml(data_home).display()
            ),
        )
    })
}

/// 发起一个委派。auto 档直接起；ask 档存下来等本机放行。
pub fn start(request: StartRequest<'_>) -> Result<AgentTask, AgentError> {
    let prompt = request.prompt.trim();
    if prompt.is_empty() {
        return Err(AgentError::new("INVALID_ARGUMENT", "prompt 不能是空的"));
    }
    if prompt.len() > PROMPT_MAX_BYTES {
        return Err(AgentError::new(
            "INVALID_ARGUMENT",
            format!("prompt 超过 {PROMPT_MAX_BYTES} 字节；背景材料放进 brief"),
        ));
    }
    let timeout_minutes = request
        .timeout_minutes
        .unwrap_or(DEFAULT_TIMEOUT_MINUTES)
        .clamp(1, MAX_TIMEOUT_MINUTES);
    let profile = find_profile(request.data_home, request.agent)?;
    let agent_approval = if request.agent_approval == "self" {
        "self"
    } else {
        "all"
    };
    check_approval(request.data_home, &profile, agent_approval)?;
    let (model, effort) = profile
        .resolve(request.model, request.effort)
        .map_err(|message| AgentError::new("INVALID_ARGUMENT", message))?;
    let root = request
        .workspace_root
        .canonicalize()
        .unwrap_or_else(|_| request.workspace_root.to_path_buf());
    let project_id = crate::tools::runs::project_id(&root);
    let agents_root = store::root(request.data_home);

    if let Some(wanted) = request.client_request_id {
        let same = list(request.data_home, Some(&project_id))
            .into_iter()
            .find(|task| {
                task.caller == request.caller && task.client_request_id.as_deref() == Some(wanted)
            });
        if let Some(task) = same {
            return Ok(task);
        }
    }
    if let Some(other) = running_in(&root) {
        return Err(busy(&other));
    }

    let task_id = uuid::Uuid::new_v4().to_string();
    let handoff = prepare_handoff(&root, &task_id, request.brief)
        .map_err(|error| AgentError::new("AGENT_HANDOFF_UNAVAILABLE", error))?;
    let full_prompt = compose_prompt(prompt, &task_id, &handoff, request.brief.is_some());
    let mut preview: String = prompt.chars().take(2048).collect();
    crate::tools::history::redact_text(&mut preview);
    let task = AgentTask {
        task_id: task_id.clone(),
        project_id: project_id.clone(),
        workspace_root: root.display().to_string(),
        workspace_name: request.workspace_name.to_string(),
        agent: request.agent.to_string(),
        caller: request.caller.to_string(),
        client_request_id: request.client_request_id.map(str::to_string),
        prompt_preview: preview,
        agent_approval: agent_approval.to_string(),
        model,
        effort,
        approval_changes: Vec::new(),
        state: State::WaitingLocalApproval,
        created_at_ms: run::now_ms(),
        timeout_minutes,
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
    };
    let dir = store::task_dir(&agents_root, &project_id, &task_id);
    store::write(&dir, &task)
        .and_then(|()| store::write_private(&dir.join(store::PROMPT_FILE), &full_prompt))
        .map_err(|error| {
            AgentError::new(
                "AGENT_STATE_UNAVAILABLE",
                format!("任务记录写不进去：{error}"),
            )
        })?;
    match request.approval {
        Approval::Ask => Ok(task),
        Approval::Auto => launch(request.data_home, dir, task, profile, full_prompt),
    }
}

/// 放行时用户在本机改的：换 Agent、模型、推理强度。都不给就照 AI 的提议跑。
#[derive(Debug, Clone, Copy, Default)]
pub struct ApprovalChange<'a> {
    pub agent: Option<&'a str>,
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
}

/// ask 档放行：照 AI 的提议起它。
pub fn approve(data_home: &Path, task_id: &str) -> Result<AgentTask, AgentError> {
    approve_with(data_home, task_id, ApprovalChange::default())
}

/// ask 档放行，顺手改 Agent、模型、推理强度（用户要的"AI 提议，你拍板"，2026-10-24）。
///
/// 换了 Agent 就不沿用 AI 给原来那家提议的模型和强度，各取新那家的默认，除非这次一起改了。项目的 `agents` 名单管的是
/// AI 能选哪几家，用户在本机换谁都行。所有核对都在改任务记录之前：核对不过任务原样留着等放行。
pub fn approve_with(
    data_home: &Path,
    task_id: &str,
    change: ApprovalChange<'_>,
) -> Result<AgentTask, AgentError> {
    let (dir, mut task) = get(data_home, task_id)?;
    if task.state != State::WaitingLocalApproval {
        return Err(not_in_state(&task, "等本机放行"));
    }
    let agent = change
        .agent
        .map(str::trim)
        .filter(|agent| !agent.is_empty())
        .unwrap_or(&task.agent)
        .to_string();
    let switched = agent != task.agent;
    let profile = find_profile(data_home, &agent)?;
    let keep = |value: &str| (!switched && !value.is_empty()).then(|| value.to_string());
    let model = change
        .model
        .map(str::to_string)
        .or_else(|| keep(&task.model));
    let effort = change
        .effort
        .map(str::to_string)
        .or_else(|| keep(&task.effort));
    let (model, effort) = profile
        .resolve(model.as_deref(), effort.as_deref())
        .map_err(|message| AgentError::new("INVALID_ARGUMENT", message))?;
    check_approval(data_home, &profile, &task.agent_approval)?;
    let shown = |value: &str| {
        if value.is_empty() {
            "默认".to_string()
        } else {
            value.to_string()
        }
    };
    let mut changes = Vec::new();
    if switched {
        changes.push(format!("agent {} → {agent}", task.agent));
    }
    if model != task.model {
        changes.push(format!("model {} → {}", shown(&task.model), shown(&model)));
    }
    if effort != task.effort {
        changes.push(format!(
            "effort {} → {}",
            shown(&task.effort),
            shown(&effort)
        ));
    }
    task.agent = agent;
    task.model = model;
    task.effort = effort;
    task.approval_changes = changes;
    let prompt = fs::read_to_string(dir.join(store::PROMPT_FILE)).map_err(|error| {
        AgentError::new(
            "AGENT_STATE_UNAVAILABLE",
            format!("读不了存下的提示：{error}"),
        )
    })?;
    launch(data_home, dir, task, profile, prompt)
}

/// 要"自行判断"那一档，档案里就得有 `self_command`。没有就拒，**不退回全放开**：那等于背着用户把权放大。
/// 起之前查一次；ask 档放行时再查一次（这期间 `agents.toml` 可能改过）。
fn check_approval(
    data_home: &Path,
    profile: &AgentProfile,
    agent_approval: &str,
) -> Result<(), AgentError> {
    if agent_approval != "self" || profile.supports_self_approval() {
        return Ok(());
    }
    let supported: Vec<String> = profiles(data_home)?
        .into_values()
        .filter(AgentProfile::supports_self_approval)
        .map(|profile| profile.name)
        .collect();
    Err(AgentError::new(
        "AGENT_APPROVAL_UNSUPPORTED",
        format!(
            "{} 没有\"自行判断、自行批准\"那一档，而这个项目设的是 agent-approval=self。有这一档的：{}。\
             要用 {} 就换一个 Agent，或者请用户改成 agent-approval=all（一键批准所有操作）",
            profile.name,
            supported.join("、"),
            profile.name
        ),
    ))
}

fn launch(
    data_home: &Path,
    dir: PathBuf,
    mut task: AgentTask,
    profile: AgentProfile,
    prompt: String,
) -> Result<AgentTask, AgentError> {
    check_approval(data_home, &profile, &task.agent_approval)?;
    let agents_root = store::root(data_home);
    let owner = crate::tools::runs::take_owner_lock(&agents_root, |_| {}).ok_or_else(|| {
        AgentError::new(
            "AGENT_STATE_UNAVAILABLE",
            format!("拿不到 {} 下的进程锁，委派没起", agents_root.display()),
        )
    })?;
    let root = PathBuf::from(&task.workspace_root);
    run::reserve(&task.task_id, &root).map_err(|other| busy(&other))?;
    task.owner = Some(owner);
    let total_timeout = Duration::from_secs(task.timeout_minutes * 60);
    run::launch(run::Launch {
        task,
        task_dir: dir,
        profile,
        prompt,
        total_timeout,
    })
    .map_err(|message| AgentError::new("AGENT_SPAWN_FAILED", message))
}

pub fn deny(data_home: &Path, task_id: &str) -> Result<AgentTask, AgentError> {
    let (dir, mut task) = get(data_home, task_id)?;
    if task.state != State::WaitingLocalApproval {
        return Err(not_in_state(&task, "等本机放行"));
    }
    task.state = State::Denied;
    task.finished_at_ms = Some(run::now_ms());
    save(&dir, &task)?;
    Ok(task)
}

/// 叫停：在跑的停进程（异步收尾，状态稍后变成 stopped）；还在等放行的直接作废。
pub fn stop(data_home: &Path, task_id: &str) -> Result<AgentTask, AgentError> {
    let (dir, mut task) = get(data_home, task_id)?;
    match task.state {
        State::WaitingLocalApproval => {
            task.state = State::Stopped;
            task.finished_at_ms = Some(run::now_ms());
            save(&dir, &task)?;
            Ok(task)
        }
        State::Running if run::request_stop(&task.task_id) => Ok(task),
        State::Running => Err(AgentError::new(
            "AGENT_NOT_HERE",
            format!(
                "任务 {} 是 pid {} 的 gld 进程起的，不在这个进程里，停不了；在起它的那个进程里停（一般是守护进程：gld agent stop）",
                task.task_id,
                task.owner_pid.map_or("?".into(), |pid| pid.to_string())
            ),
        )),
        _ => Err(not_in_state(&task, "在跑或等放行")),
    }
}

/// 审核：Agent 跑完以后给结论，意见存进任务记录和交接目录的 `review.md`。
pub fn review(
    data_home: &Path,
    task_id: &str,
    verdict: &str,
    note: &str,
) -> Result<AgentTask, AgentError> {
    let (dir, mut task) = get(data_home, task_id)?;
    let state = match verdict {
        "accept" => State::Accepted,
        "reject" => State::Rejected,
        other => {
            return Err(AgentError::new(
                "INVALID_ARGUMENT",
                format!("verdict 只能是 accept 或 reject，给的是「{other}」"),
            ))
        }
    };
    if !task.state.is_finished() || matches!(task.state, State::Denied | State::Expired) {
        return Err(not_in_state(&task, "已经跑完"));
    }
    task.state = state;
    task.review = Some(Review {
        verdict: verdict.to_string(),
        note: note.to_string(),
        at_ms: run::now_ms(),
    });
    let handoff = handoff_dir(Path::new(&task.workspace_root), &task.task_id);
    if handoff.is_dir() {
        let _ = fs::write(
            handoff.join("review.md"),
            format!("# 审核：{verdict}\n\n{note}\n"),
        );
    }
    save(&dir, &task)?;
    Ok(task)
}

/// 按任务 id 找。命令行敲完整的 uuid 太长：给前 4 位以上、只对得上一个就认。
pub fn get(data_home: &Path, task_id: &str) -> Result<(PathBuf, AgentTask), AgentError> {
    let agents_root = store::root(data_home);
    let task_id = resolve_id(&agents_root, task_id)?;
    let dir = store::find(&agents_root, &task_id)
        .ok_or_else(|| AgentError::new("AGENT_TASK_NOT_FOUND", format!("没有任务 {task_id}")))?;
    let task = store::read(&dir).ok_or_else(|| {
        AgentError::new(
            "AGENT_STATE_UNAVAILABLE",
            format!("{} 读不出来", dir.display()),
        )
    })?;
    let task = settle(&agents_root, &dir, task);
    Ok((dir, task))
}

fn resolve_id(agents_root: &Path, given: &str) -> Result<String, AgentError> {
    let given = given.trim();
    if store::valid_id(given) {
        return Ok(given.to_string());
    }
    let not_found = || AgentError::new("AGENT_TASK_NOT_FOUND", format!("没有任务 {given}"));
    if given.len() < 4 || !given.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return Err(not_found());
    }
    let matches: Vec<String> = store::list(agents_root, None)
        .into_iter()
        .map(|(_, task)| task.task_id)
        .filter(|id| id.starts_with(given))
        .collect();
    match matches.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(not_found()),
        many => Err(AgentError::new(
            "AGENT_TASK_AMBIGUOUS",
            format!(
                "{given} 对得上 {} 个任务，多给几位：{}",
                many.len(),
                many.join("、")
            ),
        )),
    }
}

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

/// 全部任务（给了项目 id 就只看这个项目），新的在前。
pub fn list(data_home: &Path, project_id: Option<&str>) -> Vec<AgentTask> {
    let agents_root = store::root(data_home);
    store::list(&agents_root, project_id)
        .into_iter()
        .map(|(dir, task)| settle(&agents_root, &dir, task))
        .collect()
}

/// 等它跑完，最多等 `max`。不在跑的立刻返回。
pub fn wait(
    data_home: &Path,
    task_id: &str,
    max: Duration,
) -> Result<(PathBuf, AgentTask), AgentError> {
    let deadline = Instant::now() + max;
    loop {
        let (dir, task) = get(data_home, task_id)?;
        if task.state != State::Running || Instant::now() >= deadline {
            return Ok((dir, task));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// pid 为 `owner_pid` 的 gld 进程起的、还在跑的委派。`gld daemon restart` 动手前看这个。
pub fn running_under(data_home: &Path, owner_pid: u32) -> Vec<AgentTask> {
    list(data_home, None)
        .into_iter()
        .filter(|task| task.state == State::Running && task.owner_pid == Some(owner_pid))
        .collect()
}

/// 项目里的交接目录。
pub fn handoff_dir(workspace_root: &Path, task_id: &str) -> PathBuf {
    workspace_root.join(".gld").join("handoff").join(task_id)
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

fn busy(other: &str) -> AgentError {
    AgentError::new(
        "AGENT_TASK_RUNNING",
        format!(
            "这个项目已经有委派在跑（任务 {other}），同一个项目同时只许一个；等它跑完或者先停掉"
        ),
    )
}

fn not_in_state(task: &AgentTask, wanted: &str) -> AgentError {
    AgentError::new(
        "AGENT_TASK_STATE",
        format!(
            "任务 {} 现在是 {}，要{wanted}才行",
            task.task_id,
            task.state.as_str()
        ),
    )
}

fn save(dir: &Path, task: &AgentTask) -> Result<(), AgentError> {
    store::write(dir, task).map_err(|error| {
        AgentError::new(
            "AGENT_STATE_UNAVAILABLE",
            format!("任务记录写不进去：{error}"),
        )
    })
}

/// 读到的状态要是已经不成立了，改过来并写回去（下一个读的人看到的是同一个结论）。
///
/// - `running`：本进程起的却不在本进程的表里，或者起它的进程不在了 → `unknown`。不猜结局、不杀那个 pid。
/// - `waiting_local_approval` 超过 24 小时 → `expired`。
fn settle(agents_root: &Path, dir: &Path, mut task: AgentTask) -> AgentTask {
    let changed = match task.state {
        State::Running => {
            let lost = match task.owner.as_deref() {
                Some(owner) if owner == crate::tools::runs::instance_id() => {
                    !run::is_live(&task.task_id)
                }
                Some(owner) => !crate::tools::runs::owner_alive(agents_root, owner),
                None => true,
            };
            if lost {
                task.state = State::Unknown;
            }
            lost
        }
        State::WaitingLocalApproval => {
            let expired =
                run::now_ms().saturating_sub(task.created_at_ms) >= APPROVAL_TTL.as_millis() as u64;
            if expired {
                task.state = State::Expired;
            }
            expired
        }
        _ => false,
    };
    if changed {
        task.finished_at_ms = Some(run::now_ms());
        let _ = store::write(dir, &task);
    }
    task
}

/// 建交接目录 `.gld/handoff/<任务 id>/`。`.gld/` 第一次建时放一个只写 `*` 的 `.gitignore`，
/// 整个目录不进 git status，也就不会被算成 Agent 的改动。
fn prepare_handoff(root: &Path, task_id: &str, brief: Option<&str>) -> Result<String, String> {
    let gld_dir = root.join(".gld");
    let handoff_root = gld_dir.join("handoff");
    let first_gld = !gld_dir.exists();
    let first_handoff = !handoff_root.exists();
    let dir = handoff_dir(root, task_id);
    fs::create_dir_all(&dir).map_err(|error| format!("建不了 {}：{error}", dir.display()))?;
    // `.gld/` 是 Planning 也在用的目录，规矩跟它（planning::store 的 ignore_self_in_git）：只在第一次建出来时
    // 放只写 `*` 的 `.gitignore`。用户删掉它是想让团队共享 Goal / Plan，不补回来。
    if first_gld {
        let _ = fs::write(
            gld_dir.join(".gitignore"),
            "# gld 在这个项目里的本地状态，默认不进版本库\n*\n",
        );
    }
    // 交接目录自己再放一个：哪怕用户共享了 `.gld/`，交接文件（Agent 写的报告、审核意见）也不进版本库，
    // 跑前跑后的 git 快照也就不会把它们算成 Agent 的改动。
    if first_handoff {
        let ignore = handoff_root.join(".gitignore");
        fs::write(&ignore, "*\n")
            .map_err(|error| format!("写不了 {}：{error}", ignore.display()))?;
    }
    if let Some(brief) = brief {
        fs::write(dir.join("brief.md"), brief)
            .map_err(|error| format!("写不了 brief.md：{error}"))?;
    }
    Ok(format!(".gld/handoff/{task_id}"))
}

/// 用户的提示后面加的固定尾巴：交接目录在哪、全权直接改、别提交、结尾写四段 recap。
fn compose_prompt(prompt: &str, task_id: &str, handoff: &str, has_brief: bool) -> String {
    let brief = if has_brief {
        "先读其中的 brief.md（背景、约束、验收标准）。"
    } else {
        ""
    };
    format!(
        "{prompt}\n\n---\n以下是 gld 加的说明（委派任务 {task_id}）：\n\
         - 交接目录：{handoff}/。{brief}\n\
         - 你有全部权限，直接在这个项目里动手改，改完自己跑相关的测试。\n\
         - 不要 git commit、不要推送：改动留在工作区，做完交回审核。\n\
         - 结束时最后一条消息写一段 recap，分四段：做了什么；改了哪些文件；跑了什么验证、结果如何；没做完的和下一步。\n\
         - 要交代的内容多，就把详细报告写到 {handoff}/report.md。\n"
    )
}

#[cfg(test)]
mod tests;

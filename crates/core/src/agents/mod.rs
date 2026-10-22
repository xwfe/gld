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
        changed_files_truncated: false,
        changes_unknown: false,
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

/// ask 档放行：起它。
pub fn approve(data_home: &Path, task_id: &str) -> Result<AgentTask, AgentError> {
    let (dir, task) = get(data_home, task_id)?;
    if task.state != State::WaitingLocalApproval {
        return Err(not_in_state(&task, "等本机放行"));
    }
    let profile = find_profile(data_home, &task.agent)?;
    let prompt = fs::read_to_string(dir.join(store::PROMPT_FILE)).map_err(|error| {
        AgentError::new(
            "AGENT_STATE_UNAVAILABLE",
            format!("读不了存下的提示：{error}"),
        )
    })?;
    launch(data_home, dir, task, profile, prompt)
}

fn launch(
    data_home: &Path,
    dir: PathBuf,
    mut task: AgentTask,
    profile: AgentProfile,
    prompt: String,
) -> Result<AgentTask, AgentError> {
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
        State::Running if run::request_stop(task_id) => Ok(task),
        State::Running => Err(AgentError::new(
            "AGENT_NOT_HERE",
            format!(
                "任务 {task_id} 是 pid {} 的 gld 进程起的，不在这个进程里，停不了；在起它的那个进程里停（一般是守护进程：gld agent stop）",
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
    let handoff = handoff_dir(Path::new(&task.workspace_root), task_id);
    if handoff.is_dir() {
        let _ = fs::write(
            handoff.join("review.md"),
            format!("# 审核：{verdict}\n\n{note}\n"),
        );
    }
    save(&dir, &task)?;
    Ok(task)
}

pub fn get(data_home: &Path, task_id: &str) -> Result<(PathBuf, AgentTask), AgentError> {
    let agents_root = store::root(data_home);
    let dir = store::find(&agents_root, task_id)
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
    let dir = handoff_dir(root, task_id);
    fs::create_dir_all(&dir).map_err(|error| format!("建不了 {}：{error}", dir.display()))?;
    let ignore = gld_dir.join(".gitignore");
    if !ignore.exists() {
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

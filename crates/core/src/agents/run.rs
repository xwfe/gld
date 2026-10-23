//! 把一个委派跑起来、盯着、收尾（RFC-0009 4.2）。
//!
//! 不复用 `exec_command` 的会话表：那边一条命令最长 1 小时、会话按调用方分、输出放内存，
//! 委派要 2 小时、要本机命令行也看得见、输出要整份留着给人查。这里直接起进程：
//!
//! - 自己一个进程组，停的时候停整组，Agent 起的子进程一个不留；
//! - 提示从 stdin 喂，喂完就关（不关的话有的 Agent 一直等输入）；
//! - stdout / stderr 直接写进任务目录的文件，"最后几行输出"从文件尾读；
//! - 一个监视线程：进程退出、被叫停、总时长到了、太久没输出，四种收尾。

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};

use super::profile::{AgentProfile, ResultFrom, StopSignal};
use super::store::{self, AgentTask, State};

/// recap 最多回多少字节。
pub const RECAP_MAX_BYTES: usize = 32 * 1024;
/// 改动文件最多列多少个。
pub const CHANGED_FILES_MAX: usize = 200;

/// 为什么停。监视线程收尾时按它记状态。
const REASON_NONE: u8 = 0;
const REASON_STOPPED: u8 = 1;
const REASON_INTERRUPTED: u8 = 2;

/// 这个进程里正在跑的委派：任务 id → 它在哪个项目、怎么叫停。
struct Live {
    project_root: PathBuf,
    reason: Arc<AtomicU8>,
}

static LIVE: LazyLock<Mutex<HashMap<String, Live>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn live() -> std::sync::MutexGuard<'static, HashMap<String, Live>> {
    LIVE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    canonical(a) == canonical(b)
}

/// 这个项目有没有委派在跑，有就是它的任务 id。gld 对这个项目的写和执行据此拦下（RFC-0009 第 5 节第 3 条）。
pub fn running_in(project_root: &Path) -> Option<String> {
    live()
        .iter()
        .find(|(_, entry)| same_dir(&entry.project_root, project_root))
        .map(|(id, _)| id.clone())
}

/// 这个进程里有没有在跑这个任务。
pub fn is_live(task_id: &str) -> bool {
    live().contains_key(task_id)
}

/// 先占住这个项目：同一个项目同时只许一个委派。占不住返回正在跑的那个任务 id。
pub fn reserve(task_id: &str, project_root: &Path) -> Result<(), String> {
    let mut live = live();
    if let Some((id, _)) = live
        .iter()
        .find(|(_, entry)| same_dir(&entry.project_root, project_root))
    {
        return Err(id.clone());
    }
    live.insert(
        task_id.to_string(),
        Live {
            project_root: project_root.to_path_buf(),
            reason: Arc::new(AtomicU8::new(REASON_NONE)),
        },
    );
    Ok(())
}

pub fn release(task_id: &str) {
    live().remove(task_id);
}

/// 叫停一个在跑的委派。不在本进程里跑的返回 false。
pub fn request_stop(task_id: &str) -> bool {
    match live().get(task_id) {
        Some(entry) => {
            entry.reason.store(REASON_STOPPED, Ordering::SeqCst);
            true
        }
        None => false,
    }
}

/// gld 正常退出：停掉所有在跑的委派、记成 `interrupted`，最多等 `wait`。
pub fn interrupt_all(wait: Duration) {
    for entry in live().values() {
        entry.reason.store(REASON_INTERRUPTED, Ordering::SeqCst);
    }
    let deadline = Instant::now() + wait;
    while !live().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// 起跑需要的东西。任务记录已经写好、项目已经 [`reserve`] 过。
pub struct Launch {
    pub task: AgentTask,
    pub task_dir: PathBuf,
    pub profile: AgentProfile,
    pub prompt: String,
    pub total_timeout: Duration,
}

/// 起进程、起监视线程。失败时任务记成 `failed`、放开项目，返回原因。
pub fn launch(launch: Launch) -> Result<AgentTask, String> {
    let Launch {
        mut task,
        task_dir,
        profile,
        prompt,
        total_timeout,
    } = launch;
    let reason = match live().get(&task.task_id) {
        Some(entry) => Arc::clone(&entry.reason),
        None => return Err("内部错误：任务没有先占住项目".into()),
    };
    let root = PathBuf::from(&task.workspace_root);
    let before = snapshot(&root, &task_dir, "before");
    let result_file = task_dir.join(store::RESULT_FILE);
    let _ = fs::remove_file(&result_file);
    let (argv, prompt_on_stdin) = profile.argv(
        super::profile::Choice {
            self_approval: task.agent_approval == "self",
            model: &task.model,
            effort: &task.effort,
        },
        &root,
        &result_file,
        &prompt,
    );
    if argv.is_empty() {
        // 调用方（agents::launch）已经查过；万一漏了也是起不来，不换成全放开的那条命令。
        task.state = State::Failed;
        task.failure = Some(format!(
            "档案 {} 没有 agent-approval={} 要用的命令",
            profile.name, task.agent_approval
        ));
        task.finished_at_ms = Some(now_ms());
        let _ = store::write(&task_dir, &task);
        release(&task.task_id);
        return Err(task.failure.clone().unwrap_or_default());
    }
    task.agent_version = agent_version(&argv[0], &task_dir);
    task.version_unverified = !profile.verified.is_empty()
        && !task
            .agent_version
            .as_deref()
            .is_some_and(|version| profile.verified.iter().any(|ok| version.contains(ok)));

    let spawned = spawn(
        &argv,
        &root,
        &task_dir,
        &profile,
        prompt_on_stdin.then_some(prompt.as_str()),
    );
    let child = match spawned {
        Ok(child) => child,
        Err(error) => {
            task.state = State::Failed;
            task.failure = Some(format!("起不来 {}：{error}", argv[0]));
            task.finished_at_ms = Some(now_ms());
            let _ = store::write(&task_dir, &task);
            release(&task.task_id);
            return Err(task.failure.clone().unwrap_or_default());
        }
    };
    task.pid = Some(child.id());
    task.state = State::Running;
    task.started_at_ms = Some(now_ms());
    task.owner_pid = Some(std::process::id());
    if let Err(error) = store::write(&task_dir, &task) {
        eprintln!("委派记录写不进 {}：{error}", task_dir.display());
    }
    let started = task.clone();
    std::thread::Builder::new()
        .name(format!("agent-{}", &task.task_id[..8]))
        .spawn(move || {
            watch(
                child,
                task,
                task_dir,
                profile,
                total_timeout,
                before,
                reason,
            )
        })
        .map_err(|error| format!("起不了监视线程：{error}"))?;
    Ok(started)
}

fn spawn(
    argv: &[String],
    root: &Path,
    task_dir: &Path,
    profile: &AgentProfile,
    stdin_text: Option<&str>,
) -> Result<Child, String> {
    #[cfg(not(unix))]
    {
        let _ = (argv, root, task_dir, profile, stdin_text);
        Err("Windows 上还不支持委派（RFC-0009 没验过 Windows 的进程组和信号）".into())
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        let open = |name: &str| {
            crate::tools::runs::create_private_file(&task_dir.join(name))
                .map_err(|error| format!("建不了 {name}：{error}"))
        };
        let mut command = Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(open(store::STDOUT_FILE)?)
            .stderr(open(store::STDERR_FILE)?)
            .process_group(0);
        // 和 exec_command 一个规矩：gld 的数据目录、名字像密钥的变量不给，档案点名要的除外
        // （有的 Agent 靠环境变量里的 key 登录）。
        command.env_remove(crate::home::HOME_ENV);
        for name in crate::tools::exec::withheld_env_names() {
            if !profile.env_pass.contains(&name) {
                command.env_remove(name);
            }
        }
        let mut child = command.spawn().map_err(|error| error.to_string())?;
        let mut stdin = child.stdin.take();
        let text = stdin_text.unwrap_or_default().to_string();
        // 另起线程写：提示很长、Agent 又不急着读的话，写满管道会卡住这里。
        std::thread::spawn(move || {
            if let Some(stdin) = stdin.as_mut() {
                let _ = stdin.write_all(text.as_bytes());
            }
            drop(stdin);
        });
        Ok(child)
    }
}

/// `<程序> --version` 的第一行，最多等 5 秒。拿不到就是 None，不挡着起跑。
///
/// 在任务目录里问：认不得 `--version` 的程序会把它当成普通参数照常干活，写出来的东西
/// 落在任务目录，不落在项目里或者 gld 的当前目录（测试的假脚本就这么干过）。
fn agent_version(program: &str, task_dir: &Path) -> Option<String> {
    let mut child = Command::new(program)
        .arg("--version")
        .current_dir(task_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let output = child.wait_with_output().ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

enum Ending {
    Exited(Option<i32>),
    Stopped(u8),
    TimedOut(String),
}

fn watch(
    mut child: Child,
    mut task: AgentTask,
    task_dir: PathBuf,
    profile: AgentProfile,
    total_timeout: Duration,
    before: Option<Snapshot>,
    reason: Arc<AtomicU8>,
) {
    let started = Instant::now();
    let idle_timeout = Duration::from_secs(profile.idle_timeout_secs.max(1));
    let mut last_size = 0u64;
    let mut last_output = Instant::now();
    let pgid = child.id() as i32;
    let ending = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ending::Exited(status.code()),
            Ok(None) => {}
            Err(error) => {
                task.failure = Some(format!("等进程时出错：{error}"));
                break Ending::Exited(None);
            }
        }
        let why = reason.load(Ordering::SeqCst);
        if why != REASON_NONE {
            stop_group(&mut child, pgid, profile.stop_signal);
            break Ending::Stopped(why);
        }
        if started.elapsed() >= total_timeout {
            stop_group(&mut child, pgid, profile.stop_signal);
            break Ending::TimedOut(format!(
                "总时长到了（{} 分钟）",
                total_timeout.as_secs() / 60
            ));
        }
        let size = output_size(&task_dir);
        if size != last_size {
            last_size = size;
            last_output = Instant::now();
        } else if last_output.elapsed() >= idle_timeout {
            stop_group(&mut child, pgid, profile.stop_signal);
            break Ending::TimedOut(format!(
                "{} 秒没有任何输出，按卡住处理",
                idle_timeout.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    // 正常退出也收一遍整组：全权跑的 Agent 留下的后台进程（dev server 之类）没人管就一直占着。
    #[cfg(unix)]
    signal(pgid, libc::SIGTERM);

    let root = PathBuf::from(&task.workspace_root);
    task.finished_at_ms = Some(now_ms());
    match ending {
        Ending::Exited(code) => {
            task.exit_code = code;
            match read_result(&profile, &task_dir) {
                Ok(recap) if code == Some(0) => {
                    task.recap = Some(recap);
                    task.state = State::AwaitingReview;
                }
                Ok(recap) => {
                    task.recap = Some(recap);
                    task.state = State::Failed;
                    task.failure
                        .get_or_insert_with(|| format!("退出码 {code:?}"));
                }
                Err(error) => {
                    task.state = State::Failed;
                    task.failure.get_or_insert(error);
                }
            }
        }
        Ending::Stopped(why) => {
            task.state = if why == REASON_INTERRUPTED {
                State::Interrupted
            } else {
                State::Stopped
            };
            task.recap = read_result(&profile, &task_dir).ok();
        }
        Ending::TimedOut(why) => {
            task.state = State::Timeout;
            task.failure = Some(why);
            task.recap = read_result(&profile, &task_dir).ok();
        }
    }
    let after = snapshot(&root, &task_dir, "after");
    task.before_tree = before.as_ref().map(|snapshot| snapshot.tree.clone());
    task.after_tree = after.as_ref().map(|snapshot| snapshot.tree.clone());
    match (&before, &after) {
        (Some(before), Some(after)) => match task_diff(&root, &task_dir, before, after) {
            Some((mut stats, bytes)) => {
                task.changed_files_truncated = stats.len() > CHANGED_FILES_MAX;
                stats.truncate(CHANGED_FILES_MAX);
                task.changed_files = stats.iter().map(|stat| stat.path.clone()).collect();
                task.file_stats = stats;
                task.diff_bytes = Some(bytes);
                // Agent 被叮嘱了别提交；提交了也照样算进 diff（树比的是工作区），只是要让审核的人知道。
                task.head_moved = before.head != after.head;
            }
            None => task.changes_unknown = true,
        },
        _ => task.changes_unknown = true,
    }
    if let Err(error) = store::write(&task_dir, &task) {
        eprintln!("委派记录写不进 {}：{error}", task_dir.display());
    }
    release(&task.task_id);
}

fn output_size(task_dir: &Path) -> u64 {
    [store::STDOUT_FILE, store::STDERR_FILE]
        .iter()
        .filter_map(|name| fs::metadata(task_dir.join(name)).ok())
        .map(|meta| meta.len())
        .sum()
}

/// 先发档案的信号等 5 秒，再 TERM 等 3 秒，最后 KILL。停的是整个进程组。
fn stop_group(child: &mut Child, pgid: i32, first: StopSignal) {
    #[cfg(unix)]
    {
        let first = match first {
            StopSignal::Int => libc::SIGINT,
            StopSignal::Term => libc::SIGTERM,
        };
        for (sig, wait) in [
            (first, Duration::from_secs(5)),
            (libc::SIGTERM, Duration::from_secs(3)),
        ] {
            signal(pgid, sig);
            let deadline = Instant::now() + wait;
            while Instant::now() < deadline {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        signal(pgid, libc::SIGKILL);
    }
    #[cfg(not(unix))]
    {
        let _ = (pgid, first);
        let _ = child.kill();
    }
    let _ = child.wait();
}

#[cfg(unix)]
fn signal(pgid: i32, sig: i32) {
    // SAFETY: kill 只是发信号；负的 pid 表示整个进程组，组已经没了会回 ESRCH，忽略。
    unsafe {
        libc::kill(-pgid, sig);
    }
}

/// 按档案取 Agent 的最后一条消息，脱敏、截到 [`RECAP_MAX_BYTES`]。
fn read_result(profile: &AgentProfile, task_dir: &Path) -> Result<String, String> {
    let text = match profile.result.from {
        ResultFrom::File => fs::read_to_string(task_dir.join(store::RESULT_FILE))
            .map_err(|_| "Agent 没写出结果文件".to_string())?,
        ResultFrom::Stdout => fs::read_to_string(task_dir.join(store::STDOUT_FILE))
            .map_err(|error| format!("读不了输出：{error}"))?,
        ResultFrom::Json => {
            let stdout = fs::read_to_string(task_dir.join(store::STDOUT_FILE))
                .map_err(|error| format!("读不了输出：{error}"))?;
            json_field(&stdout, &profile.result.field).ok_or_else(|| {
                format!(
                    "输出里找不到 JSON 字段 {}（输出前 2 KB：{}）",
                    profile.result.field,
                    stdout.chars().take(2048).collect::<String>()
                )
            })?
        }
    };
    let mut text = text.trim().to_string();
    if text.len() > RECAP_MAX_BYTES {
        let mut end = RECAP_MAX_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n…[截断，全文见任务目录]");
    }
    crate::tools::history::redact_text(&mut text);
    Ok(text)
}

/// 整个输出是一个 JSON 对象就从它取；不是的话从最后一行往前找第一个能解析的。
fn json_field(stdout: &str, field: &str) -> Option<String> {
    let parsed = serde_json::from_str::<serde_json::Value>(stdout.trim())
        .ok()
        .or_else(|| {
            stdout
                .lines()
                .rev()
                .find_map(|line| serde_json::from_str::<serde_json::Value>(line.trim()).ok())
        })?;
    match parsed.get(field)? {
        serde_json::Value::String(text) => Some(text.clone()),
        other => Some(other.to_string()),
    }
}

/// 项目某一刻的样子：整个工作区写成的一棵 git 树，加上 HEAD。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Snapshot {
    tree: String,
    head: Option<String>,
}

fn git_with(root: &Path, index: Option<&Path>, args: &[&str]) -> Option<Vec<u8>> {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    let output = command.output().ok()?;
    output.status.success().then_some(output.stdout)
}

fn git(root: &Path, args: &[&str]) -> Option<Vec<u8>> {
    git_with(root, None, args)
}

/// 把工作区整个写成一棵树。不是 git 仓库（或者没装 git）就是 None，改动记成"不知道"，不瞎报。
///
/// 用任务目录里的临时 index（`GIT_INDEX_FILE`）`git add -A` 再 `write-tree`：**不碰用户自己的 index 和工作区**；
/// 没提交的、新建的文件都在树里，`.gitignore` 忽略的不在。跑前跑后两棵树一比，就是这次任务自己改了什么——
/// 以前跟 HEAD 比，上一轮委派没提交的改动会混进来，审核的人分不清（审查 §44）。
///
/// 临时 index 先从用户的 index 复制一份：`git add -A` 靠里面记的文件状态跳过没变的文件，大仓库也不用整个重读。
/// 代价是往仓库的对象库里写了 blob 和树，没人引用，过期后 `git gc` 会清掉。
pub(crate) fn snapshot(root: &Path, task_dir: &Path, label: &str) -> Option<Snapshot> {
    let index = task_dir.join(format!("{label}.index"));
    let _ = fs::remove_file(&index);
    if let Some(path) = git(root, &["rev-parse", "--git-path", "index"]) {
        let path = PathBuf::from(String::from_utf8_lossy(&path).trim());
        let _ = fs::copy(root.join(path), &index);
    }
    // --ignore-errors：有个文件读不了（权限之类）不该让整棵树都记不下来。
    let tree = git_with(
        root,
        Some(&index),
        &["add", "-A", "--ignore-errors", "--", "."],
    )
    .and_then(|_| git_with(root, Some(&index), &["write-tree"]));
    let _ = fs::remove_file(&index);
    let tree = String::from_utf8_lossy(&tree?).trim().to_string();
    let head = git(root, &["rev-parse", "--verify", "-q", "HEAD"])
        .map(|out| String::from_utf8_lossy(&out).trim().to_string());
    Some(Snapshot { tree, head })
}

/// 两棵树之间改了哪些文件、各增删几行，并把完整 diff 写进任务目录的 [`store::DIFF_FILE`]。
/// 返回每个文件的行数和 diff 字节数。
///
/// 不跑仓库配置的外部 diff 和 textconv：那是项目里配的程序，审核时不该替它执行。
/// `--numstat` 要读文件内容才数得出行，所以它也得带 `--no-textconv`。
fn task_diff(
    root: &Path,
    task_dir: &Path,
    before: &Snapshot,
    after: &Snapshot,
) -> Option<(Vec<store::FileStat>, u64)> {
    let numstat = git(
        root,
        &[
            "diff",
            "--numstat",
            "-z",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
            &before.tree,
            &after.tree,
        ],
    )?;
    let mut stats = parse_numstat(&numstat);
    let patch = git(
        root,
        &[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            &before.tree,
            &after.tree,
        ],
    )?;
    // 两次 git diff 比的是同样两棵树、同样不认改名，文件一样、顺序一样。对不上就不记位置，不能把一个文件的段标到另一个头上。
    let sections = file_sections(&patch);
    if sections.len() == stats.len() {
        for (stat, (offset, bytes)) in stats.iter_mut().zip(sections) {
            stat.diff_offset = Some(offset);
            stat.diff_bytes = Some(bytes);
        }
    } else {
        eprintln!(
            "任务 diff 有 {} 段、numstat 有 {} 个文件，对不上，不记每个文件的位置",
            sections.len(),
            stats.len()
        );
    }
    let written = crate::tools::runs::create_private_file(&task_dir.join(store::DIFF_FILE))
        .and_then(|mut file| file.write_all(&patch));
    if let Err(error) = written {
        eprintln!("任务 diff 写不进 {}：{error}", task_dir.display());
    }
    Some((stats, patch.len() as u64))
}

/// `git diff --numstat -z --no-renames` 的输出：每个文件一条 `增\t删\t路径\0`，二进制文件的增删是 `-`。
/// 不带 `--no-renames` 的话改名会变成另一种格式（路径分两段），这里不认。
pub(crate) fn parse_numstat(raw: &[u8]) -> Vec<store::FileStat> {
    raw.split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .filter_map(|record| {
            let mut fields = record.splitn(3, |byte| *byte == b'\t');
            let count = |field: &[u8]| std::str::from_utf8(field).ok()?.parse::<u64>().ok();
            let added = fields.next()?;
            let deleted = fields.next()?;
            let path = fields.next().filter(|path| !path.is_empty())?;
            Some(store::FileStat {
                path: String::from_utf8_lossy(path).into_owned(),
                added: count(added),
                deleted: count(deleted),
                diff_offset: None,
                diff_bytes: None,
            })
        })
        .collect()
}

/// 完整 diff 里每个文件那一段从第几个字节起、多长，按出现的顺序。
///
/// 每段从行首的 `diff --git ` 起。diff 的内容行都带 ` `、`+`、`-`、`\` 前缀，路径里的换行 git 会转义，
/// 所以行首出现这几个字就是下一个文件的开头。改模式、二进制、空文件也各有一段，和 numstat 一一对应。
pub(crate) fn file_sections(patch: &[u8]) -> Vec<(u64, u64)> {
    const HEADER: &[u8] = b"diff --git ";
    let starts: Vec<usize> = std::iter::once(0)
        .chain(
            patch
                .iter()
                .enumerate()
                .filter(|(_, byte)| **byte == b'\n')
                .map(|(at, _)| at + 1),
        )
        .filter(|&start| patch[start..].starts_with(HEADER))
        .collect();
    starts
        .iter()
        .enumerate()
        .map(|(n, &start)| {
            let end = starts.get(n + 1).copied().unwrap_or(patch.len());
            (start as u64, (end - start) as u64)
        })
        .collect()
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// 测试里等监视线程收尾用。
#[cfg(test)]
pub fn wait_until_finished(task_dir: &Path, wait: Duration) -> AgentTask {
    let deadline = Instant::now() + wait;
    loop {
        let task = store::read(task_dir).expect("任务记录");
        if task.state != State::Running || Instant::now() >= deadline {
            return task;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

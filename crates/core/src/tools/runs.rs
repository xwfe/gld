//! 命令的运行记录：`exec_command` 起的每条命令，在数据目录里留一份记录和输出日志（审查 D09）。
//!
//! 为什么要有：命令会话只在内存里。结束 5 分钟后输出就没了；守护进程一重启，连"这条命令
//! 最后怎样了"都查不到；直连模式下命令行一退，下一次调用什么也读不到。任务验收要的终态也
//! 跟着丢：后台命令结束前 gld 重启了，这条证据就永远是"没结束"。
//!
//! # 放在哪、留多少
//!
//! `GLD_HOME/runs/<项目 id>/<session_id>/`，项目 id 和任务数据（`harness/`）同一个算法：
//!
//! - `run.json`：谁起的、命令（脱敏，最多 500 字符）、pid、起止时间、终态、退出码、
//!   起跑时的落盘计数器和运行期间 gld 往工作区写过几次。起命令时写一次，命令结束那一刻再写一次。
//! - `stdout.<起点>.log` / `stderr.<起点>.log`：原样的输出。按 [`SEGMENT_BYTES`] 分段，只留
//!   最新两段：至少最后 1 MiB、最多 2 MiB。文件名里的数字是这一段在整条流里从第几个字节
//!   开始，所以重启之后不靠别的记录也接得上 `read_output` 的偏移。
//!
//! 每个项目留最近 [`MAX_FINISHED_RUNS`] 条已经结束的、最长 [`MAX_AGE`]；还在跑的一条不动。
//! 起新命令时顺手清这个项目；每个进程第一次拿到 owner 锁时把所有项目扫一遍（不再用、已经删掉的
//! 项目也得有人清），不另起定时器。最坏一个项目占 64 × 2 流 × 2 MiB = 256 MiB。
//!
//! # 重启以后
//!
//! - gld 正常退出时自己停掉的命令记 `interrupted`
//!   （[`crate::tools::workspace_runtime::terminate_all_sessions_everywhere`]）。
//! - gld 没来得及记的（被 `kill -9`、崩溃、断电），记录停在 `running`。谁读到它、发现起它
//!   的那个 gld 进程已经不在了，就改成 `unknown`：命令可能跑完了、失败了，也可能还作为孤儿
//!   在跑。**不猜、不重连、不替它收尾**，也不去杀那个 pid——它可能已经分给了别的进程。
//!
//! 怎么知道起它的 gld 还在：每个写记录的 gld 进程在 `runs/owners/<实例 id>.lock` 上一直
//! 拿着独占锁，进程没了内核就放锁。别人拿得到这把锁，就说明那个进程不在了。不用 pid 判断：
//! pid 会被复用，拿 pid 判活会把一个无关进程当成 gld。拿不到自己的锁的进程不写记录——
//! 否则别人会把它还在跑的命令判成 `unknown`。
//!
//! # 管不到什么
//!
//! - 不恢复原进程、不重放命令、不承诺恰好一次。
//! - 只数 gld 自己往工作区写过几次；编辑器、`git checkout` 这些 gld 以外的改动不在这里，
//!   任务验收那边按文件指纹查。换了进程（gld 重启过）连 gld 自己写没写过都算不出来，所以
//!   重启前起的命令读得到结局，但当不了任务验收证据（见 `SessionStore::writes_since_start`）。
//! - 日志是原样的命令输出，**不脱敏**：命令打印了密钥，密钥就在盘上留到被清掉。目录 0700、
//!   文件 0600，和凭据在同一个数据目录。
//! - 盘写满、目录建不出来时这条命令照常跑，只是不留记录（结果里 `kept_on_disk: false`）。

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

/// 日志一段多大。只留最新两段，所以盘上每个流在 1～2 MiB 之间，和内存里留最后 1 MiB 对得上。
pub const SEGMENT_BYTES: u64 = 1 << 20;

/// 留几段。
const KEEP_SEGMENTS: usize = 2;

/// 每个项目最多留几条已经结束的记录。
pub const MAX_FINISHED_RUNS: usize = 64;

/// 结束之后最多留多久。输出不脱敏，别让它在盘上一直待着。
pub const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// 命令原文最多留多少字符，和任务事件里的一致。
const COMMAND_TEXT_CHARS: usize = 500;

const RECORD: &str = "run.json";
const OWNERS: &str = "owners";

/// 一条命令的运行记录，`run.json` 的内容。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub session_id: String,
    /// 起它的主体（[`crate::tools::caller::Caller::key`]）。读的人对不上就当没有这条，
    /// 和内存里的会话表按主体分开是同一个规矩。
    pub caller: String,
    /// 命令原文，已脱敏，最多 500 字符。
    pub command: String,
    pub pid: Option<u32>,
    /// 起它的那个 gld 进程：实例 id（判活用）和 pid（给人看）。
    pub owner: String,
    pub owner_pid: u32,
    pub started_at_ms: u64,
    /// running / exited / timeout / killed / interrupted / unknown
    pub status: String,
    pub exit_code: Option<i32>,
    /// 结束的时刻；`unknown` 是被发现的时刻。
    pub finished_at_ms: Option<u64>,
    /// 运行期间（起跑到结束）gld 往工作区写过几次，给人看。结束时才知道；`unknown` 的没有。
    ///
    /// **不能拿它判断结果还作不作数**：它在结束那一刻就定死了，之后 gld 再改文件它不知道。
    /// 回包里的 `workspace_writes_since_start` 按 [`RunRecord::writes_at_start`] 现算。
    pub workspace_writes_during_run: Option<u64>,
    /// 起跑时这个项目的落盘计数器是多少。计数器只在内存里，所以只有起它的那个进程拿它
    /// 现算得出"起跑到现在写过几次"；换了进程（gld 重启过）就算不出来。
    #[serde(default)]
    pub writes_at_start: Option<u64>,
    /// 日志没写全的流（写盘失败，比如盘满了，之后这个流就不再落盘）。从记录读这个流，
    /// 缺的是后面那部分。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub incomplete_logs: Vec<String>,
}

impl RunRecord {
    pub fn is_running(&self) -> bool {
        self.status == "running"
    }
}

/// 一个项目的运行记录，从某个主体的角度看过去。
#[derive(Debug, Clone)]
pub struct RunLog {
    /// `GLD_HOME/runs`
    root: PathBuf,
    /// `GLD_HOME/runs/<项目 id>`
    dir: PathBuf,
    caller: String,
}

impl RunLog {
    /// `root` 是 `GLD_HOME/runs`，`project` 是项目 id。
    pub fn new(root: PathBuf, project: &str, caller: &str) -> Self {
        Self {
            dir: root.join(project),
            root,
            caller: caller.to_string(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 给一条刚起来的命令建记录。建不出来就返回 `None`，命令照常跑、只是不留记录。
    ///
    /// `writes_at_start` 是起跑时这个项目的落盘计数器，见 [`RunRecord::writes_at_start`]。
    pub fn start(
        &self,
        session_id: &str,
        command: &str,
        pid: Option<u32>,
        writes_at_start: u64,
    ) -> Option<RunWriter> {
        if !valid_id(session_id) {
            return None;
        }
        let owner = register_owner(&self.root)?;
        self.sweep();
        let dir = self.dir.join(session_id);
        let mut text: String = command.chars().take(COMMAND_TEXT_CHARS).collect();
        crate::tools::history::redact_text(&mut text);
        let record = RunRecord {
            session_id: session_id.to_string(),
            caller: self.caller.clone(),
            command: text,
            pid,
            owner,
            owner_pid: std::process::id(),
            started_at_ms: now_ms(),
            status: "running".into(),
            exit_code: None,
            finished_at_ms: None,
            workspace_writes_during_run: None,
            writes_at_start: Some(writes_at_start),
            incomplete_logs: Vec::new(),
        };
        let opened = (|| -> std::io::Result<RunWriter> {
            create_private_dir(&dir)?;
            write_record(&dir, &record)?;
            Ok(RunWriter {
                stdout: Mutex::new(Some(StreamLog::create(&dir, "stdout")?)),
                stderr: Mutex::new(Some(StreamLog::create(&dir, "stderr")?)),
                record: Mutex::new(record),
                dir: dir.clone(),
            })
        })();
        match opened {
            Ok(writer) => Some(writer),
            Err(error) => {
                eprintln!(
                    "建不了运行记录 {}：{error}。这条命令照常跑，只是不留记录",
                    dir.display()
                );
                let _ = fs::remove_dir_all(&dir);
                None
            }
        }
    }

    /// 读一条记录。不是这个主体的、id 不像 session_id 的、没有的，一律 `None`。
    ///
    /// 记录停在 `running`、起它的 gld 进程却已经不在了：改成 `unknown` 再返回（并写回去，
    /// 下一个读的人看到的是同一个结论）。
    pub fn load(&self, session_id: &str) -> Option<RunRecord> {
        if !valid_id(session_id) {
            return None;
        }
        let dir = self.dir.join(session_id);
        let record = read_record(&dir)?;
        if record.caller != self.caller {
            return None;
        }
        Some(settle(&self.root, &dir, record))
    }

    /// 同 [`RunLog::load`]，但调用方已经确认这条命令不在本进程的内存里。
    ///
    /// 那么本进程起的、记录还停在 `running` 的，也不是真在跑：结束时写记录失败了，或者
    /// 会话表被扔掉了。按 `unknown` 处理，别告诉读的人"另一个进程还在跑它"——那个进程就是自己。
    pub fn load_detached(&self, session_id: &str) -> Option<RunRecord> {
        let record = self.load(session_id)?;
        if record.is_running() && record.owner == INSTANCE.as_str() {
            return Some(mark_unknown(&self.dir.join(session_id), record));
        }
        Some(record)
    }

    /// 一条记录留在盘上的某个流：字节，和它在整条流里一共有多少字节。
    ///
    /// 收 [`RunRecord`] 而不是 id：记录只能从 [`RunLog::load`] 来，主体已经核对过了。
    /// 和 [`crate::tools::session::ExecSession::retained_stream_bytes`] 同一个口径：返回的是
    /// 整条流的最后一段，`total - data.len()` 是这一段在流里的起点。
    pub fn stream_bytes(&self, record: &RunRecord, stream: &str) -> (Vec<u8>, usize) {
        read_stream(&self.dir.join(&record.session_id), stream)
    }

    /// 这个主体在这个项目里的全部记录，按起跑时间从新到旧（同一毫秒按 id）。给 `list_runs` 用。
    ///
    /// 别的主体的、`run.json` 读不出来的、结束超过 [`MAX_AGE`] 还没被清掉的，都不在里面：
    /// 读不出来的分不清是谁的，报个数就等于告诉别人"这里有东西"。
    ///
    /// 起它的 gld 进程不在了的 `running` 照 [`RunLog::load`] 改成 `unknown`。**本进程起的不动**，
    /// 哪怕内存里没有：[`crate::tools::session::SessionStore::insert`] 先建记录、后放进内存表，
    /// 列表正好夹在中间的话，按 `load_detached` 的规矩会把一条刚起的命令写成 `unknown`。
    pub fn list(&self) -> Vec<RunRecord> {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let now = now_ms();
        let max_age = MAX_AGE.as_millis() as u64;
        let mut records: Vec<RunRecord> = entries
            .flatten()
            .filter(|entry| entry.file_name().to_str().is_some_and(valid_id))
            .filter_map(|entry| {
                let dir = entry.path();
                let record = read_record(&dir)?;
                (record.caller == self.caller).then(|| settle(&self.root, &dir, record))
            })
            .filter(|record| {
                record.is_running()
                    || now.saturating_sub(record.finished_at_ms.unwrap_or(record.started_at_ms))
                        < max_age
            })
            .collect();
        records.sort_by(|a, b| {
            (b.started_at_ms, &b.session_id).cmp(&(a.started_at_ms, &a.session_id))
        });
        records
    }

    /// 一条记录的某个流一共写过多少字节（不是盘上留着的那么多）。只看文件名和大小，不读内容，
    /// 列表一次几十条也不用把几十 MiB 日志读一遍。
    pub fn stream_total(&self, record: &RunRecord, stream: &str) -> u64 {
        segments(&self.dir.join(&record.session_id), stream)
            .last()
            .map_or(0, |(start, path)| {
                start + fs::metadata(path).map_or(0, |meta| meta.len())
            })
    }

    /// 清掉这个项目超出配额和太旧的记录。
    fn sweep(&self) {
        sweep_project(&self.root, &self.dir);
    }
}

/// 记录停在 `running`、起它的 gld 进程却已经不在了：改成 `unknown`（并写回去，下一个读的人
/// 看到的是同一个结论）。
fn settle(root: &Path, dir: &Path, record: RunRecord) -> RunRecord {
    if record.is_running() && !owner_alive(root, &record.owner) {
        return mark_unknown(dir, record);
    }
    record
}

fn mark_unknown(dir: &Path, mut record: RunRecord) -> RunRecord {
    record.status = "unknown".into();
    record.finished_at_ms = Some(now_ms());
    let _ = write_record(dir, &record);
    record
}

/// 清掉 `runs/` 下所有项目里超出配额和太旧的记录，空了的项目目录一并删掉。
///
/// 进程第一次拿到 owner 锁时跑一次。只靠"同一个项目起新命令时顺手清"的话，不再用的、
/// 已经删掉的项目的记录永远没人清——而那是原样的命令输出。
fn sweep_all(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_name() == OWNERS || !path.is_dir() {
            continue;
        }
        sweep_project(root, &path);
        // 不空就删不掉，正好。
        let _ = fs::remove_dir(&path);
    }
}

/// 清掉一个项目超出配额和太旧的记录。所有主体的都算在这个项目的配额里；还在跑的不动。
fn sweep_project(root: &Path, project: &Path) {
    let Ok(entries) = fs::read_dir(project) else {
        return;
    };
    let now = now_ms();
    let max_age = MAX_AGE.as_millis() as u64;
    let mut finished: Vec<(u64, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !entry.file_name().to_str().is_some_and(valid_id) || !path.is_dir() {
            continue;
        }
        match read_record(&path) {
            Some(record) => {
                let record = settle(root, &path, record);
                if !record.is_running() {
                    let at = record.finished_at_ms.unwrap_or(record.started_at_ms);
                    finished.push((at, path));
                }
            }
            // 没有 run.json（建到一半被杀）或者读不出来：看目录多久没动过。
            None => {
                let idle = entry
                    .metadata()
                    .and_then(|meta| meta.modified())
                    .ok()
                    .and_then(|at| SystemTime::now().duration_since(at).ok());
                if idle.is_some_and(|idle| idle >= MAX_AGE) {
                    let _ = fs::remove_dir_all(&path);
                }
            }
        }
    }
    // 结束得越晚越靠前。
    finished.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    for (index, (at, path)) in finished.into_iter().enumerate() {
        if index >= MAX_FINISHED_RUNS || now.saturating_sub(at) >= max_age {
            let _ = fs::remove_dir_all(path);
        }
    }
}

/// 这个进程的实例 id：记录里的 `owner` 等于它，就是本进程起的。
pub fn instance_id() -> &'static str {
    INSTANCE.as_str()
}

/// 一条正在跑的命令往盘上写记录和日志。命令会话 [`crate::tools::session::ExecSession`] 拿着它。
pub struct RunWriter {
    dir: PathBuf,
    record: Mutex<RunRecord>,
    stdout: Mutex<Option<StreamLog>>,
    stderr: Mutex<Option<StreamLog>>,
}

impl RunWriter {
    /// 追加一段输出。写失败（盘满之类）就停掉这个流的日志，不影响命令本身；
    /// 结束时记进 `incomplete_logs`，读的人才知道后面缺了。
    pub fn append(&self, is_stdout: bool, bytes: &[u8]) {
        let (slot, name) = if is_stdout {
            (&self.stdout, "stdout")
        } else {
            (&self.stderr, "stderr")
        };
        let mut slot = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(log) = slot.as_mut() {
            if let Err(error) = log.append(bytes) {
                eprintln!(
                    "写不进命令日志 {}：{error}。这个流后面的输出不再落盘",
                    self.dir.display()
                );
                *slot = None;
                let mut record = self
                    .record
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                record.incomplete_logs.push(name.to_string());
                // 进程已经结束、读输出的任务还在收尾时才写坏的：结局已经落过盘了，再写一次，
                // 不然读的人看不到"这个流不全"。
                if !record.is_running() {
                    let _ = write_record(&self.dir, &record);
                }
            }
        }
    }

    /// 命令结束：记下终态。只在结束那一刻调一次。
    pub fn finish(&self, status: &str, exit_code: Option<i32>, writes_during_run: u64) {
        let mut record = self
            .record
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        record.status = status.to_string();
        record.exit_code = exit_code;
        record.finished_at_ms = Some(now_ms());
        record.workspace_writes_during_run = Some(writes_during_run);
        if let Err(error) = write_record(&self.dir, &record) {
            eprintln!(
                "记不下命令的结局 {}：{error}。重启后它会被当成 unknown",
                self.dir.display()
            );
        }
    }
}

/// 一个流的日志：当前在写的那一段。
struct StreamLog {
    dir: PathBuf,
    name: &'static str,
    /// 这一段在整条流里的起点。
    start: u64,
    len: u64,
    file: File,
}

impl StreamLog {
    fn create(dir: &Path, name: &'static str) -> std::io::Result<Self> {
        Ok(Self {
            file: create_private_file(&segment_path(dir, name, 0))?,
            dir: dir.to_path_buf(),
            name,
            start: 0,
            len: 0,
        })
    }

    fn append(&mut self, mut bytes: &[u8]) -> std::io::Result<()> {
        while !bytes.is_empty() {
            if self.len >= SEGMENT_BYTES {
                self.rotate()?;
            }
            let room = (SEGMENT_BYTES - self.len) as usize;
            let take = room.min(bytes.len());
            self.file.write_all(&bytes[..take])?;
            self.len += take as u64;
            bytes = &bytes[take..];
        }
        Ok(())
    }

    /// 换一段新的，删掉最旧的。先建新的再删旧的：读的人任何时候都看得到最后那部分。
    fn rotate(&mut self) -> std::io::Result<()> {
        let start = self.start + self.len;
        self.file = create_private_file(&segment_path(&self.dir, self.name, start))?;
        self.start = start;
        self.len = 0;
        let segments = segments(&self.dir, self.name);
        let extra = segments.len().saturating_sub(KEEP_SEGMENTS);
        for (_, path) in segments.into_iter().take(extra) {
            let _ = fs::remove_file(path);
        }
        Ok(())
    }
}

fn segment_path(dir: &Path, name: &str, start: u64) -> PathBuf {
    dir.join(format!("{name}.{start}.log"))
}

/// 一个流在盘上的各段：(起点, 路径)，按起点从小到大。
fn segments(dir: &Path, name: &str) -> Vec<(u64, PathBuf)> {
    let prefix = format!("{name}.");
    let mut found: Vec<(u64, PathBuf)> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let file_name = entry.file_name();
            let start = file_name
                .to_str()?
                .strip_prefix(&prefix)?
                .strip_suffix(".log")?
                .parse()
                .ok()?;
            Some((start, entry.path()))
        })
        .collect();
    found.sort_by_key(|(start, _)| *start);
    found
}

fn read_stream(dir: &Path, name: &str) -> (Vec<u8>, usize) {
    let mut data: Vec<u8> = Vec::new();
    let mut from = 0u64;
    for (start, path) in segments(dir, name) {
        let mut bytes = Vec::new();
        // 读的时候正好被写的那边删掉（换段），就从下一段接着读。
        if File::open(&path)
            .and_then(|mut file| file.read_to_end(&mut bytes))
            .is_err()
        {
            continue;
        }
        // 接不上（中间那段没读到）就只留后面的：偏移必须和字节对得上。
        if data.is_empty() || from + data.len() as u64 != start {
            data.clear();
            from = start;
        }
        data.extend_from_slice(&bytes);
    }
    let total = from as usize + data.len();
    (data, total)
}

/// 看着像 `exec_command` 给的 session_id（UUID）。`read_output` 的 `output_ref` 是调用方
/// 传进来的，不查的话 `session:../../x:stdout` 就拼成了数据目录外的路径。
fn valid_id(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok() && !id.contains(['/', '\\', '.'])
}

fn read_record(dir: &Path) -> Option<RunRecord> {
    let bytes = fs::read(dir.join(RECORD)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// 先写临时文件再改名换上去：读的人看到的要么是旧的一整份，要么是新的一整份。
///
/// 不 `sync`：它是事后查看用的记录，不是任务那样的账。断电后这一份坏了只会读成"没有这条"，
/// 而每条命令多两次落盘等待不值得。
fn write_record(dir: &Path, record: &RunRecord) -> std::io::Result<()> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let bytes = serde_json::to_vec_pretty(record).map_err(std::io::Error::other)?;
    let temp = dir.join(format!(
        "{RECORD}.tmp.{}.{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let written = create_private_file(&temp)
        .and_then(|mut file| file.write_all(&bytes))
        .and_then(|()| fs::rename(&temp, dir.join(RECORD)));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn create_private_dir(path: &Path) -> std::io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

fn create_private_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// 这个进程的实例 id。一个进程一个，跟 pid 无关。
static INSTANCE: LazyLock<String> = LazyLock::new(|| uuid::Uuid::new_v4().simple().to_string());

/// 这个进程在各个 `runs` 目录下拿着的锁。拿着 `File` 就一直锁着，进程退出时内核放锁。
///
/// 按目录分：测试里同一个进程会用好几个数据目录。`None` 表示那个目录下拿不到锁，
/// 这个进程在那里就不写记录。
static OWNED: LazyLock<Mutex<HashMap<PathBuf, Option<File>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 在 `root/owners/` 下拿着自己的锁，返回实例 id；拿不到返回 `None`。
fn register_owner(root: &Path) -> Option<String> {
    let mut owned = OWNED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let held = owned.entry(root.to_path_buf()).or_insert_with(|| {
        let dir = root.join(OWNERS);
        // 先在临时名字上拿到锁，再改成正式名字：别人只要看得到正式名字，锁就一定已经拿着了。
        // 反过来先建正式名字再加锁的话，中间那一瞬间别的进程清理"已经不在的进程的锁"会把它
        // 当成死锁删掉，之后这个进程起的命令都会被判成 unknown。锁跟着文件走，改名不影响。
        let staged = dir.join(format!("{}.lock.tmp", *INSTANCE));
        let taken = create_private_dir(&dir)
            .and_then(|()| create_private_file(&staged))
            .and_then(|file| file.try_lock_exclusive().map(|()| file))
            .and_then(|file| {
                fs::rename(&staged, dir.join(format!("{}.lock", *INSTANCE))).map(|()| file)
            });
        match taken {
            Ok(file) => {
                clear_dead_owners(&dir);
                sweep_all(root);
                Some(file)
            }
            Err(error) => {
                eprintln!(
                    "拿不到运行记录的进程锁 {}：{error}。这个进程起的命令不留记录",
                    dir.display()
                );
                None
            }
        }
    });
    held.is_some().then(|| INSTANCE.clone())
}

/// 起它的那个 gld 进程还在不在。拿不准（锁文件打不开之类）时算"还在"：
/// 宁可一条记录多挂一会儿 running，也不要把活着的命令说成 unknown。
fn owner_alive(root: &Path, owner: &str) -> bool {
    if owner == INSTANCE.as_str() {
        return true;
    }
    if owner.contains(['/', '\\', '.']) {
        return false;
    }
    let path = root.join(OWNERS).join(format!("{owner}.lock"));
    let file = match OpenOptions::new().read(true).write(true).open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return false,
        Err(_) => return true,
    };
    match file.try_lock_exclusive() {
        Ok(()) => {
            let _ = FileExt::unlock(&file);
            let _ = fs::remove_file(&path);
            false
        }
        Err(_) => true,
    }
}

/// 删掉已经不在的进程留下的锁文件。直连模式下每次 `gld tool call` 都是一个新进程，不清的话越攒越多。
fn clear_dead_owners(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let root = dir.parent().unwrap_or(dir);
    for entry in entries.flatten() {
        if let Some(owner) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.strip_suffix(".lock"))
        {
            owner_alive(root, owner);
        }
    }
}

/// 项目 id：和任务数据同一个算法，同一个项目在 `harness/` 和 `runs/` 下是同一个名字。
pub fn project_id(workspace_root: &Path) -> String {
    crate::harness::state::workspace_id(workspace_root)
}

/// 测试用：假装记录是另一个已经不在的 gld 进程起的。
#[cfg(test)]
pub(crate) fn orphan_record(log: &RunLog, session_id: &str) {
    let dir = log.dir.join(session_id);
    let mut record = read_record(&dir).expect("记录");
    record.owner = "0000000000000000000000000000dead".into();
    write_record(&dir, &record).expect("写回");
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn id() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    /// 输出超过两段时只留最新两段，偏移和整条流对得上：读出来的就是流的最后那部分。
    #[test]
    fn a_long_stream_keeps_its_last_two_segments_with_true_offsets() {
        let home = tempdir().expect("home");
        let log = RunLog::new(home.path().join("runs"), "p", "local");
        let session = id();
        let writer = log.start(&session, "flood", None, 0).expect("建记录");
        // 3.5 段：每个字节是它在流里位置的低 8 位，读出来的每个字节都能对回位置。
        let total = SEGMENT_BYTES as usize * 7 / 2;
        let bytes: Vec<u8> = (0..total).map(|at| at as u8).collect();
        for chunk in bytes.chunks(4096) {
            writer.append(true, chunk);
        }
        let record = log.load(&session).expect("记录");
        let (data, seen_total) = log.stream_bytes(&record, "stdout");
        assert_eq!(seen_total, total);
        assert_eq!(
            segments(log.dir().join(&session).as_path(), "stdout").len(),
            2
        );
        assert!(data.len() > SEGMENT_BYTES as usize && data.len() <= 2 * SEGMENT_BYTES as usize);
        let from = total - data.len();
        assert_eq!(from % SEGMENT_BYTES as usize, 0, "起点应落在段边界上");
        assert_eq!(&data[..], &bytes[from..]);
        let (stderr, stderr_total) = log.stream_bytes(&record, "stderr");
        assert!(stderr.is_empty() && stderr_total == 0);
    }

    /// 别的主体读不到；不像 session_id 的（路径穿越）读不到、也建不出来。
    ///
    /// 穿越的目标上真放一份像样的记录：不查 id 的话，`../x` 拼出来正好读得到它。
    #[test]
    fn a_record_is_only_visible_to_its_caller_and_ids_cannot_escape() {
        let home = tempdir().expect("home");
        let root = home.path().join("runs");
        let mine = RunLog::new(root.clone(), "p", "oauth:hub:alpha");
        let theirs = RunLog::new(root.clone(), "p", "oauth:hub:beta");
        let session = id();
        drop(mine.start(&session, "true", None, 0).expect("建记录"));
        assert!(mine.load(&session).is_some());
        assert!(theirs.load(&session).is_none(), "别人的记录被读到了");

        let mut bait = read_record(&mine.dir().join(&session)).expect("记录");
        let outside = root.join("x");
        create_private_dir(&outside).expect("穿越目标");
        bait.session_id = "../x".into();
        write_record(&outside, &bait).expect("放一份记录");
        assert!(
            read_record(&mine.dir().join("../x")).is_some(),
            "诱饵没放对"
        );
        for bad in ["../x", "../../etc", "a/b", ".."] {
            assert!(mine.load(bad).is_none(), "{bad} 读到了项目目录外的记录");
            assert!(mine.start(bad, "true", None, 0).is_none());
        }
        assert!(!root.join("etc").exists() && !root.join("p/a").exists());
    }

    /// 起它的进程还在（就是自己）：还是 running。起它的进程不在了：unknown，并写回盘上。
    #[test]
    fn a_running_record_whose_owner_is_gone_becomes_unknown() {
        let home = tempdir().expect("home");
        let log = RunLog::new(home.path().join("runs"), "p", "local");
        let session = id();
        let _writer = log
            .start(&session, "sleep 30", Some(42), 0)
            .expect("建记录");
        assert_eq!(log.load(&session).expect("记录").status, "running");

        orphan_record(&log, &session);
        let settled = log.load(&session).expect("记录");
        assert_eq!(settled.status, "unknown");
        assert!(settled.finished_at_ms.is_some());
        assert_eq!(settled.exit_code, None);
        assert_eq!(settled.workspace_writes_during_run, None);
        let again = read_record(&log.dir().join(&session)).expect("盘上的");
        assert_eq!(again.status, "unknown", "结论没写回去");
    }

    /// 另一个进程拿着锁时算活着；锁放了（进程退出）就算不在了，锁文件也顺手删掉。
    #[test]
    fn an_owner_is_alive_exactly_while_its_lock_is_held() {
        let home = tempdir().expect("home");
        let root = home.path().join("runs");
        let dir = root.join(OWNERS);
        create_private_dir(&dir).expect("owners");
        let path = dir.join("feedbeef.lock");
        let held = create_private_file(&path).expect("锁文件");
        // 同一个进程里另开一个 fd 也会被 flock 挡住，所以能在单测里模拟"别的进程拿着"。
        held.try_lock_exclusive().expect("拿锁");
        assert!(owner_alive(&root, "feedbeef"));
        drop(held);
        assert!(!owner_alive(&root, "feedbeef"));
        assert!(!path.exists(), "不在了的进程的锁文件该删掉");
        assert!(!owner_alive(&root, "neverexisted"));
    }

    /// 结束的记录超过配额时，结束得最早的先走；太旧的走；还在跑的一条不动。
    #[test]
    fn finished_records_are_capped_by_count_and_age_and_running_ones_stay() {
        let home = tempdir().expect("home");
        let log = RunLog::new(home.path().join("runs"), "p", "local");
        let running = id();
        let _running = log.start(&running, "sleep 30", None, 0).expect("建记录");
        // 把这条跑着的记录的起跑时间改到很久以前：它还在跑，按年龄也不能清。
        {
            let dir = log.dir().join(&running);
            let mut record = read_record(&dir).expect("记录");
            record.started_at_ms = 1;
            write_record(&dir, &record).expect("写回");
        }
        let mut finished = Vec::new();
        for index in 0..(MAX_FINISHED_RUNS + 3) {
            let session = id();
            let writer = log.start(&session, "true", None, 0).expect("建记录");
            writer.finish("exited", Some(0), 0);
            // 结束时间拉开，排序才有先后。
            let dir = log.dir().join(&session);
            let mut record = read_record(&dir).expect("记录");
            record.finished_at_ms = Some(now_ms() - 60_000 + index as u64);
            write_record(&dir, &record).expect("写回");
            finished.push(session);
        }
        let stale = id();
        let writer = log.start(&stale, "true", None, 0).expect("建记录");
        writer.finish("exited", Some(0), 0);
        {
            let dir = log.dir().join(&stale);
            let mut record = read_record(&dir).expect("记录");
            record.finished_at_ms = Some(now_ms() - MAX_AGE.as_millis() as u64 - 1);
            write_record(&dir, &record).expect("写回");
        }
        log.sweep();
        assert!(log.load(&running).is_some(), "还在跑的被清掉了");
        assert!(log.load(&stale).is_none(), "超过七天的没清");
        let kept = finished.iter().filter(|id| log.load(id).is_some()).count();
        assert_eq!(kept, MAX_FINISHED_RUNS);
        assert!(log.load(&finished[0]).is_none(), "最早结束的该先走");
        assert!(log.load(finished.last().expect("最后一条")).is_some());
    }

    /// 不再用的项目也会被清：进程第一次拿到 owner 锁时把 `runs/` 下所有项目扫一遍，
    /// 空了的项目目录一并删掉。只在"同一个项目起新命令"时清的话，删掉的项目永远没人清。
    #[test]
    fn records_of_projects_nobody_uses_any_more_are_swept_too() {
        let home = tempdir().expect("home");
        let root = home.path().join("runs");
        let now = now_ms();
        let max_age = MAX_AGE.as_millis() as u64;
        let finished_at = |finished: u64| RunRecord {
            session_id: String::new(),
            caller: "local".into(),
            command: "true".into(),
            pid: None,
            owner: "gone".into(),
            owner_pid: 1,
            started_at_ms: finished - 10,
            status: "exited".into(),
            exit_code: Some(0),
            finished_at_ms: Some(finished),
            workspace_writes_during_run: Some(0),
            writes_at_start: Some(0),
            incomplete_logs: Vec::new(),
        };
        let stale = root.join("removed-project").join(id());
        create_private_dir(&stale).expect("dir");
        write_record(&stale, &finished_at(now - max_age - 1)).expect("旧记录");
        let fresh = root.join("idle-project").join(id());
        create_private_dir(&fresh).expect("dir");
        write_record(&fresh, &finished_at(now - 1000)).expect("新记录");

        drop(
            RunLog::new(root.clone(), "p", "local")
                .start(&id(), "true", None, 0)
                .expect("建记录"),
        );
        assert!(
            !root.join("removed-project").exists(),
            "没人用的项目里过期的记录和空目录都该清掉"
        );
        assert!(fresh.join(RECORD).exists(), "还新的记录不该清");
    }

    /// 自己的锁文件一出现在正式名字下，就已经被锁着：别的进程这时来判活，看到的是"还在"，
    /// 不会把它删掉、也不会把这个进程的命令判成 unknown。临时名字不留下。
    #[test]
    fn the_owner_lock_is_held_before_it_appears_under_its_name() {
        let home = tempdir().expect("home");
        let root = home.path().join("runs");
        let log = RunLog::new(root.clone(), "p", "local");
        drop(log.start(&id(), "true", None, 0).expect("建记录"));
        let dir = root.join(OWNERS);
        let lock = dir.join(format!("{}.lock", *INSTANCE));
        let other = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock)
            .expect("锁文件在正式名字下");
        assert!(other.try_lock_exclusive().is_err(), "锁文件在，锁却没拿着");
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .expect("owners")
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "临时锁文件没改名");
    }

    /// 写盘失败（这里用只读目录让换段失败，和盘满一样）之后这个流不再落盘，
    /// 结束时记录里注明哪个流不全；已经写下的那部分照样读得到。
    #[cfg(unix)]
    #[test]
    fn a_log_that_could_not_be_written_is_marked_incomplete() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempdir().expect("home");
        let log = RunLog::new(home.path().join("runs"), "p", "local");
        let session = id();
        let writer = log.start(&session, "flood", None, 0).expect("建记录");
        let dir = log.dir().join(&session);
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).expect("只读");
        writer.append(true, &vec![b'x'; SEGMENT_BYTES as usize + 10]);
        writer.append(true, b"after");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).expect("恢复");
        writer.finish("exited", Some(0), 0);
        let record = log.load(&session).expect("记录");
        assert_eq!(record.incomplete_logs, vec!["stdout".to_string()]);
        let (data, total) = log.stream_bytes(&record, "stdout");
        assert_eq!(total, SEGMENT_BYTES as usize, "写下的那部分应当还在");
        assert_eq!(data.len(), total);
    }

    /// 目录 0700、文件 0600：输出不脱敏，别让同机的其他账号读到。
    #[cfg(unix)]
    #[test]
    fn records_and_logs_are_private_to_the_owner() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempdir().expect("home");
        let log = RunLog::new(home.path().join("runs"), "p", "local");
        let session = id();
        let writer = log.start(&session, "echo secret", None, 0).expect("建记录");
        writer.append(true, b"token=abc\n");
        writer.finish("exited", Some(0), 0);
        let dir = log.dir().join(&session);
        let mode = |path: &Path| fs::metadata(path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode(log.dir()), 0o700);
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join(RECORD)), 0o600);
        assert_eq!(mode(&segment_path(&dir, "stdout", 0)), 0o600);
    }
}

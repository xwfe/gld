//! 一条通往 MCP server 的通道：一次发一条 JSON-RPC 消息，一次收一条。
//!
//! 两种实现：[`Stdio`] 是本机子进程的 stdin/stdout；HTTP 的在 [`super::http`]。
//! 协议逻辑（握手、分页、谁回的是哪条）在 [`super::client`]，这里只搬字节。
//!
//! 不复用 `bridge::peer::ChildTransport`：那个是给 `ccnm mcp bridge` 用的，
//! 这里多三件它没有的事——环境变量和工作目录照配置给、一行有上限（别人家的
//! server 一行吐 2 GB 不能把 gld 吃掉）、关的时候连它起的子进程一起收
//! （`npx` 起的 server 真正干活的是 `npx` 下面那个 `node`）。

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio as Pipe};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::client::Error;

/// 一行（一条消息）最多收这么大。deepwiki 的 `read_wiki_contents` 实测一条
/// 回复 839 KB；给到 32 MiB，正常的回复远到不了，吐个没完的 server 也撑不爆
/// 内存。超了这条回复作废，调用方拿到的是"太大"，不是半截 JSON。
pub const MAX_MESSAGE_BYTES: usize = 32 * 1024 * 1024;

/// stderr 留最后这么多字节：server 起不来时，原因多半在这里。
const STDERR_KEEP: usize = 4 * 1024;

/// 关的时候等它自己退多久，到点连同子进程一起杀。MCP server 读到 stdin 的
/// EOF 就该退，正常几十毫秒。
const CLOSE_GRACE: Duration = Duration::from_secs(3);

/// 收一条的结果。
pub enum Recv {
    Line(String),
    /// 这条比 [`MAX_MESSAGE_BYTES`] 大，读过去了，没留。
    TooLong(u64),
    Timeout,
    /// 对面关了。`said` 是它在 stderr 上留下的最后一段话。
    Closed {
        said: String,
    },
}

pub trait Transport: Send {
    /// 发一条消息。HTTP 在这一步就把回复收回来了，所以要知道等多久。
    fn send(&mut self, line: &str, timeout: Duration) -> Result<(), Error>;
    /// 收下一条，最多等 `timeout`。
    fn recv(&mut self, timeout: Duration) -> Recv;
    /// 收工：之后这条通道就废了。
    fn close(&mut self);
}

/// 本机子进程。
pub struct Stdio {
    child: Child,
    /// 关掉之后是 `None`：让对面读到 EOF 就是靠丢掉它。
    stdin: Option<ChildStdin>,
    lines: Receiver<Recv>,
    stderr: Arc<Mutex<VecDeque<u8>>>,
    closed: bool,
}

impl Stdio {
    /// 起一个 server 进程。argv 直接传，不经 shell。
    ///
    /// `path` 是给它的 `PATH`（gld 的"全局可执行文件路径"加上守护进程自己的）。
    /// 程序名也按它找——守护进程由 launchd / systemd 拉起时自己的 `PATH` 往往
    /// 只有 `/usr/bin:/bin`，`npx`、`uvx` 都不在里面，报错得说清楚是这件事。
    pub fn spawn(
        command: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
        cwd: &Path,
        path: Option<&OsString>,
    ) -> Result<Stdio, Error> {
        let program = which::which_in(command, path, cwd).map_err(|_| {
            // 整条 PATH 动辄几 KB，报错里只给个数和开头几个，够判断"是不是
            // 守护进程的 PATH 太短"。
            let dirs: Vec<String> = path
                .map(|value| {
                    std::env::split_paths(value)
                        .map(|dir| dir.display().to_string())
                        .collect()
                })
                .unwrap_or_default();
            let head = dirs.iter().take(3).cloned().collect::<Vec<_>>().join(":");
            let more = if dirs.len() > 3 { ":…" } else { "" };
            Error::Start(format!(
                "cannot find `{command}`: it is not a file and not on the PATH the server is started with ({} directories: {head}{more})",
                dirs.len()
            ))
        })?;
        let mut builder = Command::new(&program);
        builder
            .args(args)
            .current_dir(cwd)
            .stdin(Pipe::piped())
            .stdout(Pipe::piped())
            .stderr(Pipe::piped());
        if let Some(path) = path {
            builder.env("PATH", path);
        }
        builder.envs(env);
        // 自己一个进程组：关的时候连它下面的进程一起收。
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            builder.process_group(0);
        }
        let mut child = builder
            .spawn()
            .map_err(|error| Error::Start(format!("cannot start `{command}`: {error}")))?;
        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");

        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || pump_lines(stdout, tx));
        let kept = Arc::new(Mutex::new(VecDeque::new()));
        let sink = kept.clone();
        std::thread::spawn(move || pump_stderr(stderr, sink));
        Ok(Stdio {
            child,
            stdin: Some(stdin),
            lines,
            stderr: kept,
            closed: false,
        })
    }

    fn said(&self) -> String {
        let kept = self
            .stderr
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let bytes: Vec<u8> = kept.iter().copied().collect();
        String::from_utf8_lossy(&bytes).trim().to_string()
    }
}

impl Transport for Stdio {
    fn send(&mut self, line: &str, _timeout: Duration) -> Result<(), Error> {
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(Error::Closed {
                during: "send".into(),
                said: String::new(),
            });
        };
        let written = stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.write_all(b"\n"))
            .and_then(|()| stdin.flush());
        written.map_err(|_| Error::Closed {
            during: "send".into(),
            said: self.said(),
        })
    }

    fn recv(&mut self, timeout: Duration) -> Recv {
        match self.lines.recv_timeout(timeout) {
            Ok(Recv::Closed { .. }) | Err(RecvTimeoutError::Disconnected) => {
                // stdout 关了之后 stderr 可能还差最后一截没读完。
                std::thread::sleep(Duration::from_millis(50));
                Recv::Closed { said: self.said() }
            }
            Ok(other) => other,
            Err(RecvTimeoutError::Timeout) => Recv::Timeout,
        }
    }

    /// 先关 stdin 让它读到 EOF 自己退；宽限期过了还在，就连同它的进程组
    /// 一起杀。
    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.stdin.take();
        let deadline = Instant::now() + CLOSE_GRACE;
        let exited = loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break true,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => break false,
            }
        };
        if !exited {
            let _ = crate::platform::platform().terminate_process_tree(self.child.id());
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        // 领头的退了，它起的进程（浏览器、`npx` 下面的 `node`）可能还在同一个
        // 进程组里。组号就是它的 pid；组已经空了时这一下什么都不做。
        #[cfg(unix)]
        unsafe {
            libc::killpg(self.child.id() as libc::pid_t, libc::SIGKILL);
        }
    }
}

impl Drop for Stdio {
    fn drop(&mut self) {
        self.close();
    }
}

fn pump_lines(stdout: ChildStdout, tx: mpsc::Sender<Recv>) {
    let mut reader = BufReader::new(stdout);
    let limits = toexec_text::LineLimits {
        keep: MAX_MESSAGE_BYTES,
        scan_limit: None,
    };
    let mut raw = Vec::new();
    let mut scanned = 0u64;
    loop {
        raw.clear();
        let before = scanned;
        let item = match toexec_text::next_line(&mut reader, &mut raw, limits, &mut scanned) {
            Ok(Some(_)) => {
                let length = scanned - before;
                // 行尾最多两个字节（`\r\n`）；比留下的多出更多，就是被截过。
                if length > raw.len() as u64 + 2 {
                    Recv::TooLong(length)
                } else {
                    Recv::Line(String::from_utf8_lossy(&raw).into_owned())
                }
            }
            Ok(None) | Err(_) => {
                let _ = tx.send(Recv::Closed {
                    said: String::new(),
                });
                return;
            }
        };
        if tx.send(item).is_err() {
            return;
        }
    }
}

fn pump_stderr(stderr: std::process::ChildStderr, sink: Arc<Mutex<VecDeque<u8>>>) {
    let mut reader = BufReader::new(stderr);
    let mut buf = [0u8; 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                let mut kept = sink.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                kept.extend(&buf[..n]);
                while kept.len() > STDERR_KEEP {
                    kept.pop_front();
                }
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn spawn(script: &str) -> Stdio {
        let cwd = std::env::temp_dir();
        let path = std::env::var_os("PATH");
        Stdio::spawn(
            "sh",
            &["-c".into(), script.into()],
            &BTreeMap::from([("GREETING".into(), "hi".into())]),
            &cwd,
            path.as_ref(),
        )
        .expect("spawn sh")
    }

    #[test]
    fn a_line_goes_out_and_one_comes_back_with_the_configured_env() {
        let mut child = spawn("read line; echo \"$GREETING:$line\"");
        child.send("ping", Duration::from_secs(1)).unwrap();
        match child.recv(Duration::from_secs(5)) {
            Recv::Line(line) => assert_eq!(line, "hi:ping"),
            _ => panic!("expected a line"),
        }
        child.close();
    }

    #[test]
    fn what_it_said_on_stderr_comes_with_the_close() {
        let mut child = spawn("echo 'missing API key' >&2; exit 1");
        match child.recv(Duration::from_secs(5)) {
            Recv::Closed { said } => assert!(said.contains("missing API key"), "{said}"),
            _ => panic!("expected closed"),
        }
    }

    #[test]
    fn a_program_that_is_not_on_the_path_says_which_path_was_searched() {
        let path = OsString::from("/nowhere/bin");
        let Err(error) = Stdio::spawn(
            "gld-no-such-mcp-server",
            &[],
            &BTreeMap::new(),
            &std::env::temp_dir(),
            Some(&path),
        ) else {
            panic!("should not start");
        };
        let text = error.to_string();
        assert!(text.contains("gld-no-such-mcp-server"), "{text}");
        assert!(text.contains("1 directories: /nowhere/bin"), "{text}");
    }

    /// 读到 EOF 不退的 server：宽限期过了连它起的子进程一起收掉。
    #[test]
    fn a_server_that_ignores_eof_is_killed_with_its_children() {
        let mut child =
            spawn("trap '' TERM; (trap '' TERM; sleep 30) & while true; do sleep 1; done");
        let started = Instant::now();
        child.close();
        assert!(started.elapsed() < Duration::from_secs(10));
        // 被杀的孤儿要等 init 收尸，给它一点时间。
        let group = child.child.id() as libc::pid_t;
        let deadline = Instant::now() + Duration::from_secs(3);
        while unsafe { libc::killpg(group, 0) } == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_ne!(
            unsafe { libc::killpg(group, 0) },
            0,
            "进程组里还有活着的进程"
        );
    }
}

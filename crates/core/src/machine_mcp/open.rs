//! gld 怎么把一个装好的 server 开出来：起进程（stdio）或连地址（HTTP）。
//!
//! 通道本身、握手、连接池都在共享库 `toexec-mcp`；这里只放 gld 自己的决定：
//!
//! - `PATH` 是 gld 的全局可执行文件路径加上守护进程自己的（[`Launch`]），程序名也
//!   按它找——守护进程由 launchd / systemd 拉起时自己的 `PATH` 往往只有
//!   `/usr/bin:/bin`，`npx`、`uvx` 都不在里面，报错要说清楚是这件事；
//! - 工作目录是主目录（这些 server 属于这台机器，不属于哪个项目）；
//! - 进程放进它自己的进程组，关的时候连它下面的进程一起收（`npx` 起的 server，
//!   干活的是它下面那个 `node`）；
//! - HTTP 照 gld 的全局出站代理（[`super::http`]）。

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use toexec_mcp::child::{ChildTransport, Stop};
use toexec_mcp::installed::{Server, Transport as Config};
use toexec_mcp::{Error, Open, Transport};

use super::http::{Http, Proxy};

/// 怎么起 server：进程的 `PATH` 和工作目录，HTTP 走不走代理。
#[derive(Debug, Clone)]
pub struct Launch {
    pub path: Option<OsString>,
    /// Codex 配置没写 `cwd` 时用它（主目录）；写了相对路径的也按它算。
    pub cwd: PathBuf,
    pub proxy: Proxy,
}

/// 按 [`Launch`] 起进程、连地址。
pub struct Opener {
    launch: Launch,
}

impl Opener {
    pub fn new(launch: Launch) -> Opener {
        Opener { launch }
    }
}

impl Open for Opener {
    fn open(&self, server: &Server) -> Result<Box<dyn Transport>, Error> {
        match &server.transport {
            Config::Stdio {
                command,
                args,
                env,
                cwd,
            } => {
                let cwd = cwd
                    .as_ref()
                    .map(|dir| self.launch.cwd.join(dir))
                    .unwrap_or_else(|| self.launch.cwd.clone());
                Ok(Box::new(spawn(
                    command,
                    args,
                    env,
                    &cwd,
                    self.launch.path.as_ref(),
                )?))
            }
            Config::Http { url, headers } => {
                Ok(Box::new(Http::new(url, headers, &self.launch.proxy)?))
            }
            Config::Sse { .. } => Err(Error::Start(
                "it uses the old HTTP+SSE transport (type \"sse\"), which is not supported here; the server's docs usually give a streamable HTTP address (often ending in /mcp) to use instead".into(),
            )),
        }
    }

    fn me(&self) -> (&str, &str) {
        ("gld", env!("CARGO_PKG_VERSION"))
    }
}

/// 起一个 stdio server。argv 直接传，不经 shell。
pub fn spawn(
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    cwd: &Path,
    path: Option<&OsString>,
) -> Result<ChildTransport, Error> {
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
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(path) = path {
        builder.env("PATH", path);
    }
    builder.envs(env);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        builder.process_group(0);
    }
    let child = builder
        .spawn()
        .map_err(|error| Error::Start(format!("cannot start `{command}`: {error}")))?;
    ChildTransport::new(child, stop())
}

/// 关的时候：没自己退的连同进程树一起杀；自己退了的，把它留在进程组里的
/// 也清掉（组号就是它的 pid，组已经空了时这一下什么都不做）。
fn stop() -> Stop {
    Box::new(|child, exited| {
        if !exited {
            let _ = crate::platform::platform().terminate_process_tree(child.id());
        }
        #[cfg(unix)]
        unsafe {
            libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
        }
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use toexec_mcp::Recv;

    fn sh(script: &str) -> ChildTransport {
        let path = std::env::var_os("PATH");
        spawn(
            "sh",
            &["-c".into(), script.into()],
            &BTreeMap::from([("GREETING".into(), "hi".into())]),
            &std::env::temp_dir(),
            path.as_ref(),
        )
        .expect("spawn sh")
    }

    #[test]
    fn the_configured_env_reaches_the_server() {
        let mut child = sh("read line; echo \"$GREETING:$line\"");
        child.send("ping", Duration::from_secs(1)).unwrap();
        match child.recv(Duration::from_secs(5)) {
            Recv::Line(line) => assert_eq!(line, "hi:ping"),
            _ => panic!("expected a line"),
        }
    }

    #[test]
    fn a_program_that_is_not_on_the_path_says_which_path_was_searched() {
        let path = OsString::from("/nowhere/bin");
        let Err(error) = spawn(
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
        let mut child = sh("trap '' TERM; (trap '' TERM; sleep 30) & while true; do sleep 1; done");
        let group = child.id() as libc::pid_t;
        let started = Instant::now();
        child.close();
        assert!(started.elapsed() < Duration::from_secs(10));
        // 被杀的孤儿要等 init 收尸，给它一点时间。
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

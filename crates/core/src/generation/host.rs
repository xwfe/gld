//! 宿主手里的一条执行代连接：起子进程、发消息、等回包。

use std::collections::HashMap;
use std::io::{BufReader, ErrorKind};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use serde_json::{json, Value};

use super::router::{CallError, Router};
use super::wire::{read_frame, FrameWriter, GenerationFrame, HostFrame, HostOp, Op};
use super::{BuildIdentity, GENERATION_PROTOCOL};

type Reply = Result<Value, String>;

/// 一个执行代子进程。挂了不重连：通道是它的 stdin/stdout，进程没了通道也就没了。
pub(super) struct Link {
    pub id: u64,
    pub exe: PathBuf,
    identity: OnceLock<BuildIdentity>,
    writer: FrameWriter<ChildStdin>,
    pending: Mutex<HashMap<u64, Sender<Reply>>>,
    next: AtomicU64,
    alive: AtomicBool,
    pub in_flight: AtomicUsize,
    child: Mutex<Child>,
}

impl Link {
    /// 起子进程，等它自报家门。超时、提前退出、发来的不是协议消息都算失败，子进程当场杀掉。
    pub fn spawn(
        id: u64,
        exe: &Path,
        router: Weak<Router>,
        ready_timeout: Duration,
    ) -> Result<Arc<Self>, String> {
        let home = crate::home::data_home().map_err(|error| error.to_string())?;
        let mut command = Command::new(exe);
        command
            .args(["daemon", "generation"])
            .env(crate::home::HOME_ENV, &home)
            .current_dir(&home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // 和守护进程自己的日志写在一起。
            .stderr(Stdio::inherit());
        // 单独一个进程组：终端上的 Ctrl-C 只到宿主，由宿主决定怎么收，
        // 杀失败的新代时也能连它开出来的子进程一起杀。
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("起不来 {}：{error}", exe.display()))?;
        let stdin = child.stdin.take().ok_or("子进程没有 stdin")?;
        let stdout = child.stdout.take().ok_or("子进程没有 stdout")?;
        let link = Arc::new(Self {
            id,
            exe: exe.to_path_buf(),
            identity: OnceLock::new(),
            writer: FrameWriter::new(stdin),
            pending: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            alive: AtomicBool::new(true),
            in_flight: AtomicUsize::new(0),
            child: Mutex::new(child),
        });

        let (ready, wait_ready) = mpsc::channel();
        {
            let link = Arc::clone(&link);
            std::thread::Builder::new()
                .name(format!("gld-gen-{id}"))
                .spawn(move || link.read_loop(stdout, router, ready))
                .map_err(|error| format!("起读线程失败：{error}"))?;
        }

        let hello = HostFrame::Hello {
            generation_protocol: GENERATION_PROTOCOL,
        };
        let outcome = match link.writer.send(&hello) {
            // 对方一启动就退了也可能走到这里；读线程那边的说法更准，等它。
            Err(error) if error.kind() != ErrorKind::BrokenPipe => {
                Err(format!("发不出 hello：{error}"))
            }
            _ => match wait_ready.recv_timeout(ready_timeout) {
                Ok(outcome) => outcome,
                Err(RecvTimeoutError::Timeout) => {
                    Err(format!("{} 秒内没有就绪", ready_timeout.as_secs()))
                }
                Err(RecvTimeoutError::Disconnected) => Err("读线程提前结束".to_string()),
            },
        };
        match outcome {
            Ok(identity) => {
                let _ = link.identity.set(identity);
                Ok(link)
            }
            Err(reason) => {
                link.kill();
                Err(reason)
            }
        }
    }

    pub fn identity(&self) -> &BuildIdentity {
        self.identity.get().expect("spawn 成功才拿得到 Link")
    }

    pub fn pid(&self) -> u32 {
        self.identity().pid
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// 发一条操作，等回包。`timeout` 只给控制类操作用；工具调用的时限归工具自己管，
    /// 这里替它做主只会把还在跑的调用报成失败。
    pub fn call(&self, op: &Op, timeout: Option<Duration>) -> Result<Value, CallError> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (reply, wait) = mpsc::channel();
        self.lock_pending().insert(id, reply);
        // 先登记再看活没活：读线程是先改 alive、再清表。要么这里看到它死了，
        // 要么那边清表时带走这一条，发送端一丢这里立刻醒，不会有卡死的窗口。
        if !self.is_alive() {
            self.lock_pending().remove(&id);
            return Err(CallError::Unavailable(format!(
                "执行代 g{} 已退出",
                self.id
            )));
        }
        let op = serde_json::to_value(op).map_err(|error| CallError::Refused(error.to_string()))?;
        if let Err(error) = self.writer.send(&HostFrame::Call { id, op }) {
            self.lock_pending().remove(&id);
            // 整行没写完对方就不会处理，所以这里可以放心说没执行。
            return Err(CallError::Unavailable(format!("发不到执行代：{error}")));
        }
        let answer = match timeout {
            Some(timeout) => wait.recv_timeout(timeout).map_err(|error| match error {
                RecvTimeoutError::Timeout => CallError::TimedOut,
                RecvTimeoutError::Disconnected => CallError::Lost,
            }),
            None => wait.recv().map_err(|_| CallError::Lost),
        };
        if answer.is_err() {
            self.lock_pending().remove(&id);
        }
        answer?.map_err(CallError::Refused)
    }

    /// 先礼后兵：让它自己收好，等不到就杀。
    pub fn shutdown(&self, wait: Duration) {
        let _ = self.call(&Op::Shutdown, Some(wait));
        self.kill();
    }

    pub fn kill(&self) {
        let mut child = self
            .child
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        #[cfg(unix)]
        {
            // SAFETY: 子进程是 process_group(0) 起的，进程组号就是它的 pid。
            // 没 wait 之前这个 pid 不会被回收，所以不会杀错。
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
        }
        let _ = child.kill();
    }

    fn lock_pending(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Sender<Reply>>> {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn read_loop(
        self: Arc<Self>,
        stdout: ChildStdout,
        router: Weak<Router>,
        ready: Sender<Result<BuildIdentity, String>>,
    ) {
        let mut reader = BufReader::new(stdout);
        let mut ready = Some(ready);
        let reason = loop {
            let line = match read_frame(&mut reader) {
                Ok(Some(line)) => line,
                Ok(None) => break "管道关闭".to_string(),
                Err(error) => break format!("读管道失败：{error}"),
            };
            let frame = match serde_json::from_str::<GenerationFrame>(&line) {
                Ok(frame) => frame,
                // 不回显内容：候选二进制的输出不可信，也可能带敏感东西。
                Err(error) => break format!("收到的不是协议消息（{error}）"),
            };
            match frame {
                GenerationFrame::Ready { identity } => {
                    if let Some(ready) = ready.take() {
                        let _ = ready.send(Ok(identity));
                    }
                }
                GenerationFrame::Refused { reason } => break format!("它拒绝了：{reason}"),
                GenerationFrame::Reply { id, ok, value } => {
                    if let Some(reply) = self.lock_pending().remove(&id) {
                        let _ = reply.send(if ok {
                            Ok(value)
                        } else {
                            Err(value.as_str().unwrap_or("执行代拒绝了").to_string())
                        });
                    }
                }
                GenerationFrame::HostCall { id, op } => {
                    let Some(router) = router.upgrade() else {
                        continue;
                    };
                    let link = Arc::clone(&self);
                    // 节点调用最长 60 秒，不能占着这条读线程：别的回包都得从这儿过。
                    let handle = router.handle().clone();
                    handle.spawn_blocking(move || {
                        let (ok, value) = match answer_host_call(&router, op) {
                            Ok(value) => (true, value),
                            Err(message) => (false, json!(message)),
                        };
                        let _ = link.writer.send(&HostFrame::HostReply { id, ok, value });
                    });
                }
            }
        };
        if let Some(ready) = ready.take() {
            let _ = ready.send(Err(reason.clone()));
        }
        self.alive.store(false, Ordering::SeqCst);
        // 在途调用立刻知道它没了，不用等超时。
        self.lock_pending().clear();
        self.kill();
        let _ = self
            .child
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .wait();
        if let Some(router) = router.upgrade() {
            router.on_lost(self.id, &reason);
        }
    }
}

fn answer_host_call(router: &Router, op: HostOp) -> Result<Value, String> {
    match op {
        HostOp::NodeMembers => router.node_members(),
        HostOp::NodeCall {
            auth,
            member,
            tool,
            args,
        } => Ok(router.node_call(&auth.into_auth(), member.into(), &tool, args)),
    }
}

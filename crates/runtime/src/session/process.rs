//! 子进程、管道读取与终态落盘。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt;
use tokio::process::Child;
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use super::output::command_ok;
use super::process_control::signal_process_tree;
use super::{ExecSession, SESSION_BUFFER_BYTES};

impl ExecSession {
    /// 测试用：没有命令文本，也不跟任何工作区的落盘计数。
    #[cfg(test)]
    pub fn new(child: Child) -> Self {
        Self::new_with_mode(child, false, String::new(), Arc::new(AtomicU64::new(0)))
    }

    pub fn new_with_mode(
        mut child: Child,
        interactive: bool,
        command: String,
        workspace_writes: Arc<AtomicU64>,
    ) -> Self {
        let session_id = Uuid::new_v4().to_string();
        let stdin = child.stdin.take();
        let stdin_open = stdin.is_some();
        let writes_at_start = workspace_writes.load(Ordering::Acquire);
        Self {
            session_id,
            command,
            pid: child.id(),
            service_port: None,
            run: None,
            child: AsyncMutex::new(child),
            stdin: AsyncMutex::new(stdin),
            stdin_open: Mutex::new(stdin_open),
            interactive,
            workspace_writes,
            writes_at_start,
            stdout: Mutex::new(Vec::new()),
            stderr: Mutex::new(Vec::new()),
            stdout_total: Mutex::new(0),
            stderr_total: Mutex::new(0),
            started_at: Instant::now(),
            exit_code: Mutex::new(None),
            exited: AtomicBool::new(false),
            finished_at: Mutex::new(None),
            termination_reason: Mutex::new(None),
            reader_tasks: AsyncMutex::new(Vec::new()),
            slow_readers: Mutex::new(Vec::new()),
        }
    }

    /// 服务模式：要在 [`SessionStore::insert`] 之前设，运行记录里才有这个端口。
    pub fn with_service_port(mut self, port: Option<u16>) -> Self {
        self.service_port = port;
        self
    }

    pub async fn spawn_readers(self: &Arc<Self>) {
        let stdout = {
            let mut guard = self.child.lock().await;
            guard.stdout.take()
        };
        let stderr = {
            let mut guard = self.child.lock().await;
            guard.stderr.take()
        };
        if let Some(stream) = stdout {
            let session = Arc::clone(self);
            let task = crate::async_rt::spawn(async move {
                session.read_stream(stream, true).await;
            });
            self.reader_tasks.lock().await.push(task);
        }
        if let Some(stream) = stderr {
            let session = Arc::clone(self);
            let task = crate::async_rt::spawn(async move {
                session.read_stream(stream, false).await;
            });
            self.reader_tasks.lock().await.push(task);
        }
    }

    /// 输出还有人在读。管道的写端全关了读任务才结束，所以它也说明还有进程占着管道。
    /// 同步函数：表里摘会话时可能在异步线程上。拿不到锁算还在读，多留一会儿没有坏处。
    pub(super) fn readers_alive(&self) -> bool {
        let pending = self
            .reader_tasks
            .try_lock()
            .map_or(true, |tasks| tasks.iter().any(|task| !task.is_finished()));
        pending
            || self
                .slow_readers
                .lock()
                .expect("slow_readers lock")
                .iter()
                .any(|task| !task.is_finished())
    }

    pub async fn wait_for_readers(&self) {
        let mut tasks = self.reader_tasks.lock().await;
        while let Some(mut task) = tasks.pop() {
            let waited =
                tokio::time::timeout(std::time::Duration::from_millis(500), &mut task).await;
            if waited.is_err() {
                self.slow_readers
                    .lock()
                    .expect("slow_readers lock")
                    .push(task);
            }
        }
    }

    async fn read_stream<T>(&self, mut stream: T, is_stdout: bool)
    where
        T: tokio::io::AsyncRead + Unpin,
    {
        let mut buf = [0u8; 4096];
        loop {
            match stream.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = &buf[..n];
                    if let Some(run) = &self.run {
                        run.append(is_stdout, chunk);
                    }
                    if is_stdout {
                        let mut data = self.stdout.lock().expect("stdout lock");
                        data.extend_from_slice(chunk);
                        *self.stdout_total.lock().expect("stdout_total lock") += n;
                        trim_buffer(&mut data, SESSION_BUFFER_BYTES);
                    } else {
                        let mut data = self.stderr.lock().expect("stderr lock");
                        data.extend_from_slice(chunk);
                        *self.stderr_total.lock().expect("stderr_total lock") += n;
                        trim_buffer(&mut data, SESSION_BUFFER_BYTES);
                    }
                }
                Err(_) => break,
            }
        }
    }

    pub async fn kill_and_wait(&self) {
        let status = {
            let mut child = self.child.lock().await;
            if let Some(pid) = child.id() {
                signal_process_tree(pid, "KILL");
            }
            let _ = child.start_kill();
            child.wait().await.ok()
        };
        if let Some(status) = status {
            self.record_exit_status(status);
        }
    }

    pub async fn refresh_status(&self) {
        let mut child = self.child.lock().await;
        if let Ok(Some(status)) = child.try_wait() {
            self.record_exit_status(status);
        }
    }

    /// 同 [`ExecSession::refresh_status`]，但子进程锁被占着就不等。
    ///
    /// `kill_session` 停命令时拿着这把锁等进程退出，最长 30 秒；`list_runs` 一次看几十条，
    /// 不能被其中一条卡住（独立审查实测卡了 4.7 秒）。拿不到锁时用当前已知的状态。
    pub fn try_refresh_status(&self) {
        if let Ok(mut child) = self.child.try_lock() {
            if let Ok(Some(status)) = child.try_wait() {
                self.record_exit_status(status);
            }
        }
    }

    fn record_exit_status(&self, status: std::process::ExitStatus) {
        // 整段握着 termination 锁：[`ExecSession::mark_termination_reason`] 也拿它，
        // 两边谁先谁后都看得到对方，内存和运行记录里的终态才对得上（独立审查发现：
        // 以前 kill 判断"还在跑"之后进程自己退了，记录写 exited、内存却被改成 killed）。
        let mut reason = self.termination_reason.lock().expect("termination lock");
        *self.exit_code.lock().expect("exit_code lock") = status.code();
        self.exited.store(true, Ordering::Release);
        let mut finished = self.finished_at.lock().expect("finished_at lock");
        let first = finished.is_none();
        if first {
            *finished = Some(Instant::now());
        }
        drop(finished);
        *self.stdin_open.lock().expect("stdin_open lock") = false;
        if reason.is_none() {
            *reason = Some("exited".into());
        }
        let reason = reason.clone().unwrap_or_else(|| "exited".into());
        // 结束的那一刻就落盘，不等有人来读：之后 gld 重启了，这份记录就是唯一知道结局的地方。
        // timeout / killed / interrupted 是停之前先标上的，所以这里拿到的已经是最终原因。
        if first {
            if let Some(run) = &self.run {
                run.finish(&reason, status.code(), self.workspace_writes_since_start());
            }
        }
    }

    pub(crate) fn has_exited(&self) -> bool {
        self.exited.load(Ordering::Acquire)
    }

    /// 进程结束了多久。还在跑就是 `None`。
    pub fn finished_ago(&self) -> Option<Duration> {
        self.finished_at
            .lock()
            .expect("finished_at lock")
            .map(|at| at.elapsed())
    }

    /// 还能读多久。还在跑的会话没有这个数——保留期从进程结束才开始算。
    pub fn expires_in(&self, retention: Duration) -> Option<Duration> {
        self.finished_ago().map(|ago| retention.saturating_sub(ago))
    }

    /// 这条命令起来之后，工作区落过几次盘。
    pub fn workspace_writes_since_start(&self) -> u64 {
        self.workspace_writes
            .load(Ordering::Acquire)
            .saturating_sub(self.writes_at_start)
    }

    /// 停它之前标上原因。**已经结束了就不改**：结局已经按实际情况记下（也写进了运行
    /// 记录），再改成 killed 就和记录对不上。
    pub fn mark_termination_reason(&self, reason: &str) {
        let mut current = self.termination_reason.lock().expect("termination lock");
        if !self.has_exited() {
            *current = Some(reason.to_string());
        }
    }

    pub(crate) fn mark_stdin_closed(&self) {
        *self.stdin_open.lock().expect("stdin_open lock") = false;
    }

    pub async fn is_running(&self) -> bool {
        self.refresh_status().await;
        !self.has_exited()
    }

    pub fn retained_stream_bytes(&self, stream: &str) -> (Vec<u8>, usize) {
        match stream {
            "stderr" => {
                let data = self.stderr.lock().expect("stderr lock").clone();
                let total = *self.stderr_total.lock().expect("stderr_total lock");
                (data, total)
            }
            _ => {
                let data = self.stdout.lock().expect("stdout lock").clone();
                let total = *self.stdout_total.lock().expect("stdout_total lock");
                (data, total)
            }
        }
    }

    /// 命令此刻怎样了：(termination_reason, exit_code, command_ok)。不碰输出缓冲。
    pub fn termination(&self) -> (String, Option<i32>, Option<bool>) {
        let exit_code = *self.exit_code.lock().expect("exit_code lock");
        let reason = self
            .termination_reason
            .lock()
            .expect("termination lock")
            .clone()
            .unwrap_or_else(|| "running".into());
        let command_ok = command_ok(&reason, exit_code);
        (reason, exit_code, command_ok)
    }

    /// 这条会话有没有在写运行记录。
    pub fn kept_on_disk(&self) -> bool {
        self.run.is_some()
    }
}

fn trim_buffer(buf: &mut Vec<u8>, limit: usize) {
    if buf.len() > limit {
        let drop = buf.len() - limit;
        buf.drain(..drop);
    }
}

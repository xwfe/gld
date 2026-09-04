//! 本机 IPC 传输层：Unix domain socket（macOS / Linux）或命名管道（Windows）。
//!
//! 只暴露三个东西：[`bind`]、[`connect`]、以及一行一条 JSON 的
//! [`read_line`] / [`write_line`]。上层不关心底层是哪种 socket。

use std::io;
use std::path::Path;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

/// 单条消息上限。本机通信不该有这么大的消息；超过说明对端不对劲。
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

/// 读一行（不含换行符）。空连接返回 `Ok(None)`。
pub async fn read_line<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> io::Result<Option<String>> {
    let mut line = String::new();
    let mut total = 0usize;
    loop {
        let buffer = reader.fill_buf().await?;
        if buffer.is_empty() {
            return Ok((!line.is_empty()).then_some(line));
        }
        let (chunk, done) = match buffer.iter().position(|byte| *byte == b'\n') {
            Some(index) => (&buffer[..index], true),
            None => (buffer, false),
        };
        total += chunk.len();
        if total > MAX_MESSAGE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("message exceeds {MAX_MESSAGE_BYTES} bytes"),
            ));
        }
        line.push_str(&String::from_utf8_lossy(chunk));
        let consumed = chunk.len() + usize::from(done);
        reader.consume(consumed);
        if done {
            return Ok(Some(line));
        }
    }
}

pub async fn write_line<W: AsyncWrite + Unpin>(writer: &mut W, line: &str) -> io::Result<()> {
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await
}

#[cfg(unix)]
mod imp {
    use std::io;
    use std::path::Path;

    use tokio::net::{UnixListener, UnixStream};

    pub type Listener = UnixListener;
    pub type Stream = UnixStream;

    pub fn bind(path: &Path) -> io::Result<Listener> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // 上一次异常退出可能留下 socket 文件；能连上说明还有活的守护进程，
        // 那是调用方的责任（server 启动前已经探测过），这里只清理残留文件。
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        let listener = UnixListener::bind(path)?;
        // 只有当前用户能连：密钥会在这条通道上传输。
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(listener)
    }

    pub async fn accept(listener: &Listener) -> io::Result<Stream> {
        listener.accept().await.map(|(stream, _)| stream)
    }

    pub async fn connect(path: &Path) -> io::Result<Stream> {
        UnixStream::connect(path).await
    }

    pub fn cleanup(path: &Path) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::path::Path;

    use tokio::net::windows::named_pipe::{
        ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
    };

    /// Windows 上 listener 记住管道名，并持有“下一个等待连接的实例”。
    ///
    /// tokio 的命名管道模型：每个客户端连接对应一个服务端实例，所以 bind 时
    /// 先创建第一个实例（顺便用 first_pipe_instance 保证名字没被占），
    /// 之后每次 accept 用掉当前实例，再为下一个客户端预创建一个。
    /// 如果不复用 bind 时创建的实例，它会截住第一个客户端却没人读，客户端就会挂到超时。
    pub struct Listener {
        name: String,
        pending: std::sync::Mutex<Option<NamedPipeServer>>,
    }

    pub type Stream = Connection;

    pub enum Connection {
        Server(NamedPipeServer),
        Client(NamedPipeClient),
    }

    impl tokio::io::AsyncRead for Connection {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            match self.get_mut() {
                Connection::Server(inner) => std::pin::Pin::new(inner).poll_read(cx, buf),
                Connection::Client(inner) => std::pin::Pin::new(inner).poll_read(cx, buf),
            }
        }
    }

    impl tokio::io::AsyncWrite for Connection {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<io::Result<usize>> {
            match self.get_mut() {
                Connection::Server(inner) => std::pin::Pin::new(inner).poll_write(cx, buf),
                Connection::Client(inner) => std::pin::Pin::new(inner).poll_write(cx, buf),
            }
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            match self.get_mut() {
                Connection::Server(inner) => std::pin::Pin::new(inner).poll_flush(cx),
                Connection::Client(inner) => std::pin::Pin::new(inner).poll_flush(cx),
            }
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            match self.get_mut() {
                Connection::Server(inner) => std::pin::Pin::new(inner).poll_shutdown(cx),
                Connection::Client(inner) => std::pin::Pin::new(inner).poll_shutdown(cx),
            }
        }
    }

    fn pipe_name(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    /// 创建监听端。
    ///
    /// `first_pipe_instance(true)` 顺带充当第二道单实例保护：同名管道已存在时
    /// 直接失败，不会出现两个守护进程各自持有一半连接。
    ///
    /// 访问控制走默认 DACL：只有创建者本人和管理员能打开，其他登录用户不能，
    /// 与 Unix 侧把 socket 设成 0600 是一个意思——这条通道上会传密钥。
    pub fn bind(path: &Path) -> io::Result<Listener> {
        let name = pipe_name(path);
        let first = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)?;
        Ok(Listener {
            name,
            pending: std::sync::Mutex::new(Some(first)),
        })
    }

    pub async fn accept(listener: &Listener) -> io::Result<Stream> {
        let server = {
            let mut pending = listener
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match pending.take() {
                Some(server) => server,
                None => ServerOptions::new().create(&listener.name)?,
            }
        };
        server.connect().await?;
        // 立刻为下一个客户端准备好实例，避免两次 accept 之间出现“管道不存在”的窗口。
        if let Ok(next) = ServerOptions::new().create(&listener.name) {
            if let Ok(mut pending) = listener.pending.lock() {
                *pending = Some(next);
            }
        }
        Ok(Connection::Server(server))
    }

    /// 所有实例都在忙时的重试窗口。
    ///
    /// 服务端在两次 accept 之间有一个极短的空档（拿走等待中的实例、还没
    /// 创建下一个），此时客户端会收到 ERROR_PIPE_BUSY。重试是必要的，
    /// 但**必须有上限**：`is_listening` 没有外层超时，而它在守护进程绑定
    /// 之前就被调用，无限重试会让启动永久挂住。
    const PIPE_BUSY_RETRY_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);

    pub async fn connect(path: &Path) -> io::Result<Stream> {
        let name = pipe_name(path);
        let deadline = std::time::Instant::now() + PIPE_BUSY_RETRY_WINDOW;
        loop {
            match ClientOptions::new().open(&name) {
                Ok(client) => return Ok(Connection::Client(client)),
                // ERROR_PIPE_BUSY
                Err(error) if error.raw_os_error() == Some(231) => {
                    if std::time::Instant::now() >= deadline {
                        return Err(error);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub fn cleanup(_path: &Path) {}
}

pub use imp::{accept, bind, cleanup, connect, Listener, Stream};

/// socket 是否有人在听（能连上就算）。
pub async fn is_listening(path: &Path) -> bool {
    connect(path).await.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn read_line_handles_partial_and_multi_line_input() {
        let (mut client, server) = tokio::io::duplex(64);
        let mut reader = BufReader::new(server);
        tokio::spawn(async move {
            write_line(&mut client, "first").await.unwrap();
            client.write_all(b"second-no-newline").await.unwrap();
        });
        assert_eq!(
            read_line(&mut reader).await.unwrap().as_deref(),
            Some("first")
        );
        assert_eq!(
            read_line(&mut reader).await.unwrap().as_deref(),
            Some("second-no-newline")
        );
        assert!(read_line(&mut reader).await.unwrap().is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_socket_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("t.sock");
        let listener = bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let mut stream = accept(&listener).await.unwrap();
            let (read_half, mut write_half) = stream.split();
            let mut reader = BufReader::new(read_half);
            let line = read_line(&mut reader).await.unwrap().unwrap();
            write_line(&mut write_half, &format!("echo:{line}"))
                .await
                .unwrap();
        });
        let mut stream = connect(&path).await.unwrap();
        let (read_half, mut write_half) = stream.split();
        write_line(&mut write_half, "hi").await.unwrap();
        let mut reader = BufReader::new(read_half);
        assert_eq!(
            read_line(&mut reader).await.unwrap().as_deref(),
            Some("echo:hi")
        );
        server.await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn is_listening_reflects_whether_a_server_is_bound() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("probe.sock");
        assert!(!is_listening(&path).await);
        let listener = bind(&path).unwrap();
        assert!(is_listening(&path).await);
        drop(listener);
        cleanup(&path);
        assert!(!is_listening(&path).await);
    }
}

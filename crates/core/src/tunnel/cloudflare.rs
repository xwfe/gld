use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::oneshot;
use tokio::time;

use crate::error::{AppError, AppResult};
use crate::platform::platform;
use crate::settings::ProxyConfig;

const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// cloudflared 默认用 QUIC（UDP）连 Cloudflare 边缘。很多网络环境不放 UDP：
/// 公司防火墙、以及国内常见的透明代理（把域名映射到 198.18.x.x 的那类）。
/// 这时 cloudflared 会一直重试 QUIC，隧道永远连不上，但 `--url` 那行早就
/// 把公网地址打出来了——看着像起好了，实际访问是 Cloudflare 1033 错误页。
///
/// 换成 http2（走 TCP 443）就能穿过去，所以第一次连不上时自动退到它。
const FALLBACK_PROTOCOL: &str = "http2";

/// Handle to a supervised `cloudflared` child process.
pub struct CloudflareTunnelHandle {
    pub child: Child,
    pub public_url: String,
    pub pid: Option<u32>,
}

pub fn resolve_cloudflared() -> AppResult<PathBuf> {
    platform()
        .cloudflared_candidates()
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| {
            crate::tunnel::missing_binary(
                "cloudflared",
                "https://github.com/cloudflare/cloudflared/releases",
            )
        })
}

pub fn extract_trycloudflare_url(line: &str) -> Option<String> {
    const PREFIX: &str = "https://";
    const SUFFIX: &str = ".trycloudflare.com";
    let lower = line.to_ascii_lowercase();
    let mut search_from = 0;

    while let Some(rel) = lower[search_from..].find(PREFIX) {
        let start = search_from + rel;
        let Some(suffix_rel) = lower[start..].find(SUFFIX) else {
            break;
        };
        let end = start + suffix_rel + SUFFIX.len();
        let host = &line[start + PREFIX.len()..end - SUFFIX.len()];
        if host.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') && !host.is_empty() {
            return Some(line[start..end].trim_end_matches('/').to_string());
        }
        search_from = start + PREFIX.len();
    }
    None
}

/// Apply the global proxy to a tunnel child process environment.
pub(crate) fn apply_proxy_env(cmd: &mut Command, proxy: &ProxyConfig) {
    let url = match proxy.mode.as_str() {
        "manual" if !proxy.url.trim().is_empty() => Some(proxy.url.trim().to_string()),
        "system" => std::env::var("HTTPS_PROXY")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| std::env::var("HTTP_PROXY").ok().filter(|s| !s.is_empty()))
            .or_else(|| std::env::var("ALL_PROXY").ok().filter(|s| !s.is_empty())),
        _ => None,
    };
    if let Some(url) = url {
        for key in [
            "HTTPS_PROXY",
            "HTTP_PROXY",
            "https_proxy",
            "http_proxy",
            "ALL_PROXY",
            "all_proxy",
        ] {
            cmd.env(key, &url);
        }
        // Some cloudflared builds consult this dedicated variable.
        cmd.env("TUNNEL_HTTP_PROXY", &url);
    }
}

/// Spawn `cloudflared tunnel --url http://127.0.0.1:{port}` (quick) or named `tunnel run --token`.
///
/// 先按 cloudflared 的默认协议（QUIC）连；如果发现连不上边缘，
/// 自动杀掉重来一次并强制 http2，见 [`FALLBACK_PROTOCOL`]。
pub async fn spawn_cloudflare_tunnel(
    port: u16,
    cwd: &Path,
    log_path: &Path,
    cloudflare_mode: &str,
    cloudflare_token: &str,
    named_public_url: &str,
    use_proxy: bool,
) -> AppResult<CloudflareTunnelHandle> {
    let quick = cloudflare_mode != "named";

    if !quick {
        if cloudflare_token.trim().is_empty() {
            return Err(AppError::Message(
                "Cloudflare 命名隧道模式需要填写 Tunnel Token。".into(),
            ));
        }
        if named_public_url.trim().is_empty() {
            return Err(AppError::Message(
                "Cloudflare 命名隧道模式需要填写固定公网地址。".into(),
            ));
        }
    }

    match try_spawn(
        port,
        cwd,
        log_path,
        quick,
        cloudflare_token,
        named_public_url,
        use_proxy,
        None,
    )
    .await
    {
        Ok(handle) => Ok(handle),
        Err(SpawnFailure::EdgeUnreachable) => {
            append_tunnel_log(
                log_path,
                &format!(
                    "[gld] 连不上 Cloudflare 边缘（UDP/QUIC 多半被网络挡了），\
                     换 --protocol {FALLBACK_PROTOCOL} 重试。"
                ),
            )
            .await;
            try_spawn(
                port,
                cwd,
                log_path,
                quick,
                cloudflare_token,
                named_public_url,
                use_proxy,
                Some(FALLBACK_PROTOCOL),
            )
            .await
            .map_err(|failure| failure.into_error(log_path, port, true))
        }
        Err(failure) => Err(failure.into_error(log_path, port, false)),
    }
}

/// 一次启动尝试失败的原因。
enum SpawnFailure {
    /// 起不来（找不到 cloudflared、参数不对……）。
    Fatal(AppError),
    /// 进程活着，但连不上 Cloudflare 边缘——这种可以换协议重试。
    EdgeUnreachable,
    /// 到点了还没连上，日志里也没有明确的失败信号。
    Timeout,
}

impl SpawnFailure {
    fn into_error(self, log_path: &Path, port: u16, retried: bool) -> AppError {
        match self {
            SpawnFailure::Fatal(error) => error,
            SpawnFailure::EdgeUnreachable | SpawnFailure::Timeout => {
                let tried = if retried {
                    format!("（QUIC 和 {FALLBACK_PROTOCOL} 都试过了）")
                } else {
                    String::new()
                };
                AppError::Message(format!(
                    "cloudflared 起来了，但 {} 秒内没能连上 Cloudflare 边缘{tried}。\n\
                     依次排查：1) 本机 {port} 端口的 MCP 服务是否在跑（gld status）；\
                     2) 出网是否需要代理（gld settings proxy --mode manual --url http://127.0.0.1:7890）；\
                     3) 看日志 {log_hint}",
                    READY_TIMEOUT.as_secs(),
                    log_hint = log_path.display()
                ))
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn try_spawn(
    port: u16,
    cwd: &Path,
    log_path: &Path,
    quick: bool,
    cloudflare_token: &str,
    named_public_url: &str,
    use_proxy: bool,
    protocol: Option<&str>,
) -> Result<CloudflareTunnelHandle, SpawnFailure> {
    let cloudflared = resolve_cloudflared().map_err(SpawnFailure::Fatal)?;

    let mut cmd = Command::new(&cloudflared);
    cmd.current_dir(cwd);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    #[cfg(windows)]
    {
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }

    #[cfg(unix)]
    {
        cmd.process_group(0);
    }

    let settings = crate::settings::AppSettings::load_or_default();
    if use_proxy {
        apply_proxy_env(&mut cmd, &settings.proxy);
    }

    // `--protocol` 是 tunnel 这一层的参数，必须排在 run 子命令前面。
    cmd.arg("tunnel");
    if let Some(protocol) = protocol {
        cmd.args(["--protocol", protocol]);
    }
    if quick {
        cmd.args(["--url", &format!("http://127.0.0.1:{port}")]);
    } else {
        cmd.args(["run", "--token", cloudflare_token.trim()]);
    }

    let mut child = cmd.spawn().map_err(|err| {
        SpawnFailure::Fatal(AppError::Message(format!("启动 cloudflared 失败: {err}")))
    })?;
    let pid = child.id();

    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| SpawnFailure::Fatal(err.into()))?;
    }

    let (ready_tx, ready_rx) = oneshot::channel();
    let log_path_owned = log_path.to_path_buf();
    let named_url = named_public_url.trim_end_matches('/').to_string();

    if let Some(stdout) = child.stdout.take() {
        let stderr = child.stderr.take();
        tokio::spawn(async move {
            stream_cloudflare_output(stdout, stderr, &log_path_owned, quick, named_url, ready_tx)
                .await;
        });
    } else {
        let _ = ready_tx.send(TunnelSignal::Ready {
            public_url: if quick {
                None
            } else {
                Some(named_public_url.trim_end_matches('/').to_string())
            },
        });
    }

    let signal = match time::timeout(READY_TIMEOUT, ready_rx).await {
        Ok(Ok(signal)) => signal,
        // 输出流没了通常意味着进程自己退了，跟超时一样按"没连上"处理。
        Ok(Err(_)) | Err(_) => {
            kill_child(&mut child).await;
            return Err(SpawnFailure::Timeout);
        }
    };

    let public_url = match signal {
        TunnelSignal::EdgeUnreachable => {
            kill_child(&mut child).await;
            return Err(SpawnFailure::EdgeUnreachable);
        }
        TunnelSignal::Ready { public_url } => {
            if quick {
                match public_url {
                    Some(url) => url,
                    None => {
                        kill_child(&mut child).await;
                        return Err(SpawnFailure::Timeout);
                    }
                }
            } else {
                named_public_url.trim_end_matches('/').to_string()
            }
        }
    };

    Ok(CloudflareTunnelHandle {
        child,
        public_url,
        pid,
    })
}

async fn kill_child(child: &mut Child) {
    let _ = child.kill().await;
    let _ = time::timeout(Duration::from_secs(3), child.wait()).await;
}

async fn append_tunnel_log(log_path: &Path, line: &str) {
    if let Some(parent) = log_path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if let Ok(mut file) = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .await
    {
        let _ = file.write_all(format!("{line}\n").as_bytes()).await;
        let _ = file.flush().await;
    }
}

/// 从 cloudflared 的输出里读出来的结论。
enum TunnelSignal {
    /// 隧道真的连上了。quick 模式带上解析到的公网地址。
    Ready { public_url: Option<String> },
    /// 连不上 Cloudflare 边缘，换个协议还有救。
    EdgeUnreachable,
}

/// 连不上边缘的判定：连着失败这么多次就别等了。
///
/// 不用一次就下结论——第一次失败可能只是那个边缘 IP 不通，cloudflared 会换一个。
/// 连着两次都失败基本就是 UDP 整个被挡了。
const EDGE_FAILURES_BEFORE_FALLBACK: u32 = 2;

/// cloudflared 连不上边缘时打的那行。
fn is_edge_dial_failure(lowered: &str) -> bool {
    lowered.contains("failed to dial a quic connection")
        || lowered.contains("failed to dial to edge")
}

/// 隧道真正建立时打的那行。两种模式都有，named 模式一直就是靠它判断的。
fn is_tunnel_registered(lowered: &str) -> bool {
    lowered.contains("registered tunnel connection")
}

async fn stream_cloudflare_output<R, E>(
    stdout: R,
    stderr: Option<E>,
    log_path: &Path,
    quick: bool,
    named_url: String,
    ready_tx: oneshot::Sender<TunnelSignal>,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    E: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let mut ready_tx = Some(ready_tx);
    let mut state = OutputState::default();

    let mut log = match tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .await
    {
        Ok(file) => file,
        Err(_) => {
            // 日志开不了就没法判断了，按老样子放行，别把隧道卡死在这儿。
            if let Some(tx) = ready_tx.take() {
                let _ = tx.send(TunnelSignal::Ready {
                    public_url: if quick { None } else { Some(named_url) },
                });
            }
            return;
        }
    };

    // cloudflared logs primarily to stderr; read stdout and stderr concurrently.
    let (line_tx, mut line_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let stderr_line_tx = line_tx.clone();

    tokio::spawn(async move {
        let mut stdout = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = stdout.next_line().await {
            if line_tx.send(line).is_err() {
                break;
            }
        }
    });

    if let Some(stderr) = stderr {
        tokio::spawn(async move {
            let mut stderr = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = stderr.next_line().await {
                if stderr_line_tx.send(line).is_err() {
                    break;
                }
            }
        });
    }

    while let Some(line) = line_rx.recv().await {
        let _ = log.write_all(line.as_bytes()).await;
        let _ = log.write_all(b"\n").await;
        let _ = log.flush().await;
        if let Some(signal) = state.observe(&line, quick, &named_url) {
            if let Some(tx) = ready_tx.take() {
                let _ = tx.send(signal);
            }
        }
    }

    // 输出流结束（进程退了）。手上有什么就报什么，让调用方去判断。
    if let Some(tx) = ready_tx.take() {
        let _ = tx.send(TunnelSignal::Ready {
            public_url: state.public_url,
        });
    }
}

/// 逐行读 cloudflared 输出时攒下来的状态。
#[derive(Default)]
struct OutputState {
    public_url: Option<String>,
    edge_failures: u32,
}

impl OutputState {
    /// 吃一行日志，能下结论就返回结论。
    ///
    /// quick 模式为什么不能看到 URL 就返回：那行是 cloudflared **申请**隧道时打的，
    /// 这时还没连上边缘。UDP 被挡的网络里，URL 照打，隧道永远连不上，
    /// 用户拿到一个访问就是 Cloudflare 1033 错误页的地址。所以两种模式统一
    /// 等 `Registered tunnel connection`。
    fn observe(&mut self, line: &str, quick: bool, named_url: &str) -> Option<TunnelSignal> {
        let lowered = line.to_ascii_lowercase();

        if quick && self.public_url.is_none() {
            if let Some(url) = extract_trycloudflare_url(line) {
                self.public_url = Some(url);
            }
        }

        if is_edge_dial_failure(&lowered) {
            self.edge_failures += 1;
            if self.edge_failures >= EDGE_FAILURES_BEFORE_FALLBACK {
                return Some(TunnelSignal::EdgeUnreachable);
            }
            return None;
        }

        if is_tunnel_registered(&lowered) {
            return Some(TunnelSignal::Ready {
                public_url: if quick {
                    self.public_url.clone()
                } else {
                    Some(named_url.to_string())
                },
            });
        }

        None
    }
}

pub async fn stop_child(mut child: Child, pid: Option<u32>) -> AppResult<()> {
    if let Some(pid) = pid {
        let _ = platform().terminate_process_tree(pid);
    }

    let _ = child.kill().await;
    let _ = time::timeout(Duration::from_secs(3), child.wait()).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 喂一串日志行，返回第一个结论。
    fn observe_all(lines: &[&str], quick: bool) -> Option<TunnelSignal> {
        let mut state = OutputState::default();
        for line in lines {
            if let Some(signal) = state.observe(line, quick, "https://named.example.com") {
                return Some(signal);
            }
        }
        None
    }

    /// quick 模式必须等真正连上，不能看到地址就说好了。
    ///
    /// 那行地址是 cloudflared 申请隧道时打的，此时还没连上边缘。
    /// 之前就是在这儿返回的，于是 UDP 被挡的网络里，gld 报"隧道 running"
    /// 并给出地址，而那个地址访问是 Cloudflare 1033 错误页。
    #[test]
    fn quick_tunnel_is_not_ready_until_the_connection_registers() {
        let url_line = "INF |  https://abc-def.trycloudflare.com  |";
        assert!(
            observe_all(&[url_line], true).is_none(),
            "只看到地址还不能算就绪"
        );

        let signal = observe_all(
            &[
                url_line,
                "INF Registered tunnel connection connIndex=0 protocol=http2",
            ],
            true,
        );
        match signal {
            Some(TunnelSignal::Ready { public_url }) => {
                assert_eq!(
                    public_url.as_deref(),
                    Some("https://abc-def.trycloudflare.com")
                );
            }
            _ => panic!("注册之后应当就绪"),
        }
    }

    /// 连不上边缘要能识别出来，好换协议重试。
    #[test]
    fn repeated_edge_failures_report_unreachable() {
        let fail = "ERR Failed to dial a quic connection error=\"failed to dial to edge with quic: timeout\"";
        assert!(
            observe_all(&[fail], true).is_none(),
            "只失败一次可能只是那个边缘 IP 不通，cloudflared 会自己换一个"
        );
        assert!(
            matches!(
                observe_all(&[fail, fail], true),
                Some(TunnelSignal::EdgeUnreachable)
            ),
            "连着失败就该判定为连不上"
        );
    }

    /// named 模式一直靠这行判断，改造不能把它弄丢。
    #[test]
    fn named_tunnel_reports_its_configured_url() {
        let signal = observe_all(&["INF Registered tunnel connection connIndex=0"], false);
        match signal {
            Some(TunnelSignal::Ready { public_url }) => {
                assert_eq!(public_url.as_deref(), Some("https://named.example.com"));
            }
            _ => panic!("named 模式注册后应当就绪"),
        }
    }

    #[test]
    fn extracts_trycloudflare_url_from_log_line() {
        let line = "INF | https://abc-def.trycloudflare.com is your tunnel URL";
        assert_eq!(
            extract_trycloudflare_url(line).as_deref(),
            Some("https://abc-def.trycloudflare.com")
        );
    }

    #[test]
    fn ignores_invalid_hosts() {
        let line = "https://bad_host.trycloudflare.com";
        assert!(extract_trycloudflare_url(line).is_none());
    }
}

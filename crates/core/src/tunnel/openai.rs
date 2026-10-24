//! OpenAI 的 Secure MCP Tunnel：本机跑一个 `tunnel-client run`，它只往外连 OpenAI，
//! 把 ChatGPT 发来的请求取回来转给本机的 MCP 服务。没有公网地址，不要域名、不开端口。
//!
//! 下面用到的参数、环境变量和 `/readyz` 的含义对着 openai/tunnel-client v0.0.18 的源码核过
//! （`pkg/runtimeconfig/config.go`、`pkg/runtimehealth/health.go`、`pkg/proxy/route.go`）；
//! 没在本机跑过它，也没连过真的 OpenAI 控制面（审查 §70）。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tokio::process::{Child, Command};

use super::cloudflare::proxy_url;
use crate::error::{AppError, AppResult};
use crate::platform::platform;
use crate::settings::AppSettings;

/// 等 `/readyz` 回 200 的上限，和 cloudflared 等连上边缘的时间一样。
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// 密钥经这几个环境变量交给 tunnel-client，参数里只写 `env:<名字>`：argv 本机任何账号用
/// `ps` 都看得到。不用它自己的 `CONTROL_PLANE_API_KEY`：没设时它会退去读 `OPENAI_API_KEY`，
/// 守护进程环境里那份多半是你调 API 的 key，不是隧道的 Runtime Key。
const RUNTIME_KEY_ENV: &str = "GLD_OPENAI_RUNTIME_KEY";
const TUNNEL_KEY_ENV: &str = "GLD_OPENAI_TUNNEL_KEY";
const PROXY_ENV: &str = "GLD_OPENAI_TUNNEL_PROXY";

/// 起隧道要的东西。
pub struct OpenAiTunnelSpec<'a> {
    /// 服务的本地端口，tunnel-client 把请求转到 `http://127.0.0.1:<port>/mcp`。
    pub port: u16,
    pub tunnel_id: &'a str,
    /// Platform 上建的 Runtime Key（要 Tunnels Read + Use），tunnel-client 拿它连控制面。
    pub runtime_key: &'a str,
    /// 服务凭据 `openai_tunnel_key`，转发时放进 [`crate::hub::OPENAI_TUNNEL_HEADER`]。
    pub tunnel_key: &'a str,
    pub log_path: &'a Path,
    /// 连 OpenAI 那一段走不走全局出站代理。
    pub use_proxy: bool,
}

pub struct OpenAiTunnelHandle {
    pub child: Child,
    pub pid: Option<u32>,
}

/// `tunnel_` 后面一串小写字母、数字或下划线。
///
/// 官方文档说是 `tunnel_` 加 32 位小写字母数字，另有带命名空间的写法没给格式，
/// 所以只拦明显贴错的（贴成了 Runtime Key、带了空格），细的交给 OpenAI 判。
pub fn is_tunnel_id(value: &str) -> bool {
    value.strip_prefix("tunnel_").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    })
}

/// 找 tunnel-client。Homebrew 装的叫 `tunnel-client`，发布页 zip 里的叫 `tunnel-client-runtime`，
/// 两个的 `run` 参数一样。守护进程由 launchd / systemd 拉起时 PATH 很短，所以再看几个常见目录。
pub fn resolve_tunnel_client() -> AppResult<PathBuf> {
    const NAMES: [&str; 2] = ["tunnel-client", "tunnel-client-runtime"];
    let mut dirs: Vec<PathBuf> = Vec::new();
    if cfg!(unix) {
        dirs.push("/opt/homebrew/bin".into());
        dirs.push("/usr/local/bin".into());
        if let Some(home) = dirs::home_dir() {
            dirs.push(home.join(".local/bin"));
        }
    }
    NAMES
        .iter()
        .find_map(|name| platform().resolve_executable(name))
        .or_else(|| {
            dirs.iter()
                .flat_map(|dir| NAMES.iter().map(move |name| dir.join(name)))
                .find(|path| path.is_file())
        })
        .ok_or_else(|| {
            crate::tunnel::missing_binary(
                "tunnel-client",
                "https://github.com/openai/tunnel-client/releases",
            )
        })
}

/// 起 `tunnel-client run`，等它报就绪（`/readyz` 回 200：它用这把请求头连得上本机服务了）。
///
/// 调用方要先把服务的监听器起好：tunnel-client 起来就拿 `initialize` 探一次服务，
/// 探不通 `/readyz` 一直是 503。
pub async fn spawn_openai_tunnel(spec: OpenAiTunnelSpec<'_>) -> AppResult<OpenAiTunnelHandle> {
    if !is_tunnel_id(spec.tunnel_id) {
        return Err(AppError::Message(format!(
            "OpenAI 隧道 ID 不对：「{}」。它长这样：tunnel_0123…（Platform → Tunnels 里复制）",
            spec.tunnel_id
        )));
    }
    if spec.runtime_key.trim().is_empty() {
        return Err(AppError::Message(
            "还没有 OpenAI Runtime Key：gld secret set openai_runtime_key <key>".into(),
        ));
    }
    let binary = resolve_tunnel_client()?;
    let health_port = free_loopback_port()?;
    if let Some(parent) = spec.log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(spec.log_path)?;

    let header = format!("X-Gld-Tunnel-Key: env:{TUNNEL_KEY_ENV}");
    let mut cmd = Command::new(&binary);
    cmd.arg("run")
        .args(["--control-plane.tunnel-id", spec.tunnel_id])
        .args(["--control-plane.api-key", &format!("env:{RUNTIME_KEY_ENV}")])
        .args([
            "--mcp.server-url",
            &format!("http://127.0.0.1:{}/mcp", spec.port),
        ])
        // 启动探测和 OAuth 发现用 discovery 那一份（给了就盖过 extra-headers），两份给同一个值。
        .args(["--mcp.extra-headers", &header])
        .args(["--mcp.discovery-extra-headers", &header])
        // 默认 127.0.0.1:8080，和本机别的服务撞的机会太大。
        .args(["--health.listen-addr", &format!("127.0.0.1:{health_port}")])
        .env(RUNTIME_KEY_ENV, spec.runtime_key.trim())
        .env(TUNNEL_KEY_ENV, spec.tunnel_key)
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    // 它按 HTTP(S)_PROXY 走代理时不会自己绕开回环地址（`pkg/proxy/route.go` 只认 NO_PROXY），
    // 不补上的话，转给本机服务的请求会被送到代理那里去。
    let no_proxy = match std::env::var("NO_PROXY").or_else(|_| std::env::var("no_proxy")) {
        Ok(existing) if !existing.trim().is_empty() => format!("{existing},127.0.0.1,localhost"),
        _ => "127.0.0.1,localhost".to_string(),
    };
    cmd.env("NO_PROXY", &no_proxy).env("no_proxy", &no_proxy);
    if spec.use_proxy {
        if let Some(url) = proxy_url(&AppSettings::load_or_default().proxy) {
            cmd.env(PROXY_ENV, url)
                .args(["--control-plane.http-proxy", &format!("env:{PROXY_ENV}")]);
        }
    }
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

    let mut child = cmd
        .spawn()
        .map_err(|err| AppError::Message(format!("启动 {} 失败：{err}", binary.display())))?;
    let pid = child.id();
    if let Err(reason) = wait_until_ready(&mut child, health_port).await {
        let _ = super::cloudflare::stop_child(child, pid).await;
        return Err(AppError::Message(format!(
            "tunnel-client 没起来：{reason}\n{}看完整日志：{}",
            log_tail(spec.log_path),
            spec.log_path.display()
        )));
    }
    Ok(OpenAiTunnelHandle { child, pid })
}

/// 问一个系统现分的空闲端口给 tunnel-client 的健康检查用。放掉之后到它 bind 之间被别人
/// 抢走的话，它起不来、立刻退出，下面会照实报出来。
fn free_loopback_port() -> AppResult<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

async fn wait_until_ready(child: &mut Child, health_port: u16) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|err| err.to_string())?;
    let url = format!("http://127.0.0.1:{health_port}/readyz");
    let deadline = Instant::now() + READY_TIMEOUT;
    let mut last = "健康检查端口一直没应答".to_string();
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("进程自己退了（{status}）"));
        }
        if let Ok(response) = client.get(&url).send().await {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            // 探测拿到 401 时它也回 200，正文写 "ready (mcp initialize requires auth: …)"：
            // 那是服务没认出这把请求头，ChatGPT 进来也是 401，不能算起好了。
            if body.contains("requires auth") {
                return Err(format!(
                    "服务没认出 tunnel-client 带的请求头（/readyz：{}）。gld restart 重起一次；还不行看 gld logs",
                    body.trim()
                ));
            }
            if status.is_success() {
                return Ok(());
            }
            last = format!("{} {}", status.as_u16(), body.trim());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{} 秒内没就绪，/readyz 最后回的是「{last}」",
                READY_TIMEOUT.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// 日志最后几行，起不来时直接附在报错里：多半是隧道 ID、Runtime Key 或权限不对，
/// 原话在它自己的日志里，省得用户再去翻。
fn log_tail(path: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let tail = &lines[lines.len().saturating_sub(5)..];
    if tail.is_empty() {
        return String::new();
    }
    let mut joined = tail.join("\n");
    if joined.len() > 1200 {
        let mut cut = joined.len() - 1200;
        while !joined.is_char_boundary(cut) {
            cut += 1;
        }
        joined = joined[cut..].to_string();
    }
    format!("它的日志最后几行：\n{joined}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnel_ids_from_the_docs_pass_and_pasted_junk_does_not() {
        assert!(is_tunnel_id("tunnel_0123456789abcdef0123456789abcdef"));
        assert!(is_tunnel_id("tunnel_ab12_0123456789abcdef"));
        for bad in [
            "",
            "tunnel_",
            "sk-proj-abc",
            "Tunnel_abc",
            "tunnel_ABC",
            "tunnel_abc def",
            " tunnel_abc",
        ] {
            assert!(!is_tunnel_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn the_log_tail_keeps_the_last_lines_on_a_char_boundary() {
        let dir = crate::home::test_scratch_dir("openai-tunnel-log");
        let path = dir.join("tunnel-client.log");
        let mut text = String::new();
        for i in 0..20 {
            text.push_str(&format!("第 {i} 行 {}\n", "隧".repeat(100)));
        }
        std::fs::write(&path, text).unwrap();
        let tail = log_tail(&path);
        assert!(tail.contains("第 19 行"), "{tail}");
        assert!(!tail.contains("第 10 行"), "{tail}");
        assert!(log_tail(&dir.join("missing.log")).is_empty());
    }
}

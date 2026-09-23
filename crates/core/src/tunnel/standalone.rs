//! 不属于任何工作区的隧道：全局入口一条、服务（hub）一条。
//!
//! 工作区的隧道归 [`super::TunnelSupervisor`] 管：按（工作区 id, 服务）分，FRP 还要把
//! 同一台 frps 上的几条线路合进一个 frpc。这两条是进程级单例、各自只有一条线路，
//! 跟着各自的监听器一起起停，放进那张表只会让"谁拥有这个进程"说不清。

use std::path::Path;

use tokio::process::Child;

use super::{cloudflare, frp, TunnelServiceKind};
use crate::error::{AppError, AppResult};
use crate::settings::AppSettings;
use crate::workspace::WorkspaceProfile;

/// 一条跑着的隧道进程（cloudflared 或 frpc）。
pub struct StandaloneTunnel {
    child: Child,
    pid: Option<u32>,
}

impl StandaloneTunnel {
    /// 结束整棵进程树并等它退出（最多几秒）。
    pub async fn stop(self) {
        let _ = cloudflare::stop_child(self.child, self.pid).await;
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// 这个进程还在不在。cloudflared 自己退了（token 失效、被 kill、边缘断了
    /// 之后放弃重连）时，监听器照常跑着，只有公网那一头没了——体检要说得出
    /// 这件事，否则用户看到的是"配置全对，ChatGPT 就是连不上"。
    ///
    /// `try_wait` 不阻塞：还在跑是 `Ok(None)`。拿不准（拿不到状态）算它还在，
    /// 免得把正常的隧道报成挂了。
    pub fn still_running(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(Some(_)))
    }
}

/// Cloudflare 的两种写法共用这一份参数。
pub struct CloudflareSpec<'a> {
    pub port: u16,
    /// 日志写到数据目录下的这个相对路径（目录不在就建）。
    pub log_name: &'a str,
    /// quick | named
    pub mode: &'a str,
    /// named 才要。
    pub token: &'a str,
    /// named 才要：对外的固定地址。
    pub public_url: &'a str,
    pub use_proxy: bool,
}

/// 起一条 Cloudflare 隧道，返回公网基地址（不带 `/mcp`）和进程。
///
/// quick 的地址要等 cloudflared 连上边缘才知道，所以调用方得先起隧道、
/// 拿到地址，再用它起监听器——OAuth 元数据里要写这个地址。
pub async fn start_cloudflare(spec: CloudflareSpec<'_>) -> AppResult<(String, StandaloneTunnel)> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let log = crate::home::data_home()?.join(spec.log_name);
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let handle = cloudflare::spawn_cloudflare_tunnel(
        spec.port,
        &cwd,
        &log,
        spec.mode,
        spec.token,
        spec.public_url,
        spec.use_proxy,
    )
    .await?;
    Ok((
        handle.public_url.trim_end_matches('/').to_string(),
        StandaloneTunnel {
            child: handle.child,
            pid: handle.pid,
        },
    ))
}

/// FRP 这一边要的东西。
pub struct FrpSpec<'a> {
    /// frpc 配置文件和日志按它分，两条单例隧道不能撞名。
    pub name: &'a str,
    pub port: u16,
    pub frp_profile_id: &'a str,
    /// 老配置里没引用 FRP 配置、直接写了服务器的；新配置留空即可。
    pub frp_server: &'a str,
    pub frp_server_port: u16,
    pub subdomain: &'a str,
    pub use_proxy: bool,
}

/// 起一个只有这一条线路的 frpc，返回公网基地址和进程。
pub async fn start_frp(
    spec: FrpSpec<'_>,
    settings: &AppSettings,
) -> AppResult<(String, StandaloneTunnel)> {
    if spec.subdomain.trim().is_empty() {
        return Err(AppError::Message(format!(
            "{} 的 FRP 子域名是空的。",
            spec.name
        )));
    }
    let profile = frp_profile(&spec);
    let handle =
        frp::spawn_frpc(spec.name, &[(&profile, TunnelServiceKind::Mcp)], settings).await?;
    Ok((
        frp::frp_public_url(&profile, TunnelServiceKind::Mcp, settings),
        StandaloneTunnel {
            child: handle.child,
            pid: handle.pid,
        },
    ))
}

/// 不起进程、只算地址：FRP 的公网地址由 frps 域名和子域名决定，不用等隧道。
/// 服务停着的时候 `gld ls` 也要能说出"起来之后是哪个地址"。
pub fn frp_public_base(spec: &FrpSpec<'_>, settings: &AppSettings) -> String {
    frp::frp_public_url(&frp_profile(spec), TunnelServiceKind::Mcp, settings)
}

/// frp 那边的代码是按工作区写的，这里拼一个只填了隧道字段的工作区给它——
/// 全局入口一直是这么用的。
fn frp_profile(spec: &FrpSpec<'_>) -> WorkspaceProfile {
    let cwd = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let mut profile = WorkspaceProfile::new(cwd.display().to_string(), Some(spec.name.into()));
    profile.id = spec.name.into();
    profile.runtime.local_port = spec.port;
    profile.tunnel.tunnel_type = "frp".into();
    profile.tunnel.frp_profile_id = spec.frp_profile_id.into();
    profile.tunnel.frp_server = spec.frp_server.into();
    profile.tunnel.frp_server_port = spec.frp_server_port;
    profile.tunnel.frp_subdomain = spec.subdomain.into();
    profile.tunnel.use_proxy = spec.use_proxy;
    profile
}

mod access;
pub(crate) mod cloudflare;
pub(crate) mod frp;
mod supervisor;

use crate::error::AppError;
use crate::settings::AppSettings;
use crate::workspace::WorkspaceProfile;

/// 找不到隧道二进制时的报错。
///
/// gld 不自己下载这两个东西了（以前会往 `~/.gld/bin` 下，frpc 甚至是在你
/// `tunnel start` 的时候偷偷去 GitHub 拉一份）。理由：多一套镜像 / 代理 /
/// 版本管理的配置要维护，而 brew / winget / apt 早就做得比这好；
/// 用户还得先知道有 `gld software` 这么个命令才用得上。
///
/// 现在只认系统里已经装好的——PATH 里有就行，装在哪、什么版本都不管。
///
/// 报错写成一处，免得 cloudflared 和 frpc 两边各写一版，改了一边忘另一边。
pub(crate) fn missing_binary(binary: &str, download_page: &str) -> AppError {
    let install = match binary {
        "cloudflared" => {
            "  macOS     brew install cloudflared\n  \
             Windows   winget install Cloudflare.cloudflared\n  \
             Linux     见下面的发布页（下载后 chmod +x，放进 PATH 里的目录）"
        }
        _ => {
            "  macOS     brew install frpc\n  \
             Windows / Linux   见下面的发布页（解压后把 frpc 放进 PATH 里的目录）"
        }
    };
    AppError::Message(format!(
        "未找到 {binary}。装好之后 gld 会自动认出来——只要它在 PATH 里，\
         装在哪、哪个版本都不用告诉 gld。\n{install}\n  发布页    {download_page}\n\
         装完直接重试，不需要改 gld 的任何配置。`gld doctor` 也会告诉你找没找到。"
    ))
}

pub use access::{
    cleanup_orphan_for_runtime, drop_workspace, ensure_frp_health_loop, maybe_start_for_runtime,
    stop_for_runtime, supervisor, sync_managed_runtime_routes,
};

#[allow(unused_imports)]
pub use cloudflare::{
    extract_trycloudflare_url, resolve_cloudflared, spawn_cloudflare_tunnel, stop_child,
};
#[allow(unused_imports)]
pub use frp::{actions_frp_snippet, mcp_frp_snippet};
#[allow(unused_imports)]
pub use supervisor::{TunnelServiceKind, TunnelStatus, TunnelSupervisor};

/// 两个隧道二进制各自的位置（找到了给路径，没找到给 None）。
///
/// `gld doctor` 用它报告"要用 frp 隧道但机器上没有 frpc"。
pub fn tunnel_binary_path(kind: &str) -> Option<String> {
    let found = match kind {
        "frpc" => frp::resolve_frpc().ok(),
        "cloudflared" => cloudflare::resolve_cloudflared().ok(),
        _ => None,
    };
    found.map(|path| path.to_string_lossy().into_owned())
}

pub fn frp_snippet(profile: &WorkspaceProfile, kind: TunnelServiceKind, reveal: bool) -> String {
    let settings = AppSettings::load_or_default();
    frp::frp_snippet(profile, kind, &settings, reveal)
}

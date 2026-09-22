//! 这台机器上装好的 MCP server，经 gld 的服务转给连上来的 AI（RFC-0006，
//! 跨仓 v4 方案第 2 步）。
//!
//! ChatGPT 这类 Web AI 只能连一个公网 MCP 地址，用不上本机 Claude Code、Codex
//! 里装的 context7、deepwiki。gld 本来就是那个地址，这里让它顺带把这些 server
//! 转过去。
//!
//! | 模块 | 管什么 |
//! | --- | --- |
//! | [`installed`] | 读 `~/.claude.json` 和 `~/.codex/config.toml` 里装了哪些 |
//! | [`transport`]、[`http`] | stdio 子进程和 streamable HTTP 两种通道 |
//! | [`client`] | 握手、列工具、调工具 |
//! | [`pool`] | 连接：用到才开、按 server + 调用方分、闲了收 |
//! | [`relay`] | 模型看到的三个工具，和结果怎么交出去 |
//!
//! **默认一个都不开**，操作员按名字开（`gld mcp on context7`），名单存在
//! `AppSettings::relayed_mcp_servers`。原因：gld 的服务可能挂在公网上，而装好的
//! server 里有能读写整个主目录的（Filesystem、desktop-commander）。能不能
//! 按"只走网络"自动放行？判断不了——context7 在这台机器上就是 `npx` 起的本机
//! 进程，跟 Filesystem 在配置里长得一样。
//!
//! 不要这个功能了：删掉这个目录，再删 `hub` 里 `machine_mcp` 那几处（列工具、
//! 分发、关服务）、`relayed_mcp_servers` 这个设置、`gld mcp` 命令组和它的三个
//! 守护进程请求。别的模块不依赖这里。

pub mod client;
pub mod http;
pub mod installed;
pub mod pool;
pub mod relay;
pub mod transport;

use std::path::PathBuf;

pub use relay::{is_tool, Relay, Scope};

/// 按这台机器的主目录和 `$CODEX_HOME` 读装好的 server。
pub fn read_installed() -> installed::Installed {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let codex_home = std::env::var_os("CODEX_HOME").map(PathBuf::from);
    installed::read(
        &installed::Places::new(&home, codex_home.as_deref()),
        &|name| std::env::var(name).ok(),
    )
}

/// 起 server 用的 `PATH` 和工作目录：`PATH` 是 gld 的全局可执行文件路径加上
/// 这个进程自己的，和 `exec_command` 同一个口径；工作目录是主目录。连 HTTP
/// server 照 gld 的全局出站代理。
pub fn launch(settings: &crate::settings::AppSettings) -> pool::Launch {
    let mut paths =
        crate::tools::context::merge_executable_paths("", &settings.global_executable_paths);
    if let Some(system) = std::env::var_os("PATH") {
        for path in std::env::split_paths(&system) {
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    pool::Launch {
        path: std::env::join_paths(paths).ok(),
        cwd: dirs::home_dir().unwrap_or_else(std::env::temp_dir),
        proxy: http::Proxy::from_settings(&settings.proxy),
    }
}

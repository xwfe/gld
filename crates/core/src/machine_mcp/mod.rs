//! 这台机器上装好的 MCP server，经 gld 的服务转给连上来的 AI（RFC-0006，
//! 跨仓 v4 方案第 2 步）。
//!
//! ChatGPT 这类 Web AI 只能连一个公网 MCP 地址，用不上本机 Claude Code、Codex
//! 里装的 context7、deepwiki。gld 本来就是那个地址，这里让它顺带把这些 server
//! 转过去。
//!
//! | 在哪 | 管什么 |
//! | --- | --- |
//! | 共享库 `toexec-mcp`（这里按原名转出来：[`installed`]、[`client`]、[`pool`]、[`shape`]、[`transport`]） | 读 `~/.claude.json` 和 `~/.codex/config.toml` 里装了哪些、握手调用、子进程通道、连接池、结果整理。ccnm 转它那台机器上的 server 也用这一份 |
//! | [`open`] | gld 自己怎么起进程（`PATH`、工作目录、进程组）和怎么杀 |
//! | [`http`] | streamable HTTP 通道（要 HTTP 客户端和异步运行时，共享库不背） |
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
//! 守护进程请求，以及对 `toexec-mcp` 的依赖。别的模块不依赖这里。

pub mod http;
pub mod open;
pub mod relay;

pub use toexec_mcp::{client, installed, pool, shape, transport};

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
pub fn launch(settings: &crate::settings::AppSettings) -> open::Launch {
    let mut paths =
        crate::tools::context::merge_executable_paths("", &settings.global_executable_paths);
    if let Some(system) = std::env::var_os("PATH") {
        for path in std::env::split_paths(&system) {
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    open::Launch {
        path: std::env::join_paths(paths).ok(),
        cwd: dirs::home_dir().unwrap_or_else(std::env::temp_dir),
        proxy: http::Proxy::from_settings(&settings.proxy),
    }
}

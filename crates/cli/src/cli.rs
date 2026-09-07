//! clap 定义。帮助文本是给第一次接触的人看的：每个子命令都说明它做什么、
//! 什么时候用、常见的下一步是什么。

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

const AFTER_HELP: &str = "\
快速上手：
  gld start ~/code/my-project             启动 MCP；目录没登记过会自动登记（守护进程自动在后台拉起）
  gld start                               同上，作用于当前目录
  gld list                                看地址、凭据与隧道；不指定工作区时列出全部
  gld share                               要接 ChatGPT 时用：一条命令拿到公网 HTTPS 地址
  gld upgrade --tunnel https://x.com/mcp  改目录 / 公网入口 / 端口 / 认证，改完自动重启
  gld stop                                停止服务（--all 停所有工作区的）；配置不动
  gld destroy                             销毁工作区：连配置和密钥一起删（项目文件不动）

公网入口（--tunnel 在 start / share / upgrade 里通用）：
  --tunnel https://mcp.example.com/mcp    已有公网地址（自建反代等），只登记不起隧道
  --tunnel cf                             Cloudflare 临时地址，零配置，重启会变
  --tunnel cf:mcp.example.com             Cloudflare 固定域名，要 Tunnel Token（没配过会当场问）
  --tunnel cf:mcp.example.com --token <token>
                                          同上，token 直接写在命令里（会进 shell 历史）
  --tunnel frp:公司                       FRP 固定域名，子域名默认取工作区名
  --tunnel off                            关掉公网入口，只留本地地址

工作区定位：
  大多数命令接受 -w/--workspace <id|id前缀|名称|路径>。不给时按当前目录归属推断；
  只有一个工作区时直接使用它。

数据目录：
  默认 ~/.config/gld，可用 --home 或环境变量 GLD_HOME 覆盖。里面有配置、密钥、日志，
  以及守护进程的 socket 与 pid 文件。frpc / cloudflared 由你自己安装，gld 从 PATH 里找。

更多：docs/cli.md（完整命令参考）、docs/daemon.md（后台进程说明）";

#[derive(Debug, Parser)]
#[command(
    name = "gld",
    version,
    about = "把本地项目变成 AI 可通过 MCP / GPT Actions 直接开发的工作区，服务常驻后台",
    long_about = "gld 管理一组“工作区”（本地项目目录），为每个工作区提供 MCP Streamable HTTP \
服务和可选的 GPT Actions OpenAPI 网关，并能通过 FRP / Cloudflare 隧道暴露到公网。\n\n\
服务运行在一个后台守护进程里：第一次执行 gld start 时自动拉起，之后关闭终端也不受影响；\
gld daemon status 可以随时查看它是否在跑。",
    after_help = AFTER_HELP,
    propagate_version = true,
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalOpts,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Args)]
pub struct GlobalOpts {
    /// 目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断
    #[arg(
        short = 'w',
        long,
        global = true,
        env = "GLD_WORKSPACE",
        value_name = "WS"
    )]
    pub workspace: Option<String>,

    /// 以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
    #[arg(long, global = true)]
    pub json: bool,

    /// 守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
    #[arg(long, global = true)]
    pub no_autostart: bool,

    /// 等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
    #[arg(long, global = true, value_name = "SECS")]
    pub timeout: Option<u64>,

    /// 数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld）
    #[arg(long, global = true, env = "GLD_HOME", value_name = "DIR")]
    pub home: Option<PathBuf>,

    /// 关闭彩色输出（也可设置环境变量 NO_COLOR）
    #[arg(long, global = true)]
    pub no_color: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// 管理后台守护进程（启动 / 停止 / 状态 / 日志）
    #[command(subcommand)]
    Daemon(DaemonCmd),

    /// 管理工作区（登记项目目录、查看、修改配置、删除）
    #[command(subcommand, visible_alias = "ws")]
    Workspace(WorkspaceCmd),

    /// 启动 MCP（默认）或 Actions 服务；目录没登记过会自动登记为工作区
    ///
    ///   gld start                          当前目录
    ///   gld start ~/code/api               指定目录
    ///   gld start ~/code/api --tunnel https://mcp.example.com/mcp
    ///                                      顺带配好公网入口，起完直接打印连接信息
    ///
    /// 守护进程没在跑会自动拉起，之后关掉终端服务也照常在。
    #[command(verbatim_doc_comment)]
    Start(StartArgs),

    /// 停止服务（默认停当前工作区的全部服务）
    ///
    ///   gld stop              当前工作区的 MCP 和 Actions
    ///   gld stop -s mcp       只停 MCP
    ///   gld stop --all        所有工作区的所有服务（守护进程留着，下次 start 照常用）
    ///
    /// 配置和密钥都不动；连守护进程一起退出用 gld daemon stop。
    #[command(verbatim_doc_comment)]
    Stop(StopArgs),

    /// 重启工作区的服务（默认全部）
    ///
    /// 改端口 / 认证 / 密钥不需要它——ws set 和 secret set 会自己重启受影响的服务。
    /// 用得上它的场景：改了全局设置（gld settings runtime），或服务卡住了想踢一脚。
    #[command(verbatim_doc_comment)]
    Restart(ServiceArgs),

    /// 查看服务与隧道状态：不带工作区时列出全部，带工作区时显示详情
    Status,

    /// 只列出正在运行的服务
    Ps,

    /// 查看工作区日志尾部，或用 -f 持续跟随
    Logs(LogsArgs),

    /// 列出工作区的连接信息：地址、认证方式、凭据、隧道
    ///
    ///   gld list                不指定工作区时列出全部；在工作区目录里则显示这一个的详情
    ///   gld list -w api         看指定工作区的详情
    ///   gld list --all          在工作区目录里也强制列出全部
    ///   gld list --reveal       凭据显示明文（默认脱敏）
    ///
    /// 敲惯了 ls 的话，gld ls 是同一条命令。
    #[command(verbatim_doc_comment, visible_alias = "ls")]
    List(ListArgs),

    /// 一条命令拿到公网 HTTPS 地址（ChatGPT 只能连公网，127.0.0.1 填进去连不上）
    ///
    /// 它把「配隧道 → 启动服务 → 查连接信息」三步合成一步：
    ///   gld share                             Cloudflare 临时地址（等价 --tunnel cf）
    ///   gld share --tunnel cf:mcp.example.com Cloudflare 固定域名，要 Tunnel Token（没配过会当场问）
    ///   gld share --tunnel frp:公司           FRP 固定域名，子域名默认取工作区名
    ///   gld share --tunnel https://x.com/mcp  已经有公网地址（自建反代等），只登记不起隧道
    ///   gld share --off                       关掉公网入口，只留本地地址
    ///
    /// 公网入口意味着"在你电脑上跑命令"这件事对外可达，开之前请读 docs/security.md。
    #[command(verbatim_doc_comment)]
    Share(ShareArgs),

    /// 销毁工作区：停掉服务与隧道，删掉它的配置和密钥（项目文件一个字节都不动）
    ///
    ///   gld destroy              当前目录对应的工作区
    ///   gld destroy api          按名称 / 路径 / id 指定
    ///   gld destroy --all        全部工作区
    ///   gld destroy -y           不询问
    ///
    /// 只是想停服务用 gld stop——那个不删任何东西。
    /// 密钥删了就没了，客户端里存的 token / 口令会全部失效。
    #[command(verbatim_doc_comment)]
    Destroy(DestroyArgs),

    /// 改工作区配置（目录 / 公网入口 / 端口 / 认证 / 名称），改完自动重启服务
    ///
    ///   gld upgrade --tunnel https://new.example.com/mcp   换公网地址
    ///   gld upgrade --path ~/code/api-v2                   项目搬了目录
    ///   gld upgrade api --port 30001 --auth bearer         按名称指定工作区
    ///   gld upgrade --off                                  关掉公网入口
    ///
    /// 只改这几项常用配置；全部字段见 gld workspace fields 与 gld workspace set。
    #[command(verbatim_doc_comment)]
    Upgrade(UpgradeArgs),

    /// 逐项检查本地 / 公网端点与 OAuth 元数据是否可达
    Health,

    /// 体检：检查配置是否自洽，并给出每个问题的修复命令
    Doctor,

    /// 直接调用工具内核：不接 AI 客户端也能验证 Agent 会看到什么
    #[command(subcommand)]
    Tool(ToolCmd),

    /// 管理公网隧道（FRP / Cloudflare）
    #[command(subcommand)]
    Tunnel(TunnelCmd),

    /// 管理全局共享公网入口（多个工作区共用一个域名，按 /w/<id> 路由）
    #[command(subcommand)]
    Gateway(GatewayCmd),

    /// 查看 / 设置 / 重新生成密钥（Bearer Token、OAuth 口令、Actions API Key…）
    #[command(subcommand)]
    Secret(SecretCmd),

    /// 管理 FRP 服务器配置（多个工作区可复用同一台 frps）
    #[command(subcommand)]
    Frp(FrpCmd),

    /// 全局设置：出站代理、局域网访问、启动时恢复、全局 Agent 说明
    #[command(subcommand)]
    Settings(SettingsCmd),

    /// Goal / Plan 规划状态与人工验收
    #[command(subcommand)]
    Planning(PlanningCmd),

    /// 列出工作区的历史会话档案（docs/history-session）
    History,

    /// 查看本次守护进程运行期间的请求次数与 Token 估算
    Usage,

    /// 查看会注入给 Agent 的说明文件与 Skill（--global 看用户级来源）
    Context(ContextArgs),

    /// 生成 shell 补全脚本
    Completions {
        /// bash | zsh | fish | powershell | elvish
        shell: clap_complete::Shell,
    },
}

// ---------------------------------------------------------------- daemon

#[derive(Debug, Subcommand)]
pub enum DaemonCmd {
    /// 在后台启动守护进程（已在运行则什么都不做）
    Start,
    /// 请求守护进程退出，并等待它停掉所有服务
    Stop {
        /// 超时后强制结束进程树
        #[arg(long)]
        force: bool,
        /// 等待退出的秒数
        #[arg(long, default_value_t = 20, value_name = "SECS")]
        wait: u64,
    },
    /// 停止后重新启动（升级二进制后用它）
    Restart {
        #[arg(long)]
        force: bool,
    },
    /// 显示守护进程是否在运行、pid、运行时长、日志位置
    Status,
    /// 在前台运行守护进程（给 systemd / launchd 或排障用；Ctrl-C 优雅退出）
    Run {
        /// 不恢复上次运行的服务
        #[arg(long)]
        no_restore: bool,
    },
    /// 查看守护进程自身日志
    Logs {
        /// 显示最后 N 行
        #[arg(short = 'n', long, default_value_t = 50)]
        lines: usize,
        /// 持续跟随
        #[arg(short = 'f', long)]
        follow: bool,
    },
}

// ------------------------------------------------------------- workspace

#[derive(Debug, Subcommand)]
pub enum WorkspaceCmd {
    /// 把一个项目目录登记为工作区（自动分配空闲端口并生成密钥）
    Add {
        /// 项目根目录（默认当前目录）
        #[arg(value_name = "PATH", default_value = ".")]
        path: PathBuf,
        /// 显示名称（默认目录名）
        #[arg(long)]
        name: Option<String>,
        /// MCP 端口（默认从 28766 起找空闲）
        #[arg(long, value_name = "PORT")]
        mcp_port: Option<u16>,
        /// Actions 端口（默认从 8787 起找空闲）
        #[arg(long, value_name = "PORT")]
        actions_port: Option<u16>,
    },
    /// 列出所有工作区
    #[command(visible_alias = "ls")]
    List,
    /// 显示一个工作区的完整配置
    Show,
    /// 删除工作区（会先停掉它的服务与隧道；不会动项目目录本身）
    #[command(visible_alias = "rm")]
    Remove {
        /// 不询问，直接删除
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// 修改配置字段：gld ws set port=30000 auth=bearer（字段见 gld ws fields）
    ///
    /// 不写前缀就是改 MCP：port 等价于 mcp.port。改 Actions 那条线路要写全
    /// actions.port。改完会自动重启受影响且正在运行的服务，不用再敲 gld restart。
    Set {
        /// KEY=VALUE，可多个
        #[arg(value_name = "KEY=VALUE", required = true)]
        assignments: Vec<String>,
    },
    /// 列出 set 支持的字段及取值（默认只列 MCP 侧，--all 连 Actions 一起列）
    Fields {
        /// 连 actions.* 一起列出
        #[arg(long)]
        all: bool,
    },
    /// 记住一个“最近使用”的工作区（供脚本或习惯用）
    Use,
}

// --------------------------------------------------------------- service

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ServiceArg {
    Mcp,
    Actions,
    All,
}

#[derive(Debug, Args)]
pub struct ServiceArgs {
    /// 操作哪个服务；stop / restart 默认 all
    #[arg(short = 's', long, value_enum)]
    pub service: Option<ServiceArg>,
}

#[derive(Debug, Args)]
pub struct StopArgs {
    /// 停哪个服务（默认 all）
    #[arg(short = 's', long, value_enum)]
    pub service: Option<ServiceArg>,

    /// 停所有工作区的服务，而不只是当前这个
    #[arg(short = 'a', long)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct DestroyArgs {
    /// 要销毁哪个工作区：目录 / 名称 / id（默认按当前目录推断）
    #[arg(id = "target", value_name = "WS")]
    pub workspace: Option<String>,

    /// 销毁全部工作区
    #[arg(short = 'a', long, conflicts_with = "target")]
    pub all: bool,

    /// 不询问，直接销毁
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct StartArgs {
    /// 项目目录（默认当前目录）；没登记过会自动登记为工作区
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// 公网入口：https://… | cf | cf:<域名> | frp:<配置名> | off
    #[arg(long, value_name = "TUNNEL")]
    pub tunnel: Option<TunnelSpec>,

    /// Cloudflare Tunnel Token（配合 --tunnel cf:<域名>）；不给会当场问
    #[arg(
        long = "token",
        alias = "tunnel-token",
        value_name = "TOKEN",
        requires = "tunnel"
    )]
    pub tunnel_token: Option<String>,

    /// FRP 子域名（配合 --tunnel frp:<配置名>）；不给则取工作区名
    #[arg(long, value_name = "SUB")]
    pub subdomain: Option<String>,

    /// 本地监听端口；Cloudflare 固定隧道需与云端回源端口一致（不会自动修改云端配置）
    #[arg(long, value_name = "PORT", value_parser = clap::value_parser!(u16).range(1..))]
    pub port: Option<u16>,

    /// 启动哪个服务（默认 mcp）
    #[arg(short = 's', long, value_enum)]
    pub service: Option<ServiceArg>,
}

/// `--tunnel` 的取值：一句话说清"公网地址从哪来"。
///
/// 以前这里是四个互斥选项（`--url` / `--named` / `--frp` / `--off`），
/// 每加一种入口就多一个开关，而它们表达的是同一件事的不同取值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelSpec {
    /// 已经有公网地址：只登记，不起隧道。
    Url(String),
    /// Cloudflare：临时地址（quick）或固定域名（named）。
    ///
    /// named 模式必须知道对外域名——它要写进 OAuth 元数据和 OpenAPI 文档，
    /// cloudflared 那边也拿它建 ingress。`domain` 是这次顺带把域名定下来，
    /// `None` 表示沿用工作区里已经配好的那个。
    Cloudflare { named: bool, domain: Option<String> },
    /// FRP，带一个 `gld frp list` 里的配置名或 id。
    Frp { profile: String },
    /// 关掉公网入口。
    Off,
}

impl std::str::FromStr for TunnelSpec {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let value = raw.trim();
        let lower = value.to_ascii_lowercase();
        if value.starts_with("http://") || value.starts_with("https://") {
            return Ok(Self::Url(value.to_string()));
        }
        // cloudflare 全称也收：字段表里写的是 mcp.tunnel=cloudflare，
        // 两处对不上会让人以为自己记错了。
        let (head, tail) = lower.split_once(':').unwrap_or((lower.as_str(), ""));
        match head {
            "off" | "none" => Ok(Self::Off),
            "cf" | "cloudflare" => match tail {
                "" | "quick" => Ok(Self::Cloudflare {
                    named: false,
                    domain: None,
                }),
                // 光写 named 表示"用工作区里已经配好的那个域名"。
                "named" => Ok(Self::Cloudflare {
                    named: true,
                    domain: None,
                }),
                // cf:<域名> 一步到位。域名可能含大写，取原串而不是小写化的那份。
                candidate if looks_like_domain(candidate) => Ok(Self::Cloudflare {
                    named: true,
                    domain: Some(value[head.len() + 1..].trim().to_string()),
                }),
                other => Err(format!(
                    "cf 后面只能跟 quick、named 或一个固定域名，收到「{other}」。\n  \
                     cf                       临时地址（每次重启都会变）\n  \
                     cf:mcp.example.com       固定域名，顺手把它配上\n  \
                     cf:named                 固定域名，沿用已经配好的那个"
                )),
            },
            "frp" if !tail.is_empty() => Ok(Self::Frp {
                // 配置名可能有大小写和中文，不能用小写化之后的那份。
                profile: value[head.len() + 1..].trim().to_string(),
            }),
            "frp" => {
                Err("frp 要带配置名，例如 --tunnel frp:公司（`gld frp list` 看有哪些）".into())
            }
            _ => Err(format!(
                "看不懂的公网入口「{value}」。可用写法：\n  \
                 https://mcp.example.com/mcp   已有的公网地址\n  \
                 cf                            Cloudflare 临时地址\n  \
                 cf:mcp.example.com            Cloudflare 固定域名\n  \
                 frp:<配置名>                  FRP（gld frp list 看有哪些）\n  \
                 off                           关掉公网入口"
            )),
        }
    }
}

/// `cf:` 后面这一段像不像域名。
///
/// 只认带点的（`mcp.example.com`）和带协议头的。这样 `cf:nmaed` 这种手滑
/// 会撞上"只能跟 quick、named 或域名"的报错，而不是被当成域名默默存进去，
/// 等到起隧道时才由 cloudflared 报一个不知所云的错。
fn looks_like_domain(value: &str) -> bool {
    value.starts_with("http://")
        || value.starts_with("https://")
        || (value.contains('.') && !value.ends_with('.'))
}

#[derive(Debug, Args)]
pub struct LogsArgs {
    /// 看哪个服务的日志
    #[arg(short = 's', long, value_enum, default_value = "mcp")]
    pub service: LogService,
    /// 显示最后 N 行
    #[arg(short = 'n', long, default_value_t = 40)]
    pub lines: usize,
    /// 持续跟随（Ctrl-C 退出）
    #[arg(short = 'f', long)]
    pub follow: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LogService {
    Mcp,
    Actions,
}

#[derive(Debug, Default, Args)]
pub struct ListArgs {
    /// 明文显示密钥（默认脱敏）
    #[arg(long)]
    pub reveal: bool,

    /// 列出全部工作区（在工作区目录里执行时用它看全局）
    #[arg(short = 'a', long)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct ShareArgs {
    /// 项目目录（默认当前目录）；没登记过会自动登记为工作区
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// 公网入口：https://… | cf | cf:<域名> | frp:<配置名> | off（默认 cf）
    #[arg(long, value_name = "TUNNEL", conflicts_with = "off")]
    pub tunnel: Option<TunnelSpec>,

    /// Cloudflare Tunnel Token（配合 --tunnel cf:<域名>）；不给会当场问
    #[arg(
        long = "token",
        alias = "tunnel-token",
        value_name = "TOKEN",
        requires = "tunnel"
    )]
    pub tunnel_token: Option<String>,

    /// FRP 子域名，公网地址为 https://<子域名>.<frps 域名>；不给则取工作区名
    #[arg(long, value_name = "SUB")]
    pub subdomain: Option<String>,

    /// 关掉公网入口，只留本地地址（等价 --tunnel off）
    #[arg(long)]
    pub off: bool,

    /// 暴露哪个服务
    #[arg(short = 's', long, value_enum, default_value = "mcp")]
    pub service: TunnelService,
}

#[derive(Debug, Args)]
pub struct UpgradeArgs {
    /// 要更新哪个工作区：目录 / 名称 / id（默认按当前目录推断）
    #[arg(id = "target", value_name = "WS")]
    pub workspace: Option<String>,

    /// 换项目根目录（目录要已存在）
    #[arg(long, value_name = "DIR")]
    pub path: Option<PathBuf>,

    /// 换公网入口：https://… | cf | cf:<域名> | frp:<配置名> | off
    #[arg(long, value_name = "TUNNEL", conflicts_with = "off")]
    pub tunnel: Option<TunnelSpec>,

    /// Cloudflare Tunnel Token（配合 --tunnel cf:<域名>）；不给会当场问
    #[arg(
        long = "token",
        alias = "tunnel-token",
        value_name = "TOKEN",
        requires = "tunnel"
    )]
    pub tunnel_token: Option<String>,

    /// FRP 子域名（配合 --tunnel frp:<配置名>）
    #[arg(long, value_name = "SUB")]
    pub subdomain: Option<String>,

    /// 关掉公网入口（等价 --tunnel off）
    #[arg(long)]
    pub off: bool,

    /// 换显示名称
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,

    /// 换 MCP 端口
    #[arg(long, value_name = "PORT")]
    pub port: Option<u16>,

    /// 换 Actions 端口
    #[arg(long, value_name = "PORT")]
    pub actions_port: Option<u16>,

    /// 换 MCP 认证方式：oauth | bearer | noauth
    #[arg(long, value_name = "AUTH")]
    pub auth: Option<String>,

    /// 改哪个服务的公网入口
    #[arg(short = 's', long, value_enum, default_value = "mcp")]
    pub service: TunnelService,
}

#[derive(Debug, Args)]
pub struct ContextArgs {
    /// 扫描用户主目录下各 IDE / Agent 的全局说明与 Skill 来源
    #[arg(long)]
    pub global: bool,
}

// ------------------------------------------------------------------ tool

#[derive(Debug, Subcommand)]
pub enum ToolCmd {
    /// 列出当前工作区暴露给 AI 的工具（取决于 mcp.tool-profile）
    #[command(visible_alias = "ls")]
    List,
    /// 显示某个工具的完整定义与参数 Schema
    Schema {
        /// 工具名，例如 read_file
        name: String,
    },
    /// 调用一个工具，打印结构化结果
    ///
    /// 参数三种写法：
    ///   key=value    字符串；true / false / null / 数字 / [ 或 { 开头会按 JSON 解析
    ///   key:=json    强制按 JSON 解析，例如 limit:=100
    ///   key=@文件    读取文件内容作为字符串，适合 apply_patch 的补丁正文
    ///
    /// 例：
    ///   gld tool call read_file path=src/main.rs
    ///   gld tool call exec_command cmd='cargo test' timeout_ms:=120000
    ///   gld tool call git_status
    ///
    /// 工具返回 ok=false 时退出码为 1，结构化结果照常打印，可以接 jq。
    /// 长命令留下的 exec 会话只在守护进程运行时才能被下一次调用读到
    /// （直连模式每次都是新进程）。
    Call {
        /// 工具名
        name: String,
        /// 参数，见上面三种写法
        #[arg(value_name = "ARG")]
        args: Vec<String>,
        /// 直接给一段 JSON 对象作为参数，与上面的写法合并（这个优先）
        #[arg(long, value_name = "JSON")]
        args_json: Option<String>,
    },
}

// ---------------------------------------------------------------- tunnel

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TunnelService {
    Mcp,
    Actions,
}

#[derive(Debug, Args)]
pub struct TunnelArgs {
    /// 哪个服务的隧道
    #[arg(short = 's', long, value_enum, default_value = "mcp")]
    pub service: TunnelService,
}

#[derive(Debug, Subcommand)]
pub enum TunnelCmd {
    /// 启动隧道（服务启动时通常已自动启动，这里用于单独重连）
    Start(TunnelArgs),
    /// 停止隧道
    Stop(TunnelArgs),
    /// 重启隧道（FRP 会原子替换线路，失败自动回滚配置）
    Restart(TunnelArgs),
    /// 验证隧道配置：拿到公网地址即通过；本地服务没在跑则测完自动断开
    Test(TunnelArgs),
    /// 查看隧道状态与公网地址
    Status(TunnelArgs),
    /// 打印这个工作区的完整 frpc 配置（存成 frpc.toml 就能自己跑）
    Snippet {
        #[command(flatten)]
        service: TunnelArgs,
        /// 输出真实的 frps token（默认是占位符，避免贴聊天窗口时泄露）
        #[arg(long)]
        reveal: bool,
    },
}

// --------------------------------------------------------------- gateway

#[derive(Debug, Subcommand)]
pub enum GatewayCmd {
    /// 显示全局入口配置与运行状态
    Show,
    /// 修改配置（只改给出的项）
    Set(GatewaySetArgs),
    /// 启动全局入口（含它的隧道）
    Start,
    /// 停止全局入口
    Stop,
    /// 检查本地与公网 /health
    Health,
}

#[derive(Debug, Args)]
pub struct GatewaySetArgs {
    /// 是否启用
    #[arg(long, value_name = "true|false")]
    pub enabled: Option<bool>,
    /// 本地端口
    #[arg(long, value_name = "PORT")]
    pub port: Option<u16>,
    /// 隧道类型：cf（Cloudflare）| frp | off（配合 --public-url 用现成地址）
    #[arg(long, value_name = "TYPE")]
    pub tunnel: Option<String>,
    /// 手动公网地址（tunnel=none 时使用）
    #[arg(long, value_name = "URL")]
    pub public_url: Option<String>,
    /// FRP 服务器配置 id
    #[arg(long, value_name = "ID")]
    pub frp_profile: Option<String>,
    /// FRP 子域名
    #[arg(long, value_name = "SUB")]
    pub frp_subdomain: Option<String>,
    /// 隧道是否套用全局代理
    #[arg(long, value_name = "true|false")]
    pub use_proxy: Option<bool>,
}

// ---------------------------------------------------------------- secret

#[derive(Debug, Subcommand)]
pub enum SecretCmd {
    /// 显示工作区密钥（默认脱敏，--reveal 明文）
    Show {
        key: String,
        #[arg(long)]
        reveal: bool,
    },
    /// 设置工作区密钥；正在运行且用到它的服务会自动重启
    Set { key: String, value: String },
    /// 重新生成工作区密钥并返回新值；相关服务自动重启
    #[command(visible_alias = "regen")]
    Regenerate { key: String },
    /// 操作共享密钥池（多个工作区勾选 shared-secrets 时共用）
    #[command(subcommand)]
    Shared(SharedSecretCmd),
    /// 列出所有合法的密钥名及用途
    Keys,
}

#[derive(Debug, Subcommand)]
pub enum SharedSecretCmd {
    Show {
        key: String,
        #[arg(long)]
        reveal: bool,
    },
    Set {
        key: String,
        value: String,
    },
    #[command(visible_alias = "regen")]
    Regenerate {
        key: String,
    },
}

// ------------------------------------------------------------------- frp

#[derive(Debug, Subcommand)]
pub enum FrpCmd {
    /// 列出 FRP 服务器配置
    List,
    /// 新增
    Add {
        #[arg(long)]
        name: String,
        /// frps 地址，例如 frp.example.com
        #[arg(long)]
        server: String,
        #[arg(long, default_value_t = 7000)]
        port: u16,
        /// frps token（保存在数据目录，不会出现在 list 输出里）
        #[arg(long)]
        token: Option<String>,
    },
    /// 修改（只改给出的项）
    Update {
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        server: Option<String>,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        token: Option<String>,
    },
    /// 删除（还被工作区引用时会拒绝，除非加 --force）
    #[command(visible_alias = "rm")]
    Remove {
        id: String,
        /// 照删不误，留下悬空引用（那些工作区下次 start 会报"引用的 FRP 配置不存在"）
        #[arg(long)]
        force: bool,
    },
}

// -------------------------------------------------------------- settings

#[derive(Debug, Subcommand)]
pub enum SettingsCmd {
    /// 显示全部全局设置
    Show,
    /// 全局出站代理（隧道进程使用）；不带参数时显示当前值
    Proxy {
        /// none | system | manual
        #[arg(long)]
        mode: Option<String>,
        /// manual 模式的代理地址，例如 http://127.0.0.1:7890
        #[arg(long)]
        url: Option<String>,
    },
    /// 运行时全局项：局域网访问、启动时恢复、可执行路径、全局 Agent 说明
    Runtime(RuntimeSetArgs),
}

#[derive(Debug, Args)]
pub struct RuntimeSetArgs {
    /// 允许 MCP / Actions / 全局入口监听 0.0.0.0（默认只监听 127.0.0.1）
    #[arg(long, value_name = "true|false")]
    pub lan_access: Option<bool>,
    /// 守护进程启动时恢复上次运行的服务
    #[arg(long, value_name = "true|false")]
    pub restore_on_launch: Option<bool>,
    /// 全局可执行文件搜索路径（换行或分号分隔）
    #[arg(long)]
    pub executable_paths: Option<String>,
    /// 注入给所有工作区 Agent 的全局说明
    #[arg(long)]
    pub ai_instructions: Option<String>,
    /// 全局说明文件来源，逗号分隔（如 cursor,claude,codex）
    #[arg(long)]
    pub instruction_sources: Option<String>,
    /// 全局 Skill 来源，逗号分隔
    #[arg(long)]
    pub skill_sources: Option<String>,
    #[arg(long)]
    pub custom_instruction_paths: Option<String>,
    #[arg(long)]
    pub custom_skill_paths: Option<String>,
}

// -------------------------------------------------------------- planning

#[derive(Debug, Subcommand)]
pub enum PlanningCmd {
    /// 显示当前模式、Goal / Plan 与执行台账
    Show,
    /// 切换模式：direct（自由改）| plan（只读，AI 先出计划）| goal（写操作须绑定 Goal）
    Mode { mode: String },
    #[command(subcommand)]
    Goal(GoalCmd),
    #[command(subcommand)]
    Plan(PlanCmd),
}

#[derive(Debug, Subcommand)]
pub enum GoalCmd {
    Create {
        #[arg(long)]
        title: String,
        #[arg(long)]
        objective: String,
        /// 可多次给出
        #[arg(long = "criterion")]
        criteria: Vec<String>,
        #[arg(long = "constraint")]
        constraints: Vec<String>,
    },
    Update {
        goal_id: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        objective: Option<String>,
        /// active | paused | completed | awaiting_acceptance | archived | cancelled
        #[arg(long)]
        status: Option<String>,
        #[arg(long = "constraint")]
        constraints: Option<Vec<String>>,
        /// 已完成的验收项 id，逗号分隔
        #[arg(long)]
        done: Option<String>,
        #[arg(long)]
        focus: Option<bool>,
    },
    /// 人工验收通过并归档
    Accept { goal_id: String },
    /// 驳回验收，Goal 回到 active
    Reject {
        goal_id: String,
        #[arg(long)]
        feedback: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum PlanCmd {
    Create {
        #[arg(long)]
        title: String,
        #[arg(long)]
        objective: String,
        #[arg(long)]
        goal: Option<String>,
        /// 可多次给出
        #[arg(long = "step")]
        steps: Vec<String>,
    },
    Update {
        plan_id: String,
        /// draft | active | paused | completed | awaiting_acceptance | archived | cancelled
        #[arg(long)]
        status: Option<String>,
        /// STEP_ID=STATUS[:备注]，可多次；STATUS 为 pending|in_progress|completed|blocked|skipped
        #[arg(long = "step")]
        steps: Vec<String>,
        #[arg(long)]
        focus: Option<bool>,
    },
    Accept {
        plan_id: String,
    },
    Reject {
        plan_id: String,
        #[arg(long)]
        feedback: Option<String>,
    },
}

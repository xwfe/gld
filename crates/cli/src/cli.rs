//! clap 定义。帮助文本是给第一次接触的人看的：每个子命令都说明它做什么、
//! 什么时候用、常见的下一步是什么。
//!
//! 命令面在 RFC-0004 收过一次：只有一个 MCP 服务，项目挂在它下面，增删改查
//! 都是顶层的一个词。老命令（`ws`、`destroy`、`hub`、`ps`、`tunnel`、`gateway`、
//! 各处的 `show`）还认，只是不进帮助。

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

const AFTER_HELP: &str = "\
快速上手：
  gld start ~/code/api          启动 MCP 服务，并把这个目录加进来（守护进程自动在后台拉起）
  gld add ~/code/web            再加一个项目；服务在跑就立即生效
  gld ls                        客户端要填的地址和凭据，和所有项目
  gld share                     要接 ChatGPT 时用：给服务拿一个公网 HTTPS 地址
  gld set web tool-profile=read-only   改某个项目的配置（字段见 gld fields）
  gld rm web                    删掉一个项目（只删 gld 这边的配置，项目文件不动）
  gld stop                      停服务；项目、配置和凭据都留着

只有一个服务：客户端里只配一条连接，AI 每次调用带 workspace 参数（项目名或 id）选项目。
拿到服务凭据就能访问全部项目——只想单独给出去的项目别加进来。

公网入口（--tunnel 在 start / share / upgrade 里通用）：
  --tunnel https://mcp.example.com/mcp    已有公网地址（自建反代等），只登记不起隧道
  --tunnel cf                             Cloudflare 临时地址，零配置，重启会变
  --tunnel cf:mcp.example.com             Cloudflare 固定域名，要 Tunnel Token（没配过会当场问）
  --tunnel frp:公司                       FRP 固定域名，子域名默认 gld
  --tunnel off                            关掉公网入口，只留本地地址

项目定位：
  改项目的命令接受项目名（或 -w <id|id前缀|名称|路径>）。不给时按当前目录归属推断；
  只有一个项目时直接使用它。

数据目录：
  默认 ~/.config/gld，可用 --home 或环境变量 GLD_HOME 覆盖。里面有配置、密钥、日志，
  以及守护进程的 socket 与 pid 文件。frpc / cloudflared 由你自己安装，gld 从 PATH 里找。

更多：docs/cli.md（完整命令参考）、docs/daemon.md（后台进程说明）";

#[derive(Debug, Parser)]
#[command(
    name = "gld",
    version,
    about = "把本地项目变成 AI 可通过 MCP 直接开发的工作区：一个服务，多个项目，常驻后台",
    long_about = "gld 在后台跑一个 MCP Streamable HTTP 服务，把登记进来的项目目录都挂在它下面：\
客户端只配一条连接，AI 每次调用用 workspace 参数选项目，项目之间互不串。\
需要时能通过 FRP / Cloudflare 隧道暴露到公网。\n\n\
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
    /// 目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断
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
    /// 启动 MCP 服务；给了目录（或当前目录就是项目）会顺带把它加进来
    ///
    ///   gld start                          起服务；当前目录是项目、或者还一个项目都没有时，顺带加当前目录
    ///   gld start ~/code/api               起服务，并把这个目录加进来
    ///   gld start --tunnel cf:mcp.example.com
    ///                                      顺带配好公网入口，起完直接打印连接信息
    ///
    /// 守护进程没在跑会自动拉起，之后关掉终端服务也照常在。
    /// -s actions 起的是这个项目的 GPT Actions（自定义 GPT 用），它还是一个项目一个。
    #[command(verbatim_doc_comment)]
    Start(StartArgs),

    /// 停止服务（项目、配置、凭据都不动）
    ///
    ///   gld stop              MCP 服务，连同各项目的 GPT Actions
    ///   gld stop -s actions   只停当前项目的 GPT Actions
    ///
    /// 连守护进程一起退出用 gld daemon stop；要删项目用 gld rm。
    #[command(verbatim_doc_comment)]
    Stop(StopArgs),

    /// 重启服务
    ///
    /// 改端口 / 认证 / 凭据不需要它——upgrade 和 secret set 会自己重启服务。
    /// 用得上它的场景：改了全局设置（gld cfg runtime），或服务卡住了想踢一脚。
    #[command(verbatim_doc_comment)]
    Restart(ServiceArgs),

    /// 服务、公网入口和项目的一览
    #[command(alias = "ps")]
    Status,

    /// 客户端要填的地址、凭据，和所有项目；给项目名就看这个项目的配置
    ///
    ///   gld ls                  服务的地址、凭据（脱敏）和项目表
    ///   gld ls api              项目 api 的配置
    ///   gld ls --reveal         凭据显示明文
    #[command(verbatim_doc_comment, visible_alias = "ls")]
    List(ListArgs),

    /// 加项目：登记目录并加入服务（服务在跑就立即生效，不用重启）
    ///
    ///   gld add                 当前目录
    ///   gld add ~/code/api ~/code/web
    ///   gld add . --name api    起个名字（默认是目录名）
    ///
    /// 另一台机器上 ccnm 管着的项目用 gld remote add。
    #[command(verbatim_doc_comment)]
    Add(AddArgs),

    /// 删项目：从服务里拿掉，并删掉它在 gld 这边的配置和凭据（项目文件一个字节都不动）
    ///
    ///   gld rm api              按名称 / 路径 / id 指定，可以一次给多个
    ///   gld rm                  当前目录对应的项目
    ///   gld rm --all -y         全部项目，不询问
    ///
    /// 远端项目（gld remote add 加的）也用它删。
    #[command(verbatim_doc_comment, visible_alias = "rm", alias = "destroy")]
    Remove(RemoveArgs),

    /// 改项目配置：gld set api tool-profile=read-only（字段见 gld fields）
    ///
    ///   gld set tool-profile=read-only              当前目录对应的项目
    ///   gld set api allowed-commands=rg,gh          按名称指定项目
    ///
    /// 下一次调用就生效，不用重启。服务本身的端口、认证、公网入口用 gld upgrade / gld share。
    #[command(verbatim_doc_comment)]
    Set(SetArgs),

    /// 列出 set 支持的项目字段及取值（--all 连 GPT Actions 那条线路一起列）
    Fields {
        /// 连 actions.* 一起列出
        #[arg(long)]
        all: bool,
    },

    /// 给服务拿一个公网 HTTPS 地址（ChatGPT 只能连公网，127.0.0.1 填进去连不上）
    ///
    /// 它把「配公网入口 → 起服务 → 查连接信息」三步合成一步：
    ///   gld share                             沿用已配好的入口；一个都没配就用 Cloudflare 临时地址
    ///   gld share --tunnel cf:mcp.example.com Cloudflare 固定域名，要 Tunnel Token（没配过会当场问）
    ///   gld share --tunnel frp:公司           FRP 固定域名，子域名默认 gld
    ///   gld share --tunnel https://x.com/mcp  已经有公网地址（自建反代等），只登记不起隧道
    ///   gld share --off                       关掉公网入口，只留本地地址
    ///
    /// 公网入口意味着"在你电脑上跑命令"这件事对外可达，而且一把凭据能进全部项目。
    /// 开之前请读 docs/security.md。
    #[command(verbatim_doc_comment)]
    Share(ShareArgs),

    /// 改服务配置（端口 / 认证 / 工具集 / 公网入口），改完自动重启；也能改项目的目录和名称
    ///
    ///   gld upgrade --port 30001 --auth bearer            服务的端口和认证
    ///   gld upgrade --tunnel https://new.example.com/mcp  换公网地址
    ///   gld upgrade --off                                 关掉公网入口
    ///   gld upgrade api --path ~/code/api-v2              项目搬了目录
    ///
    /// 项目的其余字段见 gld fields 与 gld set。
    #[command(verbatim_doc_comment)]
    Upgrade(UpgradeArgs),

    /// 远端项目：另一台机器上由 ccnm 管着的 workspace，经 ccnm mcp bridge 访问
    #[command(subcommand)]
    Remote(RemoteCmd),

    /// 只开部分项目的凭据：给别人或另一个客户端只开几个项目，随时作废
    ///
    ///   gld grant add alice api web           只开 api、web，只读：能看能搜，改不了文件、跑不了命令
    ///   gld grant add me-laptop api --write   能写能跑命令（等于把这台机器交出去，见下）
    ///   gld grant ls                          有哪些、各开了什么（--reveal 显示口令和令牌）
    ///   gld grant rm alice                    立即作废，并停掉它起的命令
    ///
    /// 客户端地址不变：OAuth 在授权页填 grant 的口令（不是服务口令），bearer 用 grant 的令牌。
    /// 服务自己的口令和令牌照旧管全部项目。
    ///
    /// 能跑命令就能以你的身份读到 gld 数据目录里的服务口令，拿到全权：--write 只发给你
    /// 愿意把服务口令交给的人，给别人一律用默认的只读。
    #[command(subcommand, verbatim_doc_comment)]
    Grant(GrantCmd),

    /// 本机装好的 MCP server：看装了哪些、开哪几个经服务转给 AI、试着起一个
    ///
    ///   gld mcp ls                     ~/.claude.json 和 ~/.codex/config.toml 里装了哪些、开了哪些
    ///   gld mcp on context7 deepwiki   开：连上服务的 AI 用 list_mcp_tools / call_mcp_tool 调它们
    ///   gld mcp off context7           关（--all 全关）
    ///   gld mcp test context7          在守护进程里起一次：起不起得来、有哪些工具
    ///
    /// 默认一个都不开。AI 经服务调它们，和你在本机 Claude Code 里调一样：Filesystem、
    /// desktop-commander 这类能读写整个主目录。服务挂了公网入口时尤其想清楚再开。
    #[command(subcommand, verbatim_doc_comment)]
    Mcp(McpCmd),

    /// 查看服务日志尾部，或用 -f 持续跟随（-w 看某个项目自己的请求日志）
    // 隐藏别名：git 是 log、docker 是 logs，两边习惯的人都不该被一句
    // "unrecognized subcommand" 拦住。不用 visible_alias 是因为它只防手滑，
    // 不值得占帮助里的一行。
    #[command(alias = "log")]
    Logs(LogsArgs),

    /// 逐项检查本地 / 公网端点与 OAuth 元数据是否可达
    Health(HealthArgs),

    /// 体检：检查配置是否自洽，并给出每个问题的修复命令
    Doctor {
        /// 再实地探一次本地 / 公网端点和 OAuth 元数据（同 gld health）；不加它一个网络请求都不发
        #[arg(long)]
        probe: bool,
    },

    /// 直接调用工具内核：不接 AI 客户端也能验证 Agent 会看到什么
    #[command(subcommand)]
    Tool(ToolCmd),

    /// 服务的凭据：看 / 自己定 / 重新生成（Bearer Token、OAuth 口令、Tunnel Token…）
    #[command(subcommand)]
    Secret(SecretCmd),

    /// 管理 FRP 服务器配置（--tunnel frp:<配置名> 引用它）
    #[command(subcommand)]
    Frp(FrpCmd),

    /// 全局设置：出站代理、局域网访问、启动时恢复、全局 Agent 说明
    #[command(subcommand, visible_alias = "cfg")]
    Settings(SettingsCmd),

    /// Goal / Plan 规划状态与人工验收（按项目）
    #[command(subcommand)]
    Planning(PlanningCmd),

    /// 列出项目的历史会话档案（docs/history-session）
    History,

    /// 查看本次守护进程运行期间的请求次数与 Token 估算
    Usage,

    /// 查看会注入给 Agent 的说明文件与 Skill（--global 看用户级来源）
    Context(ContextArgs),

    /// 管理后台守护进程（启动 / 停止 / 状态 / 日志）
    #[command(subcommand)]
    Daemon(DaemonCmd),

    /// 生成 shell 补全脚本
    #[command(alias = "completion")]
    Completions {
        /// bash | zsh | fish | powershell | elvish
        shell: clap_complete::Shell,
    },

    // ---- 以下是 RFC-0004 之前的命令，还认，不进帮助 ----
    /// 管理项目（旧写法；现在是顶层的 add / ls / set / rm / fields）
    #[command(subcommand, alias = "ws", hide = true)]
    Workspace(WorkspaceCmd),

    /// 服务本身的旧命令组（现在是顶层的 start / stop / ls / upgrade / remote）
    #[command(subcommand, hide = true)]
    Hub(HubCmd),

    /// GPT Actions 的隧道（MCP 服务的公网入口用 gld share）
    #[command(subcommand, hide = true)]
    Tunnel(TunnelCmd),

    /// 全局共享公网入口（旧：多个单项目服务共用一个域名）
    #[command(subcommand, alias = "gw", hide = true)]
    Gateway(GatewayCmd),
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
    /// 登记一个项目目录并加入服务（同 gld add）
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
    /// 列出所有项目（同 gld ls）
    #[command(visible_alias = "ls")]
    List,
    /// 显示一个项目的配置（同 gld ls <项目>）
    Show,
    /// 删除项目（同 gld rm）
    #[command(visible_alias = "rm")]
    Remove {
        /// 不询问，直接删除
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// 修改项目字段（同 gld set）
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
    /// 停哪个：mcp（服务）| actions（当前项目的 GPT Actions）| all（默认，全部）
    #[arg(short = 's', long, value_enum)]
    pub service: Option<ServiceArg>,

    /// 旧写法（停所有工作区的服务），现在不带它也是全部停
    #[arg(short = 'a', long, hide = true)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct StartArgs {
    /// 顺带加进来的项目目录；不给时见上面的说明
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

    /// FRP 子域名（配合 --tunnel frp:<配置名>）；不给则是 gld
    #[arg(long, value_name = "SUB")]
    pub subdomain: Option<String>,

    /// 服务的本地端口；Cloudflare 固定隧道需与云端回源端口一致（不会自动修改云端配置）
    #[arg(long, value_name = "PORT", value_parser = clap::value_parser!(u16).range(1..))]
    pub port: Option<u16>,

    /// 起哪个：mcp（默认，服务）| actions（这个项目的 GPT Actions）| all
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
    /// mcp：服务的日志（给了 -w 就是那个项目的请求日志）| actions：项目的 GPT Actions
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
    /// 只看这个项目的配置（名称、id、id 前缀或路径）
    #[arg(value_name = "PROJECT")]
    pub project: Option<String>,

    /// 明文显示密钥（默认脱敏）
    #[arg(long)]
    pub reveal: bool,

    /// 旧写法（在工作区目录里也列全部），现在不带它也是全部
    #[arg(short = 'a', long, hide = true)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct AddArgs {
    /// 项目目录，可以一次给多个（默认当前目录）
    #[arg(value_name = "PATH")]
    pub paths: Vec<PathBuf>,

    /// 显示名称（默认目录名；只给一个目录时能用）
    #[arg(long)]
    pub name: Option<String>,
}

#[derive(Debug, Args)]
pub struct RemoveArgs {
    /// 要删的项目：名称 / 路径 / id，可以一次给多个（默认按当前目录推断）
    #[arg(id = "target", value_name = "PROJECT")]
    pub projects: Vec<String>,

    /// 删掉全部本地项目
    #[arg(short = 'a', long, conflicts_with = "target")]
    pub all: bool,

    /// 不询问，直接删
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct SetArgs {
    /// [项目] KEY=VALUE…：第一个不带 = 的是项目，其余是要改的字段
    #[arg(value_name = "ARG", required = true)]
    pub args: Vec<String>,
}

#[derive(Debug, Args)]
pub struct HealthArgs {
    /// mcp（默认）：服务 | actions：当前项目的 GPT Actions
    #[arg(short = 's', long, value_enum, default_value = "mcp")]
    pub service: TunnelService,
}

#[derive(Debug, Args)]
pub struct ShareArgs {
    /// 顺带加进来的项目目录（可选，规则同 gld start）
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// 公网入口：https://… | cf | cf:<域名> | frp:<配置名> | off（默认沿用已配好的，没有就 cf）
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

    /// FRP 子域名，公网地址为 https://<子域名>.<frps 域名>；不给则是 gld
    #[arg(long, value_name = "SUB")]
    pub subdomain: Option<String>,

    /// 关掉公网入口，只留本地地址（等价 --tunnel off）
    #[arg(long)]
    pub off: bool,

    /// 暴露哪个：mcp（默认，服务）| actions（当前项目的 GPT Actions）
    #[arg(short = 's', long, value_enum, default_value = "mcp")]
    pub service: TunnelService,
}

#[derive(Debug, Args)]
pub struct UpgradeArgs {
    /// --path / --name 改的是哪个项目：目录 / 名称 / id（默认按当前目录推断）
    #[arg(id = "target", value_name = "PROJECT")]
    pub workspace: Option<String>,

    /// 把项目根目录换成这个（要已存在）；挑哪个项目用上面的 PROJECT 或 -w，不是它
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

    /// 换项目的显示名称
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,

    /// 换服务的端口
    #[arg(long, value_name = "PORT", value_parser = clap::value_parser!(u16).range(1..))]
    pub port: Option<u16>,

    /// 换项目的 GPT Actions 端口
    #[arg(long, value_name = "PORT")]
    pub actions_port: Option<u16>,

    /// 换服务的认证方式：oauth | bearer | noauth
    #[arg(long, value_name = "AUTH")]
    pub auth: Option<String>,

    /// 换服务列给客户端的工具集；项目自己的工具集照样生效，两边取交集
    #[arg(long, value_name = "PROFILE")]
    pub tool_profile: Option<String>,

    /// --tunnel / --off 改哪个：mcp（默认，服务）| actions（项目的 GPT Actions）
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
    /// 列出当前项目暴露给 AI 的工具（取决于它的 tool-profile）
    #[command(visible_alias = "ls")]
    List {
        /// 改看正在跑的服务此刻给客户端的 tools/list（参数、指纹、构建提交），用来查客户端是否缓存了旧表
        #[arg(long)]
        served: bool,
    },
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
    /// exec_command 跑的命令自己失败（退出非零、超时）不算 ok=false，退出码仍是 0：
    /// 脚本要判断命令结果，读结果里的 command_ok / exit_code。
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

/// 旧命令组：项目自己的隧道。MCP 服务的公网入口用 `gld share`，这里现在只对
/// `-s actions` 还有意义。
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
    #[command(visible_alias = "ls", alias = "show")]
    List,
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
    /// 隧道类型：cf（Cloudflare 临时地址，重启就变）| frp（固定子域名）| off（配合 --public-url 用现成地址）
    #[arg(long, value_name = "TYPE")]
    pub tunnel: Option<String>,
    /// 手动公网地址（tunnel=none 时使用）
    #[arg(long, value_name = "URL")]
    pub public_url: Option<String>,
    /// FRP 服务器配置：名称、id 或 id 前缀（≥4 位），见 gld frp list
    #[arg(long, value_name = "配置名|id")]
    pub frp_profile: Option<String>,
    /// FRP 子域名
    #[arg(long, value_name = "SUB")]
    pub frp_subdomain: Option<String>,
    /// 隧道是否套用全局代理
    #[arg(long, value_name = "true|false")]
    pub use_proxy: Option<bool>,
}

// ------------------------------------------------------------------- mcp

#[derive(Debug, Subcommand)]
pub enum McpCmd {
    /// 装了哪些、开了哪些（读 ~/.claude.json 的 mcpServers 和 ~/.codex/config.toml 的 mcp_servers）
    #[command(visible_alias = "ls")]
    List,
    /// 开：经服务转给 AI，下一次调用就生效，不用重启服务
    On {
        /// gld mcp ls 里的名字，区分大小写，可以一次给多个
        #[arg(value_name = "NAME", required = true)]
        names: Vec<String>,
    },
    /// 关：正开着的连接在下一次调用时收掉
    Off {
        /// 要关的名字，可以一次给多个
        #[arg(value_name = "NAME", required_unless_present = "all")]
        names: Vec<String>,
        /// 全关
        #[arg(long, conflicts_with = "names")]
        all: bool,
    },
    /// 在守护进程里起一次、握手、列工具（用的是服务起它时的 PATH 和环境变量）
    Test {
        #[arg(value_name = "NAME")]
        name: String,
    },
}

// ------------------------------------------------------------------- hub

#[derive(Debug, Subcommand)]
pub enum HubCmd {
    /// 显示状态、地址、认证、凭据和成员（同 gld ls）
    #[command(visible_alias = "ls", alias = "show")]
    List {
        /// 凭据显示明文
        #[arg(long)]
        reveal: bool,
    },
    /// 把工作区加进 hub：下一次调用立即生效，不用重启
    Add {
        /// 工作区：id、id 前缀（≥4 位）、名称或路径，可以一次给多个；不给就是当前目录所属的工作区
        #[arg(value_name = "WS")]
        workspaces: Vec<String>,
    },
    /// 把工作区移出 hub：下一次调用起就访问不到（工作区本身和项目文件都不动）
    #[command(visible_alias = "rm")]
    Remove {
        /// 工作区：id、id 前缀（≥4 位）、名称或路径；不给就是当前目录所属的工作区
        #[arg(value_name = "WS")]
        workspaces: Vec<String>,
    },
    /// 修改配置（只改给出的项）；hub 正在运行则自动重启
    Set(HubSetArgs),
    /// 启动（已在运行则按当前配置重启）；之后守护进程重启会自动恢复
    Start,
    /// 停止；配置、成员和凭据都保留，守护进程重启后不再自动拉起
    Stop,
    /// 重新生成凭据并返回新值；hub 在跑会自动重启
    ///
    ///   bearer_token         bearer 认证用的 token，客户端里要换成新值
    ///   oauth_password       授权页口令，只影响下一次授权，已授权的客户端不掉线
    ///   oauth_token_secret   令牌签名密钥，换了所有已授权的客户端都要重新授权
    ///   oauth_client_id      静态 Client ID，只影响手填了它的客户端
    #[command(visible_alias = "regen", verbatim_doc_comment)]
    Regenerate { key: String },
    /// 远端成员（同 gld remote）
    #[command(subcommand)]
    Remote(RemoteCmd),
}

/// 远端项目：另一台机器上由 ccnm 管着的 workspace。
///
/// 前提：那台机器上已经装好并配好 ccnm，本机也装了 ccnm（gld 起的是
/// `ccnm mcp bridge`，SSH 连接由 ccnm 自己管，gld 不碰凭据）。
#[derive(Debug, Subcommand)]
pub enum RemoteCmd {
    /// 登记一个远端 workspace 并加进服务（立即生效，不用重启）
    ///
    ///   gld remote add prod --node work --remote-workspace server
    ///
    /// 两个值填的都是 **ccnm 配置里的名字**，不是 host 也不是路径。在那台机器上
    /// 跑 ccnm workspace list 能看到有哪些。
    ///
    /// 叫 --remote-workspace 是因为 --workspace / -w 已经被全局参数占了，
    /// 那个说的是"本机哪个工作区"，两回事。
    #[command(verbatim_doc_comment)]
    Add {
        /// 给人看的名字，调用时 workspace 参数也能用它
        #[arg(value_name = "NAME")]
        name: String,
        /// ccnm 配置里的 node 别名（一台机器的名字）
        #[arg(long, value_name = "NODE")]
        node: String,
        /// ccnm 配置里的 workspace 名
        #[arg(long = "remote-workspace", value_name = "WS")]
        remote_workspace: String,
        /// 本机 ccnm 可执行程序（默认用 PATH 里的 ccnm）
        #[arg(long, value_name = "PATH")]
        ccnm: Option<String>,
        /// 访问上限：read（默认）| coding。coding 的成员多六个工具（改文件、跑命令、读输出、停后台命令、用那台机器上的 MCP server），用之前先 remote_coding_begin 拿句柄
        #[arg(long, value_name = "MODE")]
        mode: Option<String>,
    },
    /// 删掉一个远端项目（按名字、id 或 id 前缀；gld rm 也能删）
    #[command(visible_alias = "rm")]
    Remove {
        #[arg(value_name = "NAME")]
        selector: String,
    },
}

#[derive(Debug, Args)]
pub struct HubSetArgs {
    /// 本地端口（默认 28764）
    #[arg(long, value_name = "PORT")]
    pub port: Option<u16>,
    /// 认证方式：oauth | bearer | noauth
    #[arg(long, value_name = "TYPE")]
    pub auth: Option<String>,
    /// 列给客户端的工具集；成员自己的工具集照样生效，两边取交集
    #[arg(long, value_name = "PROFILE")]
    pub tool_profile: Option<String>,
    /// 已有的公网地址（自建反代回源到本地端口），不带 /mcp；给空串清掉
    #[arg(long, value_name = "URL")]
    pub public_url: Option<String>,
    /// 经全局入口暴露为 <入口公网地址>/hub/mcp（全局入口要先启用）
    #[arg(long, value_name = "true|false")]
    pub global_gateway: Option<bool>,
}

// ----------------------------------------------------------------- grant

#[derive(Debug, Subcommand)]
pub enum GrantCmd {
    /// 发一把新的，只开给出的项目（名称、id 或 id 前缀，本地远端都行）
    Add {
        /// 名字，在 ls / rm 里用它认这一把
        name: String,
        /// 开放的项目，至少一个
        #[arg(required = true, value_name = "PROJECT")]
        projects: Vec<String>,
        /// 能写文件、能跑命令。能跑命令就读得到服务口令，等于全权：只给你信得过的
        #[arg(long)]
        write: bool,
    },
    /// 列出全部（默认脱敏，--reveal 显示口令和令牌）
    #[command(visible_alias = "ls")]
    List {
        #[arg(long)]
        reveal: bool,
    },
    /// 作废：下一次请求起它的令牌就不能用，刷新也换不来新的；它起的命令一起停掉
    #[command(visible_alias = "rm")]
    Remove { name: String },
}

// ---------------------------------------------------------------- secret

/// 不带 `-w` 是服务的凭据；带了 `-w` 是那个项目自己的（GPT Actions 用的那些）。
#[derive(Debug, Subcommand)]
pub enum SecretCmd {
    /// 列出凭据（默认脱敏，--reveal 明文）；给了 KEY 只看那一项
    #[command(visible_alias = "ls", alias = "show")]
    List {
        key: Option<String>,
        #[arg(long)]
        reveal: bool,
    },
    /// 自己定一项凭据（记得住的授权口令、Cloudflare Tunnel Token）；服务在跑会自动重启
    Set { key: String, value: String },
    /// 重新生成一项凭据并返回新值；服务在跑会自动重启
    #[command(visible_alias = "regen")]
    Regenerate { key: String },
    /// 旧的共享密钥池（多个单项目服务共用凭据时用）
    #[command(subcommand, hide = true)]
    Shared(SharedSecretCmd),
    /// 列出所有凭据名及用途
    Keys,
}

#[derive(Debug, Subcommand)]
pub enum SharedSecretCmd {
    #[command(visible_alias = "ls", alias = "show")]
    List {
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
    #[command(visible_alias = "ls")]
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
    #[command(visible_alias = "ls", alias = "show")]
    List,
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
    #[command(visible_alias = "set")]
    Runtime(RuntimeSetArgs),
}

#[derive(Debug, Args)]
pub struct RuntimeSetArgs {
    /// 允许服务 / Actions / 全局入口监听 0.0.0.0（默认只监听 127.0.0.1）
    #[arg(long, value_name = "true|false")]
    pub lan_access: Option<bool>,
    /// 守护进程启动时恢复上次运行的 GPT Actions（MCP 服务不看它：没被 gld stop 过就恢复）
    #[arg(long, value_name = "true|false")]
    pub restore_on_launch: Option<bool>,
    /// 全局可执行文件搜索路径（换行或分号分隔）
    #[arg(long)]
    pub executable_paths: Option<String>,
    /// 注入给所有项目 Agent 的全局说明
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
    /// 本机装的 Skill 里不交给 AI 的，按名字，逗号分隔；传 "" 清空。项目自己的不受影响
    #[arg(long, value_name = "NAMES")]
    pub hidden_skills: Option<String>,
}

// -------------------------------------------------------------- planning

#[derive(Debug, Subcommand)]
pub enum PlanningCmd {
    /// 显示当前模式、Goal / Plan 与执行台账
    #[command(visible_alias = "ls", alias = "show")]
    List,
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

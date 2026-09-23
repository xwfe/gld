# gld — 让本地项目变成 AI 可直接开发的 MCP 工作区

`gld` 在后台跑**一个** MCP 服务（Model Context Protocol，说白了就是 AI 客户端调用外部工具的
通用接口），你的项目都挂在它下面。AI 客户端（ChatGPT、Claude Code、Cursor…）只配这一条
连接，就能在各个项目里读文件、改代码、跑命令、看 Git 状态，并按项目保存任务与历史记录。
工作区工具每次用 `workspace` 选项目，不共享一个可被其他对话切换的“当前目录”。
**这是路由与状态分离，不是操作系统沙箱，也不是按客户端划分的项目授权。**

*Run one MCP server that AI clients (ChatGPT, Claude Code, Cursor, Codex…) connect to
once, then develop in any of your local projects through it: read, patch, run commands,
inspect Git, and keep project-scoped task progress. Workspace routing is not an OS
sandbox or per-client project authorization. Docs are in Chinese.*

## 什么时候用

- **想让网页版 ChatGPT 直接改你电脑上的项目。** 它跑在 OpenAI 的服务器上，碰不到
  本地文件；gld 在本机起服务，再用一条隧道给它一个公网 HTTPS 地址。
- **本机的 Claude Code / Cursor / Codex 想共用同一套项目工具**：文件工具默认限制在
  项目目录，命令经过静态策略检查；子进程仍具有运行账号的系统权限。
  Planning 可要求先规划、再由你放行。
- **手上好几个项目**：客户端里只配一条连接，项目加进来就能用，不用每个项目配一次。
- **项目在另一台机器上**，由 [ccnm](https://github.com/xwfe/ccnm) 管着：把它作为
  远端项目加进同一个服务。
- **想让 ChatGPT 也用上本机 Claude Code / Codex 里装好的 MCP server**（context7、
  deepwiki……）：`gld mcp on context7` 点名开，经同一条连接转过去，见
  [concepts.md](docs/concepts.md#本机装好的-mcp-server)。

## 装

到 [Releases](../../releases) 拿对应平台的包，解压后把 `gld` 放进 PATH：

```bash
tar xzf gld-*-aarch64-apple-darwin.tar.gz
mkdir -p "$HOME/.local/bin"
install -m 755 gld-*/gld "$HOME/.local/bin/gld"
export PATH="$HOME/.local/bin:$PATH"
gld --version
```

上例适用于只解压了一份 Apple 芯片版安装包的目录。macOS 首次运行可能被 Gatekeeper 拦。
其他平台、校验和、从源码装、升级、卸载
见 [docs/install.md](docs/install.md)。

## 快速使用

```bash
gld start ~/code/my-project   # 起服务，并把这个目录加进来：端口、凭据都自动生成
gld add ~/code/another        # 再加一个项目；服务在跑就立即生效
gld ls                        # 给客户端用的地址和凭据，和项目表（gld list 也行）
```

守护进程自动在后台拉起，关掉终端服务照常在。`gld start` 不带目录时：当前目录是项目
（或者一个项目都还没有）就用它，否则只起服务、不登记当前目录——免得在主目录里随手一敲
就把整个主目录交给 AI。

**本机客户端**（Claude Code、Cursor、Codex）直接填 `gld ls` 给出的本地地址。

**ChatGPT** 只能连公网 HTTPS，`127.0.0.1` 填进去连不上，要再借一个公网地址：

```bash
gld share                              # Cloudflare 临时地址；需要 cloudflared 在 PATH 里
gld share --tunnel cf:mcp.example.com  # 自己有域名和 Cloudflare 隧道时，地址固定
```

固定域名、FRP、自建反代，以及每种客户端里具体怎么填，见
[docs/connect-clients.md](docs/connect-clients.md)。

> **开公网入口前请先读 [docs/security.md](docs/security.md)。**
> 它等于把"以你的身份在你电脑上跑命令"这件事对外开放了，而且一把凭据能进**全部**项目。

增删改查都是一个词：

```bash
gld set my-project tool-profile=read-only   # 改某个项目（字段见 gld fields）
gld upgrade --port 30000 --auth bearer      # 改服务的端口、认证、公网入口，改完自动重启
gld rm another                              # 删掉一个项目：只删 gld 这边的配置，项目文件不动
gld stop                                    # 停服务；项目、配置和凭据都留着
```

出问题先跑 `gld doctor`——它检查常见的配置不一致，
每一条下面直接写着该执行的命令。

> 2026-09-22 起 gld 只剩这一种用法（[RFC-0004](docs/rfc/0004-one-service-many-projects.md)）：
> 以前"一个项目一个服务"和"聚合入口 hub"两条路合成了一条。旧命令
> （`gld ws …`、`gld hub …`、`gld destroy`）还能敲，不进帮助。

## 当前能做到哪里

**已能支撑 AI 主导的日常编码闭环；尚不是独立、无人值守的全生命周期开发平台。**

| 范围 | 当前状态 |
| --- | --- |
| 读代码、修改、执行、Git 检查 | 已实现；包括补丁预检、命令输出续读、Notebook 和 Skills |
| 多项目、远端项目、本机 MCP 扩展 | 已实现；远端执行由 ccnm 负责，本机 MCP 需操作员点名启用 |
| 规划、任务、交接 | 可保存 Goal / Plan、任务和历史；验证证据尚未接入任务完成判定 |
| 浏览器验收、发布、部署、运维 | 可组合项目脚本和获准的外部能力；不等于内置可靠的全流程编排 |

源码有某项工具不代表当前客户端已经发现它。升级后需要核对工具名、参数与实际调用结果，
不能只对版本号。现有边界、可用流程和优先补齐项见
[项目开发生命周期](docs/project-lifecycle.md)与[2026-09-23 审查](docs/reviews/2026-09-23-lifecycle-and-docs-audit.md)。

## 查

| 我想…… | 看这里 |
| --- | --- |
| 搞清楚服务和项目、工具集、Planning 模式这些名词是什么 | [concepts.md](docs/concepts.md) |
| 项目路由、状态分离与授权的区别 | [concepts.md 为什么不会串](docs/concepts.md#为什么不会串) |
| 接到 ChatGPT / Claude Code / Cursor / 自定义 GPT | [connect-clients.md](docs/connect-clients.md) |
| 知道 AI 到底能碰什么，以及怎么收紧 | [security.md](docs/security.md) |
| 照着报错找处理办法 | [troubleshooting.md](docs/troubleshooting.md) |
| 查某个命令的全部参数 | [cli.md](docs/cli.md)，或直接 `gld <命令> --help` |
| 弄明白后台那个守护进程 | [daemon.md](docs/daemon.md) |
| 装 / 升级 / 卸载 | [install.md](docs/install.md) |
| 从桌面版迁移过来 | [migrate-from-desktop.md](docs/migrate-from-desktop.md) |
| 改这个项目的代码 | [architecture.md](docs/architecture.md)、[development.md](docs/development.md) |
| 为什么只留多项目模式、命令怎么对应 | [RFC-0004](docs/rfc/0004-one-service-many-projects.md) |
| 看共享 Rust 内核、服务接 ccnm 远端工具的方案和落地记录（已实施，第 9 节） | [RFC-0002](docs/rfc/0002-shared-kernel-and-ccnm-hub.md) |
| 看服务怎么跟上 ccnm 的新工具、compact 档怎么放回 skills | [RFC-0003](docs/rfc/0003-native-parity-sync.md) |
| 看三仓重构中的 gld 职责、优先修复项与验收依赖 | [跨项目重构落地清单](docs/reviews/2026-09-19-cross-project-refactor-actions.md) |

## Credits

[lengsukq/Coding Tools MCP](https://github.com/lengsukq/coding-tools-mcp)

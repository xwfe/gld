# gld — 让本地项目变成 AI 可直接开发的 MCP 工作区

`gld` 在后台跑**一个** MCP 服务（Model Context Protocol，说白了就是 AI 客户端调用外部工具的
通用接口），你的项目都挂在它下面。AI 客户端（ChatGPT、Claude Code、Cursor…）只配这一条
连接，就能在各个项目里读文件、改代码、跑命令、看 Git 状态，并把进度保存到项目里；
每次调用带一个 `workspace` 参数选项目，项目之间互不串。

*Run one MCP server that AI clients (ChatGPT, Claude Code, Cursor, Codex…) connect to
once, then develop in any of your local projects through it: read, patch, run commands,
inspect Git, and keep task progress inside each project. Docs are in Chinese.*

## 什么时候用

- **想让网页版 ChatGPT 直接改你电脑上的项目。** 它跑在 OpenAI 的服务器上，碰不到
  本地文件；gld 在本机起服务，再用一条隧道给它一个公网 HTTPS 地址。
- **本机的 Claude Code / Cursor / Codex 想共用同一套项目工具**：每个项目能读写的范围
  就是它自己的目录，能跑哪些命令可以用白名单收紧；还可以打开 Planning，让 AI 先出
  方案、你看过再放行。
- **手上好几个项目**：客户端里只配一条连接，项目加进来就能用，不用每个项目配一次。
- **项目在另一台机器上**，由 [ccnm](https://github.com/xwfe/ccnm) 管着：把它作为
  远端项目加进同一个服务。

## 装

到 [Releases](../../releases) 拿对应平台的包，解压后把 `gld` 放进 PATH：

```bash
tar xzf gld-*-aarch64-apple-darwin.tar.gz
sudo mv gld-*/gld ~/.local/bin  # /usr/local/bin/
gld --version
```

macOS 第一次运行会被 Gatekeeper 拦。其他平台、校验和、从源码装、升级、卸载
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

出问题先跑 `gld doctor`——它把配置里所有不自洽的地方列出来，
每一条下面直接写着该执行的命令。

> 2026-09-22 起 gld 只剩这一种用法（[RFC-0004](docs/rfc/0004-one-service-many-projects.md)）：
> 以前"一个项目一个服务"和"聚合入口 hub"两条路合成了一条。旧命令
> （`gld ws …`、`gld hub …`、`gld destroy`）还能敲，不进帮助。

## 查

| 我想…… | 看这里 |
| --- | --- |
| 搞清楚服务和项目、工具集、Planning 模式这些名词是什么 | [concepts.md](docs/concepts.md) |
| 按名字关掉某个工具、只让 AI 看到有用的 skill（`~/.agents/mcp.json`） | [concepts.md](docs/concepts.md#按名字再关掉工具和-skillagentsmcpjson) |
| 为什么项目之间不会串，代价是什么 | [concepts.md 为什么不会串](docs/concepts.md#为什么不会串) |
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

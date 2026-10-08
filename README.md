# gld

让 ChatGPT、Claude Code、Cursor、Codex 这些 AI 客户端，直接在**你电脑上的项目**里读代码、改代码、跑命令、看 Git。

gld 在本机后台跑一个 MCP 服务（MCP：AI 客户端调用外部工具的通用接口），你的项目都挂在它下面，
每个客户端只配一条连接。

*Let ChatGPT, Claude Code, Cursor or Codex work directly in the projects on your machine: one local MCP
server, all your projects behind it, one connection per client. Docs are in Chinese.*

## 适合你吗

**适合：**

- 想让**网页版 ChatGPT** 改你本机的项目。它跑在 OpenAI 的服务器上，碰不到你的文件；gld 在本机起服务，
  再给它一个公网 HTTPS 地址。
- 手上好几个项目，不想每个项目、每个客户端都配一遍。
- 想让 ChatGPT 也用上本机装好的 MCP server（context7、deepwiki……），或者操作另一台机器上由
  [ccnm](https://github.com/xwfe/ccnm) 管着的项目。

**不适合：**

- 需要沙箱隔离。AI 跑的命令以你的账号身份执行，gld 只做命令白名单这类静态检查，不是系统级隔离：
  凭据给了谁，就等于让谁能在你电脑上跑代码。
- 想要无人值守、从需求到上线全自动的开发平台。gld 管的是"AI 在你的项目里干活"这一段。

## 能做什么

- **读、改、跑**：读写文件、改之前先预检补丁、跑命令（gld 重启后也读得到命令的结局和最后一段输出）、查 Git。
- **先弄清项目怎么构建、怎么测**：列出项目里写好的命令和 CI 实际跑的步骤，AI 不用翻文档猜。
- **要紧的操作停下来问你**：装依赖、`rm -rf` 这类命令不带确认参数就不放行，提醒 AI 先问你。
- **给别人只开几个项目**：`gld grant` 发一份只能访问指定项目的凭据，默认只读，随时作废。
- **任务与验收**：记下任务、计划和历史；任务收尾只认任务期间跑通过、之后没再改过文件的命令当证据。

## 三步上手

**1. 装。** 到 [Releases](../../releases) 下载对应平台的包（macOS、Linux、Windows），解压后放进 PATH：

```bash
tar xzf gld-*-aarch64-apple-darwin.tar.gz        # Apple 芯片的 Mac；其他平台见安装文档
mkdir -p "$HOME/.local/bin"
install -m 755 gld-*/gld "$HOME/.local/bin/gld"
export PATH="$HOME/.local/bin:$PATH"
gld --version
```

macOS 上要是被拦（弹窗说无法验证开发者），核对校验和后执行 `xattr -d com.apple.quarantine ~/.local/bin/gld`；
新开终端找不到 `gld`、Windows 怎么装、从源码装，见 [docs/install.md](docs/install.md)。

**2. 加项目、起服务。**

```bash
gld start ~/code/my-project   # 起服务并加进这个项目；端口、凭据自动生成，关掉终端也照常跑
gld add ~/code/another        # 再加一个，立即生效
gld ls                        # 客户端要填的地址和凭据
```

**3. 接客户端。** Claude Code、Cursor、Codex 填 `gld ls` 给的本地地址（认证怎么配见
[connect-clients.md](docs/connect-clients.md#本机客户端claude-codecursorcodex-等)）。ChatGPT 只能连公网 HTTPS，要先拿一个公网地址：

```bash
gld share                     # Cloudflare 临时地址，要先装 cloudflared
```

临时地址在服务重启后会变，ChatGPT 里的连接器就得删了重建；长期用请换固定域名。每种客户端具体怎么填、
固定域名怎么配，见 [docs/connect-clients.md](docs/connect-clients.md)。

> **开公网入口前先读 [docs/security.md](docs/security.md)**：服务的主凭据能访问你加进来的全部项目。

出问题先跑 `gld doctor`，每个问题下面都写着该执行的命令；再不行查 [docs/troubleshooting.md](docs/troubleshooting.md)。

## 文档

| 想知道 | 看这里 |
| --- | --- |
| 各客户端怎么接、固定域名 | [connect-clients.md](docs/connect-clients.md) |
| 安装、升级、卸载 | [install.md](docs/install.md) |
| AI 能碰什么、怎么收紧 | [security.md](docs/security.md) |
| 报错怎么办 | [troubleshooting.md](docs/troubleshooting.md) |
| 项目、工具集、Planning、grant 这些概念 | [concepts.md](docs/concepts.md) |
| 拿它从需求做到上线，哪些靠得住、哪些还得人管 | [project-lifecycle.md](docs/project-lifecycle.md) |
| 后台守护进程、开机自启 | [daemon.md](docs/daemon.md) |
| 每个命令的全部参数 | [cli.md](docs/cli.md)，或 `gld <命令> --help` |
| 每一版改了什么、升级后要不要动 ChatGPT | [docs/releases/](docs/releases/) |
| 以前用桌面版 | [migrate-from-desktop.md](docs/migrate-from-desktop.md) |

参与开发：[架构](docs/architecture.md)、[开发与发布](docs/development.md)、[当前任务](task_plan.md)。

## Credits

[lengsukq/Coding Tools MCP](https://github.com/lengsukq/coding-tools-mcp)

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

**不适合：**

- 需要沙箱隔离。AI 跑的命令就是用你的账号在跑，gld 只做命令白名单这类检查，不是系统级隔离：
  凭据给了谁，就等于让谁能在你电脑上跑代码。
- 想要从需求到上线全自动、没人看着的开发平台。

## 能做什么

- **读、改、跑、查 Git**：补丁先预检再写；命令跑完的结果，gld 重启后也查得到。
- **先搞清项目怎么构建、怎么测**：列出项目里写好的命令和 CI 实际跑的步骤（[详情](docs/concepts.md#项目怎么构建怎么测list_project_commands)）。
- **起 dev server、用浏览器看页面**（[怎么配](docs/concepts.md#用浏览器验页面单独配一个隔离的-playwright-mcp)）。
- **用本机装好的 Skills 和 MCP server**（[Skill](docs/concepts.md#本机和项目里的-skill)、[MCP server](docs/concepts.md#本机装好的-mcp-server)）。
- **要紧的命令先问你**：装依赖、`rm -rf` 这类不带确认就不放行（[详情](docs/concepts.md#装东西的命令要你点头)）。嫌烦可以按项目一键全放行，
  风险自担，AI 会被告知并要它转告你（[怎么开、风险在哪](docs/concepts.md#一键放行全部命令command-policyall)）。
- **只给别人开几个项目**：`gld grant` 发一份只能碰指定项目的凭据，默认只读，随时作废（[详情](docs/concepts.md#只开几个项目gld-grant)）。
- **任务收尾要证据**：只认任务期间真跑通过的命令，AI 说"测过了"不算（[详情](docs/concepts.md#任务怎么收尾带证据才算-completed)）。
- **把活委派给本机的 Codex、Claude Code、Qwen、Antigravity、Pi、Kimi**：做完交回 AI 审核；可以一键批准它做任何操作，
  也可以让它自行判断，别的 Agent 加一条配置就能接。默认关
  （[怎么用](docs/concepts.md#能用哪些-agent怎么放权)、[风险](docs/security.md#委派出去的-agent-是全权跑的)）。
- **接另一台机器上的项目**：由 [ccnm](https://github.com/xwfe/ccnm) 管着的项目也能挂进来（[详情](docs/concepts.md#成员还可以在别的机器上)）。

## 三步上手

**1. 装。** 到 [Releases](../../releases) 下载对应平台的包，解压后放进 PATH（下面是 Apple 芯片的 Mac）：

```bash
tar xzf gld-*-aarch64-apple-darwin.tar.gz
mkdir -p "$HOME/.local/bin"
install -m 755 gld-*/gld "$HOME/.local/bin/gld"
export PATH="$HOME/.local/bin:$PATH"
gld --version
```

别的平台、macOS 弹窗说"无法验证开发者"、新开终端找不到 `gld`，见 [install.md](docs/install.md)。

发布包目前到 0.8.3。委派给本机 Agent、DeepSeek 网页、Claude Code 插件带的 skill 还没发版，要用得[从源码装](docs/install.md#从源码装)。

**2. 加项目、起服务。**

```bash
gld start ~/code/my-project   # 起服务并加进这个项目；端口、凭据自动生成，关掉终端也照常跑
gld add ~/code/another        # 再加一个，立即生效
gld ls                        # 客户端要填的地址和凭据
```

**3. 接客户端。** Claude Code、Cursor、Codex 填 `gld ls` 给的本地地址。ChatGPT 只能连公网 HTTPS，先拿一个公网地址：

```bash
gld share                     # Cloudflare 临时地址，要先装 cloudflared
```

临时地址服务一重启就变，ChatGPT 里的连接器就得删了重建，长期用请换固定域名。每种客户端怎么填、
固定域名怎么配、DeepSeek 网页怎么接，见 [connect-clients.md](docs/connect-clients.md)。

> **开公网入口前先读 [security.md](docs/security.md)**：服务的主凭据能访问你加进来的全部项目。

出问题先跑 `gld doctor`，每个问题下面都写着该执行的命令；还不行查 [troubleshooting.md](docs/troubleshooting.md)。

## 文档

| 想知道 | 看这里 |
| --- | --- |
| 各客户端怎么接、固定域名 | [connect-clients.md](docs/connect-clients.md) |
| 安装、升级、卸载 | [install.md](docs/install.md) |
| AI 能碰什么、怎么收紧 | [security.md](docs/security.md) |
| 报错怎么办 | [troubleshooting.md](docs/troubleshooting.md) |
| 上面每个功能具体怎么用 | [concepts.md](docs/concepts.md) |
| 拿它从需求做到上线，哪些靠得住、哪些还得人管 | [project-lifecycle.md](docs/project-lifecycle.md) |
| 后台守护进程、开机自启 | [daemon.md](docs/daemon.md) |
| 每个命令的全部参数 | [cli.md](docs/cli.md)，或 `gld <命令> --help` |
| 每一版改了什么、升级后要不要动 ChatGPT | [docs/releases/](docs/releases/) |
| 以前用桌面版 | [migrate-from-desktop.md](docs/migrate-from-desktop.md) |

参与开发：[架构](docs/architecture.md)、[开发与发布](docs/development.md)、[当前任务](task_plan.md)。

## Credits

[lengsukq/Coding Tools MCP](https://github.com/lengsukq/coding-tools-mcp)

# gld — 让本地项目变成 AI 可直接开发的 MCP 工作区

把项目目录登记成"工作区"，`gld` 就在后台跑一个 MCP（Model Context Protocol）服务。
AI 客户端（ChatGPT、Claude Code、Cursor…）连上来就能读文件、改代码、跑命令、
看 Git 状态，并把进度保存到项目里。

## 装

到 [Releases](../../releases) 拿对应平台的包，解压后把 `gld` 放进 PATH：

```bash
tar xzf gld-*-aarch64-apple-darwin.tar.gz
sudo mv gld-*/gld /usr/local/bin/   # ~/.local/bin
gld --version
```

macOS 第一次运行会被 Gatekeeper 拦。其他平台、校验和、从源码装、升级、卸载
见 [docs/install.md](docs/install.md)。

## 用

```bash
gld start ~/code/my-project   # 登记 + 启动：自动分配端口、生成密钥，什么都不用填
gld list                      # 拿到给客户端用的地址、凭据和隧道状态（gld ls 也行）
```

`gld start` 不带目录就是当前目录。目录没登记过会自动登记，
并在输出第一行告诉你；登记错了用 `gld destroy <名称>` 撤掉，
项目文件不会被动。守护进程会自动在后台拉起，关掉终端服务照常在。

本机客户端（Claude Code、Cursor、Codex）直接填 `gld list` 给出的**本地地址**。

ChatGPT 跑在 OpenAI 的服务器上，只能连公网 HTTPS，`127.0.0.1` 填进去连不上。
已经有公网地址（自建反代、公司域名）就在启动时一起给：

```bash
gld start ~/code/my-project --tunnel https://mcp.example.com/mcp
```

没有的话，一条命令借一个：

```bash
gld share                              # Cloudflare 临时地址；需要 cloudflared 在 PATH 里
gld share --tunnel cf:mcp.example.com  # 自己有域名和 Cloudflare 隧道时，地址固定
```

地址后来变了、项目换了目录、端口要改——`gld upgrade` 改完自动重启：

```bash
gld upgrade --tunnel https://new.example.com/mcp
gld upgrade --path ~/code/my-project-v2
```

不用了就收摊：

```bash
gld stop                      # 停当前工作区的服务（--all 停所有工作区的）
gld destroy                   # 连配置和密钥一起删掉；项目文件不动
```

> **开公网入口前请先读 [docs/security.md](docs/security.md)。**
> 它等于把"以你的身份在你电脑上跑命令"这件事对外开放了。

出问题先跑 `gld doctor`——它把配置里所有不自洽的地方列出来，
每一条下面直接写着该执行的命令。

## 查

| 我想…… | 看这里 |
| --- | --- |
| 搞清楚共享密钥池、工具集、Planning 模式这些名词是什么 | [concepts.md](docs/concepts.md) |
| 接到 ChatGPT / Claude Code / Cursor / 自定义 GPT | [connect-clients.md](docs/connect-clients.md) |
| 知道 AI 到底能碰什么，以及怎么收紧 | [security.md](docs/security.md) |
| 照着报错找处理办法 | [troubleshooting.md](docs/troubleshooting.md) |
| 查某个命令的全部参数 | [cli.md](docs/cli.md)，或直接 `gld <命令> --help` |
| 弄明白后台那个守护进程 | [daemon.md](docs/daemon.md) |
| 装 / 升级 / 卸载 | [install.md](docs/install.md) |
| 从桌面版迁移过来 | [migrate-from-desktop.md](docs/migrate-from-desktop.md) |
| 改这个项目的代码 | [architecture.md](docs/architecture.md)、[development.md](docs/development.md) |

## Credits

[lengsukq/Coding Tools MCP](https://github.com/lengsukq/coding-tools-mcp)

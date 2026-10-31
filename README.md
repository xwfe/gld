# gld

让 ChatGPT、Claude Code、Cursor、Codex 直接在**你电脑上的项目**里读代码、改代码、跑命令、看 Git。

不管有几个项目，每个 AI 客户端只配一条连接。AI 用哪个项目，它每次调用时自己点名。

> **AI 跑的命令就是用你的账号在跑。** gld 会拦下危险命令、限制能碰的目录，但它不是沙箱。
> 连接凭据给了谁，就等于让谁能在你电脑上跑代码。开公网地址前先看 [security.md](docs/security.md)。

## 能做什么

- **让 AI 改代码、跑测试、查 Git**：还能列出项目写好的命令和 CI 实际跑的步骤，不用它瞎猜怎么构建、怎么测（[详情](docs/concepts.md#项目怎么构建怎么测list_project_commands)）。
- **让网页版 ChatGPT 改本机项目**：它跑在 OpenAI 的服务器上，碰不到你的文件，经 gld 给的公网地址就能碰到。
- **危险操作先问你**：装依赖、`rm -rf` 这类命令，AI 不带确认就跑不了。嫌烦可以按项目全部放行，[风险自担](docs/concepts.md#一键放行全部命令command-policyall)。
- **AI 说测过了不算**：任务收尾只认期间真跑通过的命令（[详情](docs/concepts.md#任务怎么收尾带证据才算-completed)）。
- **让 AI 起 dev server、用浏览器看页面**（[怎么配](docs/concepts.md#用浏览器验页面单独配一个隔离的-playwright-mcp)）。
- **让 AI 用你装好的 [Skill](docs/concepts.md#本机和项目里的-skill) 和 [MCP server](docs/concepts.md#本机装好的-mcp-server)**。
- **只给别人开几个项目**：发一份单独的凭据，默认只读，随时作废（[详情](docs/concepts.md#只开几个项目gld-grant)）。
- **把活交给本机的 Codex、Claude Code 等 Agent**：做完交回 AI 审核，默认关（[怎么用](docs/concepts.md#能用哪些-agent怎么放权)、[风险](docs/security.md#委派出去的-agent-是全权跑的)）。
- **操作另一台机器上的项目**：那台机器要用 [ccnm](https://github.com/xwfe/ccnm) 管着（[详情](docs/concepts.md#成员还可以在别的机器上)）。

## 第 1 步：安装

到 [Releases](../../releases) 下载对应平台的包。下面是 Apple 芯片的 Mac：

```bash
tar xzf gld-*-aarch64-apple-darwin.tar.gz
mkdir -p "$HOME/.local/bin"
install -m 755 gld-*/gld "$HOME/.local/bin/gld"
export PATH="$HOME/.local/bin:$PATH"
gld --version
```

别的平台、macOS 弹窗说"无法验证开发者"、新开终端报 `command not found: gld`，都在 [install.md](docs/install.md)。

下载包目前到 0.10.0，和 main 一致。

## 第 2 步：加项目

```bash
gld start ~/code/my-project   # 起服务并加进这个项目，关掉终端也照常跑
gld add ~/code/another        # 再加一个，立即生效
gld ls                        # 看地址、凭据和项目
```

## 第 3 步：接 AI 客户端

### Claude Code、Cursor、Codex

这些客户端和 gld 在同一台电脑上，用本地地址就行。先把认证改成 Bearer 令牌，再拿到令牌：

```bash
gld upgrade --auth bearer
gld secret ls bearer_token --reveal
```

已经接了 ChatGPT 的别改认证，否则 ChatGPT 的连接器要删了重建；先看[这里](docs/connect-clients.md#本机客户端claude-codecursorcodex-等)。

Claude Code 这样加，把 `<bearer_token>` 换成上面拿到的值，端口以 `gld ls` 的本地地址为准：

```bash
claude mcp add --transport http --scope user gld http://127.0.0.1:28764/mcp \
  --header "Authorization: Bearer <bearer_token>"
```

Cursor、Codex 的写法见 [connect-clients.md](docs/connect-clients.md#本机客户端claude-codecursorcodex-等)。

### ChatGPT

ChatGPT 只能连公网 HTTPS，先拿一个地址。这条命令用 Cloudflare 的临时地址，要先装 `cloudflared`：

```bash
gld share
gld ls --reveal               # 公网地址，和授权页要填的"授权口令"
```

然后在 ChatGPT 里：

1. 设置 → 账户安全与登录 → 打开"开发人员模式"。
2. 左侧"插件" → `+` 新建 → 选 MCP，粘贴公网地址（以 `/mcp` 结尾）。
3. 保存后进入授权页，填授权口令。上面把认证改成 Bearer 的，这里选 Bearer、填令牌。
4. 新建一个启用了这个插件的对话。

**临时地址只适合试用**：服务一重启地址就变，连接器只能删了重建。长期用请换[固定域名](docs/connect-clients.md#办法二cloudflare-固定域名)。

### 试一下通没通

把这句发给 AI：

> 先调用 list_workspaces，再对其中一个项目调用 git_status，告诉我它的目录和 Git 状态。

它答得出真实的目录和分支，就是通了。

## 日常用到的命令

| 想做什么 | 命令 |
| --- | --- |
| 加项目 | `gld add <目录>` |
| 移除项目（项目文件不动） | `gld rm <项目>` |
| 让某个项目只读 | `gld set <项目> tool-profile=read-only` |
| 只给别人开一个项目 | `gld grant add <名字> <项目>` |
| 看地址和凭据 | `gld ls --reveal` |
| 停服务 / 再起来 | `gld stop` / `gld start` |
| 电脑重启后拉起来 | `gld daemon start`，想省掉这步就配[开机自启](docs/daemon.md#开机自启) |
| 升级 gld | 换二进制再重启，连接器不用动，见[升级](docs/install.md#升级) |

## 出问题了

先跑 `gld doctor`，每个问题下面都写着该执行的命令。连不上再跑 `gld health`，它会说清本地和公网哪一段不通。
还不行查 [troubleshooting.md](docs/troubleshooting.md)。

## 更多文档

| 想知道 | 看这里 |
| --- | --- |
| 各客户端怎么接、固定域名、DeepSeek 网页 | [connect-clients.md](docs/connect-clients.md) |
| 安装、升级、卸载 | [install.md](docs/install.md) |
| AI 能碰什么、怎么收紧 | [security.md](docs/security.md) |
| 每个功能具体怎么用 | [concepts.md](docs/concepts.md) |
| 拿它从需求做到上线，哪些靠得住、哪些还得人管 | [project-lifecycle.md](docs/project-lifecycle.md) |
| 每个命令的全部参数 | [cli.md](docs/cli.md)，或 `gld <命令> --help` |
| 每一版改了什么、升级后要不要动 ChatGPT | [docs/releases/](docs/releases/) |
| 以前用桌面版 | [migrate-from-desktop.md](docs/migrate-from-desktop.md) |

想改 gld 本身：[开发与发布](docs/development.md)。

基于 [lengsukq/Coding Tools MCP](https://github.com/lengsukq/coding-tools-mcp)。

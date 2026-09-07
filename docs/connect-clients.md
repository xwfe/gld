# 连接 AI 客户端

先启动服务、再看连接信息，所有客户端都从这里取值：

```bash
gld start              # 或 gld start ~/code/my-project，目录没登记过会自动登记
gld list               # 加 --reveal 显示凭据明文
```

> 名词看不懂（共享密钥池、工具集、全局入口…）先翻 [concepts.md](concepts.md)。
>
> `gld ws set` 的字段名可以省掉 `mcp.` 前缀（`auth` 就是 `mcp.auth`），
> 改完会自动重启受影响且正在运行的服务，不用再敲 `gld restart`。
> 下面的例子都用简写。

## 本机客户端（Claude Code、Cursor、Codex 等）

用**本地地址** `http://127.0.0.1:<端口>/mcp`。这些客户端和服务在同一台机器上，不需要隧道。

推荐把认证改成 bearer，比 OAuth 少一次授权跳转：

```bash
gld ws set auth=bearer
gld secret show bearer_token --reveal
```

Claude Code：

```bash
claude mcp add --transport http coding-tools http://127.0.0.1:28766/mcp \
  --header "Authorization: Bearer <bearer_token>"
```

Cursor（`.cursor/mcp.json`）：

```json
{
  "mcpServers": {
    "coding-tools": {
      "url": "http://127.0.0.1:28766/mcp",
      "headers": { "Authorization": "Bearer <bearer_token>" }
    }
  }
}
```

只在自己机器上用、且没开局域网访问时，也可以 `auth=noauth` 省掉 header。
`gld settings runtime --lan-access true` 之后**不要**用 noauth——服务会监听 0.0.0.0。

连上后让客户端调用 `server_info`、`get_default_cwd`、`git_status`，能返回当前项目信息就说明通了。

## ChatGPT（MCP 连接器）

ChatGPT 在 OpenAI 的服务器上，必须通过**公网 HTTPS** 地址访问，`127.0.0.1` 填进去会连不上。

`gld share` 负责整件事：配好隧道、把服务拉起来、起隧道、打印地址和凭据。
公网入口用一个 `--tunnel` 参数表达，底下四种走法选一种。

（这四种写法在 `gld start` 和 `gld upgrade` 上完全一样：
启动时就想连公网用 `gld start <目录> --tunnel …`，之后要换地址用 `gld upgrade --tunnel …`。）

### 办法一：Cloudflare 临时地址（零配置，先试试用）

```bash
brew install cloudflared   # Windows: winget install Cloudflare.cloudflared
gld share                  # 公网地址形如 https://xxx.trycloudflare.com/mcp（等价 --tunnel cf）
```

**每次重启地址都会变**，ChatGPT 里要跟着改。适合试用，不适合长期。

### 办法二：Cloudflare 固定域名

需要一个 Cloudflare 账号和一条已创建的 Named Tunnel，域名连着一起给：

```bash
gld share --tunnel cf:mcp.example.com
```

token 没配过的话，这条命令会当场问你要（输入不显示）。不想交互——比如写在
脚本里——就直接带上：

```bash
gld share --tunnel cf:mcp.example.com --token <隧道 token>
```

写在命令行里的 token 会进 shell 历史，介意的话事先存起来：
`gld secret set cloudflare_token <隧道 token>`。

**域名必须给。** 固定域名模式下 gld 不像 quick 那样能从 cloudflared 的输出里
读回地址——这个地址要写进 OAuth 元数据和 OpenAPI 文档，cloudflared 那边也拿它
建 ingress。所以不给的话会当场报错告诉你怎么写，而不是等隧道起不来。

域名配过一次之后，`--tunnel cf:named` 就是"沿用已经配好的那个"；
`gld upgrade --tunnel cf:<新域名>` 换一个。写不写 `https://` 都行。

### 办法三：FRP 固定域名（自己有公网机器）

需要一台有公网 IP 的机器跑 frps，本机装 frpc（**要求 frp ≥ 0.52**，
gld 生成的是 TOML 配置，更早的版本只认 INI）：

```bash
brew install frpc          # Windows / Linux 见 https://github.com/fatedier/frp/releases
gld frp add --name 公司 --server frp.example.com --port 7000 --token <frps-token>
gld share --tunnel frp:公司   # 子域名默认取工作区名，要指定就加 --subdomain myproj
```

`frp:` 后面填的是上一步的名称（也认 id）。填了不存在的名字会当场报错并列出已有的配置。

frps 侧需要 `subdomain_host = frp.example.com` 并把 `*.frp.example.com` 解析到 frps，
HTTPS 由 frps 前面的反向代理（Caddy / Nginx）终结。
`gld tunnel snippet` 打印的是**客户端**（frpc）的完整配置，存成 `frpc.toml`
就能自己跑一份 frpc，不用 gld 代管；token 默认是占位符，加 `--reveal` 取真值。
frps 那边要配什么见 frp 官方文档。

### 办法四：已经有公网地址（自建反向代理）

自己用 Caddy / Nginx 把 `https://mcp.example.com` 转到本地端口时，gld 不需要起隧道，
只要知道对外地址是什么（它要写进 OAuth 元数据和 OpenAPI 文档里）：

```bash
gld share --tunnel https://mcp.example.com/mcp
```

地址末尾那个 `/mcp` 写不写都行：存下来的是基地址，端点是拼出来的，
所以从客户端复制过来的完整地址可以直接粘，不会变成 `…/mcp/mcp`。

### 关掉公网入口

```bash
gld share --off            # 停隧道、清掉公网地址，本地地址照常可用
```

### 多个项目共用一个域名：全局入口

不想每个工作区都配子域名时，开一个全局入口，所有工作区走 `/w/<工作区id>` 前缀。
这条路不走 `gld share`（入口是全局的，不属于某个工作区）：

```bash
gld gateway set --enabled true --tunnel cf                 # 或 --tunnel frp --frp-profile <id> --frp-subdomain hub
gld ws set global-gateway=true                            # 自动重启，start 时会把全局入口一起拉起来
gld list                                                  # 公网地址变成 https://<入口域名>/w/<id>/mcp
```

### 在 ChatGPT 里配置

1. 设置 → 账户安全与登录 → 打开“开发人员模式”（允许添加未验证的 MCP 连接器）。
2. 左侧“插件” → `+` 新建 → 选 MCP，粘贴 `gld list` 里的**公网地址**（以 `/mcp` 结尾）。
3. 认证方式和工作区一致：
   - `oauth`（默认）：ChatGPT 支持动态注册，通常不用填 Client ID / Secret；
     保存后进入授权页，输入 `gld list --reveal` 里的**授权口令**（`oauth_password`）。
   - `bearer`：选 Bearer，填 `bearer_token`。
4. 新建一个启用了该插件的对话，发送：
   “请调用 server_info、get_default_cwd 和 git_status，告诉我当前工作区、目录和 Git 状态。”

看不到新增工具时，断开重连插件或新开对话。连不上先跑 `gld health`，
它会分别告诉你本地 `/mcp`、公网 `/mcp`、OAuth 元数据哪一项不通。

## 自定义 GPT（GPT Actions）

不支持 MCP 连接器的场景，用 OpenAPI 网关：

```bash
gld start -s actions
gld list                          # 看 “GPT Actions” 段的 OpenAPI 地址与 API Key
```

在 GPT 编辑器的 Actions 页面 “Import from URL” 粘贴 OpenAPI 地址；
认证选 API Key（Bearer），值为 `actions_api_key`。

Actions 也要公网地址时，`gld share` 加 `-s actions`（上面四种走法都适用）：

```bash
gld share -s actions
```

## 认证方式对照

| `mcp.auth` | 客户端要提供什么 | 适合 |
| --- | --- | --- |
| `oauth` | 授权页口令（PKCE + 动态注册；老客户端可填静态 Client ID / Secret） | ChatGPT、任何走标准 OAuth 的客户端 |
| `bearer` | `Authorization: Bearer <bearer_token>` | 本机 / 可信网络里的 CLI 与编辑器 |
| `noauth` | 无 | 只监听 127.0.0.1 的本机试用 |

改认证方式（`gld ws set auth=…`）和改密钥（`gld secret set/regen`）都会自动重启相关服务，
不需要手动 `gld restart`。

**`noauth` 只在没有公网入口时才叫"本机试用"。** 隧道是从 127.0.0.1 把端口转出去的，
绑回环地址挡不住它——`noauth` + 隧道等于把"在你电脑上跑命令"这件事开放给全互联网。
`gld doctor` 会把这个组合报成 ✗。开公网入口之前请读一遍 [security.md](security.md)。

## 多个工作区

每个工作区有自己的端口、认证、隧道和密钥，互不影响。

接了好几个项目、不想在客户端里存好几份凭据时，可以让它们共用同一套 —— 见
[concepts.md 共享密钥池](concepts.md#共享密钥池shared-secrets)（那里也写了代价）。

# 连接 AI 客户端

gld 只有**一个 MCP 服务**，你的项目都挂在它下面。客户端里只配这一条连接，AI 每次调用
用 `workspace` 参数（项目名或 id）选项目。先把服务起来、再看连接信息，所有客户端都从这里取值：

```bash
gld start ~/code/api       # 起服务，并把这个目录加进来
gld add ~/code/web         # 再加一个项目，服务在跑就立即生效
gld ls                     # 地址、凭据、项目表；加 --reveal 显示凭据明文
```

> 名词看不懂（工具集、公网入口、Planning…）先翻 [concepts.md](concepts.md)。
>
> 改服务的端口、认证用 `gld upgrade`，改公网入口用 `gld share`；它们改完会自动重启服务，
> 不用再敲 `gld restart`。改某个项目的配置用 `gld set <项目> key=value`，下一次调用就生效。

## 本机客户端（Claude Code、Cursor、Codex 等）

用**本地地址** `http://127.0.0.1:<端口>/mcp`（默认端口 28764，`gld ls` 里那一行）。
这些客户端和服务在同一台机器上，不需要隧道。

推荐把认证改成 bearer，比 OAuth 少一次授权跳转：

```bash
gld upgrade --auth bearer
gld secret ls bearer_token --reveal
```

Claude Code：

```bash
claude mcp add --transport http gld http://127.0.0.1:28764/mcp \
  --header "Authorization: Bearer <bearer_token>"
```

Cursor（`.cursor/mcp.json`）：

```json
{
  "mcpServers": {
    "gld": {
      "url": "http://127.0.0.1:28764/mcp",
      "headers": { "Authorization": "Bearer <bearer_token>" }
    }
  }
}
```

只在自己机器上用、且没开局域网访问时，也可以 `gld upgrade --auth noauth` 省掉 header。
`gld cfg runtime --lan-access true` 之后**不要**用 noauth——服务会监听 0.0.0.0。

连上后发给 AI：

> 先调用 list_workspaces，再分别对 api 和 web 调用 git_status，告诉我两个项目各自的分支和路径。

两个结果路径不同就是通了。报错对照见 [troubleshooting.md](troubleshooting.md)。

## ChatGPT（MCP 连接器）

ChatGPT 在 OpenAI 的服务器上，必须通过**公网 HTTPS** 地址访问，`127.0.0.1` 填进去会连不上。

`gld share` 负责整件事：给服务配好公网入口、把服务拉起来、起隧道、打印地址和凭据。
公网入口用一个 `--tunnel` 参数表达，底下四种走法选一种。

（这四种写法在 `gld start` 和 `gld upgrade` 上完全一样：
启动时就想连公网用 `gld start --tunnel …`，之后要换地址用 `gld upgrade --tunnel …`。）

**`gld share` 不带 `--tunnel` 时沿用已经配好的入口**；一个都没配过才用 Cloudflare 临时地址。

### 办法一：Cloudflare 临时地址（零配置，先试试用）

```bash
brew install cloudflared   # Windows: winget install Cloudflare.cloudflared
gld share                  # 公网地址形如 https://xxx.trycloudflare.com/mcp（等价 --tunnel cf）
```

**每次服务重启地址都会变**，ChatGPT 里要跟着改。适合试用，不适合长期。重复敲
`gld share` / `gld start` 不会重启服务，地址不会因此变；`gld restart`、改端口、改认证、
重启电脑会。

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
读回地址——这个地址要写进 OAuth 元数据，cloudflared 那边也拿它建 ingress。
所以不给的话会当场报错告诉你怎么写，而不是等隧道起不来。

域名配过一次之后，`--tunnel cf:named` 就是"沿用已经配好的那个"；
`gld upgrade --tunnel cf:<新域名>` 换一个。写不写 `https://` 都行。

#### 云端的回源端口要和服务端口一致

固定隧道把请求转到本机哪个端口（回源地址），是 **Cloudflare 云端那份配置**说了算。
Tunnel Token 只用来连上隧道，gld 不会、也没法替你把云端的端口改成服务的端口。
假设云端回源填的是 `http://127.0.0.1:28767`，把服务端口对齐：

```bash
gld upgrade --port 28767
```

`--port` 只改本地监听端口，不动云端；已经连着的本机客户端也要跟着换端口。
两边对不上时，隧道进程照样在跑，公网却是 502——看着像 gld 坏了，
其实是云端把请求转到了一个没人监听的端口。

所以固定隧道起来之后 gld 会自己访问一次公网地址：不通就返回非零退出码，并在报错里
写出它预期的回源地址。已经起来的服务和隧道会留着，方便你改完云端配置直接复查。
两点别误会：公网有响应不等于 OAuth 全流程能走通；要确认就跑 `gld health`。

`start`、`share`、`upgrade` 三个命令给 token 都用 `--token`；旧写法 `--tunnel-token`
仍然认。

### 办法三：FRP 固定域名（自己有公网机器）

需要一台有公网 IP 的机器跑 frps，本机装 frpc（**要求 frp ≥ 0.52**，
gld 生成的是 TOML 配置，更早的版本只认 INI）：

```bash
brew install frpc          # Windows / Linux 见 https://github.com/fatedier/frp/releases
gld frp add --name 公司 --server frp.example.com --port 7000 --token <frps-token>
gld share --tunnel frp:公司   # 子域名默认 gld，要指定就加 --subdomain mcp
```

公网地址是 `https://<子域名>.<frps 域名>/mcp`。`frp:` 后面填的是上一步的名称（也认 id）。
填了不存在的名字会当场报错并列出已有的配置。

frps 侧需要 `subdomain_host = frp.example.com` 并把 `*.frp.example.com` 解析到 frps，
HTTPS 由 frps 前面的反向代理（Caddy / Nginx）终结。frps 那边要配什么见 frp 官方文档。
**两台机器上的 gld 连同一台 frps 时，子域名要错开**（都用默认的 gld 会撞）。

### 办法四：已经有公网地址（自建反向代理）

自己用 Caddy / Nginx 把 `https://mcp.example.com` 转到服务的本地端口时，gld 不需要起隧道，
只要知道对外地址是什么（它要写进 OAuth 元数据）：

```bash
gld share --tunnel https://mcp.example.com/mcp
```

地址末尾那个 `/mcp` 写不写都行：存下来的是基地址，端点是拼出来的，
所以从客户端复制过来的完整地址可以直接粘，不会变成 `…/mcp/mcp`。
反代要把 `/.well-known/` 也转过来，否则 OAuth 客户端找不到授权页。

### 关掉公网入口

```bash
gld share --off            # 停隧道、清掉公网地址，本地地址照常可用
```

### 在 ChatGPT 里配置

1. 设置 → 账户安全与登录 → 打开“开发人员模式”（允许添加未验证的 MCP 连接器）。
2. 左侧“插件” → `+` 新建 → 选 MCP，粘贴 `gld ls` 里的**公网地址**（以 `/mcp` 结尾）。
3. 认证方式和服务一致：
   - `oauth`（默认）：ChatGPT 支持动态注册，通常不用填 Client ID / Secret；
     保存后进入授权页，输入 `gld ls --reveal` 里的**授权口令**（`oauth_password`）。
   - `bearer`：选 Bearer，填 `bearer_token`。
4. 新建一个启用了该插件的对话，发送：
   “请先调用 list_workspaces，再对其中一个项目调用 git_status，告诉我它的目录和 Git 状态。”

看不到新增工具时，要点一次 Refresh，只开新对话没用，见下面
[装好的连接器什么时候要动](#装好的连接器什么时候要动)。连不上先跑 `gld health`，
它会分别告诉你本地 `/mcp`、公网 `/mcp`、OAuth 元数据哪一项不通。

**服务挂了公网就不能用 `noauth`**，gld 会直接拒——那等于把全部项目的执行权限开放给整个互联网。

### 装好的连接器什么时候要动

ChatGPT 的连接器（网页上叫"插件"）建好之后，大多数操作都碰不到它。会碰到的分三档，从轻到重：
**点 Refresh**、**重新授权**、**删了重建**。先看你要做的事在哪一档。

**什么都不用做**：

- 升级 gld，而且升级前后 `gld tool list --served` 的指纹没变（怎么比见[安装 · 升级](install.md#升级)）
- `gld restart`、`gld stop` + `gld start`、守护进程重启——固定公网地址下没影响
- 重启电脑：配了[开机自启](daemon.md#开机自启)就什么都不用管；没配的话守护进程不会自己回来，期间
  ChatGPT 只报连不上，**连接器不用删**，`gld daemon start` 一次（上次 `gld start` 过的服务会跟着回来）
  就好了。2026-09-24 本机重启过一次就是这样：cloudflared 是 launchd 管的、自己回来了，gld 没配自启，
  `gld daemon start` 之后凭据、注册的客户端逐项核对都没变
- `gld add` / `gld rm` / `gld set`（改项目，包括项目自己的 `tool-profile`）——服务每次调用都重新读项目表；
  项目的工具集只在调用时生效，服务发给客户端的工具表不变
- `gld grant add`——已经连好的连接器不受影响
- `gld secret regen oauth_password` / `gld secret set oauth_password`——口令只管"下次授权填什么"，
  已经授权过的照常能用
- 长时间不用：访问令牌 30 天有效，刷新令牌 90 天，期间用过一次就自动续上

**要点 Refresh**——服务发给客户端的工具表变了。ChatGPT 只在建连接器和点 Refresh 时拉工具表
（`tools/list`），开新对话、gld 重启都不会让它重拉：

| 操作 | 工具表怎么变 |
| --- | --- |
| 升级 gld，升级前后指纹不一样 | 新版加了、删了工具或参数，或者改了说明 |
| `gld mcp on` 开第一个 / `gld mcp off` 关掉最后一个 | 多出 / 少了 `list_mcp_tools` 这三个 |
| `gld remote add` 加第一个远端项目 / 删掉最后一个；有了第一个 / 没了最后一个 `--mode coding` 的远端项目 | 多出 / 少了 `remote_*` 那几个 |
| `gld upgrade --tool-profile …` | 整张表换了 |

不刷新的后果：新工具、新参数 AI 看不见；已经没有的工具 AI 还会去调，拿到 `Unknown tool`。怎么刷：

1. 打开 <https://chatgpt.com/plugins>（要先开开发者模式：设置 → Security and login → Developer mode），
   点进 gld 这条连接，点 **Refresh**。**不用删，授权也不用重来。**
2. 开一个新对话：旧对话继续用旧表。
3. 核对刷新真的到了 gld：`gld logs -n 50` 里依次有 `method=server/discover`（回 Method not found，正常，
   见[排障](troubleshooting.md#客户端连不上)）、`method=initialize`、`method=tools/list`，`auth=` 后面还是
   原来那个 `oauth:hub:dcr-…`。
4. 核对 ChatGPT 手上的表：在新对话里问它**它自己看到的**工具定义里有没有这次新加的东西，要它逐条答有或
   没有。别拿 `server_info` 里的 `connection.tools_fingerprint` 当证据：那是 gld 按自己发出去的表算的，
   ChatGPT 用旧表时它照样对得上（详见[排障 · 核对客户端拿到的工具表](troubleshooting.md#核对客户端拿到的工具表)）。

用 grant 连的连接器看到的是它那一份（只读的更少），要刷也是各刷各的。

**要重新授权，但连接器留着**——ChatGPT 会自己提示重新连接，在授权页输一次口令即可：

| 操作 | 为什么 | 授权页填什么 |
| --- | --- | --- |
| `gld secret regen oauth_token_secret` | 令牌的签名密钥换了，已发的令牌全部作废 | 服务口令 |
| 超过 90 天没用过 | 刷新令牌也过期了 | 原来那个口令 |
| `gld grant rm` 之后又建了一把给同一个客户端 | 旧 grant 的令牌全部作废 | 新 grant 的口令 |

口令想自己定一个记得住的，别每次去查：`gld secret set oauth_password 你自己的口令`。

**要删了重建**：

| 操作 | 为什么 |
| --- | --- |
| 公网地址变了：用 `--tunnel cf` 临时地址时服务一重启、`gld upgrade --tunnel cf:新域名`、换 frp 子域名 | ChatGPT 的连接器改不了地址 |
| 认证方式变了（`gld upgrade --auth …`，oauth ↔ bearer） | 连接器要按新方式配；ChatGPT 里建好的连接器能不能改认证方式没实测过，改不了就只能重建 |
| 数据目录没了（删了 `~/.config/gld`、换了 `GLD_HOME`、换了台机器又没带备份） | 口令、签名密钥、ChatGPT 注册的客户端都在里面，没有第二份 |
| `gld grant rm` 之后不再给这个客户端开 | 它的令牌 401，留着也用不了 |

**所以长期用就选固定地址**（`cf:mcp.example.com`、frp 固定子域名、自建反代），别动数据目录，
也别换认证方式——上面最重的两档就都碰不到了。改端口（`gld upgrade --port …`）不在此列：公网地址
没变，连接器照常能用，但隧道那边的回源端口要跟着改，否则是连不上（502），不是认证问题。

## 另一台机器上的项目

项目在另一台机器上、并且那台机器用 [ccnm](https://github.com/xwfe/ccnm) 管着时，
把它作为远端项目加进同一个服务，客户端那边什么都不用改：

```bash
gld remote add prod --node work --remote-workspace server
```

远端项目的工具是另一套（`remote_*`），怎么用、有什么限制见
[concepts.md](concepts.md#成员还可以在别的机器上)。

## 自定义 GPT（GPT Actions）

不支持 MCP 连接器的场景（自定义 GPT 的 Actions），用 OpenAPI 网关。**它还是一个项目一个**：
自定义 GPT 导入的是一个项目的 OpenAPI 文档，服务没有 OpenAPI 版。

```bash
gld start -s actions       # 在项目目录里；起的是这个项目的 GPT Actions
gld ls <项目>              # 看 “GPT Actions” 段的 OpenAPI 地址与 API Key
```

在 GPT 编辑器的 Actions 页面 “Import from URL” 粘贴 OpenAPI 地址；
认证选 API Key（Bearer），值为 `gld secret ls actions_api_key --reveal -w <项目>`。

Actions 也要公网地址时，`gld share` 加 `-s actions`（上面四种走法都适用，这条隧道属于那个项目）：

```bash
gld share -s actions
```

Actions 那条线路的字段（端口、认证、隧道）写成 `actions.<字段>`：`gld fields --all`。

## 认证方式对照

| 服务认证（`gld upgrade --auth …`） | 客户端要提供什么 | 适合 |
| --- | --- | --- |
| `oauth`（默认） | 授权页口令（PKCE + 动态注册；老客户端可填静态 Client ID） | ChatGPT、任何走标准 OAuth 的客户端 |
| `bearer` | `Authorization: Bearer <bearer_token>` | 本机 / 可信网络里的 CLI 与编辑器 |
| `noauth` | 无 | 只监听 127.0.0.1、没有公网入口的本机试用 |

改认证方式和改凭据（`gld secret set/regen`）都会自动重启服务，不需要手动 `gld restart`。

**`noauth` 只在没有公网入口时才叫"本机试用"。** 隧道是从 127.0.0.1 把端口转出去的，
绑回环地址挡不住它。开公网入口之前请读一遍 [security.md](security.md)。

**一把钥匙开所有项目的门。** 拿到服务凭据的人能进服务里的全部项目，调一次
`list_workspaces` 就知道有哪几个。只想给别人开几个项目，用
[gld grant](concepts.md#只开几个项目gld-grant) 发一把单独的口令或令牌。

## 老配置：经全局入口挂公网

以前服务（那时叫聚合入口 hub）上公网还有一条路：经全局入口，地址是
`<入口域名>/hub/mcp`。这样配过的老配置照旧能用（`gld ls` 的"公网入口"一行会写
"经全局入口"），但它不再出现在帮助里：服务现在有自己的隧道，`gld share --tunnel …`
一条命令就够，地址也更短。要换过去：

```bash
gld share --tunnel frp:公司     # 或 cf:<域名>、https://…；会同时关掉经全局入口那条路
```

换了公网地址，ChatGPT 里的连接器要删了重建（见上面那张表）。

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

**标准 `/mcp` 入口的认证方式整个服务只有一种，本机客户端和 ChatGPT 共用**，默认是 oauth。只接本机客户端、不接 ChatGPT 的，
改成 bearer 最省事，少一次授权跳转：

```bash
gld upgrade --auth bearer
gld secret ls bearer_token --reveal
```

**已经接了 ChatGPT 的别改。** ChatGPT 走 OAuth，服务改成 bearer 之后它的请求一律 401，连接器只能删了重建
（见[装好的连接器什么时候要动](#装好的连接器什么时候要动)）。保持 oauth 的话，支持 OAuth 的本机客户端连上时会打开
授权页，填 `gld ls --reveal` 里的"授权口令"；这条路还没在 Claude Code、Cursor、Codex 上实测过。

下面的配置按 bearer 写。

Claude Code（`--scope user` 让所有目录都能用；不加的话只在当前目录生效，换个项目目录就看不到 gld）：

```bash
claude mcp add --transport http --scope user gld http://127.0.0.1:28764/mcp \
  --header "Authorization: Bearer <bearer_token>"
```

Codex（令牌从环境变量读，启动 Codex 的那个 shell 里没有这个变量就连不上）：

```bash
export GLD_BEARER_TOKEN=<bearer_token>     # 写进 ~/.zshrc 才会一直生效
codex mcp add gld --url http://127.0.0.1:28764/mcp --bearer-token-env-var GLD_BEARER_TOKEN
```

Cursor（写进全局的 `~/.cursor/mcp.json`；项目里的 `.cursor/mcp.json` 只对那一个项目生效）：

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

## DeepSeek 网页与 SuperAssistant

DeepSeek 网页自己没有 MCP 连接器，靠 Chrome 扩展
[MCP SuperAssistant](https://chromewebstore.google.com/detail/mcp-superassistant/kngiafgkdnlkgmefdafaibkibegkcaef)：
扩展认出模型写出来的工具调用，替它请求 gld，再把结果填回对话。**它直接连你现有的 gld 服务**——同一个端口、
同一批项目，不另起服务，也不用装扩展里提示的 `npx @srbhptl39/mcp-superassistant-proxy`；ChatGPT 等已经连好的
连接器不受影响。

> 要 0.9.0 或更新的版本才有 `gld grant browser`。换二进制按[安装 · 升级](install.md#升级)走，
> 命令行和守护进程都要是新的：只换命令行、守护进程还是旧的，会报版本不一致（退出码 4）。

**为什么要单独发一个地址**：SuperAssistant 的连接设置只有 Connection Type 和 Server URI 两栏，填不了请求头，
也不走 OAuth 授权（按 0.6.0 源码核对）。把标准 `/mcp` 地址填进去只会 401。`gld grant browser` 生成一个
**凭据就在路径里**的地址 `/mcp/browser/<随机密钥>`，挂在同一个服务上：短期有效、只开你点名的项目、默认只读。

### 接上

1. 在本机终端生成地址。`web-mcp` 是这把授权的名字，在 `grant ls` / `grant rm` 里用它认；
   后面跟要开放的项目名，可以列多个，或者用 `--all` 开服务里现有的全部项目：

   ```bash
   gld grant browser web-mcp gld --ttl 8h
   ```

   ```bash
   gld grant browser web-mcp --all --ttl 8h
   ```

   `--all` 开的是此刻服务里的项目（`gld ls` 项目表那些），之后新登记的不会自动进来，要用就 `gld grant rm web-mcp` 再重建。
   项目名和 `--all` 不能同时给。全开时默认照样只读；`--all --write` 等于把这些项目的改文件和跑命令全交给浏览器扩展。

   "本地连接地址"那一行形如 `http://127.0.0.1:28764/mcp/browser/<随机密钥>`，端口就是服务的端口（`gld ls` 里那个）。
   服务配了 HTTPS 公网地址时还会多一行"公网连接地址"；Cloudflare 临时地址不在此列。浏览器和 gld 在同一台机器就用本地地址。
   **整串地址只显示这一次**，`gld grant ls --reveal` 也找不回来——数据文件里只存了它的 SHA-256 摘要。

2. 在 DeepSeek 页面打开 SuperAssistant 侧栏 → Server Configuration：**Connection Type** 选 **Streamable HTTP**
   （默认的 Server-Sent Events (SSE) 不行），**Server URI** 粘贴整串地址，再点连接。
3. 连上以后工具列表里应该有 `list_workspaces`、`read_file` 等。在对话里用扩展的 Insert / Attach Instructions，
   把工具说明放进这一轮对话。
4. 先手动验一遍：调 `list_workspaces`，再对授权的项目调 `workspace_context` 和 `read_file`。看到真实的文件内容、
   结果填回了对话、DeepSeek 接着往下说，才算通了；只显示 Connected 不算。

首轮提示可以用这段（项目名按实际改）：

> 先调用 list_workspaces，选中 gld 项目，再调用 workspace_context。后续操作遵循它返回的项目规则，
> 每次项目工具调用都明确填写 workspace。长命令用 yield_time_ms=1000 返回 session_id，再用 read_output 读输出。
> 需要危险操作确认时，依据我对该具体操作的授权处理。

扩展不会把 gld 的 `initialize.instructions` 转给模型，所以 `workspace_context` 这一步不能省。
工具名就是 `list_workspaces` 这些，不带 `gld.` 前缀；每次项目调用都要带 `workspace`，多个 DeepSeek 标签页共用一个扩展连接也一样。
只读地址的工具比全权连接少，也没有本机 MCP 转发那几个。

### 有效期、写权限和撤销

默认只读、8 小时；`--ttl` 写正整数加 `s` / `m` / `h`，最长 24 小时。要改文件、跑命令时，另外建一把带 `--write` 的：

```bash
gld grant browser web-mcp-write gld --ttl 2h --write
```

它和普通可写 grant 一样危险：命令以你的账号执行，**项目范围不是操作系统沙箱**，见 [security.md](security.md)。
把新地址换进扩展、重连之后权限才切过去，原来的只读地址不会自己变成可写。用完就撤：

```bash
gld grant ls                       # 类型一栏写"浏览器 URL"，到期一栏是剩余时间
gld grant rm web-mcp-write         # 下一次请求起就 401，它起的命令一起停掉
```

到期后下一次请求被拒，不会自动续期。还要用就 `gld grant rm` 旧的、同名重建，再把新地址换进扩展。
到期只拦后面的请求，已经在跑的命令照常跑到它自己的 `timeout_ms`；要立刻停就 `gld grant rm`。

### Origin

默认只放行商店版 SuperAssistant 的 Origin `chrome-extension://kngiafgkdnlkgmefdafaibkibegkcaef`。
手动加载（开发者模式）的扩展 ID 不一样，用 `--origin` 列出实际的，可以写多次：

```bash
gld grant browser unpacked gld --ttl 1h --origin chrome-extension://abcdefghijklmnopabcdefghijklmnop
```

请求带 Origin 时必须在名单里；不带 Origin 的请求（非浏览器客户端）只凭地址里的密钥。
别把 `https://chat.deepseek.com` 当成要放行的 Origin：请求是扩展后台发的，带的是扩展自己的 Origin。
预检 OPTIONS 不看凭据，真正的 POST 每次都核对密钥、有效期和 Origin。

### 地址就是凭据

拿到整串地址的人就能用它，直到到期或被撤销。别贴进聊天、截图或公开 issue。gld 的请求日志不记它，
响应禁止缓存；但 SuperAssistant 会把它存在扩展的本地存储里，调试日志和侧栏也可能显示。
用公网连接地址时，地址会经过你的反向代理或隧道，让它们的访问日志别记 `/mcp/browser/` 后面那段。

### 连不上先查这些

| 现象 | 先检查 |
| --- | --- |
| 侧栏报 `SSE error: Failed to fetch`，地址是 `http://localhost:3006/sse` | 那是扩展的默认配置，指向它自家没启动的代理，和 gld 无关。按上面第 2 步改成 Streamable HTTP 和 gld 给的地址 |
| 没有 `grant browser` 这条命令，或报版本不一致 | 命令行或守护进程还是旧版，见[安装 · 升级](install.md#升级) |
| 地址确认无误，`gld logs` 里也看不到这次请求，扩展仍报 `Failed to fetch` | Chrome 142 起扩展访问本机、局域网地址可能要"本地网络访问"权限：`chrome://extensions` → MCP SuperAssistant → 详情 → 网站设置，把 Local network access 设成允许（Chrome 145 起拆成 Apps on device 和 Local network 两项，本机地址对应前者）。也可以改用公网连接地址 |
| 401，`browser_grant_invalid` | 地址没贴全、已到期或已撤销，或者扩展的 Origin 不在名单里（手动加载的扩展要 `--origin`） |
| 404 | 连到了旧版服务，或者反向代理没把整个 `/mcp/browser/…` 路径转过来 |
| 显示已连接，但工具列表是空的 | 重连一次再看 `gld logs`；扩展会把 `tools/list` 失败显示成空列表 |
| 能读文件，不能改、不能跑命令 | 只读地址就是这样；确实要写，另建 `--write` 的 |
| 工具有结果，DeepSeek 不往下说 | 检查扩展是否注入了说明、结果是否填回并提交；HTTP 调用成功不等于网页这一圈走通了 |

### 协议细节

gld 用 MCP `2025-06-18` 回初始化，通知回 `202` 空响应，工具结果用 JSON 返回；独立的 GET 推送流和 DELETE 会话都回 `405`，
这是无状态 Streamable HTTP 的正常做法，扩展用的 MCP SDK 认这个。没有传统 `/sse` 端点，也不推动态工具变更通知。

扩展在页面里等一次工具调用最多 30 秒。长命令用 `exec_command` 的 `yield_time_ms=1000`（最多 10000）先拿回
`session_id`，再用 `read_output` 读；别把命令总时长 `timeout_ms` 和单次等待 `yield_time_ms` 搞混。页面上超时不代表
gld 那边命令停了，先看结果再决定，别直接重发写操作。

依据：[SuperAssistant 的 Streamable HTTP 插件源码](https://github.com/srbhptl39/MCP-SuperAssistant/blob/c26168ee2c5708a3a65ef5afd88cda1a97c81734/chrome-extension/src/mcpclient/plugins/streamable-http/StreamableHttpPlugin.ts)、
[MCP 2025-06-18 HTTP 传输](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports)、
[Chrome 本地网络访问](https://developer.chrome.com/blog/local-network-access)。
真实二进制的 HTTP 往返由 `crates/cli/tests/browser_grants.rs` 覆盖；Chrome 扩展加 DeepSeek 页面的整圈实测记在[审查](reviews/2026-09-23-lifecycle-and-docs-audit.md)里。

## ChatGPT（MCP 连接器）

ChatGPT 在 OpenAI 的服务器上，必须通过**公网 HTTPS** 地址访问，`127.0.0.1` 填进去会连不上；
唯一的例外是办法五的 OpenAI 隧道，它不要公网地址。

`gld share` 负责整件事：给服务配好公网入口、把服务拉起来、起隧道、打印地址和凭据。
公网入口用一个 `--tunnel` 参数表达，底下五种走法选一种，同时只能有一种：换成别的会停掉原来那条隧道、
清掉原来登记的地址，ChatGPT 里按旧地址建的连接器就连不上了。

（这五种写法在 `gld start` 和 `gld upgrade` 上完全一样：
启动时就想连公网用 `gld start --tunnel …`，之后要换地址用 `gld upgrade --tunnel …`。）

**`gld share` 不带 `--tunnel` 时沿用已经配好的入口**；一个都没配过才用 Cloudflare 临时地址。

### 办法一：Cloudflare 临时地址（零配置，先试试用）

```bash
brew install cloudflared   # Windows: winget install Cloudflare.cloudflared
gld share                  # 公网地址形如 https://xxx.trycloudflare.com/mcp（等价 --tunnel cf）
```

**每次服务重启地址都会变**，ChatGPT 里的连接器改不了地址，只能删了重建。适合试用，不适合长期。重复敲
`gld share` / `gld start` 不会重启服务，地址不会因此变；会重启服务、换地址的有：`gld restart`、`gld upgrade`
改任何一项、`gld secret set` / `regen`（改凭据要重启服务才生效）、`gld daemon restart`、重启电脑。

### 办法二：Cloudflare 固定域名

需要一个 Cloudflare 账号和一条已创建的 Named Tunnel（固定隧道）。还没有的话：在 Cloudflare Zero Trust 控制台的
Networks → Tunnels 新建一条 Cloudflared 隧道，页面给的安装命令里 `--token` 后面那一长串就是 Tunnel Token
（不用照页面把 cloudflared 装成服务，gld 会自己拿 token 起它）；再在这条隧道的 Public Hostname 里把
`mcp.example.com` 指到 `http://127.0.0.1:<端口>`，端口填 `gld ls` 本地地址里那个，下文的回源端口就天然对齐。
控制台改版后菜单名可能不同。

有了隧道，把域名一起给：

```bash
gld share --tunnel cf:mcp.example.com
```

token 没配过的话，这条命令会当场问你要（输入不显示）。不想交互——比如写在
脚本里——放环境变量 `TUNNEL_TOKEN`（和 cloudflared 自己认的同名），或者用 `--token` 带上：

```bash
TUNNEL_TOKEN="$(cat ~/.config/cf-tunnel-token)" gld share --tunnel cf:mcp.example.com
gld share --tunnel cf:mcp.example.com --token <隧道 token>
```

两个都给时用 `--token` 的。给过一次 gld 就存下来，以后不用再给；环境变量里的会覆盖已经存着的那份，
所以 gld 用到它时会在终端提示一句。只有 `--tunnel cf:<域名>` 才读这个变量，临时地址、FRP、自建地址不受影响。

`--token` 写在命令行里，命令跑着的时候本机别的账号用 `ps` 看得到，也会进 shell 历史；
上面那样从文件或密码管理器读进环境变量就没有这两处。gld 起 cloudflared 时也是经环境变量把 token 交给它，
不放在它的命令行里。

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

`start`、`share`、`upgrade` 三个命令给 token 都用 `--token` 或环境变量 `TUNNEL_TOKEN`；
旧写法 `--tunnel-token` 仍然认。

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

gld 生成的是 HTTP 类型的代理加子域名，所以 `frps.toml` 至少要有 `subDomainHost = "frp.example.com"` 和
`vhostHTTPPort = 8080`（少了后者代理注册不上），再把 `*.frp.example.com` 解析到 frps，由前面的反向代理
（Caddy / Nginx）终结 HTTPS、转到 8080。其他配置见 frp 官方文档。
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

### 办法五：OpenAI 隧道（不要域名、不开端口，只给 ChatGPT）

OpenAI 官方的 [Secure MCP Tunnel](https://developers.openai.com/api/docs/guides/secure-mcp-tunnels)：
本机跑一个 `tunnel-client`，它只往外连 OpenAI，把 ChatGPT 发来的请求取回来交给 gld。
没有公网地址，互联网上找不到这台机器。

先在 OpenAI Platform 做两件事，gld 替不了：

1. 组织设置的 Tunnels 里建一条隧道，**关联上你要用的 ChatGPT 工作区**（只属于 Platform 组织不够，
   ChatGPT 里会选不到），记下 `tunnel_` 开头的隧道 ID。建隧道要 Tunnels Read + Manage 权限。
2. 准备一把 Runtime Key（`sk-…`），要有 Tunnels Read + Use 权限。新分配的权限最长 30 分钟才生效。

```bash
brew install openai/tools/tunnel-client     # Windows / Linux：发布页下 zip，把 tunnel-client-runtime 放进 PATH
gld share --tunnel openai:tunnel_0123…       # Runtime Key 没配过会当场问（输入不显示）
```

不想交互就放环境变量 `CONTROL_PLANE_API_KEY`（和 tunnel-client 自己认的同名），或者 `--token <key>`；
给过一次就存成服务凭据 `openai_runtime_key`。gld **不认** `OPENAI_API_KEY`：那多半是你调 API 的 key，不是隧道的。
发布页是 <https://github.com/openai/tunnel-client/releases>；macOS 上直接下的 zip 没做公证，会被系统拦，所以用 brew。

ChatGPT 里：Plugins → `+` → Add custom MCP server，**Connection 选 Tunnel**，选这条隧道（或贴隧道 ID），
认证选不需要认证的那项。`gld ls` 最后一行也写着这句。

**怎么认证的。**gld 自己生成一把服务凭据 `openai_tunnel_key`，起 tunnel-client 时让它给每个转发进来的请求加上
请求头 `X-Gld-Tunnel-Key`，服务认出它就放行，所以 ChatGPT 这边不走 OAuth；服务本身的认证方式（oauth / bearer）
对本机和别的客户端照旧。Runtime Key 和这把 key 都经环境变量交给 tunnel-client，不在它的命令行里。
换这把 key：`gld secret regen openai_tunnel_key`（服务和隧道一起重启，ChatGPT 那边不用动）。

**谁能用它。**关联了这条隧道的 ChatGPT 工作区里，能选到这条隧道建连接器的人，都等于拿到了服务凭据
（全部项目），见 [security.md](security.md#openai-隧道能选到隧道的人就有服务凭据)。

**官方限制：**单个请求从排队到回包最长 120 秒，请求或回包最大 10 MiB，每条隧道每分钟 600 次。
gld 单次调用最多等 30 秒（经目标机的调用最多 60 秒）、回包最大 1 MiB，都在范围内。

**起不来时**，`gld share` 的报错里附着 tunnel-client 日志的最后几行，完整的在
`~/.config/gld/logs/hub/tunnel-client.log`（`gld logs` 也有）。gld 只等它在本机就绪：它的 `/readyz` 回 200，
说明它带着请求头连得上服务。连 OpenAI 那一段（隧道 ID 不对、Runtime Key 不对或权限不够）不在这个检查里，
`gld share` 成功了 ChatGPT 还是选不到、连不上，就看这份日志。

这一节是对着 tunnel-client v0.0.18 的源码写的，还没在真的 OpenAI 隧道上跑过（审查 §70）。

### 关掉公网入口

```bash
gld share --off            # 停隧道、清掉公网地址，本地地址照常可用
```

### 在 ChatGPT 里配置

1. 设置 → 账户安全与登录（Security and login）→ 打开“开发人员模式”（Developer mode，允许添加未验证的 MCP 连接器）。
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
- `gld restart`、`gld stop` + `gld start`、守护进程重启——固定公网地址下没影响（服务断开零点几秒，在跑的命令会被停掉，
  见[安装 · 升级](install.md#升级)）
- 重启电脑：配了[开机自启](daemon.md#开机自启)就什么都不用管；没配的话守护进程不会自己回来，期间
  ChatGPT 只报连不上，**连接器不用删**，`gld daemon start` 一次（上次 `gld start` 过的服务会跟着回来）
  就好了
- `gld add` / `gld rm` / `gld set`（改项目，包括项目自己的 `tool-profile`）——服务每次调用都重新读项目表；
  项目的工具集只在调用时生效，服务发给客户端的工具表不变
- `gld grant add` / `gld grant browser`——已经连好的连接器不受影响
- `gld secret regen oauth_password` / `gld secret set oauth_password`——口令只管"下次授权填什么"，
  已经授权过的照常能用。前提是固定公网地址：改凭据会重启服务，临时地址下地址跟着变，连接器要重建
- 长时间不用：访问令牌 30 天有效，刷新令牌 90 天，期间用过一次就自动续上

**要点 Refresh**——服务发给客户端的工具表变了。ChatGPT 只在建连接器和点 Refresh 时拉工具表
（`tools/list`），开新对话、gld 重启都不会让它重拉：

| 操作 | 工具表怎么变 |
| --- | --- |
| 升级 gld，升级前后指纹不一样 | 新版加了、删了工具或参数，或者改了说明 |
| `gld mcp on` 开第一个 / `gld mcp off` 关掉最后一个 | 多出 / 少了 `list_mcp_tools` 这三个 |
| `gld remote add` 加第一个远端项目 / 删掉最后一个；有了第一个 / 没了最后一个能写的远端项目（默认就能写，`--mode read` 的不算） | 多出 / 少了 `remote_*` 那几个 |
| `gld upgrade --tool-profile …` | 整张表换了 |

不刷新的后果：新工具、新参数 AI 看不见；已经没有的工具 AI 还会去调，拿到 `Unknown tool`。怎么刷：

1. 打开 <https://chatgpt.com/plugins>（要先开开发人员模式：设置 → 账户安全与登录（Security and login）→ 开发人员模式（Developer mode）），
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

口令想自己定一个记得住的，别每次去查：`gld secret set oauth_password 你自己的口令`。用临时地址的，在建连接器
之前定好：之后再改会重启服务、换地址。

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
gld remote add prod          # prod 是 ccnm 里的项目名；默认能改能跑，只读加 --mode read
```

远端项目的工具是另一套（`remote_*`），怎么用、多台机器时怎么点名、有什么限制见
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

上表说的是标准 `/mcp` 入口。`gld grant browser` 发的浏览器地址和 oauth、bearer 并存，不改上表；noauth 下不能建 grant。

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

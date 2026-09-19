# 排障对照表

> 报错看不懂、不确定某个配置项是干什么的，先翻
> [concepts.md](concepts.md)——那里逐个解释了共享密钥池、工具集、
> Planning 模式这些名词，以及它们的默认值。

**先跑 `gld doctor`。** 它会检查配置是否自洽（端口冲突、隧道缺配置、认证缺密钥、
外部二进制没装……），每个问题下面直接写着该执行的命令。有 ✗ 时退出码为 1。

```bash
gld doctor
```

体检查不出来的问题，再看这三条：

```bash
gld daemon status        # 守护进程在不在（退出码 3 = 不在）
gld status               # 每个服务的状态、端口、公网地址
gld logs -n 50           # 本地 MCP 请求日志 + stderr
```

怀疑是工具行为问题（AI 说读不到文件、命令被拒）时，在命令行跑同一次调用：

```bash
gld tool call read_file path=src/main.rs
gld tool call exec_command cmd='cargo test'
```

命令行和 AI 用的是同一个工具上下文和同一个 `call_tool`，看到的错误结构一模一样。
工具返回 `ok: false` 时退出码是 1，但结构化结果照常打印，方便 `| jq` 分析。

## 启动 / 守护进程

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| `守护进程在 15 秒内没有就绪` | 守护进程启动失败，多半是数据目录不可写 | `gld daemon logs -n 30` 看错误 |
| `数据文件解析失败：…/profiles.json` | `profiles.json` 坏了（断电写了一半、手工编辑手滑、从坏备份还原） | **别删。** 报错里给了三条出路：还原备份 / 把 JSON 补合法（多半是末尾缺 `}`）/ 确认不要了就 `mv` 走再重新 `add`。gld 不会覆盖这个文件——它是所有密钥的唯一副本 |
| `数据目录 … 不是目录` | `GLD_HOME` 指到了一个文件上（典型：`GLD_HOME=$(mktemp)` 忘了 `-d`） | 指向一个目录，没有会自动建 |
| `建不了数据目录 …：Permission denied` | `GLD_HOME` 指的位置或它的上级目录不可写 | 换个位置，或修上级目录权限 |
| `MCP 认证方式是 bearer，但没有 bearer_token` | 认证配了 bearer 但密钥不在（手工编辑 / 旧版导入缺字段） | 照报错执行 `gld secret regen bearer_token`；服务拒绝启动是有意的——起来了也是所有请求 401 |
| `已有另一个守护进程持有 …/daemon.lock` | 同一数据目录已有实例（可能是别的用户 / 别的 shell 起的） | `gld daemon status` 看 pid；确认它已死才可删 lock 文件 |
| `守护进程（pid N）存在但不响应` | 进程活着但 socket 没在听：正在启动、或卡死 | 等几秒重试；仍不行 `gld daemon stop --force` |
| `守护进程版本 x 与命令行版本 y 不一致`（退出码 4） | 升级了 `gld`，旧进程还在跑 | `gld daemon restart` |
| `gld start` 后 `gld status` 显示 error | 端口被占、或监听器起来后立刻退出 | 错误信息里有占用者的路径与 pid；`gld upgrade --port <其他端口>`（会自动重启） |
| `本地 MCP 端口 28766 已被占用：/path/to/other` | 别的程序占了这个端口。新登记的工作区会自动避开机器上已被监听的端口，所以这多半是**之前**登记的工作区，或那个程序是后来才起来的 | `gld upgrade --port <其他端口>`；第一次启动就撞上可以直接 `gld start <目录> --port <端口>` |
| `…仍被本进程上一次的服务占用` | 上一次的监听器还没退干净。注意这**不是**"服务已经在跑"——服务在跑时再敲 `gld start` 会直接告诉你 running，不会报错 | 等几秒重试；反复出现 `gld daemon restart` |

## 客户端连不上

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| ChatGPT 提示无法连接 | 填了 `127.0.0.1` 地址，或隧道没通 | 必须是公网 HTTPS；`gld health` 看“公网 /mcp”那一行 |
| `gld health` 本地 /mcp 返回 502 | 环境里有 `HTTP_PROXY`，本地探测被代理吃了（0.3.0 起本地探测已绕过代理；旧版会有此问题） | 升级；或临时 `NO_PROXY=127.0.0.1` |
| 公网 /mcp 显示 `FRP 未挂载代理（返回 frp 404 页）` | frps 收到请求但没有对应子域名的代理 | `gld tunnel status` 看隧道是否 running；`gld tunnel restart` |
| OAuth 授权失败 | Client ID / 口令来自不同工作区，或客户端里存的是旧值 | `gld list --reveal` 重新核对（改密钥会自动重启服务，服务端一定是新值） |
| 401 Unauthorized / 客户端只说连不上 | Bearer Token 不对、改了 token 客户端没更新；或者请求压根没到 gld | 先 `gld logs -n 20` 分清是哪种：有 `[auth] rejected credential=missing` / `credential=rejected` 说明请求到了、是凭据问题，`gld secret show bearer_token --reveal` 核对（OAuth 就重新授权）；客户端一连日志里却什么都没有，说明请求没到，查公网地址和隧道（`gld health`）。日志只记带没带凭据，不记凭据本身 |
| `gld health` 显示 `HTTP 404（这个端口上应答的不是 gld 的服务）` | 这个端口上跑着别的程序（Actions 默认端口 8787 很容易被撞） | `gld ws set actions.port=<其他端口>`（会自动重启）；`gld doctor` 会告诉你占用者是谁 |
| 工具列表是旧的 | 客户端缓存 | 断开重连插件 / 新开对话；服务端 `/mcp` 已带 `Cache-Control: no-store` |
| ChatGPT 连接器突然要重新连接，配置看着没动过 | 多半是公网地址变了（临时 `cf` 隧道一重启就换地址） | 换固定地址，见 [connect-clients.md 什么时候要重新授权](connect-clients.md#什么时候要重新授权什么时候要删了重建)。重启服务本身不会掉授权 |
| 局域网另一台机器连不上 | 默认只监听 127.0.0.1 | `gld settings runtime --lan-access true` 后 `gld restart`，并确认认证不是 noauth |

## 隧道

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| `FRP 模式需要选择全局配置或填写服务器域名` | 隧道类型是 frp 但没配服务器 | `gld frp add --name <名称> …` 然后 `gld share --tunnel frp:<名称>`；不需要公网就 `gld share --off` |
| `隧道状态是 …，但没拿到公网地址` | 隧道进程起来了却没报出地址（网络被挡、frps 拒绝、token 不对） | `gld logs -n 30` 看隧道那几行输出 |
| 固定隧道 `running`，但公网 502 / `公网检查未通过` | 隧道进程状态不等于回源可用，常见原因是云端回源端口与本地监听端口不一致 | 对照错误中的预期回源地址检查云端配置；启动端口和 Token 用法见 [connect-clients.md](connect-clients.md#云端的回源端口要和本地端口一致) |
| `未找到 frpc` / `未找到 cloudflared` | 没装，或装的位置不在 PATH 里 | `brew install frpc` / `brew install cloudflared`（Windows: `winget install Cloudflare.cloudflared`）。装在别处就用 `gld settings runtime --executable-paths <目录>` 补上 |
| `… 是 frp 0.44，太老了` | gld 生成的是 TOML 配置，frp 0.52 以前用 INI 格式 | `brew upgrade frpc`，或从 releases 换 ≥0.52 的版本 |
| `同一工作区的 MCP 与 Actions 必须使用同一 FRP 服务器` | 一个工作区只跑一个 frpc，两条线路得连同一台 frps | 让两者用同一个 `frp-profile` |
| 子域名冲突 | 两个工作区配了相同子域名 | 改其中一个的 `frp-subdomain` |
| Cloudflare quick 地址每次都变 | quick 模式设计如此 | 改用 named 模式：`gld share --tunnel cf:<你的域名>`（会问你要 Tunnel Token） |
| 工作区删了、frpc 还在 | 上次是 `kill -9` 退出的 | `gld tunnel stop`；仍在的话按 `~/.config/gld/frpc/<id>/frpc.pid` 里的 pid 手动 kill |

## 工作区与配置

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| `未指定工作区` / `当前目录不属于任何工作区` | 有多个工作区，当前目录又不在任何一个里面。注意 `start` / `share` 不会报这个——它们会把当前目录登记成新工作区 | 加 `-w <名称或id前缀>`，或 `cd` 进项目目录 |
| 多出来一个没印象的工作区 | 在某个目录里敲过 `gld start`，它自动登记了。输出第一行有"已登记工作区「x」" | `gld list --all` 看都有谁；不要的 `gld destroy <名称>`（只删 gld 这边的配置） |
| `「api」匹配到多个工作区` | 名称重复 | 用 id 前缀（≥4 位） |
| `该目录已经是工作区「x」` | 重复 `gld ws add`，或 `gld upgrade --path` 指到了别的工作区的目录 | 一个目录只能属于一个工作区。想用它直接 `gld start <目录>`（会复用那个工作区，不会重复登记）；确实要腾出来先 `gld destroy` 掉占着的那个 |
| `未知字段「…」` | `ws set` 的 key 写错 | `gld ws fields` 列出全部。不写前缀就是改 MCP（`port` = `mcp.port`），改 Actions 要写全 `actions.port` |
| `新配置已经保存，但服务没能用它起来` | 自动重启用新配置起服务时失败了，最常见是新端口被别的程序占着 | 上一行错误里写着具体原因；修好后 `gld restart` |
| 改了配置没反应 | 只有受影响的那一侧会自动重启：改 `actions.*` 不会动 MCP；值没变（`auth=oauth` 设成本来就是 oauth）则不重启 | `gld ws show` 确认值真的变了；仍不对就 `gld restart` |
| `没有名为「…」的 FRP 配置` | `ws set frp-profile=` 或 `gateway set --frp-profile` 填了不存在的名称 / id | 报错里列出了已有的配置，照抄名称即可；一个都没有就先 `gld frp add`。两处都认名称、id 和 ≥4 位的 id 前缀 |
| `FRP 配置「…」还在被这些地方用着` | 想删的配置还有工作区或全局入口指着它 | 报错里列出了是谁在用；改到别的配置或 `gld share --off`，确定要留悬空引用就 `gld frp remove <id> --force` |
| `引用的 FRP 配置 … 不存在` | 之前用 `--force` 删过，或手工改过 `profiles.json` | `gld frp list` 看现有的，再 `gld ws set frp-profile=<名称>`；悬空的是全局入口就 `gld gateway set --frp-profile <名称>` |
| `mcp.frp-subdomain 无效` | 子域名要拼进 `https://<子域名>.<frps 域名>`，只能用小写字母、数字和中间的连字符 | 去掉点、空格、大写 |
| `mcp.public-url 无效：…（要带协议头）` | 手动公网地址写成了 `example.com` | 写成 `https://example.com` |
| `检测到更新的隧道配置，已拒绝用旧请求覆盖` | 隧道重启失败要回滚时，发现配置已被别处改过 | 属于保护机制；重新 `gld tunnel restart` 即可 |

## 工具调用层面

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| Agent 报 `DANGEROUS_OPERATION_REQUIRES_CONFIRMATION` | 删除 / 覆盖等危险操作要求 `confirm=true` | 让 Agent 带 `confirm=true` 重试同一工具；命令行复现加 `confirm=true` |
| Agent 说某个工具不存在 | 当前 `mcp.tool-profile` 没暴露它 | `gld tool list` 看实际暴露了什么；`gld ws set tool-profile=advanced` 换更全的工具集 |
| 写在 `.cursorrules` / `CLAUDE.md` 里的规则 AI 不理 | 默认工具集 compact 只注入工作区里的 `AGENTS.md` 一份 | `gld context` 看谁打 `✓`（真注入）谁打 `·`（只是扫到）；要全部生效 `gld ws set tool-profile=advanced` |
| AI 说没有 Skill 可用 | 0.4.0 起 compact 下 Skill 目录有字符上限（约 1200 字符），排在后面的没进说明 | `gld context` 看谁打 `✓`；让 AI 调一次 `list_skills` 就能拿到全部；要全部进说明用 `gld ws set tool-profile=advanced` |
| Plan 模式下写文件被拒 | 设计如此：Plan 模式只读 | `gld planning mode direct` 或 `goal` |
| Goal 模式下写操作被拒 | 没有聚焦的 Goal | `gld planning goal create …` 或 `goal update <id> --focus true` |
| 命令被拒 `Command is not allowlisted: <名字>` | 不在白名单 | `gld ws set allowed-commands=<名字>` 追加（默认那批仍在）；想反过来**只**允许某几个要写 `only:cargo,git`，光写 `cargo,git` 减不掉任何东西 |
| 分不清一条命令是「没装」还是「不许跑」 | 拒绝信息只说了不许跑 | 让 AI 先调 `check_command cmd='<命令>'`：它不跑命令，只回答能不能跑（`decision`）、是哪条规则拒的（`rule`）、程序在不在机器上（`program.found`）和有什么已获准的替代工具（`alternatives`） |
| 改完白名单不确定生效没有 | 配置改了，跑着的服务不一定重载了 | 改前改后各调一次 `check_command`，比对 `policy.runtime_fingerprint`：数变了才是真生效 |
| 收窄了白名单但 `python` 还能跑 | 不带 `only:` 的写法是追加，不是替换 | 改成 `gld ws set allowed-commands=only:…`；细节见 [security.md](security.md) |
| `Program not found on PATH: node`，终端里明明能跑 | 守护进程是 launchd / systemd 起的，PATH 里没有 Homebrew、`~/.cargo/bin` 这些目录 | 把目录配成全局可执行路径再 `gld restart`，写法见 [daemon.md](daemon.md#开机自启) |
| 工作区外文件写入被拒 | Workspace-first：写入永远只在工作区内 | 把目标目录登记成工作区，或把文件放进工作区 |
| `READS_CONFINED_TO_WORKSPACE`（升级到 0.3.0 后 Agent 突然读不了外部文件） | 0.3.0 起读也默认限制在工作区内，老配置升级上来一样收紧 | 确实要读外面：`gld ws set confine-reads=false`（Actions 侧 `actions.confine-reads`）。先读一下 [security.md](security.md) 再决定 |
| `GLD_DATA_HOME_DENIED` | 想用文件工具读 gld 自己的数据目录 | 有意挡的，**关掉 confine-reads 也不给读**：那里明文存着所有工作区的密钥。要看密钥用 `gld secret show <key> --reveal` |
| AI 报 `FILE_VERSION_CONFLICT` | 它读这个文件之后，文件被写过（你在编辑器里改的、另一个会话、`git checkout`），补丁没有落盘 | 这是它该做的事——让 AI 重新 `read_file` 再改。反复出现的话，看看是不是有别的程序在自动改这个文件（格式化工具、watch 任务） |
| AI 报 `WORKSPACE_BUSY`，说等了 30 秒 | 同一个工作区上另一个写操作占着写权：多半是别的会话（或另一个 gld 进程）正用 `exec_command` 同步等一条命令跑完 | 等那条命令结束再让 AI 重试，这个错误是 `retryable` 的。**连着几次都这样**说明有人在反复跑长命令：让那一侧把 `yield_time_ms` 调小让命令转后台，或者把两个会话错开用 |
| 一个会话在跑命令，另一个会话改文件却没被拦住 | 命令已经转后台了（`yield_time_ms` 到了还没跑完就会转），写权在那时就放开了——只有同步等的那一段占锁 | 想让整段都互斥就把 `yield_time_ms` 调大同步等（上限 30 秒）。这是刻意的取舍，不然 `npm run dev` 起来之后谁也改不了代码，原委见 [security.md](security.md) |
| 换了个客户端连上来，`read_output` / `kill_session` 报 `SESSION_NOT_FOUND`，`session_id` 是刚抄过来的 | 命令会话按"项目 + 谁在调"分表，不是同一个主体就看不见。换的是另一个 OAuth 客户端、或者从 hub 换到了工作区自己的地址（两套凭据两个主体） | 有意如此：别人的命令输出不该摊开。用起这条命令的那个客户端去读。真要几个客户端共用一批会话，让它们用同一份凭据连同一个入口 |
| 同一个客户端重连之后 `session_id` 就失效了 | 不是分表的事：会话本身有寿命，命令结束或超时 30 秒后会被回收；`gld hub stop`、成员被移出 hub 也会停掉经 hub 起的命令 | 结束的命令在那 30 秒里还读得到输出，过了就只能重跑。长命令别靠重连接着读，让它把结果写文件 |
| `FILE_CHANGED_EXTERNALLY`（开了 Durable Task 之后） | 有活动任务时，写工具执行前会比对工作区指纹，发现任务开始后有它没记账的文件变化 | 确实是你在编辑器里改了文件的话，这是它该做的事——让 AI 重新读一遍再动手。要是你什么都没改却一直报，看下一行 |
| 一开任务就报 `FILE_CHANGED_EXTERNALLY`，而且找不到谁改了文件 | 0.3.0 之前的 bug：gld 自己在项目里的状态目录（`.gld/`）和 history 档案被算进了指纹，而工具自己每次调用都会写它们——等于自己把自己锁死 | 升级。`.gld/` 现在不计入指纹，history 写完会自动记账 |
| 升级后，升级前就开着的任务第一次写操作就报 `FILE_CHANGED_EXTERNALLY` | 跳过名单多了 `.venv/`、`coverage/`、`Library/` 等目录（[concepts.md](concepts.md#durable-task-的工作区基线)），工作区里有这些目录就跟升级前记下的指纹对不上 | 结束旧任务再开一个：`task_manage action=finish task_id=<id> allow_unverified=true`，然后 `action=start`。`<id>` 在 `action=status` 的 `task_id` 里 |

## 聚合入口（hub）

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| 连 hub 一直 401，同一个 token 连工作区地址却能进 | 填的是工作区的凭据。hub 有自己独立的一套，两边互相打不开是有意的 | `gld hub show --reveal` 取 hub 的 |
| AI 报 `WORKSPACE_REQUIRED` | 调用没带 `workspace` 参数。hub 不记"当前工作区"，不替它猜 | 报错里列了能填什么，模型一般重试一次就对；反复出现就在对话里说一句"每次调用都带 workspace" |
| AI 报 `WORKSPACE_NOT_IN_HUB`，工作区明明登记了 | 登记了不等于加进了 hub，或者名字写错 | `gld hub show` 看成员，`gld hub add <工作区>`，立即生效 |
| AI 报 `WORKSPACE_AMBIGUOUS` | 两个成员同名 | 让 AI 填 id；或给其中一个改名：`gld ws set -w <id> name=<新名字>` |
| AI 报 `TOOL_NOT_ALLOWED_IN_WORKSPACE` | 这个成员自己的工具集里没有这个工具，hub 不会替它放宽 | 确实要给：`gld ws set -w <成员> tool-profile=compact`（或更全的） |
| `端口 … 已经分给了工作区「…」` / `已经是全局入口的本地端口` | hub 端口和别的 gld 服务撞了 | `gld hub set --port <端口>` |
| `hub 挂了公网入口…不能用 noauth` | 有意拦的：无认证的公网 hub 等于把全部成员开放给整个互联网 | `gld hub set --auth oauth` |
| `hub 设了经全局入口暴露，但全局入口没启用` | `--global-gateway true` 依赖全局入口 | 先配好全局入口（[connect-clients.md](connect-clients.md#多个项目共用一个域名全局入口)）；或 `gld hub set --global-gateway false` |
| 全局入口上访问 `/hub/mcp` 返回 404 | hub 没声明走入口，入口不替它转 | `gld hub set --global-gateway true` |
| 移出 hub 的工作区，经 hub 起的命令还在跑 | 这些命令在下一次有请求进 hub 时才被结束 | 随便再调一次 hub；或 `gld hub stop`。注意只停经 hub 起的那些——这个项目自己的服务和命令行起的命令不在范围内 |
| 改了成员配置，以为正在跑的命令会被停掉，结果还在跑 | 0.4.0 改了：重建成员上下文不再顺手杀命令（以前改一行 AI 说明就把跑着的 `npm run dev` 杀了） | 要停就明确地停：让 AI 调 `kill_session`，或 `gld hub stop` |
| `gld hub show` 状态是 `error` | 监听器跑着跑着退了 | 状态后面写着日志位置（数据目录下 `logs/hub/stderr.log`）；修好后 `gld hub start` |

## 数据目录与环境

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| 两套配置互相干扰 | 用了同一个 `~/.config/gld` | 用 `GLD_HOME=/path/a gld …` 隔离，守护进程也按 `GLD_HOME` 各自一套 |
| socket 出现在 `/tmp` 而不是数据目录 | 数据目录路径太长（>100 字节），Unix socket 放不下 | 正常；`gld daemon status` 里能看到实际路径 |
| 想看守护进程收到了什么 | — | `gld daemon logs -f`，每个请求一行含耗时；参数不记录（里面可能有密钥） |

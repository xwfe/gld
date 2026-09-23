# 排障对照表

> 报错看不懂、不确定某个配置项是干什么的，先翻
> [concepts.md](concepts.md)——那里逐个解释了服务和项目、工具集、
> Planning 模式这些名词，以及它们的默认值。

**先跑 `gld doctor`。** 它会检查配置是否自洽（端口冲突、隧道缺配置、认证缺密钥、
外部二进制没装……），加上服务此刻的端口和**隧道**状态：隧道进程还在不在、这次用的
是哪个公网地址、起隧道时报了什么错。每个问题下面直接写着该执行的命令。有 ✗ 时退出码为 1。

```bash
gld doctor
gld doctor --probe   # 再实地探一次本地 / 公网端点和 OAuth 元数据（同 gld health）
```

不加 `--probe` 时它一个网络请求都不发，所以答不了"公网地址此刻通不通"——自建反代那种
不是 gld 起的链路尤其如此，gld 只知道地址被登记过。

体检查不出来的问题，再看这三条：

```bash
gld daemon status        # 守护进程在不在（退出码 3 = 不在）
gld status               # 服务的状态、端口、公网入口（隧道没起来会写原因）
gld logs -n 50           # 服务的请求日志 + stderr + 隧道输出（-w <项目> 看某个项目的）
```

怀疑是工具行为问题（AI 说读不到文件、命令被拒）时，在命令行跑同一次调用：

```bash
gld tool call read_file path=src/main.rs        # 当前目录对应的项目；别的项目加 -w <项目>
gld tool call exec_command cmd='cargo test'
```

命令行和 AI 用的是同一套工具内核，看到的错误结构一模一样。
工具返回 `ok: false` 时退出码是 1，但结构化结果照常打印，方便 `| jq` 分析。

## 启动 / 守护进程

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| `守护进程在 15 秒内没有就绪` | 守护进程启动失败，多半是数据目录不可写 | `gld daemon logs -n 30` 看错误 |
| `数据文件解析失败：…/profiles.json` | `profiles.json` 坏了（断电写了一半、手工编辑手滑、从坏备份还原） | **别删。** 报错里给了三条出路：还原备份 / 把 JSON 补合法（多半是末尾缺 `}`）/ 确认不要了就 `mv` 走再重新 `add`。gld 不会覆盖这个文件——它是所有密钥的唯一副本 |
| `数据目录 … 不是目录` | `GLD_HOME` 指到了一个文件上（典型：`GLD_HOME=$(mktemp)` 忘了 `-d`） | 指向一个目录，没有会自动建 |
| `建不了数据目录 …：Permission denied` | `GLD_HOME` 指的位置或它的上级目录不可写 | 换个位置，或修上级目录权限 |
| `已有另一个守护进程持有 …/daemon.lock` | 同一数据目录已有实例（可能是别的用户 / 别的 shell 起的） | `gld daemon status` 看 pid；确认它已死才可删 lock 文件 |
| `守护进程（pid N）存在但不响应` | 那个 pid 上确实跑着 gld，只是 socket 没在听：正在启动、或卡死 | 等几秒重试；仍不行 `gld daemon stop --force`。这句话只在 pid 对应的进程真是 gld 时才出现，所以 `--force` 不会误伤别的程序 |
| `守护进程版本 x 与命令行版本 y 不一致`（退出码 4） | 升级了 `gld`，旧进程还在跑 | `gld daemon restart` |
| `gld start` 后 `gld status` 显示 error | 端口被占、或监听器起来后立刻退出 | 错误信息里有占用者的路径与 pid；`gld upgrade --port <其他端口>`（会自动重启） |
| `MCP 服务端口 28764 已被占用：/path/to/other` | 别的程序占了服务的端口 | `gld upgrade --port <其他端口>`；第一次启动就撞上可以直接 `gld start --port <端口>` |
| `…仍被本进程上一次的服务占用` | 上一次的监听器还没退干净。注意这**不是**"服务已经在跑"——服务在跑时再敲 `gld start` 什么都不做，不会报错 | 等几秒重试；反复出现 `gld daemon restart` |
| `新配置已经保存，但服务重启失败、现在是停的` | 改端口 / 认证 / 公网入口 / 凭据之后按新配置重启时失败了，最常见是新端口被别的程序占着 | 冒号后面写着具体原因；修好后 `gld start` |

## 客户端连不上

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| ChatGPT 提示无法连接 | 填了 `127.0.0.1` 地址，或隧道没通 | 必须是公网 HTTPS；`gld health` 看“公网 /mcp”那一行 |
| `gld health` 本地 /mcp 返回 502 | 环境里有 `HTTP_PROXY`，本地探测被代理吃了（0.3.0 起本地探测已绕过代理；旧版会有此问题） | 升级；或临时 `NO_PROXY=127.0.0.1` |
| 公网 /mcp 显示 `FRP 未挂载代理（返回 frp 404 页）` | frps 收到请求但没有对应子域名的代理 | `gld status` 看公网入口那行有没有写隧道没起来的原因；`gld restart` |
| OAuth 授权失败 | 客户端里存的是旧口令 / 旧 Client ID | `gld ls --reveal` 重新核对（改凭据会自动重启服务，服务端一定是新值） |
| 401 Unauthorized / 客户端只说连不上 | Bearer Token 不对、改了 token 客户端没更新、填的是某个项目自己的凭据；或者请求压根没到 gld | 先 `gld logs -n 20` 分清是哪种：有 `[auth] rejected credential=missing` / `credential=rejected` 说明请求到了、是凭据问题，`gld secret ls bearer_token --reveal` 核对（OAuth 就重新授权）；客户端一连日志里却什么都没有，说明请求没到，查公网地址和隧道（`gld health`）。日志只记带没带凭据，不记凭据本身 |
| `gld health -s actions` 显示 `HTTP 404（这个端口上应答的不是 gld 的服务）` | 这个端口上跑着别的程序（Actions 默认端口 8787 很容易被撞） | `gld set <项目> actions.port=<其他端口>`（会自动重启）；`gld doctor` 会告诉你占用者是谁 |
| 工具列表是旧的 | 客户端缓存 | 断开重连插件 / 新开对话；服务端 `/mcp` 已带 `Cache-Control: no-store` |
| ChatGPT 连接器突然要重新连接，配置看着没动过 | 多半是公网地址变了（临时 `cf` 隧道一重启就换地址） | 换固定地址，见 [connect-clients.md 什么时候要重新授权](connect-clients.md#什么时候要重新授权什么时候要删了重建)。重启服务本身不会掉授权 |
| 局域网另一台机器连不上 | 默认只监听 127.0.0.1 | `gld cfg runtime --lan-access true` 后 `gld restart`，并确认认证不是 noauth |

## 隧道

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| `FRP 模式需要选择全局配置或填写服务器域名` | 隧道类型是 frp 但没配服务器 | `gld frp add --name <名称> …` 然后 `gld share --tunnel frp:<名称>`；不需要公网就 `gld share --off` |
| `服务起来了，但公网入口没起来：…` | 隧道没起来（没装 cloudflared / frpc、网络被挡、frps 拒绝、token 不对）。服务照样在本地跑，本机客户端不受影响 | 冒号后面写着原因；`gld logs -n 30` 看隧道那几行输出；修好后 `gld share` 重试 |
| `隧道起来了，但没拿到公网地址` | 隧道进程起来了却没报出地址 | `gld logs -n 30` 看隧道输出 |
| 固定隧道起来了，但公网 502 / `公网检查未通过` | 隧道进程在跑不等于回源可用，常见原因是云端回源端口与服务端口不一致 | 对照错误中的预期回源地址检查云端配置；端口和 Token 用法见 [connect-clients.md](connect-clients.md#云端的回源端口要和服务端口一致) |
| `未找到 frpc` / `未找到 cloudflared` | 没装，或装的位置不在 PATH 里 | `brew install frpc` / `brew install cloudflared`（Windows: `winget install Cloudflare.cloudflared`）。装在别处就用 `gld cfg runtime --executable-paths <目录>` 补上 |
| `… 是 frp 0.44，太老了` | gld 生成的是 TOML 配置，frp 0.52 以前用 INI 格式 | `brew upgrade frpc`，或从 releases 换 ≥0.52 的版本 |
| FRP 子域名冲突 | 服务和某个项目的 Actions 用了同一个子域名，或者两台机器上的 gld 连同一台 frps、都用了默认的 gld | 换一个：`gld share --tunnel frp:<配置名> --subdomain <别的名字>` |
| Cloudflare 临时地址每次都变 | quick 模式设计如此：服务每重启一次换一个 | 改用固定域名：`gld share --tunnel cf:<你的域名>`（会问你要 Tunnel Token）。重复敲 `gld start` / `gld share` 不会重启服务 |
| 服务停了、frpc / cloudflared 还在 | 上次是 `kill -9` 退出的 | 按数据目录 `frpc/` 下对应的 `frpc.pid` 里的 pid 手动 kill；cloudflared 用 `pgrep -fl cloudflared` 找 |

## 项目与配置

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| `当前目录 … 不属于任何项目` | 有多个项目，当前目录又不在任何一个里面 | 写出项目名（`gld set api …`、`gld ls api`）或加 `-w <名称或id前缀>`，或 `cd` 进项目目录 |
| `当前目录 … 不是项目，没加进来` | 在不是项目的目录里敲了 `gld start` / `gld share`，而你已经有别的项目了：只起服务，不登记这个目录（免得把主目录这种地方交给 AI） | 真想加它：`gld add .` |
| 多出来一个没印象的项目 | 在某个目录里敲过 `gld start <目录>` 或 `gld add`，它登记了。输出第一行有"已加入项目「x」" | `gld ls` 看都有谁；不要的 `gld rm <名称>`（只删 gld 这边的配置） |
| `gld ls` 里有项目标着"不在服务里" | 旧版本登记的：那时可以"登记了但不在 hub 里"。AI 看不见它 | `gld start`（会把它们加进来并逐个说出名字），或 `gld add <目录>` |
| `「api」匹配到多个项目` | 名称重复 | 用 id 前缀（≥4 位） |
| `该目录已经是项目「x」` | `gld upgrade --path` 指到了别的项目的目录 | 一个目录只能属于一个项目。确实要腾出来先 `gld rm` 掉占着的那个 |
| `port 是项目自己那个 MCP 服务的字段…` | 用 `gld set` 改了以前单项目服务的字段（`port`、`auth`、`tunnel`、`public-url`、`frp-*`…）。现在只有一个服务，项目上没有这些 | 报错下一行就是该用的命令：端口 / 认证用 `gld upgrade`，公网入口用 `gld share --tunnel`，凭据用 `gld secret` |
| `未知字段「…」` | `gld set` 的 key 写错 | `gld fields` 列出全部。不写前缀就是改 MCP 那一半（`tool-profile` = `mcp.tool-profile`），改 Actions 要写全 `actions.port` |
| 改了配置没反应 | 项目的字段下一次调用就生效，不用重启；服务的配置（`gld upgrade`）会自动重启服务 | `gld ls <项目>` 确认值真的变了；客户端缓存了工具列表就断开重连 |
| `没有名为「…」的 FRP 配置` | `--tunnel frp:` 或 `actions.frp-profile=` 填了不存在的名称 / id | 报错里列出了已有的配置，照抄名称即可；一个都没有就先 `gld frp add`。各处都认名称、id 和 ≥4 位的 id 前缀 |
| `FRP 配置「…」还在被这些地方用着` | 想删的配置还有服务的公网入口、项目的 Actions 或全局入口指着它 | 报错里列出了是谁在用；改到别的配置或 `gld share --off`，确定要留悬空引用就 `gld frp remove <id> --force` |
| `引用的 FRP 配置 … 不存在` | 之前用 `--force` 删过，或手工改过 `profiles.json` | `gld frp list` 看现有的，再 `gld share --tunnel frp:<名称>`；悬空的是项目的 Actions 就 `gld set <项目> actions.frp-profile=<名称>` |
| `actions.frp-subdomain 无效` / `--subdomain` 被拒 | 子域名要拼进 `https://<子域名>.<frps 域名>`，只能用小写字母、数字和中间的连字符 | 去掉点、空格、大写 |
| `actions.public-url 无效：…（要带协议头）` | 手动公网地址写成了 `example.com` | 写成 `https://example.com` |

## 工具调用层面

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| Agent 报 `DANGEROUS_OPERATION_REQUIRES_CONFIRMATION` | 删除 / 覆盖等危险操作要求 `confirm=true` | 让 Agent 带 `confirm=true` 重试同一工具；命令行复现加 `confirm=true` |
| Agent 说某个工具不存在 | 这个项目的工具集没暴露它；或者服务的工具集更窄（两边取交集） | `gld tool list -w <项目>` 看实际暴露了什么；`gld set <项目> tool-profile=advanced` 换更全的，服务那边 `gld upgrade --tool-profile advanced` |
| 写在 `.cursorrules` / `CLAUDE.md` 里的规则 AI 不理 | 默认工具集 compact 只注入项目里的 `AGENTS.md` 一份 | `gld context` 看谁打 `✓`（真注入）谁打 `·`（只是扫到）；要全部生效 `gld set <项目> tool-profile=advanced` |
| AI 说没有 Skill 可用 | 0.4.0 起 compact 下 Skill 目录有字符上限（约 1200 字符），排在后面的没进说明；写了 `disable-model-invocation` 的本来就不进目录（打 `◦`） | `gld context` 看谁打 `✓`；让 AI 调一次 `list_skills` 就能拿到全部；要全部进说明用 `gld set <项目> tool-profile=advanced` |
| 自己写的 Skill 在 `gld context` 里打 `✗` | SKILL.md 没收进来：frontmatter 写坏了（报第几行）、没写 `description`、描述超过 1024 字符，或者和另一份内容一模一样 | 照 `✗` 后面的原因改文件；`list_skills` 马上看得到，进说明里的目录要等 AI 下次连上 |
| 装了的 Skill 在 `gld context` 里根本没有 | 它是主目录里链到别处的链接（`~/.claude/skills/x -> ~/code/...`），2026-09-22 之前的 gld 扫描不跟链接；或者被 `--hidden-skills` 藏了 | 升级 gld；`gld cfg runtime` 看"隐藏的 Skill"那一行 |
| AI 说读不到 Skill 里的脚本（`filesUnavailable`） | 这个 skill 在主目录里、是默认的 auto 扫描扫到的：默认只给正文不给文件 | `gld cfg runtime --skill-sources claude`（换成它实际的来源）明确启用；或者 `gld set <项目> confine-reads=false` 放开读取范围 |
| Plan 模式下写文件被拒 | 设计如此：Plan 模式只读 | `gld planning mode direct` 或 `goal` |
| Goal 模式下写操作被拒 | 没有聚焦的 Goal | `gld planning goal create …` 或 `goal update <id> --focus true` |
| 命令被拒 `Command is not allowlisted: <名字>` | 不在白名单 | `gld set <项目> allowed-commands=<名字>` 追加（默认那批仍在）；想反过来**只**允许某几个要写 `only:cargo,git`，光写 `cargo,git` 减不掉任何东西 |
| 分不清一条命令是「没装」还是「不许跑」 | 拒绝信息只说了不许跑 | 让 AI 先调 `check_command cmd='<命令>'`：它不跑命令，只回答能不能跑（`decision`）、是哪条规则拒的（`rule`）、程序在不在机器上（`program.found`）和有什么已获准的替代工具（`alternatives`） |
| 改完白名单不确定生效没有 | 配置改了，跑着的服务不一定重载了 | 改前改后各调一次 `check_command`，比对 `policy.runtime_fingerprint`：数变了才是真生效 |
| 收窄了白名单但 `python` 还能跑 | 不带 `only:` 的写法是追加，不是替换 | 改成 `gld set <项目> allowed-commands=only:…`；细节见 [security.md](security.md) |
| `Program not found on PATH: node`，终端里明明能跑 | 守护进程是 launchd / systemd 起的，PATH 里没有 Homebrew、`~/.cargo/bin` 这些目录 | 把目录配成全局可执行路径再 `gld restart`，写法见 [daemon.md](daemon.md#开机自启) |
| 项目目录外文件写入被拒 | 写入永远只在项目目录内 | 把目标目录也 `gld add` 成一个项目，或把文件放进项目 |
| `READS_CONFINED_TO_WORKSPACE`（升级到 0.3.0 后 Agent 突然读不了外部文件） | 0.3.0 起读也默认限制在项目目录内，老配置升级上来一样收紧 | 确实要读外面：`gld set <项目> confine-reads=false`（Actions 那条线路是 `actions.confine-reads`）。先读一下 [security.md](security.md) 再决定 |
| `GLD_DATA_HOME_DENIED` | 想用文件工具读 gld 自己的数据目录 | 有意挡的，**关掉 confine-reads 也不给读**：那里明文存着所有凭据。要看凭据用 `gld secret ls <key> --reveal` |
| AI 报 `FILE_VERSION_CONFLICT` | 它读这个文件之后，文件被写过（你在编辑器里改的、另一个会话、`git checkout`），补丁没有落盘 | 这是它该做的事——让 AI 重新 `read_file` 再改。反复出现的话，看看是不是有别的程序在自动改这个文件（格式化工具、watch 任务） |
| AI 报 `WORKSPACE_BUSY`，说等了 30 秒 | 同一个工作区上另一个写操作占着写权：多半是别的会话（或另一个 gld 进程）正用 `exec_command` 同步等一条命令跑完 | 等那条命令结束再让 AI 重试，这个错误是 `retryable` 的。**连着几次都这样**说明有人在反复跑长命令：让那一侧把 `yield_time_ms` 调小让命令转后台，或者把两个会话错开用 |
| 一个会话在跑命令，另一个会话改文件却没被拦住 | 命令已经转后台了（`yield_time_ms` 到了还没跑完就会转），写权在那时就放开了——只有同步等的那一段占锁 | 想让整段都互斥就把 `yield_time_ms` 调大同步等（上限 30 秒）。这是刻意的取舍，不然 `npm run dev` 起来之后谁也改不了代码，原委见 [security.md](security.md)。拦不住但看得见：补丁结果的 `warnings` 会列出还在跑的命令 |
| 后台命令跑出来的结果对不上代码（测试挂在一个你已经改掉的地方） | 后台那段没有写互斥，命令测的可能是改之前的代码 | 看那条会话结果里的 `workspace_writes_since_start`：不是 0 就说明它跑的这段时间工作区被改过，那条结果不作数，重跑一次 |
| 起后台命令（`yield_time_ms: 0`）报 `WORKSPACE_BUSY` | 0.4.0 改了：命令起来之前也要拿到写权，免得它看到别人写了一半的文件树 | 是 `retryable` 的，重试即可。占着的多半是另一条同步等结果的命令（最多 30 秒），或者一次正在落盘的补丁（毫秒级） |
| 换了个客户端连上来，`read_output` / `kill_session` 报 `SESSION_NOT_FOUND`，`session_id` 是刚抄过来的 | 命令会话按"项目 + 谁在调"分表，不是同一个主体就看不见。换的是另一个 OAuth 客户端就是另一个主体 | 有意如此：别人的命令输出不该摊开。用起这条命令的那个客户端去读。真要几个客户端共用一批会话，让它们用同一份凭据 |
| 同一个客户端重连之后 `session_id` 就失效了 | 不是分表的事：会话本身有寿命，命令结束或超时 30 秒后会被回收；`gld stop`、项目被删掉也会停掉经服务起的命令 | 结束的命令在那 30 秒里还读得到输出，过了就只能重跑。长命令别靠重连接着读，让它把结果写文件 |
| 远端项目（`remote_*` 那组工具）的后台命令忽然没了，`remote_read_output` 说不认识那个 `output_ref` | 后台命令活不过它那条 coding 会话。会话可能是被这几样结束的：一条跑过头的**前台**命令（服务对一次远端调用最多等 60 秒，超了就丢连接）、`remote_coding_end`、或者没人调用被回收——挂着后台命令时是十分钟，没挂着是两分钟 | 长命令一律 `run_in_background`；前台命令的期限服务会替你压到 50 秒以内，要更久它会拒，照着它说的改。起了后台任务就隔一会儿 `remote_read_output` 看一眼，既拿到进度也把空闲计时清零。真要长活的服务（dev server 之类）交给那台机器上的 systemd / launchd，别让它挂在一条 MCP 连接上 |
| `FILE_CHANGED_EXTERNALLY`（开了 Durable Task 之后） | 有活动任务时，写工具执行前会比对工作区指纹，发现任务开始后有它没记账的文件变化 | 确实是你在编辑器里改了文件的话，这是它该做的事——让 AI 重新读一遍再动手。要是你什么都没改却一直报，看下一行 |
| 一开任务就报 `FILE_CHANGED_EXTERNALLY`，而且找不到谁改了文件 | 0.3.0 之前的 bug：gld 自己在项目里的状态目录（`.gld/`）和 history 档案被算进了指纹，而工具自己每次调用都会写它们——等于自己把自己锁死 | 升级。`.gld/` 现在不计入指纹，history 写完会自动记账 |
| 升级后，升级前就开着的任务第一次写操作就报 `FILE_CHANGED_EXTERNALLY` | 跳过名单多了 `.venv/`、`coverage/`、`Library/` 等目录（[concepts.md](concepts.md#durable-task-的工作区基线)），工作区里有这些目录就跟升级前记下的指纹对不上 | 结束旧任务再开一个：`task_manage action=finish task_id=<id> allow_unverified=true`，然后 `action=start`。`<id>` 在 `action=status` 的 `task_id` 里 |

## 服务和项目

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| 连服务一直 401，那个 token 是从某个项目那里抄来的 | 服务只有一套凭据；项目自己留着的（GPT Actions 那套、以前单项目服务那套）打不开它 | `gld ls --reveal` 取服务的 |
| AI 报 `WORKSPACE_REQUIRED` | 调用没带 `workspace` 参数。服务不记"当前项目"，不替它猜 | 报错里列了能填什么，模型一般重试一次就对；反复出现就在对话里说一句"每次调用都带 workspace" |
| AI 报 `WORKSPACE_NOT_IN_HUB`，项目明明登记了 | 名字写错；或者是旧版本登记的、不在服务里（`gld ls` 标"不在服务里"） | `gld ls` 看项目表；不在服务里的 `gld start` 加进来，立即生效 |
| AI 报 `WORKSPACE_AMBIGUOUS` | 两个项目同名 | 让 AI 填 id；或给其中一个改名：`gld set <id> name=<新名字>` |
| AI 报 `TOOL_NOT_ALLOWED_IN_WORKSPACE` | 这个项目自己的工具集里没有这个工具，服务不会替它放宽 | 确实要给：`gld set <项目> tool-profile=compact`（或更全的） |
| `端口 … 已经分给了项目「…」` / `已经是全局入口的本地端口` | 服务端口和某个项目的 GPT Actions 或全局入口撞了 | `gld upgrade --port <端口>` |
| `服务挂了公网入口，不能用 noauth` | 有意拦的：无认证的公网服务等于把全部项目开放给整个互联网 | `gld upgrade --auth oauth`；确实只在本机用就先 `gld share --off` |
| `hub 设了经全局入口暴露，但全局入口没启用` | 老配置走的是全局入口那条路 | 改用服务自己的入口：`gld share --tunnel <入口>`（会同时关掉经全局入口那条路） |
| 删掉的项目，经服务起的命令还在跑 | 这些命令在下一次有请求进服务时才被结束 | 随便再调一次服务；或 `gld stop`。注意只停经服务起的那些——命令行（`gld tool call`）起的和项目的 GPT Actions 不在范围内 |
| 改了项目配置，以为正在跑的命令会被停掉，结果还在跑 | 0.4.0 改了：重建项目上下文不再顺手杀命令（以前改一行 AI 说明就把跑着的 `npm run dev` 杀了） | 要停就明确地停：让 AI 调 `kill_session`，或 `gld stop` |
| `gld status` 里服务的状态是 `error` | 监听器跑着跑着退了 | 状态后面写着日志位置（数据目录下 `logs/hub/stderr.log`）；修好后 `gld start` |
| 旧命令（`gld ws …`、`gld hub …`、`gld destroy`）还能敲，但帮助里找不到 | 2026-09-22 起命令收成了顶层的 add / ls / set / rm（[RFC-0004](rfc/0004-one-service-many-projects.md)）；旧写法保留兼容，不进帮助 | 照 RFC-0004 第 2 节那张表换成新写法 |

## 本机 MCP server

`gld mcp on` 开的那些（见 [concepts.md](concepts.md#本机装好的-mcp-server)）。先跑一次
`gld mcp test <名字>`：它在守护进程里真起一次，用的就是服务起它时的 `PATH` 和环境变量。

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| `gld mcp test` 报 ``cannot find `npx` … not on the PATH``，终端里 `npx` 明明能跑 | 守护进程的 `PATH` 和你的终端不一样：由 launchd / systemd 拉起时常常只有 `/usr/bin:/bin`，mise / nvm 装的 `npx`、`uvx` 不在里面。报错里写了它找的是哪几个目录 | `gld cfg runtime --executable-paths ~/.local/share/mise/shims`（换成 `which npx` 的那个目录），或者在 `~/.claude.json` 里把 `command` 写成绝对路径 |
| `gld mcp ls` 说"配置里用了环境变量 X，gld 的守护进程里没有" | `~/.claude.json` 里写了 `${X}`，而守护进程起的时候那个 shell 没 export 它（`.zshrc` 里的变量，launchd 起的进程看不到） | 在一个 export 了它的终端里 `gld daemon restart`；或者把值直接写进配置的 `env` |
| AI 报 `MCP_SERVER_NEEDS_LOGIN`（HTTP 401） | 这个远端 server 要 OAuth 登录，令牌存在 Claude Code 自己那里，gld 拿不到；或者配置里的 key 错了 | 换用它给 key 的写法（请求头或 URL 参数）；要 OAuth 的 gld 用不了 |
| AI 报 `MCP_SERVER_UNUSABLE`，说是老的 HTTP+SSE 传输 | 配置里是 `"type": "sse"`，gld 只支持 streamable HTTP | 看那个 server 的文档，多半有一个以 `/mcp` 结尾的新地址，改成 `"type": "http"` |
| AI 说有个 server，调的时候报 `MCP_SERVER_UNKNOWN` | 名字要一字不差（区分大小写：本机常见 `Context7` 和 `context7` 两个都装着）；或者刚被 `gld mcp off` 关了 | `gld mcp ls` 看开着的名字；报错里也列了 |
| 开了 server，ChatGPT 里看不到 `list_mcp_tools` | 它只在连上时读一次工具表，而这三个工具开了第一个才出现 | 在 ChatGPT 的连接器设置里刷新一下 |
| 开了但 AI 看不到、`gld mcp ls` 也说开着 | 服务的工具集是 `read-only`，一个都不转 | `gld upgrade --tool-profile compact` |
| AI 报 `MCP_OUTCOME_UNKNOWN` | 调用发出去之后连接断了或超时（默认 60 秒，Codex 配置里的 `tool_timeout_sec` 会改它），server 做没做不知道 | 查一下它该做的事做没做，再决定重不重来；下一次调用会重起这个 server |
| 用完很久，`node` / `python` 进程还在 | server 闲 5 分钟才收，每分钟看一次 | 正常；`gld stop` 立刻全收 |

## 数据目录与环境

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| 两套配置互相干扰 | 用了同一个 `~/.config/gld` | 用 `GLD_HOME=/path/a gld …` 隔离，守护进程也按 `GLD_HOME` 各自一套 |
| socket 出现在 `/tmp` 而不是数据目录 | 数据目录路径太长（>100 字节），Unix socket 放不下 | 正常；`gld daemon status` 里能看到实际路径 |
| 想看守护进程收到了什么 | — | `gld daemon logs -f`，每个请求一行含耗时；参数不记录（里面可能有密钥） |

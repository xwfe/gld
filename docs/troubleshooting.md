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
| 401 Unauthorized | Bearer Token 不对，或改了 token 客户端没更新 | `gld secret show bearer_token --reveal` |
| `gld health` 显示 `HTTP 404（这个端口上应答的不是 gld 的服务）` | 这个端口上跑着别的程序（Actions 默认端口 8787 很容易被撞） | `gld ws set actions.port=<其他端口>`（会自动重启）；`gld doctor` 会告诉你占用者是谁 |
| 工具列表是旧的 | 客户端缓存 | 断开重连插件 / 新开对话；服务端 `/mcp` 已带 `Cache-Control: no-store` |
| 局域网另一台机器连不上 | 默认只监听 127.0.0.1 | `gld settings runtime --lan-access true` 后 `gld restart`，并确认认证不是 noauth |

## 隧道

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| `FRP 模式需要选择全局配置或填写服务器域名` | 隧道类型是 frp 但没配服务器 | `gld frp add --name <名称> …` 然后 `gld share --tunnel frp:<名称>`；不需要公网就 `gld share --off` |
| `隧道状态是 …，但没拿到公网地址` | 隧道进程起来了却没报出地址（网络被挡、frps 拒绝、token 不对） | `gld logs -n 30` 看隧道那几行输出 |
| `未找到 frpc` / `未找到 cloudflared` | 没装，或装的位置不在 PATH 里 | `brew install frpc` / `brew install cloudflared`（Windows: `winget install Cloudflare.cloudflared`）。装在别处就用 `gld settings runtime --executable-paths <目录>` 补上 |
| `… 是 frp 0.44，太老了` | gld 生成的是 TOML 配置，frp 0.52 以前用 INI 格式 | `brew upgrade frpc`，或从 releases 换 ≥0.52 的版本 |
| `同一工作区的 MCP 与 Actions 必须使用同一 FRP 服务器` | 一个工作区只跑一个 frpc，两条线路得连同一台 frps | 让两者用同一个 `frp-profile` |
| 子域名冲突 | 两个工作区配了相同子域名 | 改其中一个的 `frp-subdomain` |
| Cloudflare quick 地址每次都变 | quick 模式设计如此 | 用 named 模式 + `gld secret set cloudflare_token` |
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
| `没有名为「…」的 FRP 配置` | `frp-profile` 填了不存在的名称 / id | 报错里列出了已有的配置，照抄名称即可；一个都没有就先 `gld frp add` |
| `FRP 配置「…」还在被这些地方用着` | 想删的配置还有工作区指着它 | 报错里列出了是谁在用；改到别的配置或 `gld share --off`，确定要留悬空引用就 `gld frp remove <id> --force` |
| `引用的 FRP 配置 … 不存在` | 之前用 `--force` 删过，或手工改过 `profiles.json` | `gld frp list` 看现有的，再 `gld ws set frp-profile=<名称>` |
| `mcp.frp-subdomain 无效` | 子域名要拼进 `https://<子域名>.<frps 域名>`，只能用小写字母、数字和中间的连字符 | 去掉点、空格、大写 |
| `mcp.public-url 无效：…（要带协议头）` | 手动公网地址写成了 `example.com` | 写成 `https://example.com` |
| `检测到更新的隧道配置，已拒绝用旧请求覆盖` | 隧道重启失败要回滚时，发现配置已被别处改过 | 属于保护机制；重新 `gld tunnel restart` 即可 |

## 工具调用层面

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| Agent 报 `DANGEROUS_OPERATION_REQUIRES_CONFIRMATION` | 删除 / 覆盖等危险操作要求 `confirm=true` | 让 Agent 带 `confirm=true` 重试同一工具；命令行复现加 `confirm=true` |
| Agent 说某个工具不存在 | 当前 `mcp.tool-profile` 没暴露它 | `gld tool list` 看实际暴露了什么；`gld ws set tool-profile=advanced` 换更全的工具集 |
| 写在 `.cursorrules` / `CLAUDE.md` 里的规则 AI 不理 | 默认工具集 compact 只注入工作区里的 `AGENTS.md` 一份 | `gld context` 看谁打 `✓`（真注入）谁打 `·`（只是扫到）；要全部生效 `gld ws set tool-profile=advanced` |
| AI 说没有 Skill 可用 | compact 下 Skill 完全不注入，`list_skills` / `get_skill` 也不暴露 | 同上，换 `tool-profile=advanced` |
| Plan 模式下写文件被拒 | 设计如此：Plan 模式只读 | `gld planning mode direct` 或 `goal` |
| Goal 模式下写操作被拒 | 没有聚焦的 Goal | `gld planning goal create …` 或 `goal update <id> --focus true` |
| 命令被拒 `Command is not allowlisted: <名字>` | 不在白名单 | `gld ws set allowed-commands=<名字>` 追加（默认那批仍在）；想反过来**只**允许某几个要写 `only:cargo,git`，光写 `cargo,git` 减不掉任何东西 |
| 收窄了白名单但 `python` 还能跑 | 不带 `only:` 的写法是追加，不是替换 | 改成 `gld ws set allowed-commands=only:…`；细节见 [security.md](security.md) |
| 工作区外文件写入被拒 | Workspace-first：写入永远只在工作区内 | 把目标目录登记成工作区，或把文件放进工作区 |
| `READS_CONFINED_TO_WORKSPACE`（升级到 0.3.0 后 Agent 突然读不了外部文件） | 0.3.0 起读也默认限制在工作区内，老配置升级上来一样收紧 | 确实要读外面：`gld ws set confine-reads=false`（Actions 侧 `actions.confine-reads`）。先读一下 [security.md](security.md) 再决定 |
| `GLD_DATA_HOME_DENIED` | 想用文件工具读 gld 自己的数据目录 | 有意挡的，**关掉 confine-reads 也不给读**：那里明文存着所有工作区的密钥。要看密钥用 `gld secret show <key> --reveal` |
| `FILE_CHANGED_EXTERNALLY`（开了 Durable Task 之后） | 有活动任务时，写工具执行前会比对工作区指纹，发现任务开始后有它没记账的文件变化 | 确实是你在编辑器里改了文件的话，这是它该做的事——让 AI 重新读一遍再动手。要是你什么都没改却一直报，看下一行 |
| 一开任务就报 `FILE_CHANGED_EXTERNALLY`，而且找不到谁改了文件 | 0.3.0 之前的 bug：gld 自己在项目里的状态目录（`.gld/`）和 history 档案被算进了指纹，而工具自己每次调用都会写它们——等于自己把自己锁死 | 升级。`.gld/` 现在不计入指纹，history 写完会自动记账 |

## 数据目录与环境

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| 两套配置互相干扰 | 用了同一个 `~/.config/gld` | 用 `GLD_HOME=/path/a gld …` 隔离，守护进程也按 `GLD_HOME` 各自一套 |
| socket 出现在 `/tmp` 而不是数据目录 | 数据目录路径太长（>100 字节），Unix socket 放不下 | 正常；`gld daemon status` 里能看到实际路径 |
| 想看守护进程收到了什么 | — | `gld daemon logs -f`，每个请求一行含耗时；参数不记录（里面可能有密钥） |

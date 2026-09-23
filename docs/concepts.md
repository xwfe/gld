# 这些名词到底是什么意思

gld 里有十来个概念，名字看着都认识，但**默认值和边界**跟直觉常常不一样。
这篇只讲"它是什么、什么时候该用、用错了会怎样"，具体怎么配在各自的文档里。

各节都能单独看，按需跳：

- [服务和项目](#服务和项目) · [别的机器上的项目](#成员还可以在别的机器上) · [为什么不会串](#为什么不会串) · [一把钥匙开所有项目的门](#代价一把钥匙开所有项目的门)
- [父目录当一个项目](#父目录当一个项目) · [GPT Actions 是另一条线路](#gpt-actions-是另一条线路)
- [拿公网地址的方式](#拿公网地址的方式) · [工具集 tool-profile](#工具集tool-profile)
- [权限模式 permission-mode](#权限模式permission-mode) · [Planning 三种模式](#planning-三种模式)
- [历史会话档案](#历史会话档案与-history-context) · [Durable Task 的工作区基线](#durable-task-的工作区基线)

---

## 服务和项目

**gld 只有一个 MCP 服务，你的项目都挂在它下面。** 客户端里只配这一条连接；AI 每次调用
带一个 `workspace` 参数（项目名或 id）选项目，不共享可变的当前项目。
这里的分离指路由、配置与部分状态，不代表进程沙箱或按客户端授权。

```bash
gld start ~/code/api         # 起服务，并把这个目录加进来（服务起在 http://127.0.0.1:28764/mcp）
gld add ~/code/web           # 再加一个；服务在跑就立即生效，不用重启
gld ls                       # 客户端要填的地址和凭据，和项目表
gld ls api                   # 某个项目的配置
gld set web tool-profile=read-only   # 改某个项目（字段见 gld fields）
gld rm web                   # 删掉一个项目（项目文件不动）
gld stop                     # 停服务；项目、配置、凭据都留着
```

具体怎么接 Claude Code / ChatGPT 见 [connect-clients.md](connect-clients.md)。

> 2026-09-22 之前（[RFC-0004](rfc/0004-one-service-many-projects.md)）gld 有两种用法：
> 每个项目自己起一个 MCP 服务，或者用"聚合入口 hub"把几个项目聚到一条连接上。
> 现在只剩后一种，而且它就是默认——命令行里不再出现 hub 这个词，内部名字没改
> （MCP 回报的 `serverInfo.name` 仍是 `gld-hub`，日志目录是 `logs/hub/`）。

**一个项目 = 一个本地目录 + 它自己的一套规矩**：工具集、命令白名单、读限制、Planning、
历史档案。文件工具默认只读写这个目录；读限制可显式放宽。
**执行命令的工作目录在项目内，不代表子进程只能访问项目内**，详见 [security.md](security.md)。

**登记就是加入。** `gld add <目录>` 或 `gld start <目录>` 把目录登记进来，AI 立刻就能用。
没有"登记了但 AI 看不见"这种状态——老版本留下的例外会在 `gld ls` 里标成"不在服务里"，
`gld start` 会把它们加进来并逐个说出名字。

**停和删是两回事。** `gld stop` 只是让服务不再跑，`gld start` 立刻还能用；`gld rm` 把
这个项目在 gld 这边的一切删掉——配置、Planning 与历史的记账。两者都不会碰项目文件。
删之前默认要确认一次（`-y` 跳过）。

**`gld start` 不带目录时**：当前目录本来就是（或在）一个项目里，就用它；一个项目都还没有，
就把当前目录加进来。其余情况**只起服务，不登记当前目录**——只剩一个服务之后，`gld start`
也是"把服务拉起来"的那条命令，在主目录里随手敲一下，不该把整个主目录交给 AI。
真想加就 `gld add .`。

**大多数改项目的命令不用写项目名：**

1. 你写了项目名（`gld set api …`、`gld ls api`）或 `-w <id | id 前缀(≥4位) | 名称 | 路径>` —— 用它；
2. 没写，就看**当前目录**属于哪个项目（嵌套时取最深的那个）；
3. 还定不了，但你**总共只有一个**项目 —— 就用它。

> 名称重复时会报"匹配到多个项目"并列出来，改用 id 前缀即可。

**服务的端口是固定的**（默认 28764），改用 `gld upgrade --port 30000`。项目登记时也会分到
一个"MCP 端口"，那是以前单项目服务留下的字段，现在没有东西监听它，不用管。

### AI 那边看到什么

- `list_workspaces`：列出服务里的项目（名称、id、路径、工具集）。
- 工作区工具都有**必填**参数 `workspace`，填项目的名称或 id；服务级的
  `list_mcp_tools` / `call_mcp_tool` / `read_mcp_result` 不带它：

  ```text
  read_file  workspace=api  path=src/main.rs    读的是 api 项目的 src/main.rs
  read_file  workspace=web  path=src/main.rs    读的是 web 项目的 src/main.rs
  read_file  path=src/main.rs                   报 WORKSPACE_REQUIRED，报错里列出能填什么
  ```

- `workspace_context`：取某个项目自己的说明文件、Skill、历史摘要和 Planning 模式。
  服务会提示 AI 第一次进一个项目前先调它。

名称重复时填名称会报 `WORKSPACE_AMBIGUOUS`，要求改填 id——不会挑一个猜。

### 成员还可以在别的机器上

如果项目在另一台机器上、并且那台机器用 [ccnm](https://github.com/xwfe/ccnm) 管着，
可以把它也加进服务：

```bash
gld remote add prod --node work --remote-workspace server
```

`--node` 和 `--remote-workspace` 填的都是 **ccnm 配置里的名字**，不是 host 也不是
路径——在那台机器上跑 `ccnm workspace list` 能看到有哪些。（叫 `--remote-workspace`
是因为 `-w/--workspace` 是全局参数，那个说的是"本机哪个项目"，两回事。）

前提：本机装了 `ccnm`，并且它能连到那台机器。gld 起的是公开命令
`ccnm mcp bridge`，SSH 连接、凭据和对面的目录全由 ccnm 自己管，gld 不碰。

**远端项目的工具是另一套，名字带 `remote_` 前缀。**只读的七个，任何远端项目都有：

```text
remote_workspace_info  workspace=prod                     对面项目的名字、git 状态、平台
remote_read_file       workspace=prod  path=src/main.rs   读对面的文件
remote_list_files      workspace=prod  path=src           列对面的目录
remote_search_text     workspace=prod  query=TODO         在对面搜，只有命中结果过网络
remote_load_skill      workspace=prod                     对面项目自带的 skill，不带名字就是列表
remote_view_image      workspace=prod  path=shots/a.png   看对面的图（PNG/JPEG/GIF/WebP）
remote_read_notebook   workspace=prod  path=a.ipynb       按 cell 读对面的 Jupyter notebook
```

**能写的远端项目**（`--mode coding`）还多六个，都要先 `remote_coding_begin` 拿一个句柄：
`remote_apply_patch`（改文件，含整文件覆盖和改 notebook 的 cell）、`remote_exec_command`
（跑命令，`cmd` 是 argv、`shell` 是一行 bash）、`remote_read_output`（分页读输出）、
`remote_stop_command`（停掉后台命令）、`remote_call_mcp_tool`（用那台机器上的 MCP server：项目 `.mcp.json` 里声明的、执行账号装的，对面的 ccnm 有可转的才有）、`remote_coding_end`（关会话、放写锁）。

**长命令往后台放。**服务对一次远端调用最多等 60 秒，超了这条连接会被丢掉、coding 会话
跟着结束（对面还会把这个会话起的命令一起停掉）。所以前台命令在这边封顶 50 秒：不给
`timeout_ms` 就替你填上，要更久会被拒——那不是小气，是**替你躲开一次连坐**：一条跑过
头的前台命令会把同一个会话里所有后台任务一起带走。跑得久的给 `remote_exec_command` 加
`run_in_background: true`，马上拿到 `output_ref`，再用 `remote_read_output` 加
`wait_ms`（最多 50000）等它，或者 `remote_stop_command` 停掉它。

后台命令活不过这个 coding 会话：`remote_coding_end` 会停掉它们；没人调用的会话也会被
收掉，**挂着后台命令时是十分钟，没挂着是两分钟**。十分钟不是"可以一直跑"——会话占着
那台机器上这个项目的写锁。真要长活的服务（dev server 之类），交给那台机器上的
systemd / launchd，ccnm 只负责起它。

**对面的 ccnm 太老会明说。**gld 在连上时读一次对面有哪些工具，对面没有的工具、不收的
参数，在 gld 这边就报 `REMOTE_TOOL_UNSUPPORTED`，不发过去——老版本 ccnm 遇到不认识的
参数是**默默忽略**，那样 `run_in_background` 会变成前台跑，模型却以为起了后台命令。
遇到这个错就去那台机器升级 ccnm。

**这个错会告诉你对面现在是哪一版**，否则"升级 ccnm"这句话没法执行——你不知道
自己升过没有，也不知道是不是连错了机器：

```text
remote_exec_command on remote workspace prod: the ccnm on that machine does not
take run_in_background on exec_command … It reports itself as ccnm 0.7.1 with
11 tool(s). Upgrade ccnm on that machine, or call exec_command without
run_in_background

details.remote_capabilities = {
  server_name: "ccnm", server_version: "0.7.1",
  tool_count: 11, tools_digest: "3f9a2c1d8b7e4056"
}
```

`tools_digest` 是那份工具表（工具名加各自的参数名）的摘要：版本号一样而摘要不一样，
说明对面装的是同一版号的不同构建。对面没报 `serverInfo` 就写 `unknown`——**说不知道，
不编一个版本号出来**。只有这一种错带这份信息，别的错发生在核对之前，那时还没有
依据可言。

为什么不直接复用本地那几个同名工具：因为**不是同一个契约**。gld 本机也有
`search_text` 和 `list_files`，但两边的分页、参数和错误码都不一样。混用的话，
AI 以为自己在读 A，实际读的是 B。所以用错了直接报错：

```text
read_file         workspace=prod  → TOOL_IS_LOCAL_ONLY（并告诉你该用哪个 remote_*）
remote_read_file  workspace=api   → TOOL_IS_FOR_REMOTE_WORKSPACES
```

两种情况都**不会退而求其次在另一边执行**。

`list_workspaces` 里远端项目带 `kind: "remote"`，没有 `path`——那是对面机器上的
目录，gld 不知道，编一个比不报更糟。`gld ls` 里那一列显示的是 `node:workspace`。

删掉用 `gld rm prod`（`gld remote rm prod` 也行）。远端项目除了这份配置没有别的东西。

连接是**用到才建**：第一次调用才起 `ccnm mcp bridge`，之后复用；只读连接闲 5 分钟收掉，
coding 会话采用前述 2 分钟 / 挂后台命令 10 分钟的规则。
`gld stop` 时正常关闭（让对面读到 EOF 而不是直接杀，否则远端的写锁会留下
标记要人工恢复）。

### 为什么不会串

和[父目录当一个项目](#父目录当一个项目)那条路逐条对比：

| | 父目录当一个项目 | 每个项目单独加进来 |
| --- | --- | --- |
| "当前在哪个项目" | 没有这回事：AI 写的路径就是相对父目录的，`proj-a/src/x.rs` | **服务端不记**，每次调用自己带 `workspace`；两个对话同时各干各的也不会串 |
| 边界 | 父目录，兄弟项目互相读得到 | 每个项目自己的根目录，`..` 和绝对路径的规则和单独一个项目一样 |
| 命令会话 | 共用一张表 | 按"项目 + 谁在调"分：api 里起的命令，拿它的 `session_id` 去 web 读报 `SESSION_NOT_FOUND`；**另一个客户端在 api 里读也一样报它**，见下面第四条 |
| Planning / History / Durable Task | 都以父目录作为同一个项目 | Planning 在项目 `.gld/`，History 在项目 `docs/history-session/`；Durable Task 在 `GLD_HOME/harness/workspaces/<id>/`，按项目分开 |
| 说明文件、Skill | 只读父目录那份 | 按项目单独取，api 的 `AGENTS.md` 不会拿去指导 web |
| 工具集、命令白名单、读限制 | 父目录一套 | 用每个项目自己的。服务的工具集只收紧不放宽：web 是 `read-only`，经服务也写不了（报 `TOOL_NOT_ALLOWED_IN_WORKSPACE`） |

另外四条：

- **凭据只有一套，是服务的。** 项目自己留着的凭据（GPT Actions 那套、以前单项目服务那套）打到服务上是 401。
- **不在服务里的项目，AI 看不见。** 填它的名字和填一个不存在的名字，报错一模一样，
  `list_workspaces` 里也没有它。
- **日志各记各的。** 对 api 的请求记在 api 自己的请求日志里（带 `[hub]` 前缀，
  `gld logs -w api` 看得到），web 的日志里没有；服务自己的日志（`gld logs`）记全部。
- **命令会话还按"谁在调"再分一层。** 同一个 api 里，另一个 OAuth 客户端拿着你的
  `session_id` 去 `read_output` 或 `kill_session`，报的也是 `SESSION_NOT_FOUND`——
  它连"有这么一条命令在跑"都不该知道。**前提是它们身份分得开**：`noauth` 下谁都是
  同一个主体，共用一条 bearer 令牌的客户端也是。真要互相看不见，给每个客户端各注册
  一个 OAuth 客户端。

### 改了什么要不要重启

| 操作 | 服务要不要重启 |
| --- | --- |
| `gld add` / `gld rm`、`gld set`（改项目） | **不用**，下一次调用就生效。服务每次请求都重新读项目表和项目配置 |
| `gld upgrade`（端口、认证、工具集、公网入口）、`gld secret set` / `regen` | 服务在跑就自动重启 |
| 重复敲 `gld start` / `gld share` | 跑着就什么都不做——不掉连接，Cloudflare 临时地址也不换 |
| 守护进程重启 | 服务自己回来（不看 `restore-on-launch` 开关）；`gld stop` 过的不回来 |

被删掉的项目，它**经服务**起的还在跑的命令，会在下一次有请求进服务时被结束。
这个项目自己的 GPT Actions 和命令行（`gld tool call`）起的命令不受影响。

**改了配置的项目不会被停命令。** 服务会按新配置重建它的上下文，但正在跑的命令和
它的 `session_id` 都还在——改一行 AI 说明就把跑着的 `npm run dev` 杀掉，那是以前的
毛病。要停命令用 `gld stop`（停整个服务经手的），或者让 AI 调 `kill_session`。

### 代价：一把钥匙开所有项目的门

拿到服务凭据的人能进 **服务里的全部项目**，而且连上来调一次 `list_workspaces` 就知道有哪几个。

- 只把愿意放在一起的项目加进来；要单独给别人用的项目，现在没有办法单独给——别加进来。
- 服务挂了公网（任何一种公网入口）时，gld 拒绝 `noauth`。
- 按客户端分范围（给某个客户端只开某几个项目）还没做，见下面。

### 目前没有的

- **按调用者分范围。** 所有拿着服务凭据的客户端看到同一组项目。这是跨仓评审 X07 要的
  "项目级授权"，还没做（[RFC-0004](rfc/0004-one-service-many-projects.md) 第 4 节）。
- **GPT Actions 版。** 服务只有 MCP；自定义 GPT 用的 Actions 还是一个项目一个，见下面。

---

## 本机装好的 MCP server

在 Claude Code、Codex 里装好的 MCP server（context7、deepwiki、exa……），gld 可以经服务
转给连上来的 AI——ChatGPT 这类只能连一个公网地址的客户端，这是它用上它们的唯一办法。
本机的 Claude Code、Codex 自己就连得上，用不着这个。

```bash
gld mcp ls                        # 装了哪些、开了哪些
gld mcp test context7             # 在守护进程里起一次：起不起得来、有哪些工具
gld mcp on context7 deepwiki      # 开（名字区分大小写，照 ls 里的抄）
gld mcp off context7              # 关；--all 全关
```

**装了哪些**直接读 `~/.claude.json` 的 `mcpServers` 和 `~/.codex/config.toml` 的
`[mcp_servers.*]`，不用在 gld 里再配一遍。两边同名时用 `~/.claude.json` 那份。
项目级的（项目里的 `.mcp.json`）不算。

**默认一个都不开**，要你点名。原因是服务可能挂在公网上，而 Filesystem、
desktop-commander 这类能读写整个主目录；gld 也分不出谁"只走网络"——context7 在很多
机器上就是 `npx` 起的本机进程，配置里和 Filesystem 长得一样。

**AI 那边看到三个工具**，开了至少一个才出现，都不带 `workspace`：

```text
list_mcp_tools                              开着的 server 和状态
list_mcp_tools  server=context7             它的工具、参数表、它自己给 AI 的说明
call_mcp_tool   server=context7  tool=query-docs  arguments={...}
read_mcp_result ref=r1a2…  offset=65520     读一个大结果的后面部分
```

不把每个 server 的工具直接列进工具表，是因为那样每次列工具都得把它们全起起来，工具表
也会胀（playwright 一家就 25 个工具、21 KB），ChatGPT 每开一个新的还得重建连接。

**大段文字可续读，但转发不等于结果无损。** 一次最多交 64 KiB 文字，多的有界保留
10 分钟，单条最多 16 MiB、总计 64 MiB；超限和续读位置会说明。历史实测 deepwiki 的
407 KB 文字分三次读全。当前结果整理在有文字时省略 `structuredContent`，不能据此假定
二者总是重复；上游把独立字段仅放在结构化结果里时存在信息丢失风险，见
[本次审查](reviews/2026-09-23-lifecycle-and-docs-audit.md)。单张图片超过 5 MiB 换成说明。
结果过期不能证明上游操作没执行；有副作用的调用应先核对实际状态，不能自动重发。

**什么时候生效。** 开关不用重启服务，下一次调用就按新名单来；但 ChatGPT 只在连上时读
一次工具表，**开第一个的时候要在它的连接器设置里刷新一下**，才看得见这三个工具。

**server 怎么起、活多久。** 用到才起，按"server + 哪个客户端"各起一份（有状态的
playwright 不会两个客户端共用一个浏览器），闲 5 分钟收掉，`gld stop` 全收，连它下面起的
进程一起。起的时候 `PATH` 是 `gld cfg runtime --executable-paths` 加上守护进程自己的，
工作目录是主目录。

**哪些用不了**：要 OAuth 登录的远端 server（令牌在 Claude Code 自己那里，gld 拿不到）、
老的 HTTP+SSE 传输（`type: "sse"`）、配置里用了守护进程环境里没有的变量的——`gld mcp ls`
会逐个写出原因。起不来怎么查见 [troubleshooting.md](troubleshooting.md#本机-mcp-server)。

服务的工具集是 `read-only` 时一个都不转：转过去的工具能做什么由 server 决定，只读管不住。
设计取舍和实测数字见 [RFC-0006](rfc/0006-machine-mcp.md)。

---

## 父目录当一个项目

能，但它就只是**一个项目**：把 `~/code` 加进来，AI 在里面看到的是整棵树，写
`proj-a/src/main.rs` 这样的路径去读写子项目。好处是一条 `gld add` 就够。代价：

**1. 没有边界。** 子项目之间互相读得到、写得到，真正的边界只有父目录。

**2. 说明文件只读父目录那一份。** `AGENTS.md` / `CLAUDE.md` 只在父目录里找，
不往子目录递归，proj-a 自己的 `CLAUDE.md` 不会注入。

**3. Planning 和 History 全混在一起。** `.gld/planning/state.json` 和
`docs/history-session/` 都落在父目录根上，十个项目的 Goal 和会话档案堆成一堆。

**4. [Durable Task](#durable-task-的工作区基线) 的指纹是整棵树扫。**
子项目一多，每次写操作前都要重扫一遍父目录，明显变慢。

以前单项目服务的年代还有一个 `set_default_cwd` 工具可以"切到子项目"，服务里不暴露它：
它改的是所有对话共享的状态，两个对话同时开着就会互相把对方切走。

**建议：每个项目单独 `gld add`。** 客户端里仍然只有一条连接（服务只有一个），
上面四条一条都不沾。父目录那条路只留给"一堆自己的小脚本，不在乎上面四条"。

---

## GPT Actions 是另一条线路

MCP 服务之外，gld 还能给**某个项目**起一个 OpenAPI 网关，给只能导入 OpenAPI 文档的
自定义 GPT（GPT Actions）用。它和 MCP 服务是两回事：

| | MCP 服务 | GPT Actions |
| --- | --- | --- |
| 协议 | MCP Streamable HTTP（JSON-RPC） | OpenAPI 3.1 + REST |
| 给谁用 | ChatGPT 连接器、Claude Code、Cursor、Codex | 自定义 GPT |
| 几个 | **一个**，项目都挂在它下面 | **一个项目一个**（GPT 导入的是一个项目的文档） |
| 默认端口 | 28764 | 8787 起，每个项目一个 |
| 默认认证 | oauth | api_key |
| 怎么起 | `gld start` | 在项目目录里 `gld start -s actions` |

**绝大多数人只用 MCP。** Actions 是给"只能导入 OpenAPI 文档"的自定义 GPT 准备的
退路，不需要就一直让它停着——它不启动不占任何资源，`gld ls <项目>` 里也不显示它。

Actions 的字段都带 `actions.` 前缀：

```bash
gld set api actions.port=9000      # 改这个项目的 Actions 端口
gld fields --all                   # 看全部字段（默认只列项目通用的那些）
```

Actions 的凭据属于项目：`gld secret ls actions_api_key --reveal -w api`。多个项目的
Actions 想共用一把 Key，可以勾 `actions.shared-secrets=true`，它们读的是同一个
"共享密钥池"（`gld secret shared …`，不进帮助）——代价是一把 Key 泄露，勾了的项目一起沦陷。

---

## 拿公网地址的方式

ChatGPT 跑在 OpenAI 的服务器上，只能连公网 HTTPS，`127.0.0.1` 填进去连不上。
公网入口属于**服务**，一条命令配好：

| `--tunnel` | 地址长什么样 | 谁在转发 | 什么时候选 |
| --- | --- | --- | --- |
| `cf` | `https://xxx.trycloudflare.com/mcp`，**服务每次重启都变** | 本机跑的 cloudflared 子进程 | 零配置，先试试 |
| `cf:<域名>` | `https://<域名>/mcp`，固定 | 本机跑的 cloudflared（Named Tunnel，要 token） | 有 Cloudflare 账号和域名 |
| `frp:<配置名>` | `https://<子域名>.<frps 域名>/mcp`，固定（子域名默认 gld） | 本机跑的 frpc | 自己有一台跑 frps 的公网机器 |
| `https://…` | 你自己定 | 你自己的 Caddy / Nginx | 已经有公网机器和反代，不需要 gld 打洞 |

要装 `cloudflared` 或 `frpc` 的那几种，gld 不代管，PATH 里有就自动认。

```bash
gld share                              # 沿用已配好的入口；一个都没配过就是 cf
gld share --tunnel cf:mcp.example.com  # Cloudflare 固定域名
gld share --tunnel frp:公司            # FRP 固定域名
gld share --tunnel https://x.com/mcp   # 已有的公网地址
gld share --off                        # 关掉
```

同样的 `--tunnel` 写法在 `gld start`（启动时一起配）和 `gld upgrade`（事后换）上通用。
隧道起不来时服务照样在本地跑（本机客户端不受影响），`gld ls` 会写出原因，
`gld share` 会以非零退出码报错。

**cloudflare quick 和 named 的区别：** quick 零配置但**服务每次重启地址都会变**，
ChatGPT 里得跟着改，适合试用；named 要你在 Cloudflare 建一条隧道拿到 token，
地址固定，用 `gld share --tunnel cf:<你的域名>` 一次把域名定下来——token
没配过会当场问，脚本里用 `--token <token>` 直接给。

以前还有一个"全局入口"（多个单项目服务共用一个域名，服务经它挂在 `/hub/mcp`），
老配置照旧能用，不再出现在帮助里，见
[connect-clients.md](connect-clients.md#老配置经全局入口挂公网)。

**开公网入口前请读 [security.md](security.md)。** 那不是客套话——它等于把
"以你的身份在你电脑上跑命令"这件事对外开放了，而且是**全部项目**。

---

## 工具集（tool-profile）

决定 **AI 能看到哪些工具**。这是服务端强制的：不在集合里的工具，客户端硬发
`tools/call` 也只会得到 `Unknown tool`，不靠客户端自觉。

```bash
gld set api tool-profile=read-only
gld tool list -w api                # 看这个项目实际暴露了什么
```

服务自己也有一个工具集（`gld upgrade --tool-profile …`，默认 compact），和项目的取**交集**：
服务写 advanced 也放不开一个 read-only 的项目。

| 取值 | 工具数 | 说明 |
| --- | --- | --- |
| `compact` | 28 | **默认值**。把同类操作聚合成一个带 `action` 参数的稳定 API（`history_manage` / `planning_manage` / `task_manage`），描述也更短——工具列表本身要占 token，条目少意味着每次对话省一截 |
| `core` | 40 | compact 的聚合工具 + 拆开的旧工具名并存。客户端认旧工具名时用它 |
| `advanced` | 54 | 全部工具都暴露 |
| `read-only` | 21 | 去掉 `exec_command` / `apply_patch` / `write_stdin` / `kill_session`，只剩读和 Git 查询 |
| `compat-readonly-all` | 54 | 见下面的警告 |

上面的数字是本地工具内核的 profile 口径，会随版本变；以命令输出为准。
它不是客户端实际拿到的表：服务还会加上 `list_workspaces` 等服务级工具、远端和中继工具，
去掉 `get/set_default_cwd`，每个工具多一个 `workspace` 参数；客户端还可能缓存旧表。
客户端实际拿到的那张用 `gld tool list --served` 看，核对办法见
[troubleshooting.md 工具列表是旧的](troubleshooting.md#客户端连不上)。

### compact 还会砍掉注入给 AI 的说明，Skill 目录只给一段

省 token 不只体现在工具条数上。`compact` 下：

- **说明文件只注入项目里的 `AGENTS.md`（或 `AGENTS.override.md`）一份**（经 `workspace_context` 给 AI），
  `.cursorrules`、`CLAUDE.md`、全局说明都不进去；
- **Skill 目录有字符上限**（约 1200 字符，大概 8–12 条，每条描述截到 120 字符），
  放不下的在末尾写明"还有几个"。`list_skills` / `get_skill` 两个工具照常暴露，
  AI 调一次就拿得到全部。

> Skill 这一条 2026-09-19 改过。以前 compact 下 Skill **整个不可用**：目录一条
> 不给，两个工具也不暴露。结果是项目把用法写进了 `.claude/skills/`，默认档下的
> AI 却完全不知道有这回事。现在的折中是"给一段带上限的目录"——模型至少知道
> 这里有东西，剩下的自己去问。
>
> 目录里**项目自己的 skill 排在前面**，主目录里那批装给所有项目用的排后面：
> 上限挤掉谁，得是通用的那些。

`gld context` 会把这件事标出来——打 `✓` 的才真的进去，打 `·` 的只是扫到了：

```text
说明文件（扫到 3，实际注入 1）
  ✓ [codex/workspace] AGENTS.md  11 字
  · [claude/global] ~/.claude/CLAUDE.md  888 字
  · [cursor/workspace] .cursorrules  11 字
Skill（扫到 14，目录里列了 9）
  ✓ [claude/workspace] release  .claude/skills/release/SKILL.md
  · [claude/global] brainstorm  ~/.claude/skills/brainstorm/SKILL.md
```

写坏了的 SKILL.md（引号没闭合、没写描述、描述超过 1024 字符）不进目录，但也不会悄悄消失：`gld context` 打 `✗` 并写出原因，AI 调 `list_skills` 也在 `skipped` 里看得到。

frontmatter 写了 `disable-model-invocation: true`（`yes`、`on`、`1` 也算）的 skill，**不进目录，AI 不会自己用它**；你在对话里点它的名字（"用 deploy 那个 skill"），AI 才去 `get_skill` 加载，正文前面还带一句"用户点名才照做"。`gld context` 里打 `◦`。

为什么不像原生 Claude Code 那样整个藏起来：原生是靠你敲 `/deploy` 启动的，gld 没有斜杠命令（ChatGPT、Codex 也没有），藏起来就谁都用不了了。`user-invocable` 管的正是那个斜杠菜单，gld 里没有这个菜单，所以它不起作用。

skill 目录里的脚本、参考文件，AI 用 `get_skill` 加 `file` 读——只限这个 skill 自己的目录。主目录里的用户级 skill 默认只给正文不给文件，来源明确配置之后才给，边界见[安全](security.md)那一节。

主目录里**链到别处的 skill 也算**（`~/.claude/skills/x -> ~/code/skills/x`，自己写的 skill 常这么放，skills CLI 也把每个都从 `~/.agents/skills` 链进来）：2026-09-22 之前 gld 扫描不跟链接，这种 skill 整个看不见，原生客户端却看得见。项目里的链接照旧不跟。

**不想让某个装好的 skill 出现**：`gld cfg runtime --hidden-skills pdf,pptx`（按名字，不分大小写；传 `""` 清空）。藏掉的和没装一样：目录里没有、`list_skills` 不列、`get_skill` 拿不到，`skipped` 里也不提。只管主目录和自定义路径里的，项目自己的 skill 由项目决定。

要让它们全部进去：`gld set <项目> tool-profile=advanced`。
代价是工具从 28 个涨到 53 个，加上多出来的说明和完整 Skill 目录，
每次对话的固定开销明显变大。

### `compat-readonly-all` 不是只读

名字里有 `readonly`，但它**暴露的工具和 `advanced` 完全一样（53 个，能写能执行）**。
它唯一改的是给客户端看的**标注**：把每个工具都标成 `readOnlyHint: true`、
`destructiveHint: false`。

MCP 客户端可以拿这两个标注决定要不要弹确认框、要不要限制并发。所以这个档位的
作用是"让客户端别把这些工具当危险操作对待"——**服务端一侧一个能力都没减**。
想真正只读请用 `read-only`。

---

## 权限模式（permission-mode）

`trusted`（默认）和 `dangerous` 两个值。

**先说结论：这两个值现在的差别小到可以忽略，你不需要动它。**

具体来说，两者**完全一样**的部分：

- 写入边界一样——都只能写工作区内，绝对路径和 `..` 一律拒（`ABSOLUTE_PATH_DENIED`）；
- 危险命令（`rm -rf` 那类）一样要 `confirm=true`，`dangerous` 不会替你跳过；
- 命令白名单一样生效；
- 读取限制（`confine-reads`）一样生效。

`dangerous` 唯一实际做的事，是让一个**已经不对 MCP 客户端暴露**的遗留工具
（`request_permissions`）自动返回"已授权"。也就是说对正常使用没有任何影响。

> 曾经有个说法是 `dangerous` 会"额外放开全局临时目录写入"。那是错的：
> 写入路径解析压根拿不到 permission-mode，从来就没放开过。
> `check_exec_environment` 以前会把这个不存在的能力报给模型，模型照着去写
> `/tmp` 只会撞 `ABSOLUTE_PATH_DENIED` 然后反复重试。现在报的是实话，
> 并且有测试把"报告的能力"和"实际行为"绑在一起，两边同时改才可能过。

真要收紧能力，用 [security.md](security.md) 里那四条（关隧道 / 换只读工具集 /
`only:` 收窄白名单 / 别用 noauth），不是靠这个字段。

---

## Planning 三种模式

给"让 AI 改代码之前先说清楚要干什么"用的闸门。状态存在项目里的
`.gld/planning/state.json`。

**这个目录默认不进版本库**：gld 第一次建 `.gld/` 时会在里面放一个只忽略它自己的
`.gitignore`（内容就一行 `*`），免得 AI 每动一次 Planning，你的 `git status`
里就多一条改动。它不碰项目根的 `.gitignore`。

想让团队共享 Goal / Plan，把 `.gld/.gitignore` 删掉即可——它只在目录被创建的
那一刻写一次，之后不会再补回来。

```bash
gld planning mode plan      # 切换
gld planning ls             # 看当前模式、Goal、Plan、执行台账
```

| 模式 | AI 能做什么 | 什么时候用 |
| --- | --- | --- |
| `direct` | 什么都能做（默认） | 日常。你自己盯着 |
| `plan` | **只读**。写文件、跑命令一律被拒（`PLAN_MODE_READ_ONLY`），但能读代码、能创建 Goal / Plan | 让 AI 先调研 + 出方案，你看过再放行 |
| `goal` | 必须有一个**你已聚焦且状态为 active 的 Goal** 才能写，否则拒（`GOAL_CONTEXT_REQUIRED`） | 多轮长任务，防止 AI 跑偏去改无关的东西 |

`goal` 模式的典型流程：

```bash
gld planning goal create --title "重构鉴权" --objective "把 session 换成 JWT" \
  --criterion "旧接口保持兼容" --criterion "测试全绿"
gld planning goal update <id> --focus true    # 聚焦它，AI 才能开始写
gld planning mode goal
# …AI 干活…
gld planning goal accept <id>                 # 人工验收通过并归档
gld planning goal reject <id> --feedback "兼容性没做"
```

**没聚焦 Goal 就切到 goal 模式，AI 会一个字都写不了**并反复报
`GOAL_CONTEXT_REQUIRED`——这是设计如此，但第一次遇到很容易以为是坏了。

---

## 历史会话档案与 history-context

AI 可以把"这轮对话干了什么"写成检查点，存进项目的 `docs/history-session/`，
一个会话一个编号。开关是 `history-recording`（默认开）。

```bash
gld history                  # 列出已有档案：编号、标题、更新时间、文件
```

**这些档案默认不会自动喂给 AI。** 新对话是干净的。要让某几份在**每次新会话
初始化时**注入，得显式点名：

```bash
gld set history-context=1,3        # 只注入 1 号和 3 号档案的有界摘要（当前目录对应的项目）
gld set history-context=           # 清空，恢复"什么都不注入"
```

注入的是**有界摘要**（标题 + 片段），不是全文——档案可能很长，整段塞进去
会把上下文吃光。

**代价是每次新会话都要付这些 token。** 只点名真正需要跨会话记住的那一两份，
不要把所有档案都列上。

---

## Durable Task 的工作区基线

**Durable 指任务元数据持久化，不是命令进程和输出持久化。** 任务、事件存于
`GLD_HOME/harness/`，Planning 存于项目 `.gld/`，历史档案存于项目 `docs/history-session/`。
它们不能互相代替，也没有因为用了 Task 就自动得到验证证据、任务回滚或发布审批。

### 任务怎么收尾：带证据才算 completed

```text
exec_command cmd='cargo test'                         → 记下它回的 session_id
task_manage action=finish task_id=<id> evidence_session_ids=["<session_id>"]
```

`finish` 不会替你跑测试。它只收**这个任务期间用 `exec_command` 起的、退出 0 的命令**当证据，
而且要求：命令跑的时候 gld 没往工作区写过东西，跑完之后工作区也没再变（比的是逐文件
SHA-256 指纹和 HEAD）。全部满足才进 `completed`，证据记进 `change_summary.verification`
（命令原文、退出码、当时的指纹）。有一条不满足就整个拒收，报 `VERIFICATION_REJECTED`，
任务状态不动，`details.rejected` 逐条说原因：

| `code` | 意思 | 怎么办 |
| --- | --- | --- |
| `EVIDENCE_FAILED` | 退出非零、超时、被杀 | 修好再跑 |
| `EVIDENCE_NOT_FINISHED` | 命令还在后台跑，或结束后还没人读到 | `read_output` 读到它结束（读到那一刻才记终态） |
| `EVIDENCE_STALE` | 跑的时候或跑完之后文件变了，测的不是现在的内容 | 重跑一次 |
| `EVIDENCE_NOT_FOUND` | 不是这个任务期间起的命令，或 id 写错 | 用 `details.evidence_candidates` 里列的 |

不带证据调 `finish` 进 `verifying`，回包的 `evidence_candidates` 列出现在就能用的命令；
`verifying` 里仍可跑测试、改文件（改了之前的证据自然作废）。确认放弃正式验收才用
`allow_unverified=true`，收成 `completed_unverified`——它只说明没有被接受的证据，
不代表没测过。

证据能证明"这条命令在现在这份文件上跑过、退出 0"，证明不了这条命令测到了该测的东西：
`true` 也退出 0。所以记录里留着命令原文，给看的人判断。命令运行时自己改了文件
（测试缓存、快照）照样收，但回包 `warnings` 和 `change_summary.risks` 会点名是哪些文件。

**暂停就是停写**：`pause` 之后 `exec_command` / `apply_patch` 报 `TASK_PAUSED`，`resume` 之后
恢复；暂停的任务仍占着任务位，开不了下一个。它不会替你停掉已经在跑的命令。
完整使用边界见 [项目开发生命周期](project-lifecycle.md)。

### 工作区指纹

开了 Durable Task（`task_manage action=start`）之后，写类工具（`exec_command`、
`apply_patch`）每次执行前会比一次**工作区指纹**：任务开始时记一份，之后每次工具
自己写完再记一份。对不上就拒绝执行并报 `FILE_CHANGED_EXTERNALLY`——意思是
"有人在 AI 的记账之外改了文件，它手上的认知已经过期了"。

不计入指纹的：`.git/`、`node_modules/`、`target/`、`dist/`、`build/`、`.venv/`、
`__pycache__/`、`.next/`、`coverage/` 这类构建产物和依赖缓存，工作区指到用户目录时才会
碰到的 `Library/`、`AppData/`，以及 **gld 自己在项目里的状态目录 `.gld/`**（Planning
状态存在这儿，而它每次工具调用都可能被写）。这些目录里的改动任务发现不了。
名单只对**目录**生效，任意一层都算；叫 `build` 的脚本、叫 `dist` 的文件照样计入。
History 档案（`docs/history-session/`）计入指纹，但 history 工具写完会自动把指纹记上账。

读不到的文件（没权限、遍历出错）不在指纹里，内容变了也看不出来。它们不会被当成
"不存在"悄悄略过：`task_manage action=status` 回 `baseline_complete: false` 和
`unreadable_paths`，验收时回包 `warnings` 也会点名。

**报了 `FILE_CHANGED_EXTERNALLY` / `BASELINE_STALE` 之后怎么恢复**：先看，再接纳。

```text
task_manage action=refresh_baseline task_id=<id>
    → changes：从任务上次记账到现在变了哪些文件；current.fingerprint：现在的指纹。什么都不改
task_manage action=refresh_baseline task_id=<id> accept_fingerprint=<current.fingerprint> reason="用户手改了 README"
    → 把现在的工作区接纳为新基线，记一条带 reason 的事件
```

看过之后工作区又变了，接纳会报 `BASELINE_CHANGED_SINCE_REVIEW`，得重新看——这是为了
不把没看过的改动一起吞进去。不属于这个任务的改动，先恢复原样再接纳。
升级前开的任务没存"上次记账时的逐文件清单"，第一次看到的是相对任务开始时的全部改动
（`compared_with: task_start`），会比实际外部改动多。

指纹要把剩下的每个文件完整读一遍算 SHA-256。实测工作区里有一个 511 MB 的文件时，
每次写操作前多等约 1.8 秒（内存不涨）。大文件放进上面哪个目录里，或者这种工作区别开
任务。没开任务时不算指纹。

> 这两条都是 0.3.0 修的。之前它们都算进指纹，而工具自己就会写它们——
> 结果是一开任务，第一次写操作就被判成"外部修改"，任务模式整个用不了。

---

## 命令的输出能读多久，stdin 怎么关

**输出保留 5 分钟，从进程结束那一刻算起**，和 `timeout_ms` 无关——一条给了
10 分钟上限、跑 3 秒就完的命令，和一条本来就跑 3 秒的命令，保留期一样长。
`read_output` 每次都回 `expires_in_ms`（还剩多久）和 `retention_ms`（总共多久）；
还在跑的命令没有这个数，它的保留期还没开始算。

同一个项目最多留 **32 条已经结束**的会话，超了就把结束得最早的那条收掉
（还在跑的一条都不动）。每条会话的 stdout / stderr 各留最后 1 MiB。

已知回收记录仍在时，再拿那个 `session_id` 报 `SESSION_EXPIRED`；它表示输出已释放，
**不表示命令没执行、失败或可以安全重跑**。有副作用的命令必须先核对实际结果。
`SESSION_NOT_FOUND` 只表示当前主体找不到该引用，也可能是服务重启、回收记录淘汰或主体
不同，不能断言它从未存在。已知的回收原因由 `details.reason` 区分：

| `reason` | 意思 |
| --- | --- |
| `expired` | 保留期到了 |
| `evicted_over_quota` | 结束的会话太多，它是最早那条 |
| `terminated` | 被停掉了：`kill_session`、切到 plan 模式、项目被删掉、服务停了 |

（**另一个客户端**拿你的 `session_id` 来读，报的仍然是 `SESSION_NOT_FOUND`——
会话表按"项目 + 谁在调"分，它连"有过这么一条"都不该知道。）

**stdin 有三种状态**，`exec_command` 的 `stdin_mode` 说了算：

| `stdin_mode` | 什么时候是默认 | 行为 |
| --- | --- | --- |
| `close` | 没给 `stdin` | 起来就关。`cat`、`grep foo` 这种读标准输入的命令立刻拿到 EOF 正常结束，**不会挂到超时** |
| `once` | 给了 `stdin` | 写进去，然后关 |
| `interactive` | 给了 `tty: true` | 写进去，留着不关，后面用 `write_stdin` 接着喂 |

三种都在这次调用返回**之前**就安排好，所以 `yield_time_ms: 0`（起了就转后台）
也不会丢掉初始输入。

`tty: true` 是 `stdin_mode=interactive` 的老名字，它**不给命令一个终端**：底下
是管道，认 TTY 才肯工作的程序（`less`、要密码的 `ssh`、带颜色的 REPL）不会因为
它变得可用。结果里的 `pty` 永远是 `false`。

**真 PTY 不做**（2026-09-21 定）。要它就得引入平台相关的伪终端实现或第三方
依赖，而换来的能力很窄：需要终端才肯工作的程序大多有非交互开关（`--yes`、
`--no-color`、从环境变量读密码）。所以这里只保证一件事——`pty` 恒为 `false`，
**不会有哪个版本偷偷把它变成 `true`**，你可以照着这个前提写代码。

`write_stdin` 最多等 **5 秒**。管道缓冲是有限的（Linux 64 KiB，macOS 更小），
对面不读的话写满就卡住；到点报 `STDIN_WRITE_TIMEOUT`，并**说清写进去了多少
字节**（`details.bytes_written`）——"写了一半"和"一个字节都没写"，接下来该做的
事不一样。

## 参数名写错了会被拒，不会按默认值跑

工具只收自己 schema 里写了的参数。多给一个，这次调用直接报
`INVALID_ARGUMENT`，**什么都不做**：

```text
exec_command cmd="cargo build" timeout=600000
→ INVALID_ARGUMENT
  exec_command does not take timeout. It takes: argv, cmd, confirm, cwd,
  filesystem_scope, max_output_bytes, reason, stdin, stdin_mode,
  timeout_ms, tty, workdir, yield_time_ms
  details.unknown_arguments = ["timeout"]
  details.executed = false
```

超时参数叫 `timeout_ms`。以前写成 `timeout` 这条调用会**成功**，用默认的
30 秒跑——你以为给了 10 分钟，命令在第 30 秒被杀掉，返回值里没有一个字提过
那个 `timeout` 被扔了。看着像"这条命令莫名其妙超时"，实际是参数根本没生效。
这类错误最难查，因为它长得像成功。

两个例外：

- **`env` 另有说法。**报的是"服务端不让调用方设环境变量"，而不是"没有这个
  参数"——它不是拼错了，是被策略挡的，这两句话的下一步不一样。
- **下划线开头的键**（`_host_session_key`）是服务端自己往参数里塞的，本来就不
  在 schema 里，不拒。

预检收的参数和真跑的一样。`patch_check` 现在也收 `confirm` 和
`notebook_edits`：想先试一遍那次带确认的 workflow 改动，就得能把 `confirm`
一起传进去，否则预检回答的是另一个问题。只有 `dry_run` 不收——那是
`patch_check` 自己定死的 `true`，收进来只会让人以为能关掉。

---

## 读文件：一行比 `max_bytes` 还长的时候

`read_file` 按行翻页：拿返回的 `next_start_line` 再调一次，直到它变成 `null`。
**只有一种情况这样翻会丢字节**——某一行本身就比 `max_bytes` 长。这时这一页只
给了这行的前半截，`next_start_line` 指向下一行，中间那段谁都没读到。

压缩过的 JS、一行导出的 JSON 就长这样。以前只有一句 warning 说"剩下的跳过
了"，没有任何办法把它捞回来；翻到文件末尾 `next_start_line` 变成 `null`，
看着就像全读完了。

现在这一页会多给两个字段：

```text
read_file path=bundle.js max_bytes=32768
→ next_start_line  = 2          ← 按行翻页会跳到下一行
  skipped_bytes    = 199901     ← 跳过这么多字节
  next_start_byte  = 32768      ← 想读它们，从这个字节接着读
```

```text
read_file path=bundle.js start_byte=32768
→ read_mode = "bytes"
  content   = …（接上的那一段）
  next_start_byte = 65536       ← null 表示读到文件尾了
```

给了 `start_byte` 就是**按字节读**：不数行，所以 `start_line`、`total_lines`
这些一律是 `null`——不是 0，是"没算"。要数行就得从文件头再扫一遍，而这个入口
存在的理由正是不去扫。

两个常见报错：

- `start_byte 落在一个字符中间`：UTF-8 里一个汉字 3 个字节，自己算的偏移容易
  切在中间。用上一次返回的 `next_start_byte`，它一定在边界上。
- `start_byte 超过文件末尾`：文件在这期间被改短了，重新读一次头。

平常读代码用不上这个参数：`skipped_bytes` 是 0、`next_start_byte` 是 `null`
就说明按行翻页没丢东西。

---

## 文件版本：别让补丁盖掉别人的改动

`read_file` 和 `patch_check` 会回一个 `version`，`apply_patch` 收
`expected_versions`。AI 改文件的正常流程因此是：

```text
read_file notes.md            → version "412-18d6b0…"
（照着读到的内容做补丁）
apply_patch patch=… expected_versions={"notes.md": "412-18d6b0…"}
```

中间要是有人（你在编辑器里、另一个 AI 会话、`git checkout`）写过这个文件，
这次补丁会被拒，报 `FILE_VERSION_CONFLICT`，**一个字节都不落盘**。没有这道
门的时候，AI 会拿着已经过期的内容把你的改动无声盖掉——它自己也不知道。

新建文件的前置条件写成 `null`，意思是"这个路径上应当什么都没有"。

`version` 是**文件大小 + 修改时间**，不是内容 hash。`read_file` 是流式的，
读 2 GB 文件的前 200 行不必碰后面；要算 hash 就得每次把整个文件读一遍。
代价：文件从备份恢复、或者连时间戳一起复制过来，内容没变也会报成变了——
虚惊一场。反过来，内容被等长替换且修改时间被保留时也可能漏检，因此不能把这个版本令牌
称为内容身份或强 CAS；验收证据应另外绑定被测内容的摘要。

### 三道门，和它们各自管不到的地方

| 门 | 管什么 | 管不到什么 |
| --- | --- | --- |
| `expected_versions` | 你读文件到提交补丁之间，别人写过它 | 调用方不传就没有这道门（它是可选参数，不传时行为和以前一样） |
| 落盘前复核（自动，不用传任何参数） | 这一次调用里，算完补丁到写下去之间文件变了 | 复核到 rename 之间仍有竞态窗口；不承诺跨编辑器的原子 CAS |
| 工作区写协调（自动） | 同进程共用写锁；同一 `GLD_HOME` 下的 gld 进程还经文件锁协调 | 不同 `GLD_HOME`、编辑器等未参与的写者；命令转后台之后也不持续持锁 |

**这不是跨编辑器的强 CAS。** 三道门合起来把窗口压到很小，并且任何一道发现
不对都是拒绝、不是覆盖；但操作系统层面没有"比对通过就锁住直到我写完"这种
东西，另一个进程仍然可能刚好插在最后那几微秒里。要真正互斥，得让写方都走
同一个 gld。

---

## Jupyter notebook：按 cell 读，按 cell 改

`.ipynb` 是一份 JSON，一个带输出的 notebook 几万行很常见。AI 拿着那份 JSON
手搓补丁，改对的概率很低。所以有两个专门的入口：

```text
read_notebook path=analysis.ipynb        每个 cell 一段，带 id、类型和输出
apply_patch notebook_edits=[{path, cells:[{cell_id, new_source}]}]
```

`cell_id` 就是 `read_notebook` 显示的那个。没有 id 的老 notebook（nbformat
4.5 之前）用 `cell-0`、`cell-1` 这样的序号，和 Claude Code 的规则一样。

`edit_mode` 三种：`replace`（默认）、`insert`（插在 `cell_id` 之后，不给
`cell_id` 就插在最前面）、`delete`。**替换一个代码 cell 会清空它的输出**——
旧输出配新代码是骗人的。

### 三件要知道的事

**`read_file` 没变。**对 `.ipynb` 它照旧返回磁盘上那份 JSON。已经有人照着那段
文本用普通补丁改 notebook，换成 cell 视图等于让那些补丁全部失效。两个入口
并存，各用各的。

**cell 编辑和普通补丁在同一次事务里。**一次 `apply_patch` 可以既改几个源文件
又改 notebook 的 cell，任何一处失败全都不落盘。版本前置条件（上一节）一样管着
notebook。

**输出里的图片不会发给 AI**，只标注"有一张 image/png，大约多少字节"。gld 的
工具结果是单块的，发不了图文交错。经服务连远端 ccnm 项目时那边会发图片——
同一个模型在两种成员上看到的 notebook 因此略有不同，这是已知差异。

---

## 还有这些，在别的文档里

一件事只写一处，下面这几个概念的完整说明不在本页：

| 概念 | 去哪看 |
| --- | --- |
| 三种认证方式（oauth / bearer / noauth）怎么选 | [connect-clients.md](connect-clients.md#认证方式对照) |
| `confine-reads` —— 读能不能出项目目录 | [security.md](security.md) |
| `allowed-commands` 和 `only:` 前缀 —— 命令白名单为什么"写了等于没写" | [security.md](security.md) |
| 守护进程是怎么回事、文件放哪、开机自启 | [daemon.md](daemon.md) |
| 各种客户端具体怎么接 | [connect-clients.md](connect-clients.md) |
| 每个凭据名分别是干什么的 | `gld secret keys` |
| 每个项目字段的取值 | `gld fields`（`--all` 含 Actions 那条线路） |

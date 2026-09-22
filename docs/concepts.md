# 这些名词到底是什么意思

gld 里有十来个概念，名字看着都认识，但**默认值和边界**跟直觉常常不一样。
这篇只讲"它是什么、什么时候该用、用错了会怎样"，具体怎么配在各自的文档里。

各节都能单独看，按需跳：

- [工作区](#工作区workspace) · [一个工作区能不能装多个项目](#一个工作区能不能装多个项目) · [聚合入口 hub](#聚合入口hub)（含[别的机器上的成员](#成员还可以在别的机器上)）
- [MCP 和 Actions 是两条线路](#mcp-和-actions-是两条线路) · [共享密钥池](#共享密钥池shared-secrets)
- [拿公网地址的三种方式](#拿公网地址的三种方式) · [工具集 tool-profile](#工具集tool-profile)
- [权限模式 permission-mode](#权限模式permission-mode) · [Planning 三种模式](#planning-三种模式)
- [历史会话档案](#历史会话档案与-history-context) · [Durable Task 的工作区基线](#durable-task-的工作区基线)

---

## 工作区（workspace）

**一个工作区 = 一个本地项目目录 + 一套只属于它的配置。** `gld start ~/code/api`
之后，这个目录就有了自己的端口、认证方式、密钥、隧道、工具集、命令白名单。

AI 通过 MCP 连上来之后，**能读能写的范围就是这个目录**。换句话说，工作区既是
"哪个项目"，也是"边界到哪儿"。

**不用先登记再启动。** `gld start <目录>`（或在目录里直接 `gld start`）发现这个
目录还没登记过，就当场登记，并在输出第一行写明"已登记工作区「x」"。
登记错了：`gld destroy <名称>`，项目文件不会被动。
想只登记不启动，仍然可以用 `gld workspace add`。

**停和销毁是两回事。** `gld stop` 只是让服务不再跑（`--all` 停所有工作区的），
配置密钥都留着，`gld start` 立刻还能用；`gld destroy` 把这个工作区在 gld 这边的
一切删掉——端口、认证、密钥、隧道配置、历史与 Planning 记账。密钥没有备份，
客户端里存的 token / 口令会跟着失效，所以它默认要确认一次（`-y` 跳过）。
两者都不会碰项目文件。

**端口是自动挑的。** MCP 从 28766 起、Actions 从 8787 起往上找空闲端口——
既避开别的工作区，也避开机器上其他程序正在监听的端口。撞上了要自己指定：
`gld start --port 30000`，或对已有工作区 `gld upgrade --port 30000`。

**大多数命令不用写 `-w`：**

1. 你显式给了 `-w <id | id 前缀(≥4位) | 名称 | 路径>` —— 用它；
2. 没给，就看**当前目录**属于哪个工作区（嵌套时取最深的那个）；
3. 还定不了，但你**总共只有一个**工作区 —— 就用它。

所以单项目用户基本永远不用打 `-w`；多项目的话 `cd` 进去就行。

> 第 3 条对 `start` / `share` 不适用：在一个还没登记的目录里启动，要的是这个
> 目录，而不是碰巧唯一的那个别的项目——所以它们走"登记当前目录"，不走这条回退。

> 名称重复时 `-w api` 会报"匹配到多个工作区"并列出来，改用 id 前缀即可。

### 一个工作区能不能装多个项目

能。把**父目录**登记成工作区，让 AI 用 `set_default_cwd` 切到某个子项目：

```text
你：先 set_default_cwd 到 proj-a，再看看 git 状态
AI：（调 set_default_cwd path=proj-a，然后 git_status）
```

切过去之后相对路径会自动补前缀：`read_file path=README.md` 读到的是
`proj-a/README.md`，`git_status` 返回的是 proj-a 那个仓库的分支和改动，
`exec_command` 的工作目录变成 `<工作区>/proj-a`。子项目各自的 `.git` 照常识别。

好处是**客户端里只配一条连接器**就够了。代价是下面五条：

**1. 它是默认值，不是边界。** 路径以 `..` 开头或写成绝对路径就不加前缀——
在 proj-b 里执行 `exec_command cmd='cat ../proj-a/README.md'` 照样读得到隔壁项目。
真正的硬边界只有工作区根，也就是那个父目录。**没有"锁死在 proj-a"这回事**，
指望 AI 不碰兄弟项目只能靠它自觉。

**2. 整个服务共用一份，不是一个会话一份。** 在 ChatGPT 里切到 proj-a 的同时，
连着同一个工作区的 Cursor 也跟着切过去了。单人单会话没问题；两个会话同时开着
各改各的项目，会出现"我明明在 proj-a，读出来的却是 proj-b 的文件"。

**3. 说明文件只读工作区根那一份。** `AGENTS.md` / `CLAUDE.md` 只在父目录里找，
不往子目录递归，proj-a 自己的 `CLAUDE.md` 不会注入。

**4. Planning 和 History 全混在一起。** `.gld/planning/state.json` 和
`docs/history-session/` 都落在父目录根上，十个项目的 Goal 和会话档案堆成一堆。

**5. [Durable Task](#durable-task-的工作区基线) 的指纹是整棵树扫。**
子项目一多，每次写操作前都要重扫一遍父目录，明显变慢。项目多就别开它。

怎么选：

| 情况 | 建议 |
| --- | --- |
| 想一条连接器接好几个项目，边界也要清楚 | [聚合入口](#聚合入口hub)：每个项目还是独立工作区，客户端只配 hub 这一条，上面五条一条都不沾 |
| 客户端里多配几条无所谓 | 一个项目一个工作区，公网入口用[全局入口](connect-clients.md#多个项目共用一个域名全局入口)共享——一条隧道，N 条连接器 |
| 一堆自己的小脚本，不在乎上面五条 | 父目录做一个工作区，对话开头让 AI 先 `set_default_cwd` |

---

## 聚合入口（hub）

**客户端里只配一条连接，访问你挑出来的几个工作区；每个工作区照旧各管各的。**

```bash
gld hub add api web          # 把工作区加进来：名称、id 或路径，立即生效
gld hub start                # 起在 http://127.0.0.1:28764/mcp
gld hub show --reveal        # 客户端要填的地址和凭据
```

具体怎么接 Claude Code / ChatGPT 见
[connect-clients.md](connect-clients.md#一条连接接多个工作区聚合入口)。

### AI 那边看到什么

- `list_workspaces`：列出 hub 里的成员（名称、id、路径、工具集）。
- 其余每个工具都多一个**必填**参数 `workspace`，填成员的名称或 id：

  ```text
  read_file  workspace=api  path=src/main.rs    读的是 api 项目的 src/main.rs
  read_file  workspace=web  path=src/main.rs    读的是 web 项目的 src/main.rs
  read_file  path=src/main.rs                   报 WORKSPACE_REQUIRED，报错里列出能填什么
  ```

- `workspace_context`：取某个工作区自己的说明文件、Skill、历史摘要和 Planning 模式。
  hub 会提示 AI 第一次进一个工作区前先调它。

名称重复时填名称会报 `WORKSPACE_AMBIGUOUS`，要求改填 id——不会挑一个猜。

### 成员还可以在别的机器上

如果项目在另一台机器上、并且那台机器用 [ccnm](https://github.com/xwfe/ccnm) 管着，
可以把它也加进 hub：

```bash
gld hub remote add prod --node work --remote-workspace server
```

`--node` 和 `--remote-workspace` 填的都是 **ccnm 配置里的名字**，不是 host 也不是
路径——在那台机器上跑 `ccnm workspace list` 能看到有哪些。（叫 `--remote-workspace`
是因为 `-w/--workspace` 是全局参数，那个说的是"本机哪个工作区"，两回事。）

前提：本机装了 `ccnm`，并且它能连到那台机器。gld 起的是公开命令
`ccnm mcp bridge`，SSH 连接、凭据和对面的目录全由 ccnm 自己管，gld 不碰。

**远端成员的工具是另一套，名字带 `remote_` 前缀。**只读的七个，任何远端成员都有：

```text
remote_workspace_info  workspace=prod                     对面项目的名字、git 状态、平台
remote_read_file       workspace=prod  path=src/main.rs   读对面的文件
remote_list_files      workspace=prod  path=src           列对面的目录
remote_search_text     workspace=prod  query=TODO         在对面搜，只有命中结果过网络
remote_load_skill      workspace=prod                     对面项目自带的 skill，不带名字就是列表
remote_view_image      workspace=prod  path=shots/a.png   看对面的图（PNG/JPEG/GIF/WebP）
remote_read_notebook   workspace=prod  path=a.ipynb       按 cell 读对面的 Jupyter notebook
```

**能写的成员**（`--mode coding`）还多五个，都要先 `remote_coding_begin` 拿一个句柄：
`remote_apply_patch`（改文件，含整文件覆盖和改 notebook 的 cell）、`remote_exec_command`
（跑命令，`cmd` 是 argv、`shell` 是一行 bash）、`remote_read_output`（分页读输出）、
`remote_stop_command`（停掉后台命令）、`remote_coding_end`（关会话、放写锁）。

**长命令往后台放。**hub 对一次远端调用最多等 60 秒，超了这条连接会被丢掉、coding 会话
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

`list_workspaces` 里远端成员带 `kind: "remote"`，没有 `path`——那是对面机器上的
目录，gld 不知道，编一个比不报更糟。`gld hub show` 里那一列显示的是
`node:workspace`。

删掉用 `gld hub remote rm prod`。和本地成员不同，本地的"移出 hub"只是不再暴露、
工作区本身还在；远端成员除了这份配置没有别的东西，所以是真删。

连接是**用到才建**：第一次调用才起 `ccnm mcp bridge`，之后复用，闲 5 分钟收掉，
`gld hub stop` 时正常关闭（让对面读到 EOF 而不是直接杀，否则远端的写锁会留下
标记要人工恢复）。

### 为什么不会串

和上面[父目录那条路](#一个工作区能不能装多个项目)逐条对比：

| | 父目录 + `set_default_cwd` | 聚合入口 |
| --- | --- | --- |
| "当前在哪个项目" | 服务端记着，所有对话共用一份 | **服务端不记**，每次调用自己带；两个对话同时各干各的也不会串。所以 hub 里没有 `set_default_cwd` |
| 边界 | 父目录，`../proj-a` 照样读得到 | 每个项目自己的根目录，`..` 和绝对路径的规则和单独连这个工作区一样 |
| 命令会话 | 共用一张表 | 按"项目 + 谁在调"分：api 里起的命令，拿它的 `session_id` 去 web 读报 `SESSION_NOT_FOUND`；**另一个客户端在 api 里读也一样报它**，见下面第四条 |
| Planning / History / Durable Task | 全堆在父目录 | 在各自项目里（本来就存在每个项目的 `.gld/`、`docs/history-session/`） |
| 说明文件、Skill | 只读父目录那份 | 按工作区单独取，api 的 `AGENTS.md` 不会拿去指导 web |
| 工具集、命令白名单、读限制 | 父目录一套 | 用每个成员自己的。hub 只收紧不放宽：web 是 `read-only`，经 hub 也写不了（报 `TOOL_NOT_ALLOWED_IN_WORKSPACE`） |

另外四条：

- **凭据不互通。** hub 有自己的一套 token / 口令，工作区的 token 打到 hub 上是 401，反过来也是。
- **不在 hub 里的工作区，AI 看不见。** 填它的名字和填一个不存在的名字，报错一模一样，
  `list_workspaces` 里也没有它。
- **日志各记各的。** 经 hub 对 api 的请求记在 api 自己的请求日志里（带 `[hub]` 前缀，
  `gld logs -w api` 看得到），web 的日志里没有；hub 自己在数据目录的 `logs/hub/` 里记全部。
- **命令会话还按"谁在调"再分一层。** 同一个 api 里，另一个 OAuth 客户端拿着你的
  `session_id` 去 `read_output` 或 `kill_session`，报的也是 `SESSION_NOT_FOUND`——
  它连"有这么一条命令在跑"都不该知道。**前提是它们身份分得开**：`noauth` 下谁都是
  同一个主体，共用一条 bearer 令牌的客户端也是。真要互相看不见，给每个客户端各注册
  一个 OAuth 客户端。

### 改了什么要不要重启

| 操作 | hub 要不要重启 |
| --- | --- |
| `gld hub add` / `gld hub rm`、成员自己 `gld ws set` | **不用**，下一次调用就生效。hub 每次请求都重新读成员表和成员配置 |
| `gld hub set`、`gld hub regen` | hub 在跑就自动重启 |
| 守护进程重启 | hub 自己回来（不看 `restore-on-launch` 开关）；`gld hub stop` 过的不回来 |

被移出的成员，它**经 hub** 起的还在跑的命令，会在下一次有请求进 hub 时被结束——
收回的是"经这个入口访问它"的权限，所以只停经 hub 起的那些，这个项目自己的服务
和命令行起的命令照常跑。

**改了配置的成员不会被停命令。** hub 会按新配置重建它的上下文，但正在跑的命令和
它的 `session_id` 都还在——改一行 AI 说明就把跑着的 `npm run dev` 杀掉，那是以前的
毛病。要停命令用 `gld hub stop`（停整个 hub 经手的），或者让 AI 调 `kill_session`。

### 代价：一把钥匙开几扇门

拿到 hub 凭据的人能进 **hub 里的全部成员**，而且连上来调一次 `list_workspaces` 就知道有哪几个。

- 只把愿意放在一起的项目加进来；要单独给出去的项目，让它自己连。
- hub 挂公网（`--global-gateway true` 或 `--public-url`）时，gld 拒绝 `noauth`。
- 和[共享密钥池](#共享密钥池shared-secrets)不是一回事：共享池是几条连接器共用一把钥匙，
  地址还是各是各的；hub 是一个地址加一把钥匙。

### 目前没有的

- **按调用者分范围。** 所有拿着 hub 凭据的客户端看到同一组成员；要分开，就别都加进来。
- **GPT Actions 版。** 聚合入口只有 MCP。
- **用量统计。** 经 hub 的请求不计入 `gld usage`。

---

## MCP 和 Actions 是两条线路

一个工作区同时提供两个 HTTP 服务，**各自独立的端口、认证、隧道、密钥**：

| | MCP | GPT Actions |
| --- | --- | --- |
| 协议 | MCP Streamable HTTP（JSON-RPC） | OpenAPI 3.1 + REST |
| 给谁用 | ChatGPT 连接器、Claude Code、Cursor、Codex | 自定义 GPT（不支持 MCP 连接器的场景） |
| 默认端口 | 28766 起 | 8787 起 |
| 默认认证 | oauth | api_key |
| 默认启动 | `gld start` 起它 | 要 `gld start -s actions` |

**绝大多数人只用 MCP。** Actions 是给"只能导入 OpenAPI 文档"的自定义 GPT 准备的
退路，不需要就一直让它停着——它不启动不占任何资源。

配置字段一一对应：MCP 侧的 `port`、`auth`、`tunnel`… 在 Actions 侧就是
`actions.port`、`actions.auth`、`actions.tunnel`。**不写前缀就是改 MCP。**

```bash
gld ws set port=30000            # 改 MCP 端口
gld ws set actions.port=9000     # 改 Actions 端口
gld ws fields                    # 看全部字段（--all 连 actions.* 一起列）
```

---

## 共享密钥池（shared-secrets）

**共享的只有凭据，不是配置。**

默认每个工作区有自己一套随机生成的 `bearer_token` / `oauth_password` /
`actions_api_key`。勾上 `shared-secrets=true` 的工作区，改用**同一个池子**里的那一套：

```bash
gld ws set shared-secrets=true          # MCP 侧改用共享池
gld secret shared show bearer_token --reveal   # 池子里那份
```

实际效果（两个工作区 proj-a / proj-b）：

```text
默认（各用各的）
  proj-a 的 bearer_token   ee7eb47672944cff
  proj-b 的 bearer_token   ad8ef74629b24a17     ← 不一样

两边都 shared-secrets=true
  共享池                   c93d70a059f34768
  proj-a 实际用的          c93d70a059f34768
  proj-b 实际用的          c93d70a059f34768     ← 同一把
```

**端口、隧道、子域名、工具集、命令白名单一概不共享**，仍然各管各的。
勾了共享的工作区端口还是 28766 / 28767，互不影响。

**开关是逐工作区、逐服务的**，不是全局一刀切：`shared-secrets` 管 MCP 那半，
`actions.shared-secrets` 管 Actions 那半。你可以只让其中三个项目共享，
剩下的独立；也可以只让 MCP 共享而 Actions 各用各的。

### 什么时候开

**要在客户端里少存几份凭据的时候。** 接了 5 个项目，不开共享就得在
Claude Code / Cursor 里配 5 套 token；开了只配一份。

### 代价：一把钥匙开所有门

- 任何一个客户端泄露那份 token，**所有勾了共享的工作区一起沦陷**；
- `gld secret shared regen bearer_token` 会把所有相关服务一起重启、
  所有客户端一起失效——你得同时去改 5 个地方。

**建议：本机自用的一堆小项目开共享省事；只要有一个工作区要挂公网，那个别开。**
公网入口意味着这把钥匙的暴露面从"你的电脑"变成"整个互联网"，
再让它同时开着另外四个项目的门就太亏了。

> 密钥名逐条的用途见 `gld secret keys`。注意 `oauth_client_id` 只存在于共享池里，
> 工作区级的等价物是配置字段 `oauth-client-id`，不是密钥。

---

## 拿公网地址的三种方式

ChatGPT 跑在 OpenAI 的服务器上，只能连公网 HTTPS，`127.0.0.1` 填进去连不上。
把本地服务变成公网可达有三条路，**互斥，同时只有一条生效**：

| 方式 | 地址长什么样 | 谁在转发 | 什么时候选 |
| --- | --- | --- | --- |
| **独立隧道** | `https://xxx.trycloudflare.com/mcp` | 本机跑的 cloudflared / frpc 子进程 | 默认选它。一个工作区一条隧道 |
| **全局入口** | `https://hub.example.com/w/<工作区id>/mcp` | 本机一个反向代理 + 一条隧道，按路径分流 | 项目多、不想每个都占一个子域名。**要固定地址只能走 frp 或自建反代，它的 cf 只有临时地址** |
| **手动地址** | 你自己定 | 你自己的 Caddy / Nginx | 已经有公网机器和反代，不需要 gld 打洞 |

前两种要装 `cloudflared` 或 `frpc`（gld 不代管，PATH 里有就自动认）。

```bash
gld share                              # 独立隧道（Cloudflare 临时地址，等价 --tunnel cf）
gld share --tunnel cf:mcp.example.com  # 独立隧道（Cloudflare 固定域名）
gld share --tunnel frp:公司            # 独立隧道（FRP 固定域名）
gld share --tunnel https://x.com/mcp   # 手动地址
gld share --off                        # 都关掉
```

同样的 `--tunnel` 写法在 `gld start`（启动时一起配）和 `gld upgrade`（事后换）上通用。

全局入口不走 `gld share`（它是全局的，不属于某个工作区），见
[connect-clients.md](connect-clients.md#多个项目共用一个域名全局入口)。

**cloudflare quick 和 named 的区别：** quick 零配置但**每次重启地址都会变**，
ChatGPT 里得跟着改，适合试用；named 要你在 Cloudflare 建一条隧道拿到 token，
地址固定，用 `gld share --tunnel cf:<你的域名>` 一次把域名定下来——token
没配过会当场问，脚本里用 `--token <token>` 直接给。

**开公网入口前请读 [security.md](security.md)。** 那不是客套话——它等于把
"以你的身份在你电脑上跑命令"这件事对外开放了。

---

## 工具集（tool-profile）

决定 **AI 能看到哪些工具**。这是服务端强制的：不在集合里的工具，客户端硬发
`tools/call` 也只会得到 `Unknown tool`，不靠客户端自觉。

```bash
gld ws set tool-profile=read-only
gld tool list                       # 看当前实际暴露了什么
```

| 取值 | 工具数 | 说明 |
| --- | --- | --- |
| `compact` | 28 | **默认值**。把同类操作聚合成一个带 `action` 参数的稳定 API（`history_manage` / `planning_manage` / `task_manage`），描述也更短——工具列表本身要占 token，条目少意味着每次对话省一截 |
| `core` | 40 | compact 的聚合工具 + 拆开的旧工具名并存。客户端认旧工具名时用它 |
| `advanced` | 53 | 全部工具都暴露 |
| `read-only` | 21 | 去掉 `exec_command` / `apply_patch` / `write_stdin` / `kill_session`，只剩读和 Git 查询 |
| `compat-readonly-all` | 53 | 见下面的警告 |

上面的数字是当前版本 `gld tool list` 实测出来的，会随版本变；以命令输出为准。

### compact 还会砍掉注入给 AI 的说明，Skill 目录只给一段

省 token 不只体现在工具条数上。`compact` 下：

- **说明文件只注入工作区里的 `AGENTS.md`（或 `AGENTS.override.md`）一份**，
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

skill 目录里的脚本、参考文件，AI 用 `get_skill` 加 `file` 读——只限这个 skill 自己的目录。主目录里的用户级 skill 默认只给正文不给文件，来源明确配置之后才给，边界见[安全](security.md)那一节。

要让它们全部进去：`gld ws set tool-profile=advanced`。
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
gld planning show           # 看当前模式、Goal、Plan、执行台账
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
gld ws set history-context=1,3     # 只注入 1 号和 3 号档案的有界摘要
gld ws set history-context=        # 清空，恢复"什么都不注入"
```

注入的是**有界摘要**（标题 + 片段），不是全文——档案可能很长，整段塞进去
会把上下文吃光。

**代价是每次新会话都要付这些 token。** 只点名真正需要跨会话记住的那一两份，
不要把所有档案都列上。

---

## Durable Task 的工作区基线

开了 Durable Task（`task_manage action=start`）之后，写类工具（`exec_command`、
`apply_patch`）每次执行前会比一次**工作区指纹**：任务开始时记一份，之后每次工具
自己写完再记一份。对不上就拒绝执行并报 `FILE_CHANGED_EXTERNALLY`——意思是
"有人在 AI 的记账之外改了文件，它手上的认知已经过期了"。

不计入指纹的：`.git/`、`node_modules/`、`target/`、`dist/`、`build/`、`.venv/`、
`__pycache__/`、`.next/`、`coverage/` 这类构建产物和依赖缓存，工作区指到用户目录时才会
碰到的 `Library/`、`AppData/`，以及 **gld 自己在项目里的状态目录 `.gld/`**（Planning
状态存在这儿，而它每次工具调用都可能被写）。这些目录里的改动任务发现不了。
History 档案（`docs/history-session/`）计入指纹，但 history 工具写完会自动把指纹记上账。

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

同一个工作区最多留 **32 条已经结束**的会话，超了就把结束得最早的那条收掉
（还在跑的一条都不动）。每条会话的 stdout / stderr 各留最后 1 MiB。

到期之后再拿那个 `session_id`，报的是 `SESSION_EXPIRED` 而不是
`SESSION_NOT_FOUND`——**这两个不是一回事**：前者是"确实有过，输出已经放掉了，
重跑一次"，后者是"这个 id 从来没存在过，你记错了"。`details.reason` 说清是哪种：

| `reason` | 意思 |
| --- | --- |
| `expired` | 保留期到了 |
| `evicted_over_quota` | 结束的会话太多，它是最早那条 |
| `terminated` | 被停掉了：`kill_session`、切到 plan 模式、工作区被移出 hub |

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
虚惊一场，是安全的那一侧。

### 三道门，和它们各自管不到的地方

| 门 | 管什么 | 管不到什么 |
| --- | --- | --- |
| `expected_versions` | 你读文件到提交补丁之间，别人写过它 | 调用方不传就没有这道门（它是可选参数，不传时行为和以前一样） |
| 落盘前复核（自动，不用传任何参数） | 这一次调用里，算完补丁到写下去之间文件变了 | 复核到 rename 之间还有几微秒 |
| 进程内写锁（自动） | 同一个 gld 进程里两个会话同时打补丁 | 别的进程 |

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
工具结果是单块的，发不了图文交错。经 hub 连远端 ccnm 项目时那边会发图片——
同一个模型在两种成员上看到的 notebook 因此略有不同，这是已知差异。

---

## 还有这些，在别的文档里

一件事只写一处，下面这几个概念的完整说明不在本页：

| 概念 | 去哪看 |
| --- | --- |
| 三种认证方式（oauth / bearer / noauth）怎么选 | [connect-clients.md](connect-clients.md#认证方式对照) |
| `confine-reads` —— 读能不能出工作区 | [security.md](security.md) |
| `allowed-commands` 和 `only:` 前缀 —— 命令白名单为什么"写了等于没写" | [security.md](security.md) |
| 守护进程是怎么回事、文件放哪、开机自启 | [daemon.md](daemon.md) |
| 全局入口怎么配 | [connect-clients.md](connect-clients.md#多个项目共用一个域名全局入口) |
| 聚合入口怎么接客户端 | [connect-clients.md](connect-clients.md#一条连接接多个工作区聚合入口) |
| 每个密钥名分别是干什么的 | `gld secret keys` |
| 每个配置字段的取值 | `gld ws fields`（`--all` 含 Actions 侧） |

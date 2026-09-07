# 这些名词到底是什么意思

gld 里有十来个概念，名字看着都认识，但**默认值和边界**跟直觉常常不一样。
这篇只讲"它是什么、什么时候该用、用错了会怎样"，具体怎么配在各自的文档里。

各节都能单独看，按需跳：

- [工作区](#工作区workspace) · [MCP 和 Actions 是两条线路](#mcp-和-actions-是两条线路)
- [共享密钥池](#共享密钥池shared-secrets) · [拿公网地址的三种方式](#拿公网地址的三种方式)
- [工具集 tool-profile](#工具集tool-profile) · [权限模式 permission-mode](#权限模式permission-mode)
- [Planning 三种模式](#planning-三种模式) · [历史会话档案](#历史会话档案与-history-context)
- [Durable Task 的工作区基线](#durable-task-的工作区基线)

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
| **全局入口** | `https://hub.example.com/w/<工作区id>/mcp` | 本机一个反向代理 + 一条隧道，按路径分流 | 项目多、不想每个都占一个子域名 |
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
| `compact` | 24 | **默认值**。把同类操作聚合成一个带 `action` 参数的稳定 API（`history_manage` / `planning_manage` / `task_manage`），描述也更短——工具列表本身要占 token，条目少意味着每次对话省一截 |
| `core` | 38 | compact 的聚合工具 + 拆开的旧工具名并存。客户端认旧工具名时用它 |
| `advanced` | 51 | 全部工具都暴露 |
| `read-only` | 19 | 去掉 `exec_command` / `apply_patch` / `write_stdin` / `kill_session`，只剩读和 Git 查询 |
| `compat-readonly-all` | 51 | 见下面的警告 |

上面的数字是当前版本 `gld tool list` 实测出来的，会随版本变；以命令输出为准。

### compact 还会砍掉注入给 AI 的说明和 Skill

省 token 不只体现在工具条数上。`compact` 下：

- **说明文件只注入工作区里的 `AGENTS.md`（或 `AGENTS.override.md`）一份**，
  `.cursorrules`、`CLAUDE.md`、全局说明都不进去；
- **Skill 一个都不注入**，`list_skills` / `get_skill` 两个工具本身也不暴露。

`gld context` 会把这件事标出来——打 `✓` 的才真的进去，打 `·` 的只是扫到了：

```text
说明文件（扫到 3，实际注入 1）
  ✓ [codex/workspace] AGENTS.md  11 字
  · [claude/global] ~/.claude/CLAUDE.md  888 字
  · [cursor/workspace] .cursorrules  11 字
```

要让它们全部生效：`gld ws set tool-profile=advanced`。
代价是工具从 24 个涨到 51 个，加上多出来的说明和 Skill 目录，
每次对话的固定开销明显变大。

### `compat-readonly-all` 不是只读

名字里有 `readonly`，但它**暴露的工具和 `advanced` 完全一样（51 个，能写能执行）**。
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

不计入指纹的：`.git/`、`node_modules/`、`target/`、`dist/`、`build/` 这类，
以及 **gld 自己在项目里的状态目录 `.gld/`**（Planning 状态存在这儿，而它每次
工具调用都可能被写）。History 档案（`docs/history-session/`）计入指纹，但
history 工具写完会自动把指纹记上账。

> 这两条都是 0.3.0 修的。之前它们都算进指纹，而工具自己就会写它们——
> 结果是一开任务，第一次写操作就被判成"外部修改"，任务模式整个用不了。

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
| 每个密钥名分别是干什么的 | `gld secret keys` |
| 每个配置字段的取值 | `gld ws fields`（`--all` 含 Actions 侧） |

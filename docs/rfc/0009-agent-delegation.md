# RFC-0009：把任务交给本机的编码 Agent（Agent 委派）

日期：2026-10-22。状态：**已定；D1 已实现（`0324602`–`267cdf9`，审查 §42），没部署、没真跑过 Agent。**

- 第一稿只做 Codex，走 `codex app-server`。
- 第二稿按用户的设想改写："能委派多种 Agent（Codex、Claude Code、Pi、Antigravity、Grok、DeepSeek……），先设计得简单，
  用命令行调用对应的工具，靠简短的 recap 或者文档交流"。
- 第三稿：用户 2026-10-22 定了第 5 节三件事（照建议），并改了权限——"不要限制在沙箱，解除限制，直接可写。完成后，交回给审核"。

来源：codex-bridge 的做法（[审查 §39](../reviews/2026-09-23-lifecycle-and-docs-audit.md)），同类项目调研（第 3 节）。
承接 [hub 路线](../reviews/2026-09-19-hub-development-roadmap.md)的 L6 和 [RFC-0002](0002-shared-kernel-and-ccnm-hub.md)。
排队状态只在 [task_plan.md](../../task_plan.md) 维护。

**核过的事实：**

- 本机装的 Agent 命令行和它们的 `--help`：codex 0.163.0、claude 2.1.304、pi、agy 1.1.28、kimi 0.29.1、qwen 0.19.12。
  另有 cursor-agent，但钥匙串锁着、帮助打不出来；gemini、opencode、grok、dsh 没装。
- 本机 `~/.codex/config.toml` 的 MCP server 名单。
- 各家官方文档（第 3、4 节标了链接）。

**一个 Agent 都还没真跑过。**

## 1. 要解决什么

ChatGPT 网页端经 gld 干活，每一步都是一次工具调用来回，长任务慢、费它的上下文。本机装着好几个编码 Agent，各自有循环和模型。
想要的是：ChatGPT 把一段说得清楚的活交给其中一个（"把这几个失败的测试修好""把这个模块拆成两个"），Agent 直接动手改，
做完把 recap 交回来；ChatGPT 审 diff、用 gld 跑测试，过了才算数。

**不做的：**
- gld 不实现模型循环，只拼命令行、跑起来、收结果；
- 不替 Agent 管登录；
- 不做无人值守的流水线；
- 不排队调度，同一个项目同时只跑一个委派；
- 不做运行中审批：Agent 全权跑，审在做完之后（4.4）。

## 2. 用起来是什么样

```bash
gld set api agent-delegation=ask   # 默认 off；ask = 每次要本机点头，auto = ChatGPT 直接起
gld agent ls                        # 发起了哪些、在等谁、哪些等审核
gld agent approve <task-id>         # ask 档下放行；deny 拒掉，stop 停掉在跑的
```

ChatGPT 那边：

1. `agent_manage action=agents workspace=api`：本机能用哪些 Agent。
2. `agent_manage action=start workspace=api agent=claude prompt="…"`：立刻回 `task_id`。
   ask 档的状态是 `waiting_local_approval`，回包里写明请用户在本机 `gld agent approve <id>`。
3. `action=wait`：每次最多等 30 秒，跑完提前回。没完就回 `running`、已跑多久、最后几行输出。
4. Agent 跑完，任务变成 **`awaiting_review`（交回审核）**，回包里有：
   - 结局和退出码；
   - Agent 的 recap（最后一条消息，最多 32 KB）；
   - 交接报告（有的话）；
   - gld 自己算的改动文件；
   - 原始输出的 `output_ref`。
5. ChatGPT 审：`git_diff`、`read_file` 看改动，`exec_command` 跑测试，然后
   `action=review task_id=… verdict=accept|reject note="…"`。
   - `accept`：任务记 `accepted`，审核意见存进任务记录。
   - `reject`：任务记 `rejected`。要接着改，就带上审核意见再 `start` 一次（D3 起可以接着同一个会话）。
   - **验收照旧只认 gld 跑的命令**：Agent 自己说"测试都过了"不算，`accept` 本身也不是验收证据。

## 3. 同类项目怎么做的

| 做法 | 代表 | 怎么调 Agent | 结果怎么回来 | 对 gld |
| --- | --- | --- | --- | --- |
| 每个 Agent 一条无交互命令 | [PAL MCP 的 clink](https://github.com/BeehiveInnovations/pal-mcp-server/blob/main/clink/models.py)、[acpx exec](https://github.com/openclaw/acpx) | JSON 配置写命令、参数、超时、角色提示；提示走 stdin | 按 Agent 解析最后一条消息；超过 2 万字只回 `<SUMMARY>` 标签里的 | **最贴近这里的设想，选它**。clink 超时只杀直接子进程，这处不学 |
| 每个 Agent 用它自己最强的协议 | [vibe-kanban](https://github.com/BloopAI/vibe-kanban/tree/main/crates/executors/src/executors)（2026-04 关停转社区） | Claude 用 stream-json 加 stdin 控制协议，Codex 用 app-server，Gemini、Qwen 用 ACP，OpenCode 用 HTTP；npx 钉死版本；每次一个 git worktree | 各自的事件流 | 能力最全，但一个 Agent 一套代码，不做 |
| ACP（Agent Client Protocol） | [Zed 发起](https://agentclientprotocol.com/protocol/overview)，JetBrains 加入 | stdio 上的 JSON-RPC：`session/new`、`session/prompt`、`session/cancel`；Agent 反过来请求权限 | `session/update` 事件 | Gemini、OpenCode、Qwen、Kimi、Grok Build、DeepSeek Harness 原生支持，Claude、Codex、Pi 要 npm 适配器；Rust SDK 变得快。**以后要中途补充指令时再换** |
| 终端模拟 | [coder/agentapi](https://github.com/coder/agentapi) | 内存里跑终端，把 HTTP 请求翻成按键 | 屏幕快照 diff | 仓库已归档，不走 |
| 文件交接 | [AGENTS.md](https://agents.md/)、[Anthropic 长任务做法](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents)、[Amp handoff](https://ampcode.com/news/handoff) | 长期规矩放 AGENTS.md，进度放 JSON 文件加 git 提交 | 文件本身 | 没有跨 Agent 通用的 handoff 标准。gld 只约定一个目录、两个文件（4.3） |

## 4. 设计

### 4.1 Agent 档案：一张配置表

每种 Agent 一条档案。gld 内置几份，用户可以在数据目录的 `agents.toml` 里改写或新加。档案只描述"怎么调"，不写代码：

```toml
[agents.codex]
command = ["codex", "exec", "--json", "-o", "{result_file}", "-C", "{workdir}",
           "--skip-git-repo-check", "--dangerously-bypass-approvals-and-sandbox", "-"]   # "-" = 提示从 stdin 读
result = { from = "file" }                 # 最后一条消息在 {result_file}
session = { jsonl_field = "thread_id" }    # 接着同一个会话用（D3）

[agents.claude]
command = ["claude", "-p", "--output-format", "json", "--dangerously-skip-permissions"]
result = { from = "json", field = "result" }
session = { json_field = "session_id" }
stop_signal = "INT"                        # SIGTERM 会直接退出 143、这一轮什么都不记

[agents.pi]
command = ["pi", "-p", "--no-session"]     # Pi 没有权限系统，本来就是全权
result = { from = "stdout" }

[agents.agy]
command = ["agy", "-p", "--output-format", "json", "--dangerously-skip-permissions"]
result = { from = "json", field = "response" }

[agents.kimi]
command = ["kimi", "-p", "{prompt}", "-y", "--output-format", "text"]
result = { from = "stdout" }
```

- 用的是**用户自己的配置**（模型、MCP server、全局规则），和平时在终端里用它一样；档案里想加什么参数自己加。
- **`env_pass`**：放行哪些环境变量。gld 现在一律去掉名字像密钥的变量，有的 Agent 靠环境变量里的 key 登录（比如 DeepSeek Harness），要点名放行。
- **`verified`**：验过的版本。
- **`idle_timeout`**：多久没输出算卡住。
- **`stop_signal`**：停的时候先发什么信号。
- Qwen 这一版的帮助里没找到"全放开"的参数，Grok、Gemini、dsh、OpenCode 没装，它们的档案到 D2 核过帮助再写。

### 4.2 一次委派怎么跑

1. 查项目的开关、档位、发起人能不能写（第 5 节）；这个项目已有委派在跑就拒绝。
2. 按档案拼 argv，`{workdir}` 是项目根。提示写进 stdin，写完就关：stdin 不关，有的 Agent 会一直等输入。
3. 后台跑：复用 `exec_command` 那套进程组、运行记录、有界输出、重启检查，不另起一套执行器。
   [审查 §40](../reviews/2026-09-23-lifecycle-and-docs-audit.md) 的"有命令在跑不重启"自然把委派也算进去。
4. 两道超时：总时长（默认 30 分钟，最长 2 小时）和多久没输出（默认 5 分钟）。
   到点按档案的信号停：SIGINT，等一会儿，SIGTERM，再 SIGKILL，停的是整个进程组。"是 gld 主动停的"由 gld 自己记，不靠退出码猜。
5. 取结果：按档案读结果文件、JSON 字段或 stdout。取不到、或者格式不认识，记 `failed`，带输出前 2 KB 和 Agent 的 `--version`。
6. 算改动：跑之前、跑完各记一次 `git status`，两次一比就是改了哪些文件。
7. 状态变成 `awaiting_review`，放开项目写锁（ChatGPT 审的时候要跑测试）。

### 4.3 recap 和交接文档

gld 在用户的提示后面固定加一段尾巴，要求 Agent 结尾写一段 recap，分四条：

- 做了什么；
- 改了哪些文件；
- 跑了什么验证、结果如何；
- 没做完的和下一步。

要交代的东西多时，用一个目录交接，两边都能读写：

- **目录**：项目里的 `.gld/handoff/<task-id>/`。`.gld/handoff/` 自带只写 `*` 的 `.gitignore`，交接文件不进 git status。
  `.gld/` 是 Planning 也在用的目录，它的 `.gitignore` 照 Planning 的规矩只在第一次建出来时写，用户为共享 Planning 删掉了就不补回来。
- **`brief.md`**：ChatGPT 起任务前用 gld 写进去（背景、约束、验收标准），提示里告诉 Agent 先读它。
- **`report.md`**：Agent 写详细报告。有就回给 ChatGPT（最多 64 KB），没有就只回 recap。
- **`review.md`**：ChatGPT `review` 时的意见，gld 写进去。打回重做时，下一次 `start` 的提示里告诉 Agent 先读它。

这是 [Anthropic 长任务做法](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents)和 [Amp handoff](https://ampcode.com/news/handoff) 的最小版本：
不发明新格式，就是几份 markdown 加 git diff。

### 4.4 权限：不限制，做完交回审核

用户定的：**不限制在沙箱，直接全权可写，做完交回审核。** 每个档案都用这个 Agent 自己"全放开"的参数（4.1），
跑起来和你在终端里给它开了全部权限一样，碰得到的不止项目目录，是你账号能碰的一切。

所以把关放在**起之前**和**做完之后**：

- **起之前**：项目开关默认 off；`ask` 档每次要你在本机点头；只有服务主凭据和 `--write` grant 能发起（第 5 节）。
  ChatGPT 读的网页、issue 里要是夹着提示注入，能借 ChatGPT 传给 Agent，而 Agent 是全权的——**对外接触多的项目用 `ask`，别用 `auto`**。
- **做完之后**：状态停在 `awaiting_review`，审核结论记进任务；验收只认 gld 自己跑的命令。
- **不递归**：委派出去的 Agent 不能再经 gld 发起委派（它的主体没有 `agent_manage`）。
- **Plan 模式下不能委派**：Plan 模式本来就拦写和执行，全权的委派等于绕过它。

### 4.5 工具面

- ChatGPT 看到一个工具 `agent_manage`，`action` 有 `agents`、`start`、`status`、`wait`、`review`、`stop`、`list`，
  和 compact 档里 `task_manage` 的写法一致。
- 工具表多一项，指纹会变，**要在 ChatGPT 点一次 Refresh**。没开委派的项目照样看得到这个工具，调用时回"这个项目没开"和怎么开。
- `wait` 单次最多 30 秒，经 OpenAI 隧道时官方上限 120 秒。
- 回包有界、先脱敏：recap 最多 32 KB，report 最多 64 KB，改动文件最多 200 个。
- 本机命令行：`gld agent ls | show | approve | deny | stop`。

## 5. 已定的三件事（2026-10-22，照建议）

1. **本机项目由 gld 直接用命令行调本机 Agent；远端成员以后走 `ccnm.machine/1`（D4）。**
   ccnm 拒绝 Agent 和 Runtime 是同一账号同一台（ccnm `docs/configuration.md` 第 210 行），而 gld 最常见的就是这种。
   路线 §5.6 已跟着改。
2. **谁能发起**：按项目开关 `agent-delegation=off|ask|auto`，默认 off；只有服务主凭据和 `--write` grant 能发起，
   只读 grant、浏览器 grant 一律不能；ask 档 24 小时没人放行就作废。
   **D1 实际做得更紧：所有 grant 都不能起、审、停**——调用方身份里分不出浏览器 grant（它往下传时不带标记），
   要放开 `--write` grant 得先给 grant 加上这个标记，放到 D2。
3. **在哪跑**：在项目目录里跑，同一个项目同时只许一个。跑着的时候 gld 对这个项目拦写文件和 `exec_command`，读的照常；
   跑完、进入 `awaiting_review` 就放开。不开 git worktree。

## 6. 分阶段

| 阶段 | 做什么 | 怎么算做完 | 花不花额度 |
| --- | --- | --- | --- |
| D1 | 档案表、执行和收尾、`agent_manage`（含 `review`）、`gld agent`、三档开关、项目写锁、`.gld/handoff/`；内置 Codex 和 Claude 两份档案 | 用假 Agent 脚本（按档案吐结果、卡住、不关 stdin、被停、改文件）测全部状态、超时和互斥；Codex、Claude 各在临时项目里真跑一次改一处，ChatGPT 审、gld 跑测试 | 真跑那两次要用户点头 |
| D2 | 更多档案：Pi、agy、Kimi、Qwen；Gemini、Grok、dsh、OpenCode 装了再加 | 每个档案先核帮助，再在临时项目里真跑一次 | 同上 |
| D3 | 打回后接着同一个会话：只用明确的 session id，不用 `--continue` / `--last`（并发时会接错） | 假脚本覆盖；真跑一次打回、接着改 | 同上 |
| D4 | 要中途补充指令时换 ACP；远端成员经 `ccnm.machine/1` | 另起小节 | 同上 |

## 7. 最容易踩的坑（调研里各家撞过的）

- **卡住**：stdin 没关一直等输入；非 TTY 下挂住（agy 早期版本）；无限等 MCP 工具确认（Gemini 修过）；`claude -p` 收尾时还要等后台任务。对策是 4.2 的两道超时。
- **"全放开"参数各家不一样、还会改名**：Codex 删过 `--full-auto`，Gemini 废了 `--yolo`，Claude `-p` 的默认权限模式改过一次。
  对策：起之前记 `--version`，不在 `verified` 里照样起、回包标出来；结果格式不认识就记 `failed` 带原文。
- **凭据**：gld 去掉名字像密钥的变量的规则会把 Agent 要的 key 也删掉，所以每个档案点名放行；gld 不读、不存、不转发任何 Agent 的登录凭据。
- **子进程树**：要停整个进程组，信号按档案来（4.2）。
- **额度**：每次委派花对应 Agent 的额度，gld 不设预算，回包里有就报 token 用量。

## 8. 要在 D1 实测的

- 各档案"全放开"的参数在 `-p` 下是不是真的一次都不停下来问。
- 四段 recap 各 Agent 实际守得怎么样，守不住就退回"最后一条消息原样回"。
- `.gld/handoff/` 放在项目里，会不会被项目自己的工具（格式化、lint）扫到。
- 用户自己的 Codex 配置带着 24 个 MCP server，起动慢不慢、会不会因为某个 server 起不来而卡住。

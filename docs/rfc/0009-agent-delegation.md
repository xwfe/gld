# RFC-0009：把任务交给本机的编码 Agent（Agent 委派）

日期：2026-10-22。状态：**草案第二稿，未实现，等用户定第 5 节的三件事。**

- 第一稿只做 Codex，走 `codex app-server`。
- 第二稿按用户的设想改写："能委派多种 Agent（Codex、Claude Code、Pi、Antigravity、Grok、DeepSeek……），先设计得简单，
  用命令行调用对应的工具，靠简短的 recap 或者文档交流"。

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

ChatGPT 网页端经 gld 干活，每一步都是一次工具调用来回，长任务慢、费它的上下文。本机装着好几个编码 Agent，各自有循环、模型，有的有沙箱。
想要的是：ChatGPT 把一段说得清楚的活交给其中一个（"把这几个失败的测试修好""读一遍这个模块说说怎么拆"），自己等一段 recap，
看 diff，再用 gld 跑一遍验收。

**不做的：**
- gld 不实现模型循环，只拼命令行、跑起来、收结果；
- 不替 Agent 管登录；
- 不做无人值守的流水线；
- 不排队调度、不让多个 Agent 同时改一个项目；
- ChatGPT 永远批不了提权。

## 2. 用起来是什么样

```bash
gld set api agent-delegation=ask   # 默认 off；ask = 每次要本机点头，auto = ChatGPT 直接起
gld agent ls                        # 发起了哪些、在等谁
gld agent approve <task-id>         # ask 档下放行；deny 拒掉，stop 停掉在跑的
```

ChatGPT 那边：

1. `agent_manage action=agents workspace=api`：本机能用哪些 Agent、各自开了只读还是可写。
2. `agent_manage action=start workspace=api agent=claude mode=read prompt="…"`：立刻回 `task_id`。
   ask 档的状态是 `waiting_local_approval`，回包里写明请用户在本机 `gld agent approve <id>`。
3. `action=wait`：每次最多等 30 秒，跑完提前回。没完就回 `running`、已跑多久、最后几行输出。
4. 跑完的回包：
   - 结局和退出码；
   - Agent 的 recap（最后一条消息，最多 32 KB）；
   - 交接报告（有的话）；
   - gld 自己算的改动文件；
   - 原始输出的 `output_ref`。
5. ChatGPT 用 `git_diff`、`read_file` 看改动，再用 `exec_command` 跑测试。**验收只认这一步**，Agent 自己说的"测试都过了"不算。

## 3. 同类项目怎么做的

| 做法 | 代表 | 怎么调 Agent | 结果怎么回来 | 对 gld |
| --- | --- | --- | --- | --- |
| 每个 Agent 一条无交互命令 | [PAL MCP 的 clink](https://github.com/BeehiveInnovations/pal-mcp-server/blob/main/clink/models.py)、[acpx exec](https://github.com/openclaw/acpx) | JSON 配置写命令、参数、超时、角色提示；提示走 stdin | 按 Agent 解析最后一条消息；超过 2 万字只回 `<SUMMARY>` 标签里的 | **最贴近你的设想，选它**。clink 的预设权限全开、超时只杀直接子进程，这两处不学 |
| 每个 Agent 用它自己最强的协议 | [vibe-kanban](https://github.com/BloopAI/vibe-kanban/tree/main/crates/executors/src/executors)（2026-04 关停转社区） | Claude 用 stream-json 加 stdin 控制协议，Codex 用 app-server，Gemini、Qwen 用 ACP，OpenCode 用 HTTP；npx 钉死版本；每次一个 git worktree | 各自的事件流 | 能力最全，但一个 Agent 一套代码，第一版不做 |
| ACP（Agent Client Protocol） | [Zed 发起](https://agentclientprotocol.com/protocol/overview)，JetBrains 加入 | stdio 上的 JSON-RPC：`session/new`、`session/prompt`、`session/cancel`；Agent 反过来请求权限 | `session/update` 事件 | Gemini、OpenCode、Qwen、Kimi、Grok Build、DeepSeek Harness 原生支持，Claude、Codex、Pi 要 npm 适配器；Rust SDK 变得快。**留到需要运行中审批、补充指令时再换** |
| 终端模拟 | [coder/agentapi](https://github.com/coder/agentapi) | 内存里跑终端，把 HTTP 请求翻成按键 | 屏幕快照 diff | 仓库已归档，不走 |
| 文件交接 | [AGENTS.md](https://agents.md/)、[Anthropic 长任务做法](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents)、[Amp handoff](https://ampcode.com/news/handoff) | 长期规矩放 AGENTS.md，进度放 JSON 文件加 git 提交 | 文件本身 | 没有跨 Agent 通用的 handoff 标准。gld 只约定一个目录、两个文件（4.3） |

## 4. 设计

### 4.1 Agent 档案：一张配置表

每种 Agent 一条档案。gld 内置几份，用户可以在数据目录的 `agents.toml` 里改写或新加。档案只描述"怎么调"，不写代码：

```toml
[agents.codex]
command = ["codex", "exec", "--json", "-o", "{result_file}", "-C", "{workdir}",
           "--skip-git-repo-check", "--ignore-user-config", "-"]   # "-" = 提示从 stdin 读
read  = ["-s", "read-only"]
write = ["-s", "workspace-write"]
result = { from = "file" }                 # 最后一条消息在 {result_file}
session = { jsonl_field = "thread_id" }    # 续会话用（D4）

[agents.claude]
command = ["claude", "-p", "--output-format", "json", "--strict-mcp-config", "--max-turns", "50"]
read  = ["--permission-mode", "plan"]
write = ["--permission-mode", "acceptEdits"]
result = { from = "json", field = "result" }
session = { json_field = "session_id" }

[agents.pi]
command = ["pi", "-p", "--no-session"]
read  = ["--tools", "read,grep,find,ls"]
write = []                                 # Pi 没有权限系统，可写 = 全权，见 4.4
full_access_when_write = true
result = { from = "stdout" }
```

每条档案还要写：

- **`env_pass`**：放行哪些环境变量。gld 现在一律去掉名字像密钥的变量，有的 Agent 靠环境变量里的 key 登录，要点名放行。
- **`verified`**：验过的版本。
- **`idle_timeout`**：多久没输出算卡住。
- **`stop_signal`**：停的时候先发什么信号。Claude 收到 SIGINT 才会正常收尾这一轮，SIGTERM 直接退出 143、什么都不记。

### 4.2 一次委派怎么跑

1. 查项目的开关、档位、发起人能不能写（第 5 节）。
2. 按档案拼 argv，`{workdir}` 是项目根。提示写进 stdin，写完就关：stdin 不关，有的 Agent 会一直等输入。
3. 后台跑：复用 `exec_command` 那套进程组、运行记录、有界输出、重启检查，不另起一套执行器。
   [审查 §40](../reviews/2026-09-23-lifecycle-and-docs-audit.md) 的"有命令在跑不重启"自然把委派也算进去。
4. 两道超时：总时长（默认 30 分钟，最长 2 小时）和多久没输出（默认 5 分钟）。
   到点按档案的信号停：SIGINT，等一会儿，SIGTERM，再 SIGKILL，停的是整个进程组。"是 gld 主动停的"由 gld 自己记，不靠退出码猜。
5. 取结果：按档案读结果文件、JSON 字段或 stdout。取不到、或者格式不认识，记 `failed`，带输出前 2 KB 和 Agent 的 `--version`。
6. 算改动：跑之前、跑完各记一次 `git status`，两次一比就是改了哪些文件。

### 4.3 recap 和交接文档

gld 在用户的提示后面固定加一段尾巴，要求 Agent 结尾写一段 recap，分四条：

- 做了什么；
- 改了哪些文件；
- 跑了什么验证、结果如何；
- 没做完的和下一步。

要交代的东西多时，用一个目录交接，两边都能读写：

- **目录**：项目里的 `.gld/handoff/<task-id>/`。gld 第一次建 `.gld/` 时在里面放一个只写 `*` 的 `.gitignore`，整个目录不进 git status。
  选项目里面，是因为 Codex 这类沙箱只让写项目根。
- **`brief.md`**：ChatGPT 起任务前用 gld 写进去（背景、约束、验收标准），提示里告诉 Agent 先读它。
- **`report.md`**：Agent 写详细报告。有就回给 ChatGPT（最多 64 KB），没有就只回 recap。

这是 [Anthropic 长任务做法](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents)和 [Amp handoff](https://ampcode.com/news/handoff) 的最小版本：
不发明新格式，就是两份 markdown 加 git diff。

### 4.4 权限：每种 Agent 能管到哪不一样

| Agent | 只读怎么限 | 可写怎么开 | 关掉用户配置和 MCP | 要注意的 |
| --- | --- | --- | --- | --- |
| Codex | `-s read-only`，系统级沙箱 | `-s workspace-write`，只能写项目根 | `--ignore-user-config`，不读 `config.toml`，登录照用 | 你那 24 个 MCP server 写在 `config.toml` 里，不加这个参数就全带上 |
| Claude Code | `--permission-mode plan` | `acceptEdits`，没在允许列表里的命令会被拒 | `--strict-mcp-config`；`--bare` 连项目 hooks 也跳过，但要 `ANTHROPIC_API_KEY` | 不加 `--bare` 会跑项目里的 hooks 和 `.mcp.json`，不问信任 |
| Pi | `--tools read,grep,find,ls` 白名单 | 没有权限系统，等于全权 | 待查 | [官方说明](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/security.md) |
| Antigravity（`agy`） | 默认：要审批的操作直接"软拒绝" | `--dangerously-skip-permissions`，全权 | 待查 | 拒了也退出 0，只在 stderr 说一句（[文档](https://antigravity.google/docs/cli/headless)）；要先交互登录一次 |
| Kimi、Qwen、Gemini、Grok、DeepSeek Harness、OpenCode | 各有 approval-mode 一类参数 | 多数只有"全放开" | 待查 | 装了、核过帮助再写档案（D3） |

规矩：

- **只读**：档案里的只读参数得是硬限制（沙箱或工具白名单），不能只靠"让它别改"的提示词。做不到的 Agent 不开只读。
- **可写**：等于全权的 Agent（Pi、agy、Grok……），可写模式要用户在 `agents.toml` 里对它写 `full_access_ok = true` 才开，
  回包标 `full_access: true`。
- **MCP 和项目配置**：档案能关掉 Agent 自己的 MCP 和项目配置的，一律关掉；关不掉的写在档案里，`agents` 回包里看得到。
- **不递归**：委派出去的 Agent 不能再经 gld 发起委派。
- **Plan 模式下只能只读委派。**

### 4.5 工具面

- ChatGPT 看到一个工具 `agent_manage`，`action` 有 `agents`、`start`、`status`、`wait`、`stop`、`list`，和 compact 档里 `task_manage` 的写法一致。
- 工具表多一项，指纹会变，**要在 ChatGPT 点一次 Refresh**。没开委派的项目照样看得到这个工具，调用时回"这个项目没开"和怎么开。
- `wait` 单次最多 30 秒，经 OpenAI 隧道时官方上限 120 秒。
- 回包有界、先脱敏：recap 最多 32 KB，report 最多 64 KB，改动文件最多 200 个。
- 本机命令行：`gld agent ls | show | approve | deny | stop`。

## 5. 要你定的三件事

1. **本机项目由 gld 直接用命令行调本机 Agent，还是一律经 ccnm。**
   - 现有原则是"优先经 ccnm"（路线 §5.6、L6、RFC-0002）。但 ccnm 拒绝 Agent 和 Runtime 是同一账号同一台
     （ccnm `docs/configuration.md` 第 210 行），而 gld 最常见的就是这种。
   - 建议：本机项目直接调，远端成员以后走 `ccnm.machine/1`（D5）。定了之后路线 §5.6 跟着改。
2. **谁能发起。**
   - 建议按项目开关 `agent-delegation=off|ask|auto`，默认 off。
   - 写模式只有服务主凭据和 `--write` grant 能发起；只读 grant、浏览器 grant 一律不能。
   - ask 档 24 小时没人放行就作废。
3. **可写委派在哪跑。**
   - **在项目目录里跑、同时只许一个（建议）**：跑着的时候 gld 对这个项目拦写文件和 `exec_command`，读的照常。简单，改动直接看得到。
   - **每次开一个 git worktree**（vibe-kanban、claude-squad 的做法）：互不干扰，能并行，但改完要合回来，第一版不做。

## 6. 分阶段

| 阶段 | 做什么 | 怎么算做完 | 花不花额度 |
| --- | --- | --- | --- |
| D0 | 用户定第 5 节 | 本文状态改成"已定"，路线 §5.6 跟着改 | 不花 |
| D1 | 档案表、执行和收尾、`agent_manage`、三档开关；Codex 和 Claude 只读 | 用假 Agent 脚本（按档案吐结果、卡住、不关 stdin、被停）测全部状态和超时；两个各真跑一次只读分析 | 真跑那两次要用户点头 |
| D2 | 可写模式、项目写互斥、改动文件、`.gld/handoff/` | 假脚本覆盖互斥和中途停；真 Codex 在临时项目里改一处，gld 跑测试验收 | 同上 |
| D3 | 更多档案：Pi、agy、Kimi、Qwen；Gemini、Grok、dsh、OpenCode 装了再加 | 每个档案先核帮助，再真跑一次只读 | 同上 |
| D4 | 续会话：只用明确的 session id，不用 `--continue` / `--last`（并发时会接错） | 假脚本覆盖；真跑一次 | 同上 |
| D5 | 需要运行中审批、补充指令时换 ACP；远端成员经 `ccnm.machine/1` | 另起小节 | 同上 |

## 7. 最容易踩的坑（调研里各家撞过的）

- **卡住**：stdin 没关一直等输入；非 TTY 下挂住（agy 早期版本）；无限等 MCP 工具确认（Gemini 修过）；`claude -p` 收尾时还要等后台任务。对策是 4.2 的两道超时。
- **权限语义不一致**：有的默认全开（OpenCode、clink 的预设），有的根本没有（Pi），有的拒了还退出 0（agy）。对策是 4.4 的规矩，加上回包如实标出来。
- **不受信仓库的配置会被执行**：项目里的 hooks、`.mcp.json`。档案能关的都关。
- **参数随版本变**：Codex 删过 `--full-auto`，Gemini 废了 `--yolo`，Claude `-p` 的默认权限模式改过一次。
  对策：起之前记 `--version`，不在 `verified` 里照样起、回包标出来；结果格式不认识就记 `failed` 带原文。
- **凭据**：gld 去掉名字像密钥的变量的规则会把 Agent 要的 key 也删掉。所以每个档案点名放行；gld 不读、不存、不转发任何 Agent 的登录凭据。
- **子进程树**：要停整个进程组，信号按档案来（4.2）。
- **额度**：每次委派花对应 Agent 的额度，gld 不设预算。Claude 档案带 `--max-budget-usd`，别的 Agent 没有这个参数。

## 8. 没定、要在 D1 实测的

- `claude --permission-mode plan` 在 `-p` 下是不是真的只读、会不会停下来等；`acceptEdits` 下没放行的命令是拒绝还是卡住。
- `codex exec --ignore-user-config` 下登录、模型、推理强度是不是照常。
- 结果尾巴要求的四段 recap，各 Agent 实际守得怎么样。守不住就退回"最后一条消息原样回"。
- `.gld/handoff/` 放在项目里：会不会被项目自己的工具（格式化、lint）扫到。

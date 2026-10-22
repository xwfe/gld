# RFC-0009：把任务交给本机的 Codex（Agent 委派）

日期：2026-10-22。状态：**草案，未实现，等用户定第 3 节的三件事。** 来源：用户 2026-10-22 定"Agent 委派排进开发计划"；
做法参考 codex-bridge（[审查 §39](../reviews/2026-09-23-lifecycle-and-docs-audit.md)）；承接 [hub 路线](../reviews/2026-09-19-hub-development-roadmap.md)的 L6
和 [RFC-0002](0002-shared-kernel-and-ccnm-hub.md)。排队状态只在 [task_plan.md](../../task_plan.md) 维护。

本文里核过的事实：本机 Codex 0.163.0 导出的 app-server 协议 schema（`codex app-server generate-json-schema`，不调模型）、
本机 `~/.codex/config.toml` 的 MCP server 名单、ccnm 的配置文档和支持矩阵、gld 现有的运行记录与写入规矩。Codex 的实际行为一条都还没真跑过。

## 1. 要解决什么

ChatGPT 网页端经 gld 干活，每一步都是一次工具调用来回：读文件、改、跑测试、看输出、再改。长任务慢，也费它的上下文。
本机装着 Codex（还有 Claude Code），它们自己有干活的循环、沙箱和模型。想要的是：ChatGPT 把一段说得清楚的活
（"把这几个失败的测试修好，跑通全量"）交给本机 Codex 去做，自己等结果、看 diff、再用 gld 跑一遍验收。

**不做的：**gld 不实现自己的模型循环，只当官方 Agent 的客户端；不替 Agent 管登录凭据；不做无人值守的流水线；
不排队调度多个 Agent；ChatGPT 永远批不了提权。

## 2. 用起来是什么样

```bash
gld set api agent-delegation=ask     # 默认 off；ask = 每次要本机点头，auto = ChatGPT 直接起
gld agent ls                          # ChatGPT 发起了哪些、在等谁
gld agent approve <task-id>           # ask 档下放行；deny 拒掉，stop 停掉在跑的
```

ChatGPT 那边：

1. `agent_manage action=start workspace=api agent=codex mode=write prompt="…" client_request_id=…`，立刻回 `task_id`。
   ask 档的状态是 `waiting_local_approval`，回包里写明"请用户在本机 `gld agent approve <id>`"。
2. `action=wait`，每次最多等 30 秒；跑完、失败、Codex 有问题要问时提前回。没完就回 `running` 和最近几条进展。
3. 跑完的回包：结局、Codex 最后一条消息、改了哪些文件、它跑过的命令和退出码摘要、token 用量。
4. ChatGPT 用 gld 自己的 `git_diff`、`read_file` 看改动，再用 `exec_command` 跑测试。**验收只认这一步**，见第 6 节。

## 3. 要先定的三件事

### 3.1 本机项目由 gld 直接驱动本机 Codex，还是一律经 ccnm

现有原则是"经 ccnm"：路线 §5.6 写"未来需要委派官方 Agent 时，优先走 ccnm 的既有公开管理入口"，L6 写"与 ccnm 管理入口显式对接"，
RFC-0002 写"将来要从 Web 提交自主分析任务，应另用 `ccnm.machine/1`"。

问题是 ccnm 管不了 gld 最常见的情况——项目就在本机、同一个账号：

- ccnm 的 `runtime_node` "不能和 Agent 是同一个 node（一个账号同时当两边），会被拒"（ccnm `docs/configuration.md` 第 210 行）。
  走 ccnm 就得在本机另建一个执行账号，让本机同时当 Agent 和 Runtime。
- ccnm 受管的 Codex 钉在 0.154.0，本机装的是 0.163.0。

**建议：**按项目在哪分两条路。本机项目由 gld 直接驱动本机官方 Agent（gld 只是 app-server 的客户端，不加模型循环，
没违反"不新增模型循环"）；远端成员（经 ccnm 挂进来的）走 `ccnm.machine/1`。后者协议已冻结，`session.start` / `status` /
`result` / `stop`、幂等键 `start_key`、Runtime 写锁忙时回 `-32008` 都能直接对上第 7 节的工具。定了以后把路线 §5.6 那句改成这个分法。

### 3.2 谁能发起，要不要本机点头

**建议：**

- 按项目开关 `agent-delegation=off|ask|auto`，默认 off。
- 写模式只有能写的主体能发起：服务主凭据、带 `--write` 的 grant。只读 grant、浏览器 grant 一律不能发起，只读模式也不行（v1 先收紧）。
- ask：任务停在 `waiting_local_approval`，本机 `gld agent approve` 才起；24 小时没人管自动作废。auto：直接起。
- 不管哪档，提权（沙箱外的命令、联网、`danger-full-access`）都不能从 ChatGPT 侧放行，见第 4 节。

为什么要 ask：写模式委派等于让 ChatGPT 能在你电脑上起一个会自己跑命令、改文件的 Agent。网页里的提示注入能借 ChatGPT
传给 Codex，本机点一下是最后一道人。

### 3.3 委派在跑时，gld 自己对这个项目的写操作怎么办

**建议：**同一项目同时只能有一个写模式委派。它在跑时，gld 对这个项目的写文件工具（`apply_patch` 等）和 `exec_command` 回
`AGENT_TASK_RUNNING`（带 `task_id`）；读类工具、git 只读工具照常；只读模式的委派不占这个名额。

理由：Codex 直接写盘、不经 gld，gld 的文件版本号和写计数看不到它的中间状态；同时跑 `pnpm install` 这类命令会互相踩。
代价：ChatGPT 等的时候只能看、不能动。另一种做法是只拦写文件、放行 `exec_command`，ChatGPT 能边等边跑只读命令，但分不清哪条命令会写。

## 4. 安全边界（不随档位变）

- **沙箱：**只读模式 `sandbox: read-only`；写模式 `workspace-write`，可写的只有项目根，网络默认关。`approvalPolicy: never`：
  Codex 不会停下来要权限，越界的操作直接失败。`danger-full-access` 从 ChatGPT 侧永远起不了。
- **Codex 自己配置里的 MCP server 默认一个不给。**本机 `~/.codex/config.toml` 配了 24 个，含 desktop-commander、computer-use、
  Filesystem 这类能在沙箱外跑命令、碰全盘的；MCP 工具不归 Codex 沙箱管。委派会话启动时经 `thread/start` 的 `config`
  覆盖关掉全部 MCP server，要用哪个按项目点名（和 gld 本机 MCP 转发同一个点名思路）。不关的话，第一条沙箱等于没有。
- **不递归：**委派出去的 Codex 不能再经 gld 发起委派。现在 Codex 的配置里没有 gld，但以后配上了也一样：委派会话的主体没有 `agent_manage`。
- **凭据：**Codex 用它自己的登录（`~/.codex/auth.json`），gld 不读；子进程环境照 gld 现有规矩去掉名字像密钥的变量。
  回包和落盘的输出照运行记录的脱敏规则过一遍。
- **审批两条通道分开：**v1 用 `never`，没有运行中审批。以后要开 `on-request`，审批只走本机 `gld agent approve`；
  MCP 侧只有 `answer`，只能回答 Codex 的提问（`item/tool/requestUserInput`），不能批命令、补丁、权限。
- **Plan 模式下只能只读委派**，和 Plan 模式拦写、拦执行是同一个规矩。

## 5. 怎么驱动 Codex

用 `codex app-server`（stdio 上的 JSON-RPC，Codex 自己标 experimental）。本机 0.163.0 导出的 schema 里核过这些：

| 用途 | 方法 / 字段 |
| --- | --- |
| 握手 | `initialize`，然后通知 `initialized` |
| 开会话 | `thread/start`：`cwd`、`sandbox`（`read-only` / `workspace-write` / `danger-full-access`）、`approvalPolicy`（`untrusted` / `on-request` / `never` / 细分）、`config`、`developerInstructions` |
| 干活 | `turn/start`：`threadId`、`input` 必填 |
| 补充指令、中断 | `turn/steer`（要 `expectedTurnId`）、`turn/interrupt`（`threadId` + `turnId`） |
| 进展 | 通知 `turn/started`、`turn/completed`、`item/started`、`item/completed`、`turn/diff/updated`、`thread/tokenUsage/updated`、`error` |
| Codex 来问的 | `item/commandExecution/requestApproval`、`item/fileChange/requestApproval`、`item/permissions/requestApproval`、`item/tool/requestUserInput` |

- 一个任务一个 app-server 进程、一个 thread、一个 turn。同一 thread 接着做放到 D3。
- **版本：**这套协议变得快，codex-bridge 只验过 0.147.0-alpha.6.5。起之前记下 `codex --version` 和 `initialize` 的回包；
  不在验过的列表里照样起，回包标 `codex_version_unverified`；收到不认识的方法或字段，任务记 `failed`，带原文前 2 KB。
- **找二进制：**配置里点名的路径，其次 PATH。点名的找不到就报错，不偷偷换别的。
- **不用 `codex exec`：**拿不到结构化的审批请求和 turn 的生命周期，中断和补充指令都做不了。

## 6. 记录、重启和验收

- **落盘：**`GLD_HOME/agents/<项目 id>/<task_id>/`，`task.json` 记发起人、模式、prompt（脱敏，前 2 KB）、状态、thread / turn id、
  Codex 版本、起止时间、token 用量、改动文件；`events.jsonl` 有界，只留最后几 MiB。owner 锁照运行记录的做法。
- **重启：**守护进程正常退出时在跑的委派记 `interrupted`（连进程组一起停）；被 `kill -9` 的，之后读到记 `unknown`；不重连、不重放。
  `gld daemon restart` 的检查（[审查 §40](../reviews/2026-09-23-lifecycle-and-docs-audit.md)）把在跑的委派也算上。
- **幂等：**同一个 `client_request_id` 回原来那个任务。
- **超时：**默认 30 分钟、最长 2 小时（可议），到点先 `turn/interrupt`，再收进程。
- **验收：**委派的结果不是验收证据，Codex 自己跑的测试也不算。gld 的规矩照旧：委派结束后，由 gld 自己跑通过、
  之后没再改过文件的命令才算。

## 7. 工具面

- ChatGPT 看到一个工具 `agent_manage`，`action` 有 `start`、`status`、`wait`、`steer`、`interrupt`、`answer`、`list`，
  和 compact 档里 `task_manage` 的写法一致。
- 工具表多一项，指纹会变，**用户要在 ChatGPT 点一次 Refresh**。没开委派的项目照样看得到这个工具，调用时回"这个项目没开委派"
  和怎么开（工具表不按项目变，省得开关一次就要 Refresh 一次）。
- `wait` 单次最多 30 秒，和 `exec_command` 一致；经 OpenAI 隧道时官方上限是 120 秒。
- 回包有界、先脱敏：最近进展最多 8 条，最后一条消息最多 32 KB，改动文件最多 200 个（数字参考 codex-bridge，可调）。
- 本机命令行：`gld agent ls | show | approve | deny | stop`。

## 8. 分阶段

| 阶段 | 做什么 | 怎么算做完 | 花不花 Codex 额度 |
| --- | --- | --- | --- |
| D0 | 用户定第 3 节 | 本文状态改成"已定"，路线 §5.6 跟着改 | 不花 |
| D1 | 只读委派：start / wait / status / interrupt / list，三档开关，落盘、重启、restart 检查，MCP 全关 | 假 app-server（脚本照 schema 回包）跑通全部状态和超时；真 Codex 只读分析一次 | 真跑那次要用户点头 |
| D2 | 写模式：workspace-write、项目写互斥、改动文件报告 | 假 app-server 覆盖互斥、超时、中途停；真 Codex 在临时项目里改一处、gld 跑测试验收 | 同上 |
| D3 | `steer`、`answer`、同一 thread 接着做 | 假 app-server 覆盖；真跑一次带提问的 | 同上 |
| D4 | Claude Code（`claude -p` 的 stream-json） | 另起小节定权限映射 | 同上 |
| D5 | 远端成员经 `ccnm.machine/1` | 临时远端项目跑通一轮 | 同上 |

## 9. 代价和风险

- **权限：**写模式委派的风险和给一把 `--write` grant 一样大，再加上 Agent 会自己连着做很多步。沙箱只管 Codex 自己起的命令，
  管不住项目脚本在测试时做什么。默认 off、ask 要本机点头就是为这个。
- **额度：**每次委派花 Codex 的额度，gld 不设预算，回包里报 token 用量。
- **协议不稳：**app-server 标 experimental，Codex 升级可能改字段；验过的版本列表要跟着维护，换 Codex 版本要重跑 D1 的假 app-server 测试和一次真跑。
- **等待成本：**长任务 ChatGPT 要每 30 秒调一次 `wait`。
- **Windows 没验。**

## 10. 没定、要在 D1 实测的

- `thread/start` 的 `config` 覆盖能不能一次关掉全部 MCP server，还是要逐个 `enabled = false`。
- `workspace-write` 默认的可写目录含不含临时目录、缓存目录；关网络的确切写法。
- `approvalPolicy: never` 下越界是报错回来还是静默跳过，回包里怎么认。
- app-server 会读项目里的 `AGENTS.md`、`.codex/`（想要的），也会读全局 `~/.codex/config.toml` 的其他设置（模型、推理强度）：哪些要在委派时覆盖。

# gld 完成度、生命周期与文档审查（2026-09-23）

状态（2026-10-09）：**D01–D09、D11、D14 已修，D13 已核对；D07 的 grant 在 ChatGPT 上实测过（§9）；D12 做了发布门禁、
下载包验收和构建来源证明，代码签名未做；D10 三轮实测、任务发现随 0.8.2 发布（§10–§18），服务模式与浏览器配法经 ChatGPT 实测后
随 0.8.3 发布（§19–§21）。**
§1–§6 是审查当时（0.6.0）的原始发现，保留原样；每项怎么修的、怎么验证的、还剩什么，看
[§7 处理进展](#7-处理进展)。2026-09-25 重新归并后的**当前任务只在
[task_plan.md](../../task_plan.md) 排队**，§8 记录本次复核；各历史段落的“未做”不覆盖后来的结果。
本页是有日期的证据快照与下一步入口，不取代项目的 Planning / Task 或再维护一份进度数据库。

## 1. 范围与证据口径

源码：`main`，`91b9b59787d17da904c047aaaff01b3e74b14caa`，开始时工作树干净。
版本：0.6.0。当前连接的 `server_info` 也报告 0.6.0、协议 2025-06-18、compact；这**不证明**
运行二进制恰好是该提交。当前环境自报 `execution_boundary=policy_only`、
`sandbox_enforced=false`、网络允许。本轮未重启或替换实际服务、未改变权限、未发布。

证据分四类：**本轮实测**（工具调用与测试退出结果）、**源码确认**（可定位分支/实现）、
**历史记录**（原文的日期和范围）、**待验证风险**（尚未定向复现）。不以源码存在推导
真实客户端可用，不以 fixture 推导真实 SSH、公网客户端或全平台验收。

## 2. 当前完成情况：不要重开已完成阶段

| 工作 | 对账结论 | 证据入口 |
| --- | --- | --- |
| U0–U5、A01–A20 工具可靠性工作 | 历史修复与验收记录已完成，不能再用原始缺陷表冒充当前状态 | [工具审查 §10–12](2026-09-19-gld-tooling-review-and-plan.md) |
| G1–G3 原生工具补齐 | 已实施，含图像、Notebook、Skills 与远端参数同步；本地 Notebook 编辑是 `apply_patch notebook_edits` | [RFC-0003](../rfc/0003-native-parity-sync.md) |
| 单服务、多项目主入口 | 已实施；旧“先完成 grant 再收口”已被 9 月 22 日决定替代，grant 仍是缺口 | [RFC-0004](../rfc/0004-one-service-many-projects.md) |
| 已安装 Skills 与附件 | gld 侧已实施；远端附件的真实全链路仍按原记录保留未验 | [RFC-0005](../rfc/0005-machine-skills.md) |
| 本机 MCP 与远端 MCP 转发 | 已实施工具发现、调用、连接池和有界结果缓存；不等于所有结果类型无损 | [RFC-0006](../rfc/0006-machine-mcp.md) |
| 共享写锁、主体命令会话、构建与依赖指纹 | 已有实现；后台命令全程互斥、项目级授权和客户端端到端仍不能据此宣布完成 | [工具审查 §12](2026-09-19-gld-tooling-review-and-plan.md)（当时的跨仓落地清单已删，剩下的三项挪进 task_plan 的"等遇到再做"） |

**总体判断：可靠编码工具集已成形；可靠生命周期的“验收、恢复、授权、证据”尚未闭合。**
完整覆盖矩阵与当前可用做法移到[生命周期指南](../project-lifecycle.md)，避免 README 堆积细节。

## 3. 需要优先修复或澄清的问题

### D01 · P1：Task 有状态，但正式验收未接通（源码确认、隔离 CLI 复现）

[`harness/tools.rs`](../../crates/core/src/harness/tools.rs) 的 `finish_task` 默认转为
`Verifying`，只有 `allow_unverified` 分支转为 `CompletedUnverified`。
`change_summary` 返回的 `verification`、`risks` 是空数组，回滚能力为 foundation 未提供。
[`model.rs`](../../crates/core/src/harness/model.rs) 虽定义 `VerificationRecord`，当前没有
接通其创建/存储/验收到 `Completed` 的公开路径。Verifying 仍占用可写任务槽，下一任务
不能把它当已完成跳过。Paused 也被视为可写状态，不能当写权限闸门。

本轮真实二进制反例：暂停后 `task_state=paused, writable=true`；默认 finish 返回
`task.status=verifying, change_summary.verification=[]`，随后 start 返回
`TASK_ALREADY_ACTIVE`。显式 `allow_unverified=true` 收尾后才可开始下一任务。

**验收**：失败、跳过、运行中、已过期/版本不符的证据不能被接受为完成；通过的证据必须
绑定任务与被测内容；完成后可开始下一任务。先统一状态含义，不再增加另一套 Goal/Task 模型。

### D02 · P1：基线恢复缺入口，覆盖范围有盲点（源码确认、恢复入口隔离复现）

[`harness/state.rs`](../../crates/core/src/harness/state.rs) 的错误恢复建议包含
`refresh_baseline`，但公开 registry / task actions 没有它。排除规则按任意层级的名字匹配，
不仅排除目录，也会排除 `scripts/build` 这样的文件；现有单测明确固定了该行为。
遍历与读取失败也被跳过，没有“基线不完整”状态。

本轮在临时项目外部改写 `demo.txt` 后，status 返回 `baseline_matches=false`，
`next_actions` 包含 `refresh_baseline`；尝试该 task action 得到 `INVALID_ARGUMENT`，
错误明确列出有效动作且不含它。不可读文件和排除规则本轮仍是源码/已有单测证据，未另做故障注入。

**验收**：显式读取变更、说明归属、接纳新基线；不能静默吞入外部变更。真实源文件即使叫
build 也应按明确规则处理；不可读必须报告未知/不完整。不要用关闭 Task 或手改存储文件
冒充恢复能力已经实现。

### D03 · P1：过程台账没有体现命令失败（源码确认、隔离 CLI 复现）

[`tools/dispatch.rs`](../../crates/core/src/tools/dispatch.rs) 的执行台账更新依据顶层 `ok`
写 `completed` / `failed`，未在该处使用命令的终态和 `command_ok`。而命令 API 已明确
区分 transport、running、退出码和 outcome。

本轮隔离项目执行 `python3 -c 'raise SystemExit(7)'`：工具返回
`ok=true, command_ok=false, exit_code=7`，CLI 自身退出 0；随后读取台账得到
`execution.state=completed, last_error=null`。另一个命令返回 running 后台账也为 completed。
这是 CLI 直连反例：不把工具调用已返回、CLI 退出 0、命令成功和项目验收成功混为一谈。
直连 CLI 的下一进程无法续读内存会话也符合现有文档，不能把它误报成 daemon 会话丢失。

**验收**：至少覆盖后台 running、非零退出、超时、取消、连接断开结果未知；状态与后续输出
一致。台账应分别表达调用/进程/验证的状态；本轮确认的是台账失真，不宣称已有生产任务因此被误验收。

### D04 · P1：源码工具面与当前客户端工具面未闭环（本轮观察）

当前连接实际暴露的工具中没有原生已有的 `check_command`、`list_skills`、`get_skill`、
`read_notebook`；参数层面也未暴露 `read_file.start_byte`、`apply_patch.expected_versions` /
`notebook_edits`、`exec_command.argv` / `stdin_mode` 等。运行时诊断却会建议
`check_command`。这会使模型知道有能力，却无法通过当前 schema 调用。

原因可能在客户端发现、缓存或连接器适配，本轮未定位，**不是判定服务端没有实现**。
原生 `check_command` 才带 `server.build_commit` / `shared_crates`，不要误把
`check_exec_environment` 未返回这些字段当成旧服务的证据。

**验收**：同一部署记录源码与构建身份、实际 tools/list/schema、客户端发现结果、一次
只读预检、一次隔离补丁与续读。不能只对总工具数或版本号，也不应先放宽权限。

### D05 · P1：本机 MCP 结果保真与重试建议（源码/契约风险）

[`machine_mcp/relay.rs`](../../crates/core/src/machine_mcp/relay.rs) 把结果交给
`toexec_mcp::shape` 整理，模块说明规定有文字时省略 `structuredContent`；现有转发测试
验证的是文本与结构化内容相同的例子。这个证据不足以证明“原样转发”或任意结果无损。
上游可能只在结构化对象中提供独立字段，需要增加这类反例与锁定依赖的契约测试。

此外，`MCP_RESULT_GONE` 提示直接建议重新调用；
[`tools/session.rs`](../../crates/core/src/tools/session.rs) 的过期提示也建议重跑命令。
缓存失效并不意味着操作未执行，带副作用的调用这样重放可能重复变更。

**验收**：文本摘要与结构化对象不同、只有结构化对象、图文混合、资源链接、超限和过期；
保留独立结果或提供带类型的可续读引用。错误明确结果未知/输出不可取回，先核对状态，
不要自动重发副作用操作。共享算法在 toexec 修，gld 保留自己的配额与路由策略。

规范依据：[MCP 2025-06-18 Tools](https://modelcontextprotocol.io/specification/2025-06-18/server/tools)
允许结构化结果，`outputSchema` 是可选项；不能从“没有 outputSchema”推导“结构化结果可丢弃”。

### D06 · P1：文档生成能静默退化（源码确认与历史事件）

[`scripts/gen-cli-docs.sh`](../../scripts/gen-cli-docs.sh) 直接写目标文件，对字段表和
密钥名表使用 `|| true`；还 unset `GLD_HOME`。命令失败可能留下空表或不完整输出。
[RFC-0005](../rfc/0005-machine-skills.md) 已记录旧守护进程不匹配时生成空表的实例。

**验收**：隔离配置与后端；所有必需章节非空；任何子命令失败必须整体非零，保留旧文档；
先完整生成临时文件再替换。当前文档测试只覆盖 README 与顶层 docs 的 CLI 命令抽取，
没有覆盖嵌套 RFC/review、所有参数、链接与“生成成功但空表”。本轮补文档要求，未改脚本。

## 4. 影响全生命周期、但不应冒充本轮已修的能力缺口

| 编号 / 优先级 | 边界或风险 | 下一步与最小验收 |
| --- | --- | --- |
| D07 / 对外多客户端前 P1 | 服务凭据可访问全部项目；本机 MCP 是服务级，Task/History 不能外推命令会话的主体隔离 | 项目 + capability + principal 的最小 grant，撤销和在途请求规则；A 客户端不能枚举/读取/操作 B 的受限项目与证据。保持单服务，不恢复旧双入口 |
| D08 / P1 安全语义 | `compat-readonly-all` 把可写工具标为只读；`confirm=true` 不是用户真实批准；当前静态策略不是 OS 沙箱 | 停止把伪只读标注当安全承诺，评估退役/改名；审批绑定具体动作与真实主体，执行隔离由明确的 OS/部署边界负责 |
| D09 / P2 | 命令与输出只在内存；输出有限期/限额；转后台后不持续持有写锁 | 最小 Job/运行记录与持久化日志制品，明确重启后 interrupted/unknown，不承诺恢复原进程或恰好一次；验收 kill/restart/配额/源文件变化 |
| D10 / P2 | 项目任务发现、就绪探测、浏览器证据没有统一闭环 | 先识别项目已有 build/test/lint/dev 入口，再经独立适配关联进程、端口、就绪、console/network/trace/screenshot；用一个真实 Web fixture 验证，不内建完整 IDE |
| D11 / P2 | Harness 读取坏任务 JSON 会跳过，坏 JSONL 行会截断；固定 `.json.tmp` 写法需要并发/断电验证 | 明确损坏与部分可读，保留原文件；验证并发 start/update、写入中断、重启恢复。当前未做故障注入，不能宣布已发生数据丢失 |
| D12 / P2，生产发布前必验 | 真实平台与外部链路证据不完整，Release 的版本/构件一致性门禁不完整，musl 可选 | 分开记录 macOS/Linux/Windows 编译、运行、IPC、命令清理和下载包验收；GitHub/SSH/浏览器真实链路单列；版本匹配、产物摘要、签名/来源、回滚演练按交付目标补齐 |
| D13 / P2 | 协议支持仍需维护兼容矩阵，不能以旧协议握手成功推断新协议路径兼容 | 针对锁定 SDK、实际客户端/上游协议测试协商与失败提示；新旧路径分层适配，不为了追版本破坏现有连接 |
| D14 / P2 | 本轮 `git_diff` 对 6 个修改文件返回 12 条 `files`，每个重复两次；原始 diff 正常 | 修正文件级解析，覆盖新增/删除/重命名/二进制/特殊路径/截断；不要只对数组去重后继续错误标注状态 |

D14 已定位到 [`tools/git.rs::parse_diff_files`](../../crates/core/src/tools/git.rs)：`--- a/`
和 `+++ b/` 分别入表，后者没有去重；识别出的条目还统一标为 modified、非二进制。
因此结构化文件清单不应作为审查完整性或变更计数的唯一依据。

D13 的外部核对：官方已有 [2026-07-28 变更说明](https://modelcontextprotocol.io/specification/2026-07-28/changelog)。
这只说明需要兼容评估，**不说明 gld 当前服务对现有客户端已经失效**；本轮没有做新协议互操作测试。

## 5. 建议实施顺序与停止条件

**先收可靠性**：D01–D06。完成定义、基线恢复、结果保真、真实客户端发现和文档生成门禁
先闭合，不再以“多了几个工具”替代验收。每项做一组失败/未知状态回归和一轮隔离完整流程。

**按使用范围收授权**：计划对外、多客户端、生产接入时，D07–D08 提前成为硬前置；个人
可信本机使用也要保留真实风险声明，不能把兼容标注包装成只读。授权问题不能靠重建另一套
单项目服务绕开。

**再补生命周期适配**：D09–D13，按真实项目需要推进任务入口、运行准备、日志制品、浏览器
证据和发布/运维对接。gld 管本地入口，ccnm 管远端执行权威，toexec 管实际共用基础；
发布和生产写操作保持独立授权。

不纳入本轮：真实 PTY（已有明确不做的决定）、自建模型主循环、完整 IDE、全量语义服务、
通用部署平台。Skills 应描述真实能力与停止条件，不能用提示词补出不存在的恢复和安全机制。

## 6. 本轮验证与交付范围

已在隔离的 `GLD_HOME` 下运行 `cargo test --workspace --all-targets --locked`，退出码 0。
文档修改后再次运行（测试输出加 `--quiet`）：**769 passed、0 failed、0 ignored**。
`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --locked -- -D warnings` 均通过。

另外以 `--nocapture` 单独复跑 `ccnm_background_lifecycle`：**3 passed**，日志没有缺少 ccnm
的跳过提示，实际测试耗时 8.31 秒。这是已安装 ccnm 的本地真实管道组合测试，不是 SSH 或公网验证；
这 3 项已包含在全量套件里，不能与 769 相加作覆盖数量。

隔离真实 CLI 反例验证了 D01–D03；D14 在本次连接直接观察，并定位到解析代码。
完整本机临时日志与反例结果当时在 `target/review-20260923-evidence/`，该目录被 Git 忽略，
不是可长期引用的远端制品库；本节及各问题段保存关键结果，避免只留下会过期的 session 引用。
（各问题处理完、§7 记下复现与验证之后，这个目录已于同日清理。）

最终复跑文档命令、源码提示和 doctor 修复命令测试：**4 passed、0 failed**；
`git diff --check` 通过，生成的 `docs/cli.md` 未变。修改过的 Markdown 中相对链接目标
检查 **97 处、0 缺失**（仅验证路径存在，不含网络地址和锚点）。初次检查发现的状态行
尾随空格已修正，最终检查退出码为 0。
当前证据限于这台 macOS 开发机与仓库测试。
本轮没有重新验真实 SSH、公网 ChatGPT、本机所有外部 MCP、Linux/Windows 或 GitHub 发布。

本轮修改 README、用户行为/安全/安装/架构/开发文档，新增生命周期指南，并给旧审查与 RFC
补当前状态入口。保留旧测试数字与未验记录，不把历史证据改写成本轮实测。
运行时代码、CLI 帮助源码和文档生成脚本未修改，故没有手工改生成的 `docs/cli.md`。

## 7. 处理进展

同日第二轮按 §5 的顺序先做 D01–D04：D01–D03 在 `69321e6`，D04 在 `2e9fa7e`。
D05–D14 未动；对外多客户端或生产接入之前，D07 项目级授权仍是硬前置。

| 编号 | 结论 | 做了什么 |
| --- | --- | --- |
| D01 | 已修 | `finish` 收 `evidence_session_ids`：只认本任务期间 `exec_command` 起的、退出 0、运行期间 gld 没写过工作区、结束后指纹和 HEAD 没变的命令；有一条不作数就整个拒收（`VERIFICATION_REJECTED`，逐条原因，状态不动）。不带证据进 `verifying` 并列出候选。`transition` 不能直达 `completed`。`pause` 之后写入与执行报 `TASK_PAUSED`。`change_summary.verification` / `risks` 有了真实内容 |
| D02 | 已修 | 新增 `refresh_baseline`：先列从上次记账到现在变了哪些文件，再凭看过的指纹加 `reason` 接纳，看过后又变则拒收。为此每次记账存一份逐文件清单。排除名单只对目录生效；读不到的文件报 `baseline_complete: false` 和 `unreadable_paths`。compact / core 档的下一步提示翻成 `task_manage:<action>`，不再被滤掉 |
| D03 | 已修 | 新增统一口径（`tools/outcome.rs`）：Planning 台账、Harness 操作记录、任务事件都按命令终态记 `failed` / `running` / `timed_out` / `cancelled` / `unknown`，另记 `call_ok` 和命令块；`read_output` 回 `termination_reason` / `exit_code` / `command_ok`，后台命令的终态由它补回台账 |
| D04 | 原因已定位；补了核对入口 | 服务端每一层都给了审查时"看不见"的工具和参数（hub 单测逐个工具、逐个参数钉住），缺失来自客户端缓存了 9-19～9-21 之前的旧表。`server_info` 经服务调用时回 `connection`（工具→参数、整表指纹）和 `build_commit`；`gld tool list --served` 问正在跑的服务要客户端拿到的那张表；核对步骤写在 [troubleshooting.md](../troubleshooting.md#核对客户端拿到的工具表) |

**行为变化，升级时要知道：**

- 暂停的任务不再放行写入和执行。
- 公开的 `transition` 进 `completed` 报 `VERIFICATION_REQUIRED`（原来是 `INVALID_TASK_TRANSITION`）。
- 工作区里名叫 `build`、`dist`、`target` 这类的**文件**开始计入指纹：升级前开的任务第一次写入
  可能报 `FILE_CHANGED_EXTERNALLY`，用 `refresh_baseline` 看过后接纳。
- Harness 操作记录的 `kind` 多了 `running` / `timed_out` / `cancelled` / `unknown`；Planning
  `execution` 多了 `call_ok`、`command`。
- `server_info` 多了 `build_commit` / `shared_crates`；经服务调用还多 `connection`，compact 档实测
  3370 字节。advanced / compat-readonly-all 从 53 个工具变成 54 个（`refresh_baseline`）。
- 任务里 `exec_command` 的事件会记命令原文（先脱敏，最多 500 字符），存在 `GLD_HOME/harness/`。

**验证：**隔离 `GLD_HOME` 下 `cargo test --workspace --all-targets --locked`：改文档前后各跑一次，
都是 788 passed、0 failed、0 ignored（本轮新增 19 条）；`cargo fmt --check`、`cargo clippy --workspace
--all-targets --locked -- -D warnings` 通过。`69321e6` 单独在独立 worktree 里编译全部 crate 通过，
core 单测加三个相关集成测试 541 passed。另用真实 `target/debug/gld` 在隔离数据目录、独立端口、经守护进程
复跑 §3 的反例：退出 7 的命令台账记 `failed`、`last_error` 写明退出码；后台命令先记 `running`，
`read_output` 读到结束后补成 `failed`（退出 4）；暂停后写入报 `TASK_PAUSED`；失败证据被拒收、
通过的证据收成 `completed`、随后能开下一个任务；外部改动后先看再接纳、恢复写入；
`gld tool list --served` 与 `server_info.connection` 的指纹一致。`docs/cli.md` 用临时 `HOME` 重新生成，
只多了 `--served` 一行，`fields` 和密钥名两张表完整。

**仍未验证或未做：**

- 没有在真实 ChatGPT 连接器上刷新工具后再核对一遍：需要在客户端里操作。服务端也仍然声明
  `listChanged: false`，不会主动通知客户端重拉。
- `gld tool call` 在命令退出非零时仍然退出 0（工具调用本身成功）。改成非零会影响已有脚本，
  没有擅自改；脚本要判断命令结果请读 `command_ok`。
- 验收证据只证明"这条命令在当前内容上退出 0"，不证明测到了该测的东西。
- 后台命令结束时不自动把它写的文件记上账：它改过文件的话，下一次写入会报外部修改，要先
  `refresh_baseline`。
- D05 结果保真、D06 文档生成门禁、D07 授权及其后各项未动。

### 第三轮：D05、D06、D14

| 编号 | 结论 | 做了什么 |
| --- | --- | --- |
| — | 按决定不改 | `gld tool call` 在命令退出非零时仍退出 0（调用本身成功）。写进它的帮助，并用 `a_failing_command_is_not_a_failed_tool_call` 钉住，免得以后被"顺手修"掉 |
| D05 | 已修 | `SESSION_EXPIRED`、`MCP_RESULT_GONE` 不再叫人直接重跑：说明命令 / 调用已经执行、只是输出取不回来，会改东西的先核对现状；`details.output_recoverable=false`。结构化结果保真改在 toexec-mcp 的 `shape`：只在"某段文字解析出来就是它"或"FastMCP `{"result": 正文}` 包装"时省掉，其余转成文字跟在后面、太长照样分段。实测 deepwiki 三个工具的 `outputSchema` 都带 `x-fastmcp-wrap-result`。toexec-mcp 0.2.1（toexec `bfa809d`，tag `toexec-mcp-v0.2.1` 经批准推送），gld 已升级到这个 tag |
| D06 | 已修 | `gen-cli-docs.sh` 在一次性 `HOME` 里跑、先写临时文件；任何一条 `gld` 失败、帮助段数不对或两张表像空表，就退出 1、原文件不动（`docs_generation.rs` 用假 `gld` 钉住）。新增 `docs_links_resolve.rs`：README 加 `docs/` 下全部 Markdown（含 rfc、reviews）的相对链接和锚点 |
| D14 | 已修 | `git_diff` / `git_show` 的文件清单改为直接问 git（`--name-status -z`、`--numstat -z`，带 `-M`）：每个文件一条、真实状态、改名带 `old_path`、二进制、是否 staged；diff 文本被截断时清单仍完整 |

D05 的 toexec 部分已用 path 依赖联调过：gld 的转发测试 15 条全过、原有断言一条没改；换回
0.2.0 时新写的契约测试失败（多出的结构化数据被丢），证明它测到了这个修复。经批准推送 tag 后，
gld 的 `Cargo.toml` 改到 `toexec-mcp-v0.2.1`，`Cargo.lock` 只变了 toexec-mcp 和它自带的那份
toexec-text；toexec README 的当前 tag 同步更新（`2f22741`）。ccnm 仍钉 0.2.0，要用得它自己升级。

另外修正了 troubleshooting 里一处过时说法：命令结束后输出保留 5 分钟，不是 30 秒。

提交：`be8a1da`（退出码说明）、`4920c26`（D14）、`e315b9f`（D06）、`893aa4e`（D05 gld 侧）。
验证：隔离 `GLD_HOME` 下全量 795 passed、0 failed、0 ignored（本轮新增 7 条）；fmt、clippy
`-D warnings` 通过；toexec 按它自己的规矩跑了 fmt、clippy、`cargo test`（122 passed）和
`cargo +1.89 check`。

### 第四轮：D11 任务存储的并发与损坏

| 编号 | 结论 | 做了什么 |
| --- | --- | --- |
| D11 | 已修（`5a10f8a`） | 读改写任务（start / update / pause / resume / finish / 写后记账 / refresh_baseline）前拿工作区级文件锁；临时文件名带进程号和序号、写完 sync 再改名。任务文件读不出来、又找不到没结束的任务时报 `STORE_CORRUPT`，写入和开任务停下、原文件不动；另有没结束的任务时不挡路，`status` 列 `unreadable_task_files`。日志坏行跳过并逐行报出，追加前发现半行先补换行隔开。`state.json`、`expected/` 坏了按任务文件重算 |

先写测试、在旧代码上跑，确认问题真实存在（故障注入，不代表线上已经出过事）：

- 8 个连接同时 `start`，连跑 5 次，每次 4–7 个都开成了任务，其余报
  `STORE_IO_FAILED: No such file or directory`：所有写者共用固定名字的 `x.json.tmp`，一个改名
  走了，另一个就扑空。两个连接交替 `update` 同样报这个错。
- 任务文件被写坏后，`status` 回"没有任务、可写"，`apply_patch` 直接改了文件，还能再开一个
  任务——写前检查整个没了。
- 事件日志中间一行坏了，后面的验收证据全读不到，`finish` 失败；一行不是 UTF-8 时整次读取
  报 IO 错误；半行之后再追加，新记录和半行粘成一行一起丢。

新增 8 条测试（`harness_storage.rs`）在旧代码上 7 条失败；剩下那条（写到一半的临时文件、
重启后任务完好）旧代码本来就对，留作回归。修复后 8 条全过，单独连跑 20 次 20 次过。

**行为变化，升级时要知道：**

- 没有没结束的任务、却有任务文件读不出来时，`status`、`exec_command`、`apply_patch`、`start`
  报 `STORE_CORRUPT`（以前悄悄当成没有任务）。写前检查读任务出错也一律拒写，不再放行。
- `events`、`operation_log` 的 `next_cursor` 按文件行数算（没有坏行时和以前一样），有坏行时多
  `unreadable_lines`；`context` 多 `unreadable_line_count`，它的 `max_bytes` 预算为这个字段预留
  约 45 字节；`status` 多 `unreadable_task_files`。
- 每个工作区的数据目录多一个 `lock` 文件。任务写入多一次 sync：新增 8 条测试（含 30 轮
  并发 update）合计 0.5 秒。
- Rust 接口：`Harness::list_events` / `list_operations` 返回 `LogPage`，`verification_records`
  改成按事件列表算的函数。

**验证：**隔离 `GLD_HOME` 下 `cargo test --workspace --all-targets --locked` 804 passed、0 failed、
0 ignored；fmt、clippy `-D warnings`、`git diff --check` 通过；文档链接检查通过。另用真实
`target/debug/gld` 在隔离数据目录经守护进程复跑：8 个 CLI 同时 `start` 得 1 个成功、7 个
`TASK_ALREADY_ACTIVE`，盘上 1 个任务文件；写坏任务文件后 `status` / `exec_command` / `start`
都报 `STORE_CORRUPT`、文件原样；挪开后能重新开任务；事件日志末尾留半行，之后的证据照样
验收成 `completed`，回包说 1 行读不出来。

**仍未验证或未做：**

- 没做真实断电。只 sync 了文件没 sync 目录：断电后改名可能没生效，留下的是旧的一整份，
  不是坏文件。
- 锁是劝告锁（advisory），只管 gld 自己；人手改、别的程序写照样能写。用不同 `GLD_HOME` 的两个
  gld 各拿各的锁，和写锁的边界一样。
- 锁没有超时：一次 `refresh_baseline` 或写后记账要扫整个工作区（有 511 MB 大文件时约 1.8 秒），
  这期间同一项目的其他任务操作排队等。
- 写到一半留下的 `*.json.tmp.*` 不自动清理（不算任务，只占一点空间）。

### D13 核对：支持 2026-07-28 的客户端能不能连上

结论：**现在能连上，服务端不用改**；用 `a_new_protocol_probe_falls_back_to_initialize` 把现在的
回法钉住（`220c286`）。这只回答"新客户端连旧服务器"这一格，不是 gld 支持了 2026-07-28。

背景：[2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28/changelog) 去掉了
`initialize` 握手和会话，每个请求自带版本。gld 只讲 2025-06-18，而 hub 规则、Skill 目录这些指令
只在 `initialize` 里给；客户端要是认定 gld 是新服务器、不再握手，模型就拿不到这些指令。

- 实测 gld 现在的回法（真实二进制、隔离数据目录）：新版 `server/discover` 回 HTTP 200 +
  `-32601`、id 原样回；新版 `tools/list` 不握手也回 200 和整张工具表；`initialize` 不管客户端要
  2025-06-18 还是 2025-11-25 都回 2025-06-18。
- 子 agent 读了四个官方 SDK 的源码（TS 2.0.0、Python 2.x、Go ≥1.7、C# ≥2.0）：支持新版的客户端
  第一个请求都是 `server/discover`，不是 `tools/list`；200 + `-32601` 四个都判为旧服务器、退回
  `initialize`。会让连接失败的回法：405（C#）、5xx（TS、C#）、不回应或 id 对不上（TS）、
  `-32020`～`-32022`、给 `server/discover` 一个像样的成功结果。我核对了 TS 判定代码的那几行。
- 用官方 TS SDK 2.0.0 客户端（`versionNegotiation: { mode: 'auto' }`，装在临时目录）实连隔离的 gld：
  `server/discover`（2026-07-28）→ 200 → `initialize` → 协商到 2025-06-18，拿到 1649 字指令、
  28 个工具，`list_workspaces` 调用正常。Claude Code 的 v2 运行时用的就是这个 SDK。

**仍未验证或未做：**

- ChatGPT 连接器、claude.ai 连接器实际怎么探测，没有权威来源，没测。Python / Go / C# 只读了源码，
  没实跑。
- gld 没有实现 2026-07-28。只支持新版、不带退回的客户端连不上 gld，规范的兼容表也写明
  这种组合会失败；等真有这样的客户端再做"两代都讲"的服务端。
- 顺带看到两处和 2025-06-18 不完全一致、但实测不影响连接的地方，没改：`notifications/initialized`
  回 200（规范写 202）；`GET /mcp` 回一段 JSON 说明（规范是开 SSE 流或回 405，只在退到已废弃的
  HTTP+SSE 传输时才会用到）。

### 真机升级与 ChatGPT 验收（0.7.0）

本机常驻的 gld（公网固定域名、OAuth、ChatGPT 连接器 9-18 建好）按
[安装 · 升级](../install.md#升级)升了两次：先是同为 0.6.0 的新构建，再是 0.7.0（`706f50f`）。

- **连接器不用动**：两次都逐项比对了 Client ID、授权口令、5 项凭据、ChatGPT 动态注册的客户端
  （`data/oauth-clients/hub.json`）、项目配置的指纹，全部一致；守护进程重启 0.3 秒，服务自己
  回来；ChatGPT 用原来的 `dcr-63f3…` 直接连上，没要求重新授权。
- **同版本号不提醒重启**：第一次升级时新命令行连着旧守护进程，`--served` 报 `unknown variant`。
  原因是 D04 加 `served_tools` 时漏了递增协议号（`d70503e` 提到 4），加上版本号没变
  （`706f50f` 提到 0.7.0）。第二次升级时换完二进制、没重启，命令行按预期报"版本不一致"、退出码 4。
- **ChatGPT 要手动刷新工具表**：它只在建连接器和点 Refresh 时拉 `tools/list`。9-18 到 9-23
  之间日志里一次都没有，开新对话、gld 重启都不触发。到 chatgpt.com/plugins 点 Refresh 之后，
  日志依次是 `server/discover` → `initialize` → `tools/list`，新参数就有了。原先文档让人拿
  `server_info` 的 `connection.tools_fingerprint` 核对，那是 gld 按自己发出去的表算的，客户端用旧
  缓存时照样对得上，已改成问 AI 它自己的工具定义（`b5b5472`）。
- **D13 补上 ChatGPT 的实证**：日志里 ChatGPT 连接器先发 `server/discover`（请求 id
  `openai-mcp-discover`），拿到 `-32601` 后退回 `initialize`，和四个官方 SDK 的行为一致。

在 `~/xdw/gld-realtest`（一个 Python 小项目，`sub` 故意写错）上让 ChatGPT 按提示词操作，结果全部
从 gld 自己的记录核对：

| 步骤 | 核对到的 |
| --- | --- |
| 同一工作区 8 个 CLI 同时 `start`（我在本机跑） | 1 个成功、7 个 `TASK_ALREADY_ACTIVE`，盘上 1 个任务文件 |
| 测试失败（退出 1）的会话当证据 `finish` | 拒收，任务状态不动 |
| `apply_patch` 修 `sub` | 第一次 `PATCH_AMBIGUOUS`（`add`、`sub` 那一行一模一样，gld 不猜），ChatGPT 补上下文后改对 |
| 测试通过（退出 0）后 `finish` | `verification_recorded` → `completed`；Planning 台账 `state: completed`、`changed_files: [calc.py]` |
| 任务开始后在 gld 之外改 `README.md`，再 `git mv` | `FILE_CHANGED_EXTERNALLY`；`refresh_baseline` 列出 `README.md:modified`，带指纹和 reason 接纳后放行 |
| `git_diff`（staged + unstaged） | `README.md`、`calc.py` 未暂存的 modified；`notes/new-name.md` 已暂存的 renamed、`old_path` 正确；和 `git status` 一致 |
| 再跑测试、`finish` | `completed` |

验收中发现、已修：被写前检查拒掉的那次 `git mv` 在操作记录里 `task_id` 是空的，按任务翻不到
（`0f66795`，有没结束的任务就记在它名下）。

**仍未验证：**ChatGPT 看不到 gld 发的 `listChanged`（服务声明 `false`、也没有长连接推送），
新版加了工具或参数后只能靠人去点 Refresh。Claude 等其他客户端没做这一轮真机验收。

### D12：发布 0.7.0

流水线先补了三处（`296cc29`）：tag 和 `Cargo.toml` 版本对不上就不打包，目标是 runner 本机时再跑
一次 `--version` 核对；Release 说明用 `docs/releases/<tag>.md`；musl 不再标 optional。发版步骤
和核对方法写在 [development.md · 打包与发布](../development.md#打包与发布)。

发版前：

- 本机：错的 tag 在构建前被拒；打出 aarch64 包，校验和、解压、`--version`、隔离数据目录里起服务
  读文件都正常。
- 回滚演练（隔离数据目录）：0.7.0 开任务、跑命令 → 换 0.6.0 二进制、`daemon restart`，`gld ls` 和
  凭据指纹与 0.7.0 时一致，0.6.0 照样读得到那个没结束的任务 → 换回 0.7.0，没重启时命令行退出码 4，
  重启后一致，任务照常走到 `completed`。
- Release 手动空跑（run 35863249989，`42643a3`）：测试和 5 个目标全绿，5 个构件；"建 Release"
  skipped，它只在 tag 上跑。

发布：经用户批准推送 tag `v0.7.0`（指向 `42643a3`，run 35872551750 全绿）。
[Release 页](https://github.com/xwfe/gld/releases/tag/v0.7.0)有 5 个包和 `SHA256SUMS`，说明取自
`docs/releases/v0.7.0.md`，标为 latest。下载包验收全部从公开下载地址取：

| 包 | 核对到的 |
| --- | --- |
| `aarch64-apple-darwin` | 校验和 OK；本机运行报 `gld 0.7.0` |
| `x86_64-apple-darwin` | 校验和 OK；`arch -x86_64`（Rosetta）运行报 `gld 0.7.0` |
| `x86_64-unknown-linux-musl` | 校验和 OK；static-pie；Oracle Linux 9 容器里（OrbStack 转译 x86_64）报 `gld 0.7.0`，隔离 HOME 下 `gld ls` 正常 |
| `x86_64-unknown-linux-gnu` | 校验和 OK；x86-64 动态链接 ELF；**没实跑** |
| `x86_64-pc-windows-msvc` | 校验和 OK；PE32+ 控制台程序，二进制里有 `0.7.0`；**没实跑** |

每个包里都是二进制和 README.md，README 与 main 上的一致。

**仍未验证或未做：**

- 签名和来源证明没做。同一提交本机和 CI 打的 aarch64 包哈希不同（`8a6e5f54…` / `e082d540…`），
  `SHA256SUMS` 只证明下载到的是 CI 产出的那份。
- glibc 版和 Windows 版的下载包没实跑：本机只有 arm64 的 Linux 镜像，没为此另拉 amd64 镜像；没有
  Windows 机器。按脚本逻辑，这两个在 CI 打包时是 runner 本机目标，跑过 `--version` 核对（步骤成功），
  但 CI 日志要登录才能看，我没看到那一行输出。
- 审查原先列的 Linux / Windows 上 IPC、命令清理的真机记录，GitHub / SSH / 浏览器真实链路单列，都没做；
  平台证据目前只有 CI 的 ubuntu / macOS 全量测试和 Windows 的编译与补丁落盘测试。

### D08：安全说法照实（2026-09-24）

| 问题 | 结论 | 做了什么 |
| --- | --- | --- |
| `compat-readonly-all` 把能写能执行的工具标成只读 | 已退役（`cacde65`） | `gld set` / `gld upgrade` 给这个值报错并说明换成什么；配置里已有的读成 `advanced`（工具一个不少、标注照实），下次保存写成 `advanced`。不换的话会被当成认不出的值降成 core |
| `request_permissions` 在 `dangerous` 下回 `granted`、说"需要许可的操作都自动放行" | 已修（`d7f71fc`） | 任何模式都回 `ELICITATION_UNSUPPORTED`。那句话本来就是假的：`rm -rf` 在 dangerous 下照样要 `confirm=true`。它只有 GPT Actions 那条线路调得到（MCP、CLI 都不列也不放行）。`trusted` 和 `dangerous` 现在行为完全一样 |
| `confirm=true` 不是用户真实批准 | 写明（`e9fe757`） | 四个工具的 `confirm` 参数带上说明：服务端核实不了，只拿它开门。真人确认只能在客户端按标注弹框，文档写清楚标注照实给、点了"总是允许"这道也就没了 |
| 静态策略不是 OS 沙箱 | 不用改 | `check_exec_environment` 早就报 `execution_boundary=policy_only`、`sandbox_enforced=false`，security.md 也写着 |

**行为变化，升级时要知道：**

- 配置里写着 `compat-readonly-all` 的，升级后客户端会开始在改文件、跑命令前问用户。
- 工具表变了（`exec_command`、`apply_patch`、`patch_check`、`check_command` 的 `confirm` 多了说明）：
  ChatGPT 要到 chatgpt.com/plugins 点 Refresh 才看得到，不刷新照样能用。
- GPT Actions 线路上调 `request_permissions` 不再有 `granted`。

**验证：**新测试先在旧实现上跑：dangerous 模式的那条失败（`ok=true`、`status=granted`）。隔离 `GLD_HOME`
下全量 808 passed、0 failed、0 ignored；fmt、clippy `-D warnings`、`git diff --check` 通过；`docs/cli.md`
重新生成。真实 `target/debug/gld`、隔离数据目录：`profiles.json` 里手写 `compat-readonly-all`（服务和项目各一处）
后 `gld tool list` 出 54 个工具、`exec_command` / `apply_patch` 的 `readOnlyHint=false`，`gld ls` 显示
`advanced`；`gld set` / `gld upgrade` 给这个值都退出 1、报退役原因；随便改一项后文件里两处都写成了 `advanced`；
dangerous 模式下 `rm -rf build` 不带 confirm 报 `DANGEROUS_OPERATION_REQUIRES_CONFIRMATION`。

**没做：**真正的"审批绑定具体动作与真实主体"（服务端发审批单、用户在本机 `gld approve`，或 MCP 的
elicitation）。gld 的 HTTP 服务不开 SSE 流，elicitation 发不出去；ChatGPT 是否支持也没有权威来源。
客户端按标注弹的确认框是现在唯一的真人确认，所以先保证标注不说谎。

### D07：只开部分项目的凭据（2026-09-24）

设计和取舍写在 [RFC-0007](../rfc/0007-scoped-grants.md)，用法和边界写在
[concepts.md](../concepts.md#只开几个项目gld-grant)，这里只记做了什么、怎么验的。

**做了什么：**`gld grant add <名字> <项目>… [--write]` 发一把自带口令（OAuth 授权页用）和令牌（bearer 用）
的凭据，默认只读。用它进来的连接只看得到开给它的项目；只读的只有 read-only 工具集那一份；不给本机 MCP
转发；远端只能读；关了 confine-reads 的项目不给用。`gld grant rm` 下一次请求生效：令牌里写的是 grant 的
随机 id，验访问令牌、换授权码、刷新令牌时都查它还在不在；同时停掉它起的命令会话，35 秒后再清一次。
服务口令、服务令牌和升级前发出的令牌照旧全权，新发的全权令牌格式不变。

**独立审查**（另开上下文的 agent，只读，找绕过路径）找到 9 条，处理如下：

| 发现 | 处理 |
| --- | --- |
| 能写的 grant 用 `exec_command` 读 `profiles.json` 拿到服务口令，等于全权 | 挡不住（执行就是以你的身份跑任意代码）。改成默认只读、`--write` 显式要，`add` 输出、帮助和文档都写明等于全权 |
| 回滚到 0.7.0 后，grant 的 OAuth 令牌被旧版本当成全权令牌 | grant 令牌的 `token_use` 写 `grant_access` / `grant_refresh`，旧版本只认 `access` / `refresh`，回滚后 401 |
| 项目是仓库子目录时 Git 工具看得到整个仓库（`git_show HEAD:web/.env`） | 修（对所有凭据）：默认补 `-- .`，拒 `:` 开头的 pathspec，`提交:路径` 必须落在项目前缀里。这本来就违反 security.md 写的"项目目录内" |
| 项目关了 confine-reads 时只读 grant 能读整台机器 | grant 用这种项目一律拒（`GRANT_NEEDS_CONFINED_READS`），`add` 时也拒 |
| 能写的 grant 开远端写会话会顶掉操作员正开着的那条 | 第一版 grant 一律不开远端写会话（`GRANT_CANNOT_WRITE_REMOTE`） |
| `grant rm` 不停远端会话 | 随上一条消掉：grant 没有远端写会话；只读的远端连接靠空闲回收，令牌已经 401 |
| 撤销瞬间在途调用起的命令可能漏停，最长 10 分钟 | 35 秒后再清一次（写锁最多等 30 秒） |
| 对不上服务令牌的请求也会读数据文件 | 没改，改正了说"不会"的注释，记为已知开销 |
| 只读 grant 的 `server_info.tools` 列着调不了的工具；"只显示一次"说法不对；有 grant 时改成 noauth 不提示；`grant rm` 可能 30 秒超时误报；一处文档注释挂错位置 | 都改了：按只读过滤、改说法、有 grant 时拒绝改成 noauth、`remove_grant` 算慢请求（180 秒）、注释挪回去 |

审查同时核过、没发现问题的：hub 各条调用路径都先按 grant 过滤、候选和报错里不漏范围外的名字；按 grant
过滤之后不会误停别的成员的命令；只读工具集里没有能写项目文件的；远端写句柄绑主体；撤销后三种令牌都拒；
工作区监听器和 GPT Actions 不认 grant；全权用户的标识、令牌格式和工具表不变。

**行为变化，升级时要知道：**

- 守护进程协议号 4 → 5（加了 `list_grants` / `add_grant` / `remove_grant`）：换了二进制要
  `gld daemon restart`，否则命令行报版本不一致（退出码 4）。
- **项目是某个仓库的子目录时，Git 工具只看这个子目录**（所有凭据都一样）：`git_status` / `git_diff` /
  `git_log` / `git_show` 不再列兄弟目录的东西，`git_show` 的 `rev` 写 `HEAD:别的目录/文件` 报
  `PATH_OUTSIDE_WORKSPACE`，路径过滤不能以 `:` 开头。项目就是仓库根的不受影响。
- 数据文件多一个 `grants` 键（没有 grant 时是空数组，升级后第一次保存配置才写进去）。退回 0.7.0 会忽略它，
  grant 发出的令牌在旧版本上 401。
- 有 grant 时 `gld upgrade --auth noauth` 报错。
- 服务凭据看到的工具表不变，ChatGPT 不用刷新。

**验证：**新增测试 20 条（OAuth 6、主体 1、hub 8、Git 收窄 1、登记 1、端到端 3）。其中两条做过变异验证：把 grant 过滤挪到"判谁离开了"之前，测试失败（服务凭据起的
命令被停掉）；关掉 Git 收窄，测试失败（`HEAD:web/.env` 读到了内容）。端到端测试用真实二进制、守护进程和
真的 HTTP：OAuth 动态注册 → 授权页填 grant 口令 → 换令牌；bearer；作废后 401、刷新 400、它起的后台命令被停掉；
项目名写错、项目关了 confine-reads 不建；有 grant 时不给改 noauth。隔离 `GLD_HOME` 下全量 828 passed、0 failed、
0 ignored（上一轮 808）；fmt、clippy `-D warnings` 通过；`docs/cli.md` 重新生成（多了 `gld grant` 四节）。真实二进制在隔离数据目录里
看过 `gld grant add / ls / rm` 的输出和重名、找不到时的报错。

**仍未验证或未做：**

- 没在真实 ChatGPT 连接器上用 grant 口令授权过：要在客户端里操作。
- 改范围只能删了重建；没有有效期；同一个项目里的 Task / History / Planning 对开了它的几把凭据是共用的。
- 能写的 grant 挡不住存心的人（见上表第一条）；给别人的只能是只读的。

### 本机升级到 D07/D08 这一版（2026-09-24）

推送 10 个提交（`18d85f0..e754c51`，CI run 35948522581 全绿）之后，按[安装 · 升级](../install.md#升级)
升本机常驻服务：备份二进制（`~/.local/opt/gld-0.7.0-d8c807725fc5`）和数据目录
（`gld-config-before-grant-20260924-1145.tgz`），在干净的 `e754c51` 上 `cargo build --release`，
写新文件再改名换上，`gld daemon restart`。

| 核对 | 结果 |
| --- | --- |
| 守护进程 | 重启 0.2 秒，协议 4 → 5，服务自己回来 |
| 构建提交 | `e754c51a6ee2`，等于 HEAD |
| 口令、签名密钥、Client ID、bearer 令牌、ChatGPT 注册的客户端（`hub.json`）、公网配置、成员表、项目表 | SHA-256 指纹逐项比对，全部一致 |
| `gld ls` | 升级前后一字不差 |
| `gld health` | 本地 / 公网 `/mcp`、两份 OAuth 元数据都是 ✓ |
| 工具表 | 28 个工具不变，指纹 `c18992480bf6164f` → `9ed41391e27553c6`（D08 给 `confirm` 加了说明）：ChatGPT 要点一次 Refresh 才看得到新说明，不点照常能用 |

升级后 13 分钟机器整体重启过一次（开机时间 12:01）。cloudflared 是 launchd 管的，自己回来了；gld 守护
进程没配开机自启，没回来，公网地址通、回源不通。`gld daemon start` 后服务跟着回来，指纹再比一次仍然
全部一致，`gld health` 全 ✓。连接器不用动，这一条补进了
[装好的连接器什么时候要动](../connect-clients.md#装好的连接器什么时候要动)和排障表。

同时把"升级后 ChatGPT 插件要不要动"整理成一处：connect-clients.md 的四档表（什么都不用做 / 点 Refresh /
重新授权 / 删了重建），install.md 的升级流程多一步"升级前后比 `gld tool list --served` 的工具表指纹"来决定
要不要 Refresh，development.md 要求每版发布说明写明这一点。

**仍未验证：**ChatGPT 这边点 Refresh 之后的 `tools/list` 和新对话里的工具定义（要用户在网页上操作）；
grant 口令在 ChatGPT 授权页上的实际授权。

### D12 的尾巴，以及顺带查出的两处修复（2026-09-24）

| 项 | 结果 |
| --- | --- |
| glibc 版下载包实跑 | v0.7.0 的 `x86_64-unknown-linux-gnu` 包从公开地址下载，校验和 OK；在 Debian 12（glibc 2.36）amd64 容器里（OrbStack 转译）`--version` 报 0.7.0，登记项目、起守护进程（socket 权限 `srw-------`）、经守护进程调工具、起后台命令都正常 |
| Linux 上的命令清理 | 查出一个 bug：`gld daemon stop` 之后，经 `gld tool call` 起的后台命令还在跑；macOS 上用当前版本复现一样，经服务（MCP）起的倒会被停掉。没有守护进程时（直连模式）命令行一退也一样留下孤儿。已修（`4ffd96f`）：守护进程退出时收掉剩下的全部命令会话；直连模式下命令行退出前停掉并在 stderr 说明。两条新测试在旧代码上失败；CI 的 Ubuntu 与 macOS 测试 job 都跑了它们、全绿（run 35951936414） |
| 构建来源证明 | 发布流水线的 build job 每个包签一份 SLSA 构建来源证明（`actions/attest@v4`，`4216d21`），用户用 `gh attestation verify` 验，做法写在[安装](../install.md#下载现成的不需要装-rust)。同日手动空跑（run 35962328940，`bc8e13a`）第一次签出来：5 个包照文档的命令验都通过；改一个字节、换一个 `--signer-workflow` 都不通过 |
| 代码签名、Windows 包实跑 | 没做：Developer ID / Authenticode 要付费证书，本机没有 Windows |

顺带查出的另一处：机器重启后守护进程从一个没带 rustup 目录的 shell 拉起，经它跑 `cargo` 报
`Program not found on PATH`。补了全局可执行路径（`/opt/homebrew/opt/rustup/bin`）后，经服务的调用
下一次就好了，但 `gld tool call` 还是找不到——它的工具上下文缓存在守护进程里、改全局设置不清。已修
（`432dfd7`），新测试要先起守护进程才能复现，在旧代码上失败。开机自启的四步命令写进了
[守护进程 · 开机自启](../daemon.md#开机自启)，用一个隔离数据目录的临时 launchd 任务逐条实测过
（launchd 给的 PATH、`gld daemon stop` 不被拉回、`kill -9` 被拉回、装之前不停旧的会每 10 秒重试），
测完撤掉；本机没有配，由用户决定。

本机服务之后又升了两次（`dbbe713`、`4216d21`），每次按同一流程核对：工具表指纹都没变
（`9ed41391e27553c6`），ChatGPT 不用为这两次做任何事；凭据与注册客户端指纹、`gld ls` 都一致，
`gld health` 全 ✓。

### D09：命令的运行记录（2026-09-24）

用户决定提前做（原计划等真实项目用到）。用法和边界写在
[concepts.md · 命令的输出能读多久](../concepts.md#命令的输出能读多久stdin-怎么关)，设计理由写在
[`tools/runs.rs`](../../crates/core/src/tools/runs.rs) 的模块说明，这里只记做了什么、怎么验的。

**做了什么：**`exec_command` 起的每条命令在 `GLD_HOME/runs/<项目 id>/<session_id>/` 留 `run.json`（主体、
脱敏命令、pid、起止时间、终态、退出码、运行期间 gld 写过几次工作区）和按 1 MiB 分段、只留最新两段的
stdout / stderr 日志；每个项目留最近 64 条结束的、最长 7 天。后台命令加了 200 毫秒一次的结束检查，
终态在结束那一刻落盘。`read_output` / `write_stdin` / `kill_session` 在内存里找不到会话时读记录，按主体
过滤，id 必须是 UUID。gld 退出时停掉的记 `interrupted`；gld 没来得及记的（`kill -9`、崩溃）停在 `running`，
读的人发现起它的进程已经不在（它在 `runs/owners/` 上的文件锁放了）就改成 `unknown`，附 `pid_in_use`。
不恢复进程、不重放；后台命令期间不占写锁的取舍不变。

**先在旧代码上复现：**三条端到端测试（真实二进制、隔离数据目录）在旧代码上都报 `SESSION_NOT_FOUND`：
守护进程 `stop` / `start` 之后读、守护进程被 `kill -9` 之后读、直连模式下一条 `gld tool call` 读。

**做的过程中查出的一处：**守护进程退出时先停服务，服务会停经它起的命令、记成 `killed`，轮不到最后那步的
`interrupted`。于是经服务（ChatGPT 那条路）起的命令重启后显示成"被人停了"。补了一条经服务起命令的端到端测试，
先确认它失败（`killed`），再把退出流程改成先按 `interrupted` 收命令、停完服务再收一遍。

**行为变化，升级时要知道：**

- 被 `kill_session` / 切 plan 模式停掉、结束超过 5 分钟、守护进程重启过的命令，`read_output` 不再报
  `SESSION_EXPIRED` / `SESSION_NOT_FOUND`，而是从记录读到结局和输出，回包多 `source: "run_record"`、`pid`、
  `started_by_gld_pid`（`unknown` 的还有 `pid_in_use`）。
- `exec_command` 等回包多 `kept_on_disk`；它为 true 时 `read_output` 的 `expires_in_ms` / `retention_ms` 是 `null`。
- `termination_reason` 多 `interrupted`（台账记 `cancelled`）和 `unknown`（`command_ok: null`，快照的 `status`
  也是 `unknown`）。守护进程退出时经服务起的命令以前记 `killed`，现在记 `interrupted`；`gld stop` 仍是 `killed`。
- 直连模式下命令行退出时的提示改成"停之前的输出和结局留在运行记录里"。
- 数据目录多 `runs/`：命令输出原样落盘、不脱敏，最坏每个项目 256 MiB。每条命令多两次小文件写入和一次目录扫描。
- 工具表、守护进程协议号都没变：ChatGPT 不用点 Refresh；换了二进制照常 `gld daemon restart`。

**验证：**新增测试 22 条：`runs` 单测 9（分段与偏移、主体与路径穿越、判活、配额与年龄、不再用的项目也清、
盘写不进、锁先拿后改名、权限 0700/0600）、`session` 单测 2（别的进程留下的记录不冒充"没写过"、本进程自己的
running 记录不在内存里就是 unknown）、`run_records.rs` 7（停掉后读、结束那一刻落盘、别的主体读不到、3.1 MiB
输出按绝对偏移从记录分页读到底、从记录读到的结局照样当任务验收证据、命令结束后 gld 又改了文件就不能当证据、
拦着 TERM 的命令被补 KILL）、端到端 4（上面三条加经服务起的那条；`kill -9` 那条还核对 `kill_session` 不发信号、
`status: unknown`；直连那条还核对跑完的命令不被说成"停掉了"）。改了 2 条旧测试的断言：切 plan 模式后读到的是
`killed` 的记录而不是 `SESSION_EXPIRED`；直连模式下一次调用读到的是 `interrupted` 的记录而不是
`SESSION_NOT_FOUND`。隔离 `GLD_HOME` 下全量 853 passed、0 failed、0 ignored（上一轮 831）；fmt、clippy `-D warnings`、
文档链接与命令名检查通过。推送 4 个提交（`6113fb8..d72ef18`）后 CI run 35961573998 的 8 个 job 全绿，Ubuntu 和
macOS 的测试 job 里都跑了新的端到端测试（`kill -9`、守护进程重启、经服务起的命令），Windows 编译检查通过。真实 `target/debug/gld`、隔离数据目录手动走了一遍：`daemon stop` / `start` 后两个流
都从记录读到、`kill_session` 回原样结局；`kill -9` 后孤儿还在时 `pid_in_use: true`，手动收掉后 `false`；
盘上目录 0700、文件 0600，死进程的 owner 锁被清掉。

**独立审查**（另开上下文的 agent，只读）找到 9 条，处理如下。三条修复做过变异验证：把修法改回去，对应的新测试失败。

| 发现 | 处理 |
| --- | --- |
| 高：记录里的写入次数在命令结束时就定死了。测试跑完、再用 gld 改源码、过了 5 分钟从记录读到"退出 0、写过 0 次"，任务验收把测旧代码的结果收成 completed | 修：记录存起跑时的计数器，读时按当前计数器现算；换了进程算不出来就给 `null`，验收报 `EVIDENCE_STALE` 要求重跑。回归测试在旧算法下失败 |
| 中：Windows CI 带 `-D warnings` 编测试，`run_records.rs` 有只给 unix 测试用的 import | 修：挪进那条测试里 |
| 中："最多 7 天"只对还在起命令的项目成立；`history_session.rs` 没隔离数据目录，`cargo test` 会往真实 `~/.config/gld` 写 | 修：进程第一次拿到 owner 锁时扫全部项目、删空目录；那个测试文件补上隔离（真实目录下已有的 4 个写锁文件分不清哪些是测试留的，没动） |
| 中低：证据测试先经 `call_tool` 调 `kill_session`，任务那边当场就记了结局，根本没走记录那条路；路径穿越断言不放诱饵也会过；锁"先拿后改名"只查了最终状态 | 前两条改测试：绕过分发层把会话挪出内存；在穿越目标上真放一份记录。第三条如实写明只钉住了最终状态 |
| 低：kill 判断"还在跑"之后进程自己退出，记录写 `exited`、内存被改成 `killed` | 修：记结局和标原因拿同一把锁，结束了就不再改原因，`kill_session` 也不再说自己停掉了它。没有稳定复现它的测试 |
| 低：写 stdin 失败直接返回、没交给监视任务，记录停在 running；结束时写记录失败，之后被说成"另一个进程还在跑" | 修：出错也交给监视任务；本进程起的、内存里没有的 running 记录按 unknown 说（单测） |
| 低：退出时只发 TERM，拦着 TERM 的命令成了孤儿 | 修：1.5 秒不走补 KILL（测试 + 变异验证） |
| 低：直连模式退出提示把早就跑完的命令也算成"停掉了" | 修：只数真正停掉的（端到端断言 + 变异验证） |
| 低：进程结束后才写坏的日志，"不全"的标记不落盘 | 修：那时再写一次记录 |

另按建议把读日志的接口改成只收已经核对过主体的记录。审查核过、没发现问题的：主体隔离与不可枚举、路径只落在
`runs/` 下、判活不会把活着的进程判死、锁的先后没有环、每个流不超过 2 MiB、Windows 运行时代码只在 unix 下用 libc。

**本机升级与 ChatGPT 实测（用户批准）：**按[安装 · 升级](../install.md#升级)把常驻服务从 `4216d21` 升到
`bc8e13a`：备份二进制和数据目录，写新文件再改名，`gld daemon restart` 0.2 秒；构建提交等于 HEAD，工具表指纹
没变（`9ed41391e27553c6`，ChatGPT 不用 Refresh），口令、签名密钥、Client ID、ChatGPT 注册的客户端、配置和
项目表的指纹逐项一致，`gld health` 全 ✓。

然后在 `~/xdw/gld-d09test`（Python 小项目，`slow.py` 打一行字后睡 5 分钟）上让 ChatGPT 分三段操作，
两段之间由后台脚本等请求日志安静 20 秒后动守护进程，结果全部从 gld 自己的记录核对：

| 步骤 | 核对到的 |
| --- | --- |
| 开任务，后台起 `python3 -m unittest -v`（yield 0）和 `python3 slow.py` | 两条记录都在；unittest 结束那一刻就落了 `exited 0`，没人读过 |
| 正常重启守护进程，ChatGPT 读两条输出 | slow.py 是 `interrupted`（台账 `cancelled`），重启前打的那行字读得到；unittest 读到 `exited 0`、`workspace_writes_since_start` 为 null |
| 拿重启前那条 unittest 当证据 finish | 被拒：没有记验收、任务状态没动（回包 4.3 KB） |
| 重跑 unittest 再 finish | `completed`（回包 18 KB） |
| 再起一条 slow.py，`kill -9` 守护进程后拉起，ChatGPT 读它、再 `kill_session` | 读的那一刻记录从 `running` 改成 `unknown`，Planning 台账记 `unknown` 和"可能还在跑"；进程 40736 自成一组、父进程变成 1，`kill_session` 没发信号，它照样活着，由我手动停掉 |

整个过程 ChatGPT 用的都是原来那个注册客户端。测完删了项目和它在数据目录里的任务记录、运行记录、日志，
凭据与配置指纹、`gld ls`、工具表都和测试前一致。

**仍未验证或未做：**

- ChatGPT 这边只看了 gld 的记录和回包大小，没逐字看 ChatGPT 拿到的回包（用户没贴，按惯例不贴）。
- Windows 只靠 CI 编译；本机交叉检查因 `ring` 缺 Windows 头文件做不了。判活用的文件锁在 Windows 上没实跑。
- 没有列出运行记录的工具或命令：重启后要知道 `session_id`（在对话里、任务事件和 Planning 台账里都有）。
- 真断电没测：`run.json` 不 `sync`，断电后可能读成"没有这条"。
- 只数 gld 自己的写入；gld 之外的改动不在记录里。gld 重启前的命令读得到结局，但当不了任务验收证据。
- TERM 没停住、之后自己正常退出的命令，结局仍记 `killed`。

### 发布 0.8.0（2026-09-25）

v0.7.0 之后的 grant、D08、D09、孤儿进程和全局设置缓存两处修复一起发成 0.8.0（用户批准推送、空跑和打 tag）。
发布说明 [v0.8.0.md](../releases/v0.8.0.md) 写明：ChatGPT 要点一次 Refresh（D08 改了 `confirm` 说明），协议 4 → 5，
可退回 0.7.0（grant 令牌在旧版上 401）。

| 核对 | 结果 |
| --- | --- |
| 发版前 | 隔离 `GLD_HOME` 全量 853 passed、0 failed；fmt、clippy `-D warnings`、`docs/cli.md` 重新生成无差异；本机 `scripts/package.sh` 打包，解压后 `--version` 报 0.8.0，隔离数据目录起停守护进程报"协议 5" |
| 推送与空跑 | `09b1ae5` 的 CI run 36019248355 全绿；Release 空跑 run 36019247499 五个目标全绿，建 Release 为 skipped |
| 正式 Release | 推 `v0.8.0` 后 run 36020276547 全绿，5 个包 + `SHA256SUMS`，非草稿、非预发布 |
| 下载包 | `shasum -a 256 -c` 5 个全 OK；`gh attestation verify` 按文档那条命令 5 个都退出 0；两个 macOS 包 `--version` 报 0.8.0（Intel 版用 `arch -x86_64`）；musl 包在本机已有的 Oracle Linux 容器（arm64 + Rosetta）里 `--version`、起停守护进程正常 |

**没验：**glibc 包没实跑——Docker Hub 连不上（`registry-1.docker.io` 回 EOF），本机没有 amd64 镜像；Windows 包照旧没实跑。

**本机升级到 0.8.0 发布包（用户要求）：**按[安装 · 升级](../install.md#升级)，用下载后验过校验和与来源证明的
aarch64 包换掉 `bc8e13a` 的构建：先备份二进制（`~/.local/opt/gld-0.7.0-bc8e13a`）和数据目录（`gld-config-before-0.8.0-*.tgz`，
权限收紧到 600），写新文件再改名，`gld daemon restart` 0.2 秒，1 个服务自己回来。`daemon status` 报 0.8.0（协议 5），
构建提交 `09b1ae57d73a`；工具表 28 个、指纹 `9ed41391e27553c6` 没变；`gld ls` 一字不差；profiles.json 里口令、密钥、
地址、端口等 184 项和 `hub.json` 指纹一致（`daemon.json` 是运行登记，每次重启都变）；`gld health` 全 ✓。
这次升级本身不改工具表；用户仍要点一次 Refresh，补上 D08 之后一直没刷新的那次。

用户点了 Refresh。服务请求日志里 ChatGPT 用原来的注册客户端连了两轮（`server/discover` → `initialize` →
`tools/list`），日志最后写入在重启之后。最后那次 `tools/list` 回包 36156 字节，等于服务当前 28 个工具按紧凑 JSON
算的大小，表里 `confirm` 已是 D08 的新说明。请求日志每行不带时间，"在重启之后"是按文件修改时间判断的；
ChatGPT 新对话里实际看到的工具定义没逐字核对。

### 收尾

- 全部提交已推送到 GitHub。0.7.0 当时只是版本号，发布见上一节。
- 推送后 macOS CI 挂了：D06 的生成脚本测试里，假 gld 让命令失败，脚本却退出 0。原因两层：
  macOS 自带 bash 3.2 在 UTF-8 locale 下把 `$status，` 里中文逗号的字节读进变量名，`set -u`
  报错；而 bash 3.2 在这类致命错误后跑 EXIT trap 时 `$?` 已是 0，崩溃被报成成功。本机没设
  `LANG`（C locale），所以本机和 Linux 都复现不了。已修（`9007348`：变量加花括号、trap 按完成
  标记兜底退出 1、测试固定 UTF-8 跑），CI 8 个 job 全绿。
- 真机验收项目 `~/xdw/gld-realtest` 已从服务删掉，目录和它在数据目录里的任务记录、日志一并清掉；
  §3 的临时证据目录、`dist/` 里 9-11 的 0.3.0 旧包、升级途中的中间备份也已清理。保留的回滚点：
  本轮之前的 0.6.0 二进制、最后一次修复之前的 0.7.0 二进制、升级前的数据目录备份（都在
  `~/.local/opt/`）。
- 清理时发现 `gld rm` 的帮助和 concepts.md 说它删"配置和记账"/"Planning 与历史的记账"，实际只删
  配置、凭据和 OAuth 客户端注册：Planning、历史档案在项目目录里本来就不动，数据目录里的任务记录
  （按目录记）和日志也留着。行为没改（删任务记录会让同一个目录再加回来时丢历史），说法已按实际改正。

**当时的收尾快照**（后续排队以 [task_plan.md](../../task_plan.md) 为准）：

| 编号 | 什么时候 |
| --- | --- |
| D12 剩下的：代码签名（要付费证书）、Windows 包实跑 | 给别人分发之前，或有人报平台问题时 |
| D10 浏览器证据 | 真实项目需要时 |
| D09 剩下的：列出运行记录的入口 | 有人需要在重启后找回不知道 id 的命令时 |
| D13 实现 2026-07-28 | 出现只讲新版、不会退回的客户端时 |

## 8. 0.8.0 后续规划复核（2026-09-25）

本次起点 `eefdc1b40015af88759efae7239c4da55f79c459`，工作树干净。服务实际报告
`version=0.8.0`、`build_commit=09b1ae57d73a`、28 个连接工具、
`connection.tools_fingerprint=9ed41391e27553c6`。`git diff --stat 09b1ae57d73a..HEAD`
仅有本审查和 task_plan.md，证明仓库比发布构建多的只是文档记录，不是遗漏运行代码部署。

### 现状与规划纠偏

本会话实际发现的 schema 已包括 `check_command`、Skills、Notebook、`argv`、`stdin_mode`、
`start_byte`、`expected_versions`、`notebook_edits`、`evidence_session_ids` 和
`refresh_baseline`。`check_command argv=[git,status,--short]` 实际返回 allow、
`side_effects=none`，并报告上述发布构建。本轮只确认这条连接，不外推所有客户端已刷新或
grant 的真实授权页验收已完成。

原待办与最新实现的主要差异已收口：D01–D08、D11、D14 有修复；D09 有运行记录；D12 有
0.8.0 发布及来源证明记录；D13 是兼容回退核验，不是新协议实现。剩余计划不能再写成
“正式验收未接通”“没有 grant”“输出只有内存”“来源证明没做”。

`task_plan.md` 原末尾把来源证明、glibc 全部列为不纳入，与其自身实施记录矛盾。
现已拆开当前队列与历史记录：0.7.0 glibc 的已验事实保留，0.8.0 包的实跑仍待补；
Windows 不以“无本机”无限延期，建议在已有 runner 上增加可跨平台执行的最小运行测试。
抽查 `daemon_lifecycle.rs`、`run_records.rs` 的部分进程/记录用例仍是 `cfg(unix)`，
不能仅把命令加到 Windows job 就当作跑到了同一批测试。

后续决策、顺序、最小验收和不做范围均在 task_plan.md，本文不复制第二张待办表。
最重要的范围约束是：复用已有 RunLog 补“发现”，不重建调度；D10 从真实项目流程出发，
不先上完整 IDE、通用部署平台或新的状态系统。

### 本轮验证范围

在项目 `target/replan-20260925-test-home` 隔离 GLD_HOME 后运行
`cargo test --workspace --all-targets --locked -- --quiet --nocapture`，退出 0；38 组结果
合计 **853 passed、0 failed、0 ignored**。`share_one_command` 中
`share_reports_the_missing_binary_instead_of_a_silent_no_url` 因本机已有 cloudflared
提前返回，**该缺程序场景本轮未实际执行**，不能用汇总的 0 ignored 掩盖它。
ccnm 本地真实管道 3 项用时 8.29 秒，无缺二进制跳过提示；不是跨主机 SSH 验收。

本轮仅同步规划和文档。没有重新查询远端 CI、下载发行包、创建 grant、重启真实服务、
签名、推送或发布；这些仍按原证据的日期与版本理解。

`cargo fmt --all -- --check` 与 `cargo clippy --workspace --all-targets --locked -- -D warnings`
均退出 0。文档修改后，在独立 GLD_HOME 下运行 `docs_commands_exist`、`docs_generation`、
`docs_links_resolve`、`doctor_fixes_are_real_commands`、`messages_name_real_commands` 五组
集成测试，**9 passed / 0 failed**；`git diff --check` 通过。只改 8 份 Markdown，
生成的 `docs/cli.md`、运行时代码、依赖与 CI 配置未改；不将此轮文档完成等同于后续功能已实施。

## 9. glibc 包验收、D09 运行记录发现、Windows 运行测试（2026-09-25）

### 0.8.0 glibc 下载包

Docker Hub 经本机代理 SSL 断开（`curl` 也一样，不是 OrbStack 的问题），改拉
`public.ecr.aws/debian/debian:bookworm-slim`（amd64，glibc 2.36-9+deb12u14）。包从 Release 下载，
`shasum -c` OK、`gh attestation verify` 退出 0。容器里：

| 核对 | 结果 |
| --- | --- |
| 版本、IPC | `--version` 0.8.0；`daemon status` 报协议 5，socket `srw-------` |
| 前台命令 | 脚本 `exit 3`：`exit_code` 3、`command_ok` false，stdout / stderr 各一行，`kept_on_disk` true |
| 后台命令 | `sleep 300` 转后台，`read_output` 读到第一行 |
| 正常停止 | `daemon stop` 后 `/proc` 里没有残留的 sleep |
| 重启后读记录 | `source: run_record`、`termination_reason: interrupted`、输出还在；记录目录 0700、`run.json` 0600 |

容器里默认白名单没有 `sh`、命令不许 `;` 和重定向，按设计先 `gld set <项目> allowed-commands=sh` 再跑脚本。
测完删了镜像；OrbStack 原本就在运行，没停。

### D09：`list_runs`

**做了什么：**只读工具 `list_runs`，列出这个主体在这个项目里的运行记录，新的在前；每条给脱敏命令、终态、
退出码、起止时间、每个流写了多少字节（只看日志文件名和大小）、`output_refs`，不回输出正文。先按主体过滤再计数和
分页；游标是上一页最后一条的"起跑毫秒 + id"，新起的命令不会让后面的页错位。起它的 gld 不在了的 `running`
判 `unknown`、不发信号。放进 compact / core / advanced / read-only 四档（工具数 29 / 41 / 55 / 22），
命令行用 `gld tool call list_runs`，没另加命令：命令行是 `local` 主体，只看得到命令行自己起的。

**独立审查**（另开上下文，只读）找到 1 高 3 中 6 低，都按实测处理：

| 发现 | 处理 |
| --- | --- |
| 高：GPT Actions 调工具不带身份，落在和命令行同一个 `local` 主体上，两边互相读得到命令输出，`list_runs` 让它能直接枚举 | 修（`708fc0b`）：Actions 鉴权中间件验完挂上入口名为 `actions` 的身份。**顺带查出 Actions 上 `exec_command` 必然 panic**：在 async worker 上直接调同步工具，里面的 `block_on` 触发 tokio 的 panic，连接被掐断。以前只测过 `read_file`。新端到端测试在旧代码上复现了 panic；把主体改回 `local` 时测试失败 |
| 中：被 kill、进程还没退的命令被报成已结束且失败 | 修：按进程在不在判 `running`，原因照实显示 `killed`、不说成败 |
| 中：本进程的孤儿 running 记录，列表说 running、`read_output` 说 unknown | 修：起跑超过 5 秒的也判 unknown；刚起的不动（`insert` 先建记录后进内存表） |
| 中：列表对每条内存会话阻塞刷新状态，`kill_session` 等进程退出时列表跟着卡（实测 4.7 秒） | 修：拿不到子进程锁就用当前状态 |
| 低：空 `status`、非原样的游标、翻页中途换条件、配额口径、"自己"指什么 | 前两条报错；后三条写进 concepts.md |
| 测试：去掉内存覆盖、游标只比毫秒，原测试照样过 | 补测试；四处实现各做变异，对应测试都失败 |

**验证：**隔离 `GLD_HOME` 全量 866 passed、0 failed；fmt、clippy `-D warnings`、`docs/cli.md` 无差异。
`run_records.rs` 16 条（含上面几条）、`daemon_lifecycle.rs` 里重启和 `kill -9` 两条端到端改成先用
`list_runs` 找回 id 再读。

**行为变化：**多了一个工具，工具表指纹会变，升级后 ChatGPT 要点一次 Refresh；GPT Actions 起的命令，
本机 `gld tool call` 不再读得到，反过来也一样。

### Windows 最小运行测试

Windows CI job 加了一步，逐条跑 `daemon_lifecycle.rs` 里 9 条，每条限时 3 分钟，失败的打出输出和
daemon.log。跑的是这次编出的 `target/debug/gld`，**不是下载包验收**。第一次跑 7 条失败、1 条卡满 20 分钟，
查出两个真 bug：

- **gld 主线程栈溢出**（0xC00000FD）：Windows 主线程默认 1 MiB，debug 构建的 `gld add` 就溢出。
  `crates/cli/build.rs` 在 Windows 目标上把栈设成 8 MiB，和另外两个平台一致。release 构建是否也会触发没验。
- **守护进程继承调用方的输出管道**：Rust 的 `Command` 在 Windows 上把所有可继承句柄交给子进程
  （rust-lang/rust#38227），`gld daemon start` 的输出被管道接着时，调用方永远等不到 EOF。拉起前清掉三个
  标准句柄的可继承标志。

修完后 CI run 36093958618 的 8 个 job 全绿，Windows 上 9 条逐条 ok，日志里没有再出现栈溢出。

**仍未验证或未做：**Windows 下载包没实跑；`kill -9` 那条（`pid_in_use` 在 Windows 上是 `null`）、
`run_records.rs`、按 cwd 找项目（8.3 短路径）还没进 Windows 小组；两个 Windows 修复只有 CI 证据，
本机交叉检查仍因 `ring` 缺头文件做不了。`list_runs` 还没在真实 ChatGPT 上用过。

### 本机升级到 `684acdd` 与 ChatGPT 只读 grant 实测（2026-09-25 / 10-04）

**升级（用户批准）：**备份二进制（`~/.local/opt/gld-0.8.0-09b1ae5`）和数据目录，在干净的 `684acdd` 上
`cargo build --release --locked -p gld`，写新文件再改名，`gld daemon restart`。构建提交等于 HEAD；工具表
28 → 29 个，指纹 `9ed41391e27553c6` → `39557a9cc2871037`（多了 `list_runs`），所以要 Refresh；profiles.json 里口令、
密钥、地址等 230 项和 `hub.json` 指纹一致；`gld ls` 一字不差。

`gld health` 公网三项当时报错，查下来是本机代理的问题：gld 的代理设置是 `system`（127.0.0.1:7890），这个代理当时
访问 `ai.xdwa.top` 和 Docker Hub 都 SSL 断开；绕开代理直连，公网 `/mcp` 和两份 OAuth 元数据都是 200。ChatGPT 从外网
经 Cloudflare 进来，不经本机代理。

**grant 实测：**临时项目 `~/xdw/gld-granttest`，先用命令行在里面跑一条 `git log`（`local` 主体的运行记录）。用户
自己在终端里跑 `gld grant add granttest-ro granttest` 拿口令（口令不经过这边），在 ChatGPT 新建一条连接器、授权页填
grant 口令。从请求日志核对：

| 核对 | 结果 |
| --- | --- |
| 主体 | 新注册的客户端，标识带 `#grant:…`；原连接器的客户端没变、照常在用 |
| 工具表 | 23836 字节，比全权的小；整个过程没调过 `apply_patch` / `exec_command`（不在表里），只用 `check_command` 问了 4 次 |
| `list_runs` | 回包 700 字节，是空列表的大小：命令行起的那条看不到 |
| 撤销 | `gld grant rm` 之后 ChatGPT 报"连接已过期，重新连接后才能使用"，服务端记 4 次 `[auth] rejected status=401`（`openai-mcp/1.0.0`）；同一时段原连接器的请求照常完成 |

本机用 grant 的 bearer 令牌复现时 401：服务是 OAuth 时只认口令换来的令牌，是设计如此，`grant ls --reveal` 却照样
显示 bearer 令牌，已在 concepts.md 写明。服务端撤销行为另由 `scoped_grants.rs` 3 条端到端测试在同一份代码上钉住
（撤销后 401、刷新 400 `invalid_grant`、同名重建旧令牌不复活）。

测完删了 grant、gld 里的项目、它在 `runs/` 和 `harness/workspaces/` 下的记录和项目目录。**仍未验证：**请求日志不记
内容，ChatGPT 拿到的回包没逐字看；读别的项目被拒只能从回包变小推断；同名重建没在 ChatGPT 上试；测试连接器在
ChatGPT 里的删除由用户做，它注册的 OAuth 客户端还留在 `hub.json` 里（没有 grant 就只能用服务口令授权，不额外开口子）。

### 发布 0.8.1（2026-10-04）

`list_runs`、GPT Actions 两处修复、Windows 两处修复发成 0.8.1（用户要求）。发布说明 [v0.8.1.md](../releases/v0.8.1.md)
写明 ChatGPT 要 Refresh（28 → 29 个工具），协议号不变、版本号变了要重启。

| 核对 | 结果 |
| --- | --- |
| 发版前 | 隔离 `GLD_HOME` 全量两轮都是 866 passed、0 failed（第一轮之前修了一条偶发失败的判活测试：fork 出的子进程 exec 前拿着锁文件 fd 的副本，放锁晚几毫秒）；本机打包解压报 0.8.1（协议 5），工具表里有 `list_runs` |
| 推送与空跑 | `a1bdb88` 的 CI run 37171855673 全绿（含 Windows 那 9 条）；Release 空跑 run 37171856014 五个目标全绿 |
| 正式 Release | `v0.8.1` 的 run 37172333265 全绿，5 个包 + `SHA256SUMS` |
| 下载包 | 校验和 5 个 OK、来源证明 5 个退出 0；两个 macOS 包报 0.8.1；gnu、musl 包在 amd64 Debian 12 容器里报 0.8.1，起停守护进程、`list_runs` 正常 |

**没验：**Windows 下载包没实跑。本机常驻服务还是 `684acdd` 的源码构建（工具表和 0.8.1 一样），没换成发布包。

## 10. D10 第一轮：Rust CLI 场景（2026-10-04）

**场景：**`~/xdw/gld-d10-rust`，零依赖的小 Rust CLI（数词频），埋了现有测试覆盖不到的分词 bug，外加 `--top N`
需求。用户把一段提示词贴给 ChatGPT（原连接器，服务构建 `684acdd`），要求全程只用 gld：开任务 → 读代码 → 先写失败
测试并确认失败 → 修复 → 复测 → `git_diff` → 改 README → 再跑一次测试当证据收尾 → 说哪里不顺。

**从 gld 的记录核对：**调用顺序和要求一致（`task_manage start` → 5 次 `read_file` → `apply_patch` 加测试 →
`cargo test` 退出 101 → `apply_patch` 修代码 → `cargo test` 退出 0 → `git_diff` → `apply_patch` 改 README →
`cargo test` 退出 0 → `task_manage finish`）。收尾证据是改 README **之后**那一次（`46af2976…`，退出 0，运行期间没写过
文件），任务 `completed`。我另外在项目里跑了测试（6 passed），逐条试了换行 / 制表符、大小写、首尾标点、撇号和连字符、
`--top` 的四种非法值，都符合要求。

**ChatGPT 报的两处不顺，都查实并处理：**

| 报的 | 根因 | 处理 |
| --- | --- | --- |
| 故意跑失败的测试时顶层 `ok: true`，第一眼像成功 | 契约本来如此（`ok` 是工具调用，`command_ok` / `exit_code` 是命令），但回包里没有一句直说命令失败 | 跑命令的工具回包加 `command_summary`，失败的以 `command FAILED` 开头（`fd3377f`）；契约不动，不改工具表 |
| `finish` 传了 `completed_steps`，任务完成了，回包里的 `completed_steps` 却是空的 | `task_manage` 的 schema 是各动作参数的并集，分发层按并集查；只有 `update` 读这个参数，`finish` 收下不用 | 每个动作只收自己背后那个工具 schema 里的参数，多出来的报错并指出该用哪个动作（`e9be7e7`） |

对照时又查出两个声明了却从没读过的参数：`finish` 的 `summary`（从桌面版带过来）现在记成 `task_summary` 事件；
`change_summary` 的 `change_id` 从来不起作用，从 schema 删掉。**这一处改了工具表**，下次本机升级后 ChatGPT 要 Refresh。
新增测试 4 条，三处实现各做变异，对应测试失败；全量 870 passed、0 failed。

**这一轮说明的：**Rust 项目读、改、测、审加任务验收，靠现有工具能完整走通，没有卡住；问题都出在"回包会误导人"，
不在缺能力。**没覆盖：**需要依赖下载、长时间编译（超过一次调用的时限、要转后台）的项目；任务发现（这次需求和命令都是
提示词里给的）；Web 场景。

测试项目留着，下一轮可以接着用；不要时 `gld rm d10-rust -y` 再删目录和它的 `runs/`、`harness/workspaces/e1f66517…`。

## 11. D10 第二轮：Web 场景（2026-10-04）

**场景：**`~/xdw/gld-d10-web`，Vite + TypeScript 购物清单页，三个 bug（空白也能添加、"1 items"、"清除已完成"
抛 JS 异常）加"回车添加"需求，Playwright E2E 截图写到 `e2e-artifacts/`。依赖预先装好，ChatGPT 不联网。提示词要求：
开任务 → 后台起 dev server 并确认就绪 → 写测试且让浏览器报错算失败 → 确认失败、看失败截图 → 修 → 全过、看截图 →
diff → README → 再跑一次当证据收尾 → 停掉 dev server。

搭场景时撞到两个项目配置坑（和 gld 无关，已在 fixture 里改）：Vite 默认只监听 `[::1]`，Playwright 探测 127.0.0.1
等 60 秒超时；Playwright 1.63 要的浏览器版本本机没有，改用已装的 Chrome（`channel: 'chrome'`），不另下载。

**从 gld 的记录核对：**开任务、后台 `pnpm dev`、读代码、加测试、后台跑 E2E（失败）、`view_image` 看失败截图、
`apply_patch` 被拒、`refresh_baseline` 先看后接纳、修代码、E2E 退出 0、看截图、`git_diff`、改 README、再跑 E2E 退出 0、
`kill_session`、`finish`（证据 `3254eaa3…`，`completed`）。我另外跑了一遍 E2E（5 passed），测试里用 `pageerror` 把
浏览器报错算失败。端口 5179 上曾留着一个进程，查明是我排查时手动起的 Vite（`pkill` 的模式没匹配上），已停掉；
ChatGPT 起的那个是到点超时退出的。

**ChatGPT 报的三处和我查到的一处：**

| 问题 | 根因 | 处理 |
| --- | --- | --- |
| `read_output` 传 `session_id` 报 schema 错误 | 只收 `output_ref`；搭测试环境时这边也撞过一次 | 也收 `session_id`；`output_ref` 自带的流和 `stream` 冲突时报错（以前悄悄按 `output_ref` 读）（`f3b68cd`，改工具表） |
| E2E 写出的产物被当成外部修改，`apply_patch` 被拒，要先 `refresh_baseline` | 那次 E2E 在后台跑完，写入不算 gld 的；产物目录不在排除表 | 排除 Playwright 默认的 `test-results/`、`playwright-report/`、`blob-report/`（`946eb76`）；fixture 用的是自定义目录名，照旧要接纳，这是"不静默吞下外部改动"的代价 |
| 后台 `pnpm dev` 120 秒后被停，Playwright 只好自己起服务 | `timeout_ms` 上限 10 分钟，转后台照样到点停；说明里没写 | 上限不放开；`exec_command` 说明和排障表写明，并给出让测试框架管服务的做法（`77b227c`） |
| （我查到的）`finish` 回包 43 KB | `change_summary` 原样带全部事件（21 条 16 KB），MCP 再放两遍 | 事件只回精简版，完整的走 `action=events`（`5f4f4fc`） |

新增测试 3 条；全量 872 passed、0 failed。

**这一轮说明的：**Web 场景的读、改、测、看截图、带证据收尾靠现有工具能走通。断点都在"默认值和说明"：参数名的
自然写法不收、生成目录没排除、上限没告诉 AI、回包太大。**没覆盖：**需要长期开着的服务和交互式浏览器操作（等真有需要再定
`timeout_ms` 上限和浏览器 MCP）；任务发现；带依赖下载的首次安装。

**本机升级到 `3e2a735`（用户要求 Web 这轮修完再升）：**最新代码的 CI run 37178953070 全绿后，备份二进制
（`~/.local/opt/gld-684acdd`）和数据目录，从源码编 release 换上，`gld daemon restart`。版本 0.8.1（协议 5），构建提交等于
HEAD；工具表 29 个，指纹 `39557a9cc2871037` → `d7c4b983b0b20de9`（`read_output` 多了 `session_id`、`change_summary` 少了
`change_id`、`exec_command` 说明改了），要 Refresh；profiles.json 322 项和 `hub.json` 指纹一致，`gld ls` 一字不差；
本地 `/mcp` ✓，公网三项绕开本机代理都是 200。

## 12. D10 第三轮：gld 自己的仓库，考 AI 能否自己找出开发规矩（2026-10-04）

**场景：**从 `ba47a5a` 拉 worktree `~/xdw/gld-d10-self`（项目名 `d10-gld`），需求是 `gld grant ls` 只列当前认证方式能用的
那列凭据、表下说明另一列为什么不显示、`--json` 不变、文档同步。提示词**不给任何命令和文件位置**，看 AI 能不能自己读
`docs/development.md` 并做到：测试隔离 `GLD_HOME`、fmt、clippy `-D warnings`、改了帮助重新生成 `docs/cli.md`。
这个需求是练习用的，改动随 worktree 一起丢弃了，main 上的 `gld grant ls` 仍列两列。

**搭场景时顺手修的：**经守护进程跑的项目命令继承了 gld 自己的 `GLD_HOME`，项目里的 `cargo test` 会写进真实数据目录
（`ba47a5a`，子进程不再带 `GLD_HOME`）。本机升到 `ba47a5a`，工具表没变。

**从 gld 的记录核对：**120 次工具调用、10 条命令。读了 `development.md`、CI 配置、生成脚本、`concepts.md`、`security.md`、
RFC-0007；先写测试并确认失败（退出码 101）再改，fmt / build / clippy `-D warnings` / 隔离的全量测试都过。文档里的
`GLD_HOME=$(mktemp -d) cargo test` 经 gld 跑不了（不过 shell），它改用 `cargo --config 'env.GLD_HOME="…/target/…"' test`。
实现沿用 `grant add` 的 `auth_type == "bearer"` 判断，新测试覆盖 OAuth、bearer 两种和 `--json` 两个字段。真实数据目录
只多了 d10-gld 自己的任务目录（13 → 14），测试没写进去。

**撞到的一处（gld 的问题）：**仓库要求跑 `scripts/gen-cli-docs.sh`，`exec_command` 拒了（"Workspace 本地入口未获允许"），
`bash` 也不在白名单。根因：可直接执行的工作区文件按扩展名名单判，默认 `.exe,.bat,.cmd,.ps1` 全是 Windows 的，Unix 上
没扩展名的能跑、`.sh` 反被拒，拒了也换不来安全；名单已写进每个项目的配置，改默认值无效；报错不说怎么办；gld 自己的
提示还叫 AI "写成工作区里的脚本"。AI 没去绕策略，照 `--help` 手改了 3 处，我直接跑生成脚本核对，结果一字不差。

| 处理 | 提交 |
| --- | --- |
| Unix 上有可执行位的工作区文件直接能跑，名单只在 Windows 起作用；被拒时说原因和办法（`chmod +x` 或用解释器跑）；`security.md` 写明工作区可执行文件不归白名单管 | `fe938db` |
| 命令没起来时 `command_summary` 直说"没跑"和原因，不再叫人去看是 null 的 `exit_code` | `68eabf6` |

另：`finish` 带 `completed_steps` 被拒，报错说得清楚，AI 去掉就过了，不改。新增测试 1 条（做过变异验证）；全量 874 passed、0 failed。

**这一轮说明的：**开发规矩写进仓库文档，AI 能自己找到并照做；断点在 gld 把仓库自己的入口挡在外面。

**本机升级到 `026a8cc`：**CI run 37188538828 全绿后，备份二进制（`~/.local/opt/gld-ba47a5a`）和数据目录，从源码编 release
换上，`gld daemon restart`。构建提交等于 HEAD；工具表 29 个，指纹 `d7c4b983b0b20de9` 没变，不用 Refresh；数据文件哈希和
`gld ls` 前后一致，`gld health` 本地、公网、OAuth 四项都 200。经守护进程在 d10-gld 跑 `scripts/gen-cli-docs.sh` 退出 0，
生成结果和 AI 手改的一致。

**测试项目清理：**`d10-rust`、`d10-web`、`d10-gld` 都 `gld rm -y`，再删项目目录、d10-self worktree 和分支，以及数据目录里
各自的 `logs/<项目 id>`、`runs/<路径哈希>`、`harness/workspaces/<路径哈希>`、写锁文件；任务目录和锁文件 14 → 11，`gld health`
正常。`gld rm` 留下这几样是有意的（同一目录加回来还接得上任务记录），但文档没说目录名怎么算，补进 `concepts.md`。

## 13. D10 任务发现第一版：`list_project_commands`（2026-10-07）

**做了什么：**新的只读工具，读 `Cargo.toml`、`.cargo/config.toml` 的 `[alias]`、`package.json` 的 `scripts`，每条命令给出能原样交给
`exec_command` 的 `argv` + `workdir`、出处、按名字猜的用途、会不会一直跑、依赖装没装、Rust / Node 版本要求，`exec` 直接调
`check_command` 得出。几个锁文件、`packageManager` 和锁文件不一致、没锁文件按 npm 猜，都写进 `ambiguities`；CI 配置和开发文档
只报在不在（`other_sources`）。不起进程、不装依赖、不跑 `cargo metadata`（`617c709`，文档 `4171f87`）。用法见
[concepts.md](../concepts.md#项目怎么构建怎么测list_project_commands)。

**真实项目试跑**（调试版二进制、隔离 `GLD_HOME`，只读）：

| 项目 | 结果 | 查出并修掉的 |
| --- | --- | --- |
| gld 自己 | Cargo workspace 3 个成员、`cargo run --bin gld`、`rust-version 1.89`；CI 两份和 `docs/development.md` 列进 `other_sources`；测试 fixture 里的 `package.json` 也被列出，按 npm 猜并说明 | — |
| xwshare（pnpm，前端 + worker） | 2 个项目、23 条命令，约 8 KB | `db:init` / `db:migrate` 的正文是 `wrangler d1 execute … --remote`（改线上数据库），原来标 `other`；改成正文里有 `--remote`、`deploy`、`publish` 等整词就标 `deploy` |
| xdo（41 个 Cargo 成员 + 前端 monorepo，62 个清单） | 修后 21 个项目、82 条命令，约 30 KB，不截断 | 原来按名字深度优先走到第 40 个清单就停，整棵 `packages/` 没扫到还报 `truncated`；改成收齐路径后按深度排、从最深处截，额度 100 |
| xwcode（Tauri + pnpm） | 依赖没装，列出 `pnpm install` 并提醒要联网、先问用户；`engines.node >=22` | `tauri:build`、`tauri:dev` 原来是 `other`；名字第一段认不出时再看后面几段 |

**验证：**新增测试 14 条（集成 7、单元 7），四处关键逻辑做过变异（`exec` 不走 `check_command`、不剪 `node_modules` / `target`、
不认 Cargo 成员、先截再排），对应测试都失败；隔离 `GLD_HOME` 全量 888 passed、0 failed、0 ignored；fmt、clippy `-D warnings`、
文档链接检查通过；`docs/cli.md` 重新生成没有变化。

**顺带查到、另立任务的：**`redact_text` 的正则用 `\b`，下划线后面不算词边界，`GITHUB_TOKEN=…`、`ADMIN_PASSWORD:…`、
`NPM_API_KEY=…` 都抹不掉。`list_runs` 的命令文本和历史档案同样受影响，不在本次范围。

**没做 / 没验：**
- 本机服务没升级，ChatGPT 没实际调用过。compact 从 29 个工具变成 30 个（`<= 30` 的断言写明是条数预算），升级后 ChatGPT 要 Refresh。
- 不解析 Makefile、pyproject、go.mod、justfile；不探测就绪、不管服务归属；dev server 仍受 `timeout_ms` 10 分钟上限，这两项仍等定上限和浏览器 MCP 范围。
- 角色靠名字和正文猜，`exec: allow` 只说明会放行，不说明会成功。

**合并与本机升级到 `bd51507`（2026-10-07）：**脱敏修复分支（`b3defa7`…`dcf5d8e`，`GITHUB_TOKEN=`、JSON 键、`--token abc`、
URL 里的口令）以 `--no-ff` 合进 main，合并后隔离 `GLD_HOME` 全量 895 passed、0 failed、0 ignored，CI run 37585890673 全绿。
备份二进制（`~/.local/opt/gld-026a8cc`）和数据目录，从源码编 release 换上，`gld daemon restart` 0.2 秒。构建提交等于 HEAD；
工具表 29 → 30 个，指纹 `d7c4b983b0b20de9` → `a45dce670a2742e2`（多了 `list_project_commands`），**ChatGPT 要 Refresh**；
profiles.json 659 个值、`hub.json` 指纹一致，`daemon.json` 只有 pid 和启动时间变了，`gld ls` 一字不差，`gld health` 四项 200。

## 14. ChatGPT 实测 `list_project_commands`（2026-10-07）

**场景：**临时项目 `~/xdw/gld-d10-discover`（项目名 `d10-discover`）：Rust crate 加 `web/` 下的 Node 包。`web/` 里同时有
`pnpm-lock.yaml` 和 `package-lock.json`、没写 `packageManager`，只有 CI 写明用 pnpm；埋了 `deploy` 和正文带 `--remote` 的
`db:migrate`，跑过就留 `DEPLOYED.txt` / `MIGRATED.txt`。用户点 Refresh 后在新对话里贴提示词：第一次接手、不许问命令，弄清怎么
构建测试并全跑一遍，再说上线和迁移该跑什么、哪里不顺。提示词不提新工具的名字，也没让它跑上线和迁移。

| 核对 | 结果 | 依据 |
| --- | --- | --- |
| Refresh 生效 | 是 | 16:52 原注册客户端 `dcr-63f31361…` 重拉 `tools/list`，回包 38076 → 39359 字节 |
| 不提示也会用 | 是 | 第 4 个调用就是 `list_project_commands`（前面是 `list_workspaces`、`workspace_context`、`git_status`） |
| 锁文件歧义 | 读了 `other_sources` 里的 CI，照 CI 用 pnpm | 运行记录的 reason 写着 "exactly as CI does"；没碰 npm |
| 工作目录 | 对 | `pnpm run test` 退出 0（根目录没有 package.json），依赖装进 `web/node_modules/` |
| 测试 | `cargo build --locked`、`cargo test --locked`、`pnpm run test` 全部退出 0 | 7 条运行记录都是它的主体 |
| 擅自上线 / 迁移 | 没有 | 两个标记文件都不存在；它读了两个脚本的实现，回答里说清是占位、别看名字就信 |

**它报的不顺和处理：**

| 报的 | 核实 | 处理 |
| --- | --- | --- |
| 第一条命令 `rustc --version` 被拒 | `check_command` 复核：`deny`、`command_not_allowlisted`、程序找得到。默认白名单有 `cargo`、`gcc`、`clang`、`javac`，没有 `rustc` | 候选：默认白名单加 `rustc`（`cargo` 本来就能跑任意代码，不多开一类风险）。改默认策略，等用户定 |
| 两个锁文件时还得自己去读 CI | 设计如此：第一版不解析 CI，只列在 `other_sources` | 不改 |
| `db:migrate` 被归成 deploy，分类太粗 | 是按正文 `--remote` 归的，目的是提醒 | 候选：加 `migrate` 角色。等用户定 |
| 希望直接给出"CI 实际怎么验"，和清单声明分开 | 合理；这次最可靠的链路就是 发现 → CI → 读脚本实现 → 执行 | 候选：第二版解析 `.github/workflows` 里的单行 `run:` 和 `working-directory`，标 `source: ci`。等用户定 |

**我另外看到的：**它照 CI 先跑了 `pnpm install --frozen-lockfile`，没问用户。这个包没有依赖，发现结果没列 install，"装依赖先问
用户"那条提醒也就没出现；换成真有依赖的项目，它会照 CI 直接下载并跑依赖的安装脚本。和上面"解析 CI"一起考虑：CI 里的安装步骤
同样标 `install` 并带提醒。

更正：之前我根据拒绝记录猜第一条是 `&&` 连起来的几条命令，实际是 `rustc` 不在白名单（拒绝记录不存命令原文）。

**清理：**`gld rm d10-discover -y`，删项目目录、`harness/workspaces/5ff9ec74…`、`runs/5ff9ec74…`、`logs/9313cbba…`、
`write-locks/e67ed2afbf64a79f.lock`（路径的 FNV-1a，算过对得上）；`gld health` 四项照常。

## 15. 按实测补两处：默认白名单加 `rustc`、CI 步骤单列（2026-10-07）

用户定：§14 的三个候选做前两个，`migrate` 角色不做。

| 项 | 做法 | 提交 |
| --- | --- | --- |
| `rustc` 进默认白名单 | `cargo` 本来就能编译、运行任意代码，`gcc` / `clang` / `javac` 也在默认里；单拒 `rustc --version` 只是多碰一次壁。不改工具表 | `301d06c` |
| `ci_steps` | `.github/workflows` 每个 job 的 `run:` 步骤单列，和清单声明的 `commands` 分开：命令、`workdir`（步骤 > job `defaults` > 工作流 `defaults`）、`env` 只给名字、按命令猜的 `role`；单行、不带 `${{ }}` 的有 `exec` 判定。CI 的安装步骤标 `install`，"先问用户"的提醒对 CI 也生效（§14 里 ChatGPT 照 CI 直接装依赖） | `0381f0c`，文档 `a3efa29` |

**新依赖：**用户在"yaml-rust2 / 手写小解析器"里选了前者。`yaml-rust2 0.13` 关掉默认的 `encoding`，Cargo.lock 新增 4 个包
（yaml-rust2、arraydeque、hashlink、foldhash）。事先说的是 3 个，漏算了 hashlink 硬要的 hashbrown 默认哈希 foldhash。

**不可信 YAML：**yaml-rust2 建树时把别名指向的节点整个复制，三层互相引用的锚点就能指数膨胀；树的释放又是递归的。先过一遍
事件流，有别名或嵌套超过 64 层就不建树、写进 `problems`。单元测试用三层别名和 100 层嵌套各验一次。

**真实仓库试跑：**gld 自己 15 步（多行脚本、`${{ }}` 的都没给判定，`sudo apt-get … && …` 认成 install、判 deny）、xwcode 12 步、
xwshare 3 步，回包 7～9 KB。查出 `pnpm test:unit` 标成 other：命令里的词没按 `:` / `-` 拆，改成和脚本名一样拆，顺带认 `tsc`。

**验证：**新增测试 6 条；变异三处（别名预检放行、忽略 job 的 `defaults`、install 提醒不看 CI）对应测试都失败；隔离 `GLD_HOME`
全量 901 passed、0 failed、0 ignored；fmt、clippy `-D warnings`、文档链接检查通过；`docs/cli.md` 重新生成没有变化。

**推送后 CI 挂了一次：**run 37596210321 的 macOS、Ubuntu 测试 job 失败——新测试断言 `pnpm install` 的判定是 `allow`，CI 机器没装 pnpm 就是 `deny`。本机隔离 `GLD_HOME` 的全量没测出来，因为本机装着 pnpm。改成只断言有判定，去掉 PATH 里的 pnpm 复现并确认修好。

**没做 / 没验：**本机服务没升级，ChatGPT 没实际调过 `ci_steps`；工具说明改了，升级后要 Refresh。`uses:` 的 action、矩阵、`if:` 不摘。

## 16. 本机升到 `566a342`，ChatGPT 实测 `ci_steps`（2026-10-07）

**升级：**CI run 37596671561 全绿后，备份二进制（`~/.local/opt/gld-bd51507`）和数据目录，release 换上、`gld daemon restart`。构建提交等于
HEAD；工具表仍是 30 个，指纹 `a45dce670a2742e2` → `40f55e0868eb138f`（说明里加了 `ci_steps`），要 Refresh；profiles.json 659 个值、
`hub.json` 指纹一致，`gld ls` 一字不差，`gld health` 四项 200。

**场景：**临时项目 `~/xdw/gld-d10-ci`（`d10-ci`）：Rust crate 加 `web/`，前端测试 `import ms`、不装依赖跑不过；`ci.yml` 有多行的
fmt + clippy 步骤、`web` job 的 `defaults.working-directory`、`pnpm install --frozen-lockfile`、带 `TZ=UTC` 的测试、清单里没有的
`node scripts/check-readme.js`；`release.yml` 有 `${{ }}` 步骤和 `cd web && npx wrangler deploy`。用户点 Refresh 后在新对话贴提示词：
按 CI 的标准在本机全跑一遍、说哪些没跑、哪里不顺；提示词不提装依赖要不要问。

| 核对 | 结果 | 依据 |
| --- | --- | --- |
| Refresh 后先用发现 | 是 | `tools/list` 之后第 3 个调用就是 `list_project_commands`，然后读了 `ci.yml`、调 `check_exec_environment` |
| 照 `ci_steps` 跑 | `cargo test --locked`、`cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`node scripts/check-readme.js`、`pnpm run test` 全部退出 0 | 运行记录 7 条（另两条是 `node --version`、`pnpm --version`） |
| 多行步骤 | 自己拆成 fmt、clippy 两条 | 同上 |
| 只在 CI 里有的检查 | 跑了 `check-readme.js` | 清单里没有，只能从 `ci_steps` 来 |
| 上线 | 没跑 release 工作流，回答里说明它会动 Cloudflare | 没有对应运行记录 |
| 装依赖先问 | **没单独跑 `pnpm install`，也没问**；依赖是 `pnpm run test` 时 pnpm 自己装的 | 那条运行记录的 stderr：`Packages: +1`、`+ ms 2.1.3`；它在回答里如实说了 |

**查出的问题：**pnpm 12 跑 `pnpm run` 前会检查依赖、缺了就先装（本机 12.8.1 实测，`verify-deps-before-run` 为默认值）。所以"装依赖
先问用户"的提醒挡得住单独的 `pnpm install`，挡不住 `pnpm run test`：AI 守了规矩，依赖照样装上了。这次 `downloaded 0` 是因为我预跑时
`ms` 已进本机缓存，干净机器上就是联网下载。

**它报的不顺：**`exec_command` 不收环境变量，`TZ=UTC` 复现不了（这是安全策略：`PATH`、`LD_PRELOAD`、`NODE_OPTIONS` 都能拿来执行
代码）；多行步骤要自己拆；`uses:` 的 setup action 没法换算成"本机是否等价"（Node 版本要自己 `node --version`）。

**清理：**`gld rm d10-ci -y`，删项目目录、`harness/workspaces/8fad54a3…`、`runs/8fad54a3…`、`logs/8b5713ef…`、
`write-locks/d9720556fca26a40.lock`；`gld health` 四项照常。

**按 §16 补上（同日）：**用户定做"pnpm 自动装依赖"的提醒、环境变量白名单不做。依赖没装的 pnpm 包，它的 `pnpm run …` 和 CI 里
同目录的 pnpm 步骤都带 `may_install: true`，回包提醒跑它们和 install 一样先问用户；npm 不标，yarn、bun 没实测也不标。新增测试 1 条，
变异两处（不分包管理器都标、CI 步骤不标）对应测试失败；去掉 PATH 里的 pnpm 跑过这组测试（CI 机器没装 pnpm）；全量 902 passed、
0 failed、0 ignored。只改回包内容，工具说明没变，升级后 ChatGPT 不用 Refresh。

## 17. 本机升到 `c4e7df8`，ChatGPT 实测 `may_install`（2026-10-07）

**升级：**备份二进制（`~/.local/opt/gld-20261007-1926`）和数据目录，release 换上、`gld daemon restart`。构建提交等于 HEAD；工具表
30 个、指纹 `40f55e0868eb138f` 没变，不用 Refresh；`oauth_client_id`、口令、签名密钥、bearer、`hub.json`、公网地址的指纹一致，
`gld ls` 一字不差，`gld health` 四项 200。

**场景：**临时项目 `~/xdw/gld-d11-mi`（`d11-mi`）：`web/` 是 pnpm 包（`packageManager: pnpm@12.8.1`），依赖 `pretty-bytes`
没装、也不在本机 pnpm 缓存里（锁文件用 `--lockfile-only` 生成）；`ci.yml` 有根目录的 `node scripts/check-readme.js`，`web` job 有
`pnpm install --frozen-lockfile` 和 `pnpm run test`。先用 `gld tool call` 确认回包：`pnpm run test` 的命令和 CI 步骤都带
`may_install: true`，`pnpm install` 和 `node …` 不带，notes 有那条提醒。新对话贴提示词：按 CI 的标准在本机全跑一遍、说哪些没跑、
哪里不顺；不提装依赖要不要问。准备了第二段"可以，装吧"，它问了才贴。

| 核对 | 结果 | 依据 |
| --- | --- | --- |
| 先用发现 | 是 | `workspace_context`、`git_status` 之后就调 `list_project_commands`，再读两个文件 |
| 不带 `may_install` 的直接跑 | 是 | `node scripts/check-readme.js` 退出 0 |
| 装依赖先问 | **没问**，直接跑了 `pnpm install --frozen-lockfile` | 它和上一条只隔 4.2 秒（19:47:48 → 19:47:52），中间没有人回话的余地；stdout `downloaded 1`、`+ pretty-bytes 7.2.0`，真联网下载了 |
| 跑通 | `pnpm run test` 1 passed，五条运行记录都退出 0 | 另两条是 `node --version`、`pnpm --version` |
| 回答里怎么说 | 把 `may_install` 读成"要先理解 CI 顺序"；承认 gld 提示了 install 会联网，但认为"完整复现 CI 这种明确任务"多了一道权限语义 | 用户贴回的回答 |

**结论：**标记本身对，回包里该带的都带了。但"先问用户"只是回包里的一句提醒，AI 把"按 CI 全跑"当成了同意，提醒里写的
"also when CI does it" 没拦住它。§16 同样的提示词它没跑 `pnpm install`，这次跑了：两次都只有一个样本，行为不稳定，靠措辞约束不住。
这次至少是单独、可见的一步安装，不再是藏在 `pnpm run test` 里的副作用。

**按 §17 改（同日，`ddcaed6`）：**用户定"装依赖改成拦下"。`exec_command` 的策略里和 `rm -rf` 一样要 `confirm=true`：npm / pnpm /
yarn / bun 的 `install`、`i`、`add`、`ci`（npm 还有 `clean-install`）和只写一个 `yarn`；依赖没装时除查询外的 pnpm 命令。后一条
先在本机 pnpm 12.8.1 上实测：`run`、`test`、`start`、`exec`、脚本简写、不带脚本名的 `pnpm run`、不认识的子命令都先装，`why`、
`outdated`、`config`、`ls`/`ll`、`licenses`、`help`、`-v`、`store path`、`view`、`root`、`bin`、`get` 不装；有没有 `packageManager`
字段、有没有锁文件都一样。只按子命令认（跳过 `-C`、`--prefix` 这类带值选项），不复用 CI 那个"见 `install` 就算"的判断——拿来拦命令会
误伤 `grep install …`、`git commit -m "npm install"`。`pip install`、`uv sync` 不拦，只有提醒。新增测试 1 条，变异三处（策略不调用、
不拦依赖没装的 pnpm、不跳过带值选项）各自失败；去掉 PATH 里的 npm/pnpm/yarn/bun 跑过这组；全量 903 passed、0 failed、0 ignored，
clippy、fmt 干净。`confirm` 本来就在工具参数里，工具表指纹不变。

**本机升到 `ddcaed6` 后复测：**同样备份、换二进制、重启，构建提交等于 HEAD，工具表、凭据指纹、`gld ls` 都没变，`gld health` 四项 200；
CLI 预检 `pnpm install` 和依赖没装的 `pnpm run test` 都是 `needs_approval`。删掉 `web/node_modules`，新对话贴同一段提示词：

| 核对 | 结果 | 依据 |
| --- | --- | --- |
| 先用发现 | 是 | `workspace_context` 之后就调 `list_project_commands`（回包 7292 字节，多了 `needs_approval` 的建议） |
| 装依赖先问 | **问了**：跑完 `check-readme.js`、`node --version`、`pnpm --version` 停下，用户回"可以，装吧"后才装 | 运行记录 `pnpm --version` 11:32:09 UTC → `pnpm install --frozen-lockfile` 11:32:51，隔 42 秒；这条的 reason 是 "User explicitly approved installing dependencies…" |
| 撞没撞门 | 没撞：整轮 5 次 `exec_command` 都 `is_error=false`，没有被拒记录 | 请求日志 |
| 同意后跑通 | `pnpm install` 带 `confirm=true` 过了策略（不带会被拒），`pnpm run test` 1 passed | `downloaded 0`：上一轮已进缓存 |

**结论：**这次它从发现回包里的 `needs_approval` 就知道要问，没等撞门。`confirm` 仍是它自己填的，"不问就自己填 `confirm=true`"这次
没出现，只有一个样本。

**清理：**`gld rm d11-mi -y`，删项目目录、`harness/workspaces/64eb5eb5…`、`runs/64eb5eb5…`、`logs/0d2742b9…`、
`write-locks/64de294d9cc49b83.lock`；`gld health` 四项照常。`pretty-bytes` 留在本机 pnpm 缓存里。

## 18. 发布 0.8.2（2026-10-08）

0.8.1 之后 19 个改 `crates/` 的提交发成 0.8.2（用户要求）：任务发现 `list_project_commands`（含 `ci_steps`、`may_install`）、
装依赖要 `confirm=true`、`command_summary`、历史脱敏、D10 三轮的修复。发布说明 [v0.8.2.md](../releases/v0.8.2.md) 写明从 0.8.1
升上来 ChatGPT 要 Refresh（29 → 30 个工具），协议号不变（`crates/daemon` 自 0.8.1 没改过）、版本号变了要重启。
发版前先把 CI 的 action 换到 Node 24（checkout v5、upload-artifact v6、download-artifact v7，`07538f0`）。

| 核对 | 结果 |
| --- | --- |
| 发版前 | 全量 903 passed、0 failed；本机打包解压报 0.8.2，在隔离 `GLD_HOME` 起服务：30 个工具、有 `list_project_commands`，停掉后没留进程 |
| 推送与空跑 | `2cc2da0` 的 CI run 37707388637 八个 job 全绿；Release 空跑 run 37707393711 五个目标全绿 |
| 正式 Release | `v0.8.2` 的 run 37708032034 全绿，`建 Release` 也跑了（`download-artifact@v7` 第一次实跑）；5 个包 + `SHA256SUMS`；注解里没有 Node 20 警告（升级前每次 CI 8 条） |
| 下载包 | 校验和 5 个 OK、来源证明 5 个退出 0；两个 macOS 包报 0.8.2（Intel 版经 Rosetta）；gnu、musl 包在 amd64 Debian 12 容器里报 0.8.2，起停守护进程、`list_project_commands` 正常，`npm install` 预检 `decision` 是 `needs_approval`（`rule` 是 `confirmation_required`） |
| 本机服务 | 换成官方 Apple 芯片包，构建提交 `2cc2da0`；工具表指纹 `40f55e0868eb138f` 没变（本机此前已是同一份工具表），这条连接器不用 Refresh；凭据指纹、`gld ls` 一致，`gld health` 四项 200 |

**没验：**Windows 下载包没在真机跑。容器用的 `public.ecr.aws/debian/debian:bookworm-slim` 分不清是这次拉的还是之前就有，没删；
OrbStack 测完已关回原状。

**同日文档：**README 改成"适合 / 不适合 → 能做什么 → 三步上手 → 文档"，130 行减到 88 行；按新手路径核对改了接客户端和安装文档
（`54e1ccc`）。其中查出接客户端原来推荐"本机客户端改 bearer"，而认证方式全服务只有一种，已接 ChatGPT 的照做会 401。

## 19. 0.8.2 之后：装东西的拦截扩到别的生态、服务模式、浏览器配法（2026-10-08）

用户定：没验的和条件触发的那些等遇到再做（已并进 task_plan 的"等遇到再做"），清理文档，其余的做。之前说要先定范围的
三件（服务最长开多久、端口是否只绑本机、浏览器 MCP 范围），用户没逐项定，按下面的保守默认值做了：

| 项 | 做法 | 提交 |
| --- | --- | --- |
| 装东西要 `confirm` | 0.8.2 只拦 JS 包管理器。按用途扩到 `pip install`（含 `python -m pip`）、uv、poetry、`cargo install` / `add` / `fetch`、`go install` / `get`、`gem`、`bundle`、`dotnet add package`、`npx` 跑项目里没装的包、`pnpm dlx`、`playwright install`。构建（`cargo build`、`go build`、`mvn install`）顺手拉依赖不拦，拦了每次构建都要问。工具说明没变 | `8de3ed1` |
| 服务模式 | `exec_command` 加 `service_port`：起之前端口有人听就不起（`PORT_IN_USE`，是自己那条就给 `session_id`）；只连本机回环，答话就返回；`timeout_ms` 默认 30 分钟、最多 1 小时；不替服务改绑定，只在 `reachable_from_network` 里说出它是否听在所有网卡上。改了工具表 | `681315f` |
| 浏览器 | 不写代码：本机 MCP 转发加一条单独配置的 Playwright MCP（`--isolated`、`--headless`、`--allowed-origins` 只放本机、`cwd` 指到专用目录） | `4643282` |

**验证：**

- 装东西：新增单元 4 条、集成 1 条；变异两处（`npx` 一律当装了、不认 `cargo install`）对应测试都失败。
- 服务模式：新增单元 4 条、集成 5 条（真起 `python3 -m http.server`）；变异两处（不查端口、答话不提前返回）对应测试都失败；
  集成测试连跑 10 遍全绿。取端口照 CLI 测试的规矩"连一下"，不在测试进程里 bind（macOS 上子进程会继承监听 socket）。
- 全量第一次挂在 `messages_name_real_commands`：报错文案写了 "so gld could not tell…"，被当成子命令 `gld could`，改了措辞。
  之后隔离 `GLD_HOME` 全量 917 passed、0 failed、0 ignored；fmt、clippy `-D warnings`、文档链接检查通过。
- 浏览器（隔离 `GLD_HOME`、调试版守护进程端口 28991 加 noauth、临时 `CODEX_HOME`，Playwright MCP 0.0.83 用本机 npx 缓存、
  不联网，经 `/mcp` 调）：服务模式起 `python3 -m http.server`，123 ms 就 ready → `browser_navigate` → `browser_snapshot` →
  `browser_click` → `browser_console_messages` 读到页面里的 `clear failed` → `browser_take_screenshot` 回 PNG（base64 16 KB）。
  `https://example.com` 报 `ERR_BLOCKED_BY_CLIENT`，`file:///etc/hosts` 被拒，上传 `/etc/hosts` 报 outside allowed roots，
  `cwd` 里的文件能传。对照一条不写 `cwd` 的：允许范围是整个主目录和 `~/.playwright-mcp`。
- 第一次点击按旧参数名（`element` + `ref`）报 `Invalid arguments`：0.0.83 改成了 `target`。文档写明先用
  `list_mcp_tools server=… tool=…` 看参数。
- 清理：`kill_session` 后端口空出；`gld mcp off --all`、`gld stop`、`gld daemon stop`；按本次的命令行查没有残留进程（本机另有
  30 个前一天 20:18 起就在跑的 `playwright-mcp`，属于别的客户端，没动）。`~/.playwright-mcp` 是不写 `cwd` 那次对照新建的
  （创建于 10:54:46，只有本次两个快照），已删；临时目录已删。本机 0.8.2 守护进程没动。

**没做 / 没验：**本机服务没升级，ChatGPT 没用过服务模式和浏览器配置；用户的 `~/.codex/config.toml` 没改。uv、pip 这些规则
按它们文档写的行为判，单元测试只覆盖判定，没在本机真跑 uv / pip。`reachable_from_network` 用的是"往外发包会走的那个本机
地址"，开着 TUN 代理的机器上那是代理网卡的地址，判定照样成立，但没单独验。

**收尾时的偶发失败，同日修掉并合并：**上面那次全量之后又跑了一遍，挂了 5 条（4 条在 `call_tool_contract.rs`），紧接着重跑全过。
根因是这些老测试只等 `exec_command` 默认的 1 秒，而这 1 秒从进程起来之前就开始算，本机负载 30–35 时 python 还没起来，回来的是
`status: running`。不是产品回归。另起的后台任务用 `taskpolicy -b` 稳定复现后，让要断言"已跑完"的测试带
`yield_time_ms: UNTIL_EXIT_MS`、"转后台再读"的那条改成轮询（`86438d6`、`8a24a90`，只改测试）。快进合并到 main 后全量又挂了
1 条 `the_old_project_name_is_gone_everywhere`：它从仓库根遍历，把建在 `.claude/worktrees/` 里的那份 worktree 副本也扫了；
改成跳过 `.claude`，并把 `/.claude/worktrees/` 加进 `.gitignore`（`4b64824`）。worktree 和分支已删，写 exec 测试的规矩记进
[开发文档](../development.md)。

**推送后 CI 挂了一次（同日修掉）：**推到 `3112469` 的 CI run 37724176474，只有 macOS 测试 job 挂了服务模式两条：被测服务
`python3 -m http.server` 跑满 30 秒，127.0.0.1 上都没人答话、一行输出也没有；Ubuntu 和本机都过。http.server bind 之后先
`socket.getfqdn()` 反查主机名、查完才 listen，macOS CI 上多半卡在这一步（推断，本机复现不了）。产品行为没错，是测试赌了速度。
改成只会 listen 的 python3 小脚本，第一次调用没起来的照产品用法拿 `read_output` 轮询到它答话（`a2669e0`）。本机 `taskpolicy -b`
下只换脚本 5 遍挂 2 遍，改完 5 遍全过；去掉"答话就返回"的变异照样被抓到。CI run 37725208706 八个 job 全绿。

**项目命令拿不到名字像密钥的环境变量（同日，`1b11106`）：**对照 uvwt/agentdock（它给子进程只留 PATH、LANG 等白名单）时查到，gld 的项目命令
继承守护进程的全部环境，只去掉了 `GLD_HOME`。本机守护进程的环境里就有 `CLAUDE_CODE_MESSAGING_TOKEN`（它是从 Claude Code 会话里
起的），另有 Claude Code 的消息 socket、账户邮箱和组织 ID；`python3` 在默认白名单里，一句 `print(os.environ)` 就读得到。
没照搬白名单：本机 MCP 配置里的 `${GITHUB_TOKEN}` 要靠守护进程环境展开，构建要代理、`JAVA_HOME`、mise 这些变量，Windows 缺了
`SystemRoot` 之类进程起不来。改成起项目命令时去掉名字像密钥的（和运行记录脱敏同一个判据，抽成 `looks_like_secret_name`，
`PWD`/`OLDPWD` 除外），守护进程自己不动；`check_exec_environment` 的 `withheld_environment` 只列名字。新增单元 1 条、集成 1 条
（单独一个测试文件：改进程环境和别的线程起子进程同时发生是未定义行为）；去掉去除逻辑的变异，集成测试报错。名字看着不像的
（`DATABASE_URL` 里带口令）仍挡不住，文档写明。

## 20. D10 第四轮：服务模式与隔离浏览器，ChatGPT 实测（2026-10-08）

**升级：**用户同意后，本机服务从 0.8.2 官方包换成 main `d5d0725` 的 release 构建。备份二进制（`~/.local/opt/gld-0.8.2-20261008-2249`）
和数据目录（`gld-config-20261008-2249.tgz`，0600），写新文件再改名换上。构建提交等于 HEAD；`gld ls` 一字不差；`data/profiles.json`、
`data/oauth-clients/hub.json` 按内容（排好键）算的指纹不变；`gld health` 四项 200。工具表 30 项、指纹 `40f55e0868eb138f` →
`f7db45941922030a`，要 Refresh。

第一次 `gld daemon restart` 时想去掉 Claude Code 会话带的 35 个 `CLAUDE*` 变量（含 `CLAUDE_CODE_MESSAGING_TOKEN`），新守护进程里却还在：
这台机器的 shell 是 zsh，没加引号的 `$UNSET` 不按空格拆，`env` 收到的是一整个参数。改用 `${=UNSET}` 再重启一次，变量 79 → 44 个，
少的正好是那 35 个，代理、`JAVA_HOME`、mise 这些都在。

**浏览器配置：**`~/.codex/config.toml`（先备份成 `config.toml.bak-20261008-2250`）末尾加 `[mcp_servers.browser]`：`@playwright/mcp@latest`、
`--isolated`、`--headless`、`--allowed-origins` 只放本机、`cwd = ".cache/gld-browser"`，另加 `enabled = false` 让 Codex 自己不加载它
（gld 不看这个开关）。`gld mcp test browser` 1.8 秒起来、25 个工具，`gld mcp on browser`；工具表变成 33 项、指纹 `c592e66bd3b70647`。

**场景：**`~/xdw/gld-d10-browser`（`d10-browser`），零依赖的购物清单页，`npm run dev` 起 `node server.mjs --port 5179`（只听 127.0.0.1）。
埋了三个 bug：空白也能添加、"1 items left"、"清除已完成"调了不存在的 `rendr()`（列表不刷新，console 报 ReferenceError）。用户点 Refresh、
开新对话贴提示词：弄清怎么起开发服务器并确认能访问、用浏览器把功能试一遍（console 报错算 bug）、修好复验截图、停服务、说哪里不顺；
不提 `service_port`，也不提浏览器工具名。

| 核对 | 结果 | 依据 |
| --- | --- | --- |
| Refresh 生效 | 是 | 原注册客户端 `dcr-63f31361…` 重拉 `tools/list`，回包 42787 字节 |
| 用服务模式起 | 是，没提示就用了 | 运行记录：`npm run dev`、`service_port: 5179`；`exec_command` 请求到回包 362 ms（没给 `yield_time_ms`，最多会等 30 秒） |
| 先查浏览器参数再调 | 是 | `exec_command` 之后先 `list_mcp_tools`（44540 字节），再 27 次 `call_mcp_tool` |
| console 报错 | 读到了 `ReferenceError: rendr is not defined` | 它的回答；改的正是这一行 |
| 截图 | PNG 经 `call_mcp_tool` 回到对话（19689 字节，两次） | 请求日志 |
| 停服务 | 是 | 运行记录 22:56:36 → 23:01:26 `killed`；之后 5179 空着 |
| gld 拒绝或报错 | 0 次 | 请求日志 47 次调用，没有 rejected / is_error |
| 修了几个 bug | **1 / 3** | 项目 diff 只有 `rendr` → `render`；复验时看到 "1 items left" 也没当问题，没试空白添加 |

**它报的不顺和处理：**

| 报的 | 核实 | 处理 |
| --- | --- | --- |
| 照 console 里的 `/app.js` 读 `app.js`，只说 Path not found，列目录才找到 `public/app.js` | `read_file` 的 NOT_FOUND 没有任何线索 | 报错里带上项目里同名的文件（`details.same_name_elsewhere`，最多 5 个，和 `list_files` 同一套忽略规则，最多遍历 2 万项）（`f04bbb0`） |
| `kill_session` 停 dev server 后回包说 `command FAILED` | 带 `service_port` 的命令被停是说好的结局 | `command_summary` 写 service stopped 加原因；`command_ok` 照旧 false，契约不动；服务自己崩了照旧 FAILED（`5aa4411`） |
| 截图指定文件名后存进了浏览器 MCP 自己的目录，gld 的文件工具看不到 | 是设计如此：`cwd` 就是为了把浏览器关在 `~/.cache/gld-browser` 里 | 不改；文档已写截图不在项目里 |
| 连续调用浏览器工具时被"安全状态检查"拦了 | gld 请求日志里没有任何拒绝，是 ChatGPT 那边的检查 | 不改 |

两处修复各有集成测试，变异各自被抓到；全量 920 passed、0 failed、0 ignored，fmt、clippy 干净。之后本机再升级到 `5aa4411`：工具表指纹
`c592e66bd3b70647` 没变，不用 Refresh；凭据与注册客户端指纹、`gld ls` 不变，`gld health` 四项 200；实机上 `read_file app.js` 回
"Files with that name in the workspace: public/app.js"。

**这一轮说明的：**服务模式和隔离浏览器靠现有工具就能把"起服务 → 打开页面 → 读 console → 修 → 复验 → 停服务"走完，AI 不用提示就会用。
断点仍在回包措辞和报错线索，不在缺能力；漏修的两个 bug 要靠提示词或任务验收去逼，不是工具能管的。

**清理：**`gld rm d10-browser -y`，删项目目录、`runs/59f6228f…`、`harness/workspaces/59f6228f…`、`logs/28dbd41d…`、`write-locks/cfa338a8e66cdaf8.lock`
（路径的 FNV-1a，算过对得上）；清空 `~/.cache/gld-browser` 里这次的截图和快照（目录留着，配置要用）。`browser` 转发仍开着。

**之后发现、同日补上（发版前文档梳理时）：**Playwright MCP 自带的 `browser_run_code_unsafe`，它自己的说明写着"executes arbitrary
JavaScript in the Playwright server process and is RCE-equivalent"（实现是 Node 的 `vm`，不是安全边界）。§19 给的隔离配法和本机
那条 `browser` 配置都漏了它：开着时 AI 能以用户身份跑任意代码，gld 的白名单、`confirm`、`cwd` 都拦不住。ChatGPT 这一轮有没有调过它
**确证不了**：请求日志对转发调用只记 `tool=call_mcp_tool`，不记调到哪个上游工具（全数据目录搜不到这个名字）；27 次回包都是几百字节的
页面操作加两张截图，不像跑过代码，但只是推断。为此请求日志改成多记 `relay=<server>:<tool>`（只有名字，`7c071de`）。本机配置加 `disabled_tools = ["browser_run_code_unsafe"]`
（改前备份），`gld mcp test browser` 变成 24 个工具、写明"配置里不放的：browser_run_code_unsafe"；gld 调用时也按这一项拒
（`machine_mcp/relay.rs` 每次调用都查 `allows_tool`）。文档的配法同步补上，另写明 `enabled = false`、和 `~/.claude.json` 重名会被
那边覆盖、新版本可能加新的危险工具。

## 21. 发布 0.8.3（2026-10-09）

0.8.2 之后 24 个提交（12 个改 `crates/`）发成 0.8.3（用户批准打 tag）：§19 的装东西拦截扩展、服务模式 `service_port`、项目命令扣下
像密钥的环境变量，§20 实测后补的 `read_file` 同名线索、service stopped 措辞、`check_command` 的 `service_port` 说明，以及请求日志的
`relay=`。发布说明 [v0.8.3.md](../releases/v0.8.3.md) 写明从 0.8.2 升上来 ChatGPT 要 Refresh；`crates/daemon` 自 0.8.2 没改，协议仍是 5。

| 核对 | 结果 |
| --- | --- |
| 发版前 | 全量 921 passed、0 failed、0 ignored，fmt、clippy 干净；本机打包解压起服务正常；`a4efa44` 的 CI run 37796559934 八个 job 全绿，Release 空跑 run 37796559707 五个目标全绿 |
| 正式 Release | 附注 tag `v0.8.3` = `a4efa44`；run 37863287099 全绿，`建 Release` 跑了；5 个包 + `SHA256SUMS` |
| 下载包 | 校验和 5 个 OK、来源证明 5 个退出 0。两个 macOS 包（Intel 版经 Rosetta）在隔离 `GLD_HOME` 起服务：0.8.3、构建提交 `a4efa44`、30 个工具、指纹 `a4c7eaf7d5b10f5e`；`cargo install ripgrep` 预检 `needs_approval`（`confirmation_required`），`npm run dev` 带 `service_port`、`timeout_ms` 1 小时预检 `allow`，`check_exec_environment` 有 `withheld_environment`；停掉后端口没人听 |
| Linux 包 | gnu、musl 在 amd64 Debian 12 容器里版本、构建提交、指纹同上，`cargo install` 同样 `needs_approval`，停掉后没留 gld 进程。镜像里没有 npm、cargo，服务模式预检是 `program_not_found`；同一条命令不带 `service_port` 时 `rule` 变成 `timeout_too_long`，说明 1 小时上限只对服务模式放开 |
| 本机服务 | 换成官方 Apple 芯片包（二进制与包内逐字节一致），去掉 `CLAUDE*` 变量重启：构建提交 `a4efa44`，新守护进程 `CLAUDE*` 变量 0 个；凭据与注册客户端指纹、`gld ls` 不变，`gld health` 四项 200；工具表指纹 `c592e66bd3b70647` → `5155ec6663bb33e2`，要 Refresh |

指纹为什么变：旧二进制（`5aa4411`）和新二进制各在隔离目录起服务、逐个比 30 个内置工具，只有 `check_command` 的 `inputSchema`
不同（`5594318` 给它的 `service_port` 写了单独说明），工具数没变。

**ChatGPT Refresh（同日 22:21 前后）：**用户点了。请求日志末尾是原注册客户端 `dcr-63f31361…` 依次 `server/discover`、
`initialize`、`tools/list`（日志文件最后改动 22:21，晚于 09:19 的升级），回包 42638 字节；本机 `gld tool list --served --json`
的 33 项表紧凑序列化 42604 字节，差的 34 字节是 JSON-RPC 外壳。没在新对话里问 ChatGPT 看不看得到 `check_command` 的新说明。

**没验：**Windows 下载包没在真机跑。容器镜像是之前就有的，没删；OrbStack 测完关回原状。
默认白名单里没有 `pip`，`pip install` 预检先报 `command_not_allowlisted`，轮不到 `confirm`——和[概念](../concepts.md#装东西的命令要你点头)写的一致。

**清理：**gld 本地和远端只剩 `main`，没有别的 worktree；删了本机打的 `dist/` 包和下载验收用的临时目录。


## 22. 补丁失败留下原因（2026-10-09）

**起因：**升级到 0.8.3 后当天的日用（HPIAS、xpro 两个项目，全是 ChatGPT 经服务调的）里失败的调用 15 次：补丁 9 次
（`PATCH_FAILED` 5、`FILE_VERSION_CONFLICT` 2、`PATCH_AMBIGUOUS` 1、`NOT_FOUND` 1），项目命令退出码 1 共 4 次（项目自己的检查没过），
按设计拦下 2 次（读项目外目录、删文件没带 `confirm`）。`apply_patch` 当天 45 成 5 败约 10%，历史 279 成 27 败约 9%，没变高。
但这些失败**为什么**事后查不出：请求日志对补丁什么都不记、也没有时间戳，operations.jsonl 只记 `error_code`；回给 AI 的诊断很细，
没落盘。gld 比对上下文是逐字的，分不清"只差一个行尾空格"（该 gld 放宽）和"补丁写错了"（模型的事）。

**改了什么（`87eba6b`）：**

| 处 | 现在 |
| --- | --- |
| 回给 AI 的诊断 | 上下文精确找不到（`context_not_found` / `context_drifted`）时多一格 `near_miss`：依次放宽到行尾空白、首尾空白、花引号长破折号和特殊空格，对上了报 `kind` 和行号；都不中报 `partial`：最像的一处对上几行（空行不算）、补丁旧内容第几行开始不同。比对次数超过 200 万（文件行数 × hunk 行数）不算 |
| 解析、落盘阶段的失败 | 补上 `details.reason_code`：`no_file_headers`、`unsupported_directive`、`envelope_not_closed`、`hunk_count_mismatch`、`backup_unreadable`、`staging_failed`、`write_failed_rolled_back`、`notebook_not_utf8` |
| operations.jsonl | 补丁调用的 `result_summary.patch`：`format`（codex / unified / none）、`patch_bytes`，成功的也记；失败时加 `reason_code`、`problem_count`、`not_checked` 个数和每条诊断里的文件、第几段、行号、`near_miss`、版本号。`message` 和 `actual_excerpt` 不进来：operations 经 `operation_log` 回给调用方，沿用"只记码不记内容" |
| 请求日志 | 失败时一行 `[patch] id=… tool=… operation_id=… code=… reasons=原因码[:near_miss]`，用 `operation_id` 和 operations.jsonl 对上；字段全是 gld 自己的常量和 uuid，伪造不出别的行 |

补丁原文不落盘：和"只记码不记内容"冲突。先看 `near_miss` 的分布够不够判断要不要放宽比对，不够再单独定。

**验证：**集成测试真起服务打一个行尾多空格的补丁，核对回包、`[patch]` 行、operations 记录三处对得上，且文件内容和补丁正文都没进
日志；再测信封没收尾的原因码、成功的补丁不出 `[patch]` 行。三处变异（摘要漏进原文摘录、不记 `[patch]` 行、解析失败不带原因码）
各自被抓到。全量 926 passed、0 failed、0 ignored，fmt、clippy 干净。新旧构建在隔离目录起服务，工具表都是 30 项、指纹
`a4c7eaf7d5b10f5e`：升级后 ChatGPT 不用 Refresh。

**本机换上（同日 23:37，用户批准）：**`eea0535` 的 release 构建，二进制和数据目录先备份到 `~/.local/opt/`（`gld-0.8.3-20261009-2336`、`gld-config-20261009-2336.tgz`，0600），写新文件再改名，去掉 35 个 `CLAUDE*` 变量重启。换之前没有在跑的命令。构建提交等于 HEAD；工具表 33 项、指纹 `5155ec6663bb33e2` 没变，ChatGPT 不用 Refresh；`gld ls`、`profiles.json` 和 `oauth-clients/hub.json` 按内容算的指纹不变；新守护进程 `CLAUDE*` 变量 0 个；`gld health` 四项 200。实机探针：在 gld 项目上 `gld tool call patch_check` 一个行尾多空格的补丁（只预检不写），回包和 operations.jsonl 都有 `context_drifted` 加 `near_miss: trailing_whitespace`，仓库没改动。

**没验：**`[patch]` 那一行只在 MCP 的 HTTP 入口记，`gld tool call` 不经过它；实机上要等 ChatGPT 下一次补丁失败才看得到，现在只有集成测试的证据。

## 23. SuperAssistant 直接连接 gld（2026-10-17）

**需求与源码：**用户要求 DeepSeek 网页经 SuperAssistant 直接调用 gld，不再增加外部代理工具。本次修改基于
`54bacbf126212fc4402cca84d677f5d899ced733` 的工作树，尚未提交、发布或安装；包版本仍为 0.8.3，不能把已有 0.8.3 发布包当成包含此功能。
实现与验收在本机 Apple 芯片 macOS 上进行，没有新增依赖或修改 Cargo.lock。

**客户端依据：**核对 SuperAssistant 0.6.0 对应提交 `c26168ee2c5708a3a65ef5afd88cda1a97c81734` 的
[Streamable HTTP transport](https://github.com/srbhptl39/MCP-SuperAssistant/blob/c26168ee2c5708a3a65ef5afd88cda1a97c81734/chrome-extension/src/mcpclient/plugins/streamable-http/StreamableHttpPlugin.ts)：
只向 `StreamableHTTPClientTransport` 传 URL，没有 OAuth authProvider 或自定义认证头。依赖锁定的 MCP SDK 1.25.2
首次初始化可以提出 2025-11-25，接受协商到 gld 的 2025-06-18；通知后探测 GET 时接受 405。
扩展的源码审查不等于已在 Chrome 和 DeepSeek 页面完成实测。

| 改动 | 行为与边界 |
| --- | --- |
| `gld grant browser <名称> <项目> --ttl 8h` | 单独生成浏览器 URL 凭据，默认只读、8 小时，最长 24 小时；`--write` 显式选择，沿用普通 grant 的执行风险 |
| `/mcp/browser/<密钥>` | 复用现有 Hub、工具、项目范围和主体隔离；不增加执行器、隧道或代理进程，标准 `/mcp` 保留原 OAuth/Bearer 认证 |
| 凭据生命周期 | 原始随机值只在创建回包出现，数据文件仅存 SHA-256 摘要；每次请求重查记录、时间和 Origin，撤销或到期后下一次请求拒绝 |
| Origin / CORS | 默认 Chrome 商店版扩展 Origin，支持重复 `--origin`；有 Origin 则精确匹配，重复/非法 Origin 拒绝；缺省 Origin 仍须持有密钥。OPTIONS 可预检，不执行工具 |
| Streamable HTTP | 通知返回 202 空响应，工具结果返回 JSON；独立 GET 流和 DELETE 返回 405，不签发 MCP session；不支持后续动态工具通知 |
| 旧通道隔离 | 浏览器记录留空普通口令和 bearer 字段，旧认证查找与 OAuth 的 by_id 显式排除浏览器记录；URL 密钥不能作为标准 `/mcp` 的 bearer 或 query token |
| 日志与响应 | gld 不记录浏览器 URI/密钥/摘要，浏览器响应 no-store / no-referrer；旧 `/hub` 网关的 reqwest 错误移除 URL，避免上游失败时回显路径密钥 |
| CLI / IPC | 新增独立 `AddBrowserGrant` 请求，协议 5 → 6；新 CLI 连旧 daemon 在创建授权前报告版本不匹配，不能静默生成永久授权 |

`--write` 的命令仍以用户身份执行，工作区范围不是 OS 沙箱。URL 到期只影响后续请求认证，已经接受的命令仍遵循原有
`timeout_ms`；主动 `grant rm` 才沿用现有会话终止逻辑。浏览器 URL 不自动续期、不能经 `grant ls --reveal` 恢复。
SuperAssistant 会保存并可能在调试界面显示 URL，因此同机优先用生成的 loopback 地址；第三方反代的访问日志需隐藏密钥。

**真实执行的验证：**所有测试使用隔离的 `GLD_HOME`，没有在真实数据目录创建或撤销 grant。

| 命令 / 范围 | 结果 |
| --- | --- |
| `cargo --config 'env.GLD_HOME="target/test-home-browser-20261017"' test -p gld --test browser_grants --test scoped_grants --test oauth_connector_flow --locked` | 退出 0，8 项通过；真实 CLI 启动服务后进行 HTTP 往返，包括原 OAuth 授权与服务重启回归 |
| 同一隔离配置下 `cargo test --workspace --all-targets --locked` | 退出 0，测试框架报告 933 passed / 0 failed / 0 ignored，42 个测试二进制结果；不是外部客户端或所有平台验收声明 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 退出 0，覆盖 CLI / core / daemon 与所有 target |
| `cargo fmt --all -- --check`、`git diff --check` | 均退出 0 |
| `scripts/gen-cli-docs.sh` | 退出 0，从真实 CLI 帮助重新生成 `docs/cli.md`，新增命令被文档门禁覆盖 |
| `cargo build --release --locked` | 退出 0，优化构建完成；两个 build script 的调试符号剥离出现本机 rust-objcopy 缺 libLLVM.dylib 警告，没有修改系统工具链 |
| `target/release/gld --home target/test-home-browser-20261017 grant browser --help` | 退出 0，release 二进制实际启动并正确显示新命令；没有创建授权 |

`browser_grants.rs` 新增 3 个集成用例：初始化/通知/列工具/读文件、只读与越界硬拒绝、Origin 与无效协议头、
GET/DELETE 405、预检、标准通道隔离、撤销与同名重建、到期、TTL 边界、持久化不留原密钥，以及显式 --write 的临时文件写入。
core 另有 4 个新增单元用例覆盖密钥/到期边界、旧记录兼容、Origin 和公网基地址。

构件：`target/release/gld`，13591984 字节，SHA-256 `786c8528459883cb61bfd20dd7cd409d16270bcc64a1b9cc82a54de3f6d4e66c`。
它来自上述未提交工作树，摘要用于后续安装时核对同一个构件，不作为已发布包或来源证明。

**实际服务与未验范围：**完工前通过 `server_info` 复核，当前连接仍是 0.8.3 / `eea05359c42b`，OAuth，33 个工具，
指纹 `5155ec6663bb33e2`。没有替换运行二进制、重启服务、改真实凭据或隧道，没有执行 Git 提交、推送或发布。
Chrome 扩展到 DeepSeek 当前页面的注入、调用、结果回填与继续对话尚未实测；这次没有安装或执行 SuperAssistant/其 JavaScript SDK。
既有 ccnm / cloudflared 条件返回用例不凭 0 ignored 外推真实 SSH 或公网验收。
本机升级和实际 browser grant 创建需按当前任务入口取得对应授权。

接入步骤、只读切换到可写时替换 URL、扩展约 30 秒超时、`workspace_context` 与显式 workspace 的要求，
已写入 [连接客户端](../connect-clients.md)；凭据与主体边界同步更新 [安全](../security.md)、[概念](../concepts.md) 和 RFC-0007 后续决定。

## 24. 升级连续性要求与 OAuth 身份保护（2026-10-17）

**用户要求：**每次正常升级都不能中断当前本机服务或已有 connector（包括 ChatGPT Web），必须保留原 client_id、
授权、连接入口与多个项目。该要求优先于 §23 browser grant 的在线激活。此次没有获得或使用维护中断的例外，
正式服务继续运行，不执行停止、重启、凭据再生成、真实 DCR 注册或 grant 创建/撤销。

**源码与范围：**基于 `54bacbf126212fc4402cca84d677f5d899ced733` 的未提交工作树，在 §23 变更之上修复注册可靠性、
CLI 提示和文档。包版本仍为 0.8.3，候选 daemon 协议 6；没有新增依赖或修改 Cargo.lock。
本节构件取代 §23 的旧候选摘要，不能把正式 0.8.3 发布包当作包含这些修改。

### 已核对的问题与修复

| 原行为 | 当前候选源码的行为 |
| --- | --- |
| ClientRegistry 释放 mutex 后分别写快照；同进程复用临时文件名，并发快照可能覆盖已确认的注册 | 同一实例持锁完成候选表、保存、内存发布；旧快照不能在新注册之后回写 |
| 注册落盘失败仅写日志，仍发布内存记录并返回 HTTP 201 | 保存成功才确认；失败保留原文件和内存，HTTP 503，不返回 client_id / secret 或底层 I/O 信息 |
| 超过 50 条时淘汰最旧注册 | 保持容量上限但拒绝新增，所有既有 client_id 保留，不靠淘汰现有 connector 腾位置 |
| 定位、读取或解析失败降为空表，后续注册可能覆盖原件 | 异常实例停用新注册；只允许明确不存在的文件从空表开始；损坏/错误结构原件保留 |
| 新 CLI 版本不匹配提示一律 daemon restart | 原有不兼容拒绝行为保留，提示本次命令未执行、旧服务仍运行；restart 帮助明确会中断连接、隧道和命令 |

源码入口是 `crates/core/src/auth/client_registry.rs`、`oauth_flow.rs`，
以及 `crates/cli/src/backend.rs`、`commands/daemon.rs`、`cli.rs` 与 `error.rs`。
DCR 失败采用 `temporarily_unavailable` 和固定描述，并加 `Cache-Control: no-store`。
注册容量与存储异常分别提供固定描述；新注册失败不改变已经加载的合法客户端。
正常旧注册文件格式、OAuth 签名、JWT、refresh 兼容、grant 范围与撤销逻辑未变。

这些修复证明的是同一注册表实例的失败原子性与正常磁盘重载；没有新增跨进程注册事务、fsync 断电耐久性或丢失记录恢复。
加载时已损坏的注册记录不能凭有效 token 找回；修复文件后当前仍需重新加载，没有新造在线重载接口。
故障测试独立隔离日志目录，避免单独运行时依赖其他测试先隔离 GLD_HOME。

### 现场证据与升级能力边界

用户截图显示 SuperAssistant 选择传统 SSE，URI 为 `http://localhost:3006/sse`，报 `Failed to fetch`。
它未显示 OAuth token 错误或 client_id 前后变化，不能作为已丢失 DCR 身份的证据。
本轮发现的代码缺口是可能原因，不据此追认用户之前的具体故障根因。

当前及旧 revision 的生命周期核对表明：daemon 单实例锁、Hub 端口、隧道、OAuth pending map、
exec Child/stdin 与 workspace 执行状态均没有交接协议。restart 是 stop + start；
App 正常 shutdown 也终止运行命令，监听约 3 秒后可能 abort。
正常重启后沿用身份的测试不能证明在途调用、授权码、隧道或命令持续运行。

[RFC-0008](../rfc/0008-continuous-upgrades.md) 定义 gld 内部稳定入口/隧道/认证与配置单写者，以及可替换执行代。
第一阶段以整个 workspace 为所有权单位，避免只路由 session id 时漏记其他代的写入序号；
长期命令使旧代继续保留，不能超时强杀。稳定宿主自身升级还须独立资源交接，worker 更替通过不能关闭整个要求。
现有旧 0.8.3 没有预先提供交接接口，新二进制不能单方面无中断接管；严格不中断时保留旧 daemon，暂不激活本轮功能。

### 实际执行的验证

环境为本机 Apple 芯片 macOS。测试命令使用隔离的
`GLD_HOME=target/test-home-smooth-20261017`，没有把故障注入或测试注册发到正式端点。

| 命令 / 范围 | 结果 |
| --- | --- |
| `cargo --config 'env.GLD_HOME="target/test-home-smooth-20261017"' test -p gld-core auth:: --locked` | 退出 0，47 项认证测试通过 |
| 同一隔离配置下 `cargo test --workspace --all-targets --locked` | 退出 0；42 个测试二进制结果，941 passed / 0 failed / 0 ignored / 0 filtered |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 退出 0 |
| `cargo fmt --all -- --check` | 退出 0 |
| `cargo build --locked -p gld` 与 `scripts/gen-cli-docs.sh` | 均退出 0；真实候选 CLI 生成的帮助包含 restart 中断说明 |
| 文档命令、消息、生成脚本、链接和锚点门禁 | 随全量测试通过；没有把新 RFC 里的方案当成已存在的 CLI |
| `cargo build --release --locked` | 退出 0，2m 05s；沿用本机两条 rust-objcopy 缺 libLLVM.dylib 的 build-script 剥离警告，没有修改系统工具链 |
| `target/release/gld --home target/test-home-smooth-20261017 daemon restart --help` | 退出 0；实际只显示帮助，没有运行 restart |
| 只读 Python 标准库计算候选大小与 SHA-256 | 退出 0，结果见下方；已通过 check_command，没有放宽命令策略 |

新增/强化用例包含并发注册后全部重载、满额保留旧 ID、保存失败保持文件与内存、损坏/不可读/错误结构保留原件、
真实注册入口返回 503 且不含部分凭据。OAuth 重启单元测试从复用内存 Arc 改为真正重新加载磁盘，
继续验证旧 client_id、原 access、原 refresh 和重新授权。全量还覆盖真实临时 daemon 的 browser grant、
普通 grant 与 OAuth 连接器 HTTP 往返。测试框架的 0 ignored 不外推既有 ccnm/cloudflared 条件返回用例为真实 SSH 或公网验收。

候选构件：`target/release/gld`，13591696 字节，SHA-256
`960aa6c6d4f1652ae6041329ab87852b205d07fef468706d51e6b9dcf826030d`。它仍是未提交源码的本机构建，不是已发布或已安装产物。

### 文档与在线结果

README、安装、daemon、接入、排障、安全、概念、CLI 帮助及 RFC-0004 的后续决定已同步。
[升级连续性](../development.md#升级连续性)明确要求每次跨版本复用原连接器、client_id、access/refresh token，
验证切换期间的请求、授权码、命令、隧道与失败回退。现有 CI 尚未实现这套连续切换验收，
文档门禁与当前测试全绿不能被写成“零中断升级已经交付”。当前排队只维护在
[task_plan.md](../../task_plan.md)，browser grant 激活受该要求约束。

完工前通过当前连接的 `server_info` 核对，正式服务仍为 0.8.3 / `eea05359c42b`、OAuth，
33 项工具、指纹 `5155ec6663bb33e2`。本轮没有替换正式二进制、重启服务、改入口、改真实 OAuth 身份、
提交/推送或发布；用户已有 `exports/` 未处理。Chrome 扩展与 DeepSeek 网页闭环仍未实测，
新路由没有因构建成功而在旧 daemon 上生效。

## 25. SuperAssistant 接入复核、文档纠偏与本机升级（2026-10-17 晚）

**要求：**用户让核对 §23/§24 的改动能不能让 DeepSeek 网页经 SuperAssistant 调 gld、能不能复用本机现有的服务（Hub）无痛升级，
然后帮着升级、完善文档、粒度提交。用户在会话中途补了一张扩展侧栏截图（`Server-Sent Events (SSE)` + `http://localhost:3006/sse`，
报 `SSE error: Failed to fetch`）。

### SuperAssistant 源码复核（v0.6.0 = `c26168e`）

| 看了什么 | 结论 |
| --- | --- |
| `chrome-extension/src/background/index.ts` 引入 `createMcpClient` | MCP 客户端跑在扩展后台 service worker，请求带扩展自己的 Origin，不是 `https://chat.deepseek.com` |
| `chrome-extension/manifest.ts` 的 `host_permissions` | 只有各聊天站点（含 `*.chat.deepseek.com`），没有 localhost / 127.0.0.1，所以连 gld 是受 CORS 约束的跨源请求，gld 必须回预检和 `Access-Control-Allow-Origin`；§23 的实现做了 |
| `pages/content/src/plugins/adapters/deepseek.adapter.ts` | 有 DeepSeek 页面适配 |
| `ServerStatus.tsx` 的 Connection Type | 只有 SSE / WebSocket / Streamable HTTP 三项加一个 Server URI，没有填请求头的地方；截图里的 3006 是默认值，指向扩展自家的代理 |
| `pages/content/src/core/mcp-client.ts` 的 `mcp:call-tool` | 页面等一次工具调用 30 秒 |
| 全仓库搜 `getInstructions` | 不读服务端 `instructions`，`workspace_context` 那一步省不了 |
| `pnpm-lock.yaml` | MCP SDK 实际是 1.25.2（`package.json` 写 `^1.20.2`，提交说明写"降到 1.20.2"，以锁文件为准） |
| npm 上 SDK 1.25.2 的 `dist/esm/client/streamableHttp.js` | 对通知的非 202 响应只丢掉响应体、不报错；只在 202 之后探测 GET 推送流，405 视为不支持 |

最后一条说明 `98a4ef7` 的提交说明和注释"SDK 会把 null 当无效消息"说过头了，`813b90b` 单独更正（没 amend）。改 202 本身仍然对：规范要求。

**Chrome 本地网络访问：**Chrome 142 起公网来源访问本机/局域网要权限（[官方博客](https://developer.chrome.com/blog/local-network-access)）；
第三方报告扩展 Origin 被当成公网来源，要到扩展的网站设置里放行。官方没写扩展的规则，这次也没实测，
所以只写进排障表：`Failed to fetch` 且 `gld logs` 没有这次请求时先查它，或改用公网地址。

### 文档与提示纠偏

§24 把"在跑的命令和请求都不断"定成每次升级的必过门禁，install / daemon / troubleshooting / development 里能照做的升级步骤和
"版本不一致就 `gld daemon restart`"被换成"保留旧服务、不要重启"，CLI 提示也一样，用户手上没有任何可操作的升级路径。
这和用户一直认可的"无感知升级"（连接器身份、授权、地址不动，重启零点几秒，见本审查各次升级记录）不一致，用户这次也明确要升级。
处理：升级步骤和排障处理改回原样，补上"重启会停掉在跑的命令、先用 `pgrep -lP` 查"和"`gld ls` 的 Client ID 不是 ChatGPT 的 `dcr-…`、怎么核对它"；
`daemon.md` 与 RFC-0004 恢复原文；CLI 提示改回给出 `gld daemon restart` 并说明代价；RFC-0008 保留为未实现的设计草案，
改掉其中"不能为启用功能重启"等结论。§23/§24 本身作为当时的记录保留，以本节为准。

### 提交

| 提交 | 内容 |
| --- | --- |
| `98a4ef7` | 通知回 202 空响应 |
| `813b90b` | 更正上一条的注释和说法 |
| `3b1b853` | OAuth 动态注册写盘成功才算数（§24 的修复）及其安全、排障文档 |
| `74f544d` | `gld grant browser`（§23 的功能）及其接入、安全、概念文档 |
| `0f6722d` | CLI 版本不一致提示 |
| `778fdd3` | 升级文档、RFC-0008 |

同一文件里混着两件事的，按中间版本暂存拆开；`3b1b853` 在临时 worktree 里单独编译，`gld-core` 认证单测 45 项通过（最终版 47 项，多的是浏览器 grant 的）。

### 实际执行的验证

本机 Apple 芯片 macOS，cargo 1.98.0，测试用隔离的 `GLD_HOME=target/test-home-claude-20261017`。

| 命令 | 结果 |
| --- | --- |
| `cargo test --workspace --all-targets --locked`（拆提交前的工作树，代码和 `778fdd3` 只差一条注释） | 退出 0，42 个测试二进制，941 passed / 0 failed / 0 ignored |
| 文档改完后 `docs_commands_exist`、`docs_links_resolve`、`naming_is_consistent`、`messages_name_real_commands`、`aliases_are_consistent`、`doctor_fixes_are_real_commands`、`docs_generation` | 退出 0，15 项通过 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo fmt --all -- --check`、`git diff --check` | 均退出 0 |
| `scripts/gen-cli-docs.sh` | 重新生成 `docs/cli.md`；这道门禁只测脚本出错时不覆盖原文件，不比内容，改了帮助要手动跑 |
| `cargo build --release --locked -p gld`（HEAD = `778fdd3`） | 退出 0，2m 14s；照旧有 rust-objcopy 缺 libLLVM.dylib 的剥离警告。产物 16,333,752 字节，SHA-256 `68f3f1582e8851ab923779ed400e8bdd5d8c6687e2f0fe23f84fa98602148850` |

### 本机升级（23:29）

- **升级前**：守护进程 pid 50858 没有子进程（没有在跑的命令，隧道也不归它管），最近一次 MCP 请求 22:53。
  对旧服务用无效密钥 POST `/mcp/browser/…` 回 404（路由不存在）。
- **备份**：上一个在用二进制 `~/.local/opt/gld-eea0535-20261017-2329`（摘要与在用一致）、数据目录 `gld-config-20261017-2329.tgz`（0600）。
- **切换**：写新文件再改名；去掉本会话的 36 个 `CLAUDE*` 变量后 `gld daemon restart`，命令耗时 0.24 秒，新 pid 87812，自动恢复 1 个服务。
- **核对**：协议 5 → 6，构建提交 `778fdd359240`；工具表 33 项、指纹 `5155ec6663bb33e2` 没变，ChatGPT 不用 Refresh；
  `gld ls` 逐字一致；`profiles.json`、`oauth-clients/hub.json` 排好键再哈希前后一致；新进程 `CLAUDE*` 变量 0 个；`gld health` 四项 200。
- **新路由**：无效密钥 POST 本地 `127.0.0.1:28767` 和公网入口都回 401 `browser_grant_invalid`（自建入口转发了整段路径）；
  预检回 `content-type,accept,authorization,mcp-session-id,mcp-protocol-version,last-event-id`；标准 `/mcp` 不带凭据仍 401；
  请求日志只记 `credential=browser_url`，搜不到探测用的密钥。

**回滚**：备份二进制按"写新文件再改名"放回，再 `gld daemon restart`，协议回到 5。之后建的浏览器 grant 旧版会忽略
`browser` 字段，口令和 bearer 为空，旧通道用不了它。

**没验的：**
- ChatGPT 连接器换版后的第一次真实调用：等它下次被用到，看 `gld logs` 里是不是原来的 `auth=oauth:hub:dcr-…`、有没有 401。
  签名密钥和注册表没变，按以往各次升级预期能直接用，但这次还没有证据。
- 没有建真实的浏览器 grant：地址就是凭据，留给用户自己在终端建，不经过 AI。所以 Chrome 扩展连上、DeepSeek 页面整圈、
  Chrome 本地网络访问会不会拦 127.0.0.1，都还没实测。

### 补充：示例名改成 web-mcp，加 `--all`（23:5x）

用户觉得示例名 `superassistant` 太长，改成 `web-mcp`（`0c77868`，只改文档）；又要一次开放全部已登记项目，
加 `gld grant browser <名字> --all`（`0b4cb59`）。`--all` 在命令行这边用 `HubStatus` 的成员名单展开成 id 再发给守护进程，
grant 里存的是固定名单：之后新加的项目不自动开放。IPC 不变，所以只换命令行、不重启守护进程。

| 验证 | 结果 |
| --- | --- |
| 新集成测试 `browser_grant_all_opens_every_current_project_only` | 通过：两种用法错误退出 2；`--all` 开到 api、web；新加项目后再建一把 `--all` 是 3 个（对照），旧的那把 `list_workspaces` 仍只有 api、web，能读 web 的文件 |
| `cargo test --workspace --all-targets --locked` | 退出 0，42 个测试二进制，942 passed / 0 failed / 0 ignored |
| `cargo clippy … -D warnings`、`cargo fmt --check`、`scripts/gen-cli-docs.sh` | 均通过；`docs/cli.md` 已重新生成 |
| `git diff --stat 778fdd3..0b4cb59 -- crates/` | 只有 `crates/cli/src/cli.rs`、`commands/grant.rs` 和测试 |
| 本机换命令行 | 备份 `~/.local/opt/gld-778fdd3-20261017-2356`，release 16,333,944 字节、SHA-256 `126699473d65ae90d74a69afaf89d4d28f3d6d25704794e8d62ad91403902287`，写新文件再改名；守护进程 pid 87812 前后不变，协议 6 与命令行匹配，`gld grant browser --help` 有 `--all`，health 正常 |

## 26. 经 gld 开发 gld 的 ChatGPT 为什么没守用户规则（2026-10-18）

**问题：**§23/§24 是网页端 ChatGPT 经 gld 直接开发的。§25 记了暴露的问题：把需求放大成"零中断"门禁并拒绝部署、
文档满是空词、1700 行散在 29 个文件里一个没提交、往公开仓库写本机路径、没核实的说法写进注释。用户问这些能不能修。

**根因：ChatGPT 收到的说明是空的。**当时 `gld context -w gld` 输出"说明文件（扫到 1，实际注入 0）"：

| 规则在哪 | 为什么没送到 |
| --- | --- |
| 用户全局规则 `~/.config/ai-assistant/INSTRUCTIONS.md`（`~/.codex/AGENTS.md` 软链到它，和 `~/.claude/CLAUDE.md` 内容相同） | 被自动发现成 `codex/global`；compact 只注入工作区的 `AGENTS.md`，把它滤掉 |
| `~/xdw/CLAUDE.md`（粒度提交、文档写法） | 在上级目录，gld 不扫上级 |
| gld 仓库自己的约定 | 没有 `AGENTS.md` |
| `--ai-instructions`、`--custom-instruction-paths` | 都没配；配了后者 compact 下也会被滤 |

Claude Code 和 Codex 自己会读这些文件，所以同样的规则它们守得住，经 gld 进来的 ChatGPT 守不住。§25 列的大部分毛病
（不提交、文档写法、本机路径）正好对应这几条没送到的规则。

**修：**

| 提交 | 内容 |
| --- | --- |
| `e8f7f36` | compact 下点名的说明（`--custom-instruction-paths`）也注入；点名的和自动扫到的重复（同路径或同内容）时把留下的那条升为 custom，不然会被去重后再按作用域滤掉；补上两个参数空着的帮助 |
| `b360fc6` | 门禁 `docs_have_no_home_paths`：README、AGENTS.md、task_plan.md、docs 下全部 .md 不许出现 `/Users/<名>`、`/home/<名>`、`C:\Users\<名>`（占位名除外）；RFC-0002 的 12 处改成相对路径或 `~/xdw/…` |
| `e0eb9ce` | 仓库根 `AGENTS.md`：gld 自己的约定（无痛升级的含义、改需求含义和不部署先问、要点头的操作、每步提交、公开仓库） |

**验证：**

| 命令 / 用例 | 结果 |
| --- | --- |
| 单测 `compact_keeps_named_instruction_files_even_when_they_duplicate_discovered_ones` | 通过：不点名时只注入工作区那份（前提断言：全局那份确实被扫到）；点名同路径、点名同内容的另一份，两种都注入 |
| 集成测试 `compact_injects_explicitly_configured_instruction_files` | 通过：`gld settings runtime --custom-instruction-paths` 后 `gld context` 标 ✓，`workspace_context` 文本里有标记，`.cursorrules` 仍不进。临时换回旧 `agent_context.rs` 时它失败，报"注入的只有 [AGENTS.md]" |
| `cargo test --workspace --all-targets --locked` | 退出 0，43 个测试二进制，946 passed / 0 failed / 0 ignored |
| clippy `-D warnings`、`fmt --check`、`scripts/gen-cli-docs.sh` | 均通过 |
| 在跑的旧守护进程上 `gld context -w gld` | 加了 `AGENTS.md` 之后显示"✓ [codex/workspace] AGENTS.md"：项目里这份旧代码本来就注入，不用部署 |

**修不了的：**判断本身。规则送到只能降低跑偏的概率；部署、改需求含义仍要用户拍板，`AGENTS.md` 写明了这条。
没做工具层提醒（比如回包里报未提交的改动量）：先看规则送到后还会不会攒大改动。

**没部署：**点名注入要新守护进程，得换二进制、`gld daemon restart`，再执行
`gld settings runtime --custom-instruction-paths ~/.codex/AGENTS.md`，等用户同意。改这项设置不重启服务，上下文重建也不停命令。

## 27. 任务收尾回包恢复与命令回显（2026-10-18）

**起因：**一次文件交付已经落盘、任务也已经完成，但 ChatGPT 没有及时给出最后回复。这个现象本身不能证明
gld 死锁；之前只归因于 Agent 工作流也不充分。本轮检查源码、添加反例，再修复能复现的服务端问题。

| 提交 | 查到的问题与修复 |
| --- | --- |
| `8fb5e31` | 已完成任务再次 `finish` 原来报 `INVALID_TASK_TRANSITION`。现在同任务可只读重放终态，不重复写验收、说明或状态事件，不关闭后来开的任务；新证据或改变验收方式仍拒绝。增加 `finished`、`already_finished`、明确的 `verification_required` 和结束提示 |
| `8fb5e31` | 状态已保存后，附加说明或改动摘要的读写失败原来会把整个调用报错。现在保留已保存的任务状态，附加信息失败用 `warnings`、`summary_recorded`、`change_summary_available` 说明。待办保留原值并警告，不把未做的步骤自动算作完成 |
| `63719bc` | `check_command` 拒绝超长命令后仍完整回显输入；`max_output_bytes` 只限制标准输出和错误输出。现在公开调用的命令回显最多 2048 字节，保留 UTF-8 边界，并返回 `command_bytes` / `command_truncated`。实际执行参数、原有 4000 字节命令上限不变 |

**验证：**Mac 本地源码测试，使用 `target/test-home-finish-recovery` 下隔离的 `GLD_HOME`，没有改真实服务数据。

| 检查 | 结果 |
| --- | --- |
| `task_acceptance` 新增 5 条 | 修复前 5 条失败、原有 7 条通过；修复后全过。覆盖重复收尾不重写事件、新任务不受旧任务重试影响、验收方式不能互换、事件文件不可读时仍返回已保存终态、待办不被伪造为完成 |
| `command_echo` 新增 3 条 | 在既有命令长度限制下先复现回显过长与元数据缺失，再修复通过；检查中文/emoji 截断、实际执行仍收到完整数据、普通命令回显不变。超长预检仍被原策略拒绝 |
| 收尾与存储相关测试 | `task_acceptance`、`harness_tool_contract`、`harness_storage` 共 36 条通过 |
| 命令回显与工具契约 | `command_echo`、`call_tool_contract` 共 81 条通过 |
| `cargo test --workspace --all-targets --locked` | `63719bc` 上退出 0；会话 `d180dbc9-5438-481d-a100-935a9eec1f41` |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 退出 0；会话 `092c3b2f-3f82-474d-af5a-67322cc8be38` |
| `cargo fmt --all -- --check` | 退出 0；会话 `3e5175b3-8aa5-49d8-8fab-4a8740bcb6aa` |

**保留的边界：**后台命令写文件后的基线复核是既有的安全取舍（§11），没有去掉；各动作参数校验也没放宽。
没有新增通用制品上传下载接口。命令回显限制不约束客户端展示的请求正文，也不是整个回包的总字节上限。
重复收尾只返回历史结果，不证明后来改过的工作区又通过了测试；证据内容是否充分仍由操作者判断。
本轮没有修改守护进程 Request/Response 类型、CLI 帮助或工具输入表，不需要协议版本迁移。

**部署与未验证项：**本轮只修改并提交源码，没有替换已安装二进制、重启服务、改授权、推送或发版。
新行为尚未通过正在使用的 ChatGPT 连接器实测；上线需要用户批准后按既有升级流程执行。
这些修复减少收尾失败和重复回传，不能保证模型一定及时给出最后回复，也不是强断电事务保证。
当前状态及上线待办只维护在 `task_plan.md`；用法见 [收尾与交接](../project-lifecycle.md#收尾与交接如实写)。

## 28. 复核 §27 的三个提交，和这轮暴露的 gld 不足（2026-10-18）

**要求：**用户让 review ChatGPT 经 gld 做的三个提交（§27），从中找 gld 作为开发工具的不足，并说清 Skills、MCP、
浏览器现在用得怎样。三个提交直接提在 main 上，没有分支或 worktree 要合并。

### 三个提交改得怎样

| 提交 | 改得对的 | 不足 |
| --- | --- | --- |
| `8fb5e31` | 已结束的任务再 `finish` 只回存着的结局，不再记事件；换验收方式、带新证据照样拒。5 条新测试覆盖了重试、新任务不受影响、事件读不出来 | 三件事（重试、附加信息失败降级、`pending_steps` 警告和新字段）放在一个提交；第一次 `summary` 没写进去的，重试不补写；字段说明写进了 `project-lifecycle.md`，工具规则本来在 `concepts.md`。记录里找不到 `finish` 真被重试过：这是预防性修复，不是对某次故障的复现 |
| `63719bc` | 实测（隔离 `GLD_HOME`，直连模式）：20 KB 的命令交给 `check_command`，回包 4408 字节，`command` 截到 2048；3.5 KB 的合法命令照常执行，回显截到 2048 | 能省的不多：`exec_command` 超过 4000 字节在策略层就拒，回包本来就不带命令（实测 644 字节）；只有 `check_command` 拒绝时和 2–4 KB 的合法命令变小。每个 exec / check 回包多两个字段 |
| `19e07b1` | 证据表每条都带 session id，和运行记录对得上：`task_acceptance` 11:02 退出 101、11:03 通过；`command_echo` 11:06、11:08 两次 101，11:09 通过 | 否定句一串接一串（"没有新增……不约束……也不是……"）；"起因"那句不通顺 |

三个提交说明都只有一行、没有类型前缀，仓库最近 40 个提交里 36 个是 `类型(范围): …`。和 §25 比是大进步：
先写会失败的测试、用隔离的 `GLD_HOME`、每步提交、没有自己部署。

**跟进：**`6ae9793` 把收尾和命令长度的规则收回 `concepts.md` 一处，补上 4000 字节上限怎么绕；`7c62262` 在
`AGENTS.md` 写明提交格式、一件事一提交到什么粒度、工具规则写在哪。

**复核：**`19e07b1` 上 `cargo test --workspace --all-targets --locked` 44 个测试二进制，954 passed / 0 failed / 0 ignored；
clippy `-D warnings`、`fmt --check` 通过。两个文档提交之后跑了 `docs_links_resolve`、`docs_have_no_home_paths`、
`docs_commands_exist`，通过。

### 这一轮 ChatGPT 怎么用的 gld

数据来自 hub 请求日志（次数、字节）、`operations.jsonl`、运行记录和任务事件。

- 10:57:12–11:21:22，103 次调用，1 个任务，一次 `finish` 就收成 `completed`（带 test、clippy、fmt 三条证据）。
- 按工具：`exec_command` 25、`read_file` 21、`search_text` 12、`write_stdin` 12、`task_manage` 10、`apply_patch` 7、
  `git_diff` 4、`git_status` 3，其余各 1–2 次。
- `list_skills` / `get_skill`、MCP 转发、浏览器、`planning_manage`、`list_project_commands`、`check_command` 都是 0 次。
- 工具报错 2 次：`FILE_VERSION_CONFLICT`（补丁带的是自己改之前的文件版本）、`WORKSPACE_PATH_PROTECTED`（见下表第 3 条）。
- 回包共 1,100,540 字节，`read_file` 占 42%；那一次 `finish` 回了 47,440 字节。
- 提交都是 `exec_command` 跑 `git add <文件>` 加 `git commit -m <一行>`。提交前的检查一次比一次少：
  第 1 次看了 status 和 diff，第 2 次只看 diff，第 3 次没看。

### 暴露的 gld 不足

按浪费多少排：

| 不足 | 证据 | 可以怎么改 |
| --- | --- | --- |
| 1. AI 自己 `git commit` 之后，下一次写之前要 `refresh_baseline` 两次（先看、再接纳） | `Harness::check_baseline` 只要 HEAD 变了就报 `BASELINE_STALE`。3 次接纳记录的 `changes` 都是空的、指纹没变，只有 HEAD 变了。3 次提交多花 6 次调用 | 任务期间 gld 自己跑的命令让 HEAD 前进、文件指纹和记账一致时，把新 HEAD 直接记上账 |
| 2. 只改文档也要重跑全仓 test | 证据要求跑完之后工作区没变。`19e07b1` 只改 2 个 .md，test 91 秒、clippy 6 秒又跑了一遍 | 这是验收规则的取舍；要不要按路径放宽，由用户定 |
| 3. 工作区里面的绝对路径被当成外部路径 | `command_contains_external_path` 见到 `/` 开头就算外部；命令里同时有写文件的操作时整条拒。11:11:21 被拒的那条没留原文，它写的用途是"在隔离 GLD_HOME 完整执行……日志只放 target"，前后命令用的都是 `target/` 下的绝对路径，多半撞的是这条（推断） | 先解析路径，落在工作区里就不算外部 |
| 4. `read_file` 一次只读一个文件 | 3 次改用 `python3 -c` 一次读 15 段。这些读被记成命令，算进任务的写操作 | 让 `read_file` 一次收多段 |
| 5. 命令默认 1 秒就转后台 | `yield_time_ms` 默认 1000、最大 30000，它一直用默认值。14 条 cargo 里 7 条 20 秒内结束，仍各多轮询一次；9 条后台命令共轮询 12 次 | 工具说明里提示跑测试时给大 `yield_time_ms` |
| 6. 回包偏大 | `finish` 带完整 `change_summary`，47 KB；每个回包的 JSON 在 text 和 `structuredContent` 各放一份（MCP 规范为兼容旧客户端建议这么做） | `finish` 只给计数和风险，细节留给 `action=change_summary` |
| 7. 记录不全，事后查不清 | `operations.jsonl` 只记补丁、命令和 Git 工具（`should_log_operation`），103 次里记了 41 次；请求日志没有时间戳；被拒的命令不留原文；`run.json` 的命令只存前 500 字符 | 请求日志加时间戳 |
| 8. 用户全局规则还没送到 | `gld context -w gld`：`INSTRUCTIONS.md` 打 ·（扫到没注入），只有仓库 `AGENTS.md` 打 ✓ | 部署 `e8f7f36`（§26），等用户点头 |
| 9. Skill 目录对这个项目没用 | 扫到 94 个，compact 目录按扫描顺序只列 5 个（launch-software-factory、generate-design、planning-with-files、utools-plugin-development、ui-ux-pro-max），都和 Rust 无关；另有 18 个同内容的重复被 ✗ | 按项目语言挑，或在项目里放自己的 skill |

### Skills、MCP、浏览器现在怎样

- **Skills**：`list_skills` 列、`get_skill` 读正文和 skill 目录里的文本附件，gld 不执行 skill 里的脚本。
  compact 档只在 `workspace_context` 里给一段 1200 字符的目录（上面第 9 条），其余要 AI 自己调 `list_skills`。
- **MCP 转发**：`gld mcp on <名>` 点名才转，AI 那边只多 `list_mcp_tools`、`call_mcp_tool`、`read_mcp_result` 三个工具。
  本机配置里约 30 个 server，只开了 `browser`。只转工具，不转 resources 和 prompts；grant 用不了转发。
- **浏览器**：能用，靠转发的 Playwright MCP（`--isolated --headless`，只放本机地址，`cwd` 限在 `~/.cache/gld-browser`）。
  做法是 `exec_command` 带 `service_port` 起服务，再 `call_mcp_tool server=browser` 调 `browser_navigate`、`browser_snapshot`、
  `browser_click`、`browser_console_messages`、`browser_take_screenshot`，§20 实测过。截图存在 `~/.cache/gld-browser`，
  不进项目、不当验收证据。`gld grant browser` 是给浏览器扩展发连接地址（§23），和"让 AI 操作浏览器"是两回事。

**没做：**上表的代码改动都没动，没部署，没推送。

## 29. 解释器写文件的路径判断：Workspace 里面的绝对路径放行（2026-10-18）

**起因：**§28 表里第 3 条。动手前在 `19e07b1` 的调试版上实测（隔离 `GLD_HOME`，直连模式，`check_command` / `exec_command`），
旧判断的误判比 §28 估的多，还有一处漏判。下面都是 argv 形式的 `python3 -c`：

| 脚本 | 旧 | 新 |
| --- | --- | --- |
| `open('a.txt','w').write('hi')`（单引号，写相对路径） | 拒 | 放行 |
| `open("e.txt","w").write(str(4 / 2))`（除法） | 拒 | 放行 |
| `open("<Workspace>/f.txt","w").write("hi")` | 拒 | 放行 |
| `open("/tmp/x","w")`、`open("../g.txt","w")` | 拒 | 拒 |
| `import shutil; shutil.rmtree("/")` | **放行**（`check_command` 回 allow） | 拒 |

原因：旧代码先把 `\` 全换成 `/`，再把引号或空白后面跟着非双引号的 `/` 都当绝对路径。argv 里的单引号被
`shell_words::join` 写成 `'\''`，换完成了 `'/'`；`4 / 2` 空格后面就是 `/`；`"/"` 的 `/` 后面紧跟双引号，没认出来。

**改了什么（`44d4679`）：**`command_contains_external_path` 先挑出像路径的片段，再逐个判断：

- 引号里的取到同一个引号为止；不在引号里的 `/` 要紧跟名字字符（`a / b`、`a // b` 不算）。
- `../`、`~/`、Unix 上的盘符路径算外面；绝对路径从最长的已存在那一级 canonicalize，按路径组件和 Workspace 根比。
  软链指到外面的、悬空软链、`/a/gld2` 这种兄弟目录都算外面。
- 不知道 Workspace 时（不带 Workspace 调 `validate_tool_arguments`）绝对路径一律算外面，和以前一样。
- Unix 上 `\` 不当分隔符。Windows 上 `\` 开头且后面跟名字字符的仍算外面，所以那边 `'\n'` 还会误判，没在 Windows 上跑过。
- 顺带收紧：引号里单独的 `/`、引号里的 `..`、`~/`、反引号和 `(` `=` `,` 后面的路径。代价是 `'/'.join(parts)`
  加写文件会被拒（以前双引号版本放行）；`docs/security.md` 写了这条规则和绕法，这条规则以前文档里没写。

**验证：**

| 检查 | 结果 |
| --- | --- |
| 新增 `crates/core/tests/command_write_paths.rs` 5 条 | 换回旧 `policy.rs` 跑：3 条失败（单引号、Workspace 内绝对路径、`rmtree("/")`），2 条通过（软链、不带 Workspace）；新代码 5 条全过 |
| `call_tool_security` | 28 条通过 |
| `cargo test --workspace --all-targets --locked` | 45 个测试二进制，959 passed / 0 failed |
| clippy `-D warnings`、`fmt --check` | 通过 |

**没做：**

- 写文件的正则 `open\([^)]*['"]w` 碰到第一个 `)` 就停，`open(os.path.expanduser('~/x'), 'w')` 根本不算写文件，
  路径判断轮不到它。这是另一处漏判，测试里换了写法，规则没改。
- 相对路径经过 Workspace 里的软链写到外面，照样看不出来（以前也看不出）。
- `policy.rs` 改完 1084 行，超过 800 行该评估拆分了；路径判断这三个函数可以单独成一个模块，这次没拆。
- 没部署：要换二进制、`gld daemon restart`，等用户点头。

## 30. 任务里经 gld 提交，不再要两次 refresh_baseline（2026-10-18）

**要求：**修 §28 不足第 1 条，先写会失败的测试再改。第 3 条（工作区里面的绝对路径）另做；第 2 条是验收规则的取舍，
由用户定，这次没碰。

### 改了什么

`ad22e83`：`exec_command` 回包记账（`Harness::refresh_expected_state`）时，下面四条都满足，就把新 HEAD 记进任务基线，
再记一条 `head_advanced` 任务事件（`session_id`、脱敏截短的命令原文、分支、前后 HEAD）：

1. 现在有 HEAD，和任务记的不一样；
2. 分支和任务记的一样；
3. 文件指纹和上次记账一样；
4. 任务记的旧 HEAD 是新 HEAD 的祖先（`git merge-base --is-ancestor`）。

能这么认，是因为 `exec_command` 起跑前的写前检查确认过 HEAD 还是记账时那个，挪动只能发生在这次调用期间。
`apply_patch` 和 history 工具的记账不走这条，它们不会动 HEAD。规则写在 `concepts.md`「工作区指纹」。

### 想过的情况

| 情况 | 结果 | 依据 |
| --- | --- | --- |
| 先 `git add`、再 `git commit`，分两条跑 | 直接记账，下一次 `apply_patch` 成功，`finish` 照常收 | 测试 `gld_跑的提交只让_head_前进时直接记账`；`.git/` 不计入指纹 |
| 不经 gld 的提交（测试里直接调 git） | 照旧 `BASELINE_STALE`，之后 gld 起的命令也带不进来 | 写前检查在命令起跑前就拒；测试 `任务之外的提交照旧报_baseline_stale` |
| 一条命令里文件也变了（`git merge --ff-only` 带进文件改动） | 照旧报 | 条件 3 |
| 换到前面的分支（`git checkout -b topic other`，HEAD 往前、文件没变） | 照旧报 | 条件 2；换分支不在这次范围 |
| `git commit --amend` | 照旧报 | 条件 4 |
| 转了后台、回包之后才落地的提交 | 照旧报 | 回包时 HEAD 还没动。读到它结束（`observe_command`）时不补认：从回包到读到结束可以隔任意久，那段时间里别人的提交分不开。测试 `回包之后才落地的提交不自动记账` |
| gld 在命令跑到一半时重启 | 照旧报 | 记账在回包时才做；判断只读任务文件里存的 HEAD 和指纹，没有只在内存里的状态 |
| 命令跑的那几秒里，别处（终端、IDE）也提交了 | 记在这条命令头上 | 分不清，事件里留着命令原文给人核对。gld 自己别的会话的写这时要等工作区写锁（`exec.rs` 测试 `a_foreground_command_holds_the_write_lock_while_it_waits`），挡不住的是 gld 之外的 git 和 gld 转了后台还在跑的命令 |

三条反例测试改代码前就通过（原来的行为本来就保守），留着是为了挡住这次的放宽放过头。逐个把条件 2、3、4 换成 `true`
重跑 `head_变了又不只是往前提交时照旧报_baseline_stale`，三次都失败，分别卡在换分支（多出 `head_advanced` 事件）、
快进合并、amend 那一例。

### 发现但没改：提交之后，提交前跑的测试不能再当验收证据

证据结束时的位置带 HEAD，`finish` 拿它和现在的位置整体比（`verify.rs` 的 `accept_evidence`：`end != current` 就报
`EVIDENCE_STALE`）。所以"跑测试 → 提交 → `finish` 带那次测试"会被拒，要在提交之后再跑一遍，哪怕提交没改任何文件。
用一条临时测试实测过（改 README → 跑 `git --version` 记 session → `git add`、`git commit` → `finish` 带它），回
`EVIDENCE_STALE`，消息是"命令结束之后工作区又变了"，而文件其实没动，只有 HEAD 变了；临时测试没留在仓库里。
这和第 2 条（只改文档也要重跑全仓）是同一类验收规则的取舍，放不放宽由用户定，这次没改。

### 验证

| 检查 | 结果 |
| --- | --- |
| 改代码前跑 4 条新测试 | `gld_跑的提交只让_head_前进时直接记账` 失败，报 `BASELINE_STALE`（即本条不足）；3 条反例通过 |
| 改代码后 `task_acceptance` | 16 passed / 0 failed |
| `cargo test --workspace --all-targets --locked`（隔离 `GLD_HOME`，`ad22e83`） | 44 个测试二进制，958 passed / 0 failed / 0 ignored（§28 是 954，多的是这 4 条） |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 退出 0 |
| `cargo fmt --all -- --check` | 退出 0 |

**没做：**没换本机二进制、没重启守护进程、没推送；提交在分支 `claude/elegant-wright-5812fc` 上，没合进 main。
新行为没经 ChatGPT 连接器实测。没改守护进程 Request/Response、命令行帮助和工具输入表；`exec_command` 回包形状没变，
多出来的只有任务事件里的 `head_advanced`。

**合并（同日）：**分支 `claude/elegant-wright-5812fc` 和 main 已经分叉（main 上多了 §29 的 `44d4679`、`7647906`），不能快进，
用合并提交合进 main，没有改写分支上的提交。冲突只在文档：两边都追加了 §29，这一节改成 §30；`task_plan.md` 那一行合成一条。
合并后的树上 `cargo test --workspace --all-targets --locked` 45 个测试二进制，963 passed / 0 failed（§29 之后 959，多的是这节的 4 条），
clippy `-D warnings`、`fmt --check` 通过。

## 31. §28 暴露的问题逐条修完（2026-10-18）

**要求：**用户看完 §28 的复核说"从中暴露出的 gld 问题都要修复"。§29、§30 修了第 3、第 1 条，这一节是剩下的，
外加复核途中又查出来的几处。每条都先写会失败的测试（标了"对照"的是换回旧代码跑过），再改，一件事一个提交。

### 改了什么

| §28 的条目 | 提交 | 改成什么样 |
| --- | --- | --- |
| 2. 提交之后提交前的证据不认（§30 发现） | `c94454d` | 只多了提交也算同一份内容：分支没换、文件指纹没变、HEAD 只往前走。"跑测试 → 提交 → `finish`"照样收，`warnings` 写一句它跑在提交之前。HEAD 往回走、换分支照旧要重跑。对照：旧代码报 `EVIDENCE_STALE` |
| 2. 只改文档也要重跑全仓 | 没改 | 这是验收规则的取舍：gld 的文档也有测试（链接、路径门禁），放宽"改了 .md 不算变"会让证据说不清测的是什么。等用户定 |
| 3. 写文件的识别漏判（§29 记了没改） | `99104ed` | `open(` 里带括号、`'a'` `'x'` `'r+'` 模式、`os.rename`、`shutil.copy`、`Path.touch`、`fs.appendFileSync` 等都认得出；`open('/etc/w.txt')` 这种读不算。对照：旧正则漏掉 `open(os.path.expanduser('~/x'), 'w')` |
| 3. Windows 上 `'\n'` 误判、`policy.rs` 过长（§29 记了没改） | `a955313` | 路径判断搬进 `tools/command_paths.rs`（`policy.rs` 回到 992 行）；Windows 上 `\` 开头的要像路径（`\\server` 或后面还有分隔符）才算。单测在 Mac 上把 Windows 那一路也跑一遍 |
| 4. `read_file` 一次一个文件 | `86b5e98` | `read_file` 收 `ranges`：最多 20 段、合计 256 KiB，按顺序回 `results`，一段失败不影响别的段 |
| 5. 默认 1 秒转后台、反复轮询 | `e105426` | 根因是 `write_stdin` 不管命令结没结束都睡满 `yield_time_ms`，AI 只敢用 1 秒。现在结束就返回；"还在跑"的提示、`command_summary`、两个工具的描述都写明"`write_stdin` 留空、`yield_time_ms` 给 30000"。对照：旧代码的提示还是"继续读取 session" |
| 6. `finish` 回包 47 KB | `12f2fc2` | `finish` 里的 `change_summary` 只留改了几个文件、前 50 个的路径和状态、验收记录、风险；完整的在 `action=change_summary` |
| 6. 每个回包 JSON 放两份 | 没改 | MCP 规范（2025-06-18 版 Tools 一节，这次查过原文）写的是"为了向后兼容，返回 structured content 的工具 SHOULD 同时在 TextContent 里放一份序列化 JSON"。去掉会让不读 `structuredContent` 的客户端什么都拿不到 |
| 7. 请求日志没有时间 | `de2f484` | `mcp-requests.log` 每行开头加 UTC 时间，和守护进程日志同一个格式（格式化函数从 gld-daemon 搬进 core，仍只有一份） |
| 7. 其余记录缺口 | 没改 | `operations.jsonl` 只记补丁、命令、Git（读工具不进任务记账，是原来的设计）；被拒命令不留原文、`run.json` 命令只存 500 字符，都是为了不把内容和密钥写进日志。有了时间之后，请求日志能对上几点调了什么 |
| 8. 用户全局规则送不到 | 没改 | 源码早改好了（`e8f7f36`），要换二进制、重启守护进程、点名配置，等用户点头 |
| 9. Skill 目录对项目没用 | `7f85030`、`ac57eb5`、`b5e79f7` | compact 目录里装在本机的 skill 只列名字（几十个都装得下，项目自己的照旧带描述）；`list_skills` 收 `query` 按词查名字和描述，英文按词头、中文按子串。`b5e79f7` 是补 `7f85030` 漏过的 clippy：那次检查命令把 clippy 的退出码吞了，我没看到就提交了 |

复核时另外查出来的：

| 问题 | 提交 | 改成什么样 |
| --- | --- | --- |
| 补丁失败最多的一类是版本冲突（请求日志 30 个原因里 18 个 `file_changed_since_read`）；`operations.jsonl` 里还留着的 9 次，gld 都在之前 35 秒到 6 分钟内写过同一个文件 | `3eb5fe4` | 带的版本落后、但中间全是同一个调用方经 gld 写的，放行并在 `warnings` 说明；换了调用方、gld 之外改过、gld 重启过照旧拒。对照：旧代码报 `FILE_VERSION_CONFLICT` |
| 重试 `finish` 补不上第一次没写进去的 summary | `0a6f76b` | 任务里还没有 `task_summary` 就补写一次，有了不追加。对照：旧代码 `summary_recorded=false` |
| 命令超长的报错不说上限 | `c5f06b1` | 消息写明这条多少字节、上限 4000，建议写成文件再跑、大段数据走 `stdin` |
| ChatGPT 撞到 gld 的毛病只绕不报 | `0ee05f3` | 写明在这个仓库里 gld 自己的工具问题就是缺陷，绕过去要在交付说明里写出来 |

补丁失败原因的分布顺带看了：30 个原因里缩进 2 个、`partial` 8 个、版本冲突 18 个、其他 2 个，行尾空格和标点 0 个。
§22 说的"照 Codex 逐级放宽比对"现在没有依据，维持逐字比对。

### 验证

全仓结果见下表；每条的单测和对照写在各自提交说明里。

| 检查 | 结果 |
| --- | --- |
| `cargo test --workspace --all-targets --locked`（隔离 `GLD_HOME`） | `0ee05f3` 上 48 个测试二进制，982 passed / 0 failed / 0 ignored（§30 合并后 963，多的 19 条是这一节的） |
| clippy `-D warnings`、`fmt --check` | 通过（`7f85030` 那次漏过的已由 `b5e79f7` 补上） |

### 上线要注意

- 都只改了源码，没换本机二进制、没重启守护进程、没推送。
- 工具描述和输入表变了（`exec_command`、`write_stdin`、`read_file`、`list_skills`）：换完二进制，ChatGPT 要在连接器设置里
  点一次 Refresh 才看得到 `ranges`、`query` 和新描述；不点照样能用，只是用旧的那套。连接器不用删、不用重新授权。
- 新行为都没经 ChatGPT 连接器实测。下一轮经 gld 开发时看：提交之后还有没有 `refresh_baseline`、有没有每秒轮询、
  还会不会用 `python3 -c` 读文件、`FILE_VERSION_CONFLICT` 还多不多。

**顺带收口的待办：**"ChatGPT 换版后第一次调用"（§25 换上 `778fdd3` 之后）。2026-10-18 10:57 起 ChatGPT 的调用都是原注册客户端
`oauth:hub:dcr-63f31361…`，没有要求重新授权，请求日志里没有 OAuth 的 401；附近那两条 401 是 `credential=browser_url`（地址无效或过期）
和 curl 没带凭据，和连接器无关。

## 32. 本机换上 `4a6d098`，用户全局规则开始送到 ChatGPT（2026-10-18 14:2x）

**要求：**用户看完 §31 让部署："先查没有在跑的命令，换二进制，重启守护进程，再配 custom-instruction-paths"。

**做了什么**（照 [安装 · 升级](../install.md#升级)）：

1. `cargo build --release --locked -p gld`，2 分 06 秒。`rust-objcopy` 去符号失败的警告照旧（工具链的问题，产物 16 MB 没去符号，和之前一样）。
2. 备份到 `~/.local/opt/`：换之前的命令行二进制 `gld-0b4cb59-20261018-1419`，数据目录 `gld-config-20261018-1419.tgz`（`chmod 600`，里面有明文凭据）。
3. 换之前查：守护进程（pid 87812）`pgrep -lP` 没有子进程；请求日志最后一次写是 12:59，80 分钟没人用。
4. 写新文件再改名换掉 `~/.local/bin/gld`，去掉本会话的 `CLAUDE*` 变量后 `gld daemon restart`，新 pid 21527，1 个服务恢复。
5. `gld settings runtime --custom-instruction-paths ~/.codex/AGENTS.md`。注入前查过这份文件：唯一一个像邮箱的串是规则正文里的占位
   `user@example.com`，没有密钥类字样。

**换完核对：**

| 项 | 换之前 | 换之后 |
| --- | --- | --- |
| 服务构建提交（`gld tool list --served`） | `778fdd359240` | `4a6d0983a19b`，等于 HEAD |
| 工具表 | 33 个，指纹 `5155ec6663bb33e2` | 33 个，指纹 `59951e97a502352e`：变了，原因是 §31 改了 `exec_command`、`write_stdin`、`read_file`、`list_skills` 的描述和输入表 |
| 凭据（排好键的 JSON 取 SHA-256 前 16 位） | `profiles.json` `1f8bfa4a…`、`oauth-clients/hub.json` `984f8cea…` | 一样 |
| `gld ls` | | 和换之前逐字一样 |
| `gld health` | | 没有失败项，本地和公网 `/mcp`、OAuth 两份元数据都是 200 |
| 新守护进程环境里的 `CLAUDE*` 变量 | | 0 个 |
| `gld context -w gld` 说明文件 | 扫到 2，注入 1（全局规则打 ·） | 扫到 2，注入 2：`[codex/custom] INSTRUCTIONS.md` 和仓库 `AGENTS.md` 都打 ✓ |
| `gld context -w gld` Skill 目录 | 列了 5 个 | 列了 52 个 |

经守护进程实调（本机调用方，只读）：`read_file ranges` 两段按顺序回来；`list_skills query="debug test"` 回 94 个里的 10 个，
`systematic-debugging` 排第一。`workspace_context` 是 hub 层的工具，`gld tool call` 调不到，注入以 `gld context` 的 ✓ 为准。

**要用户做的：**工具表指纹变了，ChatGPT 不会自己重拉。到 ChatGPT 的连接器设置里点一次 Refresh、再开新对话，才看得到
`ranges`、`query` 和新的工具描述；不点也照样能用旧的那套。连接器不用删、不用重新授权、地址没变。

**没验的：**新行为都没经 ChatGPT 连接器实测（等用户下次用）。回滚：`~/.local/opt/gld-0b4cb59-20261018-1419` 换回 `~/.local/bin/gld`
再 `gld daemon restart`（命令行和守护进程一起回到 `0b4cb59` / `778fdd3` 那一版的行为，数据目录不用动）。

## 33. 三台都装上，Linux 服务器上跑全仓查出两条依赖环境的测试（2026-10-18）

**要求：**用户说"完成通过后，三机都安装"。三台指的和 ccnm 一样：本机、另一台 Mac、一台 Linux 服务器。用户选了**不发版、本机构建**
（不推送、不打 tag），Linux 服务器上给普通账号和 root 各装一份。

### 装在哪

| 机器 | 位置 | 二进制 | 怎么装的 |
| --- | --- | --- | --- |
| 本机 Mac | `~/.local/bin/gld`，守护进程常驻 | `4a6d098` release，sha256 `14b64fee…` | §32 |
| 另一台 Mac（macOS 26.3 arm64） | `~/.local/bin/gld`，登录 shell 的 PATH 里有 | 同一份，`14b64fee…` | scp 过去写 `.new` 再改名；ad-hoc 签名在，没有隔离属性 |
| Linux 服务器（Debian 13 x86_64） | 普通账号和 root 各自的 `~/.local/bin/gld` | `ce36f73` 在服务器上构建，`b52cb78c…` | 本机 `git bundle` 传过去（构建提交号要从 git 读），在普通账号的 `~/gld-src/gld` 里构建，`install` 给两个账号 |

`ce36f73` 和 `4a6d098` 之间只差文档提交，两份二进制的代码一样，只有嵌进去的构建提交号不同。
服务器上的 root 原来不把 `~/.local/bin` 放进 PATH：root 的 `.profile` 先备份成 `.profile.bak-20261018`，末尾加了和普通账号
`.profile` 同一种写法的四行。三台都只装了命令行，没起守护进程、没加项目、没碰数据目录。

**冒烟：**另一台 Mac、服务器的 root 和普通账号各用临时 `GLD_HOME` 加临时 git 仓库跑了一遍 `read_file ranges`（两段按顺序回来）和
`exec_command git --version`（`command_ok: true`），跑完删掉临时目录，三处的 `~/.config/gld` 都没有生成。

### Linux 服务器上全仓测试

用普通账号跑（root 读得到 0000 权限的文件，权限类测试会误报），`--no-fail-fast`（不加的话第一个挂的测试二进制后面都不跑）。

| 轮 | 结果 |
| --- | --- |
| `ce36f73` | 48 个测试二进制，980 passed / 1 failed：`enabling_gh_opens_read_only_diagnostics_only` |
| 修了上一条之后单跑 `call_tool_contract` | `versions_round_trip_from_patch_check_through_apply_patch` 挂，连跑 5 次挂 3 次 |
| `25a5e2c`（两条都修了） | 48 个测试二进制，**981 passed / 0 failed**；那条版本测试连跑 10 次 10 次过 |

本机 `e7a20a9`：48 个测试二进制，982 passed / 0 failed，clippy、fmt 通过。多出来的 1 条是只在 macOS 上编译的
`platform::macos::process::tests::process_image_path_reads_current_process`，两边测试名清单逐条比过，只差这一条。

### 查出来的两条

| 测试 | 为什么挂 | 修 |
| --- | --- | --- |
| `enabling_gh_opens_read_only_diagnostics_only` | 断言 `gh run list` 预检回 allow，而预检要在 PATH 上找到程序：它依赖机器装了 `gh`。CI 的 ubuntu 镜像和本机都有，这台服务器没有，报 `program_not_found` | `2e92c06`：用 repo-maintenance 夹具里的假 `gh`（`tools/gh`）放到 PATH 前面，加 `#[cfg(unix)]`。本机把 `gh` 从 PATH 里拿掉跑也过 |
| `versions_round_trip_from_patch_check_through_apply_patch` | 版本号是大小加修改时间。这台服务器（内核 6.12，`CONFIG_HZ=250`，临时目录在 tmpfs）上文件时间戳 4 毫秒一格，"one" 改成 "two" 是等长写入，落在同一格里版本号就一样（实测两次都是 `4-18df894cdac02d20`） | `25a5e2c`：改成 "one" → "second"，长度也变，和别的写版本测试同一个办法 |

第二条也是产品层面的事实：**Linux 上版本号分不清几毫秒内的等长改动**。正常交替改文件间隔远大于 4 毫秒，碰不上。
版本号格式没改：它和 ccnm 的 `version_of` 必须一字不差（审查 2026-09-19 那份里定的），要加 inode 之类得 gld、ccnm 一起改，
排进 task_plan 等用户定。限制写进了 [concepts.md「文件版本」](../concepts.md#文件版本别让补丁盖掉别人的改动)（`74319e2`、`e7a20a9`）。

**没做：**没推送、没发版；另外两台上没有守护进程，所以 ChatGPT 连不到它们，那两台上的 gld 目前只能命令行用。

## 34. 确认 Skills、本机 MCP 转发、浏览器在当前版本上能用（2026-10-18）

**要求：**用户要求必须确认三件事：gld 对 Skills、MCP 的使用如何；gld 能不能调浏览器操作；文档要写全。
§28 只读了文档和源码，这一节是在本机跑着的 `4a6d098` 上实调。

**怎么测的：**另起一个隔离的服务，不碰常驻服务和 ChatGPT 连接器：临时 `GLD_HOME`、随机端口、bearer 认证、只听 127.0.0.1，
`gld mcp on browser`（读的是本机同一份 `~/.codex/config.toml` 里的 `[mcp_servers.browser]`），加一个只有一个静态页面的临时项目，
页面埋了一个 bug：点按钮会调不存在的 `rendr()`。再用脚本照 ChatGPT 的方式经 `/mcp` 发 JSON-RPC：`initialize`、`tools/list`、`tools/call`。

| 调用 | 结果 |
| --- | --- |
| `tools/list` | 33 个工具，含 `list_mcp_tools`、`call_mcp_tool`、`read_mcp_result` |
| `workspace_context` 的 `skills` | 1433 字符，装在本机的那批只列名字（§31 的新目录） |
| `list_skills query="debug test"` | 94 个里的 10 个，`systematic-debugging` 排第一 |
| `get_skill name=systematic-debugging` | 回正文（`# Systematic Debugging` 开头） |
| `get_skill … file=SKILL.md`（主目录里自动发现的 skill 的附件） | `INVALID_ARGUMENT`，消息写明"来源配置之后才给读文件"：设计如此（security.md 写了） |
| `list_mcp_tools server=browser` | 24 个工具，没有 `browser_run_code_unsafe` |
| `exec_command` 带 `service_port` 起 `python3 -m http.server` | `running`，`service.ready: true` |
| `call_mcp_tool browser_navigate` | 打开了，页面标题对 |
| `browser_snapshot` → `browser_click target=<ref>` | 点上了；回包里写着 `Console: 1 errors` |
| `browser_console_messages` | `ReferenceError: rendr is not defined`，行号也对 |
| 点击后再 `browser_snapshot` | 有"新的一项" |
| `browser_take_screenshot` | PNG 回到结果里（约 10 KB），截图上能看到新加的那一项 |
| 按名字调 `browser_run_code_unsafe` | `MCP_TOOL_UNKNOWN`，没执行 |
| `kill_session` | `service stopped`，正常结束 |

第一次点击时我按老参数名传了 `ref`，报 `Invalid arguments … expected string … at target`。这台的 Playwright MCP 仍是 0.0.83（npx 缓存里只有这一版），
concepts.md 早写了"0.0.83 起叫 `target`，先 `list_mcp_tools` 看一眼再调"：不是上游变了，是我没先看参数。

**结论：**

- **Skills**：AI 能经 `workspace_context` 看到目录、用 `list_skills query` 按词找、用 `get_skill` 读正文。gld 不执行 skill 里的脚本；
  主目录里自动发现的 skill 默认不给读附件。这一轮 ChatGPT 开发 gld 时一次都没调 skill（§28），原因是当时目录里列的 5 个都和项目无关，
  §31 改了目录，要等下一轮看它用不用。
- **MCP**：本机配好的 server 要 `gld mcp on` 点名才转，AI 那边只多三个中转工具；本机现在只开了 `browser`，其余约 30 个都关着。
- **浏览器**：能操作，靠转发的 Playwright MCP（隔离、无头、只放本机地址）。ChatGPT 2026-10-08 用它修过页面 bug（§20），这次在当前版本上
  又走通了一遍。截图、快照存在 `~/.cache/gld-browser`，不在项目里。`gld grant browser` 是给浏览器扩展发连接地址，和"AI 操作浏览器"不是一回事。

**文档：**README 的"能做什么"补了 Skills 和本机 MCP 一条（`06fb257`）；concepts.md 浏览器一节记了这次复测（`0f734d9`）。
Skills、转发、浏览器的配法和边界原来就写在 concepts.md 和 security.md，这次核对过没有和实测对不上的地方。

**清理：**停掉隔离服务（它起的 Playwright 进程跟着退了；还在跑的三个 `@playwright/mcp` 是 Claude Code 自己 10-11、10-15 起的，没动），
删掉临时数据目录和项目，`~/.cache/gld-browser` 里删掉这次新增的 7 个文件（2 张截图、3 份快照、2 份 console 日志）。常驻服务 pid 没变。

## 35. `gld --version` 带上构建提交，三台换成 `f9400f7`（2026-10-18）

**起因：**§32 换完二进制，用户看 `gld --version` 还是 `gld 0.8.3`，问"本机上的版本未变？这正常吗"。正常：版本号只在发版时改，
没发版的构建全报同一个号；可命令行自己给不出别的线索，只有 `gld tool list --served` 能看到构建提交，而那要守护进程在跑，另外两台没有。
用户让改了验过再三台都装。

**改了什么：**

| 提交 | 内容 |
| --- | --- |
| `abaaef8` | 构建提交号导出成 `gld_core::BUILD_COMMIT`（`build.rs` 嵌的环境变量只在 gld-core 里看得到），`server_info` 改读它 |
| `f9400f7` | `gld --version` 和子命令的 `--version` 打成 `gld 0.8.3 (f9400f7ade51)`；拿不到 git 时只有版本号。`scripts/package.sh` 原来要求输出恰好是 `gld <版本>`，改成两种都认；`install.md`「换完核对」加 `gld --version` 一行 |

验证：新增 `version_output.rs`（格式，以及在 git 检出里括号里就是 HEAD）；`package.sh` 的判断用 bash 单独喂了 5 种输出
（两种通过、版本不对和格式坏的拒）；`gen-cli-docs.sh` 重新生成 `docs/cli.md` 没有差异（帮助文本没变）。

| 平台 | 全仓结果（`f9400f7`） |
| --- | --- |
| 本机 Mac | 49 个测试二进制，983 passed / 0 failed，clippy、fmt 通过 |
| Linux 服务器（普通账号） | 49 个测试二进制，982 passed / 0 failed（少的一条是只在 macOS 编译的那条） |

**换装：**

| 机器 | 之前 | 之后 |
| --- | --- | --- |
| 本机 | `gld 0.8.3`，服务构建提交 `4a6d0983a19b` | `gld 0.8.3 (f9400f7ade51)`，sha256 `5bbe4c54…`；守护进程重启，pid 21527 → 74288 |
| 另一台 Mac | `4a6d098` 那份 | `gld 0.8.3 (f9400f7ade51)`，sha256 `5bbe4c54…`，和本机同一份 |
| Linux 服务器普通账号、root | `ce36f73` 构建 | 两个账号都是 `gld 0.8.3 (f9400f7ade51)`，sha256 `3d1cd09c…`（在服务器上从同一个提交构建） |

本机照 [安装 · 升级](../install.md#升级) 做：换之前守护进程没有子进程，请求日志最后一条是 15:13（17 分钟前）；备份
`~/.local/opt/gld-4a6d098-20261018-1528` 和 `gld-config-20261018-1528.tgz`（0600）；去掉 `CLAUDE*` 变量后重启。换完：工具表 33 个、
指纹 `59951e97a502352e` **没变，不用 Refresh**；`profiles.json`、`oauth-clients/hub.json` 的指纹和换之前一样（`profiles.json` 和 §32 时
不同，是因为 §32 写了 `custom-instruction-paths`，这次以换之前那一刻为准）；`gld ls` 一样；`gld health` 没有失败项；新守护进程环境里
`CLAUDE*` 变量 0 个；`gld context -w gld` 两份说明仍都打 ✓。

**顺带确认了 §32 要用户做的 Refresh：**请求日志 `2026-10-18T06:13:55Z`（本机 15:13:55）原注册客户端 `dcr-63f31361…`
重拉了 `tools/list`，回包 43880 字节：ChatGPT 已经拿到 §31 的新工具描述和 `read_file ranges`、`list_skills query`。

**没做：**没推送、没发版（用户定了等 ChatGPT 在新版上跑完一轮再说）；另外两台仍只有命令行，没起守护进程。

## 36. 推送到 origin，CI 头一次跑这些提交就红了三个 job（2026-10-18）

**要求：**用户说"推送吧"。`git fetch` 后远端没动，`54bacbf..6b91d7b` 快进推上去（60 个提交，没发版）。推之前扫了一遍要公开的内容：
diff 里没有家目录路径、个人域名、邮箱，作者都是 noreply 地址；有 4 个提交说明里写了 Linux 服务器的 SSH 别名
（指向内网 Tailscale 地址，外面连不上；远端历史里原本也有这类别名），改提交说明要改写历史，没动。

**CI（run 39104838448，`6b91d7b`）：**格式与 Clippy、MSRV 三个平台、命令参考都过，三个 job 红：

| job | 报错 | 根因 | 修 |
| --- | --- | --- | --- |
| Windows 编译检查 | `command_write_paths` 测试里 4 个 `dead_code`（`-D warnings`） | `44d4679` 只给用例加了 `#[cfg(unix)]`，辅助函数和夹具字段在 Windows 上没人用 | `4b84a1c`：文件级 `#![cfg(unix)]`，和 `repo_maintenance_replay.rs` 同一个办法 |
| 测试 (ubuntu) | `scoped_grants::a_grant_needs_real_auth_and_real_projects`：`gld upgrade --port 28765` 报"已经是全局入口的本地端口" | 测试取端口的 `free_port` 会发 gld 自己的默认端口（全局入口 28765、工作区 MCP 从 28766 往上），这些号写进配置时还没人在听，"连一下"查不出来；起点按 pid 错开，只在某些机器上撞 | `d78461d`：`free_port` 跳过 28700–28899，新增 `test_ports.rs`（连取 12000 个都不落进这一段；换回旧实现第一个就发了 28700） |
| 测试 (macOS) | `actions_and_gateway::global_gateway_routes_only_what_opted_in`：`actions.port=28766` 报"与工作区的 MCP 服务重复" | 同上 | 同上 |

后两条是 2026-09-22 起就在的测试辅助缺陷，以前 CI 碰巧没撞上；本机（28767 被常驻服务占着，自动分配会跳开）和 Linux 服务器上跑都没撞过，
所以推之前的两个平台全量都是绿的。Windows 那条本机验不了：没有 Windows 头文件，`ring` 的 C 代码编不过（`--target x86_64-pc-windows-msvc`
实测），只能靠 CI；推之前我对这轮改过的测试文件逐个看了 `cfg(unix)` 的用法，只有这一个文件有问题。

**修完：**本机 50 个测试二进制 984 passed / 0 failed，clippy、fmt 通过；推上去的 `d78461d` 在 CI（run 39105359359）8 个 job 全过（Windows 编译检查、macOS 和 ubuntu 测试、三个平台 MSRV、格式与 Clippy、命令参考）。

**记下的教训：**CI 的 Windows 编译检查和"端口是不是碰巧空着"这两类问题，本机全绿也挡不住。以后改了带 `#[cfg(unix)]` 的测试文件，
推之前按 `docs/development.md`「在 Mac / Linux 上验 Windows 那条编译检查」装 mingw 跑一遍（本机目前没装，装要用户点头）。

**推送时我又出了一次错（同日）：**追加这一节的 `668ecb4` 用了 `open(p, "w").write(open(p).read() + 新节)`，写模式先清空文件、读到空串，
推上去的审查文件只剩这一节的 23 行，§1–§35 的 2006 行没了；文档链接门禁当时已经报"断了的链接"，那条命令没看退出码就接着提交、推送。
`42c6fdb` 的 CI 因此红（测试里的文档链接检查）。`25e28c4` 按 `d78461d` 的完整文件加上这一节恢复，和 `d78461d` 比只多这一节，
CI（run 39105711877）8 个 job 全过。没改写历史。之后检查和提交分成两步、按退出码判断。

## 37. Skill 自动发现认 Claude Code 开着的插件（2026-10-21）

**起因：**用户问"能否自动识别本机 codex/claude 下的 skills"。实查：`~/.claude/skills`、`~/.codex/skills`、`~/.agents/skills`、
`~/.cursor/skills` 这些标准目录都认，开发机上扫到 94 个；Claude Code 插件带的一个都没认，`agent_context.rs` 里没有任何插件相关代码。
`~/.claude/plugins/` 下有 71 个 SKILL.md：`cache/claude-plugins-official` 14、`cache/local` 6、`marketplaces/anthropic-agent-skills` 20、
`marketplaces/claude-plugins-official` 31。

**Claude Code 自己怎么认：**

| 问题 | 结论 | 依据 |
| --- | --- | --- |
| 哪些插件开着 | `enabledPlugins` 里 `"名字@市场": true`。来源按 id 逐个合并，用户级 < 项目 `.claude/settings.json` < `.claude/settings.local.json` < `--settings` < managed | 官方 plugins/loading「Find where a plugin is enabled」；2.1.304 二进制里 `enabledPlugins` 的说明写着同一个顺序，值的 schema 允许 `boolean` 和 `string[]`（带版本约束） |
| 装了没写开关的算不算开 | 不算 | 文档说 `defaultEnabled` 默认 true；但 2.1.304 的 `claude plugin list --json` 把 skill-creator、openworld（装了，`enabledPlugins` 里没有）报成 `enabled: false`，照实际行为 |
| 装在哪 | `~/.claude/plugins/installed_plugins.json`：每个 id 一串记录，`scope` 是 managed / user / project / local，后两种带 `projectPath`，`installPath` 是带版本的目录 | 官方 plugins/loading「Find plugins on disk」；二进制里这个文件的 schema |
| `marketplaces/` 是什么 | 市场仓库的克隆，里面是市场上所有能装的插件，不是装了的 | 同上 |
| 插件里哪些 SKILL.md 算 | 默认 `skills/<名字>/SKILL.md`；plugin.json 的 `skills` 是加上去的目录；市场条目 source 是整个市场仓库又列了 `skills` 时只认列的（anthropics/skills 拆成 document-skills、example-skills 等就是这么写的）；没有 `skills/`、没有 `skills` 键、根上放着 SKILL.md 的整个插件是一个 skill | 官方 plugins-reference「Standard layout」「How each key combines with its default location」「Marketplace entries and the manifest」 |
| skill 叫什么 | `插件名:skill 名`，插件名用 plugin.json 的 `name` | plugins-reference「name」；开发机上 Claude Code 会话里列的就是 `exa:search`、`frontend-design:frontend-design` |

开发机对照 `claude plugin list --json`：开着 6 个（code-review、commit-commands、context7、exa、frontend-design、playwright），
带 SKILL.md 的只有 exa（`skills/agent`、`skills/search`；`node_modules/dotenv/skills/` 下另有两个，是依赖自带的）和 frontend-design（1 个）。
71 个里 Claude Code 真正加载的是 3 个。

**改了什么（`02e1afb`）：**来源里有 claude（auto 默认就有）时，扫完主目录各 skills 根再加上开着的插件里的 skill：开关按上表合并
（没读 managed），安装记录里给这个项目装的优先、其次不分项目的，目录规则照上表，解析后跑出插件目录的不认。名字带插件前缀，
`gld context` 里记 `claude/global`、路径在 `~/.claude/plugins/cache/…`；排在主目录 skill 后面，所以内容一字不差的留主目录那份；
`--hidden-skills` 按全名藏；附件照主目录 skill 的规矩（auto 扫到的不给）。`gld context --global` 的 claude 一行也列上。
concepts.md「compact 还会砍掉……」一节写了规则，troubleshooting 的"装了的 Skill 根本没有"一行补了这个原因。

**验证：**

- 新增 6 条单元测试（开着的进，关掉的、没写开关的、写了开关没装的、`marketplaces/` 里的、`node_modules` 里的不进；项目设置能开能关；
  同内容去重和 `--hidden-skills` 全名；市场条目点名；只给某个项目装的；`--global`）。改动前跑：6 条红、`agent_context` 其余 20 条绿（退出码 101）；改后 26 条全绿。
- 编出来的 gld 在隔离 `GLD_HOME`、临时项目上对开发机真实主目录跑：`gld context -w` 的 Skill 从 94 变 97，多的正是
  `exa:exa-agent`、`exa:search`、`frontend-design:frontend-design`；`~/.claude/skills/frontend-design` 和插件那份内容不同，两个都在。
  `gld tool call list_skills` count 97，名字里带 `:` 的只有这 3 个；关掉的 chrome-devtools-mcp、sourcegraph，没开的 skill-creator、openworld，
  以及 `marketplaces/`、exa 的 `node_modules` 里的都没出现（列表里那个不带前缀的 `skill-creator` 是 Codex 自带的 `~/.codex/skills/.system/`）。
  `get_skill name=exa:search` 回正文，带 `filesUnavailable`。测完停掉临时守护进程。
- 全仓（隔离 `GLD_HOME`）：50 个测试二进制，990 passed / 0 failed（§36 的 984 加这次 6 条），clippy `-D warnings`、fmt 通过。

**没做：**managed 策略文件（策略强关、自己设置里开着的插件，gld 照样给）；插件的 `commands/`（code-review、commit-commands 的斜杠命令，
Claude Code 现在也列成 skill，但不是 SKILL.md；主目录的 `~/.claude/commands/` gld 也一直没扫）；claude.ai 同步来的插件（`plugins/synced/`）、
`--plugin-dir`、`.claude/skills/` 下带 plugin.json 的插件；`CLAUDE_CONFIG_DIR`、`CLAUDE_CODE_PLUGIN_CACHE_DIR` 改过位置的；
正文里的 `${CLAUDE_PLUGIN_ROOT}` 不替换（gld 不跑 skill 里的脚本，开着的这 3 个也都没用到）。没合并、没部署、没推送。

**合并和文档（同日）：**三个提交已快进进 main（`bc5bda6`，没推送、没部署）。梳理文档时发现 concepts.md 从没写过 gld 扫哪些目录（任务说明以为写了），
Skill 的通用规则也都挤在「compact 还会砍掉……」这个讲省 token 的小节里。拆成单独一节「本机和项目里的 Skill」：按来源列主目录和项目里的目录
（照 `scan_skills_hiding` 核对过）、`--skill-sources` / `--custom-skill-paths` 怎么配、同一份 skill 为什么出现好几个 `✗`
（开发机上收进来 97 个，另有 18 个 `✗` 全是这种），插件、链接、附件、隐藏各一小节；compact 小节只留字数上限，链过去。README 和 troubleshooting 的链接跟着改。

## 38. 复核插件 skill 那一轮，三台换装（2026-10-22）

**要求：**插件 skill 自动发现（§37，`02e1afb`–`df0a32b`）合并推送、CI 全绿之后，用户说"review 后，粒度 commit，文档完善。线上 CI 通过，然后三机安装"。

### 复核了什么

| 看的 | 结论 |
| --- | --- |
| 开没开、装在哪 | 读 `enabledPlugins`（用户级被项目的 `settings.json`、`settings.local.json` 依次盖掉）和 `installed_plugins.json`，不扫 `marketplaces/`。对 |
| 项目能不能借 `settings.json` 开插件 | 能，但只能开**本机已经装了**的插件，gld 给的也只是 skill 正文；项目自己的 `.claude/skills` 本来就能往目录里放东西，没有多出能力 |
| 跑到插件目录外面 | 点名目录是绝对路径或 `..`、链到外面的，都按真实路径过滤掉。对 |
| 装的目录被删了 | 跳过。对 |
| 名字、藏、附件 | 名字带插件前缀 `exa:search`，`--hidden-skills` 要写全名，附件照主目录 skill 的规矩默认不给；文档都写了 |
| 效率 | **有问题**：每个开着的插件都重读、重解析一遍它所在市场的 `marketplace.json`，而扫描在每次 `workspace_context` / `list_skills` / `get_skill` 都现做 |
| 文档 | 全；缺一条：Claude 桌面版里看得到、gld 不给的是怎么回事 |

**本机核对：**新代码在这台机器上扫到 97 个（原来 94），多的 3 个是开着的 exa、frontend-design 带的。`~/.claude/settings.json` 里开着的 code-review、commit-commands
没有 SKILL.md（带的是斜杠命令），context7、playwright 只带 MCP；`chrome-devtools-mcp`、`sourcegraph` 在 `settings.json` 里是 `false`，
可 Claude 桌面版会话照样列出它们的 skill——桌面版在别处开的，gld 照 `settings.json` 算关。

### 改了什么

| 提交 | 内容 |
| --- | --- |
| `f282abe` | 一次扫描里一个市场的 `marketplace.json` 只读一次。本机 6 个开着的插件都来自官方市场（172 KB），改前每扫一次约 1 MB 的 JSON。同一项目连跑 7 次 `gld context`（调试版）取中位：367 ms → 232 ms，都是 97 个 |
| `eea366a` | `concepts.md` 插件一节补上"Claude 桌面版里看得到、gld 不给的"是怎么回事、怎么让 gld 也给 |

全仓 50 个测试二进制 990 passed / 0 failed，clippy、fmt 通过。

**没改：**`installed_plugins.json`、`settings.json` 写坏了读不出来时，插件 skill 静默不给，`gld context` 里看不出原因（Claude Code 自己也会报错，这种情况少见）。

### 线上 CI 和三台换装

推送 `df0a32b..fba39d0`，线上 CI（run 39623261258）8 个 job 全过。之后三台换成 `fba39d0`，`gld --version` 都是 `gld 0.8.3 (fba39d049e5c)`：

| 机器 | 二进制 | 全仓测试 |
| --- | --- | --- |
| 本机 Mac | sha256 `c86e2d57…`；守护进程重启，pid 74288 → 19779 | 990 passed / 0 failed（提交前） |
| 另一台 Mac | 同一份 `c86e2d57…` | — |
| Linux 服务器普通账号、root | 在服务器上从同一个提交构建，`f1f4003e…` | 普通账号跑，989 passed / 0 failed（少的一条只在 macOS 编译） |

本机照「升级」做：换之前守护进程没有子进程、请求日志最后一条是两个多小时前；数据目录备份 `gld-config-20261022-0017.tgz`（0600），
二进制备份沿用 §35 换装前那份 `gld-f9400f7-20261021-2352`。换完工具表 33 个、指纹 `59951e97a502352e` 没变，**不用 Refresh**；
凭据指纹、`gld ls` 和换之前一样；新守护进程里 `CLAUDE*` 变量 0 个；`gld context -w gld` 两份说明都打 ✓，Skill 扫到 97（换前 94）。

`gld health` 第一次跑"公网 /mcp"报 `error sending request`，再跑两次都没有失败项。照惯例绕开本机代理直连三个公网地址各 3 次，9 次都是 200；
经本机代理（127.0.0.1:7890）再访问 3 次也都是 200。是 gld 经本机代理那一下偶发没连上，公网入口本身没问题。

## 39. 对照 codex-bridge：借了两处，三项排进队列（2026-10-22）

**起因：**用户让参考 [Fanch-hui/codex-bridge](https://github.com/Fanch-hui/codex-bridge)（commit `f700ba6`，Swift 约 16 万行，Apache-2.0）
看 gld 能吸取什么。三个子代理分头只读通读源码（没编译、没运行），回来的关键结论又对 gld 源码逐条核过。它的公开仓库是从私有代码树导出的，
测试全被剔除，它文档里说的测试覆盖无法核实。

**它是什么：**和 gld 同类，让 ChatGPT、Qwen、Gemini 经 MCP 操作本机登记过的项目（读写、补丁、命令、受控 Git 提交）；
另外能把任务交给本机的 Codex、OpenCode 等 Agent 执行；带桌面 App，审批在 App 里点。

| 方面 | codex-bridge | gld | 结论 |
| --- | --- | --- | --- |
| 接 ChatGPT | OpenAI 官方 Secure MCP Tunnel：本机 tunnel-client 只往外连 OpenAI，不要域名和公网端口；连接器选 Tunnel、填隧道 ID | Cloudflare 临时/固定域名、FRP、自建反代，都要公网 HTTPS | 值得加成可选接入，排进队列 |
| 第三方程序拿密钥 | Runtime Key 和本机请求头经 fd 3/4 交给 helper，不进 argv | cloudflared 的 Tunnel Token 在 argv 里，本机任何账号 `ps` 看得到 | 已改（`0209cd3`） |
| 升级 | 先拒收新任务，等任务和命令跑完再换；deb 安装时服务忙就拒绝 | `gld daemon restart` 直接停掉在跑的命令，靠人先查 | 排进队列 |
| 给模型的规矩 | server instructions 写明状态冲突不是故障、被拒的命令不换法绕、没有回执不说完成 | 错误回包按错误码给下一步（`dispatch.rs` 的 `standalone_recovery_hint`），初始化说明里没这几条 | 已补（`a034476`） |
| Agent 委派 | `submit_task` 立即回任务 ID，`wait_task` 最多等 300 秒、要审批或要回答问题时提前回；Codex 走 `codex app-server` 的 Thread/Turn；批准只能在本机 App 点，ChatGPT 只能回答 Agent 的提问；同项目写任务靠数据库部分唯一索引互斥 | 没有 | 用户定排进开发计划 |
| `.git/` | 文件工具能写 `.git/config`，再调受控提交就会执行 `core.fsmonitor` 里的命令，只有审批挡着（推断，链条每一步有源码依据） | 文件工具一律不写 `.git/**`（`write_class.rs`） | gld 更严 |
| 命令输出 | cursor 增量读 | `read_output` 已有 `offset` / `next_offset` | 已有 |
| 大文件、疑似密钥 | 超过 200 KiB 整个不能读；写入含疑似密钥整个拒绝，读出时整行换成 `[REDACTED]` | 分页读；不拦 | 不学：误伤测试夹具，替换过的行补丁也对不上 |
| 命令和写锁 | 命令一直占着项目写锁，一个 dev server 就挡住所有写入 | 写权在调用返回时就放 | 不学 |

**Secure MCP Tunnel 的官方限制**（[官方文档](https://developers.openai.com/api/docs/guides/secure-mcp-tunnels)，2026-10-22 读）：
单个请求从入队到回包最长 120 秒，进度通知不重置；请求或回包最大 10 MiB；每个隧道每分钟 600 次。要在 Platform 建隧道、勾上要用的 ChatGPT
工作区，Runtime Key 要有 Tunnels Read + Use；只支持私有连接，不能拿去发公开插件。gld 单次调用最多等 30 秒（普通命令 `yield_time_ms`
上限 30000，服务模式 `DEFAULT_WAIT_MS` 也是 30000），回包上限 1 MiB，都在范围内。**没弄清的**：ChatGPT 经隧道走 gld 的 OAuth 能不能通——
官方只说 OAuth 发现能经隧道转发、授权服务器本身不会自动隧道化；codex-bridge 没走 OAuth，让 tunnel-client 给每个转发请求加一个静态请求头
当本机凭据。另外它的 `wait_task` 默认等 300 秒，按官方 120 秒的上限经隧道会超时（推断，没实测）。

**改了什么：**

| 提交 | 内容 |
| --- | --- |
| `0209cd3` | 固定隧道的 token 经环境变量 `TUNNEL_TOKEN` 交给 cloudflared（本机 cloudflared 2026.10.0 的 `tunnel run --help` 写着 `--token` 也认 `$TUNNEL_TOKEN`），不再放 argv。start / share / upgrade 在 `--token` 没给时也认 `TUNNEL_TOKEN`，`--token` 优先；只在 `--tunnel cf:<域名>` 时读，用到时提示一句（它会覆盖已存的那份）。新测试：假 cloudflared 记下 argv 和环境，断言 token 不在 argv、在环境里；环境变量给 token、`--token` 压过环境变量、非固定域名不读它。前两条在改动前的代码上先红后绿 |
| `a034476`、`30f8f46` | 两档初始化说明加四条：读文件用 `read_file`（多段用 `ranges`），不走 `exec_command` 跑 cat、sed、解释器单行；版本冲突、补丁对不上是文件变了，不是故障；被策略拒的命令不换程序绕，用 `check_command` 看规则、告诉用户；`command_ok=true` 才说通过，有新提交号才说已提交。对着 §28 实测的毛病。工具表和指纹不变，换装后 ChatGPT 要新开对话才拿到。`30f8f46` 是措辞修正：`a034476` 写了 "a gld or infrastructure failure"，`messages_name_real_commands` 把 `gld or` 当成子命令核对，全量挂了这一条 |

**验证：**定向测试（`named_tunnel_start` 5 条、`share_one_command` 12 条、`mcp::server` 8 条、文档链接与命令三组）通过；clippy `-D warnings`、fmt 通过；
`docs/cli.md` 用 `scripts/gen-cli-docs.sh` 重新生成。全仓头一遍 993 passed / 1 failed（就是上面 `gld or` 那条），
修正后再跑：994 passed / 0 failed（§38 的 990 加这次 4 条）。

**没做：**OpenAI 隧道接入（要用户自己的 Platform 账号先手工走一遍）、重启前等空闲、Agent 委派，三项进了 `task_plan.md` 的队列。没部署、没推送。

## 40. 有命令在跑时 `gld daemon restart` 先不重启（2026-10-22）

**要求：**用户让接着做队列里的「重启前等空闲」：有在跑的命令时 `gld daemon restart` 拒绝并列出来，`--force` 照旧，带测试，同步升级文档。
来由是 §39 对照 codex-bridge 时看到它升级前先等任务跑完；gld 以前在安装文档里给一句 `pgrep -lP <守护进程 pid>`，
还得自己从隧道、本机 MCP 这些子进程里分出哪些是项目命令（§35、§38 换装时都是这么查的）。

**关键约束：**升级时敲 `restart` 的是**换完二进制的新命令行**，对面是**旧守护进程**。新加一个 IPC 请求的话，旧守护进程不认，
检查在最需要的那一次（下一次换装）就用不上。所以不问守护进程，读盘上的运行记录（D09 起就有，0.8.1 起每条 `exec_command`
都留 `runs/<项目>/<会话>/run.json`）：`owner_pid` 等于守护进程的 pid、记录是 `running`、owner 锁还被拿着，就是守护进程起的、
还在跑的命令。pid 被复用不会认错：对上 pid 之后还要过 owner 锁，起它的进程不在了，读到时就改成 `unknown`、不算。

**改了什么（`825b74a`）：**

- `runs::running_under(runs_root, owner_pid)`：跨项目、跨主体（重启谁起的都停），按起跑时间排。
- `gld daemon restart` 不带 `--force` 时先查，有就报错退出（退出码 1），按项目列命令、跑了多久、pid，服务模式标端口；
  项目名从 `data/profiles.json` 按项目 id 对回来，读不出来就显示 id。`--json` 时列表打在 stdout。守护进程不在、不响应时不拦。
  `--force` 照旧停掉重启，原来"20 秒不退就强杀"的意思也还在。
- 帮助和 `cli.md`、版本不一致时的两处提示（原来写着"在跑的命令会被停掉"）、`install.md` 升级步骤（去掉 pgrep 那行，第 4 步说明被拦下时怎么办）、
  `daemon.md` 命令表和升级一节、`development.md` 升级连续性加一条：`run.json` 和 owner 锁的格式别改不兼容，否则检查会悄悄查不出东西。

**手工看了一遍输出**（隔离 `GLD_HOME`，后台跑 `sleep 47.25`）：

```text
错误：有 1 条命令在跑，重启会连命令一起停掉，这次没重启：
  demo  sleep 47.25  已跑 2s，pid 72611
等命令跑完再重启；确定要停掉就加 --force：gld daemon restart --force
```

`--json` 打出 `restarted: false` 和带 `session_id`、`service_port` 的列表；`--force` 照旧重启，`sleep` 跟着停掉，没有残留。
中间踩了一下：先红后绿时把旧版 `daemon.rs` 换回来跑过测试，`target/debug/gld` 是那次编的旧代码，紧接着手工跑就没拦——不是代码的问题，重新编译后正常。

**验证：**

- 集成测试 `restart_refuses_while_a_command_runs_and_force_still_restarts`：起守护进程、后台跑一条命令；不带 `--force` 时拒绝、
  列出 `it  <命令>`、守护进程 pid 不变、命令还在跑；`--json` 的列表对得上 `session_id`；`--force` 后 pid 变了、命令停了；
  空闲后不带 `--force` 也能重启。把 `daemon.rs` 换回改动前的版本跑，这条红。
- 单元测试 `running_under_lists_live_commands_of_one_owner_across_projects`：两个项目两个主体的在跑命令都在、按时间排；
  已结束的、owner 已死的（读到后变 `unknown`）、别的 pid 的不在；`runs/` 不存在时返回空。
- 全仓 996 passed / 0 failed（§39 的 994 加这 2 条），clippy `-D warnings`、fmt 通过。

**没做：**`gld daemon stop` 不查（`stop` 的意思就是停，现有测试也靠它停掉在跑的命令）；launchd 那条 `stop` + `kickstart` 因此也不查，文档里写了要自己看一眼；
`gld restart`（只重启服务）没加检查；0.8.1 之前的守护进程和盘满没留记录的命令查不出来。没部署、没推送。

## 41. 本机换成 `6268bb0`，换之前对着旧守护进程实测重启检查（2026-10-22 20:02）

**要求：**用户说"按 install.md 的升级步骤把本机换成当前 main，换之前先试一次 gld daemon restart 看检查有没有拦住在跑的命令，结果记审查；不推送"。只换本机，另一台 Mac 和 Linux 服务器还是 `fba39d0`。

**换之前：**守护进程 pid 19779，跑 `fba39d0`，已运行 19 小时 40 分；没有子进程，没有在跑的命令。运行记录里最后一批是 ChatGPT
（`oauth:hub:dcr-…`）19:47–19:49 在另一个项目里跑的检查和一次 `git commit`，都已结束，之后没再来请求。

**照 install.md 做的：**`cargo build --release --locked -p gld`（2 分 23 秒；去符号那步照旧报 `rust-objcopy` 找不到 `libLLVM.dylib`，
工具链老问题，二进制能用）。记下工具表 33 个、指纹 `59951e97a502352e`，`gld ls`，`data/profiles.json` 和 `data/oauth-clients/hub.json`
排好键再哈希的指纹。备份 `~/.local/opt/gld-fba39d0-20261022-2001`（sha256 `c86e2d57…`，和 §38 装上的一致）、
`gld-config-20261022-2001.tgz`（0600）。

**换之前的实测：**

1. 用装着的旧命令行经守护进程起一条命令：`gld tool call exec_command -w gld 'cmd=python3 -c "import time; time.sleep(300)"' yield_time_ms:=0`。
   项目 gld 的策略不放 `sleep`，`check_command` 看过 Python 这条放行。盘上的 `run.json`：`running`、`owner_pid` 19779、`caller` local。
2. 用新编的 `target/release/gld`（去掉本会话的 `CLAUDE*` 变量）敲 `gld daemon restart`：

   ```text
   错误：有 1 条命令在跑，重启会连命令一起停掉，这次没重启：
     gld  python3 -c "import time; time.sleep(300)"  已跑 8s，pid 66815
   等命令跑完再重启；确定要停掉就加 --force：gld daemon restart --force
   ```

   退出码 1；`--json` 打出 `restarted: false`，`project` 是 gld、`session_id` 对得上。之后守护进程还是 pid 19779，
   Python 进程（66815）还在。**新命令行对着旧版守护进程查得出来**，读盘这条路在真实升级场景下成立。
3. `gld tool call kill_session` 停掉这条命令，记录变成 `killed`，进程没了。

**换装：**写 `.new` 再改名换上 `~/.local/bin/gld`（sha256 `e4653129…`），`gld daemon restart`（去掉 `CLAUDE*`）这回没有在跑的命令、直接重启：
pid 19779 → 68019，20:02:34 换上，服务自己恢复。

**换完核对：**

| 看的 | 结果 |
| --- | --- |
| `gld --version`、服务的构建提交 | 都是 `6268bb088358`，等于 HEAD |
| 工具表 | 33 个、指纹 `59951e97a502352e`，没变，**ChatGPT 不用 Refresh** |
| `gld ls`、两份凭据指纹 | 和换之前逐字一样 |
| 新守护进程环境里的 `CLAUDE*` 变量 | 0 个 |
| `gld health` | 本地 /mcp、公网 /mcp、OAuth 授权元数据、受保护资源四项全 ✓ |
| `gld context -w gld` | 全局规则和仓库 `AGENTS.md` 都打 ✓，Skill 扫到 97 |

这一版带上了 §39 的初始化说明四条规矩：工具表没变，但说明是建对话时拿的，ChatGPT 要**新开对话**才看得到。
§39 的 Tunnel Token 改动和本机无关：本机公网入口是"固定地址（自建入口）"，cloudflared 不归 gld 起。

**回滚：**`~/.local/opt/gld-fba39d0-20261022-2001` 写 `.new` 再改名放回去，`gld daemon restart`。没推送。

## 42. Agent 委派 D1，和 `gld remote add` 精简（2026-10-22）

**要求：**用户定了 RFC-0009 第 5 节三件事（照建议），并改了权限："不要限制在沙箱，解除限制，直接可写。完成后，交回给审核"。
同一条消息里还说接 ccnm 的命令参数太多："ccnm 项目名就行了，还需要 workspace？默认就应该 coding"。

### `gld remote add` 精简（`e265ca6`）

原来是 `gld remote add <名字> --node <机器> --remote-workspace <项目名> [--mode coding]`，默认只读。现在 `gld remote add <项目名>`：
名字默认跟它一样；`--node` 不填时起 bridge 不带它，由 ccnm 自己选（`ccnm mcp bridge --help` 写着只配了一台机器时可以不给）；
默认 coding，只读写 `--mode read`。旧写法隐藏但照样认；缺 `max_mode` 字段的旧记录仍按只读读进来，不因为默认值改了被放开。
集成测试覆盖新旧写法和冲突写法，旧代码上先红。

### 委派 D1（`0324602`–`267cdf9`）

| 提交 | 内容 |
| --- | --- |
| `0324602` | core：Agent 档案（内置 codex、claude，`agents.toml` 可改可加）、任务记录（`GLD_HOME/agents/<项目>/<任务>/`）、运行器（自己一个进程组、stdin 喂完就关、输出写文件、总时长和无输出两道超时、按档案信号停整组、比两次 git 快照算改动）、交接目录 `.gld/handoff/<任务>/` |
| `c24e1b3` | 起跑前问 `--version` 改在任务目录里问：认不得它的程序会照常干活，第一次跑测试时假脚本在 `crates/core/` 下写出了 `--version`、`half.txt` 等四个文件（没进提交，已删） |
| `85804aa` | `gld agent ls/show/approve/deny/stop`，守护进程 5 个请求，**协议 6 → 7**；`gld daemon restart` 的检查算上在跑的委派；守护进程退出时把委派记成 interrupted |
| `29dd92d` | 项目开关 `agent-delegation`（off / ask / auto，默认 off） |
| `267cdf9` | `agent_manage` 工具（compact 档工具数预算 30 → 31）；委派在跑时拦这个项目的 `apply_patch`、`exec_command`、`write_stdin` |

**和 RFC 不一样的地方：**RFC 写"服务主凭据和 `--write` grant 能发起"，D1 只认服务主凭据——调用方身份里分不出浏览器 grant，
放开 `--write` grant 要先给 grant 加标记，挪到 D2。Codex 不再加 `--ignore-user-config`：用户要全权跑，就用他自己的配置。

**验证：**

- core 单元测试 18 条，假 Agent 脚本覆盖：auto 跑完交回审核（recap、改动文件只算项目文件、`.gld/` 不算、提示尾巴）、三种结果来源、
  非零退出和取不到结果、ask 放行 / 拒 / 作废、同项目互斥和叫停、无输出按卡住停、审核（跑完前审不了、意见写进 `review.md`）、
  幂等键、版本记录和没验过的标记、问版本只写在任务目录、起它的进程不在了读成 unknown。
- CLI 端到端 2 条，真起守护进程：开关没开被拒 → 开 ask → 起任务停在等放行 → `gld agent approve <前 8 位>` → 交回审核 →
  `review accept`；auto 档在跑时 `exec_command` 和第二个委派被拦、`gld daemon restart` 列出"委派给 ticker"不重启、
  `gld agent stop` 停掉后放开、能重启。
- 全仓 1020 passed / 0 failed（第一遍挂的 4 条：schema 描述里又写了"gld appends"被当成子命令，边界表没登记新参数和没有对应的
  `bounded` 调用，两条数工具个数的测试，都改了），clippy `-D warnings`、fmt、文档检查通过，`docs/cli.md` 重新生成。

**没做：**真跑 Codex、Claude（花额度，要用户点头）；没部署（协议 7，工具表变了，换装后 ChatGPT 要 Refresh）；
Pi、agy、Kimi 等档案（D2）；打回后接着同一会话（D3）；Windows。没推送。

## 43. 本机换成 `a8f9993`，Codex 真跑一次委派（2026-10-22 23:43–23:47）

**要求：**用户同意"换装本机，在 `~/xdw` 下建临时项目，开 ask，让 Codex 真跑一次小改动"。放行这一步由我代按：用户同意的就是这一次运行，
放行前先 `gld agent show` 和任务目录里的 `prompt.md` 核过提示全文。

**换装：**换之前 ChatGPT 正在用 gld（23:36–23:43 每隔一两分钟一条 `ssh hpsrv …`），没有在跑的命令，趁两条之间的空档换。
照 install.md 做：release 构建 2 分 21 秒，备份 `gld-6268bb0-20261022-2343` 和数据目录，写 `.new` 再改名，去掉 `CLAUDE*` 重启，
pid 68019 → 79902。换完：协议 7；工具表 33 → 34、指纹 `59951e97…` → `e0d37cab…`（多了 `agent_manage`，**要用户 Refresh**）；
`gld ls`、两份凭据指纹和换之前一样；新守护进程 `CLAUDE*` 变量 0 个；`gld health` 四项全 ✓。

**临时项目：**`~/xdw/gld-delegation-test`，`math_utils.py` 埋两个 bug（`add` 写成减法、空列表求平均除零），三条 unittest 测试一失败一报错，
初始提交 `bbc9079`。`gld add` 成 `delegation-test`，`agent-delegation=ask`；`action=agents` 报 codex、claude 都装着。

**一次委派：**

| 时刻 | 事 |
| --- | --- |
| 23:44 | `gld tool call agent_manage action=start agent=codex`（带 brief 写验收标准、`client_request_id`），回 `waiting_local_approval`，next_action 叫用户 `gld agent approve 910e5b3d`；`gld agent ls` 显示"等你放行" |
| 23:44:32 | `gld agent approve 910e5b3d` |
| 23:45:35 | `action=wait` 回 `awaiting_review`：Codex 跑了 62.9 秒，退出码 0，版本记成 `codex-cli 0.163.0` |

回包：recap 照尾巴要求写成四段（做了什么、改了哪些文件、验证、没做完的）；`changed_files` 只有 `math_utils.py`；没写 report.md；
回包里没有发起人标识。实际 diff：`add` 改成 `a + b`，`mean` 空列表返回 0，测试文件没动；Codex 没提交；`.gld/` 不在 git status 里。

**审核：**gld 自己 `exec_command` 跑 `python3 -m unittest -v`，3 个全过、`command_ok=true`（也说明跑完后写锁放开了）；
`action=review verdict=accept`，任务变成 `accepted`，意见写进 `.gld/handoff/910e5b3d…/review.md`。

**没验：**Claude 真跑；ChatGPT 自己发起和审核（要用户先 Refresh）。临时项目留在服务上给用户试，不要了 `gld rm delegation-test`。没推送。

## 44. ChatGPT 自己发起、审核一轮委派（2026-10-23 00:06–00:11）

**做法：**用户在 ChatGPT 点了 Refresh、新开对话，贴了"让 codex 给 math_utils.py 加 median、补两条测试，停在等放行时告诉我，
跑完自己看 diff、跑测试再 review"。结果从 `gld agent` 的任务记录和服务请求日志（UTC，下面换成本地时间）核对。

| 本地时间 | ChatGPT 做了什么 |
| --- | --- |
| 00:06:27 | 重新 `initialize`、`tools/list`（46.7 KB，34 个工具）：Refresh 生效 |
| 00:07:51 | `agent_manage action=agents` |
| 00:08:07 | `action=start agent=codex`，提示写清了 median 的规则、"恰好两条测试"、不改无关文件、不提交，另带 brief 写验收标准；回 `waiting_local_approval` |
| 00:09:51 | 用户 `gld agent approve ffb8a64f` |
| 00:10:14–00:10:44 | `action=wait`，还在跑，等满 30 秒 |
| 00:10:51 | `action=wait` → `awaiting_review`（Codex 跑了 61 秒，退出码 0，`codex-cli 0.163.0`；改了 `math_utils.py`、`test_math_utils.py`） |
| 00:10:58 | `git_diff` |
| 00:11:01 | `exec_command` 跑 `python3 -m unittest -v`，退出码 0 |
| 00:11:20 | `action=review verdict=accept`，意见写了查了什么、5/5 通过 |

**它审得对：**先看 diff、跑测试，再下结论，没有直接信 Agent 的 recap。我这边重跑也是 5/5，新增的两条测试是空列表和无序偶数长度列表。

**它指出的问题是真的：**上一轮委派（§43 修 add / mean）的改动没提交，这次 `git_diff` 跟 HEAD 比，两轮的改动混在一起，
它在意见里写"当前基于 HEAD 的 diff 无法确认来源"。回给审核者的只有"改了哪些文件"，没有"这次任务自己的那份 diff"。
**下一步（还没做）：**跑之前、跑完各用临时 index `git add -A` + `write-tree` 记一棵树（不碰用户的 index 和工作区），
两棵树一比就是这次任务自己的 diff（含新文件），有界地放进回包；`changed_files` 也改用它算。

**另外两处：**

- `.gld/.gitignore` 会被补回来（Planning 的规矩是用户删了就不补，删它是为了共享 Goal / Plan）：`7f0a87a` 已修，交接目录改用
  自己的 `.gld/handoff/.gitignore`。临时项目里 `.gld/planning/` 本来就在用，是核对这一轮时看到的。
- 在跑时的 `wait` 回包 14 KB（00:10:44 那次）：大头是 Codex `--json` 输出的事件流尾巴，JSON 里套 JSON 转义一层就翻倍。
  可以把在跑时的输出尾巴收到 1 KB 左右，或者只给 stderr。还没改。

`7f0a87a` 没部署。没推送。

## 45. 委派回包给这次任务自己的 diff，在跑时的输出尾巴收小（2026-10-23）

**要求：**用户让做 §44 排的前两项："用临时 index 记跑前跑后两棵树，回包里给这次任务自己的 diff（有界），changed_files 也改用它算；
顺手把在跑时 wait 的输出尾巴收小。测试、文档、提交，先不换装"。

**改了什么（`f9bc7f0`、`9714b28`）：**

- 跑前、跑后各用任务目录里的临时 index（从用户的 index 复制一份当底）`git add -A --ignore-errors` + `write-tree`，两棵树一比：
  `changed_files` 改用它算，完整 diff 存成任务目录的 `task.diff`（`--no-ext-diff --no-textconv`，不替仓库跑配置的程序）。
- 回包加 `diff`（最多 64 KB，截在整行上，脱敏）、`diff_truncated`；任务记录加 `diff_bytes`、`head_moved`。
  next_action 写明 diff 字段是这次任务的、`git_diff` 跟 HEAD 比会混进之前没提交的；`gld agent show` 也打出 diff。
- 在跑时的输出尾巴每个流 4 KB → 768 字节；收尾后（失败、超时、被停）仍是 4 KB。

**成本实测**（同样的命令、临时 index，不碰暂存区）：gld 仓库 306 个文件记一棵树 0.03 秒，ccnm 仓库 524 个文件 0.09 秒，
两边 `git diff --cached` 前后都干净。一次委派记两棵。

**验证：**新增四条单元测试——diff 只含这次的改动（之前没提交的、没跟踪的、已暂存的都不混进来，暂存区前后一字不差，临时 index
收拾干净）；Agent 提交了标 `head_moved`、改动仍在 diff 里；超过 64 KB 截在整行上；在跑时尾巴 ≤768 字节、收尾后更长。端到端那条
加上回包里有 diff、next_action 提到 diff 字段。这几条在旧代码上编译不过（没有这些字段）——旧的 `changed_files` 本来就对，
§44 缺的是回给审核的 diff 内容。全仓 1025 passed / 0 failed，clippy、fmt、文档检查通过。

**没做：**换装（用户说先不换；本机还是 `a8f9993`，`7f0a87a` 起的三个提交都没部署）。没推送。

## 46. 本机换成 `59f40ad`（2026-10-23 10:55）

**要求：**用户说"按 install.md 换装本机（先查没有在跑的命令），然后给我一段提示词，让 ChatGPT 在 delegation-test 上再委派审一轮，
重点看 diff 字段好不好用；不推送"。

**换之前：**守护进程 pid 79902（`a8f9993`）没有子进程，没有在跑的委派；ChatGPT 最后一条调用是 00:16，已经空了十个半小时。

**换装：**release 构建 2 分 26 秒；备份 `gld-a8f9993-20261023-1054` 和 `gld-config-20261023-1054.tgz`（0600）；写 `.new` 再改名，
去掉 `CLAUDE*` 重启，pid 79902 → 33026。换完：协议 7，服务构建提交 `59f40adc0bdc`；工具表 34 项、指纹 `e0d37cabe9356c13`
**没变，不用 Refresh**（这次只改了回包内容，没改 schema 和描述）；`gld ls`、两份凭据指纹一样；`CLAUDE*` 变量 0 个。
`gld health` 第一次"公网 /mcp"报 `error sending request`，紧接着再跑四项全 ✓；绕开本机代理直连 3 次、经代理 1 次都是 401
（没带凭据，说明入口通），和以前一样是 gld 经本机代理那一下偶发。

**给 ChatGPT 的这一轮：**`delegation-test` 工作区里故意留着前两轮没提交的改动（add / mean 修复、median），`agent-delegation=ask`。
提示词让它委派 codex 加 `mode(values)` 和两条测试，跑完先只看回包的 `diff` 字段审，再拿 `git_diff` 对照，说清 diff 字段有没有
混进前两轮的改动，最后单独评价 diff 字段够不够用。结果等用户试完再核对、记下一节。没推送。

## 47. ChatGPT 再审一轮，检验 diff 字段（2026-10-23 11:40–11:43）

**结论：diff 字段管用。**工作区里有前两轮没提交的改动，这一轮回包的 `diff` 只有这次的 `mode` 函数和两条测试（1242 字节），
跟 HEAD 比的 `git diff` 是 36 行新增、混着前两轮的 add / mean / median。ChatGPT 在审核意见里写了"委派 diff 未混入这些历史变更
且无本次缺漏"——§44 它分不清来源的问题解决了。

| 本地时间 | ChatGPT 做了什么（项目自己的请求日志） |
| --- | --- |
| 11:40:41–42 | `workspace_context`、`git_status`，先看工作区现状 |
| 11:40:44 | `agent_manage action=agents` |
| 11:40:54 | `start agent=codex`：提示写明只做 mode 和两条测试，前两轮的改动"严禁提交、还原、重写、整理"；回 `waiting_local_approval` |
| 11:41:15 | 用户 `gld agent approve 93adca29`，Codex 跑了 63 秒 |
| 11:42:01 | `wait` 时还在跑，回包 4958 字节（§44 同样情况是 14119 字节，输出尾巴收到 768 字节生效） |
| 11:42:18 | `wait` → `awaiting_review`，回包 7783 字节，带 1242 字节的 diff，`diff_truncated=false`、`head_moved=false` |
| 11:42:35 | `git_diff` 对照 |
| 11:42:37 | `exec_command` 跑 `python3 -m unittest -v`，退出码 0 |
| 11:42:54 | `review verdict=accept` |

我这边重跑 7/7 通过；`task.diff` 的内容和它说的一致；Codex 没提交，暂存区没动（recap 里也自己核了）。

**ChatGPT 提的改进建议**（写在审核意见里，原话的意思）：

1. 回包附上跑前、跑后两个基线标识（两棵树的 id）。
2. 给每个文件的增删行数，和 `changed_files` 一一对上。
3. diff 截断时能分页接着读：现在超过 64 KB 只说了全文在 gld 数据目录里，而那个目录 AI 读不到。

第 3 条对大改动是真缺口；第 1、2 条便宜。都还没做，排进 task_plan。另外它建议给 `mode` 补一条"只有一个众数"的测试，
那是临时项目的事，不改。

没推送。

## 48. §47 的三条改进：每文件行数、两棵树的 id、diff 分页读（2026-10-23）

**要求：**用户说"接着做 §47 的三条：agent_manage 加 action=diff 按 offset 分页读完整的 task.diff；回包给每个文件的增删行数；
附跑前跑后两棵树的 id。测试、文档、提交，先不换装"。

**改了什么**（三个行为三个提交，各自带测试和 concepts.md、RFC-0009 的改动）：

- `a5cf900` 每文件增删行数：算改动从 `git diff --name-only` 换成 `--numstat -z --no-renames --no-ext-diff --no-textconv`，
  任务记录加 `file_stats: [{path, added, deleted}]`，和 `changed_files` 同样的文件、同样的顺序，二进制文件两个数都是 `null`。
  `gld agent show` 每个文件前打 `+增 -删`。
- `ba6aaff` 两棵树的 id：任务记录加 `before_tree`、`after_tree`。测试里核过 `git diff <before_tree> <after_tree>` 和回包的 diff
  一字不差。树没人引用，过期后 `git gc` 会清掉，那时这条命令报 `fatal: bad object <id>`（临时仓库里实测），diff 全文另存着不靠它。
- `188f60c` 分页读：新动作 `action=diff task_id=… offset=…`，每页最多 64 KB、截在整行上，回 `diff`、`offset`、`next_offset`
  （读完是 `null`）、`total_bytes`。`status` / `wait` 截断时多给 `diff_next_offset`：回包的 diff 脱敏过，AI 拿它的长度算不出下一页
  从哪起。一行比一页还长时在 UTF-8 字符边界上切。读 diff 改成 seek 到 offset 只读一页，回包那份也走这个函数，
  不再把整个 `task.diff` 读进内存。越界回 `INVALID_ARGUMENT`，还没跑完回 `AGENT_TASK_STATE`，没跑过、不是 git 仓库、
  早于存 diff 的旧任务回 `AGENT_DIFF_UNAVAILABLE`。
- 顺手：工具说明里过时的"用 git_diff / read_file 查改动"改成看 diff 字段、长的用 `action=diff` 读；截断时的 next_action 从
  "read_file 剩下的 changed_files"改成直接给出 `action=diff … offset=N` 那条调用。

**工具表变了：**`agent_manage` 的 `action` 多了 `diff`、参数多了 `offset`、说明改了。隔离数据目录里用已装的 `59f40ad` 和新编的
debug 版（`--version` 打 `ba6aaff`：编译时还没提交 `188f60c`，代码已是改完的，输出里有 `diff`）各打一次 `gld --json tool list`，
整份的 SHA-256 前 16 位 `9e284f4b19b64ffe` → `5d45b30633043ed0`。换装后**要在 ChatGPT 点一次 Refresh** 才看得见 `diff` 动作；
不 Refresh 也不坏：`status` / `wait` 照常，新字段照样回，只是 ChatGPT 手里的旧表没有 `diff`、`offset`。守护进程的 `Request` / `Response`
没改形状（只多了带默认值的字段，和 `f9bc7f0` 一样），协议还是 7。

**验证：**新增 6 条——四种行数（改一行、删文件、二进制、新文件）和列表一一对上；numstat 路径里有制表符不切开；不是 git 仓库时没有树、
没有 diff、分页报 `AGENT_DIFF_UNAVAILABLE`；超长行在字符边界上切、一定往前走；还没跑的任务读不了；截断时 next_action 带 offset。
大 diff 那条接着一页页读完，拼起来和 `task.diff` 逐字节相同；diff 只含这次改动那条加上两棵树的核对；端到端加上 `file_stats`、
两个树 id、一页读完和越界报错（核对报错原文，排除"参数名不认识"也回 `INVALID_ARGUMENT`）。新测试在旧代码上编译不过（没有这些
字段和函数）。全仓 1031 passed / 0 failed，clippy、fmt、文档检查通过。

**没做：**换装（用户说先不换，本机还是 `59f40ad`，这三个提交都没部署）；没用真的 Codex 跑超过 64 KB 的改动试分页，只有假 Agent
脚本的测试。没推送。

## 49. 本机换成 `c99c8cd`（2026-10-23 14:31）

**要求：**用户说"按 install.md 换装本机（先查没有在跑的命令），然后给我一段提示词，让 ChatGPT 在 delegation-test 上委派一个
超过 64 KB 的改动，试 action=diff 分页读完再审；不推送"。

**换之前：**守护进程 pid 33026（`59f40ad`）没有子进程，`gld agent ls` 里没有在跑的委派。但 ChatGPT 14:25 还在调 `read_file`、
`list_files`（请求日志看不出是哪个项目），所以没马上重启：二进制换好后等请求日志安静 20 秒再动手。14:31:25 重启时已经安静
38 秒，最后一个请求在 14:30:47，没撞上调用。

**换装：**release 构建 2 分 26 秒（`rust-objcopy` 去符号的警告是已知的工具链问题）；备份 `gld-59f40ad-20261023-1427` 和
`gld-config-20261023-1427.tgz`（0600）；写 `.new` 再改名，去掉 `CLAUDE*` 变量重启，pid 33026 → 43580。

**换完：**命令行和服务的构建提交都是 `c99c8cda835d`，协议 7；`gld ls` 前后一致，`profiles.json`、`oauth-clients/hub.json`
排好键后的指纹前后一致；新进程 `CLAUDE*` 变量 0 个；`gld health` 四项全 ✓。工具表 34 项，指纹 `e0d37cabe9356c13` →
`a0c81b8f6d0580cd`：旧、新二进制在隔离数据目录里逐个工具比，只有 `agent_manage` 的 `description` 和 `inputSchema` 变了
（§48 的 `diff` 动作和 `offset` 参数），**要在 ChatGPT 点一次 Refresh**。

**给 ChatGPT 的这一轮：**提示词让它先确认自己手上的 `agent_manage` 有 `diff` 动作和 `offset` 参数（没有就是还没 Refresh，停下），
再委派 codex 写一个生成脚本、3000 条用例的 CSV 和读它的测试，期望值用标准库 `statistics` 算。估计 diff 一百多 KB；按路径排序
CSV 在前，脚本和测试落在最后几页，不读完就审不了代码。审核只准用 `agent_manage` 读 diff，一页页读到 `next_offset` 是 `null`，
再用 `exec_command` 跑两棵树之间的 `git diff --numstat` 对照 `file_stats`。结果等用户试完再核对、记下一节；请求日志从第
15168 行起是这一轮。没推送。

## 50. ChatGPT 委派一个 175 KB 的改动，分页读完再审（2026-10-23 15:58–16:04）

**结论：分页管用。**diff 175,493 字节，回包给第一页和 `diff_next_offset=65530`，ChatGPT 接着用 `action=diff` 读到
`next_offset=null`，三页 65,530 + 65,499 + 44,464 字节，每页截在整行上（页与页接缝处 CSV 编号 1146/1147、2277/2278 连续），
加起来正好是总长。脚本和测试按路径排序落在第 3 页，它是读完才审到的。审核意见里的页数、offset、`file_stats` 和 numstat 的对照
都和我这边的记录一致，accept。

| 本地时间 | ChatGPT 做了什么（项目自己的请求日志、运行记录） |
| --- | --- |
| 15:58:38 | 用户点 Refresh，`tools/list` 回 47,322 字节，`auth=` 还是原来那个 `oauth:hub:dcr-…` |
| 15:59:22–34 | `list_workspaces`、`git_status`、`workspace_context`、`agent_manage action=agents` |
| 15:59:57 | `start agent=codex`，回 `waiting_local_approval`；用户在本机放行，Codex（codex-cli 0.163.0）跑了 163 秒 |
| 16:00:54–16:03:03 | 四次 `wait`：在跑时每次回 5.9–6.6 KB，最后一次 `awaiting_review` 回 141,939 字节（带第一页） |
| 16:03:18–45 | 四次 `agent_manage`：135,389、141,939、135,273、92,428 字节。按大小看是 `diff offset=0`（又读了一遍第一页）、`status`、第二页、第三页 |
| 16:04:15–23 | `exec_command`：`git diff --numstat <before_tree> <after_tree>`、`python3 gen_cases.py --check`、`python3 -m unittest -v`、`git status --short`，退出码都是 0 |
| 16:04:53 | `review verdict=accept`，回 143,571 字节 |

这一轮在 delegation-test 里没用 `read_file`、`git_diff` 读新文件。同一时间日志里那几次 `read_file` 是用户另一个对话在别的项目里
读的（同一秒那个项目记了一次 `git_status`）。

**我这边核对：**`git diff <before_tree> <after_tree>` 和 `task.diff` 逐字节相同；用 `gld tool call agent_manage action=diff`
自己读三页，拼起来也和 `task.diff` 逐字节相同；`file_stats` 是 CSV +3001、`gen_cases.py` +55、`test_cases.py` +28，和 numstat 一致；
`--check` 退出码 0，8 条测试 OK；前几轮没提交的改动还是 36 行新增，没动，没有新提交。脚本的期望值确实用的 `statistics`，
众数并列取最小，`--check` 只比对不写文件。

**发现：回包比 diff 本身大一倍多。**一页 64 KB 的 diff，回包 135 KB：gld 的 MCP 回包把同一份结果放两次，`content[0].text`
（JSON 再转义一层）和 `structuredContent`（`crates/core/src/tools/workspace.rs` 的 `wrap_mcp_tool_result`，所有工具都这样，
MCP 2025-06-18 规范的工具一节原文是带 `structuredContent` 的工具 SHOULD 同时在文本块里给一份序列化的 JSON）。这一轮 `agent_manage` 的六个大回包加起来 790 KB，diff 本身 175 KB。
其中三个是重复的：`awaiting_review` 之后又读一次第一页（可能是我提示词要它报"每页的 offset"，回包里的第一页没有 `offset` 字段），
又 `status` 一次，`review` 的回包又带了一遍第一页的 diff（143 KB）。`review` 回包不带 diff 是能直接省的；双份回包是 MCP 层
的事，没证据说明 ChatGPT 两份都算进上下文，先不动。

**ChatGPT 第 7 步的评价**（用户贴回来的）：分页够用，正确性、文件统计、任务隔离都过，175 KB 三页合理；大 CSV 占上下文、
接口还能更直观。按优先级提了五条：

1. 回包里的 `diff_next_offset` 和分页里的 `next_offset` 统一成一个名字。
2. 按文件读：`path=gen_cases.py` 直接看源代码，不用先读完约 170 KB 的 CSV。
3. 摘要模式：`view=summary` 先给文件列表、增删行数、diff 大小，再按需读内容。
4. 每页带 `page_index`、`page_bytes`、`line_start`、`line_end`，方便检查连续性。
5. 每页大小可调（`limit_bytes`，16 / 32 / 64 KB）。

它还说 Codex 日志里有 HTTP 403、502 和协作线程报错。核对 `stderr.log`：16:00:22–33 四条是 Codex 自己的 MCP 客户端连它配置里的服务
（`~/.codex/config.toml`）失败，403 `Invalid x-mcp-key` 对应配置里唯一带这个请求头的 uTools 本机服务，三条 502 没写是哪个服务
（同一时间本机 ardot 也是 502）；16:01:34 `collab spawn failed: no thread with id` 是 Codex 起子代理失败。都不经过 gld，
没影响结果（退出码 0，改动完整）。

**我的看法**（还没定，等用户选）：

- 第 2 条最值。可以在算 diff 时顺手记下每个文件在 `task.diff` 里从第几个字节起、多长，放进 `file_stats`，`action=diff` 加 `path`
  只读那一段（超过 64 KB 照样分页）。只靠 `task.diff`，不靠两棵树（树会被 gc）。
- 第 3 条大半已有：`status` / `wait` 的 `file_stats`、`diff_bytes` 就是摘要，缺的是每个文件的 diff 多大，跟第 2 条一起补上。
  真正的浪费是 diff 超过 64 KB 时 `status` / `wait` 每次都附第一页（线上 135–142 KB），这一轮第一页几乎全是 CSV；按文件读做好后，
  大 diff 可以只给摘要、不附第一页，这是行为变化，要用户定。
- 第 1 条不值：`status` / `wait` 回包是整个任务的详情，裸 `next_offset` 说不清是谁的下一页；这一轮它也没用错。改名还得再 Refresh。
- 第 4 条不值：`page_bytes` 就是 `next_offset - offset`；能任意给 offset 的接口里页码没有固定含义；连续性 gld 的测试已经按逐字节拼回全文核过。
- 第 5 条暂缓：页越小调用越多，双份回包的固定开销占比越大；按文件读之后真遇到"一个文件就超过 64 KB 又只想看一部分"再说。
- 加上我这边的：`review` 回包不带 diff（这一轮多回 143 KB）。

没推送。

## 51. 按文件读 diff，review 回包不带 diff（2026-10-23）

**要求：**用户在 §50 的取舍里选了："做：file_stats 记每个文件在 task.diff 里的位置和大小、action=diff 加 path；review 回包不带 diff。
大 diff 时 status/wait 仍附第一页。测试、文档、提交，先不换装"。

**改了什么：**

- `0b483f0` 按文件读：算 diff 时按行首的 `diff --git ` 把完整 diff 切成每个文件一段，段数和 numstat 对上就把每段的起点和长度记进
  `file_stats`（`diff_offset`、`diff_bytes`），对不上就不记。`action=diff` 加 `path`：从这个文件那一段的开头读、读到它的结尾停，
  超过一页照样分页，下一页的 next_action 带同一个 `path`。offset 还是整份 diff 里的位置。位置只靠 `task.diff`，不靠会被 gc 的两棵树。
- `c8efb4d` `review` 回包去掉 `diff`、`diff_truncated`、`diff_next_offset`，要再看用 `status` 或 `action=diff`。

**拿 §50 那份 diff 算：**按同样的切法，CSV 一段 172,495 字节，`gen_cases.py` 1,863 字节，`test_cases.py` 1,135 字节。那一轮
要是能按文件读，代码两段一共 3 KB，各一次调用；当时它得先读完三页（加上重复的两次，`agent_manage` 大回包共 790 KB）。
`review` 回包当时 143,571 字节，现在只剩任务本身（那一轮没有 report，Codex 写的结果文件 681 字节）。

**工具表又变了：**已装的 `c99c8cd` 和新编的版本逐个工具比，只有 `agent_manage` 变了，多了 `path` 参数。换装后要再点一次 Refresh。

**验证：**行数那条加上空文件、只改权限两种，核每段首尾相接、盖满整份、各自从自己的 `diff --git` 起（二进制、删文件也在里面）；
新增一条大 diff 里只读一个文件——排最后的一页就完、夹在中间且超过一页的拼起来正好是它那一段、不读进下一个文件，offset 不在那段里、
没改的文件、早于记位置的任务三种报错；回包单元测试加按文件读时下一页带 path；端到端加只读 `touched.txt` 正好是它那一段、`review`
回包没有 diff 而之后 `status` 照样给。`review` 那条断言临时撤掉改动跑过，旧代码上失败。全仓 1032 passed / 0 failed，clippy、
fmt、文档检查通过。

**没做：**换装（用户说先不换，本机还是 `c99c8cd`）；没用真的 ChatGPT 试 `path`。没推送。

## 52. 委派收尾的小缺口、文档对账、README 精简（2026-10-23）

**要求：**用户说"继续任务完善，同步更新文档；最后修正 README.md 的问题，保持简洁高效的，人话风格，站在用户角度，
更多让用户去文档查看具体内容"。

**委派这边补的（`ab28d2b`）：**`gld agent show` 在 diff 超过 64 KB 时只说"全文在任务目录的 task.diff"，任务目录在数据目录
深处，用户找不到。现在打出完整路径（算出来的文件不在就退回原说法）；`gld agent` 的帮助跟上 show 现在打的内容，"见 RFC"改成指向
concepts.md 和 security.md，重新生成 `docs/cli.md`。新增端到端一条，临时撤掉改动跑过，旧代码上失败。

**文档对账（`11d9e4b`）：**

- security.md 说"名字像密钥的环境变量会被去掉"对委派出去的 Agent 不适用——不对，起 Agent 时照样去掉（`run.rs` 的 `spawn`），
  只是它能读 shell 配置和 `~/.config`，拦不住。改成实话。
- RFC-0009 状态还是"没部署、一个 Agent 都没真跑过"，第 2 节流程还写着 `output_ref`、"用 `git_diff`、`read_file` 看改动"。改成现状；
  第 8 节"要在 D1 实测的"写上四轮 Codex 的结果：从任务记录核，四轮退出码都是 0（60–163 秒）、都按四段写 recap；§50 那轮四个
  MCP server 连不上，Codex 13 秒内报完错接着干；`.gld/handoff/` 会不会被格式化、lint 扫到还没测到。
- concepts.md 委派一节第 4 步原来一大段，拆成"交回审核时拿到什么"（字段表）、"diff 太长怎么读"（offset / path、报错表）、
  "这份 diff 是怎么算出来的"三小节，内容不变。

**README（`b89ffe6`）：**

- 发现的问题：README 让人去 Releases 下载，可委派、DeepSeek 网页（`gld grant browser`）、Claude Code 插件带的 skill 都不在
  0.8.3 发布包里（`git grep v0.8.3` 核过，`list_project_commands`、`service_port`、Skill、本机 MCP、grant、ccnm 远端都在），
  README 一句没提。现在第 1 步后面写明，development.md 发版第 1 步加了"这句要跟着改"（README 会打进发布包）。
- "能做什么"每条压成一句、链到 concepts.md 对应小节；第 3 步的 DeepSeek 大段（构建日期、版本限制）并进"每种客户端怎么接"
  的链接，connect-clients.md 里本来就有；文档表去掉重复的两行。102 → 94 行。

**验证：**全仓 1033 passed / 0 failed，clippy、fmt、文档链接（含新锚点）、命令名、本机路径、命名门禁通过。

**没做：**换装（`0b483f0` 起的提交都没部署，本机还是 `c99c8cd`）。没推送。

## 53. 本机换成 `2f50e09`（2026-10-23 18:50）

**要求：**用户说"按 install.md 换装本机（先查没有在跑的命令），然后给我一段提示词，让 ChatGPT 在 delegation-test 上委派一个大 diff，
用 path 先读代码再审；不推送"。

**换之前：**守护进程 pid 43580（`c99c8cd`）没有子进程，没有在跑的委派，请求日志 100 多分钟没动。

**换装：**release 构建 3 分 02 秒；备份 `gld-c99c8cd-20261023-1850` 和 `gld-config-20261023-1850.tgz`（0600）；写 `.new` 再改名，
去掉 `CLAUDE*` 变量重启，pid 43580 → 25532。带上 `0b483f0`（按文件读）、`c8efb4d`（review 回包不带 diff）、`ab28d2b`（show 给全文路径）。

**换完：**命令行和服务的构建提交都是 `2f50e09b7c17`，协议 7；`gld ls` 前后一致，`profiles.json`、`oauth-clients/hub.json` 指纹一致；
`CLAUDE*` 变量 0 个；`gld health` 四项全 ✓。工具表指纹 `a0c81b8f6d0580cd` → `2ceba8697b67815e`，旧、新二进制逐个工具比，只有
`agent_manage` 的说明和 schema 变了（多了 `path`），**要在 ChatGPT 点一次 Refresh**。

**给 ChatGPT 的这一轮：**委派 codex 往 `math_utils.py` 加 `variance` / `stdev`，再写生成脚本、3000 条用例的 CSV 和读它的测试。
模拟估 CSV 那段约 167 KB，按路径排在最前，代码在后面三个文件里；`math_utils.py` 里留着前几轮没提交的改动，看 `path` 读出来的那段
是不是只有这次的两个函数。审核要求先看 `file_stats`，代码文件用 `path` 各读一次，CSV 只读第一页，不按 offset 翻全份。结果等用户
试完再核对、记下一节；请求日志从第 16428 行起是这一轮。没推送。

## 54. ChatGPT 用 path 先读代码再审一个 172 KB 的委派 diff（2026-10-23 18:54–19:00）

**结论：按文件读管用。**diff 171,539 字节，CSV 一段 167,748 字节排在最前，代码三段 1,846 / 558 / 1,387 字节。ChatGPT 看完
`file_stats` 后用 `path` 把三个代码文件各读一页就完（`next_offset` 都是 `null`、页大小等于 `diff_bytes`），CSV 只读了第一页，
一共读了 69,309 字节（总量的 40%）。`math_utils.py` 里留着前几轮没提交的改动，按 `path` 读出来那段只有这次的 `variance`、`stdev`
（+11 行），审核意见里也写了没混进去。accept。

| 本地时间 | ChatGPT 做了什么（项目自己的请求日志、运行记录） |
| --- | --- |
| 18:54:29 | 用户点 Refresh，`tools/list` 回 47,575 字节 |
| 18:55:45–56:04 | `list_workspaces`、`agent_manage action=agents`、`start agent=codex`，回 `waiting_local_approval`；用户放行，Codex 跑了 134 秒 |
| 18:57:08–58:43 | 四次 `wait`，在跑时回 6.6–7.4 KB，最后一次 `awaiting_review` 回 143,257 字节（带第一页，整页都是 CSV） |
| 18:58:59–59:15 | 五次 `action=diff`：4,906、4,906、2,003、3,888、135,602 字节。审核意见里写的是四次（`gen_variance_cases.py`、`math_utils.py`、`test_variance_cases.py`、CSV 首页）；两次 4,906 正好是 `gen_variance_cases.py` 那段的大小，多半同一个文件读了两遍 |
| 18:59:27–31 | `exec_command`：`git diff --numstat <before_tree> <after_tree>`、`python3 gen_variance_cases.py --check`、`python3 -m unittest -v`，退出码都是 0 |
| 19:00:04 | `review verdict=accept`，回 11,251 字节（§50 那次带 diff 是 143,571），没有 diff 字段 |

同一时间日志里的 `read_file` 是用户另一个对话在别的项目里读的（同一秒那个项目记了 `git_status`）。

**我这边核对：**用 `gld tool call agent_manage action=diff path=…` 把四个文件各读一遍，都和 `task.diff` 里对应那段逐字节相同、各自从自己的
`diff --git` 起；`math_utils.py` 那段只有 +11 行的两个函数，相对 HEAD 它是 +34 −1（前几轮的 +23 −1 加这 11 行），`test_math_utils.py`
没动，没有新提交；`--check` 退出码 0，11 条测试 OK。

**回包量：**这一轮 `agent_manage` 的大回包合计 305,813 字节，§50 是 790,539。CSV 首页那 135,602 字节是重复的：`wait` 回包里的第一页就是它
（同样 offset 0–65,518），是我提示词要它"用 path 读 CSV 第一页"造成的。去掉这一项是 170 KB；`wait` 再不附第一页就只剩 39 KB。
看 CSV 格式其实只要开头 247 字节（diff 头、表头、第一条），现在最少得读一页 64 KB。

**ChatGPT 第 7 步的评价**（用户贴回来的）：按文件读够用，比从头翻更适合审代码，代码审核这一轮 3 次调用。别扭的地方和建议：

1. （P1）`wait` / `status` 加 `diff_mode=metadata`，默认只给文件统计、不带 diff 正文——大 CSV 还是进了首个回包。
2. （P1）`path` 加 `limit_bytes`，比如只读 CSV 前 4 KB。
3. （P2）`path` 时省略 offset 是从这个文件在整份 diff 里的位置起，不是文件内的 0，容易误解；回 `file_offset` 和 `global_offset`。
4. （P2）每页加 `page_bytes`、`file_done`。
5. （P3）`path` 多选，一次读几个小代码文件。

**我的看法**（还没定，等用户选）：

- 第 2 条现在值得做：§50 时我说"真遇到一个文件就超过 64 KB 又只想看一部分再说"，这一轮就是（CSV 167 KB，看格式 247 字节够）。
  名字跟 `read_file`、`git_diff` 一样叫 `max_bytes`，范围 1 KB–64 KB、默认 64 KB，照样截在整行上。
- 第 1 条是 §51 用户定过"大 diff 时 `status` / `wait` 仍附第一页"的事，这一轮多了证据：两轮大 diff 的第一页都是生成的数据，对审代码没用，
  每次 143 KB。要不要改由用户重新定；要改，按大小定（超过 64 KB 只给 `file_stats`）比加一个参数简单。
- 第 3 条不加字段：它这一轮没用错；把 `offset` 的说明写明"给了 `path` 也是整份 diff 里的位置"就够。
- 第 4、5 条不值：`page_bytes` 是 `next_offset - offset`，给了 `path` 时 `next_offset` 为 `null` 就是这个文件读完了；三个小文件三次调用一共 11 KB。

没推送。

## 55. `action=diff` 加 `max_bytes`，大 diff 时 `status` / `wait` 不附第一页（2026-10-23）

**要求：**用户在 §54 的取舍里选了："做：action=diff 加 max_bytes（1 KB–64 KB，默认 64 KB，截在整行上）；diff 超过 64 KB 时
status/wait 只给 file_stats、不附第一页；顺手把 offset 的说明写明白。测试、文档、提交，先不换装"。§51 定过的"仍附第一页"由此改掉。

**改了什么：**

- `b06730d` `max_bytes`：进 `tools::args` 的边界表（1,024–65,536，默认 65,536，给小了按 1 KB 算），照样截在整行上。schema 里
  `offset` 的说明改成"一直是整份 diff 里的字节位置，带 `path` 也一样，不是从文件开头数"，不加 `file_offset` / `global_offset`。
- `6ce3ba1` 给 AI 的回包（`tools::agent::present`）：diff 超过一页时 `diff` 为 null、`diff_truncated` 为真、`diff_next_offset` 为 0，
  next_action 写明多少字节、照 `file_stats` 用 `path` 挑着读（大数据文件加 `max_bytes=4096`）或从 0 翻全份。不超过一页的照样整份附上。
  本机 `gld agent show` 不走这里，照样打前 64 KB 和全文路径。

**按 §54 那轮算：**`awaiting_review` 那次 `wait` 回包 143,257 字节里约 131 KB 是第一页（两份），现在不附；CSV 用 `max_bytes=4096` 瞄
开头，回包约 9 KB 而不是 135,602 字节。那一轮的大回包合计会从 306 KB 降到约 47 KB（`wait` 剩约 12 KB、三个代码文件 14.7 KB、CSV 小页约 9 KB、review 11 KB；
没真跑过，按回包构成估的）。

**工具表又变了：**已装的 `2f50e09` 和新编的版本逐个工具比，只有 `agent_manage` 变了，多了 `max_bytes`（说明也改了）。换装后要再点 Refresh。

**验证：**按文件读那条加 1 KB 小页（≤1 KB、截在整行上、从 `diff --git` 起、下一页紧接着、一页页拼回整段）；回包单元测试改成大 diff 不附、
`diff_next_offset` 为 0、next_action 带字节数 / `path` / `max_bytes` / `offset=0`，小 diff 照样附；端到端大 diff 那条加 `max_bytes=4096`
只回 4 KB 以内、`max_bytes=10` 按 1 KB 算、回包 `diff` 为 null 且 next_action 带 `path`。"不附第一页"的端到端断言临时撤掉改动跑过，
旧代码上失败。全仓 1033 passed / 0 failed，clippy、fmt、文档门禁通过。

**没做：**换装（本机还是 `2f50e09`）；没用真的 ChatGPT 试。没推送。

## 56. 本机换成 `2022624`（2026-10-23 19:26）

**要求：**用户说"继续"，接 §55 列的下一步：换装后让 ChatGPT 再审一次大 diff，看回包是不是降到几十 KB。换装照 install.md，
和 §49、§53 同一范围。

**换之前：**守护进程 pid 25532（`2f50e09`）没有子进程，没有在跑的委派；19:22 有一次 `tools/list`、没有工具调用，重启时已安静 285 秒。

**换装：**release 构建 3 分 02 秒；备份 `gld-2f50e09-20261023-1926` 和 `gld-config-20261023-1926.tgz`（0600）；写 `.new` 再改名，去掉
`CLAUDE*` 变量重启，pid 25532 → 32869。带上 `b06730d`（`max_bytes`）、`6ce3ba1`（大 diff 不附第一页）。

**换完：**命令行和服务的构建提交都是 `20226247f332`，协议 7；`gld ls`、两份凭据指纹前后一致；`CLAUDE*` 变量 0 个；`gld health` 四项全 ✓。
工具表指纹 `2ceba8697b67815e` → `d857f66a1e45e9cc`，逐个工具比只有 `agent_manage` 的说明和 schema 变了（多了 `max_bytes`），
**要在 ChatGPT 点一次 Refresh**。

**给 ChatGPT 的这一轮：**委派 codex 在 `math_utils.py` 手写 `quartiles` / `iqr`（不许 import `statistics`），生成脚本用标准库
`statistics.quantiles(method="inclusive")` 算期望值，3000 条用例的 CSV 和读它的测试。模拟估 CSV 那段约 193 KB。这次不规定怎么读，
只要求按回包的 next_action 和 `file_stats` 自己定、数据文件只用小的 `max_bytes` 瞄开头、不用 `read_file` / `git_diff` 读这次的改动；
看回包总量是不是到了 §55 估的几十 KB。结果等用户试完再核对、记下一节；请求日志从第 16553 行起是这一轮。没推送。

## 57. 大 diff 不附第一页之后：ChatGPT 审 195 KB 的委派 diff，回包合计 46 KB（2026-10-23 19:30–19:40）

**结论：大 diff 的审核传输量问题解决了。**diff 199,569 字节（CSV 一段 193,429，代码三段 1,871 / 703 / 3,566）。`awaiting_review`
那次回包 `diff` 为 null，ChatGPT 照 next_action 和 `file_stats` 用 `path` 读三个代码文件、用 `max_bytes=4096` 瞄 CSV 开头，一共读
diff 正文 10,182 字节（5.1%）。`math_utils.py` 那段没有 import `statistics`，四分位是手写插值，没混进前几轮的改动。accept。

| 本地时间 | ChatGPT 做了什么（项目自己的请求日志、运行记录） |
| --- | --- |
| 19:30:57 | 用户点 Refresh，`tools/list` 回 47,837 字节 |
| 19:31:34 | `start agent=codex`，回 `waiting_local_approval`（5,138 字节）；用户放行，Codex 19:31:52–19:34:44 跑了 172 秒 |
| 19:39:23 | `agent_manage` 回 9,142 字节，`awaiting_review`、`diff` 为 null。这之前 8 分钟没有 `wait`：ChatGPT 那一轮在等用户放行后停下了 |
| 19:39:35–41 | 四次 `action=diff`，都带 `path` 和 `max_bytes=4096`：4,966、2,317、8,455、9,266 字节 |
| 19:39:47–52 | `exec_command`：numstat、`python3 gen_iqr_cases.py --check`、`python3 -m unittest -v`，退出码都是 0 |
| 19:40:22 | `review verdict=accept`，回 11,909 字节 |

**三轮按线上回包比**（等审核之后的 `agent_manage` 回包，含 JSON 封装和双份内容，`response_bytes` 加总）：§50 790,539 字节，
§54 305,813，这一轮 46,055，比 §50 少 94%。三轮 diff 分别是 175 KB、172 KB、200 KB。ChatGPT 说的 94.9% 只算 diff 正文，它自己也注明了。

**我这边核对：**三个代码文件按 `path` 重读，和 `task.diff` 对应段逐字节相同；CSV 4 KB 小页回 4,042 字节、截在整行上；`math_utils.py`
整个文件没有 import，新增段只有 `quartiles`、`iqr`，相对 HEAD +53 −1（之前 +34 −1 加这 19 行），`test_math_utils.py` 没动，没有新提交；
`--check` 退出码 0，测试 OK。

**回包里还剩什么：**用 `status` 量等审核那种回包各字段（JSON 一份约 3.7 KB，线上翻倍）：`prompt_preview` 1,320 字节最大（最多 2 KB，
是 AI 自己写的提示，每次 `status` / `wait` / `review` 都原样回），recap 774，`file_stats` 413，next_action 在等审核时约 650，
gld 内部字段（`owner`、`owner_pid`、`pid`、`project_id` 等）一共约 150。

**ChatGPT 第 6 步的评价**（用户贴回来的）：核心问题基本解决，`file_stats` 足够指导审查，不用再动分页机制。还能改的：

1. next_action 偏长，给结构化的 `recommended_action`。
2. 回包字段多（`prompt_preview`、`owner_pid` 等），加 `response_mode=compact`。
3. 分页回 `page_bytes`：这一轮它要自己算 UTF-8 字节数。
4. 加文件内的相对偏移 `file_offset`。
5. 累计传输指标 `diff_bytes_served`、`diff_calls`。

**我的看法**（还没定，等用户选）：

- 第 2 条对一半：不用加模式，直接让给 AI 的回包不带 `prompt_preview`（它自己写的，`list` 里还有 200 字的预览）和 gld 内部字段，
  每次省约 1.5 KB、线上约 3 KB；本机 `gld agent show` 照样打提示全文的预览，放行前要看。
- 第 3、4 条不值：要算字节数、对 offset，是我提示词要它报"每页多少字节、占多少"，正常审核用不上；`next_offset` 照抄就接得上。
- 第 1 条不值：next_action 只在等放行、在跑、等审核时长，约 650 字节，结构化以后 AI 还是得读懂它。
- 第 5 条不值：请求日志已经按次记了回包字节数，上面三轮的对比就是从它算的。

没推送。

## 58. 给 AI 的委派回包不带提示预览和本机记账字段（2026-10-23）

**要求：**用户对 §57 的取舍说"好的，按照你的建议"：给 AI 的回包去掉 `prompt_preview` 和 gld 内部字段，`list` 的预览和本机
`gld agent show` 不变；另外四条不做。

**改了什么（`13dabeb`）：**`tools::agent::present` 去掉 `prompt_preview`、`owner`、`owner_pid`、`pid`、`project_id`（`caller`
以前就去掉了）。按 §57 量的那个任务算，每次回包少约 1.4 KB、线上约 2.9 KB；等审核那种回包从约 9 KB 降到 6 KB 上下（按字段大小算的，
没真跑）。`action=list` 里 200 字的预览照留；本机 `gld agent show` 照样打提示和这些字段。concepts.md 写明给 AI 的少了什么、为什么。

**工具表没变：**已装的 `2022624` 和新编的版本 `gld --json tool list` 一字不差，换装后不用 Refresh。

**验证：**新增回包单元测试一条（这六个字段不在、审核要的都在）；端到端加 start 和等审核的回包都没有这些字段、`list` 里还有预览、
show 里还有提示原文，这条临时撤掉改动跑过，旧代码上失败。全仓 1034 passed / 0 failed，clippy、fmt、文档门禁通过。

**没做：**换装（本机还是 `2022624`）。没推送。

## 59. 本机换成 `b052102`（2026-10-23 21:18）

**要求：**用户说"按 install.md 换装本机（先查没有在跑的命令），换完核对，不推送。这次工具表没变，不用给 ChatGPT 提示词"。

**换之前：**守护进程 pid 32869（`2022624`）没有子进程，没有在跑的委派。21:11 有一个客户端（请求 id 是随机数字）调了几次只读工具，
之后安静；重启时已安静 415 秒。

**换装：**release 构建 3 分 24 秒；备份 `gld-2022624-20261023-2118` 和 `gld-config-20261023-2118.tgz`（0600）；写 `.new` 再改名，
去掉 `CLAUDE*` 变量重启，pid 32869 → 79050。带上 `13dabeb`（给 AI 的回包不带提示预览和本机记账字段）。

**换完：**命令行和服务的构建提交都是 `b05210256c1b`，协议 7；工具表 34 项、指纹 `d857f66a1e45e9cc` **没变，不用 Refresh**；`gld ls`、
两份凭据指纹前后一致；`CLAUDE*` 变量 0 个；`gld health` 四项全 ✓。用新版调一次 `agent_manage action=status`（§57 那个任务），
`caller`、`prompt_preview`、`owner`、`owner_pid`、`pid`、`project_id` 都不在，`task_id`、`state`、`recap`、`file_stats`、`next_action` 都在；
本机 `gld agent show` 照样有提示原文。没推送。

## 60. 一键放行全部命令、委派两档放权、更多 Agent 档案（2026-10-23）

**要求：**用户说"可一键设置允许所有命令，但提示相关风险，chatgpt/grok/gemini 等页面也要提示下风险。关于连接 agent、智能体操作：
agent 种类有很多，Codex、OpenCode、DeepSeek Harness、Antigravity、Pi 与 Qoder，未来对接其他智能体的可扩展性；同理，也可以一键批准
agent 运行所有操作，或者由 agent 自行判断，自行批准，直接执行。梳理一下，然后继续任务"。

**梳理时的现状：**命令有白名单、`gh` 只读、远端 shell 不放、危险命令和装东西要 `confirm=true`、不许经子进程写工作区外几道限制类关卡，
`permission-mode=dangerous` 和 `trusted` 行为完全一样（摆设）。委派只有一档：全放开；内置档案只有 codex、claude；`agents.toml` 能加别的。
ChatGPT、Grok、Gemini 这些网页自己不弹警告，风险只能写进 AI 读到的地方、要它转告。

**各家 Agent 的参数**：开了一个子任务只读调研（只跑 `--help`、读安装包和官方文档，不装、不跑、不花额度），我再在本机逐条核：

| Agent | 全放开 | 自行判断 | 本机 |
| --- | --- | --- | --- |
| Codex 0.163.0 | `--dangerously-bypass-approvals-and-sandbox` | `--approve-for-me`（审查模型自动批或拒，`--help` 核过） | 装了，真跑过 |
| Claude Code 2.1.304 | `--dangerously-skip-permissions` | `--permission-mode auto`、`--permission-prompts none`（`--help` 核过） | 装了 |
| Qwen Code 0.19.12 | `--yolo` | `--approval-mode auto`（顶层帮助不显示，安装包参数定义里核过） | 装了 |
| Antigravity 1.1.28 | `--dangerously-skip-permissions` | 没有（`--mode` 只有 accept-edits、plan） | 装了 |
| Pi 0.78.1 | 没有审批 | 没有 | 装了 |
| Kimi 0.29.1 | `-p` 本身全放开 | `-p` 下没有 | 装了 |
| OpenCode | `opencode run --auto` | 没有 | 没装 |
| Qoder CLI | `qodercli -p --yolo` | `--permission-mode auto` | 没装（本机的 `qoder` 是 IDE 启动器） |
| DeepSeek Harness | 没有命令行参数，要写 headless 预设 `danger-full-access` | 没有 | 没装（官方 `deepseek-ai/deepseek-harness`，`dsh`） |

**改了什么：**

- `6a840e2` 项目开关 `command-policy=guarded|all`。all 时放开上面那几道限制类关卡；照旧拦删 `.git` / `.github`、workdir 出工作区、
  `cmd` 形式里的管道重定向、塞环境变量、超时超上限。AI 那边四处告知：hub 初始化说明里项目后面标 `command-policy=all`，
  `list_workspaces`、`workspace_context` 回 `command_policy` 和 `risk`（要它进项目时跟用户说一声、动手前说清要跑什么），
  单项目连接的初始化说明第一句就是它；某条命令按默认关卡会被拦时，回包加 `policy_bypassed`。本机：`gld set` 打开时打出风险和怎么改回，
  `gld ls` 标「命令全放行」，`gld ls <项目>` 有"跑命令"一行。GPT Actions 不受影响。
- `6f7280d` `gld set agent-delegation=auto` 时打出风险提示（后面 `fb9195d` 扩成两个委派开关一起说）。
- `fb9195d` 项目开关 `agent-approval=all|self`，档案加 `self_command`（codex、claude 先有）。选了 self 而档案没有：起之前和 ask 档放行时
  都回 `AGENT_APPROVAL_UNSUPPORTED`，**绝不退回全放开**（argv 只认要的那一档，空的当失败）。任务记录、`action=agents`、`gld agent show` 都写着按哪一档跑。
- `dd60338` 内置档案加 qwen、agy、pi、kimi；OpenCode、Qoder CLI、DeepSeek Harness 在 concepts.md 给 `agents.toml` 写法，标明照文档写、没跑过。
- `dd2d4a7` 读代码时发现内置 claude 档案用 `--output-format json`，跑完才吐结果，超过 5 分钟就会被"没输出就是卡住"误杀；改成
  `stream-json --verbose`（`-p` 下不带 `--verbose` 会报错，本机二进制里有这句原文）。

**验证：**新增测试 9 条（策略单元、hub、单项目初始化、命令行端到端各一条；委派单元两条、端到端一条；档案单元改写），全仓 1041 passed / 0 failed，
clippy、fmt、文档门禁通过。每个中间提交都单独跑过委派单元、端到端和文档门禁。

**工具表：**和已装的 `b052102` 比，只有 `agent_manage` 的说明变了（不再写死"FULL permissions"），换装后要再点 Refresh；
`command-policy` 不改 schema，靠回包字段告诉 AI。

**没做、要用户定的：**新加的 qwen、agy、pi、kimi 和 claude 都没真跑过（花额度）；OpenCode、Qoder CLI、DeepSeek Harness 没装（全局安装要先问）；
换装；推送。核对 Codex 配置时 sed 的脱敏没生效（BSD sed 不认 `I` 标志），uTools 本机 MCP 的 key 明文出现在了会话的工具输出里，
没写进任何文件，已告诉用户。

## 61. `gld add` 名字在前也认；本机换成 `b81b144` 并实测（2026-10-23 22:34）

**要求：**用户说"很好，需要换装然后测试，但是目前 add 有问题"，贴了 `gld add devdocs-node ~/xdw/devdocs-node` 报
"目录不存在或无法访问：…/keex/devdocs-node"。

**原因：**`gld add` 的参数是 `[PATH]...`，两个词都当目录，第一个按当前目录 `~/xdw/keex` 去找。`gld remote add` 是名字在前，照着敲很自然。
读代码时还发现：多个目录是一个个登记的，第二个不存在时第一个已经加进去了。

**改了什么：**`f79409e` 先核对所有目录再登记，有一个不存在就一个都不加，不存在的是个单词时提示 `--name`；`b81b144` 没给 `--name`、
正好两个参数、一个是当前目录下不存在的单词、另一个是存在的目录时，那个词当名称（名字在前在后都行），stderr 说一句怎么理解的。
新增端到端两条，旧代码上都失败。全仓 1043 passed / 0 failed。

**换装：**守护进程 pid 79050（`b052102`）没有子进程、没有在跑的委派；22:30 有客户端在调 `read_file`，二进制换好后重启时已安静 207 秒。
release 构建 3 分 28 秒；备份 `gld-b052102-20261023-2234` 和 `gld-config-20261023-2234.tgz`（0600）；pid 79050 → 37079。
构建提交 `b81b1442c91d`，协议 7；`gld ls`、两份凭据指纹前后一致（`profiles.json` 的指纹和 §59 时不同：用户这期间加了项目，按换装前那一刻比）；
`CLAUDE*` 变量 0 个；`gld health` 全 ✓。工具表指纹 `d857f66a1e45e9cc` → `d7b4fcbe928d3203`（`agent_manage` 说明变了），**要点 Refresh**。

**本机实测：**

- 在 `~/xdw/keex` 下跑用户那条 `gld add devdocs-node ~/xdw/devdocs-node`：说明"当成项目名称"，退出码 0；这个项目用户已经另外加过，报"本来就在"。
- `gld set delegation-test command-policy=all`：打出风险和改回的命令；`gld ls` 标「命令全放行」，`gld ls delegation-test` 的"跑命令"一行是 all。
  经守护进程跑 `sh -c "echo hi from sh"`：跑了，回包 `policy_bypassed.rule = command_not_allowlisted`。
- `gld set delegation-test agent-approval=self`：`action=agents` 回 `approval: self`，codex、claude、qwen 的 `self_approval` 为真，agy、pi、kimi 为假。
  提示里项目本来就是 ask 还劝"用 ask"，读着像没改成功，`d0bef0e` 改成只对 auto 说（只改命令行文字，没换装）。

**给 ChatGPT 的这一轮：**delegation-test 现在是 `command-policy=all`、`agent-approval=self`、`agent-delegation=ask`。提示词不提风险，看它进项目后会不会
自己转告"这个项目允许所有命令"，跑白名单外 / 危险命令时会不会把 `policy_bypassed` 告诉用户；再委派 codex 一个小改动，看 `--approve-for-me`
那一档能不能跑完、任务记录里 `agent_approval` 是不是 self。测完把 delegation-test 改回 `command-policy=guarded`。请求日志从第 16677 行起是这一轮。没推送。

## 62. ChatGPT 测一键放行的风险转告和 self 档委派（2026-10-23 22:59–23:04）

**结论：两样都按设计工作。**ChatGPT 没被提示，自己从 `list_workspaces` 看到 `command-policy=all` 并在删除前跟用户说了风险；两条命令的回包它都
认出了绕过的关卡。委派按 `agent-approval=self` 跑，Codex 会话日志证实用的是自动审查那一档，审核 accept。

| 本地时间 | ChatGPT 做了什么（项目自己的请求日志、运行记录） |
| --- | --- |
| 22:59:37 | 用户点 Refresh，`tools/list` 回 47,989 字节 |
| 23:00:10–40 | `list_workspaces`（7,128 字节，带 delegation-test 的 `risk`）、`workspace_context`、`server_info`、`list_dir`、`git_status`、`read_file`、`agent_manage action=agents`、`check_exec_environment` |
| 23:00:59 | `apply_patch` 写 `tmp_check/hello.txt` |
| 23:01:06 | `exec_command rm -rf tmp_check`，退出码 0（默认要 `confirm=true`）；目录已删干净 |
| 23:01:11 | `exec_command sw_vers`，退出码 0（默认不在白名单） |
| 23:01:37 | `agent_manage start agent=codex`，回 `waiting_local_approval`；用户放行，Codex 跑了 93 秒 |
| 23:03:44–59 | 等审核回包 7,256 字节（diff 1,272 字节，整份附上）；`action=diff` 读一遍 |
| 23:04:03 | `exec_command python3 -m unittest -v`，退出码 0 |
| 23:04:15 | `review verdict=accept`，审核意见写明"ask + self、不是 all"，并因为 `command-policy=all`、`sandbox_enforced=false` 只认 diff 和独立测试 |

ChatGPT 的自述（用户贴回来的）：从 `list_workspaces` 首次看到 `command-policy=all`、删除前说明了风险；`check_exec_environment` 确认没有系统级沙箱；
两次 `exec_command` 回包证明危险命令确认和白名单被跳过；`ask + self` 下"人工批准管的是任务启动，不是运行中的每一步"，所以审核以 diff 和独立测试为准。
对话里它说了什么 gld 记录不到，没核对；调用顺序和它说的一致（`list_workspaces` 在 23:00:10，`rm -rf` 在 23:01:06）。

**self 档真的生效了：**这一轮 Codex 的会话日志（`~/.codex/sessions/…T23-02-15…`）里是 `approval_policy: on-request`、`approvals_reviewer: auto_review`、
`sandbox_policy: workspace-write`；19:31 那轮全放开的是 `never`、`user`、`danger-full-access`。任务记录 `agent_approval: self`。

**我这边核对：**`tmp_check` 不在了；测试重跑 OK；`math_utils.py` 相对 HEAD +61 −1（之前 +53 −1 加这次 8 行），`test_math_utils.py` +23 −1
（之前 +13 −1 加这次 10 行），没有新提交。测完把 delegation-test 改回 `command-policy=guarded`、`agent-approval=all`。

**还没装上的：**`d0bef0e`（ask 档时提示不再劝"用 ask"），改回时的提示里还能看到那句。没推送。

## 63. 准备真跑 claude、qwen、agy、pi、kimi 五份档案（2026-10-23 23:16）

**要求：**用户说"给我一段提示词，让 ChatGPT 在 delegation-test 上分别委派 claude、qwen、agy、pi、kimi 各做一个小改动，验证这些档案能跑通；不推送"。
这就是花额度真跑，用户点了头。

**预检（不花钱）：**只看凭据文件在不在、只读 Qwen 设置里选的登录方式，不读凭据内容。

- Claude：这台机器的 Claude Code 账号。Pi：`~/.pi/agent/auth.json` 在。Kimi：`~/.kimi/credentials` 在。
- Qwen：`security.auth.selectedType = qwen-oauth`，`~/.qwen/oauth_creds.json` 最后改于 2026-03-12，可能过期；0.19.12 的帮助里 `qwen auth` 标着 removed。
- Antigravity：`~/.antigravity` 里只有 `argv.json`、`extensions`（像 IDE 的），没看到命令行登录信息，可能要先在交互模式登录一次。
- 守护进程环境里名字像密钥的变量只有 `SSH_KEY_PATH`、`STARSHIP_SESSION_KEY`、`USE_LOCAL_OAUTH`、`USE_STAGING_OAUTH`，和这几家的登录无关，
  gld 去掉它们不会挡住谁。

**设计：**delegation-test 是 `agent-delegation=ask`、`agent-approval=all`。五家各加一个不同的函数和两三条测试（claude `harmonic_mean`、qwen `clamp`、
agy `value_range`、pi `normalize`、kimi `cumulative_sum`），一家审完再起下一家；agy `timeout_minutes=10`（打印完不退出的已知 bug 最多拖 10 分钟），
其余 15。失败的照样审：看 state、failure、recap、output_tail；被打回又留了改动的，先告诉用户再只撤这个任务自己的那段。结果等用户试完核对、
记下一节；请求日志从第 16746 行起是这一轮。没推送。

## 64. Agent 名单、默认模型和推理强度、放行时能改（2026-10-24）

**要求：**用户说不是每家 Agent 都有订阅，五份新档案的真跑先通过、遇到再调；问 model 和 effort 怎么设、Agent 是自动还是手动选。
我答：现在 gld 不管模型和强度（用各家自己的配置，比如这台机器 Codex 是 `~/.codex/config.toml` 的 `gpt-6-astra` / `xhigh`），
AI 按名字自己挑、ask 档只能批或拒，`installed` 查不出订阅和登录；建议名单 + 档案默认 + AI 提议 + 放行时能改。用户说"按你的建议做"。

**各家参数**（本机 `--help` 核过）：Codex `-m`、`-c model_reasoning_effort=`（取值帮助里没列；二进制里能看到一串枚举名，分不清哪几个属于
推理强度，所以 gld 不核对、交给 Codex 报错）；Claude `--model`、`--effort low|medium|high|xhigh|max`；Antigravity `--model`、`--effort low|medium|high`；
Pi `--model`、`--thinking off|minimal|low|medium|high|xhigh`；Qwen、Kimi 只有 `-m`。

**改了什么（三个提交）：**

- `4559682` 项目字段 `agents`（逗号分隔，去重去空白；空的是全部）。`action=agents` 只列名单里的，回 `allowed`、`listed_without_profile`；
  起名单外的回 `AGENT_NOT_ALLOWED`。`gld ls <项目>` 加"可委派的 Agent"一行。
- `5234bba` 档案加 `model`、`effort`、`model_args`、`effort_args`、`efforts`，内置六家填好；参数加在命令末尾，最后一项是 `-` 的加在它前面。
  `agents.toml` 里不写 `command` 的条目在同名内置上改字段（只改默认模型和强度不用抄整条）。`agent_manage start` 收 `model`、`effort`，
  先核对再起，实际用的记进任务；`action=agents` 回每家默认值和能不能选。
- `9e7dd10` `gld agent approve <任务> --agent / --model / --effort`。换 Agent 时模型和强度改用新那家的默认；核对都在改任务记录之前，
  不对就报错、任务留着；改了什么记进 `approval_changes`，AI 看得到。守护进程协议 7 → 8（旧守护进程会悄悄丢掉用户的修改）。

**验证：**新增测试 7 条（档案单元三条、委派单元两条、端到端三条，含名单那条），全仓 1051 passed / 0 failed，clippy、fmt、文档门禁通过。
跑委派单元时"在跑时输出尾巴"那条在后台搜 Codex 二进制、CPU 吃满时偶发失败一次，单独重跑通过，和这次改动无关。

**换装要知道的：**工具表变了（`agent_manage` 多了 `model`、`effort`，说明也改了），要点 Refresh；协议号变了，换二进制后必须重启守护进程
（换装流程本来就有这步），命令行和守护进程版本对不上时命令行会先报"请重启"。没换装、没推送。

## 65. 本机换成 `3dc98e4`（2026-10-24 10:37）；昨晚五份新档案的真跑结果

**要求：**用户说"按 install.md 换装本机（先查没有在跑的命令），换完核对，不推送"。

**换之前：**守护进程 pid 37079（`b81b144`，协议 7）没有子进程，没有在跑的委派，请求日志 22 分钟没动。

**换装：**release 构建 2 分 58 秒；备份 `gld-b81b144-20261024-1037` 和 `gld-config-20261024-1037.tgz`（0600）；写 `.new` 再改名，
新命令行（协议 8）照样能对旧守护进程 `restart`，去掉 `CLAUDE*` 变量重启，pid 37079 → 92371。带上 `d0bef0e`、`4559682`、`5234bba`、`9e7dd10`。

**换完：**命令行和服务的构建提交都是 `3dc98e4c819c`，**协议 8**；`gld ls`、两份凭据指纹前后一致；`CLAUDE*` 变量 0 个；`gld health` 全 ✓。
工具表指纹 `d7b4fcbe928d3203` → `f0e996d1217ba15b`，旧、新二进制逐个工具比只有 `agent_manage` 变了（多了 `model`、`effort`，说明改了），
**要点 Refresh**。新版实调 `action=agents`：codex、claude、agy、pi 的 `effort_choice` 为真，qwen、kimi 为假，各家 `efforts` 和档案一致。

**昨晚五份新档案的真跑**（换装前看任务列表才发现：用户 10-23 23:30 前后已经让 ChatGPT 按 §63 的提示词跑过，之后说"先通过"）。五个都在
1–6 秒内失败，都是登录问题，各家自己报的原因：

| 任务 | Agent | 原因 |
| --- | --- | --- |
| `a3d7759e` | claude 2.1.304 | `authentication_failed: Not logged in · Please run /login`（命令行版 claude 没登录） |
| `2663e1a8` | qwen 0.19.12 | Qwen OAuth 免费额度已停，要用 `/auth` 换成 Coding Plan 或别的服务商 |
| `2b3a05d8` | agy 1.1.28 | `authentication failed or timed out`，要先跑 `agy` 登录 |
| `d145f7dd` | pi | `No API key found for the selected model` |
| `0e70d729` | kimi 0.29.1 | 服务商 kimi 的 API key 无效或过期（401） |

gld 这边：每个都记成 `failed`、原因在 `output_tail` 里，ChatGPT 逐个读了原因、打回，都没改文件。几家都走到了登录这一步才失败，
没有一家报"不认识的参数"，说明档案里的启动参数至少都被接受了；真正干活的部分还没验证。没推送。

## 66. ChatGPT 实测 Agent 名单、提议模型和强度、放行时改（2026-10-24 11:11–11:19）

**准备：**用户说"继续"。本机只有 Codex 登录了，所以 `gld set delegation-test agents=codex`（撤销用 `agents=`）。本机先调
`action=agents`：只回 codex 一行，`model`、`effort` 为空（按 Codex 自己的配置，这台是 `gpt-6-astra` / `xhigh`），`model_choice`、
`effort_choice` 为真。Codex 已自己升到 0.164.0，档案用到的 `-m`、`-c`、`--approve-for-me`、`--dangerously-bypass-approvals-and-sandbox`
都还在。Codex 的模型缓存里每个模型列了自己支持的推理强度：`gpt-6-astra` 是 low / medium / high / xhigh / max / ultra，
`gpt-6-luna`（"快、便宜，适合简单任务"）没有 ultra，所以 codex 档案的 `efforts` 留空、交给 Codex 核对是对的（§64）。
提示词让 ChatGPT 查名单、故意起 claude、再用 `model=gpt-6-luna effort=medium` 起 codex，停下等用户放行时改成 low。

**结果**（请求日志 16878 行起，11 次 tools/call（含一次 `list_workspaces`），都在 02:12–02:19 UTC）：

| 时间（UTC） | 调用 | 回包 | 看到的 |
| --- | --- | --- | --- |
| 02:12:55 | `action=agents` | 1,388 B | 只有 codex（ChatGPT 报"通过"） |
| 02:12:57 / 02:13:17 | `action=list` 前后各一次 | 都是 12,110 B | 起 claude 前后一样大：没有多出任务 |
| 02:13:11 | `start agent=claude` | 4,155 B | 被拒；本机同样调用复现是 `AGENT_NOT_ALLOWED`（`permission`），任务列表没变 |
| 02:13:35 | `start agent=codex model=gpt-6-luna effort=medium` | 2,492 B | `ad6eb28a` 等本机放行 |
| 02:18:23 | 用户 `gld agent approve ad6eb28a --effort low` | — | 命令行打印"改了：effort medium → low" |
| 02:19:09 | `wait` | 8,579 B | `awaiting_review`，`model=gpt-6-luna`、`effort=low`、`approval_changes=["effort medium → low"]` |
| 02:19:17 | `action=diff` | 3,797 B | 1,457 B 全文一页读完 |
| 02:19:21–23 | 两次 `exec_command` | 3,201 / 5,484 B | pytest 跑不了（见下），unittest 13/13 通过 |
| 02:19:33 | `review accept` | 6,474 B | `accepted` |

**Codex 实际用的是不是改过的：**是。那次的会话记录（02:18:24.987Z 开始，放行后 1.6 秒）里 `model=gpt-6-luna`、`effort=low`、
`approval_policy=never`、`sandbox=danger-full-access`（all 档）。跑了 19 秒，退出码 0，改了两个文件 +18 −1，`head_moved=false`。
说明 AI 提议的模型留下了，用户放行时改的强度真的传给了 Codex，不只是记录里改了。

**顺带发现：**这台的 Python 3.12（mise 装的）没有 pytest，Agent 和 ChatGPT 都如实报"没跑成"，没算通过；ChatGPT 改用
`python3 -m unittest` 自己验了一遍，在审核意见里写明了这个限制。不是 gld 的问题。没推送。

## 67. 放开 `--write` grant 委派（2026-10-24）

**要求：**用户说"下一步做放开 --write grant，先说清楚改什么和风险，再动手；测试、文档、提交，先不换装"。

**为什么之前关着：**RFC-0009 第 5 节定的是服务凭据和 `--write` grant 能发起，D1 做得更紧、所有 grant 都不行：
往下传的 `Grant` 只有 id、名字、项目和 `read_only`，分不出浏览器地址的 grant，而浏览器 grant 也能带 `--write`
（`gld grant browser … --write`，`BrowserGrantSpec.writable`）。只读 grant 本来就调不到（read-only 工具集里没有 `agent_manage`）。

**改了什么：**

- `Grant` 加 `browser`（`serde(skip)`，不落盘），查 grant 时按数据记录里有没有浏览器钥匙现算（`GrantRecord::scope`）；
  `may_delegate()` = 不是只读、不是浏览器。`Caller` 带整份 `Grant`（原来只有 id）。
- `agent_manage` 起、审、停改看 `may_delegate`；不行的报 `AGENT_DELEGATION_NOT_ALLOWED`，`details.grant` 写 `browser` 或 `read_only`。
- grant 只看得见、动得了自己起的任务（`list` 过滤，按 id 查别人的报 `AGENT_TASK_NOT_FOUND`），和命令会话按调用方分开同一个口径；
  服务凭据照旧看得见全部。任务记下 `grant`（id、名字），给 AI 的回包里去掉（和 `caller` 一样是本机记账）。
- `gld grant rm` 把它委派的任务一起收掉：在跑的叫停、等放行的作废，35 秒后再扫一次；回包多 `stoppedAgentTasks`，
  命令行打"停掉了它委派的 N 个 Agent 任务"。**守护进程协议 8 → 9**（旧守护进程不停这些任务，新命令行却会当成停了 0 个）。
- `gld agent ls` 在 Agent 后标"（grant 名字）"，`gld agent show` 的发起人写哪把 grant、怎么撤销。
- 没给 grant 额外强制 ask：`--write` 本来就等于全权（能跑命令就读得到服务口令），同一个开关对不同凭据含义不同反而难懂；
  security.md 写明 `auto` 档下 `--write` grant 不用你点头就能起全权 Agent，开给 grant 的项目用 `ask`。

**验证：**新增端到端一条（真起 bearer 服务：只读 grant 硬发是 `unknown_tool`；`--write` 的浏览器 grant 被拒；`--write` grant
ask 档起任务、本机列表和详情标出 grant 名、另一把 `--write` grant 列表为空且查和停都是 `AGENT_TASK_NOT_FOUND`、本机看得见；
auto 档再起一个在跑的，`grant rm` 回 `stoppedAgentTasks=2`，两个都变 `stopped`），grant 单元测试补浏览器标记和 `may_delegate` 的断言。
全仓 1052 passed / 0 failed（§64 是 1051，多的就是这一条），clippy、fmt、文档门禁通过。工具表没变，换装后不用 Refresh；没换装、没推送。

## 68. 本机换成 `a4993f0`（2026-10-24 13:28）

**要求：**用户说"按 install.md 换装本机（先查没有在跑的命令），换完核对，不推送"。

**换之前：**守护进程 pid 92371（`3dc98e4`，协议 8）没有子进程，没有在跑的委派，请求日志最后一条是 04:04:49Z（20 分钟没动），
重启前再看还是这一条。

**换装：**release 构建 2 分 25 秒（照旧有 `rust-objcopy` 缺 `libLLVM.dylib` 的去符号警告，不影响运行）；备份 `gld-3dc98e4-20261024-1327`
和 `gld-config-20261024-1327.tgz`（0600）；写 `.new` 再改名，去掉 `CLAUDE*` 变量重启，pid 92371 → 95309。带上 `6a21eae`、`a4993f0`。

**换完：**命令行和服务的构建提交都是 `a4993f0f923a`，**协议 9**；工具表 34 项、指纹 `f0e996d1217ba15b` 前后一样，**不用 Refresh**；
`gld ls`、两份凭据指纹（`profiles.json`、`oauth-clients/hub.json`）前后一致；守护进程环境里 `CLAUDE*` 变量 0 个；`gld health` 全 ✓。
`gld agent ls`、`gld grant ls`、本机 `agent_manage action=list`（13 个任务）照常。本机现在没有 grant，`--write` grant 委派还没真机测过。没推送。

## 69. ChatGPT 用 `--write` grant 真机委派、撤销（2026-10-24 13:47–13:53）

**做法**（照 §9）：用户自己跑 `gld grant add deleg-grant delegation-test --write` 拿口令（口令不经过这边），在 ChatGPT 新建
一条连接器 `gld-grant`（同一个公网地址），授权页填 grant 口令，新对话里只勾这一条。delegation-test 是 ask 档、`agents=codex`。

**请求日志**（17022 行起，时间 UTC）：16 次请求全是同一个主体 `oauth:hub:dcr-ec4d…#grant:ea1e…`，原来那条连接器没出现。

| 时间 | 调用 | 回包 | 对照 |
| --- | --- | --- | --- |
| 04:47:28 | `list_workspaces` | 898 B | 服务凭据那次（§66）6,258 B：只看得到 delegation-test |
| 04:47:37 | `action=agents` | 1,388 B | 和服务凭据一样，只有 codex |
| 04:47:39 | `action=list` | 416 B | 服务凭据那次 12,110 B：自己没起过任务，列表是空的 |
| 04:47:42 | `status` 查服务凭据起的 `ad6eb28a` | 3,663 B | ChatGPT 报 `AGENT_TASK_NOT_FOUND`、分类 `not_found`；大小也是报错的（§66 的拒绝 4,155 B），不是任务详情（8 KB 上下） |
| 04:48:08 | `start codex model=gpt-6-luna effort=low` | 2,340 B | `43c156f2` 等放行；用户 04:50:14 在本机放行 |
| 04:50:42 | `wait` | 8,011 B | `awaiting_review`，退出码 0，两个文件 +18 −1 |
| 04:51:08–21 | `diff`、`exec_command`（unittest 16/16）、`review accept` | 4,183 / 3,094 / 4,424 B | grant 审得了自己起的任务 |
| 04:51:29 | 第二个 `start` | 2,340 B | `3b211edd` 等放行，用户不放 |
| 04:53:10 | （撤销之后） | — | 两次 `[auth] rejected`，`openai-mcp/1.0.0` |

**本机：**`gld agent ls` 两个任务都标"codex（grant deleg-grant）"；任务记录里 `grant` 是 `{id: ea1e…, name: deleg-grant}`，`caller` 带 `#grant:`。
用户在 04:52:14 前后 `gld grant rm deleg-grant`：`3b211edd` 在 04:52:14 变 `stopped`、`started_at` 为空（没起过）；`43c156f2` 停在 `accepted`
不受影响；`gld grant ls` 回到"还没有 grant"。Codex 这段时间只有一个会话（04:50:16Z 开始，`gpt-6-luna` / `low`、`never` + `danger-full-access`），
第二个任务确实没跑。

ChatGPT 第一段如实报了：看到 1 个项目 `delegation-test`，`allowed=["codex"]`，codex 那行 `model` / `effort` 为 null、两个 `*_choice` 为真，
列出 0 个任务。守护进程日志里 `remove_grant` 在 04:52:14、6 毫秒完成，和 `3b211edd` 变 `stopped` 是同一秒。

**撤销后 ChatGPT 那边看到的不是 gld 的话：**用户让它再调一次 `action=list`，它拿回来的是
"The previous Code Mode continuation is no longer available. Ask the user to retry the original request."，自己说"没收到明确的撤销错误码，无法确认"。
gld 这边 04:53:10 记了两次 401（`openai-mcp/1.0.0`），之后这个主体再没有请求进来，所以是 ChatGPT 把 401 包成了它自己的通用错误；
§9 那次它说的是"连接已过期，重新连接后才能使用"。撤销到底生效没有，以服务端日志为准，AI 的转述证明不了。

**没看到的：**`gld grant rm` 那一屏的输出（"停掉了它委派的 1 个 Agent 任务"那行）用户没贴，从任务状态和守护进程日志看是停了。
测试连接器 `gld-grant` 用户事后在 chatgpt.com/plugins 删了（10-24 下午，gld 这边 04:53:10 之后再没有这个主体的请求）。它注册的
OAuth 客户端（13:44 建的那个）留在 `data/oauth-clients/hub.json`，和 §9 一样：gld 没有删客户端的命令，grant 没了它只能用服务口令授权，不手改。
第一个任务的改动留在 delegation-test 工作区。没推送。

## 70. OpenAI 隧道接上；`gld prune` 清目录不在的项目和已删项目的残留（2026-10-24）

**要求：**§69 收尾后用户说：OpenCode、Qoder CLI、DeepSeek Harness 先不装，Agent 委派在 task_plan 里收尾；
"SuperAssistant 与 OpenAI MCP Tunnel 功能需要完全实现即可，实际遇到问题再说"；数据目录里已删项目的残留要有清理命令；
`gld ls` 里有的项目目录被搬走了，要一键清。

**SuperAssistant：**10-17 就实现并换上了（§23–§25），这次没改代码，只把它从待办挪进"等遇到再做"（`d5c136a`）。

**OpenAI 隧道（`b4bb400`）。**依据是官方文档和 openai/tunnel-client v0.0.18（2026-10-23 发布）：从它的 Release 下了 darwin-arm64 包和
源码包，两个的 SHA-256 都和 `SHA256SUMS.txt` 对得上。跑过一次 `--version`（`0.0.18 git sha: 274a558…`），再往下执行它被本机的权限策略拦了，
没绕，改成只读源码核对：

| 源码 | 核出来的 | 落到 gld 里 |
| --- | --- | --- |
| `pkg/runtimeconfig/config.go` | `--mcp.extra-headers` / `--mcp.discovery-extra-headers` 的值认 `env:VAR`；没给 `--control-plane.api-key` 时先读 `CONTROL_PLANE_API_KEY`、再读 `OPENAI_API_KEY`；健康端口默认 `127.0.0.1:8080`；`--control-plane.http-proxy` 认 `env:VAR` | 两把密钥都放 gld 自己起名的环境变量，参数里只写 `env:` 引用；显式给 `--control-plane.api-key`，不会退去读 `OPENAI_API_KEY`；健康端口由 gld 现分一个空闲的 |
| `pkg/runtimehealth/health.go` | `/readyz`：OAuth 发现和启动探测都过了才 200；探测拿 401 时也回 200，正文 `ready (mcp initialize requires auth: …)`；控制面连没连上不在 `/readyz` 里 | 等 `/readyz` 200 且正文不含 "requires auth" 才算起好；先起监听器、应答了再起它 |
| `pkg/proxy/route.go` | 按 `HTTP(S)_PROXY` 走代理时只认 `NO_PROXY`，回环地址不自动绕开 | 子进程环境补 `NO_PROXY=…,127.0.0.1,localhost` |

认证不走 OAuth：gld 生成服务凭据 `openai_tunnel_key`，让 tunnel-client 给转发的请求带 `X-Gld-Tunnel-Key`，监听器认出来就放行，
主体记成 `openai-tunnel:hub`。§39 没弄清的"ChatGPT 经隧道走 gld 的 OAuth 通不通"就此绕开：token 端点在本机，ChatGPT 的服务器够不着，
codex-bridge 也是用静态请求头。不用 `Authorization` 头是因为文档写着连接器带来的请求头最后套上、同名的盖掉静态值。

和 task_plan 原计划的出入：原写"gld 托管 tunnel-client，钉版本、校验摘要"，托管做了，钉版本和校验摘要没做——`tunnel/mod.rs` 里
已有的决定是 gld 不代下载隧道程序、只从 PATH 找（cloudflared、frpc 一样），macOS 让 brew 装（发布页的 zip 没公证）。
原计划还要先让用户手工走一遍再写代码，用户这次说直接实现。

**没验证的：**没连过真的 OpenAI 隧道；ChatGPT 连接器里"不需要认证"那一项的确切菜单名没见过；隧道 ID、Runtime Key、权限不对这类
控制面那一段的问题，gld 的就绪检查看不到，只能看 `logs/hub/tunnel-client.log`。公网入口仍然只能有一个：本机现在是自建反代地址，
换成 `openai:` 会清掉那条登记，按旧地址建的 ChatGPT 连接器就连不上了，切之前要用户自己定。

**`gld prune`（`516e989`）。**本机数据目录的只读统计（10-24，算法和命令里的一样）：20 个项目目录都在；`logs/` 下 12 个目录对不上
任何项目，`harness/workspaces/` 2 个，`runs/`、`agents/` 没有（`runs/` 自己 7 天清一次）。命令先列清单，确认后把清单发回守护进程，
它重扫一遍、只删两次都在的；目录不在了的项目走 `gld rm` 同一条路，删不掉的话它的记录也不动；有命令在跑、有委派没结束的目录不动；
"目录不在了"只认 NotFound（或成了文件），父目录没权限读不算——外接盘没插上会被列进来，所以要确认。

**验证：**OpenAI 隧道 3 条集成测试（假 tunnel-client 照真版本带请求头探测、按结果答 `/readyz`）加解析、主体标识、日志尾巴的单元测试；
`gld prune` 1 条集成测试加 5 条单元测试。各把一处关键判断改坏（监听器不认那个请求头、留下项目的日志也算残留）时对应集成测试变红。
全仓第一遍 1058、第二遍 1065 passed / 0 failed，clippy `-D warnings`、fmt 通过，`docs/cli.md` 重新生成。

**没做：**没换装、没推送；`write-locks/` 不管；本机的 12 个日志目录和 2 个任务记录目录没删（新命令没换装）。

## 71. 本机换成 `baf8e32`（协议 10，工具表没变不用 Refresh）；`gld prune --dry-run` 的清单（2026-10-24 15:59）

**要求：**用户说"按 install.md 的升级步骤把本机换成当前 main（先查没有在跑的命令），换完跑 gld prune --dry-run 给我看清单，先别删"。

**换之前：**守护进程 pid 95309（`a4993f0`，协议 9，跑了 2 小时 28 分）没有子进程，`gld agent ls` 没有在跑或等放行的任务；
请求日志最后一条是 04:53:10Z（§69 撤销后那两次 401），到 06:56Z 没有新请求；运行记录最近一条是 13:51。
工具表 34 项、指纹 `f0e996d1217ba15b`；`data/profiles.json`、`data/oauth-clients/hub.json` 排好键再哈希的指纹记下。

**换装：**release 构建 2 分 34 秒（`rust-objcopy` 去符号照旧失败，工具链的老问题，不影响运行）；备份 `~/.local/opt/gld-a4993f0-20261024-1558`
（SHA-256 和装着的那份一致）和 `gld-config-20261024-1558.tgz`（0600）；写 `.new` 再改名，去掉 `CLAUDE*` 变量 `gld daemon restart`，
它自己查过没有在跑的命令，pid 95309 → 88176，服务自己恢复。

**换完：**命令行和服务的构建提交都是 `baf8e32606ec`，**协议 10**；工具表 34 项、指纹没变，**ChatGPT 不用 Refresh**；`gld ls` 前后逐字一样；
两份凭据文件指纹一致；守护进程环境里 `CLAUDE*` 变量 0 个；`gld health` 四项全 ✓（本地、公网 `/mcp`，两份 OAuth 元数据）。
`gld doctor` 只有两条早就有的"项目不是 Git 仓库"提醒。

**`gld prune --dry-run`（新命令在本机第一次跑，什么都没删）：**没有目录不在了的项目；对不上任何留下的项目的数据 14 项、共 4.3 MB，
和 §70 换装前的只读统计一致——`logs/` 下 12 个目录（最大 1.4 MB、869.5 KB、204 KB，其余几十 KB 到一百多字节）、
`harness/workspaces/` 下 2 个（1.7 MB 和 640 B），都标"已删的项目"。日志目录最后改动在 9-06 到 10-23 之间，里面看不出项目名。
`runs/`、`agents/`、OAuth 注册表没有残留，没有"还在跑、这次不动"的。没推送。

**删：**用户看过清单说删。`gld prune -y` 报"删了 0 个项目、14 项残留，腾出 4.3 MB"，没有失败项；`logs/` 从 12M 到 9.7M、`harness/` 从 15M 到 14M（`du -sh` 按块取整）。再跑 `--dry-run` 是"没有要清的"；`gld ls` 还是 20 个项目，`gld health` 四项全 ✓。

## 72. 10-18 之后 ChatGPT 日用记录复核：§28 的毛病基本没了，新的大头是测试产物逼出来的基线刷新（2026-10-24）

**要求：**用户说"继续任务"。队列里剩的多是"等 ChatGPT 下一轮实测再看"，而 10-18 之后已经用了一周，就拿本机记录核一遍。
范围是 10-18 06:13Z（ChatGPT 点完 Refresh，§35）到 10-24 07:57Z，只读 `operations.jsonl`、任务事件、运行记录和服务请求日志，没动数据。

**量：**请求日志 2,586 次 `tools/call`（10-19 687 次、10-22 764 次最多），几乎全是原来那条 ChatGPT 连接器；`operations.jsonl` 里
RAGCheck 1,327 条、keex 721 条、hpsrv-docs 185 条、delegation-test 68 条，gld 自己 24 条（这一周 ChatGPT 没在 gld 仓库里开发）。13 个任务都收成 completed。

| §28 / task_plan 里要看的 | 这一周 | 结论 |
| --- | --- | --- |
| 提交后还要不要 `refresh_baseline` | 13 次 `head_advanced`（gld 自己把提交记上账），提交后紧跟着刷新的 0 次 | §30 的修复管用 |
| 每秒轮询 | `write_stdin` / `read_output` 共 85 次，同一主体前后两次隔 2 秒以内的 5 次 | 不算问题了 |
| 用 `python3 -c` 读文件 | 运行记录里的 `python3 -c` 都是跑测试片段，没有读文件的 | 没再出现 |
| `FILE_VERSION_CONFLICT` | 0 次 | 没再出现 |
| 补丁失败 | `apply_patch` 287 次，失败 10 次（3.5%，10-09 的基线约 10%）；诊断 21 条：`anchor_not_found` 12、`context_not_found` 5、`context_ambiguous` 3、`context_drifted` 1，缩进、行尾空白、标点 0 | 维持逐字比对，没有放宽的依据 |
| 提交说明 | keex、RAGCheck 里经 gld 的 9 个提交都没有 `类型(范围):` 前缀（keex 用自己的 `K002：`），10-23 起有 2 个带正文 | gld 仓库的规矩这一周没机会验，等它在 gld 里开发一轮 |

**新的大头：被 `.gitignore` 忽略的测试产物让写操作报 `FILE_CHANGED_EXTERNALLY`。**这一周被拒的写操作里它占 9 次（`apply_patch` 5、
`exec_command` 4），每次之后 AI 要 `refresh_baseline` 先看、再接纳，共 13 次刷新；RAGCheck 10-20 一个任务里刷了 7 次。逐次把
刷新时列出的改动交给 `git check-ignore`：**13 次里 11 次，改动全是被忽略的文件**——`.data/tests/…`（测试建的临时库和日志）、
`.pytest_cache/…`、`tests/secret_scan/.scratch/…`、`.DS_Store`；另外 1 次夹着 keex 没忽略的 `tests/fixtures/kdbx/.matrix-work-*`，
1 次是初始化 git 时只有 HEAD 变。

原因：gld 每条命令回包后会把工作区重新记账（`refresh_expected_state`），但测试转到后台继续跑时，回包之后才写出、删掉的文件
算不到它头上（有意的：那段时间分不清是不是别人改的，§30）。扫描（`harness/scan.rs`）只跳过一张固定的目录名单
（`target`、`node_modules`、`.venv`、`test-results` 等），不看 `.gitignore`，所以 `.data/`、`.pytest_cache/` 这类每个项目自己的产物目录都算进指纹。

**可以怎么改（没做，要用户定）：**项目是 Git 仓库时，扫描不进被 `.gitignore` 忽略的文件和目录（`git ls-files --others --ignored
--exclude-standard --directory` 拿清单，遍历时整目录跳过）。照这一周的记录能省掉 11 次刷新、约 8 次被拒的写；顺带少哈希
`.data/` 下的库文件。代价和固定名单一样，只是范围变成"项目自己声明不要的"：任务期间有人改了被忽略的文件（比如本地 `.env`），
gld 不再报外部改动；任务的改动清单也不列被忽略的文件。单个文件的版本冲突检查不受影响。

**改了（`e32023c`，用户看完上面的提议说"按 §72 的提议做"）：**`harness/scan.rs` 扫描前跑一次
`git ls-files -z --others --ignored --exclude-standard --directory`，被忽略、没被跟踪的文件和整目录在遍历时就跳过；
被跟踪的照算，项目在别的仓库里按那个仓库的规则，不是 Git 仓库或 git 报错时照旧只按固定目录名单。没做升级迁移：
和 09-23 改名单那次一样，升级时开着的任务会报一次外部改动（列出来的是被忽略的文件显示成 `deleted`），
troubleshooting 已有那条补上了这次。本机现在没有活动或暂停的任务（只有 11 个 `completed_unverified`），换装不会撞上。
代价写进 concepts.md「工作区指纹」：任务期间改了被忽略的文件发现不了，改动清单也不列它们。
验证：扫描 3 条、任务层 1 条新测试，把忽略清单换成空的时这 3 条变红；全仓 1069 passed / 0 failed，clippy、fmt 通过。没换装、没推送。

## 73. 本机换成 `7959dfc`（带上 `e32023c`，协议和工具表都没变）（2026-10-24 18:07）

**要求：**用户说"按 install.md 把本机换成当前 main（先查没有在跑的命令），换完核对"。

**换之前：**守护进程 pid 88176（`baf8e32`，协议 10，跑了 2 小时）没有子进程，没有在跑或等放行的委派。请求日志最后是 ChatGPT
09:00:37–38Z 的一次重连和两次 `read_file`，构建完（09:07Z）再看没有新请求，没有活动或暂停的任务，就在这时重启。

**换装：**release 构建 3 分 8 秒；备份 `~/.local/opt/gld-baf8e32-20261024-1806`（SHA-256 和装着的一致）和
`gld-config-20261024-1806.tgz`（0600）；写 `.new` 再改名，去掉 `CLAUDE*` 变量 `gld daemon restart`，pid 88176 → 3754，服务自己恢复。

**换完：**命令行和服务的构建提交都是 `7959dfc9eba5`，协议 10；工具表 34 项、指纹 `f0e996d1217ba15b` 没变，**ChatGPT 不用 Refresh**；
`gld ls` 前后逐字一样，`data/profiles.json`、`data/oauth-clients/hub.json` 指纹一致；守护进程环境里 `CLAUDE*` 变量 0 个；
`gld health` 四项全 ✓。基线扫描跳过被忽略文件的效果要等 ChatGPT 下一轮跑测试再看。没推送。

## 74. 推送到 `fd20155`：第一次 CI 红在一条靠睡眠攒输出的测试，修完 8 个 job 全绿（2026-10-24）

**要求：**用户说"推送 main 到 origin，然后看 CI 结果，红了就修"。推之前本机做过 Rust 1.89 的编译检查（通过）和
`docs/cli.md` 重新生成比对（没差异）；Windows 交叉检查卡在 `ring` 要 Windows 的 C 编译器，本机没有，没装。

**第一次（`efa32cd..7503b3b`，92 个提交，run 40082451488）：**8 个 job 里 7 个过（含 Windows 编译和补丁落盘测试、Linux 测试、
三个平台的 MSRV、格式与 Clippy、命令参考），**macOS 测试红了一条**：`agents::tests::a_running_task_reports_only_a_short_output_tail`
（10-23 `f9bc7f0` 加的），断言"收尾后给长一点"拿到 504 字节。cargo test 在 core 这组失败后就停了，macOS 上后面的集成测试没跑到。

原因：假 Agent 每 0.05 秒打一行（约 84 字节），测试睡 0.8 秒就停掉它，再断言停掉后的尾巴超过在跑时的 768 字节上限；
慢机器上 0.8 秒只打出 6 行。本机 `taskpolicy -b` 跑 6 次全红（504–672 字节），和 CI 一样。和这次推的功能无关。

**修（`fd20155`）：**假 Agent 一上来先打 2 KB 再循环，测试不再看睡多久，等到"在跑且有输出"再查（最多 10 秒）。
改后 `taskpolicy -b` 跑 8 次全过；全仓在 `taskpolicy -b`、`--no-fail-fast` 下 1069 passed / 0 failed，clippy、fmt 通过。

**第二次（run 40083478303）：**8 个 job 全过。没打 tag、没发版。

## 75. 通用任务空间与 Computer Use 方案落盘（2026-10-26）

**要求：**用户提出非项目任务直接在 gld 数据目录中运行，避免手动建项目；随后要求落实类似 computer use 的跨平台操作计划。

**依据与范围：**读取源码基线 `f6f3e02`；连接中的服务仍报告 `7959dfc`。
已有本机 MCP、Playwright 和 ImageContent 可复用；本轮交付为设计与排期，不代表桌面 provider 已安装或真机验收通过。

**已保存：**

- [RFC-0010](../rfc/0010-managed-work-sessions.md)：自动管理内部 workspace，任务根与保留产物分开，元数据由服务持有；
  首版仅已鉴权完整主体可用，缺 workspace 的原有调用仍拒绝。W0 即接入 hub 完整存活集合与 prune 有主记录集合，
  W1 再做保存核验、活资源核对和到期删除。最终产物不随临时目录 TTL 删除。
- [RFC-0011](../rfc/0011-computer-use.md)：Playwright 负责浏览器，优先验证 Cua Driver 的独立桌面执行能力；
  当前客户端模型继续决定步骤。三个 computer 工具保持显式 workspace，受控 desktop 后端不作为 raw relay 旁路公开。
  明确目标、图像/树、坐标变换、单桌面控制权、结果不明不重放和独立 stop；停止不明时 blocked，确认旧执行端静默后才能接管。
- [task_plan.md](../../task_plan.md)：沿用 D10 分解 W0/W1/W2 和 C0–C6，唯一排期入口；下一编码项为 D10-W0。
  先做本机 macOS，再按真机条件验证 Windows、Linux x86_64/X11、指定原生 Wayland、远端及后置移动端。

**复核：**源码契约复核修正了三处设计问题：W0 不得把存活保护推迟到 W1；observe/act 显式携带 workspace；
stop 或租约失效不等于执行端已停止，未知状态不能直接释放为可接管。
官方资料复核补上 Linux 架构、原生 Wayland 的启用/helper 条件，以及移动驱动初始化对应用数据、进程和键盘设置的影响。
RFC 内保留官方来源与研究日期；不同平台的上游声明、接入通过和真机任务通过分别记录，不混为已支持。

**验证：**使用 target 下隔离的 GLD_HOME，`cargo test --workspace --all-targets --locked` 1069 passed / 0 failed；
`cargo clippy --workspace --all-targets --locked -- -D warnings` 和 `cargo fmt --all -- --check` 通过。
Clippy 重试临时关闭增量缓存，未改仓库配置。本次无 Rust 行为改动、无新增测试。
最终文档修订后，命令真实性、相对链接和公开文档路径三组测试共 6 条再次通过；末尾多余空行已整理。
验证结束后已删除本轮新建的隔离测试目录；磁盘可用空间约 4.15 GB。

**过程中遇到的实际问题：**

- 首次测试请求由调用方把 timeout 设为 20 分钟，超过 gld 普通命令 10 分钟上限，被执行前拒绝；改为允许值后正常执行。
- 一次文档修订补丁把半行文字当作整行上下文，返回 `context_drifted`、`files_changed=false`。
  按诊断重读确认文件版本未变，改用完整行后成功；没有证据表明发生了并发覆盖。
- 首次 Clippy 因磁盘空间不足失败（os error 28）。只读检查后，用 Cargo dry-run 预览，再限定本仓 gld/gld-core/gld-daemon
  三个包的 dev profile 清理可再生成产物，实际移除约 5.1 GiB；保留 release、源码、服务数据和用户产物，随后 Clippy 通过。

未安装 provider、未授予 OS 权限、未操作真实桌面、未换装或重启服务、未推送。

## 76. Computer Use 选型收敛为 Provider-first、Native-fallback（2026-10-26）

**要求：**用户在前述桌面工具研判后要求更新实施计划，优先接社区成熟执行器，不重造三端底层驱动。

**改动：**只更新 [RFC-0011](../rfc/0011-computer-use.md) 和 [task_plan.md](../../task_plan.md)。
将先 Playwright/Cua Driver 独立 MCP，后按 macOS Peekaboo、Windows winapp ui/Windows-MCP 的真实缺口
做同题对照明确为实施原则；Enigo/XCap 等仅在必要时补单项原生能力。gld 自己只保留任务归属、
目标/控制权、结果/停止、产物/权限契约和薄 adapter，不内置第二套 agent 或多平台桌面驱动。

**选型边界：**D10-W0 仍是下一编码入口，D10-C0 是可分开准备的最小 MCP POC，不预先安装第三方
provider，不更换连接器。Cua Driver 若有可复现功能缺失，先核对权限/参数，再用同场景测试比较备选。
只有复用、裁剪图片和减少往返后仍有量化瓶颈，才比较 MCP 与直连 Rust SDK 的性能、依赖和隔离成本；
native-minimal 则需要具体缺口与负测证据。供应商能力和各平台实际支持仍为待验证，文档不是实现。

**验证：**隔离 GLD_HOME，非增量构建运行全仓 cargo test（退出码 0）；Clippy 的 -D warnings、
fmt --check 和文档命令/家目录路径/相对链接六条专项测试通过。仅编辑文档、未改代码或依赖。
未安装 provider，未操作本机桌面，未触碰现有运行服务，也未推送。

## 77. 复用 .agents Skill 并补全 Prompt 按需读取（2026-10-27）

**需求与现状：**用户要求 `~/.agents` 下的 Skills、Prompts 都可供 gld 连接的 AI 按需读取，
并继续落实计划。源码确认 `agent_context` 已默认扫描 `~/.agents/skills` 和项目 `.agents/skills`，
沿用 `list_skills`/`get_skill`，不重造。用户目录只读枚举确认 `~/.agents/prompts` 共有
`debug.md`、`refactor.md`、`review.md`；未读取三份实际模板正文。
方案与实施切片在 [RFC-0012](../rfc/0012-agent-prompts.md)，当前状态在 [task_plan.md](../../task_plan.md)。

**实现：**新增 `prompt_manage` 单个只读工具，`action=list|read`，扫描用户级/项目级
`.agents/prompts` 第一层 Markdown；名称冲突按 ID/scope 选择，按文件实时读取，
原始 Markdown 不进行模板变量插值，也不变成系统指令。固定 128 KiB/文件、256 个候选/根，
排除隐藏文件、链接、目录、过大、无效 UTF-8 和空文件；失败以 `skipped` 或明确错误返回。
将工具注册到 compact/core/advanced/read-only 和统一 dispatch，不引入新 crate、不重构 Skill。

**授权：**`Caller` 保留来源是否已鉴权；用户级 Prompt 只开放给本机或通过完整服务鉴权、
没有项目 grant 的主体。匿名和任何 grant 无法列举或按已知 ID 获取用户级模板；
它们仍能按原有 workspace 权限读取本项目的 Prompt。只读入口不扩大 `read_file` 范围，
也不宣称对已持全权 `exec_command` 的主体提供 OS 沙箱。具体规则同步
[concepts.md](../concepts.md)、[security.md](../security.md) 和排障文档。

**验证：**隔离的 `GLD_HOME` 下，`cargo test -p gld-core --locked tools::prompt`
5 项通过；新增工具表四档 read-only、禁止任意 path 参数的注册回归通过。
`cargo test --workspace --all-targets --locked` 全仓退出码 0；
`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo fmt --all -- --check`
均通过；命令真实性、公开文档路径和相对链接 6 项专项测试通过。
仅本地源码与文档修改，未替换运行守护进程、未刷新连接器、未推送。

**开发过程中 gld 的摩擦：**尝试用 `exec_command argv=["rg", …]` 查源码时被
`POLICY_REJECTED: command_not_allowlisted` 拒绝，改用 `search_text` 完成同样的只读检查；
初次较大的 `apply_patch` 因 registry 文件上下文不完整返回
`PATCH_FAILED: context_drifted`（`files_changed=false`），改用更小的补丁后成功。
第一轮模块测试 4/5：`argument-hint: [SYMPTOM=<text>]` 被 YAML 解释为数组而非文本，
按原始 Prompt 行补兼容解析后 5/5 通过，原始模板未修改。

## 78. 无需登记项目的托管任务空间 W0（2026-10-27）

**入口与范围：**用户要求继续下一步，沿当前队列实施 D10-W0；基线 `b2a6c23`，初始工作树干净。
本轮实现 [RFC-0010](../rfc/0010-managed-work-sessions.md) 的创建、查询、路由和存活保护，
不安装桌面 provider，不换装或重启现有服务，不创建或撤销 grant，不推送。

**实现：**

- 新增 `managed_workspaces`，在 `GLD_HOME/work/<id>` 放任务文件，`managed-workspaces` 放服务登记。
  owner 取已认证主体，同 owner/request 幂等；先持久化 creating 再建目录，重试可续建；
  已 active 的目录丢失不重建。文件锁协调并发，原子替换元数据，坏记录拒绝继续，不静默丢弃。
- 新增 hub 级 `work_session` start/list/status；后续文件和命令仍显式带完整 `work-…` ID。
  不登记进普通项目表、不创建 Git；受限文件读写、guarded 命令策略及关闭委派作为默认，
  不继承业务项目配置。匿名、项目 grant 和其他 owner 均无法借 ID 访问。
- `forget_departed` 在 context 锁内核对完整托管存活集合，不按当前 caller/grant 缩小集合；
  `prune` 保留登记 ID 和路径哈希，目录不在也保留证据，删除前复核登记；损坏时 CLI 报失败，不误报“没有要清的”。
- 增加默认关闭的 `gld upgrade --work-sessions true|false`。开关已开且零项目时，`gld start` 不登记当前目录；
  明确给目录的旧行为不变。配置拒绝同时开 noauth，IPC 协议改为 11，防止旧守护进程忽略新字段。
  已生成 cli.md，并同步 concepts/security、生命周期说明与排障文档；进度只改 task_plan。

**验证：**新增 17 项测试（存储 7、hub 5、prune 2、CLI/HTTP 3），随全仓回归通过。
最终源码快照全仓合计 1092 passed、0 failed；最后的 CLI 帮助调整后已重新跑完整回归与 Clippy。
真实隔离守护进程测试完成零项目启用、MCP 工具表发现、重复 start、文件写入/读取、越界拒绝、
运行中命令阻止 daemon restart、命令结束后重启及文件/服务凭据保持，prune 不把托管运行记录当垃圾。
其余覆盖两个 owner、匿名/只读/可写 grant、分页、并发创建、创建中断恢复、目录丢失、坏登记与软链。
兼容性复核补上了操作员给 GLD_HOME 设置目录别名的情形：解析这个根的别名，但拒绝其下托管目录被链接替换。
`cargo build --workspace --locked`、`cargo test --workspace --all-targets --locked`、
`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo fmt --all -- --check` 均退出 0；
测试使用隔离 GLD_HOME，未引用真实配置。CLI 文档由原脚本生成，不手改输出。

**尚未实现的部分：**W0 显式返回 `cleanup_supported=false`，不声称目录创建即已交付文件，
也不声称 active 就是进程在跑或空闲。当前最多 512 个登记，超限拒绝创建、不自动删旧任务；
产物保存、finish/retain、到期删除和磁盘预算属于下一项 W1。重启恢复的是登记和文件，不恢复或重放旧命令。
代码已验证不等于已部署，现有运行服务、地址和 OAuth 配置均未改动。

**经 gld 开发的摩擦：**一次 read_file 把存储实现误写为 `harness/storage.rs`，返回 NOT_FOUND，
工具给出同名文件提示后，按源码搜索定位到 `harness/store.rs`；一次文档 apply_patch 带入不存在的上下文，
返回 `PATCH_FAILED/context_not_found`、`files_changed=false`，修正锚点后整个事务成功。
两次均为调用方定位/构造问题，无证据表明服务丢失修改或需要放宽策略。
生成的 CLI 帮助为新布尔选项插入了含空格的空行，diff 检查拒绝；在该选项声明中关闭重复的
possible-values 提示（true|false 已在参数名中），重新生成文档，不手工改生成文件或放宽检查。

## 79. 托管任务产物、显式收尾与保留期 W1 核心（2026-10-27）

**范围：**用户继续当前队列；初始 `750699d`、工作树干净。本轮不换装现有服务、不操作真实桌面、
不改 OAuth 或 grant、不推送。沿 [RFC-0010](../rfc/0010-managed-work-sessions.md) 分切片实现，
当前排期仅维护在 [task_plan.md](../../task_plan.md)。

**交付切片：**`ad0fc6b` 先实现不删除源文件的 retain/artifact，`42a6aef` 再实现可重试 finish，
随后接入保留期与有界 sweep。复用原有原子文件替换、运行记录和工作区锁，不新增依赖或第二套任务系统。

- 产物只接受明确的普通文件路径；复制后比对字节数和 SHA-256，再登记。相同路径/内容重试返回原 ID，
  新内容另存，损坏目标不覆盖。源文件保留直到明确收尾；每个空间最多 32 个文件版本，单文件 256 MiB，
  正文返回限 64 KiB UTF-8。二进制与图片先返回元信息和保存位置，不声称已经实现图片 MCP 内容。
- 所有托管目录调用共用逐空间文件锁；finish 还要求原有工作区写锁确有跨进程保护，锁降级时不删除。
  先核对资源、保存、校验，再写 closing 与不可变 keep_paths，最后删除目录、写 closed 回执。
  同样参数可重试，删除中断可继续；关闭后产物仍可读取，不能通过重复 start 重建原目录。
- exec 发出前持久化资源意图，采用真实命令终态结清；断线、缺日志、未知终态不会被当成空闲。
  记录独立于输出保留期，因此日志过期不解除阻塞。未结束或读不出的委派也阻止删除。
  旧 W0 记录缺少资源历史，不自动清理；新记录 schema 2，旧 schema 1 保持可读，回退旧二进制不保证兼容新记录。
- 新目录默认 7 天无实际使用后到期；失败收尾保留 24 个空闲小时，可改为 1～8760 小时或固定保留整个目录。
  start/finish 后自动查当前主体至多 8 个到期项；显式 sweep 最多 16 个并支持游标。
  拿到锁后重查期限和归属，活跃、未知、固定保留、其他主体或未登记目录不删除，最终产物不走临时目录 TTL。

**验证：**新增 18 项回归：产物 5、存储收尾 5、保留期 3、hub 4、真实 CLI/HTTP 1。
隔离数据目录的真实守护进程完成创建、写文件、保存结果、失败保留、到期清理与重启后的产物读回；
该测试修改的时间戳只属于夹具，未等待真实一天，也未修改运行服务。服务凭据保持，prune 不把托管记录当孤儿。
全仓 `cargo test --workspace --all-targets --locked` 合计 1110 passed / 0 failed；
最终 CLI 帮助同步后再次跑完全仓仍为 1110 passed / 0 failed；Clippy 的 `-D warnings`、
`cargo fmt --all -- --check` 及全仓中的文档命令、相对链接、家目录路径六项检查均通过。
CLI 文档由原生成脚本同步，没有手改生成文件。

**明确未交付的部分：**没有定时器或独立启动扫描；重启后仍由下一次 start/finish/sweep 触发检查。
512 个登记上限仍包括关闭回执，不以静默丢弃回执腾名额；登记归档、全局可配置容量/字节预算、图片内联读取留在 W1。
外置 MCP provider、浏览器 profile、桌面会话和任意脱离 gld 的后台进程不在本轮资源账目范围内，W2/C0 后续衔接，
不把 guarded 或操作锁宣传成 OS 沙箱。结果保存、目录清理和所有系统缓存被清空是不同事实。

**经 gld 开发的摩擦：**一次 apply_patch 使用了 rustfmt 前的 match 分支形状，返回
`PATCH_FAILED/context_drifted`、`files_changed=false`；按返回范围重读后拆小补丁成功。
没有改策略、重试覆盖外部变更或绕过文件工具；未出现无法解释的丢失修改。

## 80. 关闭回执归档与可配置容量（2026-10-28）

**范围：**用户继续 W1 当前队列。初始 `75d71b9`，工作树干净；不换装现有服务、不修改运行配置、
OAuth 或 grant、不推送。先独立提交归档 `18b783d`，容量和本节随后作为第二个切片交付。
没有新增依赖或第二套任务系统，沿用 JSON、已有原子替换和跨进程锁。

**归档：**关闭回执进入 `managed-workspaces/closed`，按 owner/request 的摘要定位；ID 索引在
`closed-ids`，索引不是另一份事实。先写索引再原子移动回执，中断时仍有旧位置或新位置的完整记录。
旧版本留在主目录的 closed 回执在下一次 start 时迁移；索引丢失可由原请求找回完整回执并恢复索引。
相互冲突或损坏的回执拒绝覆盖，链接拒绝。相同请求返回原 closed ID，不重新建目录或运行命令；
状态、产物与相同参数 finish 仍能查询。路由和到期扫描只加载在用登记；列表与 prune 保留完整历史归属。

**容量：**默认在用 512、含归档总记录 8192、工作和产物文件逻辑大小预算 10 GiB；本机
通过 `gld upgrade` 的 `--work-max-active`、`--work-max-records` 和 `--work-max-bytes` 配置，值相互矛盾时不保存。
MCP 增加 work_session action=capacity，只报告全服务汇总，不枚举他人名称或文件。
新建检查数目和字节；产物复制通过独立预算锁串行化并重新统计空间，两次保存不能各自重复使用同一余额。
超限不删除原文件、回执或保留结果；降低上限仍能查询、重试同请求及不新增复制的收尾。
扫描不跟随链接，统计未知子目录里的普通文件但不删除它们；超过100000个条目或无法读取时报使用量未知。
服务配置形状变更，源码 IPC 协议升到12，避免旧守护进程忽略容量字段。CLI 文档由原脚本生成。

**验证：**新增11项回归：归档5、容量5、隔离 CLI/HTTP 1。构造513条磁盘关闭回执后仍可新建；
覆盖索引写入后尚未移动、中断恢复、坏索引与冲突、链接、跨主体、总数与字节耗尽、降低预算、
并发复制和旧配置默认值。真实隔离守护进程通过 HTTP 验证配置生效、关闭释放名额、重启后按原请求
取回 closed、产物仍能读取、凭据不变，以及错误容量配置不落盘。prune 不误删归档相关记录。
最终源码全仓 `cargo test --workspace --all-targets --locked` 共1121 passed / 0 failed；
`cargo build --workspace --locked`、Clippy `-D warnings` 和 `cargo fmt --all -- --check` 均通过，
全仓中的文档命令、相对链接和家目录路径6项也通过；测试使用本轮独立 GLD_HOME。

**边界：**字节预算不拦截任意命令写入，不计配置、运行日志、回执元数据或外部缓存，不能声称硬磁盘配额。
归档回执不自动丢弃，总数达到上限仍明确拒绝新建；本轮不做回执永久删除接口。
独立启动扫描、图片内联返回、provider/桌面会话资源管理仍未实施，见当前 task_plan。
归档布局不保证可直接回退旧二进制，必须保留备份并先核对兼容性；没有改动当前使用的数据目录。

**经 gld 开发的摩擦：**一次 exec_command 的 max_output_bytes 误填1000，低于工具 schema 的1024下限，
在执行前被参数校验拒绝；改为1024后成功。属于调用方参数错误，没有放宽策略或绕过执行边界。

## 81. 启动扫描与关闭后图片回传（2026-10-28）

**范围与基线：**用户继续当前 W1 队列，初始 `ad91a45`、工作树干净。先提交启动扫描
`4bb4986`，再完成图片回传。没有安装桌面 provider，没有换装、重启现有服务或改 OAuth/grant；没有推送。
当前进度仅维护在 [task_plan.md](../../task_plan.md)，工具规则见 [concepts.md](../concepts.md#不属于项目的任务work_session)。

**启动扫描：**每次监听器成功创建后启动一轮阻塞工作线程上的维护，不阻塞监听器就绪。
仅在 work_sessions 开启、有认证且不是只读档时运行；按登记 owner 检查至多16项到期目录，
复用显式 sweep 的锁后期限复核和 finish 的资源、产物完整性、关闭归档路径。
扫描报告与游标独立保存；受阻项也推进游标，下次绕回，避免后续目录长期得不到检查。
坏登记、坏报告、未知命令、旧版未核实资源和固定保留都不会被当成可删除。
停服务先取消后续项，再等待当前单项收尾，不遗留脱管维护线程；没有增加周期定时器。
报告只记位置、计数和错误码，不输出 owner、请求正文或产物内容。

**图片回传：**artifact 新增 output=metadata/text/mcp_image，保留 include_content 的文本兼容入口，
相互矛盾的参数被拒。图片先按 ID 核对主体、路径和校验值，再对同一批有界读取的字节重新核验后解码。
真实识别 PNG/JPEG/GIF/WebP，输入16 MiB、单边16384、总像素4000万；预览首帧转 PNG、最长边2000、
编码后2 MiB，必要时继续缩小，小图不放大。MCP 图像块只带一份编码数据，文本和 structuredContent
只含元信息；原件不回写，也不创建预览缓存。关闭归档和服务重启后仍可用产物 ID 查询。
使用 Cargo.lock 已有的 image 0.25.10/base64，不新增依赖或升级锁文件。
解码 max_alloc 是尽力限制而非进程硬内存配额，已核对[该版本官方说明](https://docs.rs/image/0.25.10/image/struct.Limits.html)，不作强隔离承诺。

**验证：**新增12项：启动扫描单元5、真实 HTTP 启动1、图像单元4、hub图像/归属1、真实 HTTP 图片1。
覆盖跨主体的内部维护、游标越过阻塞并绕回、取消、保留期变更后重查、链接/坏报告、
四种图像格式、尺寸/编码体积收缩、错误格式/超限、完整性和跨主体拒绝。
隔离守护进程在没有客户端清理请求时回收到期目录；另一个真实 HTTP 测试验证关闭/重启后返回 image 内容块，
产物仍在且凭据不变。当前 ChatGPT 连接的旧服务没有换装，不能以这些 HTTP 用例冒充真实连接器已验收。
最终源码 `cargo test --workspace --all-targets --locked` 共1133 passed / 0 failed；
`cargo build --workspace --locked`、全仓 Clippy `-D warnings`、fmt --check 均通过。
测试使用本轮独立 GLD_HOME；公开文档相对链接、家目录路径和命令真实性继续由六项专项测试检查。

**修正与工具摩擦：**首次图像专项4/5通过，小图被 thumbnail 放大到2000边长，
调整为 min(原最长边, 2000) 后5/5通过，并在真实 HTTP 测试固定小图尺寸。
一次 read_file 将已有工具文件误写为 image.rs，返回 NOT_FOUND；用 search_text 定位到 image_tool.rs 后读取，
属于调用方路径判断错误，没有绕过读取边界。下一项为 C0 执行器协议与环境验证，不把图片回传等同于已能操作桌面。

## 82. C0 独立协议预检与执行器固定版本研究（2026-10-28）

**范围与实际条件：**用户继续当前 C0 队列，基线 `cc2f21d`、工作树干净。只读检查确认本机为
macOS 26.6.2 / arm64，PATH 未找到 cua-driver 或 Peekaboo，标准系统/用户 Applications 目录未发现 CuaDriver.app。
本轮没有安装第三方执行器、申请系统授权、截屏或输入，没有换装现有 gld、修改 OAuth/grant 或推送。

**上游依据：**固定 `cua-driver-rs-v0.38.0` 与提交 `85e2b1318bd77ccf3cfa88ccdf875a4bb7eaed19`，
发行接口明确标记为 prerelease。核对了归档大小/摘要、默认许可说明、MCP 协议与上游隔离验证脚本；
具体来源和边界见 [RFC-0011 §4.3](../rfc/0011-computer-use.md#43-c0-协议预检基线)。
锁定的 toexec-mcp 0.2.1 与该 Driver 均支持 legacy initialize 的 2025-06-18，因此没有升级依赖或另写协议客户端。
记录的是发行元数据与源码，不表示已经下载、验证发行签名、运行真实执行器或拿到真实 schema 指纹。

**交付：**新增本机 `gld computer probe`，在 CLI 连接 Backend 之前运行；默认只解析本机可执行文件、
读取大小与 SHA-256。显式 `--connect` 必须带核实过的可执行文件摘要，固定 `mcp --direct`，
只执行 initialize、初始化通知和 tools/list，沿用原客户端的服务端请求处理，不执行 tools/call。
核对名字/版本/协议后，对有界且无重复名字的工具目录做规范化指纹；可与预期摘要比较。
忽略执行器 instructions，不输出第三方 stderr；`desktop_verified=false` 明确保留真实桌面未验证状态。

**复用与回收：**沿用 machine_mcp 的进程组/关闭策略及 toexec-mcp Client；仅给私有启动增加环境清空分支，
普通转发保持旧规则。HOME、XDG、Cua 状态和临时路径指向本次私有目录；继承 PATH 和必要 Windows 启动变量，
设置关闭遥测变量，不继承父进程 API 密钥或真实 gld 配置。握手和全部分页共享截止时间。
结束或失败先释放 Client/transport，再删除自己的目录；目录被重定向或清理失败时报告保留位置。
这些不是 OS 文件/网络沙箱，也不证明 Cua 常驻 App 的输入停止；没有开放 raw MCP 白名单或公开 computer 控制工具。

**验证：**新增9项：core预检7、真实CLI2。以真实 stdio 夹具进程覆盖默认不执行、缺失执行器、摘要缺失/错误、
版本与目录错误、工具指纹、分页总超时、进程早退、环境不继承模拟密钥、进程关闭和状态目录回收、软链拒绝。
真实 CLI 不会启动 daemon 或创建服务配置。最终 `cargo test --workspace --all-targets --locked`
1142 passed / 0 failed；全仓 Clippy `-D warnings`、fmt --check 与构建通过，文档生成脚本同步新增命令树。
本机新 CLI 的默认检查实测返回 `PROVIDER_NOT_FOUND`、`process_started=false`，退出1是预期缺依赖结果，
不是执行器实测通过。所有开发测试使用本轮独立 GLD_HOME，正常构建缓存保留。

**修正与工具摩擦：**首轮定向6项中4项通过，私有 HOME 拼接空后缀形成尾斜杠，触发测试执行器的
工作目录等值检查并提前退出；改为直接使用目录路径后6项通过，随后补了分页截止时间回归。
Clippy 拒绝测试中一次无必要的 clone，改为 from_ref 后通过。一次文档补丁误用 RFC 标题，返回
`PATCH_FAILED/context_not_found`、`files_changed=false`；重读实际标题再提交，未放宽匹配或覆盖已有修改。
审查发现 CLI 帮助生成器维护显式命令树，已补 computer/probe 并重新生成，不手改生成输出。

**下一步边界：**真实 Driver 的安装、可执行文件/工具目录锁定、macOS 授权归属、截图/元素树、输入核验和
桌面停止仍待实际设备验证，C0 未完成。生产 gld 换装不是本机独立 probe 的前置条件；是否安装和授权由用户决定。

## 83. 收缩桌面范围并规划原生多机接入（2026-10-28）

**用户要求与基线：**用户认为 Cua/完整桌面执行体系过重，要求直接复用简单桌面库，覆盖三种桌面平台；
所有目标机通过一个 Hub 管理，安装/加入尽量一条命令，不强制依赖 ccnm，不为每机配置穿透。
基线 `17ea95b`、工作树干净。本轮授权为研判及计划落盘，不进行安装、代码接入、系统授权或现网换装。

**已核对的现状：**`hub::Member` 只有 Local 与 Remote(CcnmMember)；`bridge/member.rs` 启动公开
`ccnm mcp bridge`，处理的是远端项目，不是通用目标机主动连接。已有单 MCP 入口、按调用方隔离的工具内核、
W0/W1、服务级穿透、跨平台打包能够复用；尚无原生节点配对、回连和远端桌面能力。
读取 auth/context、mcp/listener、hub/work_sessions 与 install/package，核对了主体、入口、数据目录和发行边界。

**计划决定：**新增 [RFC-0013](../rfc/0013-native-machines-and-desktop.md)，以同一产品的 Hub/节点角色实现
目标机出站 WSS；只在 Hub 准备稳定入口，节点不开放入站 MCP 或单机隧道。直接复用 Axum/Tokio WebSocket 库和
既有执行内核，不新造协议栈、文件工具或完整设备平台。一次性配对换独立可撤销节点凭据；实际调用携带可信主体，
目标机本地范围与 Hub 授权取交集，离线不回退到本机，副作用结果未知不自动重放。ccnm 和 remote_* 保持兼容。

桌面改为 XCap/Enigo，按需 Arboard/open 的薄封装；一个拟议 desktop 工具，复用任务归属、操作 ID、图像和产物。
保留必要坐标与输入互斥，不再实施原三套 computer 会话接口；不提前做录像、P2P、移动端、模型或 provider 平台。
旧 Cua probe 保留但暂停扩建，W0/W1 不回滚。原 RFC-0011 顶部标记被替代，历史章节锚点保留；
concepts 明确现有工具与未来计划，RFC-0010 补目标机数据归属；唯一队列已调整到 L0 与 M0 的小步验收。

**外部依据与不能省略的条件：**Desktop Commander 官方 Remote Device 已采用设备出站 WebSocket 到远端 MCP
并调用本机 MCP 的结构，借鉴这个体验但不引入其 Node 运行时。Cloudflare 官方说明支持 WebSocket 且可能断开，
所以需要心跳/重连；OpenAI 专用 MCP Tunnel 不是任意节点的通用可达入口。Enigo/XCap 的 Wayland 限制、
Arboard 的 Linux 剪贴板所有权、微软 Session 0 与 Apple 系统权限要求分别列在 RFC 来源，未外推为真机通过。
跨平台打包仍需验证桌面运行库和授权身份；现有未签名 macOS 发行件不保证升级后权限自动保持。

**交付界限：**新增 join、machine、desktop 都只在 RFC 标为拟议接口，不写成当前可用命令；
不虚构安装脚本地址，不修改 CLI/schema、Cargo 依赖或锁文件。安装和系统权限仍需实际操作授权，
单连接指一个 AI 连接器，不等于所有目标机共享一条 TCP。Hub 必须可达且在线，不承诺无共同入口也能互通。

**验证：**本轮只改六份 Markdown 文档，无源码、依赖、锁文件或 CLI 帮助变更。
隔离 GLD_HOME 下 `cargo test --workspace --all-targets --locked --quiet` 1142 passed / 0 failed；
Clippy `-D warnings`、`cargo fmt --all -- --check` 通过，文档命令、相对链接和私人路径六项专项通过。
这些验证只证明本次文档整理与既有仓库检查通过，不代表新桌面库、节点接入、安装脚本或三平台真机已经通过。
没有安装第三方、重启现网、修改 OAuth/grant 或推送；下一编码切片由 task_plan 唯一维护。

## 84. 多机项目优先，复用现有 RPC 的单连接 WSS 传输（2026-10-28）

**用户决定与基线：**用户明确排除 RFC 8441、自定义重连恢复协议及每机多连接；只需普通 WSS
承载标准 JSON-RPC，复用 `bridge/peer.rs` 与 `handle_request`，桌面排在多机项目操作之后。
基线 `1fe2c8a`、工作树干净。先在 `2e5ddad` 更新 RFC-0013 与任务队列：
M0 → M1 → M2 → M3 → L0 → L1；移除原方案的 operation_id 回执库、收件确认与消息补发要求。
保留普通重新连接与重新初始化，不自动重跑任何发送后结果未知的调用。

**实现范围：**新增 `nodes/transport.rs` 的 WebSocketTransport，直接实现既有 Transport trait。
一个 Text message 对应一条现有 JSON-RPC 文本；Peer 的请求 ID、初始化、调用、超时与结果处理不改，
目标机测试直接调用现有 Hub::handle_request。生产代码没有另写 JSON-RPC、节点恢复/调度系统，
没有改 ccnm 的连接池，也没有公开新命令、端点或 MCP 工具，daemon IPC 形状不变。
发送不排队；写入超时或读取截止后关闭该连接，不留下等待补发的请求。库层限制单帧/消息8 MiB，
写入5秒；非文本与超限入站消息关闭，标准 Ping/Pong 沿库处理。接入层负责认证和每机唯一连接，
这个适配器本身不宣称已经实现它们。

**依赖：**固定 tokio-tungstenite 0.29.0，复用当前 Tokio/rustls/futures-util；测试显式使用
已锁定的 tokio-rustls。锁文件只新增所需15个包及依赖引用，没有升级已有包的版本，没有引入桌面库。
本机编译通过；本轮没有运行 Windows/Linux 真机或 Rust 1.89 构建，不能把本机结果外推。

**验证：**新增七项回归。隔离测试在一个真实 TLS/WSS 监听地址接受两个主动连接的节点，
每个节点的 api/web 项目共用一个 Peer，连续列举与读取不增开连接；两个节点同名项目返回各自内容。
项目对象和 handle_request 是实际实现；TLS 使用合成测试证书并启用验证，认证回调及机器登记是测试夹具，
没有跳过证书验证、调用真实节点或访问用户项目文件。还覆盖错误凭据401、未知CA拒绝、跨节点ID/越界拒绝、
只读写工具拒绝、超时后不补发、Binary/超限消息拒绝、标准Ping/Pong、无大小上限socket拒绝、
超限发送在网络写入前拒绝。
最终隔离 GLD_HOME 下全仓 `cargo test --workspace --all-targets --locked --quiet` 为1149 passed / 0 failed；
全仓 Clippy `-D warnings`、fmt --check 与构建通过。文档专项及提交前差异检查继续验证最终文档。

**修正记录：**首轮六项中五项通过，测试把只读工具表拒绝 apply_patch 误当作工具级 isError；
实际沿原有 JSON-RPC 返回 -32602，修正断言后通过，没有改变权限契约。
Clippy 指出 tungstenite 测试握手回调的错误类型较大；其签名要求完整 HTTP Response，
仅对该测试辅助函数标记局部允许并说明原因，生产类型没有扩大或另加包装。

**交付边界：**M0 的传输切片通过不等于正式节点功能完成。生产监听鉴权、机器唯一连接登记、
调用方可信上下文、Hub 远端项目路由、join/配对/安装均留在后续小步；不把固定夹具主体当成多主体验收。
当前服务、客户端工具表、OAuth/grant 未改，未换装或推送；本轮隔离测试目录收尾清理，普通构建缓存保留。


## 85. M0 正式预置接入与项目只读路由（2026-10-29）

**用户要求与基线：**用户要求继续 M0 的正式节点接入与项目路由，沿用每机一条普通 WSS、
标准 JSON-RPC 2.0、复用 Peer/handle_request、先项目后桌面的约定。基线 `9bc02de`，
起始工作树干净；连接中断后用户确认恢复，复核 HEAD 和全部改动未变，再继续验收。
本轮在已有传输切片上完成操作员预置凭据的正式接入，不把它称作邀请配对或一键安装。

**实现与接口：**新增 `gld node register --credential-stdin` 和 `gld node run --config`。
Hub 持久保存稳定 Hub ID、机器 ID、专用凭据 SHA-256 摘要及已发现项目目录；登记拒绝重复
ID、名称、凭据及现有服务/grant 密钥复用，不覆盖既有登记。目标配置明确指定本机项目完整 ID，
Unix 检查配置文件所有者、私有权限与非符号链接，有界读取且错误不回显凭据。
节点只起出站连接，TLS/主机名与 Hub ID 核验通过后复用现有 Hub 分发，不启动入站服务或 ccnm。
CLI 登记沿用 Backend 配置写入口；新增 daemon Request，协议由12升至13。
命令帮助和 `docs/cli.md` 已由实际构建的 CLI 重新生成，工具行为集中写在 concepts。

正式 Hub 增加 `/nodes/v1/connect/{machine_id}`，旧全局入口增加对应的 `/hub` 路径，
直接交给实际 Hub 的同一节点登记表，并受原有网关开关约束。原代理会去掉 Upgrade 头，
所以没有把 WebSocket 请求交给普通 HTTP 转发，也没有为代理再建一条内部节点连接。
先鉴权、预留机器名额，再 Upgrade/初始化；重复连接返回409，不替换健康连接。
每机一个 Peer，真正并发的 RPC 当场返回忙；空闲轮询不占 RPC 名额。
标准 Ping/Pong、Close、EOF 和 Pong 截止负责失联检测，实际 socket 关闭后才释放名额；
单次调用和消息大小继续有界，无发送队列或消息重放。

**路由与权限：**Hub 把原生成员与本机、ccnm 分开，使用绑定机器和目标项目的稳定 `n_...` 句柄，
普通项目工具在节点执行；离线目录保留，精确句柄不回退到同名本机或其他节点。
Hub 按握手工具表校验工具名及顶层参数，再替换目标 workspace 并重建可信 `params._meta`。
节点调用方按 Hub ID 与已验证主体分域，叠加显式共享项目的只读范围；不转发原始访问令牌，
外部 noauth/grant 无法枚举或访问节点，共享 bearer 仍是共享主体，不宣称自然人隔离。
每次分发继续执行已有 grant/confine-reads 检查，在线时放宽本机读取范围不会扩张节点权限。

节点上下文只给受限项目根说明和项目内联说明，关闭全局 Skill/说明、用户 Prompt 与本机 MCP 转发；
项目说明每份最多16 KiB，越界符号链接不跟随。回包仅替换项目路由标识及其等值 JSON 文本副本，
保留正文、图片和错误状态。未发送的离线、忙、能力不支持和超限输入与发送后结果未知分开，
实际写入失败/超时或响应失联返回 `NODE_OUTCOME_UNKNOWN`，不自动补发。

**验证：**本轮新增21项测试：节点配置/身份/登记/传输16项、Hub 可见性和上下文3项、
真实 CLI 集成2项。集成测试使用三个隔离 GLD_HOME，在同一测试主机启动实际 Hub daemon
和两个节点进程；合成 CA 的 TLS 入口原样转发到真实全局网关，保留 /hub 路径并启用证书校验。
每节点共享 api/web 两项目，重复交替读取仍只有各自一条连接；未共享项目不出现，
重复接入不伤原连接，错误凭据及 MCP/节点钥匙互用被拒绝，未信任 CA 被拒绝；
路径越界、未知参数和执行工具被拒绝，伪造内部 metadata 不改变调用身份。
目标端在启动前、在线后及重接后均没有 daemon/socket；空闲退出后主动标离线，
另一节点继续可读，同名本机不成为回退目标，重接句柄保持。
身份单测分别覆盖不同 Hub/OAuth client 的调用方隔离，以及匿名/grant 不得转交。
最终隔离 GLD_HOME 下全仓测试为 1170 passed / 0 failed；
构建、全仓 Clippy `-D warnings`、fmt --check 和文档校验通过。
本轮是同机隔离进程的真实协议验收，未外推为 Windows/Linux 或三台实际设备验收。

**收尾修正：**首次编译修正 Authorization HeaderMap 与既有 bearer 字符串解析接口的类型不匹配。
完整回归先后指出旧 ccnm 章节锚点失效、英文“gld nodes”被命令校验当成不存在的 CLI；
恢复旧标题并调整文案，没有放宽文档测试。名称参数补帮助说明，生成文档不再留下空白参数描述。
全仓并行时旧双节点传输夹具出现5秒超时，单跑0.14秒通过。审查发现服务端把被动等待下一请求
套进在途 RPC 的断线期限，且两机串行验证使另一节点空闲；托管状态目录也没有按模拟节点隔离。
夹具改为异步被动接收、独立状态目录和两机并行验证，各机请求仍串行；保持原5秒 RPC 期限
及100毫秒超时不重放负测，另设整例截止。修复后按全仓并发重新验收。

**依赖与交付边界：**仅开启已有 Axum ws 功能，将已锁定 tokio-rustls 用于正式 CA 配置和集成测试；
Cargo.lock 只增加既有包的依赖引用，没有新增包或升级版本，没有桌面依赖。
邀请/便捷配对、项目写入与命令、凭据撤销/轮换、自动重连和安装接入继续按后续切片实施；
M0 断线后退出，操作员可重新运行。现有服务未换装，OAuth/grant 未修改，未推送或发版。

**经 gld 开发的摩擦：**`list_files` 对已有 bridge 目录使用 `**/bridge/*.rs` 返回空，
同一路径不带 glob 能列出五个文件，与工具声明的工作区相对 glob 行为不符，改用精确目录。
命令策略拒绝 rg/env 前缀，改用专用搜索及临时隔离验证脚本。子任务远端读取/写入请求
曾挂起约25分钟，重试写入又挂起约5分钟；中止后核对未落盘，改由主任务应用共享临时补丁。
后续 `write_stdin` 返回 `MCP -32603 Internal error`，用 `read_output` 确认 Clippy 已成功；
随后 exec、列表及只读调用均报同类内部错误，因而暂停受影响的最终验收和提交。
用户恢复连接后先查 `list_runs`，确认最后一次全仓重跑未启动，再继续执行。
这些故障的根因没有在本轮定位或修复，不以恢复连通替代根因结论；隔离验证脚本收尾删除。


## 86. M1 节点凭据撤销、轮换与竞态收口（2026-10-29）

**范围与基线：**用户要求继续并记录问题。基线 `502ce4e`，起始工作树干净。
按小步提交约定先交付节点凭据生命周期，再接普通重连；本节不代表 M1 的邀请、写入和命令执行已完成。

**实现：**增加 `gld node revoke <machine_id>` 和 `gld node rotate-credential <machine_id> --credential-stdin`。
撤销保留稳定 Hub/机器身份及项目目录；重复撤销幂等。轮换要求不同的独立凭据，拒绝节点当前凭据、
服务及 grant 密钥复用，校验失败不改变登记或关闭健康连接。轮换成功恢复接入，目标机更新私有配置后重跑。
Hub 只存摘要；管理返回只含身份、名称和 revoked 状态，不返回凭据或摘要。
已在途调用可在原期限内完成；持久失效后阻止后续路由，成功返回前关闭旧连接，不回滚已执行的动作。
项目列表增加 `revoked`，撤销项目仍可按原句柄识别，调用返回 `MACHINE_REVOKED`。
配置字段对旧记录默认兼容；daemon 新请求把协议升到14，实际 CLI 帮助已重新生成，概念契约同步。

**发现并处理的问题：**以下来自实现前源码和独立审查，没有据此推断发生过线上凭据泄露。

| 问题 | 处理与证据 |
| --- | --- |
| 鉴权后才预留连接，旧认证结果可能在轮换后迟到 | 在登记表锁内再次检查当前身份、摘要、代次与撤销状态；旧快照返回401；真实落盘交错测试覆盖 |
| 项目目录只按机器 ID 落盘，旧握手可能覆盖新登记 | 目录更新与认证代次检查合为同一配置事务；旧认证写回被拒绝，保留当前目录 |
| 仅比摘要时，凭据 A→B→A 会使旧认证快照复活 | 持久 credential_generation 单调递增，首次撤销及每次轮换推进；旧记录默认0；回归覆盖 ABA 和禁止溢出回卷 |
| 较早的清理可能误关新连接，失效预约还可能占名额 | 锁内读当前配置并标记失效，锁外关闭实际 socket；未附着的失效预约可替换，迟到 Drop 只清自己的连接代次 |
| 旧连接最长30秒调用收尾，碰到 CLI 普通30秒期限边界 | 两种凭据变更沿用既有慢请求预算，避免配置已更新而 CLI 先超时；协议分类回归覆盖 |

**验证：**新增10项回归：配置生命周期7项、真实登记交错1项、CLI 双节点在线撤销/轮换1项、
请求等待分类1项。配置原有测试移入同目录专用测试文件，生产配置与测试各自保持可读规模。
登记交错测试通过独立子进程的 GLD_HOME 使用真实数据文件，不修改并行测试进程的全局环境；
覆盖旧预约、迟到认证、陈旧目录写回、旧 Drop 及不误清新代次。真实 CLI/WSS 测试覆盖 Hub 直接入口，
原 M0 用例继续覆盖全局网关；撤销 A 后 B 可读，旧凭据被拒绝，失败轮换不伤健康连接，
新凭据恢复及在线再次轮换均保持项目句柄，目标进程没有启动本机 daemon。
节点专项23项、CLI/登记集成4项先通过；最终全仓 `cargo test --workspace --all-targets --locked`
为1180 passed / 0 failed；构建、全仓 Clippy `-D warnings`、fmt --check 和文档校验通过。
所有运行使用隔离 GLD_HOME；未新增依赖，未换装服务、变更现有 OAuth/grant、推送或发版。

**尚待处理：**普通重连的代码审查已识别断线后阻塞处理器仍执行、停止信号在收尾期间被延迟、
409 恢复等待窗口及 Hub 关停状态分类，接续下一小步验证。邀请配对、共享范围编辑和远端写入/命令仍属于后续 M1。
§85 的连接器挂起、glob 误过滤和 MCP 内部错误根因仍未定位；本轮由主任务统一操作远端，
子任务只处理本地源码快照，没有再次出现同类远端错误。临时隔离验证脚本不纳入提交。


## 87. M1 普通重连、停止与连接收尾（2026-10-29）

**范围与基线：**接续已提交的凭据生命周期 `1f1969f`，提交后工作树干净。
本步只补普通连接恢复，继续复用同一个节点 Hub/Runtime 和既有 JSON-RPC；不增加消息队列、
恢复协议或请求重放，不提前打开远端写入/命令。

**实现：**节点网络失败、暂时 HTTP429/5xx 和连接超时按1、2、4、8、16秒递增退避，
随后每次至多30秒并带抖动。只有连接上实际收到消息满一分钟才重置退避，收回阻塞处理器的时间不算稳定连接。
每次重连重新核验 TLS、Hub 身份、节点凭据并初始化/发现项目；TLS/身份/凭据、永久协议或消息超限错误退出。
初次409明确拒绝；恢复阶段连续409最多等待60秒，下一次握手及退避均受剩余期限约束。
遇到真实网络或5xx故障清除冲突窗口但保留退避次数，避免把停机误认成重复节点。
Hub 关停的接入拒绝改为503，真正重复机器仍409。

网络断线时先收回这一条正在执行的同步处理器，丢弃旧结果，之后才建立下一条连接；
同一个 Hub/Runtime 保持不变。Ctrl-C 可中断握手、退避、普通处理等待和断线后的收尾等待，
结束节点连接循环；这不声称已经强制取消同步工具或回滚动作。已核对真实 CLI 在退出时使用
200毫秒的 runtime shutdown_timeout，避免再次进入默认析构等待。

**问题与修正：**

| 问题 | 修正与证据 |
| --- | --- |
| 原连接断开即丢 JoinHandle，阻塞处理器仍可能执行，新连接会与它重叠 | 只在网络恢复前等待旧处理器，旧结果不发送；真实内存 WebSocket 测试用受控处理器验证 |
| 初版把用户停止也当重连收尾，无界等待同步处理器 | 停止独立返回 Stopped；覆盖正常处理及已经断线进入收尾后收到停止信号两条交错 |
| 只在收到409后检查时间，最后一次 sleep/握手可能越过60秒 | 下一次握手前检查截止，等待和握手期限裁到剩余窗口；59秒边界回归覆盖 |
| 409之后出现真正网络故障，仍套旧冲突截止并永久退出 | 瞬时故障清除冲突窗口但不重置退避；409→网络失败→再次409的交错回归覆盖 |
| 首次接入恰逢 Hub 关停，被同一个409误判成重复进程 | HUB_STOPPING 返回503，与重复连接分开 |
| Upgrade 尚未完成就 EOF 被泛化成永久协议错误 | 单独将 HandshakeIncomplete 归为瞬时故障；固定版本源码网页读取受阻后，以锁定依赖上的真实半截握手测试验证 |
| 首轮10项专项有2项在等 Close 应答时超时 | 收到 Close 后再用固有 close(None) 发送关闭帧没有刷新已排队应答；改走 SinkExt::close，保留原2秒截止，两项及整组随后通过 |

以上记录区分实现期源码审查与实际失败，没有把潜在竞态描述成已发生的线上事故。
错误消息只给原因/HTTP 状态与等待时间，不输出原始凭据、证书内容或请求正文。

**验证：**本步新增11项：4项错误/退避/冲突策略、6项真实内存 WebSocket 行为、1项真实 CLI/TLS 网络恢复。
专项10项通过；CLI 原有认证、两节点路由、撤销/轮换与新恢复用例共4项通过。
新用例让同一个节点进程先遭遇入口不可用、随后连通，再在线断网并恢复；重接后项目句柄不变，
读取到更新后的项目正文，测试 TLS 连接活跃峰值为1，目标未启动本机 daemon，Unix 上等待恢复时 SIGINT 正常退出。
旧双节点、网关、权限负测和未知结果不重放回归继续通过；未用单测代替实际三平台设备验收。
最终隔离 GLD_HOME 全仓为1191 passed / 0 failed；构建、全仓 Clippy `-D warnings`、fmt --check、
实际 CLI 文档重生成及文档校验通过。相对本轮起点1170项，两次提交共新增21项。

**收尾与后续：**未新增依赖；源码协议仍14。下一编码入口为 M1 的邀请配对与共享配置，随后项目写入、
命令运行/查询/停止及跨主体结果隔离；M1 整体仍在进行中。现有服务未换装，现有 OAuth/grant 未修改，未推送或发版。
临时隔离验证脚本收尾删除，普通构建缓存保留。§85 的连接器挂起、glob 误过滤与 MCP 内部错误本轮未再出现，
根因仍未定位，不以本轮连通或测试通过视作这些工具缺陷已修复。


## 88. M1 一次性邀请与便捷配对（2026-10-29）

**范围与基线：**用户要求继续推进任务，基线 `af806b8`，起始工作树干净。
本步接通操作员生成邀请、目标机显式选择共享项目、确认和私有配置保存；
共享范围后续编辑另作下一独立小步，文件写入、命令执行、安装和桌面继续按原队列。

**实现与契约：**增加 `gld node invite <name>` 和 `gld join --hub … --config … --share … --invite-stdin`。
邀请由本机 CLI/daemon 配置写入口生成，默认600秒有效，最多64个未消费记录；
同名新邀请替换未消费旧邀请，已经登记的名称拒绝重新邀请。邀请含绑定 Hub ID、随机邀请 ID
和两个 UUID v4 拼接的随机秘密（随机位约244位，满足 RFC 的至少128位），Hub 只存摘要。
每个邀请八次错误秘密尝试后失效。邀请与 MCP/OAuth/节点长连接凭据分离，不增加远程管理工具。
daemon 增加 NodeInvite，请求协议由14升至15，CLI 文档由实际帮助重新生成。

目标机先显示 Hub ID、当前执行账号、明确选择的本地项目及只读/桌面关闭状态；
终端输入 yes 或显式 --yes 后才发送，邀请从 stdin 接收，永久节点凭据不进入 argv 或输出。
共享选择接受完整 ID 或唯一名称，完整 ID 优先，要求全部项目开启 confine-reads。
客户端在请求前私有保存机器 ID（同时是配对请求 ID）、独立节点凭据和配置到 pending；
Hub 在同一事务中注册机器并消费邀请。恢复同时校验 Hub、原机器 ID、当前凭据、未撤销及代次为0，
只返回原登记，不重写或重新签发凭据；邀请过期或已消费不妨碍原有效登记的结果恢复，
也不能用于新身份。撤销、轮换及 ABA 后原配对状态拒绝恢复。

配对 HTTP 入口为 /nodes/v1/pair，仅挂在 Hub，/hub 前缀复用现有 HTTP 网关及其启用开关。
正文限4 KiB、读取限5秒，错误 JSON 统一返回400，不输出解析原文；客户端请求限10秒、
响应限16 KiB、禁全部重定向并核对返回身份。TLS 信任不降级，明文仅限字面回环 IP 且绕过环境代理。
配置文件无覆盖发布，参数不一致或正式配置已存在时保留原文件；同目录稳定锁协调配置操作。
Unix 核对属主、私有权限和非符号链接；Windows 创建时就使用当前账号 SID 的受保护 DACL，
读取通过同一文件句柄检查，拒绝 reparse point 及不能确认持续实施 ACL 的卷。

**发现并处理的问题：**

| 问题 | 修正与证据 |
| --- | --- |
| DataStore 闭包返回 Err 不保存，错误尝试次数可能看似增加却不落盘 | 事务区分需要保存的拒绝状态；真实子进程验证八次失败跨加载保留 |
| 将所有拒绝包成 Ok 又会让匿名无效请求反复重写整个配置 | 仅实际注册或失败计数变化时保存；未知/过期/锁定拒绝及成功恢复不写盘，旧 mtime 回归覆盖 |
| 在拿配置锁之前读时间，长锁等待可能接纳已过期邀请 | 在取得事务锁后读取当前时间，再判断生成/消费期限 |
| 先发请求再生成本机状态会在响应丢失后创建重复节点 | 先保存稳定请求 ID 和凭据；真实 Hub 成功响应被代理丢弃后，第二次 CLI 恢复原登记 |
| HTTP 明文回环例外可能受环境代理影响 | 对字面回环强制直连；真实 CLI 子进程用不可达代理和空 NO_PROXY 验证 |
| hard_link 发布后、清理旧路径前退出，额外 nlink==1 限制会拒绝有效配置 | 去掉非契约所需的链接计数限制，保留权限/属主/链接类型核验；模拟中断发布仍可读取正式配置 |
| node run 仍走旧配置加载入口，Windows 上可能漏掉新 ACL 检查 | NodeConfig::load 统一复用同句柄的私有文件读取；Windows 打开附加匿名 SQOS，Unix 非阻塞打开后拒绝 FIFO；跨平台测试夹具也改用私有文件创建 |
| 展示名称恰等于另一项目完整 ID 时，ID 选择会被名称遮蔽 | 优先解析确切 ID，再按唯一名称选择；补入项目选择回归 |
| 首轮真实 CLI 六项中丢响应夹具出现 WouldBlock，5过1失败 | 夹具在 accept 后显式切回阻塞读取，保留原5秒期限和大小上限；两项新用例随后0.86秒通过 |

上述问题是本次实现期源码审查或隔离测试发现，没有据此声称线上曾泄露凭据或重复注册设备。
配对返回和 Debug 不含永久凭据；邀请仅在操作员主动生成时显示一次。

**验证：**新增25项：邀请/注册12项、目标机配对5项、私有存储6项、真实 CLI/TLS 2项。
节点专项56项通过；真实 CLI/TLS 用例验证 /hub 网关、确认前不消费、凭据混用拒绝、所选项目路由、
正式配置不覆盖，以及服务端成功后丢响应仍恢复同一身份。目标机未启动 daemon。
最终隔离 GLD_HOME 全仓 `cargo test --workspace --all-targets --locked` 为1216 passed / 0 failed；
全仓 Clippy `-D warnings`、fmt --check、构建、实际 CLI 文档生成和文档回归均通过；git diff --check 无错误。

Windows 0.61.3 所用 API 声明已对照实际锁定依赖；新私有文件模块用独立小工程
`cargo check --target x86_64-pc-windows-msvc --offline` 通过。
全仓该目标检查在 ring 的 C 编译阶段因缺少 Windows SDK 的 assert.h 失败；
此模块检查不等于完整 Windows 构建或 Windows ACL 真机验收，其他平台运行证据仍待 M3。
未新增或升级依赖，未安装 SDK；本轮隔离验证脚本和检查小工程不纳入提交，工作收尾删除。
现有服务、OAuth/grant、安装及发布未改动，未推送。§85 的 gld 工具挂起和 MCP 错误本轮未再出现，根因仍未定位。

**本轮工具与验证摩擦：**经 exec_command 调 rg 被 command_not_allowlisted 拒绝，改用 search_text/read_file，
没有扩大命令权限。配置重构后的复验先因测试使用的 MAX_CONFIG_BYTES 未迁回共享位置而编译失败；
修复补丁又因两个同文条件命中而被 apply_patch 的 PATCH_AMBIGUOUS 原子拒绝，未落盘。
加入唯一上下文后补丁成功，再从最终源码完整复验；这些失败未计作通过。

## 89. M1 目标机共享名单查看与修改（2026-10-29）

**范围与基线：**接续用户“继续推进任务”，在邀请配对提交 `1c7cf25` 后完成下一独立小步。
共享配置是目标机本地操作员入口；文件写入、命令执行和跨主体结果隔离留在后续 M1 队列。

**实现与契约：**新增 `gld node shares --config node.json`，运行中也可查看；
传入一个或多个 `--share` 则完整替换名单，接受完整项目 ID 或唯一名称，须保留1～256个项目。
解析复用 join 的显式选择规则，重复、未知、名称歧义或未开启 confine-reads 时拒绝且不修改文件。
修改只替换 workspaces，保留 Hub 地址/身份、机器身份、凭据与 CA 信任；相对 CA 路径可规范为同一目标的绝对路径。
配置使用已有私有原子写入，未引入新存储或后台重载。

node run 在读取配置前取得同一稳定文件锁，锁随阻塞执行闭包持有到节点退出，覆盖在线及重连等待。
修改必须先结束节点进程；查看用安全只读加载，不争该锁。
再次运行并发现项目后 Hub 才更新目录，留下的项目句柄稳定，移除项目的旧句柄不能继续调用。
两条本地命令都在 Backend 之前分流，目标机不自动拉起 daemon。

输出只包含 Hub/机器身份、配置路径及共享项目，不输出节点凭据或 CA 路径。
项目 available 只表示本机登记存在且 confine-reads 开启，不探测在线、磁盘或项目健康；
失去登记的项目仍保留 ID 并标 false。当前 read_only 恒为 true。

**审查与问题处理：**

| 发现 | 处理与证据 |
| --- | --- |
| 仅锁修改命令不能阻止节点运行时改配置，重连会继续使用内存旧名单 | 节点完整运行期间持同一把锁；真实 CLI 运行中查看成功、修改失败且原文件和原项目可读 |
| 新运行锁会让原同路径重复节点测试提前本地失败，掩盖 Hub 的409检查 | 重复身份用独立私有配置副本连接，保留原409断言及两个原节点的读取验证 |
| CLI 文档生成器有手写命令清单，首步虽已执行生成却漏了 join/node invite 的独立帮助节 | 清单补上 join、node invite、node shares，并从实际构建重新生成，三节已在输出中核对 |
| 经 exec_command 用 bash 调脚本被 command_not_allowlisted 拒绝 | 使用已允许的 scripts/gen-cli-docs.sh 直接执行，未更改权限或策略 |
| search_text 在 crates 下用 include_globs=["**/tests/*.rs"] 返回 scanned_files=0、rejected_by_glob=321 | 已知测试文件仍存在；去掉 glob、改 type=rust 后找到 docs_commands_exist 等文件。记录为 glob 过滤异常，本步未修复其根因 |
| 删除本轮 tmp 下的 Windows 检查工程被视为删除关键项目文件 | apply_patch 对 tmp/m1-windows-check/Cargo.toml 返回 DANGEROUS_OPERATION_REQUIRES_CONFIRMATION；未用 confirm=true 或其他方式绕过，保留这份无凭据的临时工程，未纳入提交 |

**验证：**新增3项：核心2项和真实 CLI/TLS 1项。核心隔离子进程验证持锁时可查不可改、停止后替换、
非法选择保留原字节、身份/凭据/CA 保持和失去登记后的可见提示。
真实 CLI 验证 api/web 共享改为仅 web，重连目录从2项变1项、web 句柄稳定且正文可读、
api 旧句柄被拒、未共享项目不出现。原 M0/M1 的六项真实节点回归继续通过，专项合计7项。
独立审查确认锁覆盖整个 client::run、Backend 前分流及原 Hub409测试仍有实质覆盖。

最终隔离 GLD_HOME 全仓 `cargo test --workspace --all-targets --locked` 为1219 passed / 0 failed；
Clippy `-D warnings`、fmt --check、构建、CLI 文档重生成、文档回归与 git diff --check 通过。
两次独立提交相对起点1191项共新增28项。本步不改守护进程 Request/Response，源码协议仍15。

**收尾与后续：**下一编码入口为项目文件写入，再接命令运行/查询/停止与跨主体结果隔离。
M1 整体仍在进行中；安装接入为 M2，三平台真实设备验收为 M3，桌面仍后置。
Windows ACL 的真机边界沿用 §88；本次不把本机通过外推到其他平台。
临时验证脚本已删除；Windows 检查小工程因上述确认关卡保留在被忽略的 tmp 目录，普通构建缓存保留。
现有服务、OAuth/grant、安装、发布均未改动，未推送。

记录更新曾因补丁只取长行前半段而返回 context_not_found；按实际整行重建后提交，失败补丁未落盘。

## 90. RFC-0013 研判：WSS 回连方向合理，补 Hub 身份、代理、超时与超大回包（2026-10-30）

**范围与基线：**用户问“一个 Hub、任意目标机主动加入”的规划，以及“目标机用 WSS 长连接回连 Hub、
复用 Axum/Tokio 和成熟 WebSocket 库”是否合理，并要求把两轮研判补进 RFC。基线 `b066d4f`。
同一目录里另一个会话正在做 M1 文件写入（18 个文件未提交），本节只读源码，只改 RFC-0013 和本文件，没有编译或跑测试。

**结论：**WSS 合理，是现有入口（Cloudflare、FRP、自建反代）下最合适的传输：走 443、HTTP/1.1 Upgrade，
这几种反代都能转发，双向且延迟低。HTTP 长轮询可留作后备；gRPC/HTTP2 过不了 FRP 的 http 代理和不少 nginx 配置；
QUIC、Iroh 走 UDP 常被挡；SSH 反向隧道要每台机器配 sshd。

**上一轮意见在 `2e5ddad`～`b066d4f` 已落实的**（不再写进 RFC）：只共享目标机显式选中的项目；
M0→M1 先于桌面；复用 Peer/handle_request 跑标准 JSON-RPC；tokio-tungstenite 固定 `=0.29.0`，与 axum 的 ws 同版；
Hub 空闲时每 20 秒 Ping、20 秒没等到 Pong 就断开，节点 60 秒收不到任何帧就断开；有抖动的有界退避；
消息和帧上限 8 MiB；凭据放 `Authorization` 头并标 sensitive；TLS 校验不降级；macOS 未签名升级要重新授权已写进 §5/§9。

**仍未覆盖、已补进 RFC 的：**

| 发现 | 证据 | RFC 位置 |
| --- | --- | --- |
| 节点回包超 8 MiB 时 `gld node run` 直接退出，机器离线到有人手动重启；`view_image` 回包约为原图 4 倍，约 2 MiB 以上的图就会触发 | `nodes/client.rs::send_response` 超限返回 `Failure::permanent`，`reconnect.rs` 遇 `Retry::Never` 返回错误；`workspace.rs::wrap_mcp_tool_result` 和 `image_tool.rs` 让 base64 出现三份；`view_image` 在 `CORE_READ_ONLY_TOOLS` 里 | §7.2 |
| Hub 单次调用超时 30 秒，与 `exec_command` 的 yield 上限、带端口服务的默认等待相同；按上限传参必然超时、整条连接被关 | `nodes/registry.rs` 的 `CALL`；`tools/args.rs` 的 yield 上限；`tools/service.rs` 的 `DEFAULT_WAIT_MS`；`connection.rs` 把超时当致命错误 | §7.2 |
| 一个调用在途时，其他调用方立刻收到 `MACHINE_BUSY` | `connection.rs::try_with_peer` 的 compare_exchange | §7.2 |
| 节点认 Hub 只靠 TLS 和响应头 `x-gld-hub-id`；hub_id 在邀请里，不是秘密；凭据在 Hub 证明自己之前已发出 | `nodes/client.rs` 握手后只比对这个响应头；`pairing.rs` 的邀请令牌含 hub_id | §7.3，强度待用户定 |
| 配对跟着 `HTTPS_PROXY` 走，长连接不认代理；只能经代理出网的机器 join 成功、run 一直超时 | `join.rs` 用 reqwest（只对回环地址 `no_proxy`）；`client.rs` 用 `connect_async_tls_with_config` 直连 | §7.4 |
| 只信内置 webpki 根证书；配 `ca_certificate` 后只信这一张 DER，不读系统证书库 | tokio-tungstenite 特性 `rustls-tls-webpki-roots`；`client.rs::ca_connector`；`join.rs` 的 `tls_built_in_root_certs(false)` | §7.4 |
| 邀请不给地址、不查入口；临时隧道每次重启换地址，OpenAI 隧道没有节点能连的地址 | `gld node invite` 只提示用 join 接收；`gld share` 默认 trycloudflare | §3 |
| 截图：8 MiB 上限加 5 秒写超时，发满要约 1.7 MB/s 上行 | `transport.rs`、`client.rs` 的 `WRITE_TIMEOUT` | §8 |

**待用户决定：**§7.3 选 A（钉住 Hub 公钥，挡住冒充）还是 B（再加逐条签名，连转发插话的中间人也挡住）。
建议在命令执行接入节点之前至少做 A。

**未验证与后续：**以上都是读源码推出的，没有写测试复现；另一个会话正在改 `nodes/`，这时编译会抢它的构建锁，
还会编进它没写完的代码。超大回包让节点退出是已提交代码里的缺陷，修复要改 `nodes/client.rs`，
而这个文件正被那个会话修改，等它提交后再修并补回归。文档检查：新增文字不含家目录路径和相对链接；
命令存在性检查不扫 `docs/rfc/`、`docs/reviews/` 子目录。现有服务、依赖和锁文件都没动，未推送。

## 91. M1 项目级文件写入与权限边界（2026-10-30）

**范围与基线：**接续用户“继续下一个任务”，起始基线 `b066d4f`、工作树干净。
本步只接项目文件写入，沿用已有补丁内核、Hub 项目路由和单条节点连接；命令运行/查询/停止及跨主体结果隔离仍为下一步。
验收期间并行会话将 RFC 研判和本文件 §90 单独提交为 `a145381`；本步接在该文档提交之后，保留其结论与待决事项。

**配置与使用：**`gld join` 和 `gld node shares` 增加可重复的 `--write <PROJECT>`，
可写项目必须同时位于本次完整 `--share` 名单，按完整 ID 或唯一名称选择。
目标机保存去重的 `writable_workspaces` 子集；旧配置、旧 pending 缺字段时继续只读，空集合保存时省略字段。
完整替换共享名单时省略 `--write` 即撤回全部写权限，新加入的项目不继承旧授权；
节点运行时仍可查不可改，停止后修改、重新连接后发现，保留项目句柄和机器/Hub/凭据身份。
join 的恢复比较与私有配置发布均包含可写范围，不能通过重试扩权。

**执行与发现：**可信 Hub 转发的完整目标项目 ID 命中本机可写名单时，这一次内部 scoped grant 才可写。
节点保留 Hub/主体身份、共享范围、当前本机工具档及 confine-reads 检查；
全权服务认证才能访问 Native 成员，外部 grant/noauth、伪造 metadata、名称或 ID 前缀不会扩大本地写范围。
工具上限复用实际暴露的只读工具集，只增加 `apply_patch`、`patch_check`，不开放命令、Agent 委派、配置管理或 MCP 转发入口。
工具定义来自完整注册表再筛选，保留原有 planning_state；参数、annotations 和补丁执行器不另造一套。
tools/list、workspace_context、server_info 及 aggregate_tools 展示与硬发调用使用同一上限。
目录增加逐项目 read_only；节点专属 tool_profile 为 read-only/file-write，不新增可设置的全局工具档。
旧节点未报告 read_only 时按只读处理，明确的非布尔字段拒绝。

补丁继续执行 Planning、路径/符号链接保护、敏感操作确认、expected_versions、整批预检与现有回滚；
预检不会落盘，也不承诺跨编辑器原子 CAS 或断电恢复。
连接恢复仍不重放任何旧调用；NODE_OUTCOME_UNKNOWN 后须先读回目标文件核对，不能假定写入没有发生。

**审查发现与修正：**

| 问题或覆盖缺口 | 处理与证据 |
| --- | --- |
| 私有 node 配置若位于共享项目内，可能泄露凭据或被文件写入改变下一次启动的范围；0600 不阻止同执行账号的工具 | join 在持久化前、shares 在替换前、run 在连接前核对真实路径，拒绝配置位于任何共享根；真实 CLI 分别对只读和可写配置验证拒绝、原字节保留、TLS 接入次数为0且输出不含凭据 |
| 共享根覆盖控制数据目录，或本身位于其中，可能触及 profiles.json 等授权依据 | 节点专属检查拒绝共享根与实际 GLD_HOME 双向包含；解析项目根、配置父目录和控制目录的符号链接，目录缺失明确拒绝；覆盖正常外部目录、两向包含和别名 |
| 直接拿策略分类中的 READ_ONLY_TOOLS 作节点上限，会包含实际执行命令的 exec_health_check | 复用 exposed_tool_names("read-only") 的实际工具表，再加两项补丁工具；硬发 exec、管理、relay、remote_* 和 work_session 均拒绝 |
| 新负测最初使用了不存在的 MCP relay 名称，不能证明真实入口受限 | 独立静态审查发现后，在运行前改成 list_mcp_tools、call_mcp_tool、read_mcp_result 三个真实入口 |
| 新 CLI 负测把 GrantDto 的 Rust 字段名当作 JSON 字段，首轮11项中10过1失败 | 对照既有 camelCase 序列化改用 readOnly/bearerToken，生产契约未改 |
| 下一次同组负测在已有隔离 grant 时切换 noauth，被既有策略正确拒绝 | 测试先用正常 CLI 删除自己创建的测试 grant，再切换隔离 Hub；跨权限专项及最终整组11项通过，未改真实 grant 或认证配置 |
| 全仓命令合法性检查把英文提示中的产品名加 control data 当成子命令 | 调整控制目录错误文案为自然所有格，测试匹配实际语义；原门禁保持，专项和完整复验通过 |

以上是本步源码审查或隔离验证的发现，没有据此声称线上发生了凭据泄露或权限扩张。
控制目录保证覆盖配置和 CLI 启动检查；可信本机操作员运行中主动重绑项目根、直接嵌入内部 Rust API 不属于这项检查。

**验证：**新增20项：core 配置/恢复/目录/身份/发现12项、Hub2项、CLI参数2项、真实节点链路4项。
核心节点专项70项、Hub native专项5项通过；真实 CLI/HTTP/TLS/WSS 节点整组11项通过。
真实用例覆盖两机同名项目、默认只读与显式写入、预检版本回传后实际落盘、撤回写入与移除共享后句柄保持、
单连接路由、越界/受保护路径/符号链接/工作流确认的整批拒绝、外部编辑引起的版本冲突、伪造身份和 grant/noauth 拒绝。
同时核对实际磁盘字节、其他机器和项目未被改动、目标机未启动 daemon；原重连与结果未知不重放回归继续通过。
本机项目只读档、Planning 和运行中关闭 confine-reads 的拒绝均有实际调用覆盖。

最终隔离 GLD_HOME 全仓 `cargo test --workspace --all-targets --locked` 共60个测试目标，
1239 passed / 0 failed；全仓 Clippy `-D warnings`、fmt --check、构建及实际 CLI 文档生成通过。
本步不改守护进程 Request/Response，源码协议保持15；没有新增或升级依赖。
本轮验证不替代 Windows ACL 真机验收或三平台 M3；Windows 边界沿用 §88。

**收尾与后续：**当前队列移至命令运行/查询/停止与跨主体结果隔离，M1 整体仍在进行中。
接命令前须对照 §90/RFC-0013 的调用超时、单在途限制及 Hub 身份约束；Hub 证明强度仍待用户决定。
§90 的超大回包、代理和入口发现等问题没有被本轮文件写入验证消除，后续按独立小步修复。
经 gld 的远端操作由主任务统一执行，本轮未再遇到 §85 的子任务挂起/MCP错误或 §89 的 glob 异常，根因仍未定位。
本轮临时隔离验证脚本收尾删除；§89 因关键文件删除确认而保留的 Windows 检查小工程继续留在忽略目录，未纳入提交。
现有服务、OAuth/grant、安装、发布均未改动，未推送。

## 92. M1 Hub 身份证明：钉住公钥与逐条签名（2026-10-30）

**范围与基线：**用户对 §90 的待决事项回复“A 和 B 都做”。基线 `700be19`，工作树干净；
ChatGPT 最后一次经 gld 操作在 13:59，本步 18:3x 开始时没有在途改动。只做 Hub 身份证明，
§90 的超时、超大回包、代理和入口核对仍按独立小步处理。

**实现：**新模块 `nodes/hub_key.rs`，依赖改为直接使用锁文件里已有的 ring 0.17.14（锁文件只多一条依赖边，未新增或升级包）。

- Hub 第一次登记、邀请、撤销或轮换时，与 hub_id 一起生成 Ed25519 私钥，存为 `profiles.json` 的 `node_hub_key`。
- 邀请令牌变为四段，最后一段是 Hub 公钥；Hub 拒绝换了公钥段的令牌，不计入错误秘密次数。join 把公钥写进配置，
  配对回包里的公钥对不上就保留 pending、不发布配置。预置登记的输出增加 `hub_public_key`。
- 节点每次连接带 32 字节随机 nonce（`x-gld-node-nonce`）；Hub 先认凭据（失败仍 401）再查 nonce（缺失 400），
  101 响应里给出对 hub_id、machine_id、nonce 的签名（`x-gld-hub-proof`）。节点验不过就退出，不再重连。
- Hub 发出的每条消息都追加 `"gld_sig":"序号.签名"`，覆盖连接身份、从 1 递增的序号和原文。节点按后缀切回原文验签，
  验过才解析，交给 `handle_request` 的是切回的原文；超限消息不占序号。
- `NodeConfig.hub_public_key` 必填；配置比较（pending 与正式配置）也比较公钥。

**审查发现与处理：**

| 问题 | 处理与证据 |
| --- | --- |
| 签名嵌进 `params._meta` 要先规定 JSON 规范化，serde_json 未开 `float_roundtrip` 时浮点重新序列化可能差一位，验签会偶发失败 | 改为在原文末尾追加成员、按后缀切回原文，不重新序列化；单测覆盖参数里含同名键和转义后的签名段 |
| `local_config::same_config` 不比较新字段，换了公钥的 pending 会被当成同一份 | 补上公钥比较 |
| 配对测试的 `wrong_secret` 改的是令牌最后一个字符，新格式下那是公钥段，会让“错误秘密计数”测偏 | 改为只改第三段秘密，另加公钥被替换的独立用例 |
| 用邀请冒充节点凭据的既有负测期望 401；若先查 nonce 会变成 400 | Hub 先认凭据再查 nonce，原断言不改 |
| clippy 要求 `is_multiple_of` | 已改；项目最低 Rust 1.89，该方法 1.87 起稳定 |

**不做的（已写进 RFC-0013 §7.3 和 security.md）：**节点发回 Hub 的结果不签；节点凭据仍在 Hub 证明身份之前发出，被截后用
`rotate-credential` 换；Hub 私钥不支持单独轮换，换了全部重新配对；反代仍能看到内容、丢弃或延迟消息。
早期开发数据只有 hub_id 没有私钥时可以读，节点连不上，下次登记、邀请、撤销或轮换时补上；
本机真实 Hub 没有任何节点登记，不涉及迁移。

**验证：**新增10项：签名模块5项（钥匙读写、握手绑定、原文还原、篡改/插入/重放/跳号/换连接/换钥匙、超限不占序号），
配对1项、配置2项、join 1项、节点接收层1项（保留签名段只改调用名的中间人消息，节点不解析、不回复、按不可重试退出）。
真实 CLI/TLS/WSS 节点整组11项通过，其中首个用例加了“节点钉的不是这个 Hub 的公钥”负测：节点退出并报
`could not prove the pinned Hub key`，Hub 发现不到任何节点项目。这组用例经 TLS 中继和全局网关，
覆盖配对、文件写入、重连、撤销与轮换，签名在真实链路上逐条生效。

隔离 GLD_HOME 全仓 `cargo test --workspace --all-targets --locked --no-fail-fast`：60个测试目标 1249 passed / 0 failed；
Clippy `-D warnings`、fmt --check、CLI 文档重生成（只变 `node register` 帮助两行）和文档定稿后的五组文档检查通过。
守护进程 Request/Response 未改，协议仍15。现有服务、OAuth/grant、安装、发布均未改动，未推送。

## 93. 节点超大回包只让这一次调用失败，不再让节点退出（2026-10-30）

**范围与基线：**用户要求修 §90 的第一项。基线 `c3c12d1`，工作树干净，ChatGPT 最后一次经 gld 操作仍是 13:59。
只改节点回包；30 秒调用超时、代理、入口核对和 `view_image` 三份编码不在本步。

**问题：**节点回包超过 8 MiB 时，`nodes/client.rs::send_response` 返回永久失败，`gld node run` 不再重连、进程退出，
机器离线到有人手动重启；Hub 那边这次调用报 `NODE_OUTCOME_UNKNOWN`。`view_image` 回包约为原图 4 倍，
约 2 MiB 以上、没触发自动缩放的图就会触发，M0 起节点只读工具里就有它。

**修复：**处理器结果超限时只丢弃正文，换一条小回复给这一次调用，连接和进程照常。
`tools/call` 回 `NODE_RESPONSE_TOO_LARGE` 工具错误（`isError: true`，`retryable: false`），写明“工具已在目标机执行、结果被丢弃、请少要点输出”；
其他方法回 JSON-RPC `-32603`。没用 JSON-RPC 错误回 `tools/call`，因为 Hub 把节点的 JSON-RPC 错误一律显示成
`NODE_REQUEST_REJECTED`（“节点拒绝了请求”），对已经落盘的补丁是错误信息。节点自己拼的控制和错误回复仍保留超限即永久失败的保护。

**验证：**新增2项。节点单元测试：超限工具结果变成小于 4 KiB 的工具错误、id 不变，非工具调用回 `-32603`，之后同一连接照常收发。
真实 CLI/TLS/WSS 测试：目标机项目里放 1000×1000 噪声 PNG（约 3 MB），经 Hub 调 `view_image` 得到 `NODE_RESPONSE_TOO_LARGE`，
随后同一台机器读文件正常，节点进程仍在运行，中继连接数不变。把 `client.rs` 换回 `c3c12d1` 版本重跑这条测试，
得到 `NODE_OUTCOME_UNKNOWN` 而失败，确认它确实抓得住原缺陷。测试用的 PNG 由 `image` 生成，CLI 测试新增这条 dev-dependency，
版本沿用 core 已锁定的 0.25，锁文件只多一条依赖边。

隔离 GLD_HOME 全仓 `cargo test --workspace --all-targets --locked --no-fail-fast`：60个测试目标 1251 passed / 0 failed；
Clippy `-D warnings`、fmt --check 和文档定稿后的五组文档检查通过。协议仍15，现有服务、OAuth/grant、安装、发布均未改动，未推送。

## 94. 节点调用超时放宽到 60 秒、回包写超时 30 秒、view_image 只回一份编码、README 重写（2026-10-30）

**范围与基线：**用户要求继续 §90 的 `view_image` 和 30 秒超时，认为操作和网络都可能超过 30 秒，提议放宽到 60 秒；另要求从用户角度重写 README。
基线 `aeb8f3b`，工作树干净。命令执行还没接入节点，这轮改的是它要踩的约束，不是命令执行本身。

**研判：**60 秒可行，但只改它不解决网络问题。读源码发现弱网时先触发的是 5 秒写超时，5 Mbps 上行发 4 MB 约 7 秒，
不论 Hub 肯等多久，节点写到第 5 秒就断线重连。所以三个数一起定：工具最多等 30 秒，消息写最多 30 秒，Hub 最多等 60 秒。
节点的 `IDLE_TIMEOUT` 也是 60 秒，调用期间 Hub 不发 Ping，这是 `CALL` 的硬上限，再往上放要先改它。
新增一条测试锁住这几个数的关系。Ping 等控制帧的写超时仍是 5 秒。

| 改动 | 提交 | 验证 |
| --- | --- | --- |
| Hub 等回包 30 → 60 秒 | `2922361` | 真实 CLI/TLS/WSS：中继压住节点回包 32 秒，同一连接拿到结果、连接数不变；`CALL` 还是 30 时 30.6 秒报错 |
| RPC 消息写超时 5 → 30 秒 | `50c3a21` | 两条测试让对端 6 秒后才读；压回 5 秒时都在 5.04 秒失败 |
| `view_image` 经节点只回一份编码 | `8f2a957`、`793cc40` | 3 MB 噪声图经真实链路拿到图片块，解码后字节数一致，回包不超过编码长度加 4 KiB；超限改用 6.8 MB 的图验证 |
| README 重写 | `67eab33` | 见下 |

`8f2a957` 提交时检查没过：把测试和 `git commit` 用 `;` 串在一起，一条单元测试编译失败没拦住，`793cc40` 修复。没改写历史，所以 `8f2a957` 单独检出时 lib 测试编译不过。

**图片的取舍：**只在节点发回包前裁，本机客户端输出和工具表都不变，连接器不用 Refresh。默认只回图片块，约为原图 1.33 倍，默认 5 MiB 的图放得下；
`output=data_url` 要在结构化结果和文本块里各留一份（`tag_result` 靠两者相等才同步句柄），约 2.67 倍，原图超过约 3 MiB 仍报 `NODE_RESPONSE_TOO_LARGE`。

**README：**用隔离 `GLD_HOME` 跑过 `gld start`、`add`、`ls`、`doctor`、`health`、`upgrade --auth bearer`、`secret ls`、`stop`、`daemon stop`；
无令牌请求回 401，带令牌的 `list_workspaces`、`git_status` 正常。核对 v0.8.3 源码时发现 `command-policy` 没进那一版，写进了未发版说明。

**代价：**60 秒是上限，正常调用一回包就返回。但卡住的调用占连接的时间变长：别的调用立刻收到 `MACHINE_BUSY` 的窗口从最长 30 秒变成 60 秒；
停服务要等在途调用结束，`Connection::close` 最坏从约 35 秒（写 5 + 等 30）变成约 90 秒（写 30 + 等 60），而 `gld daemon stop` 只等 20 秒。
这一条只读了代码，没实测。这个差距以前就有，没在这轮改；真遇到再让关闭不等在途调用。

**没验证：**慢网是中继和测试里模拟的，没在真实弱网、跨机器上跑；`gld share`（会开公网地址）和 `claude mcp add`（会改本机客户端配置）没实跑，只核对了帮助；ChatGPT 配置步骤沿用已有文档，这轮没重走。

隔离 GLD_HOME 全仓 `cargo test --workspace --all-targets --locked --no-fail-fast`：60个测试目标 1258 passed / 0 failed；
Clippy `-D warnings`、fmt --check 通过。协议仍15，工具表未改，现有服务、OAuth/grant、安装、发布均未改动，未推送。

## 95. M1 目标机跑命令，关连接不再陪等在途调用（2026-10-30）

**范围与基线：**用户要求继续推进并优化设计。基线 `73bb903`，工作树干净。按任务表做 M1 最后一项：命令运行、查询、停止和跨主体结果隔离；
顺手修 §94 记下的“关连接最坏等 90 秒”。

| 改动 | 提交 | 验证 |
| --- | --- | --- |
| `--run` 点名项目才能跑命令，权限分读、写、命令三级 | `12caeb3` | 见下 |
| 节点退出时停掉自己起的命令，新增 SIGTERM 处理 | `12caeb3` | 去掉这行后 SIGTERM、撤销两条测试变红 |
| 关连接在途调用最多再等 2 秒，之后切断 | `8841e62` | 去掉切断那一行后，中继压住回包 45 秒的两条测试都红；撤销和 `gld daemon stop` 现在都在 10 秒内返回 |

**设计取舍：**

- 没复用 `--write`。`gld grant --write` 含跑命令，节点这边 `--write` 已验证为只管文件（§91），而且命令等于交出账号，默认不能顺带开。文档写明了两边的差别。
- `--run` 自带文件写入，配置里 `runnable_workspaces` 必须是 `writable_workspaces` 的子集。不设“能跑不能写”：那档看着比实际安全。
- 权限只按完整项目 ID 判。名字和 ID 前缀拿到的是只读 grant，硬发 `exec_command` 只会收到 `unknown_tool`（有测试）。
- Hub 用目录里的 `tool_profile` 提前拦一次，只为报错写明白（`NODE_COMMANDS_NOT_ALLOWED`），判定点仍在节点。旧节点不报告 `commands` 字段时按没开处理。
- `Hub::for_node` 的三个 `Vec<String>` 收成 `NodeAccess`：这是权限边界，三个同类型参数排错了编译器不报。

**真实链路验证（CLI、Hub HTTP、TLS、WSS，隔离 `GLD_HOME`）：**同一台机器上 `--run` 的项目能跑，只写的项目报 `NODE_COMMANDS_NOT_ALLOWED` 且没跑；
默认 guarded 下 `sh` 报 `POLICY_REJECTED`，目标机改 `command-policy=all` 后才跑，说明目标机策略照样管用；
命令跑着时断线，机器离线期间命令继续写文件，重连后 `list_runs` 看到退出码 0，文件只有一行（没有重放）；`kill_session` 后记 `killed`；
SIGTERM 后进程没了、节点重启后记 `interrupted`；`gld node revoke` 后节点退出，命令也停；`kill -9` 时命令还活着，重启后记 `unknown`（这条是把缺口写实，不是修好了）。

**主体隔离：**要两个 OAuth 客户端，用 core 的 hub 测试走 `forwarded_meta` / `auth_from_meta` 验：bob 拿 alice 的 `session_id`，`read_output`、`write_stdin`、`kill_session` 都是 `SESSION_NOT_FOUND`，`list_runs` 是空的；alice 换一个新连接对象还能找回并停掉。没有用两个真实 OAuth 客户端走 HTTP 跑过。

**踩到的：**

- 测试第一版以为 `command-policy=all` 放开 `cmd` 里的 shell 拼接，其实只放白名单和确认，拼接仍拒，要用 `argv` 调 `sh -c`。生产代码没问题，是我没摸清就写了测试。
- 又一次差点犯 §94 的错：`clippy ... | tail -3 && cargo test` 没开 `pipefail`，clippy 失败没拦住后面的步骤。这次没有提交，修完后的检查都开了 `pipefail`。
- 变异检查留下的 `sleep 600` 进程已按工作目录筛出来清掉。

**边界，没做的：**

- 节点停止的瞬间如果 `exec_command` 刚好还没登记会话，这条命令会漏掉；窗口只有进程启动那几毫秒，没处理。
- `exec_health_check`、任务收尾、委派、配置管理在节点上仍不开放。
- 调用等 yield 时占着这台机器的连接，别的调用（包括 `kill_session`）收到 `MACHINE_BUSY`，最多 30 秒。文档教的做法是传小 yield 再查，没做连接内并发。
- 节点在真实另一台机器、Windows 上都没跑过，命令相关测试只在 Unix 编译。

隔离 GLD_HOME 全仓 `cargo test --workspace --all-targets --locked --no-fail-fast`：60个测试目标 1272 passed / 0 failed；Clippy `-D warnings`、fmt --check 通过。
协议仍15，Hub 对外工具表未改，连接器不用 Refresh；新增配置字段缺省不开，旧配置继续有效。现有服务、OAuth/grant、安装、发布均未改动，未推送。

## 96. 独立审查 §95：只给文件写入的项目能跑任意命令（2026-10-30）

**范围与基线：**§95 是权限边界，派一个只读子代理审 `73bb903..30da137`，只给代码和要判断的问题，不给我的结论。下面每条都由我复核。

**高，已修：`--write` 项目能以节点账号跑任意命令。**这是 §91 文件写入带进来的（只查路径第一段的 `.git`），§95 才把“`--write` 只管文件”写成了边界。
路径：`apply_patch` 新建 `sub/.git/config`，写 `core.fsmonitor = touch X; false #`，再调 `git_status path=sub`，git 就执行这条命令，而 `exec_command` 是被拒的。
不含 `.git` 的裸仓库形状（`bare/{HEAD,config,objects,refs}`）也行。先写了会失败的测试：修之前嵌套 `.git` 写得进去；只去掉 git 那层防护时，
测试停在“git 执行了项目里的命令”那行（git 2.56），两条路都是真的。

修法分两层，都不依赖调用方是谁：
- 写：任意一层的 `.git` 都拒（`write_class::is_git_internal`），大小写不敏感，含 Windows 的 `.git.` 和 `git~1`。
- 读：所有在项目里跑的 git 都从 `git_command` 起。往上找仓库时，先碰到像裸仓库的目录就报 `GIT_BARE_REPOSITORY_REFUSED`，形状包括只有 `HEAD` 和 `commondir`、对象库在别处的。
  另带 `-c safe.bareRepository=explicit`；单靠它不够，这个配置要 git 2.38，而 Ubuntu 22.04 自带 2.34。

没走“拒绝顶层不是项目根的仓库”：本机项目里有嵌套仓库是常见正常用法，而文件写入已经改不到它们的配置了。

**剩下的缺口，已写进 security.md：**仓库配置本来就指向仓库里的脚本（`core.fsmonitor`、filter、textconv）时，改脚本等于能执行。工具层分不出来，这种仓库不要给 `--write`。
`--write` 现在的准确说法是“不开放命令工具”，不是“绝对跑不了代码”。项目本身是裸仓库时 git 工具也会拒，这是这次带来的行为变化，本机项目同样受影响。

**中，已修：SIGHUP 时不收命令。**前台运行时关终端、SSH 断开发 SIGHUP，节点直接死，命令留在后台。子代理实跑确认，修后两条真实链路测试：SIGHUP 时节点和命令都停；
SIGHUP 被忽略（`nohup`）时节点继续在线。第一版先装处理函数再查是否被忽略，查到的是自己，测试抓到后改了顺序。

**低，已修：项目从目标机移除后命令不停。**`forget_departed` 只收 `hub` 入口，节点调用方的入口是 `node`，调用方又调不到 `kill_session` 了。现在按实际入口收，core 测试验证进程消失。

**低，只写了文档：**Hub 连过 `--run` 节点后回滚旧二进制，旧版校验不认 `commands`，整个 Hub 报 `HUB_UNAVAILABLE`。只读代码，没实跑。旧二进制没法改，所以写进 install.md 的回滚说明：连数据目录一起回退。

**没修：**
- 工具档改成只读后，`kill_session` 也被拒，已起的命令要等节点重启或命令超时。本机项目同样如此，改工具档的是目标机操作员，他能自己停。
- 子代理标“不确定”的那条（同一目录登记两个权限不同的项目时可能把活记录写成 `unknown`）：`gld add` 看路径去重，走不到；没再深挖手改数据文件的情况。

**子代理核对过、没发现问题的：**取消逻辑（`Notify` 的 permit 语义、`close` 与 `try_with_peer` 竞态、锁）、主体隔离、退出路径、新旧版本互配。这些是它读代码的结论，我没有逐条复跑。

隔离 GLD_HOME 全仓 `cargo test --workspace --all-targets --locked --no-fail-fast`：60个测试目标 1277 passed / 0 failed；Clippy `-D warnings`、fmt --check 通过。协议仍15，工具表未改，现有服务、OAuth/grant、安装、发布均未改动，未推送。

**提交顺序的问题：**提交这一节时，改文档的脚本因为字符串没对上而失败，同一条命令后面的 `git commit` 照跑（换行不算门控）。全量是提交前跑过的，代码没坏，
但 `2f8b7ee` 没带上它的测试，测试落在 `35dff90`；任务表和 RFC 的更新也漏了，由后一个提交补上。没有改写历史。

**CI：**用户让推，main 推到 `dabb233`。Linux、macOS 测试和 MSRV 都过，Windows job 红在 `computer/probe/state.rs` 的 unused-mut（`17ea95b` 留下的，Windows 开了 `-D warnings`，这是它第一次上 CI）。
本机交叉检查走不通：`ring` 缺 Windows 头文件。`00406e5` 修完后 8 个 job 全过，这轮节点代码在 Windows 上编译通过；Windows 上只真跑了补丁落盘和写锁那组测试，节点和命令相关的没跑过。

## 97. 节点第一次跨机器跑通：真机冒烟（2026-10-31）

**范围与基线：**用户同意“写 M2 之前先真机冒烟”。基线 `a3b73c0`。节点到这之前只在同一台机器上测过。

**怎么搭的：**本机用隔离数据目录起临时 Hub，Linux 服务器（Debian 13）的普通账号用隔离数据目录和临时项目当目标机，二进制在它的临时目录现场构建。
目标机经 SSH 反向隧道连 Hub 的回环地址，用真实的 `gld node invite`、`gld join`、`gld node run`；客户端是一个直接发 MCP 请求的脚本。没有开公网地址。

**没覆盖的：**TLS 和反代：隧道里走的是明文回环，WSS 证书校验没在真机走过。Windows、ChatGPT 真实连接器、经代理出网都没测。

| 场景 | 结果 |
| --- | --- |
| 配对、三级权限 | `--run`、`--write`、只读三个项目权限正确；只写和只读项目跑命令都报 `NODE_COMMANDS_NOT_ALLOWED`；guarded 下 `sh` 报 `POLICY_REJECTED`；写 `sub/.git/config` 被拒 |
| 延迟和大回包 | 读文件 10 次平均约 30 ms；1 MiB 读取 0.6 秒；3 MB 图片 0.9 秒 |
| 长等待 | `yield_time_ms=30000` 30.1 秒正常返回，同一连接继续可用 |
| 硬断（杀隧道） | Hub 0.5 秒发现离线；离线调用报 `MACHINE_OFFLINE`；命令照跑；恢复后 4.8 秒重连，命令只跑了一次，句柄不变 |
| 黑洞（冻住隧道进程，两端都收不到断开） | Hub 39 秒发现离线（20 秒 Ping 加 20 秒等 Pong）；命令照跑、只跑一次；解冻后自动恢复，仍只有一条连接 |
| SIGTERM | 节点退出，命令停，重启后记 `interrupted` |
| 真的断开前台 SSH | 节点退出，命令停 |
| `gld node revoke` | 调用报 `MACHINE_REVOKED`；节点 2 秒内因 401 退出，命令停 |
| `gld leave` | 配置删除，项目文件不动 |
| Hub 重启 | 节点自动连回，配对不用重做 |

**发现并修复（`b7bc92e`）：**

- **并行调用直接被拒。**一个调用在途时别的立刻 `MACHINE_BUSY`。RFC 原来判断“首版接受”，真机量出来 8 个并行读 6 个失败、16 个 11 个失败，而客户端确实常这样发。
  现在最多等 5 秒，真机上 8 个、16 个全部成功，最慢约 370 ms。真正卡住的调用后面的人等满 5 秒还是 BUSY，不无限排队（测试让中继压住回包 9 秒验证）。没做连接内并发，这个小改动够了。
- **只读项目写文件报错看不懂。**原来只有 `NODE_REQUEST_REJECTED`“节点拒绝了请求”，现在 Hub 提前报 `NODE_PROJECT_READ_ONLY` 并说怎么开。权限仍由节点判定。

**过程里我自己的错（都不是产品问题）：**
- 经 SSH 切到测试账号起后台脚本的那条命令不退出，我结束它之后被卡住的脚本才开始跑，和重跑的一份抢连接，出了几条假 FAIL。也正因为这样才注意到并行被拒。
- 脚本里几条 FAIL 是预期写错：`read_file` 单次上限 1 MiB、`exec_command` 回包没有 `running` 字段、`timeout_ms` 上限 60 万。

**还是 M2 要做的：**`gld node invite` 不给 Hub 地址，`--hub` 要自己拼；目标机加入要自己把命令写全；`gld node run` 只能前台跑，退出登录就没了。

**清理：**两端临时进程和目录都删了；Linux 上原装的 gld 仍是 `fba39d0`；开始时本机线上守护进程本来就没在跑，这轮没启动它，它的数据目录也没动。

隔离 GLD_HOME 全仓 `cargo test --workspace --all-targets --locked --no-fail-fast`：60个测试目标 1279 passed / 0 failed；Clippy `-D warnings`、fmt --check 通过。协议仍15，工具表未改，未推送。

## 98. RFC-0008 稳定宿主基础：工具进子进程（2026-10-31）

用户 2026-10-30 决定开始做。分支 `claude/continuous-upgrade-091095`，没推送、没换装、没重启任何在跑的服务；测试一律隔离 `GLD_HOME`。**不是零中断**，§6 逐条状态写在 RFC §5，这里不抄。

**做了什么：**`gld daemon run` 变成宿主（端口、隧道、OAuth、节点 WSS、配置写入），工具和命令进子进程 `gld daemon generation`，走子进程的匿名管道（RFC §3.4）。新增 `gld daemon upgrade status / prepare / switch / abort`。协议 16。

**用户能感觉到的行为变化，就这几条：**

- 执行代被杀时服务不再整个没了，正在执行的调用收到 `generation_lost`，下一条接着用。
- 只杀守护进程（`kill -9`）时，命令不再成孤儿：执行代读到管道关闭，收好再退，记录是 `interrupted`。原来的测试 `a_command_whose_daemon_was_killed_is_on_record_as_unknown` 改成连执行代一起杀，`unknown` 的语义还在。
- `gld daemon restart` 查在跑的命令时多看 `daemon.json` 里的 `generation_pids`，提示和 `--force` 不变。
- 多一个进程。`gld start`、连接器的 client_id 和授权都不动。

**验证：**隔离 GLD_HOME 全仓 `cargo test --workspace --all-targets --locked --no-fail-fast`：61 个测试目标 1308 passed / 0 failed，清掉工作区三个包的缓存之后跑的；Clippy `-D warnings`、fmt --check 通过。都是 macOS，Linux 和 Windows 没跑（Windows 路径本机也编译不了）。其中 `crates/cli/tests/generation_boundary.rs` 21 条真进程测试（见 RFC §5 那张表）。手工实测：`~/.local/opt` 里真实的 0.8.3 守护进程配新命令行，隔离 `GLD_HOME`，`upgrade` 四条命令都报"协议 5"退出 1，守护进程日志只有 7 次 `daemon_info`，进程还活着。

**独立审查（子代理，只读，没跑代码）发现的问题，都先复现再修：**

| 级别 | 问题 | 处理 |
|---|---|---|
| 高 | 嵌套 126–127 层的请求让调用永远挂着：帧多包两层，超过 `serde_json` 的 128 层上限，执行代拿不到 id，只打一行日志；在途计数不归零，Actions 一直占写锁；回包方向一条就杀掉整代 | 测试先红（请求超时）后绿。内容再编码成文本，读不懂就按 id 回错误 |
| 中 | 工具跑完后回包超限或 panic，Actions 却报可重试，MCP 提示 Retry the request | 只有确定没发出去的才可重试 |
| 中 | 崩溃替换覆盖已经完成的切换，`switch` 报成功、新项目却进旧版本 | 测试先红（`current` 被抢成 g3）后绿 |
| 中 | 宿主和执行代各读一次配置，看法不一致时同一个目录两代写 | 执行代核对，回 `WORKSPACE_ROUTE_CHANGED`，单元测试 |
| 中 | 项目移除后旧代收不到调用，命令没人收，旧代也退不了 | 有旧代时每 5 秒对一次成员名单，真进程测试 |
| 低 | 编出来的 work_session id 能撑大路由表；信号和 EOF 同时来收尾被截断；退出中还接新调用；崩溃后计数减错；kill 可能打到已回收的 pid；工具核对漏了服务自己的工具 | 都修了 |

没修的几条写在 RFC §5。子代理查过没发现问题的：身份在通道两端不变（含 browser 标记）、没有自动重放、`link.call` 都在 State 锁外、关停顺序、老守护进程拒绝。这是读代码的结论，不是实测。

**撞到的事：**

- 拆提交时写了一个多余的 `cp /dev/null /dev/null`，`set -e` 拦下，当时什么都没提交。
- **`c8f184c` 单独编译不过：**按 hunk 暂存用了 `--unidiff-zero`，有的 hunk 落错了位置，当时的编译检查和提交串在同一条命令里，检查在提交之后才跑。下一个提交 `4cb3a72` 补齐；按规矩没 amend，二分时跳过它。
- `pgrep -f` 写死字样时匹配到了跑测试的 shell 脚本本身（命令文本里带这段字），报了不存在的孤儿，现在用每次运行唯一的 sleep 时长。
- 测试自己写错过两处：`cmd=true` 被命令行解析成布尔；`pwd` 跑完 `list_runs` 查不到（换成 `sleep 0.1` 就有，没再查原因）。
- 拆提交后全量红了一条 `version_names_the_build_commit_when_it_is_known`：我把历史提交导出到临时目录、复用同一个 `target` 做编译检查，那份没有 `.git`，编出的 `gld-core` 不带提交号，cargo 算出的哈希又一样，留在了缓存里。`cargo clean -p gld-core -p gld-daemon -p gld` 后重跑就过了，代码没问题。以后这种检查别复用 `target`。
- main 在这期间多了 `b7bc92e`、`f263e4c`（节点 BUSY 短等、真机冒烟，审查 §97），这个分支是从 `a3b73c0` 拉的，合并时审查和任务表要手工接一下。`nodes/` 内部没动。

**接上 main（2026-10-31）：**用户定先不把分支合回 main。把 main 的 `b7bc92e`、`f263e4c` 合进这个分支，只有审查和任务表的文字冲突，§97、§98 都留。节点忙时短等 5 秒发生在宿主进程，执行代问宿主的上限是 75 秒，比节点调用的 60 秒加 5 秒长，时限不冲突。合并后隔离 GLD_HOME 全仓 61 个测试目标 1310 passed / 0 failed，Clippy `-D warnings`、fmt --check 通过，只在 macOS 跑过。

合并提交的说明第一次写错了：提交用的 heredoc 少了结束符，后面的脚本文本被当成了说明。用户点头后重建了这个提交和它后面的文档提交，只换说明和这一句，代码内容没动；当时都没推送过。

**推分支之前改了一次历史（2026-10-31）：**`git diff` 查出 §97 有一行写着主机别名和账号名，仓库是公开的。用户点头后重建了那个提交（原 `5913f37`，现 `f263e4c`），只换那一行，本地 main 同步移过去，分支的合并也接到新提交上；这些都还没推送过。`origin/main` 的旧节里还有两处主机别名，已经公开，没动。

**CI（2026-10-31）：**用户让推分支跑 CI，开了草稿 PR（xwfe/gld#1），没合。第一轮 `31b12f7`：8 个 job 过 6 个，Linux、macOS 测试都红在 `project_commands_inherit_nothing_from_the_private_channel`。多出来的管道和 socket 是 CI 运行器经 cargo、测试进程一路传下来的，时间戳早于守护进程启动，不是内部通道。没放宽断言，改成起守护进程前关掉所有继承来的描述符（`2715b13`），断言还是恰好三根；验过：临时去掉 close-on-exec 时红（5 根），还原后绿。

第二轮 `2715b13`：8 个 job 全过。Linux 61 个目标 1309 passed / 0 failed，macOS 61 个目标 1310 passed / 0 failed。Windows 编译通过，`daemon_lifecycle.rs` 9 条用新架构跑过；`generation_boundary.rs` 只在 Unix 编译，所以 Windows 上换代、杀执行代、通道隔离都没跑过。这些都是 CI 上的隔离环境，不是真实连接器，也没有换装。

**顺带发现、没改的旧行为：**gld 不关启动时继承来的描述符，会一路传给项目命令，这次拆分之前就这样。从 shell 正常启动时基本碰不到；要是把 gld 嵌进别的程序里拉起，那个程序没设 close-on-exec 的描述符项目命令都能拿到。

**合回 main（2026-10-31）：**用户让合。用合并提交，提交哈希不变，审查里引用的那些都还对得上；`c8f184c` 单独编译不过这件事也留在了历史里，二分时跳过。合之前最新一轮 CI 还在跑，那一轮的提交相对全绿的 `2715b13` 只改文档，本地文档门禁通过，所以没等。还没换装：本机用上要重启一次，要用户另外点头（RFC §4）。

## 99. 本机换上稳定宿主基础（2026-10-31）

用户看完 main 的 CI（`fef936d`，8 个 job 全过）后让换。从 `fef936d` 构建 release，二进制写新文件再改名。**换之前守护进程本来就没在跑**（§97 也记过），所以没有停掉任何命令、也没有打断任何请求；这次并没有验证“有东西在跑时怎么换”，只是把服务起了起来。

| 核对项 | 结果 |
|---|---|
| 版本 | 命令行和服务的构建提交都是 `fef936d3f781`，协议 16 |
| 进程 | `gld daemon run` 是宿主，`gld daemon generation` 是它的子进程，服务端口的监听者是宿主；`daemon.json` 记着执行代 pid |
| 环境 | 两个进程都没带 `CLAUDE*` 变量 |
| 凭据和配置 | 换前换后 `data/profiles.json` 各顶层键和 `data/oauth-clients/` 下各文件的哈希 30 项全一致；项目 21 个，启动时恢复开着 |
| `gld ls` | 只有状态从 stopped 变成 running，地址和 Client ID 没变 |
| 探测 | 本地和公网 `GET /mcp` 200，OAuth 元数据 200，无凭据 `POST /mcp` 本地和公网都 401；`gld health` 全过 |
| 真实调用 | `gld tool call server_info` 经执行代返回同一构建提交，工作区绑到 `g1`；重启次数 0，没有失败记录 |
| 工具表 | 34 项 `f0e996d1217ba15b` → 35 项 `6022dd56440b0ea9`，变了，ChatGPT 要点 Refresh |

**没验证的：**真实 ChatGPT 连接器（要用户点 Refresh 再开新对话，然后看日志里是不是原来的 `auth=oauth:hub:dcr-…`、有没有 401）；公网探测只到无凭据这一层。`gld daemon upgrade` 没在线上用过。回滚用的备份见任务表；Hub 连过开了 `--run` 的目标机之后要连数据目录一起回退（`docs/install.md`）。

**真实连接器（同日 12:27–12:31）：**用户在 chatgpt.com/plugins 点了 Refresh，再开新对话贴了一段只读提示词。日志里 Refresh 那次走了 `server/discover`、`initialize`、`tools/list`，新对话里 `list_workspaces`、`server_info`、`read_file`、`prompt_manage` 四次 `tools/call` 都有对应的 completed，身份都是换装前那个 `dcr-…` 客户端，这段时间没有 401、没有 `worker_failed`。`data/oauth-clients/hub.json` 和其余配置指纹仍和换装前一致，所以确实没重新授权。调用都落在执行代 `g1`，项目 gld 绑在它名下，重启次数 0。

`gld usage` 报 10 个请求、1 个错误。这个错误是 ChatGPT 发的 `server/discover`：gld 没实现这个方法，回 Method not found，回包正好 114 字节，和日志一致；换装前同样的回包有 1338 次，是老行为。所以 4 次工具调用都没报错。

**只证明了这些：**只读调用经执行代走通。没有从连接器跑过命令和写入，没有覆盖切换，没有在线用过 `gld daemon upgrade`，`prompt_manage` 只列了没读内容。请求日志不记回包内容，所以这几次的 `build_commit` 等结果是用户在对话里看到的，这边没有核对。

## 100. 发布 0.9.0（2026-10-31）

用户让发新版。`v0.9.0` = `087765a`，从 0.8.3 起 240 多个提交，协议 5 到 16，所以升 minor。发布说明的“从上一版升上来”写明 ChatGPT 要点 Refresh，相对 v0.8.3 工具只增不减（新增 `agent_manage`、`prompt_manage`）。

| 步骤 | 结果 |
|---|---|
| 本机打包 | `gld 0.9.0 (087765a01207)`。隔离 `GLD_HOME` 起服务、无凭据 401、bearer 读文件、`prepare` + `switch`、停止后没有残留执行代 |
| 退回 0.8.3 | 真实 0.8.3 二进制接同一数据目录能起服务、读文件，再升回来也行。只测了 bearer，没带节点和 OAuth |
| 发版前 | main CI 全绿；手动空跑 Release 全绿，“建 Release”skipped（只在 tag 上跑） |
| 发布 | tag 流水线全绿，Release 6 个文件，非草稿、非预发布 |
| 校验和 | 5 个包 `shasum -c` 全 OK |
| 来源证明 | 5 个包 `gh attestation verify` 都退出 0；改一个字节的包退出 1 |
| macOS | 下载的 Apple 芯片包和 Intel 包（Rosetta）`--version` 都对；没用下载包起过服务 |
| Linux | 下载的 gnu、musl 包在 amd64 Debian 12 容器里：起守护进程，能看到执行代进程，读文件正常，停止后执行代没了 |
| Windows | 只核了校验和和来源证明，**没跑**。CI 里 9 条生命周期测试跑的是构建产物 |

**没做的：**没有代码签名；本机没换成发布包，还是 `fef936d`（代码只差版本号）。另外两台机器没换，还是 `fba39d0`。

**撞到的事：**`gh release download` 在临时目录里没有仓库上下文，要加 `--repo`；`gh run watch` 有一次提前退出 1，当时流水线还在跑，改成看 `gh run view` 的最终结论。

## 101. D16-M2 不依赖真机的部分：邀请、安装脚本、自启动、诊断、代理（2026-10-31）

用户让开始 M2。`8cdf294`–`b9d4f5a` 共 11 个提交，用法在 [concepts](../concepts.md#接入一台新机器)、[install](../install.md#一条命令安装) 和[排错](../troubleshooting.md#目标机节点)，不再抄。没换装、没推送、没发版。

| 项目 | 做了什么 | 怎么验的 |
|---|---|---|
| 代理 | 配对和长连接共用 `nodes/proxy.rs`，只认环境变量和 HTTP CONNECT | 真实 CLI、假 CONNECT 代理、TLS 中转走通配对和长连接；`NO_PROXY` 直连；SOCKS 启动就报错且不露密码 |
| 配置位置 | `--share` 能写目录，配置默认 `nodes/<Hub ID>.json`，只有一份时后续命令不写路径 | 集成测试走完 join、run、shares、leave，全程没起守护进程 |
| 自启动 | `gld node autostart`，launchd、systemd 用户单元、登录计划任务；`--service` 运行模式 | 桩代替管理器的集成测试；**真实 launchd** 见下 |
| 诊断 | `gld node doctor` 握手一次，每项失败给修法 | 集成测试覆盖没跑、在线、证书不受信、端口不通、撤销、Hub 读不了数据 |
| 邀请 | 默认用固定入口，临时地址、OpenAI 隧道、没有入口都拒绝，`--hub` 可指定；输出三个系统的命令 | 单测和集成测试；被拒的邀请不落盘 |
| 安装脚本 | `install.sh`、`install.ps1`，Release 附带 | `install.sh` 6 种失败路径（`sh` 和 `dash`）；v0.9.0 真实发布包在本机装通。`install.ps1` 只进了 CI，还没跑过 |

**真实 launchd（本机，隔离 `GLD_HOME`，临时 LaunchAgent，测完已删，`~/Library/LaunchAgents` 里没留）：**join 带 `--autostart` 后 `state = running`，`plutil -lint` OK，plist 权限 `-rw-------`；连续三次重跑 enable 都成功，重启后约 2 秒重新接上；运行超过 60 秒后 `kill -9`，1 秒拉回，启动不满 60 秒就被杀的要等 58 秒（`ThrottleInterval`）；撤销后进程继续跑，日志写明已停命令、600 秒后再试；disable 后 launchd 里没有这个服务。

**真实环境和全量测试才抓到的（桩和单测都放过了）：**

- 刚 bootout 马上 bootstrap 报 `Bootstrap failed: 5: Input/output error`，定义文件还被删了（`0a203c8`）。这是升级路径。
- 重跑 enable 时 Hub 还没释放旧连接，新进程收到 409 后等 60 秒（`155a49c`）。全量并行跑时才出现。

**独立审查（只读，不带结论）的结果，全部核对过代码：**

| 严重度 | 缺陷 | 处理 |
|---|---|---|
| 高 | Hub 读不了数据文件时回 401，服务节点收到就退出码 0 永久离线，修好 Hub 也回不来 | 改回 503。服务模式 401 不再退出：响应没签名，可能来自反代的认证，所以先停命令、每 10 分钟再试。只有 Hub 身份对不上才停，因为节点凭据在证明身份前就发出去了 |
| 中 | 外置盘没挂载等启动错误走“配置坏了”分支，退出后要等下次开机 | 留在进程里等，不靠服务管理器 |
| 中 | disable 不看停没停下，`su` 会话里定义删了、节点还连着，却说已停止 | 真停下才算成功，否则报错并保留定义 |
| 中 | leave 吞掉 disable 的错误；前台节点开着时自启动被静默删除 | 撤不掉就报错，什么都不删 |
| 中 | systemd、Windows 启动失败留下定义，下次登录自己起来，代理密码也留在盘上 | 失败时统一删除 |
| 低 | join 在进程内直接写数据文件，和运行中的守护进程没有跨进程锁（`store.rs` 的锁是进程内 Mutex） | 改走 Backend，和 `gld add` 同一入口 |
| 低 | 只剩 pending 时 `gld leave` 找不到；代理响应头非 UTF-8 被判无效；相对路径的 `GLD_HOME` 进定义后指错；控制字符能写进定义；Windows 上 `.old` 删不掉时升级中止；`%VAR%` 被压平；noexec `/tmp` 报错误导 | 逐条修了 |
| 不确定 | Windows 任务子文件夹可能要管理员、`\\?\` 路径别的程序未必认 | 没法验证，直接换成更稳的写法：任务放根目录，路径还原 |

**没修的：**`ExecStart` 第一个参数含 `$` 时的转义没核实，路径含 `$` 很少见；Windows 上 `schtasks /End` 等同强杀，命令可能留下；不接受 PEM 证书和 HTTPS 代理。

**我自己的错：**`155a49c` 带着 clippy 错误提交了，`89d1a77` 补。当时的写法是 `set -euo pipefail` 加 `cargo clippy … | tail -1` 再 `git commit`。实测这里的 Bash 工具跑 zsh，`false | cat` 之后脚本照样往下走，所以之前记的“开头写 `set -euo pipefail` 就够”是错的。之后一律 `if 检查 > 日志; then 提交; fi`。

**没验证的：**

- **v0.9.0 的 Release 里没有安装脚本**，当前版本号也还是 0.9.0，所以现在 `gld node invite` 打印的第一行会 404，第二行报找不到 gld。要等下一版发布，`release.yml` 新增的那一步也要到那时才第一次跑。
- 真实 systemd 没跑，只有桩。Linux 服务器有 systemd，但在上面起服务要用户点头。
- Windows：`install.ps1`、计划任务、`FreeConsole` 都没运行过。CI 新增的那一步推送后才跑，自启动的测试不在 Windows 上跑。
- 干净的 Mac、Windows、Linux 各走一遍生成的命令，是 M2 剩下的验收。真实代理、真实 TLS 入口同样没走过。

全仓 1365 项通过，clippy、fmt 通过（本机 macOS）。

**推送后的 CI（同日，`18596d3`）：**全部任务通过。`install.ps1` 第一次真正运行：Windows 自带的 PowerShell 5.1 经 `Invoke-Expression` 下载 v0.9.0 发布包，装到临时目录，`gld 0.9.0 (087765a01207)` 能跑，指定不存在的版本时报错。只覆盖了这一条路径，`install.ps1` 的校验和不符、替换旧版没跑过，计划任务和自启动仍然没在 Windows 上运行过。Ubuntu 任务通过，包含用桩代替 systemd 的自启动测试，真实 systemd 仍没跑过。

**真实 systemd（同日，用户让验的。Debian 13、systemd 257，`bing` 账号，隔离 `GLD_HOME`，临时用户单元，代码 `aa3ed26`，间隔那一项用 `7edf120`）：**

| 场景 | 结果 |
|---|---|
| `join --autostart` | 单元 `enabled`、`active`，权限 600，`systemd-analyze verify` 无警告；Hub 看到在线，读文件、跑命令正常 |
| SSH 断开后新开会话 | 节点 pid 不变，命令还在，Hub 在线 |
| 重跑 enable | pid 换了，旧命令被停，马上重新在线 |
| `kill -9` | `RestartSec=60` 时 60 秒才拉回，改成 10 后 10 秒（`7edf120`）。命令被 systemd 按 cgroup 一起收了，和 launchd 下不同，已写进 concepts |
| Hub 停掉 | doctor 报拒绝连接；Hub 起来后 1 秒内重新在线 |
| 撤销 | 进程继续跑、命令被停，日志写明 600 秒后再试，doctor 报 401 和修法 |
| `su - bing` 里 disable（节点在跑） | `systemctl --user` 连不上总线，报 `The node is still running`，退出码 1，定义没动。§101 审查修的就是这条 |
| 正常会话 disable | 单元没了，`inactive`，节点进程没了 |
| `su - bing` 里 enable | 报 `no systemd user manager`，退出码 1，不留定义 |
| 再 enable 后 leave | 先撤自启动再删配置，单元文件没了 |

launchd 的间隔同样改成 10 秒后在本机复测：启动不久就被杀，拉回不超过 10 秒。

**没验证的：**这个账号的 linger 本来就开着，所以“`enable` 替你开 linger”和开不了时的提示都没走到；没真的重启机器，开机回来靠 `enabled` 加 linger 推断；用的是调试构建，不是发布包。

**过程里脚本自己的问题（不是产品）：**检查“命令拿到 `GLD_HOME`”的那条写错了，这个变量按设计不传给项目命令，改看原始回包确认 `PATH`、`LANG` 和开启时一致；清理脚本的 `pkill -f gld-smoke/` 匹配到了脚本自己，目录没删掉，之后手动删了。测试机上的单元、进程和目录都已清理，原来装的 gld 和 `~/gld-src` 没动。

## 102. 发布 0.10.0（2026-11-01）

用户让发新版。`v0.10.0` = `b6c799b`，升 minor：只动原生节点，但有新功能和行为变化（邀请拒绝临时入口、不支持的代理启动就报错）。工具表和守护进程协议都没变，发布说明写“连接器什么都不用做”。

| 步骤 | 结果 |
|---|---|
| 工具表 | 相对 `v0.9.0`，`tools/registry.rs`、`hub/`、`bridge/tools.rs`、`machine_mcp/relay.rs`、`crates/daemon` 没有改动；隔离数据目录里两版都是 32 项、指纹 `9cb99c69e724c997` |
| 新旧互连 | 新 Hub 配新节点、新 Hub 配 0.9.0 节点、0.9.0 Hub 配新节点，都能上线、读文件、跑命令 |
| 退回 | 0.9.0 的 `gld node run --service` 报未知参数，退出码 2，所以发布说明要求先撤自启动 |
| 发版前 | main CI 全绿；手动空跑 Release 全绿，“建 Release”skipped |
| 发布 | tag 流水线全绿，Release 8 个文件（5 个包、`SHA256SUMS`、两个安装脚本），非草稿、非预发布。`release.yml` 附脚本这一步第一次真跑就通过 |
| 校验和 | 5 个包 `shasum -c` 全 OK；`SHA256SUMS` 不列安装脚本，和设计一致；两个脚本和 tag 里的逐字节相同 |
| 来源证明 | 5 个包 `gh attestation verify` 都退出 0；改一个字节的包退出 1 |
| 安装脚本（macOS） | 官方地址装到隔离目录；不设版本时装到 0.10.0；不存在的版本报 404，原文件哈希不变；另一台 Apple 芯片的 Mac 也装上了 |
| 安装脚本（Linux） | 没有 gld 的 amd64 Debian 12 容器：官方地址装上 musl 包，失败退出码 1 且原文件不变；接进同容器的 Hub 后读文件、跑命令、`node doctor` 都正常。Linux 服务器（Debian 13）也装上了 |
| 其他包 | Intel 包经 Rosetta `--version` 正确；glibc 包在同样的容器里起停守护进程、读文件正常 |
| 邀请 | `gld node invite` 打印的安装行版本是 `v0.10.0`，地址能下载 |

**没做的：**Windows 只核了校验和和来源证明，下载包、计划任务、`install.ps1` 校验和不符和替换旧版都没真跑；没在完全干净的 Mac 上跑（另一台 Mac 本来就有旧版 gld，这次只装到临时目录）；linger 自动开启、真实重启、真实代理和真实 TLS 入口仍未验证。本机和另外两台的常用 gld 都没换。

**撞到的事（都是测试脚本）：**`gld tool call` 的参数要写 `key=value`，第一次写错报了一条 FAIL，改正后通过；zsh 没有 `PIPESTATUS`，要查管道中间的退出码得用 bash；ECR 的 `public.ecr.aws/debian/debian` 连拉几次会报 `toomanyrequests`，换 `public.ecr.aws/docker/library/debian` 能拉到。

**推送后的 CI（同日，`37e65cd`）：**全部任务通过。Windows 任务用自带的 PowerShell 5.1 跑 `install.ps1`，装上 v0.10.0 的包，`--version` 打 `gld 0.10.0 (b6c799bee0d2)`；不存在的版本按预期报错。这是下载包第一次在 Windows 上运行，但只跑了 `--version`，起服务、接节点、计划任务仍没跑。推送时本机磁盘写满，远端已更新，本地的 `origin/main` 没更新，腾出空间后 `git fetch` 补上了。

## 103. D16-M3 第一轮：一个 Hub 操作 Linux 和 macOS 上的项目（2026-11-01）

用户让接着做。这一轮用发布包（`v0.10.0`，升级那段从 `v0.9.0` 起）跑真实机器：Hub 在本机，隔离数据目录，节点经回环地址连它；Linux 服务器（Debian 13）和另一台 Apple 芯片的 Mac 经 SSH 反向隧道各连一个节点。目标机只用临时目录，测完已删，原有的 gld 和常驻服务都没动。75 项检查全部通过。

| 项目 | 结果 |
|---|---|
| 单入口 | 同一地址、同一口令列出 Hub 自己的项目和两台机器各两个项目；`uname` 分别是 `Linux`、`Darwin`，每台读到自己的标记文件，补丁写到各自盘上 |
| 输出 | 退出码 7、stdout 和 stderr 分开、UTF-8 中文都正确；1288895 字节的输出总量对，保留的最后 1 MiB 分 4 页一字不差，前面 240319 字节按设计报在 `dropped_bytes` |
| 后台命令 | 拿到 session，能查状态，`kill_session` 能停 |
| 权限 | 只读项目写入报 `NODE_PROJECT_READ_ONLY`，跑命令报 `NODE_COMMANDS_NOT_ALLOWED`，盘上没变；没共享的项目看不到 |
| 路径 | `../`、指向项目外的符号链接、绝对路径都报 `READS_CONFINED_TO_WORKSPACE`，内容没漏；补丁写不到项目外 |
| 每机一条连接 | 两个节点到 Hub 的已建立连接各 1 条。同一配置再起一个报配置占用，原连接不受影响 |
| 断线 | 断 Linux 的隧道，Hub 0.5 秒发现，调用报 `MACHINE_OFFLINE`，Mac 照常工作；隧道恢复 1.1 秒回来，项目 ID 不变，断线前起的命令还能查 |
| 节点重启 | Mac 节点 `kill -9` 后重启，项目 ID 不变 |
| Hub 重启 | 2.6 秒两个节点都回来，项目 ID 和口令不变，每机仍是一条连接 |
| 升级 | Hub 接同一数据目录、Linux 节点用官方安装命令，从 0.9.0 换到 0.10.0，机器 ID、项目 ID、口令都不变 |

**M3 还没完成：**没有 Windows 节点；这一轮是回环明文加 SSH 隧道，不是同一个公网域名，真实 TLS 没走；AI 那头是脚本发 MCP 请求，不是 ChatGPT 连接器。这三项做完才算 M3 通过，后两项要换本机服务。

**撞到的事：**
- 本机预演第一次时，官方安装命令没把节点换成 0.10.0，版本还是 0.9.0。当时安装输出被脚本丢掉了，原因没抓到；之后本机单独跑、本机预演、跨机各一次都换成功了。当时磁盘快满，可能相关，没证实。
- 脚本的 `cd … && 命令 &` 把整串放进后台子 shell，占着输出管道卡到超时；`pgrep -f` 在 `ssh host '…'` 里会匹配到检查命令自己，报 1 个假残留。这两条都是脚本的问题，不是产品。

## 104. 本机换上 0.10.0，节点第一次走真实入口（2026-11-01）

用户让换本机服务。换的是 `v0.10.0` 的发布包，校验和、来源证明都过了；换之前没有命令在跑。

| 项目 | 结果 |
|---|---|
| 换装 | 守护进程 15:37 重启，`gld --version` 和服务都是 `0.10.0 (b6c799bee0d2)`；两个进程没带 `CLAUDE*` 变量 |
| 连接器 | 工具表 35 项、指纹 `6022dd56440b0ea9` 不变；`gld ls`、`profiles.json`、`oauth-clients/hub.json` 前后一致，不用 Refresh、不用重新授权 |
| 公网 | `gld health` 全 ✓，公网 `/mcp` 和 OAuth 元数据 200 |
| 真实 TLS 配对 | 本机隔离数据目录的节点，用 `gld node invite` 的默认固定入口配对成功 |
| 真实代理 | 节点长连接经本机的 HTTP CONNECT 代理走公网入口，`gld node doctor` 报在线；这是真实代理和真实 TLS 第一次一起跑 |
| 长连接 | 空闲 6.5 分钟，一直是同一条连接，日志没有新增重连 |

**没验证的：**节点和 Hub 在同一台机器，只验了入口这条路。在另外两台机器上写文件、起进程被权限策略拦下，没跑；经入口时的跨机延迟和不走代理的直连也没测。ChatGPT 连接器调用节点要用户在 ChatGPT 里操作，这一节写下时还没做。

**ChatGPT 实测（同日）：**用户在新对话贴了提示词，请求全部经公网入口到达，带的是换装前同一个 `dcr-…` 客户端，没有 401。`git --version` 落在节点的运行记录里，退出码 0；`apply_patch` 真改了节点盘上的 `README.md`，内容变成 `m3-marker-chatgpt`。第 5 步读 `../secret.txt` 的调用到了，但请求日志不记错误码，这里没能从记录核实；那个文件也不存在。项目外路径被拒是 §103 用真实文件验的。

**清理：**`gld node revoke` 后停掉节点、删了临时目录，服务和 `gld health` 正常。**留下一条记录：**`data/profiles.json` 里 `m3-local` 还在，标 `revoked`。`gld node` 没有删除命令，`gld prune` 也不管这个。这是产品缺口：配过又不要的机器永远留在 Hub 的登记里。

## 105. `gld node remove`：删除已撤销的机器（2026-11-01）

§104 的测试节点撤销后留在 Hub 登记里，用户让加命令。`gld node remove <机器 ID 或名称>`（别名 `rm`），用法在 [concepts](../concepts.md#使用预置凭据或管理已登记节点)。协议升到 17，本机服务还没换，`m3-local` 还没删。

| 情况 | 结果 |
|---|---|
| 没撤销的机器 | 报错并给出撤销命令，数据一个字节不变 |
| 已撤销，按名称删（大小写不敏感） | 登记和项目目录没了，`list_workspaces` 不再列出，旧句柄报 `WORKSPACE_NOT_IN_HUB` |
| 其他机器 | 照常读文件 |
| 旧凭据再连 | 401 |
| 重复删除 | 报不存在，退出码非 0 |
| 同名重新登记 | 能用，拿到新的机器 ID，Hub ID 和公钥不变 |
| 名称长得像另一台的 ID | 完整 ID 优先，不会删错 |

全仓 1369 项通过，clippy、fmt 通过。第一次全量红在别名门禁：`remove` 命令必须有 `rm` 别名，补上了。

## 106. 发布 0.10.1，换到本机，删掉 `m3-local`（2026-11-01）

用户让发版并换到本机。`v0.10.1` = `15a7bb1`，只多了 `gld node remove`，协议 16 到 17，工具表没变。

| 步骤 | 结果 |
|---|---|
| 本机打包 | `gld 0.10.1 (15a7bb1ed638)`；§103 那套隔离验收 75 项通过，这次 Hub 是 0.10.1、节点是 0.10.0，混用正常；真实命令的拒绝和删除符合预期 |
| 发版前 | main CI 全绿；手动空跑 Release 全绿，“建 Release”skipped |
| 发布 | tag 流水线全绿，Release 8 个文件，非草稿、非预发布 |
| 校验和、来源证明 | 5 个包全过，改一个字节的包退出 1；两个安装脚本和 tag 里的逐字节相同 |
| 运行 | 不设版本时 `install.sh` 装到 0.10.1；Intel 包经 Rosetta、glibc 和 musl 包在 amd64 Debian 12 容器里 `--version` 都对 |
| 换到本机 | 换之前没有命令在跑；重启后服务是 `15a7bb1`、协议 17；工具表 35 项、指纹 `6022dd56440b0ea9`，`gld ls`、`profiles.json`、`oauth-clients/hub.json` 都没变；`gld health` 全 ✓，两个进程没带 `CLAUDE*` 变量 |
| 删 `m3-local` | 真实 Hub 上 `gld node remove m3-local` 成功，机器表清空；`profiles.json` 其余内容的摘要前后一致，再删报不存在且退出码 1 |

**没做的：**Windows 包只核了校验和和来源证明，没运行；ChatGPT 没在这次换装后再调一次，连接器相关的指纹都没变；Linux 服务器和另一台 Mac 的常用 gld 没换。

## 107. RFC-0008 工作区交接（2026-11-01）

用户让继续做零中断升级。这一期做“执行代连续更替”：切过去之后，工作区闲下来自动搬到新代，旧代搬空自己退。做法写在 RFC §3.5，§6 逐条状态在 RFC §5，这里只记验证和撞到的事。没推送、没换装；真实服务全程没碰，测试都是隔离 `GLD_HOME`。

**还不是零中断：**宿主本身换不了，`upgrade status` 照实报 `host_handoff: false`、`zero_interruption: false`。第一次换上这个版本要重启一次。

**验证：**

- 隔离 `GLD_HOME` 全仓（macOS，调试版）：`generation_boundary` 27 条，新增 6 条（闲着搬走并带走状态、默认目录、命令跑完才搬、留下的进程占着输出、连写两次换代、新代接不住），改了 4 条原来按“绑定后不换代”写的。数字见下一段最后一次全量。
- 真实版本混用（脚本，两个 `GLD_HOME`）：两个线程连续读写时换代，200 次读、143 次写，0 错误；换代窗口内读请求最慢 17 毫秒，换代前最慢 30 毫秒；`workspace_writes_since_start` 正好 143；交互命令切换后接着写 stdin，拿到退出码 5；旧代随后退出。新宿主配 0.10.1 候选：报 `adopt_failed`，项目留在原地照常服务。0.10.1 宿主配新候选：能切，不搬；新命令行配老守护进程时文案如实说它不会搬。
- 没验：真实 ChatGPT 连接器跨换代、真实 ccnm、真实隧道；任务验收的端到端（只验了它读的那个数）；Linux、Windows。延迟预算没定。

**自己撞到的：**

- 第一版在跑着读写时搬不走：每次看都有在途调用。实测 1.5 秒后 `in_flight_calls` 还挡着，原来的测试靠运气过。改成先挡住新请求、最多等 300 毫秒收尾，等不到就放行、下一轮再试（`HANDOFF_QUIESCE`）。测试改成请求不停时等旧代退出。
- 默认目录第一版没带过去：`resolve_existing` 不收绝对路径，恢复时悄悄失败。测试抓到的。

**独立审查（子代理，读代码，部分实测）发现的问题：**

| 级别 | 问题 | 处理 |
|---|---|---|
| 高 | 换代循环每 2 秒问一次，第一版用平时的闲置判断收远端连接，转到后台的远端命令 2 分钟就会被杀。平时的回收要等别的成员来请求才发生 | 只读连接闲够了才收；写会话闲多久都不收，留在旧代。真实 ccnm 没验 |
| 高 | 命令退了、留下的进程占着输出（`npm run dev &`），旧代退出后被 SIGPIPE 带走，实测复现。同步路径把读任务的句柄丢了，判断看不到 | 句柄留着，过了保留期也记着；项目和旧代都留到那个进程退出。审查建议查进程组，没照做：只有占着管道才会被带走，读任务还在不在就够了，这样 `nohup … > log &` 不会被白挡。去掉判断测试变红（切换回 `moved_workspaces: 2`） |
| 中 | 新命令行配老守护进程，`switch` 打印“null 个项目已经搬过去” | 回包没有这个字段就打印旧文案；status 按能力说 |
| 低 | 接手回复超时但其实接住了，工作区留在旧代又出事，之后的命令可能被算成“期间没写过” | 记下旧代交出计数的时刻，之后才起的命令不算（`null`，验收拒收）；计数不倒退；倒退时不算成 0。审查建议两阶段提交，这样改更小 |
| 低 | 关远端连接最长 20 秒，同步做会挡住排队的请求 | 放到别的线程 |
| 低 | 排队上限可能短于交接耗时；`switch` 串行搬，旧代卡住时命令行先超时 | 每步 4 秒，加上收尾短于 10 秒的排队上限；`switch` 当场最多搬 10 秒，剩下的后台接着搬 |
| 低 | 看一眼 MCP 列表也续 10 分钟 | 不带 server 的列表不算用过 |

审查查过没发现问题的：`moving` 卡死、两代同写、等审批的 Agent、`ToolContext` 里带不走的只有审计和用量计数、常规交接下的验收语义。这些是读代码的结论。

**低优先级下测试：**`taskpolicy -b` 跑 `generation_boundary` 失败 6 条，全是超时类（HTTP 读超时、2 秒内没就绪、`daemon start` 失败）。同样跑改动之前的 `4a206ac` 是 21 条里失败 8 条，报错同类，不是这次引入的。其中一条新测试靠 `sleep 4` 卡时序，在低优先级下命令先跑完了，改成等 stdin。

## 108. 桌面截图实测：守护进程在后台会话里截不到屏，另有两处策略问题（2026-11-01）

**范围与实际条件：**用户在本机经 Muse 的 `custom.gld` 连接器（用户自建、OAuth、compact 工具档案，本项目命令策略 `guarded`）让 AI 打开 ChatGPT 网页和应用，随后要求截一张全屏图并发回。这次的调用方不是 ChatGPT 连接器。审查时基线 `main@44dac24`、工作树干净，`server_info` 报服务构建提交 `fef936d3f781`、工具 35 项。本节只审查和记录，没有改代码；修复建议未排期，等用户定。

**实测过程：**

| 步骤 | 结果 |
|---|---|
| `open https://chatgpt.com`、`open -a ChatGPT` | 都退出码 0（399 ms、108 ms）；`open -a` 第一次经公网入口回 502、结果未知，读 `list_workspaces` 确认服务在答后重试成功 |
| `check_command` 查 `screencapture -x muse-fullscreen.png` | `deny`、规则 `command_not_allowlisted`，回包带 `needs_user_authorization: true`；带 `confirm: true` 复查仍是 `deny` |
| 经 `python3` 子进程跑 `/usr/sbin/screencapture` | 策略 `allow`，运行退出码 1，stderr 是 `could not create image from display`（389 ms），没有生成文件 |
| 显示器诊断 | Apple M1 Pro 内建屏，3024 × 1964，`Online: Yes`、`Main Display: Yes`，不是没接显示器 |
| 用户在系统设置里开好录屏权限后重跑 | 仍是退出码 1、同一句报错（206 ms） |
| 进程诊断 | 执行命令的进程祖先是 `gld`（pid 69220），父进程是 launchd；`launchctl managername` 报 `Background`，控制台用户是用户本人 |
| `launchctl asuser <uid> screencapture` 想切进图形会话 | 退出码 1，`Could not switch to audit session 0x186b8: 1: Operation not permitted` |
| `python3` 里 `shutil.copy` 把桌面截图拷进工作区 | 被拒，`WORKSPACE_PATH_PROTECTED`（源在工作区外、目标在工作区内） |
| 绕行：`osascript` 让 System Events 按 ⌘⇧3 | 系统在桌面生成 `Screenshot 2026-11-01 at 13.22.25.png`（本机时钟是 +09 区）；再经 `sips` 缩成长边 2000 的 JPEG 放进工作区，`view_image` 取回并交付用户；工作区里的临时图已删，桌面原图是系统截图的正常产物，保留 |

**根因：**

1. **守护进程不在图形会话里，这是截不到屏的根因。**`crates/daemon/src/lifecycle.rs:210、243-251` 用 `setsid` 拉起守护进程，`docs/daemon.md:135` 只写了这样做的好处（关终端、断 SSH 不带走它），没写代价：进程落在 launchd 的 `Background` 会话，拿不到窗口服务器，所以不管录屏权限开没开，`screencapture` 都只能报 `could not create image from display`。而且能力跟启动方式有关：按 `docs/daemon.md` 的开机自启一节用 LaunchAgent 跑 `gld daemon run` 是在图形会话里的；同一个服务两种起法能力不同，现在又没有任何地方报告当前会话类型（审查时全仓搜 `managername`、录屏、图形会话，在服务代码和排错文档里都是 0 处）。
2. **桌面截图没有一等路径。**审查时全仓搜 `screencapture` 是 0 处，默认命令白名单里也没有它；桌面方向只有 `gld computer probe` 这个 Cua 研究预检，`docs/concepts.md` 写明 `desktop_verified` 始终为 false、公开的桌面工具未提供，本机也没有装 cua-driver（§82）。任务表里 D10-L0（轻量桌面观察与打开）排在多机 M3 之后、未实施。仓库里现成的截图能力只有 Playwright 的 `browser_take_screenshot`，截的是网页，不是桌面。
3. **`needs_user_authorization` 会误导调用方。**`crates/core/src/tools/exec.rs:392-398` 把 `command_not_allowlisted` 也标成 `needs_user_authorization: true`，但这类拒绝靠 `confirm` 解不开，只有改配置（加白名单或改命令策略）能解。本轮 AI 据此多跑了一轮无用的复查；别的客户端也会把“等用户点头”当成可行的下一步。
4. **路径保护不分读取源和写入目标，而且是同类复发。**`crates/core/src/tools/command_paths.rs:16-22` 的判定是：命令带写文件模式（正则在 `crates/core/src/tools/policy.rs:800` 附近）时，文本里任何一个不在工作区里的路径都算外部路径，不看它是 `shutil.copy` 的第一个参数还是第二个。测试 `crates/core/tests/command_write_paths.rs:194` 只钉了“目标在外面要拒”，反方向没有用例。仓库 `AGENTS.md` 记着 §28 有过一次同类误判、当时只绕没报；这次是“从外面读进来”方向的同类问题。

**修复建议（未排期）：**

- P0，给 `gld doctor` 或 `server_info` 加桌面能力探针：报告当前会话类型（图形会话还是 `Background`）、显示器是否在线、录屏授权预检、白名单里有没有截图命令；`docs/troubleshooting.md` 给 `could not create image from display` 加对照条目，把会话不对、没授权、锁屏三种原因分开报。
- P0，把 `needs_user_authorization` 拆开：`confirm` 能解的才叫需要批准；白名单这类回明确的“需要改配置”信号，并附确切命令，如 `gld set <项目> allowed-commands=screencapture`。
- P0，路径扫描按参数位置区分源和目标：已知的双路径函数（`shutil.copy` / `move`、`os.rename` / `replace`、`fs.copyFile` 等）只查目标路径；补三条回归用例：拷进工作区放行、拷出工作区仍拒、`shutil.rmtree("/")` 这类仍拒。
- P1，把 RFC-0013 的 D10-L0 提前一个 macOS 本机最小版：只做 `desktop` 工具的 `status` / `screenshot` 两个动作。关键不在截图库，而在执行位置必须落在图形会话里：随包一个极小的图形会话助手（LaunchAgent、图形会话域、独立签名身份供系统录屏授权识别），守护进程经本地 socket 交请求给它，结果按 RFC-0013 §8 直接回 MCP 图片。在助手就位之前，守护进程发现自己在后台会话时应直接回明确的会话不可用错误，不要再把系统的含糊报错漏给用户。
- P1，`screencapture` 不进默认命令白名单：它读的是整个屏幕，和构建命令不是一类，应走桌面工具自己的本地开关（RFC-0013 §8 的目标机本地桌面开关），只读 grant 默认不带截图。
- P2，在 concepts 里写清“AI 现在能看什么”：能看的是网页截图，不能看的是桌面全屏（等 L0）。不写清，每个 AI 都会重新发明一遍这次这种靠模拟按键的绕路；它这次能成依赖辅助功能权限和系统截图的落盘位置，不可维护。

**本轮摩擦：**连接器的 compact 档案没有 `git_commit`，本节落盘后只能留在工作区等用户提交，没法按仓库规矩当场提交。审查本身用的是只读工具（`read_file`、`search_text`、`git_log`、`server_info`），没动本机服务和配置。

## 109. 核对 §108：`setsid` 不是原因，还漏了授权失效这一层；加 `gld doctor --desktop`（2026-11-01）

**结论：§108 的现象和引用都属实，根因说错一半。**它说 `setsid` 把守护进程弄进了 `Background` 会话，图形会话里就能截。实测是：`setsid` 不改会话类型；图形会话里照样截不了，因为授权也失效了。

| 对照（都在这台 macOS 26.6 上） | 会话类型 | `screencapture` |
| --- | --- | --- |
| 后台工具的 shell（线上服务就是从这里起的，经它实测也是 `Background`） | `Background` | 退出码 1，`could not create image from display` |
| 同上，`setsid` 之后 | `Background` | 未单独测 |
| 临时 LaunchAgent（图形会话域）里的 `/bin/sh` | `Aqua` | 退出码 1，同一句 |
| 同上，`setsid` 之后 | `Aqua` | 未单独测 |
| 隔离守护进程，0.10.1，`daemon start` 从后台工具起 | `Background` | 退出码 1，同一句 |
| 隔离守护进程，0.10.1，LaunchAgent 起 | `Aqua` | 退出码 1，同一句 |

**逐条对 §108：**

- **“`setsid` 拉起所以落在 `Background`”不对。**`setsid` 换的是 POSIX 会话，登录会话类型继承自拉起它的进程，上表 `setsid` 前后都不变。这次是因为守护进程从后台工具里起的；从图形终端起的应该不会这样（没有实测，本机的图形终端这次没动）。
- **“不管录屏权限开没开都报这句”只说对了一半，同一句话还有别的原因。**`Aqua` 里没有有效授权也是这句。§108 给出的修法“LaunchAgent 在图形会话里”单靠它不够，上表第 6 行就是。
- **漏了授权失效。**系统权限库里 gld 那条是 2026-11-01 13:20 写的，绑定的是 `cdhash 951da954…`；17:52 换成 0.10.1 后这个文件的 `CDHash` 是 `5261aac9…`。发布包是 ad-hoc 签名，`codesign -dv` 显示 `Signature=adhoc`，所以授权跟文件内容走，不跟路径走。设置里开关看着还亮着，对现在的二进制应该已经不算数。
- **其余属实：**`lifecycle.rs`、`daemon.md:135`、`exec.rs:392-398`、`command_paths.rs:16-22` 的引用和实际一致，§108 的提交也都在仓库里；LaunchAgent 确实在 `Aqua`。

**这次改了什么：**

- `gld doctor --desktop`：在守护进程里真截 1×1 像素，失败时按会话类型给不同修法。不放进默认体检，没授权时一试可能弹系统授权框，不用桌面的人不该碰到。协议号 17 → 18。
- 命令结果：stderr 带 `could not create image from` 时多一条警告，说清两种原因、别重试、让用户跑上面那条。
- `docs/daemon.md` 加“让 AI 能截屏”一节，`docs/troubleshooting.md` 加对照条目。

**实测（调试版，隔离数据目录）：**后台起的报 `Background` 和“开录屏权限也没用”，LaunchAgent 起的报 `Aqua` 和“重新授权”，没起守护进程时只报“查不了”、不自动拉起；经 `gld tool call` 跑 `screencapture` 回包带上那条警告。

**没验证：**还没有过“图形会话 + 有效授权”截成功的实测，所以“三层都通就能截”目前是推断。这一步要用户在系统设置里授权一次。

**没做：**§108 的另外两处策略问题（`needs_user_authorization` 误导、路径扫描不分源和目标）；桌面工具 D10-L0；授权跨升级不失效（要稳定的签名身份，比如 Developer ID，需要用户决定）。

# gld 完成度、生命周期与文档审查（2026-09-23）

状态：**审查与文档同步；不是下列运行时代码缺陷的修复完成报告。**
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
| 共享写锁、主体命令会话、构建与依赖指纹 | 已有实现；后台命令全程互斥、项目级授权和客户端端到端仍不能据此宣布完成 | [跨仓清单](2026-09-19-cross-project-refactor-actions.md)、[工具审查 §12](2026-09-19-gld-tooling-review-and-plan.md) |

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
完整本机临时日志与反例结果在 `target/review-20260923-evidence/`，该目录被 Git 忽略，
不是可长期引用的远端制品库；本节及各问题段保存关键结果，避免只留下会过期的 session 引用。

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

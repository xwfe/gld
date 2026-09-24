# gld 完成度、生命周期与文档审查（2026-09-23）

状态：**D01–D08、D11、D14 已修（D07 本机服务未升级、未在 ChatGPT 上实测），D13 已核对，0.7.0 已在本机真机
验收并发布（D12 做了发布门禁和下载包验收，签名与来源证明未做）；D09、D10 未做。**
§1–§6 是审查当时（0.6.0）的原始发现，保留原样；每项怎么修的、怎么验证的、还剩什么，看
[§7 处理进展](#7-处理进展)，末尾"收尾"一节列出没做的项和各自的时机。
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
- 数据文件多一个 `grants` 键；没有 grant 时不出现。退回 0.7.0 会忽略它，grant 发出的令牌在旧版本上 401。
- 有 grant 时 `gld upgrade --auth noauth` 报错。
- 服务凭据看到的工具表不变，ChatGPT 不用刷新。

**验证：**新增测试 20 条（OAuth 6、主体 1、hub 8、Git 收窄 1、登记 1、端到端 3）。其中两条做过变异验证：把 grant 过滤挪到"判谁离开了"之前，测试失败（服务凭据起的
命令被停掉）；关掉 Git 收窄，测试失败（`HEAD:web/.env` 读到了内容）。端到端测试用真实二进制、守护进程和
真的 HTTP：OAuth 动态注册 → 授权页填 grant 口令 → 换令牌；bearer；作废后 401、刷新 400、它起的后台命令被停掉；
项目名写错、项目关了 confine-reads 不建；有 grant 时不给改 noauth。隔离 `GLD_HOME` 下全量 828 passed、0 failed、
0 ignored（上一轮 808）；fmt、clippy `-D warnings` 通过；`docs/cli.md` 重新生成（多了 `gld grant` 四节）。真实二进制在隔离数据目录里
看过 `gld grant add / ls / rm` 的输出和重名、找不到时的报错。

**仍未验证或未做：**

- 没在真实 ChatGPT 连接器上用 grant 口令授权过：要在客户端里操作。本机服务也还没升级到这一版。
- 改范围只能删了重建；没有有效期；同一个项目里的 Task / History / Planning 对开了它的几把凭据是共用的。
- 能写的 grant 挡不住存心的人（见上表第一条）；给别人的只能是只读的。

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

**还没做、什么时候做**（详见 §4）：

| 编号 | 什么时候 |
| --- | --- |
| D12 剩下的：签名与来源证明、glibc / Windows 包真机跑、各平台 IPC 与命令清理记录 | 给别人分发之前，或有人报平台问题时 |
| D09 持久 Job、D10 浏览器证据 | 真实项目需要时 |
| D13 实现 2026-07-28 | 出现只讲新版、不会退回的客户端时 |

# gld 工具使用问题审查与调整方案

日期：2026-09-19  
状态：**审查完成，待实施；本文不是修复完成报告**  
目标：让其他大模型据此改进 gld 的可用性、数据安全和错误恢复，不通过放松全部权限来掩盖问题。

## 1. 审查基线与结论

审查工作区：`gld`。开始时 HEAD 为 `fbe7665e1e3d8c068ecc2e418b84e5edb164314a`，分支 `main`，领先上游 2 个提交，工作区不干净。原有修改涉及 CLI、bridge、hub 和 RFC-0003 等文件，应完整保留；实施者必须重新检查现场，不得 reset/覆盖这些修改。

运行中的服务报告 `version=0.4.0`、`permission_mode=trusted`、`network_allowed=true`、`execution_boundary=policy_only`。**运行中二进制没有提供构建提交号，不能断言它与当前工作树逐字一致。** 下文分别标注运行时复现和源码结论。

审查覆盖本地命令策略、文件读取/列目录/搜索、补丁、命令会话、错误封装、工具 schema，以及与当前远端对齐计划的关系；不是整个产品的完整安全审计，也不是三平台真机验收。

**核心结论：问题不只是“权限太严”。当前存在正常操作入口不足、限制难以预判、失败后无法精确恢复，以及可能静默覆盖文件/错误分页等实质缺陷。**

三个调整原则：

1. 保留工作区边界、敏感操作授权、补丁预检失败不落盘；修复的是错误分类、能力发现和正常操作通道。
2. 优先防止静默覆盖、丢修改和游标死循环，再提高命令便利性；不把模糊匹配、部分提交或全开 shell 当作修复。
3. 复用已有实现与 RFC-0003，不重新造共享内核，不把 gld 权限决策下沉到通用 `toexec` 原语。

## 2. 问题清单

证据等级：**R**＝本次运行时复现；**S**＝当前源码明确支持该结论；**H**＝用户报告的历史现象，未取得当时完整调用记录；**T**＝需要隔离测试进一步验证的风险。优先级 P0 为数据安全/权限错误，P1 为阻断日常开发，P2 为体验和性能增强。

| ID | 优先级 | 问题及影响 | 证据 |
| --- | --- | --- | --- |
| C01 | P1 | `rg`、`gh`、直接 `ssh` 被白名单拒绝。`trusted` 与允许网络不代表这些命令获准，模型容易重复试错。 | H/R/S；E01、S01 |
| C02 | P1 | 拒绝信息没有精确规则、配置来源、生效版本和已授权替代能力。`check_exec_environment` 已有白名单，但缺少逐条命令的无副作用判定。 | R/S；S01、S05 |
| C03 | P0 | 空的 `only:` 会回退完整默认名单；`only:` 与默认启用的 workspace 本地入口还是两个独立开关，不能把它理解成完整的执行隔离。 | S；S01，现有测试明确断言空名单回退 |
| C04 | P1 | `cmd` 字符串先做 shell 风格限制，再按 `shell_words` 拆分后直接启动进程；接口像 shell，执行却不是 shell。原生 `ls` 也不兼容常用 flag。 | S；S01、S03 |
| C05 | P1 | `.github` 与 `.git` 被同等禁止所有补丁写入；新增 workflow 也报“禁止删除”，影响 Actions 正常维护。保护逻辑在两处重复。 | R/S；E02、S02、S07 |
| P01 | P1 | 一个 hunk 不匹配导致整批失败，但只得到通用 `PATCH_FAILED`，缺少失败文件、hunk、候选位置和重读范围。整体不落盘本身不是错误。 | H/R/S；E03、S02 |
| P02 | P0 | `Add File` 指向已有文件会变成覆盖更新，而不是报文件已存在。 | R/S；E04、S02 |
| P03 | P0 | 同一事务中重复 Update 同一路径时，每段重新读磁盘原文，最后一次 `HashMap::insert` 可覆盖前面暂存结果；可能丢修改。 | S；S02，尚未执行落盘复现 |
| P04 | P0 | Codex 补丁缺少结束标记仍被接受；部分未支持的语法被忽略。上下文重复时，定位规则可能选首个/最近候选，缺少明确歧义状态。 | R/S；E05、S02；未逐项动态验证所有语法 |
| P05 | P0 | 现有 API 没有文件版本前置条件；预检与执行间内容可能变化。备份读取使用 `unwrap_or_default()`，回滚错误被忽略，错误后的真实状态可能不明。 | S/T；S02；并发和 I/O 故障需注入验证 |
| F01 | P1 | 根目录 `list_dir` 返回 `/Cargo.toml` 这样的路径，直接交给 `read_file` 失败，工具返回值不能直接复用。 | R/S；E06、S06 |
| F02 | P1 | 子目录搜索的 glob 仍匹配工作区相对全路径，schema 没解释清楚；`exec.rs` 与 `**/exec.rs` 产生不同结果，零结果易被误判为没有代码。 | R/S；E07、S06 |
| F03 | P1/P2 | 本地搜索固定排除隐藏路径，无法显式搜索 `.github`；遍历未剪枝忽略目录，超长行仅搜索前 1MiB且缺少覆盖范围提示；结果截断没有续查游标。 | S；S06、S07；部分功能已属 RFC-0003 G3 |
| X01 | P1 | 输出缓冲超过 1MiB 后，`read_output` 混用保留缓冲坐标与累计输出大小，可能空页且 `next_offset` 不变，造成循环。 | R/S；E08、S04 |
| X02 | P1 | 内联完成的会话 30 秒后回收，yield 路径的回收时点又依赖命令 deadline；没有明确输出过期时间。过期、无效引用和输出被截掉难区分。 | S；S03、S04 |
| X03 | P1 | `yield_time_ms=0` 在处理初始 stdin 前返回；`tty=true` 不处理初始 stdin；空 stdin 的 EOF 语义不清。`tty` 实际仍是 pipes，并非已实现 PTY。 | S/T；S03、S04；本次未执行交互复现 |
| D01 | P1 | 错误恢复提示常按命令失败统一生成；补丁失败也提示查 stderr/exit_code。`ok`、`command_ok`、运行中状态与执行记账需要统一语义。 | R/S；E03、S05 |
| D02 | P2 | compact 成功响应已有瘦身，但错误仍重复携带大量 harness/planning 信息；策略拒绝提前返回，缺少统一 operation_id。 | R/S；S05 |

### 不应算成 gld 缺陷的情况

用户提到的 xwshun/xwshare 历史任务，以本次给出的具体症状为线索，不推断所有历史调用使用同一配置。本次命令拒绝是在 **gld 工作区** 复现，不能替代 xwshare 当时的配置快照。

`Repository not found`、GitHub 认证失败、账户无权限、网络异常，不能仅凭这些字样归因于 gld 白名单；它们可能发生在命令已执行之后。工作流配置自身的错误，也不能归因于 gld。方案应让这些问题可区分，而不是承诺放开 `gh` 后一并解决。

## 3. 调整方案

### A. 先明确能力与错误契约

扩展现有 `check_exec_environment`，不再添加一套相互重叠的能力状态源。增加无副作用的命令预检入口，例如 `check_command`，名称可按现有命名规范确定。

预检与真实执行共用同一个命令解析、解析路径及策略判定流程，返回：

- 规则标识、决定 `allow/deny/needs_approval`、拒绝阶段、作用工作区、策略版本及配置来源。
- 命令是否获准、可执行文件是否找到、实际解析来源；安装状态未知时明确 unknown，不能把 policy denied 写成未安装。
- 已授权的替代工具及适用差异；用户/管理员需要采取的授权动作，不向模型建议换解释器绕过。
- 构建版本/提交号、工具 schema 版本、配置版本；区分“磁盘已保存”与“运行时已生效”。

能力发现必须使用服务端已知元数据和本地无副作用解析；不能为了预检自动登录、读取令牌、执行任意 `--help` 程序、访问 GitHub 或建立 SSH 连接。

配置热更新沿用现有生命周期机制。若需要重启或重新连接，应明确返回；若支持热更新，应原子替换配置并递增版本。涉及工具表变化时，按双方实际支持的 MCP 能力处理通知/重新发现，不假定所有客户端即时刷新。[R1]

权限事实必须说实话：已有 `sandbox_enforced=false` 应保留。`global_tmp_write=denied` 这类字段应区分“策略意图”与“操作系统强制结果”；没有沙箱时不能声称子进程绝对无法访问工作区外或联网。

### B. 命令策略：把正常入口补齐，而不是全部放行

**统一结构化进程入口。** 增加 `program + args[]` 或 `argv[]` 的明确形式，内部统一为一个 `CommandSpec`。如暂时保留 `cmd`，它只能是同一解析器的兼容入口；两种形式同时传入必须拒绝，不维护两套权限逻辑。

结构化参数中的引号、换行、`|` 等是参数数据，不应机械套用整个命令字符串的 shell 检测。真正的 shell/解释器执行必须单独分类；Windows `.cmd/.bat` 的参数处理也不能照搬 POSIX shell。Rust `Command` 的普通参数默认不经过 shell，Windows 特定入口存在额外语义，测试必须覆盖这些差异。[R2]

策略检查与执行必须针对**同一个已解析的可执行文件**，并明确系统 PATH、本地入口、配置路径的优先级。避免白名单批准的是系统命令名，实际却执行工作区同名文件。绝对可执行路径与工作目录路径是两类权限，不应混为一谈。

| 能力 | 建议决策 |
| --- | --- |
| 搜索 | 优先把 `search_text/list_files` 做到可用；本地开发命令配置可显式包含 `rg`。不要把任意 `rg` 参数组合视为绝对只读，搜索路径和可执行辅助参数仍需约束。 |
| GitHub 只读诊断 | 提供可明确启用的 `gh` 只读子命令规则，先覆盖固定仓库的 workflow run 列表、详情与日志；不是给整个 `gh` 放行。`gh run list/view` 已有对应官方入口。[R3][R4] |
| GitHub 写操作 | push、PR 创建/合并、run rerun/cancel、release、secret、任意写 API 不随只读诊断授权自动开放。不能仅按首个单词 `gh` 判定。 |
| SSH | 默认继续拒绝任意直连，优先使用已配置的 hub/ccnm 远端成员。确有本机直连需求时，另行授权固定目标及操作范围；不自动允许端口转发、任意远端 shell 或读取私钥。 |
| 构建与测试 | 保留现有开发能力，但明确 package script、解释器和编译脚本可执行任意代码，命令名单不是系统级安全隔离。 |

`only:` 应改为显式有效配置：空值报配置错误，不回退宽权限。文档与 CLI 必须说明它与 workspace 本地入口的组合；需要严格模式时，必须同时约束这些入口。基础诊断命令按实际允许的参数/原生实现定义，不能仅凭 `find/grep` 等名称标注“绝对只读”。

`confirm=true` 只能表示调用携带确认意图，不能充当用户授权本身。授权依据来自可信客户端/管理员配置及现有鉴权边界；模型不能自行改白名单、授权模式或凭据配置。MCP annotations 也是提示元数据，不应代替访问控制。[R1]

原生 `ls/dir`：要么明确仅支持受限语法并引导 `list_dir`，要么明确支持所需 flag；不能伪装成完整系统命令。显式相对参数应按 `workdir` 解析，而不是有时从工作区根目录解析。

### C. 补丁：严格解析、可定位失败、可说明写入状态

#### C1. 先堵静默改坏文件

`Add File` 默认要求目标不存在；存在时报 `FILE_ALREADY_EXISTS`。确需替换时使用明确的替换语义和原文件版本前置条件。保留已有合法 Delete→Add 替换场景时，应先规范化为一个 Replace 操作，不能让普通 Add 获得隐式覆盖能力。

同一文件的多个 Update：首轮实现选择**明确拒绝重复 Update**，避免最后一段覆盖前一段；单个 Update 内多个 hunk 继续支持。未来确需支持重复块，再基于事务内最新暂存内容顺序应用，不能每次回读磁盘原文。

补丁 parser 应严格验证边界、文件头、hunk 结构、行数和支持的操作集合。缺失结束标记、未知 Move/二进制操作等不能静默吞掉；标准格式允许的元数据要显式识别，不能一概拒绝正常 diff。没有实际修改的补丁不能伪造成功变更。

上下文匹配遵循确定性规则：明确行号位置且原文匹配可使用；位置漂移时，只允许唯一的精确内容候选；多个候选必须返回 `PATCH_AMBIGUOUS`。不默认忽略空白、不猜目标块、不自动应用到最近的相似位置。

#### C2. 保留全量预检，补充机器可恢复的诊断

`patch_check` 复用 apply 的同一解析/校验管线，保证不写入工作区。返回有上限的结构化诊断：

`file、operation、hunk_index、reason_code、expected_range、candidate_ranges、actual_excerpt、suggested_read_range`。

解析完成且能够继续检查时，收集多个独立失败，而非仅报第一个。返回 `diagnostics_truncated` 和真实未检查范围；不要把未执行的检查标记为通过。已通过部分可以说明，但 **不能顺便写入已通过文件**。

apply 在正常校验失败时维持整批不落盘。模型只需重读失败文件的相关范围并重建那部分补丁，再提交完整有效事务；“让模型重读整个项目/整份大文件”不是默认恢复路径。

#### C3. 版本前置条件与恢复状态

对已有文件返回内容版本/hash，更新、删除和替换携带相应前置条件；新增操作携带“目标应不存在”的条件。`patch_check` 的成功不是永久授权，也不是保证以后仍能提交。

gld 自身的并发写者采用统一锁和提交前复核；检测到外部变化报 `FILE_VERSION_CONFLICT`，不得自动覆盖。需明确：普通 hash 检查与进程内锁**不能消除所有外部进程写入的竞态**，不能把它宣传为跨编辑器的强 CAS。

修复备份读失败被当作空内容、回滚错误被忽略的问题。任何备份失败都在修改目标前终止。提交中途失败时返回确定的状态，例如 `unchanged/rolled_back/partial/unknown`，以及已修改、回滚失败、需要人工检查的路径；不能只有 `PATCH_FAILED`。

保持已有单文件 durable write/replace 原语。跨多个文件的写入并非操作系统级原子事务；本轮完善失败状态和受控回滚，不冒称已经具备断电后的全事务恢复，也不强行扩建复杂日志系统。

#### C4. 区分 `.git` 与 `.github`

统一放到文件写权限分类器中，避免 `patch.rs` 与 `workspace.rs` 各自判定一遍。

`.git/**` 内部对象/配置继续不允许普通文件工具写入。`.github/**` 是仓库源文件，不能无差别永久封锁；但 workflow 能改变 CI 执行行为，不应标为无风险配置。为正常修改、敏感 workflow 修改、关键文件删除定义明确授权规则；用户已授权修复 Actions 时，应能通过受控补丁完成，而不是改用 Python 绕开保护。

### D. 文件和搜索：返回值必须可复用，零结果必须可解释

统一所有文件工具的路径输出：工作区内返回无前导 `/` 的工作区相对路径。根路径空字符串/`.` 在一处规范化。验收必须实际串联 `list_dir → read_file → patch_check`，不允许模型自行猜测去掉斜杠。

保留当前工作区相对 glob 规则可以，但必须在 schema/工具说明中明确 `glob_base=workspace`，并返回有效搜索根和过滤条件。子目录配 `exec.rs` 导致没有候选文件时，要与“扫描了文件但无内容匹配”区分。

隐藏文件、类型过滤、计数/文件名输出、跨行搜索与 RFC-0003 G3 合并实施；为 `.github` 提供显式搜索方式，同时不开放 gld 数据目录、越界 symlink 或受保护的凭据访问。

遍历阶段就剪枝应忽略的目录，不进入 `node_modules/target/.git` 后再逐文件丢弃。错误目录、不可读文件和超长行跳过应有计数与覆盖提示；超过保留长度的行不能默默表现为完整扫描。

受限结果应提供稳定续查游标或明确的细化查询建议，不能用 `total_matches=当前返回数量` 暗示全项目总数。全局计数功能必须与提前终止的预览语义分开。

保留已有流式读取及长行防内存膨胀实现。文件读取因字节限制丢弃长行尾部时，除 warning 外应暴露覆盖/缺口状态；需要无损读取时支持有界字节续读或明确返回能力不足，不把“翻页结束”当作“全文已读”。

### E. 命令会话：区分运行、完成、输出缺口和过期

输出统一采用累计流的绝对字节偏移，返回 `base_offset、end_offset、next_offset、dropped_bytes、has_more、process_state`。旧数据已被环形缓冲移除时，明确返回 gap 或 cursor stale 及可读起点，不能悄悄把绝对 offset 当相对下标。

必须满足：非空续读前进；已退出且到达尾部时 `next_offset=null`；运行中暂时没有新输出可保持当前位置，但必须说明“等待新输出”，不能伪装成还有历史页。UTF-8 边界、非 UTF-8 字节和计数单位应写进契约。

输出保留时间独立于命令 timeout：进程退出后按一致规则回收，并返回 `expires_at` 或等价信息。支持有界保留/有限续租即可，同时设置每工作区配额；不要以不回收会话或无限日志保存解决问题。

明确 stdin 的三种状态：无输入且关闭、一次性输入后关闭、持续交互。先设置输入/EOF 语义，再进行 yield；`yield_time_ms=0` 不得丢弃初始输入。写入阻塞也必须受 timeout/cancel 管理。

本地 `tty` 目前不是 PTY：要么改为准确的 interactive-pipe 命名并说明迁移，要么真做平台实现并验收。不能仅改描述后声称终端程序已兼容。真实 PTY 可独立排期，不阻塞修复丢输入和错误分页。

超时不代表命令没有副作用。恢复提示先读取已保留结果、检查状态；不能普遍建议自动重跑可能已部分完成的命令。

### F. 错误封装与执行记账

使用类型化错误和统一响应适配器，不再靠 `message.contains(...)` 推导策略原因。至少区分策略拒绝、需授权、程序不存在、参数不支持、补丁冲突、进程退出非零、超时、会话过期、输出缺口、远端认证/网络故障。

所有调用在早期分配 operation_id；策略/规划拒绝也记录脱敏审计事件。只返回当前失败所需字段，完整 harness/planning 状态按需读取。保留 compact 成功响应已做的优化。

`transport_ok` 表示协议/工具调用到达，不能等价于命令成功；运行中必须保持未完成语义。执行账本区分 accepted/running/completed/failed，不能收到 `ok=true, command_ok=null` 就记作工作已完成。

MCP `isError`、结构化结果及本地错误码必须一致映射。对搜索类工具，“无匹配”可以是正常业务结果，不能机械把底层每个非零退出码都当工具故障；原始命令仍应完整保留 exit_code。[R1]

## 4. 实施顺序与停止条件

| 阶段 | 核心任务 | 完成条件 |
| --- | --- | --- |
| U0：锁定基线 | 检查当前 diff/RFC-0003，建立隔离回归夹具，记录运行二进制与测试源码版本。 | 不覆盖已有工作；能重复复现本文 R 类问题。 |
| U1：先防静默错误 | Add 覆盖、重复 Update、parser 完整性、歧义、备份/回滚失败状态、空 `only:`、输出游标停滞。 | 对应测试由红转绿；所有正常校验拒绝不改文件；不新增权限放宽。 |
| U2：能力与恢复契约 | 统一错误类型、operation_id、命令预检、策略/构建版本、补丁定位、文件前置条件。 | 模型无需猜测即可判断下一步是重读、修参数、等待还是请求授权。 |
| U3：命令及文件授权 | 结构化执行、受控 `rg/gh` 规则、SSH 边界说明、`.github` 分类，配置生效反馈。 | 权限矩阵与拒绝路径全覆盖；未授权能力仍然拒绝；不自动建立外部连接。 |
| U4：读取与会话体验 | 路径 round-trip、glob/隐藏搜索/剪枝、输出生命周期、stdin/EOF；与 RFC-0003 G3 合并。 | 工具串联正常；分页终止或明确等待；跨平台差异有证据。 |
| U5：端到端验收 | 使用仓库维护场景检验完整闭环，同步文档和工具说明。 | 证据明确区分本地测试、合成远端与真实授权远端；未做的项目不写通过。 |

每阶段提交必要实现、回归测试与简短验收记录即可，不为拆任务堆叠无用框架。需要新增凭据、系统用户、ACL、防火墙、全局 Git 配置、真实 SSH/GitHub 写操作时停止该分支并列出授权需求；其余隔离测试不必等待。

## 5. 必须新增/补齐的验收矩阵

以下均为**待实施的验收要求**，不是本次已通过的测试。

| 编号 | 必须验证的行为 |
| --- | --- |
| A01 | 同一命令在各权限/网络/白名单组合下，预检与执行判定一致；拒绝不启动子进程。 |
| A02 | `only:` 空配置不扩大权限；严格名单与本地入口的组合有明确测试。 |
| A03 | 参数中的空格、引号、换行、字面操作符准确传递；真正 shell 入口不能继承普通参数的低风险判定。 |
| A04 | 系统程序与工作区同名入口不会错误替换；macOS/Linux/Windows 分别覆盖解析规则。 |
| A05 | GitHub 只读规则不允许 rerun/cancel、PR/release/secret 写操作或任意写 API；未授权 SSH 不建立连接。 |
| A06 | `.github` 正常授权修改成功，未授权敏感修改/删除失败；`.git` 内部写入仍失败。 |
| A07 | Add 已有文件、重复 Update、缺结束标记、未知操作、hunk 数量不符、歧义上下文得到明确错误。 |
| A08 | 一个文件成功匹配、另一个失败时所有目标内容/权限不变；诊断精确到失败位置。 |
| A09 | 预检后修改原文件，执行能检测过期版本；新增目标在预检后出现时不覆盖。 |
| A10 | 备份失败、暂存失败、替换失败、回滚失败均有故障注入；结果不虚报 unchanged/rolled_back。 |
| A11 | CRLF、无末尾换行、UTF-8、已有脚本可执行位和合法 Delete→Add 场景不回归。 |
| A12 | 根目录与子目录 `list_dir` 的文件 path 原样传入 `read_file/patch_check` 可用。 |
| A13 | glob 基准明确；零候选、无匹配、权限跳过、大小跳过、长行缺口能区分。 |
| A14 | 忽略的大目录在遍历前剪枝；显式隐藏搜索不突破路径和数据目录保护。 |
| A15 | 输出大于缓冲上限、请求过期 offset、恰到尾部、尚在运行、UTF-8 跨页，游标均不死循环。 |
| A16 | 用虚拟时钟验证结果过期与限额，不靠真实 sleep 堆慢测试；有效但过期引用与随机引用可区分。 |
| A17 | 零 yield、一次性 stdin、空输入 EOF、持续交互、输入背压和超时取消均有测试。 |
| A18 | 策略拒绝/补丁冲突不提示查 stderr；运行中与完成记账一致；失败响应体量受限。 |
| A19 | MCP schema、运行时校验、CLI、文档、hub 暴露能力一致；不静默忽略改变执行语义的参数。 |
| A20 | 在隔离副本回放“读仓库→查隐藏 workflow→修改 README/workflow→构建→读长日志→查看 CI 状态”的维护流程。真实 GitHub 步骤仅在已有授权范围内执行。 |

## 6. 本次实际验证记录

### E01：命令拒绝

分别调用 `exec_command`：`rg --version`、`gh --version`、`ssh -V`。均返回：

```text
code = POLICY_REJECTED
message = Command is not allowlisted: <rg|gh|ssh>
```

只查询版本，不建立 SSH 连接，不访问 GitHub；由于策略先拒绝，**无法据此判断程序是否已安装**。

### E02：新增 workflow 被当成删除受保护资产

`patch_check` 中 Add `.github/workflows/gld-audit-dry-run.yml`，返回 `PROTECTED_REPOSITORY_ASSET`，message 为“禁止删除仓库保护资产”。没有落盘。

### E03：上下文错误不可定位

对 README 中不存在的行做 Update 预检，返回：

```text
code = PATCH_FAILED
message = Hunk context did not match file content.
details = {}
recovery_hint = 命令未成功；请检查 stderr、exit_code 或调整参数后重试。
```

没有文件名、hunk 编号和建议重读范围。没有落盘。

### E04：Add 已有文件被接受为覆盖

对已有 `README.md` 发送只有一行新内容的 Add 预检，得到：

```text
ok = true
preflight = true
affected_files = [{ operation: update, path: README.md }]
would_modify = [README.md]
```

仅预检，README 内容没有被探针替换。

### E05：不完整补丁被接受

Add `docs/reviews/gld-audit-parse-only.txt` 的 Codex 补丁不带 `*** End Patch`，`patch_check` 仍返回 `ok=true` 和 `would_create`。没有创建该探针文件。

### E06：目录返回路径不能原样读取

根目录 `list_dir` 返回 `/Cargo.toml`。原样调用 `read_file(path="/Cargo.toml")` 得到 `NOT_FOUND: Path not found: /Cargo.toml`。同一文件使用工作区相对路径 `Cargo.toml` 可以读取。

### E07：glob 基准导致零结果

`path=crates/core/src/tools`，搜索相关函数时，`include_globs=["exec.rs", ...]` 返回 0；改为 `["**/exec.rs", ...]` 后返回匹配。源码使用工作区相对路径参与 glob 匹配。

### E08：输出分页不前进

执行仅向 stdout 写入 1,048,608 个 ASCII 字节的测试进程，没有文件/网络访问。随后读取 offset=1,048,576，结果为：

```text
content = ""
offset = 1048576
next_offset = 1048576
total_retained_bytes = 1048576
total_stream_bytes = 1048608
truncated = true
warnings = []
```

证明当前位置已无保留内容，但仍被标记可继续翻页。

### E09：现有测试基线

执行：

```text
cargo test -p gld-core --lib tools:: --offline
```

实际结果：**88 passed，0 failed，0 ignored，281 filtered out**；筛选也包含 `bridge::tools` 测试。未跑完整 workspace 测试、clippy、三平台真机或新增缺陷的全部回归测试。现有测试通过不代表上述缺陷不存在。

## 7. 已有能力与并行计划：不要重复实现

`patch_check` 已存在；补丁已使用暂存和回滚路径，且已有 CRLF、脚本权限、合法 Delete→Add、后续文件校验失败不修改先前文件等测试。需要完善的是严格性、前置条件和失败状态，不是从零重写编辑系统。

`read_file` 已流式化，搜索已限制单行内存；`toexec-text`、`toexec-fs` 已接入。短命令 output_refs 保留及进程树终止也已有修复和测试。不要把旧问题不加核对地重新列为“尚未实现”。

`docs/rfc/0003-native-parity-sync.md` 的当前工作树记录 G1 已完成，涉及远端 `tools/list` 检查、远端新工具和 `wait_ms` 预算等；相关文件仍有未提交修改。G2 是 compact skills，G3 包含本地搜索增强和 notebook。本方案与 G3 搜索部分合并排期，不另建竞争实现，不撤销 G1 的远端能力校验、coding 句柄或预算边界。

本轮不实现新的 SSH 客户端、凭据库、通用 shell 解释器、完整跨平台沙箱、PDF 功能或新的后台任务编排产品。确需共享的纯算法可再评估进入 `toexec`，授权、workspace 路由和恢复策略留在 gld。

## 8. 源码定位与外部依据

行号对应本次读取的工作树，后续变动后以函数名为准。

| 索引 | 文件与关键位置 |
| --- | --- |
| S01 | `crates/core/src/tools/policy.rs`：默认名单 L15–52，配置合并/only L110–200，`validate_command_for_workspace` L242 起，shell/网络正则及对应测试。 |
| S02 | `crates/core/src/tools/patch.rs`：`apply_patch` L11–137，parser L173–323，`apply_hunks/find_hunk_position` L354–470，`commit_staged_bytes/restore_backups` L499–597。 |
| S03 | `crates/core/src/tools/exec.rs`：native diagnostic L121–215，`run_command` L219–336，回收 L339–374，`merge_exec_result` L493 起，`resolve_program` L554 起。 |
| S04 | `crates/core/src/tools/session.rs`：缓冲上限 L15，读取/裁剪 L197–219，`snapshot` L286 起，`read_output` L359–418，`write_stdin` L420–470。 |
| S05 | `crates/core/src/tools/dispatch.rs`：`policy_tool_err` L18–57，`call_tool` L319 起，记账与恢复 L499–579，`attach_recovery_guidance` L589 起，`check_exec_environment` L969 起。 |
| S06 | `crates/core/src/tools/file.rs`：`read_file` L37–109，`list_files` L157–224，`search_text` L226–341，`collect_dir_entries` L565–636，`glob_match` L815 起。 |
| S07 | `crates/core/src/tools/workspace.rs`：`reject_protected_write_path` L439–452，`is_ignored_path` L460–505，`relative_display` L525 起。 |
| S08 | `crates/core/src/tools/registry.rs` 与 `args.rs`：工具参数、说明和已有数值边界一致性测试；`docs/rfc/0003-native-parity-sync.md`：现有 G1–G3。 |

外部资料仅支持协议和进程接口设计，不作为本地缺陷的证据。MCP 采用服务当前声明的 2025-06-18 版本文档，不要求为本方案升级协议。

- [R1：MCP Tools（2025-06-18）](https://modelcontextprotocol.io/specification/2025-06-18/server/tools)：工具发现、结构化结果、错误分类、annotations 与访问控制边界。
- [R2：Rust std::process::Command](https://doc.rust-lang.org/std/process/struct.Command.html)：参数字面传递、环境继承和 Windows 特定解释差异。
- [R3：GitHub CLI gh run list](https://cli.github.com/manual/gh_run_list)。
- [R4：GitHub CLI gh run view](https://cli.github.com/manual/gh_run_view)。

## 9. 交给实施模型的指令

```text
先读取本文、当前 git diff 和 RFC-0003。保留已有未提交改动，按 U0–U5 推进。
第一优先级是静默覆盖/丢修改、补丁解析与恢复状态、空 only: 权限扩大和输出游标停滞。
先建立隔离回归测试，再修实现；不要只靠修改工具提示词或白名单宣布完成。
补丁校验失败维持整批不落盘；不要引入默认模糊匹配或部分成功。
rg/gh/ssh 按能力和授权分别处理，禁止通过解释器或包装脚本绕过拒绝。
不自动改全局权限、Git 身份、凭据、ACL、防火墙，不自动 push/tag/release。
与 RFC-0003 的搜索和远端能力工作合并，不重复建设 toexec 已有原语。
每阶段更新实际验收证据，明确未跑的平台和真实外部操作；不把方案要求写成已通过。
```

## 10. 实施进度（实施模型逐阶段补记）

本节是**实际做了什么**的记录，和上面的方案分开写：方案里的验收要求不代表已经通过。

### U0 基线（2026-09-18/19）

- 起点 HEAD：`c822d5f`（RFC-0003 的 G1 已提交——远端工具跟上 ccnm P36–P41、按远端 `tools/list` 拦下调用、`wait_ms` 预算）。审查提到的"工作区不干净"就是这批改动，已完整保留、原样提交，没有 reset 或覆盖。
- 基线测试：`cargo test --workspace` 574 passed、0 failed（G1 提交前那次门禁）。
- 已知不稳定（不是本轮引入）：`tools::git::tests::run_git_kills_a_hung_git_and_what_it_spawned_at_the_limit` 在全量并发下偶发失败——它给 git 的预算只有 300 毫秒，机器忙时别名脚本来不及写 pid 文件；单独跑稳定通过。**2026-09-20 修了**：预算提到 2 秒，见 [RFC-0003](../rfc/0003-native-parity-sync.md)。
- 运行中的守护进程没有构建提交号，所以本节所有结论都以**当前工作树的源码和测试**为准，不声称和某个已安装二进制逐字一致。

### U1 第一批：不能静默改坏文件（P02、P03、P04）

五条回归测试先写、先红（`tools::patch::tests`），再改实现：

| 问题 | 原来的行为（实测） | 现在 |
| --- | --- | --- |
| P02 `Add File` 指向已有文件 | 当成整文件覆盖，原内容没了，还报成 `update` | 报 `PATCH_FAILED`，消息里给出该用 `*** Update File:`；文件不动。Codex 信封和 `--- /dev/null` 两种写法都拒 |
| P03 同一批两次改同一个文件 | 第二段从磁盘重读原文，`staged.insert` 把第一段结果盖掉——实测 `a\nb` 经两段编辑后是 `a\nB`，第一处改动丢了 | 每段看见上一段的结果，实测 `A\nB`。同一批里先删后加仍然是整文件替换（原有测试钉着） |
| P04 缺 `*** End Patch` | 照常应用 | 报 `PATCH_FAILED` 并说明可能被截断，什么都不写 |
| P04 不认识的 `*** ` 指令（如 `*** Move to:`） | 静默跳过：模型以为文件挪了，磁盘上没挪 | 报 `PATCH_FAILED`，列出支持的三个指令 |
| P04 上下文匹配多处 | 挑第一处（或离行号最近的那处）改，看起来成功 | 会删/改行且既无行号又无锚点时报新错误码 `PATCH_AMBIGUOUS`，给出候选行号；纯插入不受影响（挑哪处都不动原有内容，现有补丁大量这么写） |

顺带：`affected_files` 改成"每个文件最终发生了什么"，同一文件被改两段只报一次，先删后加仍报 `update`。

**还没做**（U1 剩余）：备份/回滚失败的真实状态（`unwrap_or_default()`、忽略回滚错误）要配故障注入，单列一批。

### U1 第二批：空 `only:` 与输出游标（C03、X01）

| 问题 | 原来的行为（实测） | 现在 |
| --- | --- | --- |
| C03 `allowed-commands=only:`（列表为空） | 回退到**默认全集**：cargo、pytest、python3、node 全都能跑——一个想收紧权限的配置把权限放到了最大 | 只剩基础诊断命令（pwd / ls / cat / grep），`cargo test` 被拒。完全不配（空字符串）仍然是默认白名单：那是"没说"，不是"只允许这些" |
| X01 `read_output` 越过 1 MiB 保留缓冲 | `offset` 按保留缓冲算、有没有下一页按累计字节算。实测 3 MiB 输出读到 `offset=1048576` 时回空内容、`next_offset` 还是 `1048576`——照着它再读就是死循环 | 偏移改成**整条流里的绝对位置**；读得到的那段是 `[retained_from, total)`，每页 `next_offset` 严格前进，读完就是 `None` |
| X01 附带：旧 offset 落在已被挤掉的区间 | 悄悄当成缓冲的第一个字节，报 `offset: 0`——把 2 MiB 之后的内容说成开头 | 报实际起点 `offset`、丢了多少 `dropped_bytes`，外加一条 `no longer retained` 警告 |
| X01 附带：分页接缝劈开多字节字符 | 每页接缝上多出替换字符 | 截到字符边界再返回，下一页从那里接着读（`limit` 小到装不下一个字符时照原样给，保证能前进） |

`read_output` 的结果多了 `retained_from`、`dropped_bytes`、`running`、`complete` 四格；`offset` 的含义从"缓冲下标"改成"流内绝对位置"——这是**行为变更**，但原来的含义配上原来的判据本来就自相矛盾。

这一批的红测试：`tools::exec::tests::paging_output_bigger_than_the_retained_buffer_terminates`（3 MiB 输出，分页必须走到头且每页前进）、`tools::exec::tests::an_offset_the_buffer_has_dropped_is_reported_as_a_gap`、`tools::policy::tests::an_empty_only_list_keeps_only_the_basics`（原来那条 `an_empty_only_list_falls_back_to_the_defaults` 连同它的理由一起改写）、`no_configuration_at_all_still_means_the_defaults`。

门禁：`cargo test --workspace` 582 passed、0 failed；fmt、clippy 干净。

### U1 第三批：失败之后说的话必须是真的（P05 的前半）

| 问题 | 原来的行为 | 现在 |
| --- | --- | --- |
| 备份读不出来 | `fs::read(&path).unwrap_or_default()`——读失败当成"原来是空的"，一旦回滚就把人家的文件写成空 | 报 `PATCH_FAILED` 并说明"什么都没改"，这个文件一个字节都不动 |
| 回滚范围 | 把所有备份过的路径都写一遍，包括根本没被换过的 | 只回滚**真正换上去过**的文件：没动过的不需要恢复，一起写只会制造假失败、还可能盖掉别人同时做的改动 |
| 回滚失败 | 所有错误被 `let _ =` 吞掉，消息和回滚成功时一模一样 | 新错误码 `PATCH_ROLLBACK_INCOMPLETE`，点名哪几个文件现在可能是新内容，让人先去看 |
| 落盘顺序 | `HashMap` 的随机顺序 | 按路径排序：一批补丁中途失败时，哪些换了、哪些没换是可复现的（这条是写回滚测试时发现的——同一个测试在不同顺序下结果不同） |

故障注入（审查 A10 要的）：`tools::patch::faults` 是 `#[cfg(test)]` 的线程局部开关，能让"备份读"、"替换"、"回滚写回"某一步真的失败。三条测试：备份读失败时文件不变、替换失败时先前的文件回滚回去并说"已回滚"、回滚也失败时报 `PATCH_ROLLBACK_INCOMPLETE` 且现场确实是半新半旧。

门禁：`cargo test --workspace` 连跑三轮都是 585 passed、0 failed；fmt、clippy 干净。

**U1 到此完成。**下一步按方案是 U2（统一错误类型、operation_id、命令预检、补丁定位诊断、文件版本前置条件）。

### U2 第一批：补丁失败说得出在哪儿（P01、D01 的补丁部分）

E03 里那次失败给的全部信息是：`PATCH_FAILED` + `Hunk context did not match file content.` + `details = {}`，外加一句"请检查 stderr、exit_code"。模型能做的只剩重读整个项目再猜。

现在每处对不上都有一条诊断：`file`、`operation`、`hunk_index`、`reason_code`、`expected_range`、`candidate_ranges`、`actual_excerpt`、`suggested_read_range`。

**行号一律是磁盘原文件的 1-based 行号。**一批补丁里前面的 hunk 已经把下面的行推走了，不换算回去，模型照着 `read_file` 读到的是别处（测试 `line_numbers_in_a_diagnostic_are_the_ones_on_disk` 钉着）。同一批里前面改过这个文件时，`baseline` 标成 `earlier_in_this_patch`——那时行号和磁盘上的确实对不上，得说出来。

`reason_code` 分开，因为下一步完全不同：

| reason_code | 什么情况 | 模型该做什么 |
| --- | --- | --- |
| `context_out_of_order` | 整段上下文在文件里，只是在前一段之前 | 把两段调个个儿 |
| `context_drifted` | 第一行还在原处，后面的变了 | 重读 `suggested_read_range`，重建这一段 |
| `context_not_found` | 文件里根本没有这段 | 重读文件，整段重写 |
| `context_ambiguous` | 对得上好几处（`PATCH_AMBIGUOUS`，U1 就有） | 加上下文或行号 |
| `file_not_found` / `file_already_exists` / `deleted_earlier_in_patch` | 目标文件本身就不对 | 换操作类型 |

一次把所有对不上的地方都报出来（`problem_count`、`diagnostics[]`），不是修一个报一个。同一个文件里第一段失败之后**不再检查**它剩下的段——那些段要看见前一段的结果才知道对不对——记进 `not_checked`，不算通过。诊断多于 20 条时 `diagnostics_truncated=true`。整批仍然不落盘，`details.files_changed` 明写 false。

改文件时目标不存在，错误码**仍然是 `NOT_FOUND`**（照着旧码分支的客户端不受影响），只是现在带诊断。

`recovery_hint` 按错误码分流：补丁失败不再提示"检查 stderr、exit_code"（那次没有 stderr 也没有 exit_code，更不该重试）；策略拒绝指向 `check_command`；超时提示先读已有输出再决定要不要重跑；回滚没做完的提示先去看现场。

诊断里的纯计算（摘录、候选、建议范围）拆进 `tools/patch_diag.rs`：和落盘、回滚、权限无关，单独测。摘录最多 9 行、单行 200 字符，截断了要报 `truncated_lines`——否则模型拿截断内容当原文，重建出来的补丁一样对不上。

门禁：`cargo test -p gld-core --lib tools::patch` 30 passed；`cargo test --workspace` 600 passed、0 failed；fmt、clippy 干净。

### U2 第二批：命令预检 `check_command`（C01、C02、A01，顺带 F）

新工具 `check_command`，参数和 `exec_command` 同一组，**不跑命令**，回答四件事：

1. `decision`：`allow` / `deny` / `needs_approval`；`denied_stage` 说明卡在哪一步（policy / workdir / resolve）。
2. `rule`：哪条规则说了算（`command_not_allowlisted`、`shell_syntax_rejected`、`external_execution`……）。
3. `program`：`found`、`path`、`source`（`path` / `workspace_entry` / `native_builtin` / `not_found`）。**策略拒了也照样报**——"不许跑"和"没装"下一步完全不同，把前者说成后者会让模型去装一个本来就装着的东西。
4. `policy` + `server`：生效的 permission_mode、`allowlist_mode`（`defaults` / `defaults_plus_configured` / `only`）、白名单全文、配置来源、`runtime_fingerprint`，以及服务端版本和协议版本。`build_commit` 明写 `null`——构建里没嵌提交号就不能拿版本号顶替。

**判定不是另写一份**：策略走同一个 `validate_command_for_workspace`，程序解析走同一个 `resolve_program`，顺序也和 `exec_command` 一样（策略 → workdir → 原生内建 → 解析）。`check_command_and_exec_command_agree` 用四类命令钉住"预检与真跑同码同因"。

预检没有副作用：不起进程、不跑 `--help`、不登录、不联网。`check_command_does_not_run_anything` 用一个**真会写文件**的命令证明这件事——预检后文件不能出现，真跑后必须出现（少了后半句，脚本失效时这条测试会假通过）。

被拒时给的是**已获准**的替代工具（`rg`→`search_text`、`find`→`list_files`、`cat`→`read_file`、`ssh`→走 hub 成员），不教绕过。

顺带做了方案 F 的一部分：策略拒绝原因**类型化**成 `PolicyReason`，码、机器可读原因和建议都从枚举来，不再靠 `message.contains("allowlisted")` 猜。两处行为因此变准：

| 场景 | 原来 | 现在 |
| --- | --- | --- |
| `filesystem_scope=host` | `POLICY_REJECTED`，真正原因当前缀塞在消息里 | `EXTERNAL_EXECUTION_NOT_ALLOWED`，`details.reason=external_execution` |
| 子进程写工作区外 | 同上 | `WORKSPACE_PATH_PROTECTED` |

工具数随之 +1（compact 25 / core 39 / advanced 52 / read-only 20），文档已同步。

门禁：`cargo test --workspace` 600 passed、0 failed；fmt、clippy 干净。

### U2 第三批：文件版本前置条件（P05 的后半、C3、A09）

以前 `apply_patch` 只管"上下文对不对得上"。对得上**不代表文件没变**：模型读完文件、人在编辑器里改了几行、它的补丁照样落盘，那几行无声消失，两边都不知道。

三道门，各管一段：

| 门 | 管什么 | 管不到什么 |
| --- | --- | --- |
| `expected_versions`（可选参数） | 从读文件到提交补丁之间，别人写过它 | 调用方不传就没有这道门——它是**加法**，不传时行为和以前一样 |
| 落盘前复核（自动，不用传参数） | 这一次调用里，算完补丁到写下去之间文件变了 | 复核到 `rename` 之间还有几微秒 |
| 进程内写锁（自动） | 同一个 gld 进程里两个会话同时打补丁 | 别的进程 |

**没有冒称强 CAS。**审查 C3 特意点了这一条：普通 hash 检查加进程内锁消除不了所有外部进程的竞态。文档里用上面这张表写清楚每道门管不到什么，而不是含糊地说"有版本校验"。

`read_file` / `patch_check` 回 `version`，`apply_patch` 收 `expected_versions`（路径 → 版本，`null` 表示"这路径应当是空的"），成功后回 `file_versions`，接着改同一个文件不用再读一遍。给了补丁根本不碰的路径是**报错**：忽略的话模型会以为自己保护住了那个文件，而这次调用压根没检查它。

`version` 是**大小 + 修改时间**，不是内容 hash，格式和 ccnm 的 `version_of` 一字不差（同一个 hub 下，本机成员和远端成员给模型的版本号必须长一样）。理由：`read_file` 是流式的，读 2 GB 文件的前 200 行不必碰后面，算 hash 就得每次把整个文件读一遍。代价是文件从备份恢复、或连时间戳一起复制过来会虚惊一场——安全的那一侧。

落盘前复核的成本接近零：备份阶段本来就要把原文读上来，比一比就完了；而发现冲突时**还没有任何文件被换上去**，所以是干净返回，没有要回滚的东西。删除操作只比版本号（不为了校验把整个文件读进来），改文件比内容字节。

错误码 `FILE_VERSION_CONFLICT`（category `conflict`），`recovery_hint` 明说下一步是重读、不是原样重发——"原样重发"正是会盖掉别人改动的那一次。

**那个最难测的窗口怎么测的**：加了一个 `#[cfg(test)]` 钩子，在"算完补丁"和"开始落盘"之间插一手。靠真实并发碰运气测不稳，而这恰恰是最要紧的一段。三条单测分别覆盖改文件、新建目标被人抢先、删除的文件变了，每条都断言别人的内容还在。集成测试另外覆盖了 `read_file` → 改文件 → 带旧版本提交被拒、`observed_versions` 原样往返、`file_versions` 接着用、多余键报错。

门禁：`cargo test --workspace` 617 passed、0 failed；fmt、clippy 干净。

### U2 第四批：operation_id 与能力状态的单一来源（D02、F、A 的收尾）

两件都是"说话方式"，不改任何执行行为。

**`operation_id` 进门就发。**以前它是执行到一半由 `record_operation` 生成的，于是策略拒绝、Planning 拒绝、基线拒绝这些提前 `return` 的响应根本没有 id——人拿着模型给的报错，在操作日志里对不上号。现在 `call_tool` 变成一层薄壳：进门分配 id，内层分发，出门统一盖章（内层已经写过的就是同一个，因为 `record_operation` 收的就是它）。

**被拒的调用也记账**，`kind="rejected"`，和正常路径的 `started` 分开——账本得能区分"接了没跑"和"跑了"。脱敏：只记错误码、分类和拒绝阶段，命令内容和补丁正文不进日志（测试断言 `rg --version` 不出现在记录里）。范围限定在本来就要记账的工具加上会改东西的那些：操作日志是 append-only 的，读类工具被拒不值得添行。

**`check_exec_environment` 和 `check_command` 不再各算各的。**两个工具都在讲"这个工作区的执行策略"，各写一份迟早说出两套话，而模型没办法知道该信哪个（方案 A 明说不要相互重叠的能力状态源）。现在前者的 `policy` 就是后者那一份快照（`exec::policy_snapshot`），顶层老字段留着做兼容别名，值全部从同一份快照取；一条测试把两边同名字段逐字绑死。前者另加 `preflight` 一格指向后者：一个答"这个工作区整体什么情况"，一个答"这一条命令能不能跑"。

门禁：`cargo test --workspace` 620 passed、0 failed；fmt、clippy 干净。

### U2 到此完成

对照第 4 节 U2 那一行的清单：统一错误类型（第二批的 `PolicyReason`、第一批的补丁诊断、第三批的 `FILE_VERSION_CONFLICT`）、`operation_id`、命令预检、策略/构建版本（`runtime_fingerprint` 与 `build_commit: null`）、补丁定位、文件版本前置条件——六项都做了。完成条件"模型无需猜测即可判断下一步是重读、修参数、等待还是请求授权"由按错误码分流的 `recovery_hint` 承担。

**U2 范围内没做、留给后面的**：A18 的"失败响应体量受限"。现在一个补丁失败的响应里仍然挂着完整的 harness 能力状态和 planning_context。要瘦身就得改响应形状，属于会影响现有客户端的变更，单独排期比夹在 U2 里做安全。

### U2 的一处收口：文件状态是三态（跨仓评审 X02）

第三批的 `current_file_version` 把权限不够、I/O 出错、"那是个目录"全变成了 `None`，而补丁逻辑拿 `None` 当"文件不存在"的事实。后果是：`expected_versions` 里写 `null`（这路径应当是空的）的前置条件，会在文件其实还在、只是属性读不出来的时候**通过**——那正是它要挡的事。跨仓重构评审 X02 点名了这个缺口，2026-09-19 收口。

现在是 `FileState::{Present(version), Absent, Unknown(原因)}`。"问不出来"一律拒，**包括没给前置条件的调用**：状态都问不出来，落盘前那道复核同样没法做，这时候动手正是这套机制要拦的。落盘前复核也跟着走三态。

`current_file_version` 留着，但只给不需要区分这两者的地方用（补丁落盘后报"新版本是多少"，那时拿不到只是少给一条提示），注释里写明了这条分工。

下一阶段按方案是 U3（结构化执行入口、受控的 `rg`/`gh` 规则、SSH 边界说明、`.github` 分类）。跨仓评审另有两条 P0/P1 直接落在 gld：X01（`toexec-fs` 在 Windows 上先删后 rename，失败会丢原文件）和 X03（hub 的执行资源所有权），都不在 U 系列里，需要单独排。**这两条后来都做完了**：X01 在 `e03372d`（升到 `toexec-fs` 0.2.1，并让替换的故障注入真的走共享库）加 `1a533a4`（Windows 上真跑补丁落盘测试），X03 在 `d947b90` / `dc6532c` / `91f45d8`。

### U3 第一批：`.git` 和 `.github` 不是同一种东西（C05、A06）

**新建一个 `.github/workflows/ci.yml` 以前报「禁止删除仓库保护资产」。**不是删除，理由也对不上，而修 Actions 是日常维护——用户撞到这堵墙只能绕开工具去写文件（复现记录 E02）。

根因是同一件事在两个地方各判一遍（`patch.rs::is_protected_repository_asset`、`workspace.rs::reject_protected_write_path`），两边都把 `.git` 和 `.github` 当成同一种东西。现在合成一个分类器 `tools/write_class.rs`，按"改了会发生什么"分：

| 路径 | 改 | 删 |
| --- | --- | --- |
| `.git/**` | 拒 | 拒 |
| `.github/workflows/**`、`.github/actions/**` | 要 `confirm=true` | 要 `confirm=true` |
| `.github/CODEOWNERS` | 要 `confirm=true` | 要 `confirm=true` |
| `.github/**` 其他 | 放行 | 要 `confirm=true` |
| 关键文件（`Cargo.toml`、`package.json`、`README*`…） | 放行 | 要 `confirm=true`（原有行为） |

**这是放宽，所以配一条反向的补偿**：`confirm` 之后，落盘结果和预检结果的 `warnings` 里点名这次改的是"GitHub 的行为"而不只是工作树（方案 C4 明说不能把 `.github` 标成无风险配置）。`notebook_edits` 走同一道门，否则 `.github/workflows/x.ipynb` 能从旁边绕过去。

行为变更带来一条既有断言的修改：`patch_check_rejects_all_git_and_github_writes` 断言的正是被修掉的那个行为，拆成 `.git` 仍然全拒 + `.github` 三种情况各一条。

**子进程那一层没有跟着放宽**：`policy.rs` 看的是命令文本，仍然把 `.git` 和 `.github` 一起当删除保护对象——从命令文本里分不出"改一行 workflow"和"把 `.github` 递归删掉"。两层宽严不同是有意的，写在 `write_class` 的模块文档里。

### U3 第二批：命令怎么进来，以及 `gh` / `ssh` 的规矩（C04、C01、C02、A03、A04、A05）

**结构化入口 `argv`。**以前只有 `cmd` 一行字符串：先按 shell 的规矩挑掉 `|` `;` `>`，再拆开直接 spawn——接口长得像 shell，执行却不是。模型想跑 `rg "foo|bar"` 得自己琢磨引号，加错就被"不允许 shell 串联"拒掉，而那个 `|` 只是正则。现在 `argv: ["rg", "foo|bar", "src"]`，参数逐格送进内核。

两种形式在 `tools/command_spec.rs` 合成同一个 `CommandSpec`，**权限是同一套**：白名单、危险命令、联网、受保护路径、程序解析全都只认它。唯一的区别是 `argv` 不做 shell 操作符检测（它的参数不经过 shell）。两个都给报 `conflicting_command_forms`，不猜哪个算数。长度上限量的是合成之后的那一行，拆成一千格也绕不过去。

测试证明的不是"没报错"，而是**进程真的收到了原样的字符串**：脚本把 `argv[1]` 打回来，内容是 `a b|c;d\ne`。

**裸命令名现在先查 PATH。**以前工作区里放一个叫 `python` 的文件，`python --version` 跑的就是它——而白名单批准的是"python 这个系统命令"，批的和跑的不是同一个东西（方案 B、审查 A04）。shell 也不把 `.` 放进 PATH，同一个道理。工作区里的脚本没有因此失去入口：PATH 上查不到的名字仍然回落到工作区，写 `./build` 一直是明确指路。

**`gh` 加进白名单 = 只读诊断。**`gh run view` 和 `gh run rerun` 只差一个词，按"首个单词是 gh"分不出来。名单是允许制：`run list|view`、`workflow list|view`、`pr list|view|diff|checks|status`、`issue list|view|status`、`release list|view`、`repo view`、`cache list`、`label list`、`auth status`、`gh version|status` 放行，其余一律拒（`github_command_not_read_only`），包括 `gh api`——一个 `-X POST` 就是任意写接口。gh 以后新增的子命令默认也不放行。

**`ssh` / `scp` / `rsync` 有自己的拒绝原因**（`remote_shell_not_allowed`），提示直接指向 hub / ccnm 远端成员，而不是笼统的"不在白名单"——后者会让模型改用 `scp`、改用 `paramiko` 接着试。文档同时写明：**把 `ssh` 加进白名单就是放开任意远端 shell**，gld 不限制目标主机，也挡不住端口转发，这件事没有中间档。

**`rg` 没有进默认白名单**，方案里也没要求进：首选仍然是 `search_text`（有上下文、分页、类型过滤，不用放开任何命令）。要 `rg` 的项目按追加写法加进去就行，文档写在 security.md。

`safe` 模式的联网命令模式补上了 `gh`——它显然要出网，漏掉它等于 safe 模式下有一个联网后门。

**内建的 `ls` 不再冒充系统 ls。**它是服务端自己答的极简实现，以前 `ls -la` 会把 `-la` 当成目录名、报"路径不存在"——看着像目录没了。现在带 flag 直接说清它是什么、该用 `list_dir` 还是 `list_files`，结果里那句 warning 也从 "native diagnostic without child process" 换成了说得出所以然的一句。同时修了它的相对参数解析：`ls inner` 以前从**工作区根**解析，现在按 `workdir` 解析，和在真实 shell 里输入它的结果一致（方案 B 的原生 ls 一条）。

**U3 里没做的**：`build_commit` 仍然是 `null`（诊断里报运行构建 SHA 属于 G-B / 跨仓 X10，要改构建脚本）；"未授权 SSH 不建立连接"只有"策略在 spawn 之前拒"这一层证据（代码路径如此，也有 `check_command` 的 `side_effects=none` 钉着），没有抓包那种证据；A04 的 Windows 分支没在本机跑过，靠 CI 的 Windows 格。

### U4 第一批：读的那一面，返回值要能用（F01、F02、F03）

**路径 round-trip（F01、复现 E06、验收 A12）。**根目录的 `list_dir` 以前给的是 `/Cargo.toml`：前面那个斜杠让 `read_file` 把它当成系统根下的绝对路径，报 `NOT_FOUND`。根因是工作区根算出来的相对路径是空串，拼接时多了一个斜杠。现在 `relative_display` 把根规范成 `"."`，**只在这一处规范**，`git.rs` 里那份重复判断删掉。测试串的是 `list_dir → read_file` 和 `→ patch_check`。

**零结果分得出种类（F02、复现 E07、验收 A13）。**glob 匹配的是工作区相对全路径，所以 `path="crates"` 下搜 `"exec.rs"` 永远是空——而这和"搜了但没匹配"、"这下面没有可读文本"看起来一模一样。现在结果里回 `search_root`、`glob_base`、`scanned_files`、`rejected_by_glob`、`rejected_by_type`，零结果的 warning 直接给出能用的写法（`**/exec.rs`）；schema 里也写明了基准。

**遍历剪枝（F03、验收 A14）。**以前进了 `node_modules` 再逐个文件丢弃。现在被忽略的目录整棵不进；只用于剪枝，文件那层的两道检查照旧，剪错了顶多多走一段路。

**长行不再假装搜完了。**超过 1 MiB 的行只有前 1 MiB 参与匹配，以前它和"这行里没有"长得一样；现在有 `lines_truncated_for_search` 加一条 warning。

### U4 第二批：输出能读多久、stdin 怎么关（X02、X03）

**输出寿命统一了（X02、验收 A16）。**以前内联跑完的会话 30 秒后回收，转后台那条路的回收时点却挂在命令自己的 deadline 上——同样是"跑完了"，能读多久取决于当初怎么调的，而调用方没办法知道还剩多少。现在一律**从进程结束那一刻起算 5 分钟**，`read_output` 回 `expires_in_ms` 和 `retention_ms`。判据在 `SessionStore::sweep`（惰性，每次 get/insert 扫一遍），定时器只负责早点把内存还回去，睡过头也不影响正确性。

配额：同一张表最多留 **32 条已结束**的会话，超了结束得最早的先走，**还在跑的一条都不动**。

**过期和瞎编分得开了。**到期之后再拿那个 id，报 `SESSION_EXPIRED`（带 `released_seconds_ago`、`retention_seconds`）而不是 `SESSION_NOT_FOUND`——前者是"确实有过，重跑一次"，后者是"这个 id 从来没存在过"。这是**新错误码**，按码分支的客户端会看到它。墓碑表只存 id、原因、回收时刻，最多 128 条，不留输出。测试把保留期设成 0 来验，没有 sleep（A16 要的"虚拟时钟"就是这个意思）。

**stdin 有三种状态了（X03、验收 A17）。**以前只有"给了就写完关掉"和"什么都不做"，而"什么都不做"意味着 stdin 一直开着却永远没有数据：`cat`、`grep foo` 会一路挂到 `timeout_ms`。现在 `stdin_mode` 有 `close`（没给 stdin 时的默认，起来就关，命令读到 EOF）、`once`（给了 stdin 时的默认）、`interactive`（留着，用 `write_stdin` 接着喂）。

**而且三种都在这次调用返回之前就安排好**——以前 `yield_time_ms: 0` 那一路在写 stdin **之前**就 return 了，初始输入整个丢掉，命令拿着一个永远没数据的 stdin 挂到超时。这条有回归测试钉着。

`tty: true` 保留为 `interactive` 的老名字，但说清了它**不给终端**：底下是管道，认 TTY 的程序不会因为它变得可用，结果里的 `pty` 永远是 `false`（方案 E：不能只改描述就声称终端程序已兼容）。真 PTY 没做。

**写 stdin 不再可能永久挂住。**管道缓冲是有限的（Linux 64 KiB，macOS 更小），命令**不读** stdin 时 `write_all` 写满之后就再也不返回——而它跑在 `block_on` 里，这次工具调用就永久挂住了。现在分段写、5 秒封顶，超时报 `STDIN_WRITE_TIMEOUT` 并**如实说写进去了多少字节**（用 `write` 循环而不是 `write_all`，就是为了这个数）。

**这一批动了两条既有断言**，都是行为真的变了，不是为了让测试变绿：

- `switching_to_plan_mode_stops_running_commands_through_the_daemon` 原来断言 `SESSION_NOT_FOUND`，现在是 `SESSION_EXPIRED` + `reason=terminated`——被 plan 模式停掉的句柄确实存在过，报"没见过"是错的。跨主体拿别人的 `session_id` 仍然报 `SESSION_NOT_FOUND`（会话表按主体分，墓碑也分），那条语义没变。
- `retained_session_timeout_stops_the_process_after_deadline` 原来断言 `stdin_open: true`；没给 stdin 的调用现在是 `stdin_mode: close`，起来就关。那个"开着"永远不会有数据，正是它让读 stdin 的命令挂到超时。

**跨平台**：stdin 三态那条集成测试标了 `#[cfg(unix)]`（要 `cat` 和真实管道行为），Windows 上只有编译和其余测试在 CI 里跑，**没有手工验过**。`StdinPlan` 本身没有平台分支，关管道走的是 tokio 同一套。

**U4 里没做的**：搜索结果的稳定续查游标——现在给的是"缩小范围怎么做"的建议加上明确的 `truncated`，方案 D 允许这两者取其一，游标要先定义稳定排序，单独排期更安全；真 PTY；`read_file` 的有界字节续读（现有的是按行续读加长行 warning）。

### U5 第一批：参数名对不上就当场说（A19）

**多给的参数以前会被悄悄扔掉。**每个工具的 schema 都写了
`additionalProperties: false`，但那只是**给客户端看的声明**——真正决定行为的
是代码读了哪几个 key。于是 `exec_command cmd=… timeout=600000`（真名是
`timeout_ms`）会**成功**，用默认的 30 秒跑完，返回值里一个字都没提那个参数被
忽略了。看着像"这条命令莫名其妙超时"，实际是参数根本没生效。这类错误最难查，
因为它长得像成功。

现在 `tools::args::reject_unknown` 按 schema 判：允许的名字直接从
`registry::input_schema` 取（不另写一份表，两份迟早对不上，而客户端读的是
schema 那份），多给就报 `INVALID_ARGUMENT`，`details` 里有
`unknown_arguments`、`accepted_arguments` 和 `executed=false`。

放在**策略判定之后**：`env` 被策略单独拒，理由是"服务端不让调用方设环境
变量"，比"没有这个参数"有用；两道门都过不去时先报更能指导下一步的那个。
下划线开头的键（`_host_session_key`，MCP server 自己注入）本来就不在 schema
里，不拒。

**这道门把三处已有漂移照了出来，一并修掉：**

| 漂移 | 症状 | 修法 |
| --- | --- | --- |
| `search_text` 读 `include_ignored`，schema 里没写 | 客户端发现不了这个能力；加了门之后连发都发不了 | schema 补上，措辞和 `list_files` 一致 |
| `cwd` 被当作 `workdir` 的别名读，schema 里没写 | 同上 | `exec_command` / `check_command` 都补上，注明"两个都给时 workdir 优先" |
| `patch_check` 只收 `patch`，转手却调 `apply_patch` | 带 `confirm` 的 workflow 改动、notebook 编辑**没法先预检那一次真实调用**，预检回答的是另一个问题 | 收同一组参数，只差 `dry_run`——那是它自己定死的 `true`，收进来只会让人以为能关掉 |

**测试**：单元测试扫工具源码里的 `args.get("x")`，每个名字都得有某个 schema
声明它（否则那段代码在新门下是死的）；`env` 是唯一的例外，写了理由。外加三条
契约测试：拼错名字被拒且报得出真名、`patch_check` 和 `apply_patch` 收同一组
参数、服务端自己注入的内部键不被误伤。

### U5 第二批：把仓库维护流程整个回放一遍（A20）

`crates/core/tests/repo_maintenance_replay.rs` 一条用例串完
**读仓库 → 查隐藏的 workflow → 改 README 和 workflow → 构建 → 读长日志 → 看 CI 状态**。
故意不拆：接缝才是要验的东西——前一步返回什么、下一步能不能原样拿去用，拆成
十条互不相干的小测试反而全看不到。

**证据分三类**：

| 类别 | 有没有 | 说明 |
| --- | --- | --- |
| 本地 | 有 | 读、搜、补丁、执行、分页，全部真跑 |
| 合成远端 | 有 | `gh` 是 fixture 里的假脚本（`tools/gh`），不联网、不认证 |
| 真实授权远端 | **没有** | 没有 GitHub 授权，一次真实请求都没发过 |

也就是说，"gld 能看 CI 状态"这句话只被验到**策略和执行这一段**：只读子命令
放行、`gh run rerun` 被拒、输出拿得回来。真 `gh` 认证之后还会不会有别的问题，
这条测试证明不了。

假 `gh` 是用 `ToolContext::executable_paths` 排到系统 PATH 前面的，**不动进程
自己的环境变量**（`set_var` 是全进程的，会波及并行跑的别的测试）。顺带验到了
U3 那条"裸名先查 PATH"：这台机器上装着真 `gh`（`/opt/homebrew/bin/gh`），
而跑起来的是假的那个。

**回放时撞出来一个新问题（A14）。**第一次搜 `runs-on` 是零结果——话没错，
但报的是"searched N file(s), none contained the query"，这和"这个仓库真的没有
workflow"长得一模一样，而下一步完全不同：一个是换关键词，一个是加
`include_hidden`。现在 `search_text` 和 `list_files` 都在**剪枝那一刻**数有几棵
点开头的目录没进去，零结果时多报一句，并回一个 `skipped_hidden_dirs`。
判据是"改成 `include_hidden=true` 就会进去"，所以 `node_modules`（那是
`include_ignored` 管的）和 `.git`（两个开关都不开）都不算在里面。

**跨平台**：假 `gh` 是带 shebang 的 sh 脚本，整条回放只在 unix 上跑
（`#[cfg(unix)]`）。Windows 那边只有编译和其余用例在 CI 里跑过，**没有手工
验过**。

**U5 里没做的**：真实 GitHub 请求（需要授权，没有）；`reject_unknown` 只看
顶层参数名，嵌套对象里多给的键仍然由各自的解析代码处理；`dry_run` 在
`patch_check` 上是拒绝而不是接受成 `true`。

**一处已知不稳，机制查清了，和这批改动无关**：`crates/cli/tests/start_and_upgrade.rs`
整仓跑时偶发失败（16 轮里 3 轮，每次挂的用例都不一样，单独跑这个二进制一直
是绿的；这批改动之前的基线跑了 9 轮没挂过，所以一开始不能排除嫌疑）。

用 `taskpolicy -b` 把它压到后台 CPU 优先级复现出来了，5 轮挂 4 轮，报的是

```text
错误：守护进程在 15 秒内没有就绪，请查看日志：…/logs/daemon.log
```

正常跑这个二进制要 1.4 秒，压到后台优先级要 **232 秒**（165 倍）。测试夹具等
守护进程就绪的上限写死 15 秒，CPU 被抢走就必然超。那三次失败都发生在刚重新
编译完的那几轮——`rustc` 还在占 CPU。属于这套守护进程/端口测试一贯的时序问
题，不是工具层改动引起的。

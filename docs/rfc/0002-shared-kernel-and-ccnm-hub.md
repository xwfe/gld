# RFC-0002：共享 Rust 内核，gld hub 接入 ccnm Runtime

日期：2026-09-15。方向已由用户确认；本文为实施方案。**2026-09-16 已实施**：共享内核 K1/K2 与 hub 接入 H1–H8 都落地了，逐项对照在第 9 节的补记；正文其余部分保留为当时的方案。Codex 原生 exec-server 那条线不在 gld 里，进度在 ccnm（P21–P24）和 toexec 的 v2 计划。

本 RFC 替代 [RFC-0001 的 WebCodex 服务采用路线](0001-shared-workspace-runtime.md)。不增加第三个 Server，不部署 WebCodex，不重写模型循环。原[证据附录](0001-shared-workspace-runtime-evidence.md)中的源码事实继续适用，不继续执行其中的服务准入计划。

## 1. 已确认目标

1. **共享内核**：gld 本机工具与 ccnm Runtime 编译使用同一份 Rust 基础实现，减少重复修复和行为漂移；产品策略、协议、身份和状态不合并。
2. **远端工具接入**：Web 端 AI → gld hub → ccnm 公共 bridge → 远端 Runtime。Web AI 自行分析，远端只执行读写、搜索、命令并返回结果。
3. **本轮明确不做**：启动第二个 Claude/Codex Agent、增加模型费用、Agent 任务编排、WebCodex Server/Runner 接管现有产品。
4. WebCodex、Codex 等只作为实现参考或经过审查的小组件来源；“参考成熟机制”不等于采用其整套产品架构。

这两项改造解决不同问题：共享内核减少代码重复；hub 远端接入打通调用链。协议接入不必等全部内核抽取完成，共享内核也不以新网络服务为前提。

## 2. 目标架构

```text
Web AI / ChatGPT / 其他 MCP Client
             │ 现有 HTTP MCP、OAuth、隧道
          gld hub
             ├─ Local 成员
             │    → gld 策略 / Planning / Harness
             │    → workspace-kernel → 本地项目
             │
             └─ CcnmRuntime 成员
                  → ccnm mcp bridge（gld 所在 Operator 主机）
                  → SSH stdio MCP
                  → ccnm Runtime（远端执行身份）
                  → ccnm 策略 / writer guard
                  → 同一 workspace-kernel → 远端项目

工具结果沿原路返回；Web AI 根据结果继续分析。
```

共享的是编译期代码，不是一个跨机器内存对象或全局写锁。远端成员不能回到 gld 本机调用文件/命令工具；远端不可达时明确失败，不静默本地降级。

ccnm 的官方 Agent 管理入口保持原样，但不接入本次 hub。将来要从 Web 提交自主分析任务，应另用 `ccnm.machine/1`，不能在工具入口背后偷偷启动 Agent。

## 3. 方案选择

权重：保持现有架构和明确需求 40%，减少重复维护 30%，权限与兼容 20%，渐进迁移成本 10%。硬性边界不由评分抵消；下表为定性评审，不是性能跑分。

| 方案 | 架构 40% | 复用 30% | 边界 20% | 迁移 10% | 决策 |
| --- | --- | --- | --- | --- | --- |
| 两产品继续完全独立（基准） | 保持现状 | 重复实现继续增长 | 当前边界不变 | 最低 | 仅作基线 |
| **共享库 + hub 公共 bridge** | **沿用两个产品和已有服务** | **公共原语只维护一份** | 策略/身份分别保留 | 可拆小提交 | **采用此方向** |
| WebCodex 整体 Server + Runner | 增加当前不需要的产品层 | 可服务复用 | 需重新整合身份/状态 | 高 | 当前不采用，未来另议 |
| 将 gld/ccnm 合为一个产品 | 侵入现有架构 | 表面统一 | 混淆权限/职责 | 最高 | 不采用 |

## 4. 共享内核：先小后大

暂名 `workspace-kernel`，拟放在独立源码仓库；本轮未创建。两个项目通过固定 revision 的依赖消费，Cargo.lock 各自保留；本地 path 仅供开发，不提交依赖个人目录的路径配置。发布包或外部仓库创建按实际实施范围另行安排。

> **2026-09-16 补记（RFC 正文保持原样，这里只记事实）**：仓库已创建，正式名是
> **`toexec`**（`github.com/xwfe/toexec`，公开），不叫 workspace-kernel。第一个
> crate 是 `toexec-text`（有界行读取），按 tag 消费而不是 revision——`{ git = ...,
> tag = "toexec-text-v0.1.0" }`；一个 crate 管一件事，所以 crate 不跟仓库同名。
> "本地 path 仅供开发、不提交"这一条按原样执行过一次教训：提交了 path 依赖，
> 两边 CI 当场构建不了。
>
> **K1 里两处关于工具链的说法已经不成立**：基础库不是 MSRV 1.85 / edition 2021，
> 两个 crate 都是 **edition 2024、rust-version 1.89**；gld 的工具链也**确实跟着
> 升了**（1.85 → 1.89），那是用户 2026-09-15 定的「三仓统一 rust-version、时点为
> 第一个共享 crate 被产品链接时」。所以 K05 里"基础库用既有 Rust 1.85 验证"这一条
> 不再适用，其余（两产品各自门禁、gld Windows 编译分开记录）照旧。
>
> K1/K2 的实际结果：`toexec-text` 只抽了有界行读取——两边的 `read_file` 是两套
> 对外契约，**不统一**；`toexec-fs` 抽了原子写入的两步纯机制，`PreparedBatch`
> 那种统一的写入底座**没有做**，journal 和两种回滚编排仍各自留在产品里。逐项
> 对照见 toexec 仓库 `evidence/v2-k/duplication-audit.md`。

### K1：第一步只共享文本扫描原语

从现有 gld 的固定块扫描实现提取、整理，不重新写一套完整 `read_file`：

- 输入是已由产品授权并打开的 `Read`，不是路径、workspace 名称或 token。
- 提供有界行片段扫描、增量 UTF-8 校验、UTF-8 边界截断。
- 巨型无换行输入不能先整行读入内存；调用方控制保留量和停止条件。
- gld 保留 strict UTF-8、完整总行数、原 JSON 和错误码；ccnm 保留窗口停止、lossy/BOM 标注和原 MCP 文本。
- 两边真的调用同一代码才算完成，不以相同 trait、复制函数或空 crate 代替共享。

基础 crate 首阶段 **std-only、MSRV 1.85**。edition 2021 是兼容默认选择，不是技术上不能采用 2024。ccnm 的 Rust 1.89 锁实现仍留在 ccnm，不因此升级 gld 的工具链。

### K2：写入底座，保留两种补丁前端

```text
gld 文本 patch / Unified Diff → gld 解析、定位与路径策略 ─┐
                                                        ├→ PreparedBatch
ccnm JSON edits + version   → ccnm 校验与路径策略 ───────┘
     → 共用临时文件、提交、journal、失败恢复原语
```

`PreparedBatch` 是拟议内部结构，不是新公共 wire 格式；应明确文件动作、写前状态、目标字节及元数据。gld 不要求客户端改传 ccnm JSON，ccnm 也不放弃 version guard。

必须区分：产品授权、workspace writer authority、一次提交的串行修改权、journal。产品持有写权覆盖整个内核提交/写进程生命周期；journal 不授予权限，Rust 类型本身也不证明跨进程互斥。

先保留各自锁实现。ccnm 以 canonical Git common directory 和 Runtime state 定位锁，hub 远端操作仍由该 Runtime 持锁。若以后让两个本地进程直接写同一资源，另定共同 lock authority；不能默认不同 state 目录的锁已经共享。

gld 的 `.github`/自身数据目录保护、外部读开关，ccnm 的固定 root、绝对/父路径限制仍是产品 Policy。文件版本、特殊文件、权限位、Windows 替换、部分失败与恢复冲突逐项测试。不能将逐文件 rename 宣称为多文件瞬时原子提交。

### K3：进程原语；K4：其他真实重复

进程后续单独共享 spawn、输出排空、超时、等待回收和子进程组/Job Object 等机制。gld 保留交互 session、stdin 和续读；ccnm 保留 argv、受控环境、一次命令结果和输出存储。

同步文件处理由宿主放在阻塞线程；异步进程由宿主提供运行环境。内核不创建全局 Tokio runtime，不自行读取 HOME/config、不探测 Provider、不管理认证。

搜索、glob、Git 运行辅助等只在重复与收益明确后提取；ignore 默认过滤、编码、排序和预算不能顺便改变。已确认的 Git 超时、hunk 定位和进程清理问题先写失败测试、单独修复，不夹带成“搬代码”。

## 5. hub 接入：复用 ccnm 公共协议

### 5.1 成员类型，而不是伪造一个本地目录

当前 hub 的 `CachedContext`、`Routed`、`context_for` 依赖本地 `SharedToolContext`，远端成员不能直接塞进这条缓存。

拟将 hub 内部成员分为 Local 与 CcnmRuntime。保留现有 `members` 与本地 WorkspaceProfile 的序列化兼容；远端新增独立配置，至少保存稳定成员 ID、展示名、已配置的 ccnm node/workspace 引用及最大访问级别。具体字段在实现前定稿，不用 fake path 或空目录迁就本地类型。

每次工具调用仍必须明确 `workspace`。远端 root 由 ccnm Runtime 自己解析，工具入参不得覆盖本机 bridge 启动程序、SSH host、账号、私钥或远端 root。这里限制的是控制面启动参数；远端 `exec_command.cmd` 的程序和 argv 仍按 ccnm 原工具契约授权执行。成员删除/权限收紧/配置变化须使对应旧会话失效。

只启动公开命令 `ccnm mcp bridge <workspace> --node <configured-node> --mode read|coding`，参数来自操作员受控配置和限权后的选择，以 argv 启动，不拼 shell。gld 不生成内部 `mcp-serve` payload、不解析 SSH 登录凭据、不跳过官方 bridge。

### 5.2 不伪装成本机同名工具

ccnm 与 gld 的同名工具不是同一契约：`cmd` 数组/字符串、JSON edits/文本 patch、分页与输出引用都有区别。第一版采用窄的远端工具会话入口，不强行无损转换所有 gld 工具。

拟议最小入口（名称未成为现有 API）：

| 入口 | 行为 |
| --- | --- |
| `runtime_open` | 指定远端成员和受限模式，完成真实 MCP 初始化；返回随机会话句柄、工具 schema、预算、会话期限与范围 |
| `runtime_call` | 每次同时提交成员、会话、ccnm 工具名和参数；只允许该会话实际发现且服务端批准的工具 |
| `runtime_close` | 停止接收新调用，结束 bridge；返回已确认关闭或仍需核对，不把本机进程退出冒充远端资源已回收 |

现有 gld 本地工具不改名、不改变 schema；对远端成员误调本地工具应明确提示使用远端入口，不本地执行。`runtime_open` 返回 schema 后，Web AI 按 ccnm 格式组装请求；结果保留远端 MCP `content/isError`，外层附成员/会话/连接代次，不把文本错误当成功。

工具允许集取 hub 权限、远端成员最大模式、服务端批准的 ccnm 工具语义与 Runtime 实际能力的交集。不能因外层工具叫 `runtime_call` 就绕过 read-only；exec 保守视作可写，不从命令名字推断只读，也不只信任 MCP annotations。

远端只报告自身能力。没有原生 Git 工具就使用受授权的远端 exec，不在本机 Git 上求结果；没有 stdin/PTY/运行中取消能力就明确不支持，不制造虚假 session 接口。长命令预算必须与真实 Web 客户端链路验证，不能在请求已经启动后才发现 HTTP 等不及，随后盲目重试。

> **2026-09-16 补记：入口形状按跨仓计划改为静态工具（RFC 正文保持原样）。**
>
> 本节的 `runtime_open` / `runtime_call` / `runtime_close` 是 09-15 16:22 的草案。
> 同日 22:49 起草、23:15 定稿的跨仓计划（toexec 仓库 `docs/plan/implementation-plan-v2.md`
> 第 7 节）给了另一个形状：**固定前缀的静态工具** `remote_workspace_info` /
> `remote_read_file` / `remote_list_files` / `remote_search_text`，schema 基于冻结协议，
> 「只开放已评审工具，不自动跟随上游增加权限」。
>
> 以后者为准，理由不只是它更新：
>
> 1. **`runtime_call` 转发任意工具名，等于自动跟随上游。**远端 ccnm 哪天加一个工具，
>    Web AI 立刻就能调用，而 gld 这边没有评审过。静态工具要求逐个显式声明。
> 2. **schema 的可见性。**静态工具的 schema 在 `tools/list` 里就能看到，Web AI 和
>    审计一眼知道能干什么；`runtime_call` 的能力藏在 `runtime_open` 的返回值里，
>    每个会话还可能不一样。
> 3. **权限交集更好落实。**静态工具集天然是「已评审」的子集，不必在调用时再去
>    推断某个远端工具名是不是只读。
>
> 本节其余的约束**全部继续适用**，尤其是 5.3 的认证主体、5.4 的写租约和 5.5 的
> Planning 边界。会话句柄也没有被否定——它在 coding 模式下仍然必要，只是名字按
> 跨仓计划叫 `remote_coding_begin` / `remote_coding_end`，read 模式不需要句柄。

### 5.3 认证主体必须进入 hub 调用链

当前 HTTP 鉴权只放行/拒绝，请求进入 hub 时没有经过验证的主体上下文。支持 remote coding 前，必须把受信任的 `AuthContext` 传入；不能只按 workspace ID 缓存长期 bridge。

远端会话绑定：已验证的授权主体、成员 ID、访问模式、随机句柄、配置版本及连接代次。每次调用重新检查授权与归属；ChatGPT session metadata、MCP clientInfo 或模型传入的 session 字符串不能单独作为身份。

共享 bearer 密钥代表共享授权，不承诺识别不同自然人；随机句柄用于防串会话，不代替服务端授权。第一版 remote coding 要求可验证的认证身份；noauth 不开放该能力。不记录原始 token，不把本机/其他成员的说明、skills 或历史混入远端上下文。

### 5.4 coding bridge 是写租约，不是免费连接池

ccnm 在 MCP 初始化前就获取 writer guard，`read_output` 又依赖该连接的会话。因此禁止启动 hub 时预热全部 coding 连接，也不能每次请求随意新建连接后假定旧输出仍可读取。

- 显式、按需打开；每个远端会话串行执行调用，不能在同一 writer 内并发 patch 与 exec。
- 空闲租期和单操作 deadline 在实现前冻结。租期只由实际授权调用续期，不能由 transport ping 无限续期。
- 空闲到期停止新调用并关闭 bridge；在途操作按其期限等待结束或进入关闭/未知状态，不能到点直接把 writer 判为空闲。
- Web 断开不一定可被即时识别；不能代答 ccnm 心跳后声称远端能自行识别真正 Web 用户离线。
- gld 重启不恢复旧 coding 句柄、不重放请求；重新打开是新连接代次，旧输出引用不能冒充新会话结果。
- 不确定远端是否执行、是否仍持锁时，返回 `outcome_unknown`/明确诊断，保留核对路径。不按时间强夺 ccnm 锁，不转到本机补执行。

### 5.5 Planning、History 和远端策略

远端成员不构建本地 Harness，不调用本地路径的 `PlanningService`，不在 gld 主机替远端项目创建计划/历史文件。

第一版远端仅声明明确的访问模式和工具能力；本地 Goal/Plan/History 能力保留给 Local 成员。若调用方要求远端 Planning 门禁而尚未实现，应拒绝该模式，不伪报 Direct 或自动绕过。操作员收紧访问模式后立即阻止新不允许调用，旧在途写入按安全关闭流程处理，不能假称已经撤销执行。

## 6. WebCodex 只读参考已落地

本机交互 shell 中 `gc1` 是 `git clone --depth=1` 的别名。本轮使用该展开式完成浅克隆，位置 `/Users/bing/xdw/webcodex`，HEAD 为 `e11cb3e4c96b8b4fa9f84ca59fa072ca0bb0b2ae`，origin 为用户给定仓库；工作区干净。

这是 main 的源码快照，不是旧 RFC 的固定发布版实验。未运行 Cargo metadata、构建、测试、服务或安装脚本；未引入其任何运行依赖。

| 参考对象 | 可复用内容 | 不能一起带入 / 必须验证 |
| --- | --- | --- |
| `webcodex-process` | 独立 ManagedChild、Unix 进程组、Windows Job Object，优先对照完整小 crate 的引用/vendor 成本 | 不提供文件/网络沙箱；需核对 MSRV、同步宿主适配、真正进程树测试。它 `publish=false`，不能假装已能从 crates.io 获取 |
| `core/apply_patch_shared` | 无 I/O 的 patch 解析、新内容推导和行尾处理 | 无 mode 的辅助函数实际可走 FirstMatch，必须显式选匹配策略；不能复制它的产品敏感路径规则 |
| `workspace/file_read_range` | 对 `impl Read` 扫描、跨块 UTF-8、内容预算等算法与反例 | 部分路径为 SHA/行数读到 EOF；内存有界不等于 I/O/时间有界，不照搬默认预算、错误码和 envelope |
| validation 的 Cargo/Go parser | 按需要取测试/诊断输出解析与测试 | 不复制 Workflow Session、ledger、attempt；解析到文本不等于命令确实成功 |
| `persistent-shell` | 未来确需跨调用保持 cwd/env 时，再评估完整小包 | 不是普通 PTY 小封装，带自己的绑定和恢复状态；第一版不引入 |

每个实际移植模块记录来源 SHA、原文件、许可、修改摘要和带入测试；保留适用的 Apache/MIT 版权与许可声明。先比较“直接引用独立组件”和“少量移植”的长期成本，不以 fork 整个 WebCodex 为默认。

`cap-std`、`ignore/grep-*`、`process-wrap` 等仍是可选成熟积木，不构成自动安全保证。`rmcp` 属于 hub 协议客户端，不放入文本/文件内核；选择版本/features 时单独检查 gld MSRV。若所需客户端 SDK 无法兼容既有工具链，必须先明确升级影响，不在依赖解析时自动安装或升级。

## 7. 实施顺序与验收

本轮仅文档与参考源码下载。以下均为待实施；共享内核与远端接入可按依赖分线推进，不能用一个大提交同时改两产品所有工具。

| 阶段 | 最小交付 | 验收/停止点 |
| --- | --- | --- |
| S0 固定边界 | 现有读/路径/输出 fixture、内核接口、许可证与依赖清单；远端成员/认证/会话契约 | 确定哪些行为必须不变、哪些缺陷独立修复；冻结预算和支持平台，不先造空框架 |
| K1 共享文本原语 | 独立基础库，gld 与 ccnm 两个 read adapter 真正消费 | K01–K05 通过；两个公共接口都保持各自语义，任一产品可单独回退 |
| H1 hub 分型与只读模拟 | Local 路径不变；AuthContext 贯穿；用合成 stdio peer 验证 open/list/call/ping/close | H01–H05 通过；不碰 SSH、不创建假本地工作区；无用户配置/凭据读取 |
| H2 真实只读链 | 显式配置的远端成员经公开 bridge 完成 read/search/输出闭环 | 真实 Web 客户端与 Runtime 返回一致；认证、预算和资源回收有证据，不据一次连通宣称稳定 |
| H3 远端 coding | 显式 writer 会话、串行调用、租期/断线/重启与关闭 | H01–H08 全过；临时远端项目 patch → test → result；不启动官方 Agent，不放开本机降级 |
| K2/K3 共享写入与进程 | 分别抽提交/journal与进程生命周期底座，不改变两种工具前端 | K06 及两产品完整相关门禁通过；故障注入和真实平台单列，不默认清除恢复记录 |
| S1 收敛重复实现 | 移除已证明被共享代码替代的局部重复；固化接口与升级回归 | 不留两份持续演进的同类算法；不删除仍有消费者或未验证的路径 |

推荐先完成 K1 与 H1，再让 H2/H3 交付 Web 远端能力；K2/K3 不阻塞只读打通，但也不因为 bridge 已可用就宣称共享内核已全部完成。

ccnm 的现有 P0–P12 已结束；实际修改它之前须在该仓库按规则立新阶段并记录契约影响。本轮不改其 `status.json`，也不把这里的阶段状态同步为 ccnm 已完成。

### 可判定的验收清单

| 编号 | 用例与结果 |
| --- | --- |
| K01 | 两产品调用同一基础实现；旧算法不另复制；内核无 HOME、配置、认证、全局 runtime 依赖 |
| K02 | 普通文本、CRLF、BOM、无尾换行、跨块/非法 UTF-8、分页，各自与原产品契约差分一致 |
| K03 | 生成式 Read 提供超长单行；片段固定上界、可提前停止，不创建巨型常驻 fixture 或缓存整行；S0 冻结输出/内存/时间预算 |
| K04 | 两产品的路径与特殊文件回归各自通过；不将 gld 外部读规则、`.github` 保护或 ccnm 禁止父路径悄悄改掉 |
| K05 | 基础库用既有 Rust 1.85 验证；ccnm 原工具链通过；gld Windows 编译和实际读取回归分开记录，不自动升级依赖 |
| K06 | 写入版本冲突、重复片段、部分失败、恢复再次中断、执行位、Windows 替换；inline/yield 超时、取消、持管道后代、stdin 与输出续读均有测试 |
| H01 | 旧本地成员配置原样可用；远端没有本地 ToolContext/Harness/Planning 文件副作用；不认识和未授权成员不泄露信息 |
| H02 | AuthContext 来自真实鉴权；伪造 metadata、跨主体/成员/模式/配置代次/bridge 句柄均拒绝；原始凭据不进输出/日志 |
| H03 | 只开放受批准 ccnm 工具；只读模式拒绝 patch/exec；任意 RPC、bridge/SSH 启动参数与 root/key 不可透传；远端命令 argv 按 ccnm 原契约执行；新增上游工具不自动放行 |
| H04 | 保留原参数/schema/content/isError；绝不将 gld 字符串 cmd 当 ccnm argv；旧连接 output_ref 不能在新会话或其他成员读取 |
| H05 | 初始化失败、版本错配、stderr 有界收集、ping、权限收紧、成员删除、空闲关闭、Web 链路预算均可复现；不在持全局锁期间等待远端 I/O |
| H06 | 两会话竞争同一 Runtime writer、同会话两个并发修改、既有离线 fixture 模拟的 Managed writer 与 hub coding 相互互斥，保持 ccnm 原范围，不为该测试启动真实模型 |
| H07 | HTTP 断开、SSH 黑洞、bridge 异常、hub 重启、在途 exec 租期到期均不重放副作用；明确 unknown，不冒充远端释放、不强删锁文件 |
| H08 | 真实 Web → hub → bridge → Runtime 的 read/search/patch/test/结果闭环与关闭有脱敏证据；不启动模型进程，不触碰非试点目录 |

S0 必须在实验前冻结每个平台/能力的适用矩阵、输出预算、deadline、空闲回收规则、取消等待预算与性能容差。只读和 coding 分别准入；缺乏身份隔离或不互信多用户测试时，不宣传多租户能力。上游 ignored 的真实进程测试须显式选择，不以默认测试数量代替。

## 8. 回滚与不确定性

- **共享库回滚**：单个消费者可独立退回旧 adapter/revision；不变更端口和持久格式。若写入阶段新增 journal/schema，须先完成兼容与恢复设计，再切换，不将旧二进制直接指向新状态。
- **远端入口回滚**：阻止新远端调用，排空或核对在途操作，关闭自有 bridge，禁用对应成员。已有本地 hub 成员继续按原路径运行，不全局停服务。
- **数据恢复单列**：禁用远端入口不是撤销远端文件修改；不得自动 reset/clean、覆盖用户修改或重发最后一次 patch。
- **断线未知**：没有确认完成/取消就不能报告成功、失败或锁已释放；保留可核对的成员、会话、请求及远端诊断，不扩大权限修复。
- **版本回退**：固定公共协议版本、共享库 revision 与各自 lock；兼容行为变更单独标记，不能靠重录 golden 通过。

## 9. 当前完成与后续入口

本轮（09-15）完成：用户边界确认、WebCodex 浅克隆与只读组件核查、旧服务方案明确废止、新方案入库。共享库、remote hub 类型/工具/认证上下文和真实远端试点均未实施。

下一实施入口是 S0/K1 与 H1 的小范围设计和测试，不再安排 WebCodex 服务 PoC。新库创建、两产品代码迁移按明确实施任务推进；真实系统账号、ACL、防火墙、已安装二进制替换、公网发布继续遵守既有授权要求。

### 2026-09-16 补记：K1/K2 与 H1 已落地，H2 卡在一跳 SSH

**共享内核**（K1、K2）：仓库 `xwfe/toexec`，两个 crate 各自发版，按 tag 引用。
`toexec-text` 是有界行读取，`toexec-fs` 是原子落盘与替换。gld 和 ccnm 都真的在
消费，各自保留原有的对外契约——尤其 `toexec-fs` 返回的是 `WriteError { step, source }`
而不是一个合成的 `io::Error`，否则 ccnm 会把刷盘失败从 `internal` 静默变成
`invalid_args`。盘点结论在 toexec 仓库的 `evidence/v2-k/duplication-audit.md`：
两个产品真正重复的只有这些纯机制，read 契约和回滚编排是各自的外部契约，不共享。

**H1 完成**，四块：

| 块 | 源码 | 钉住的验收项 |
| --- | --- | --- |
| MCP stdio 客户端 | `crates/core/src/bridge/peer.rs` | H05 的超时、stderr 有界收集、握手版本错配 |
| 远端成员模型 | `crates/core/src/bridge/member.rs` | 5.1 的 argv 只由配置决定、模式只降不升 |
| 静态远端工具 | `crates/core/src/bridge/tools.rs` | H03 的工具名与参数名双白名单 |
| 连接生命周期 | `crates/core/src/bridge/session.rs` | 5.4 的按需打开、串行、空闲回收；H05 的不在全局锁里等远端 I/O |
| hub 分型与路由 | `crates/core/src/hub/mod.rs` | H01 本地路径不变、H04 结果原样透传 |
| 鉴权主体 | `crates/core/src/auth/context.rs` | 5.3 的连接按主体分；H02 的凭据不进日志 |
| 操作员配置面 | `gld hub remote add/rm` | 远端成员从此可配，不只存在于测试里 |

现场证据在 [`evidence/v2-h-read-chain.md`](evidence/v2-h-read-chain.md)。

**H2 完成**：跨两台真实机器走通了。这台 Mac 当 MCP Host，fodelf 当 Runtime，
四个只读工具（`workspace_info` / `read_file` / `list_files` / `search_text`）加分页
和越界路径拒绝全部在真机上验过；ccnm 自报的 `[server pid …, call N]` 证明一条连接
服务了全部调用（首次 465 ms，之后 34–60 ms）；`gld hub stop` 之后两台机器上的进程
都归零，走的是 EOF 正常收尾而不是 kill。

两台机器原有的 Agent/Runtime 角色一个字没改——fodelf 作为 Agent Node 把 workspace
列表委托给了 Runtime，ccnm 正确地拒绝了「既委托又自带列表」，所以探针走另一份
`CCNM_CONFIG` 配置文件。细节和清理清单见证据文档。

**H3（远端 coding）完成**：`remote_coding_begin` 开一段写租约发一个随机句柄，
`remote_apply_patch` / `remote_exec_command` / `remote_read_output` 挂在它上面，
`remote_coding_end` 关掉。同样跨两台真机验过——patch 真的在 fodelf 上建了文件，
exec 的 `output_ref` 分页偏移稳定且只在本会话里有效，写锁被别人占着时报
`REMOTE_WRITE_LOCK_BUSY` 而只读照常通。

几条钉死的规矩：句柄不是授权（每次调用重对主体/成员/代次）；传输层一断会话
就结束、**绝不偷偷重开**（协议 6.3 没有 resume）；read 和 coding 是两条独立
连接；写调用断在半路报 `REMOTE_OUTCOME_UNKNOWN` 且**不标可重试**；
写锁 busy 与状态 unknown 分成两个错，后者按协议第 7 节绝不标可重试；
noauth 不开放 coding（5.3）。coding 空闲 2 分钟回收，比只读的 5 分钟短，
因为它占着远端写锁。

**H06 / H08 也做完了。**H06：同会话并发被有界等待挡住（等 2 秒后报
`REMOTE_CODING_BUSY`，可重试，被挡的调用根本不发到远端）——串行不等于无限排队，
远端 exec 最长 10 分钟，挂在锁上那么久 Web 那头早断了；两会话争同一 writer 真机
和合成 opener 两条路都验过；Managed 与外部 coding 共用一把锁，gld **不替远端
认定占锁的是哪一种会话**。判断写锁失败时**只看消息第一行**，因为 ccnm 的 fixture
写明「第一行之后是给人的排查指引，措辞会变」，在整段 stderr 上匹配迟早被指引
带偏；测试用的 stderr 逐字抄自 `start-refused-busy.json` /
`start-refused-guard-unknown.json`。

H08：一个纯 HTTP 的 MCP 客户端走完 read → search → patch → **test** → 结果 →
关闭。远端一个真会红的小项目，改之前三个 unittest 挂两个，patch 之后全绿；
中间故意拿过期 version 再打一次，拿到 `CCNM_E_STALE_EPOCH` 原样透传——**远端的
版本守卫是活的**。没有启动任何模型进程，只动试点目录。**未做**的是单独归档
脱敏 transcript，以及公网入口链路。

**参数名的出处改了**：冻结协议的 `tools-list-*.json` fixture 是删节版，而且
`tools-list-coding.json` 把 `apply_patch` 的参数写成 `changes`，实现收的是
`files`。现在以 ccnm 的 `*Args` 结构体为准，read 白名单也照真二进制补齐了
`end_line` / `max_bytes` / `include_hidden` / `glob` / `case_sensitive` /
`context_lines`。

### 本轮源码定位

- gld hub 本地绑定：`/Users/bing/xdw/gld/crates/core/src/hub/mod.rs:101-118`、`:301-326`、`:353-388`、`:411-435`；成员配置：`/Users/bing/xdw/gld/crates/core/src/settings/model.rs:87-108`。
- 鉴权与调用上下文：`/Users/bing/xdw/gld/crates/core/src/mcp/listener.rs:70-83`、`:369-403`、`:568-589`。
- ccnm 公共 bridge：`/Users/bing/xdw/ccnm/docs/protocol/remote-workspace-mcp-v1.md:65-96`；writer 获取：`/Users/bing/xdw/ccnm/crates/ccnm-core/src/mcp/server.rs:304-327`；心跳：同文件 `:943-968`。
- 共享 reader 切口：`/Users/bing/xdw/gld/crates/core/src/tools/file.rs:631-734`、`/Users/bing/xdw/ccnm/crates/ccnm-core/src/mcp/read.rs:292-332`。
- WebCodex 独立进程组件：`/Users/bing/xdw/webcodex/crates/webcodex-process/Cargo.toml:1-25`；真实进程 ignored 测试：`/Users/bing/xdw/webcodex/crates/webcodex-process/tests/managed_child.rs:234-258`。
- 纯 patch 参考：`/Users/bing/xdw/webcodex/crates/webcodex-core/src/apply_patch_shared.rs:782-808`；句柄输入读取：`/Users/bing/xdw/webcodex/crates/webcodex-workspace/src/file_read_range.rs:411-442`。

这些是源码事实与方案，不是本轮运行了上述能力的声明。

本轮文档验证：隔离 `GLD_HOME`，用已安装工具链运行 `cargo test --locked --offline -p gld --test docs_commands_exist`，2 passed / 0 failed；本地链接、证据路径/行号、验收编号和差异检查通过。独立只读评审后明确区分“本机 bridge 启动参数不可覆盖”和“远端命令 argv 可以按契约执行”。未运行共享库、hub 远端链或 WebCodex 的构建与测试。

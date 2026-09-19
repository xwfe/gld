# gld：hub 收敛、完整开发闭环与社区借鉴路线

日期：2026-09-19  
状态：**提案，待其他模型实施；不代表下列能力已经交付**  
前置审查：[工具问题与修复方案](2026-09-19-gld-tooling-review-and-plan.md)  
现行架构：[RFC-0002](../rfc/0002-shared-kernel-and-ccnm-hub.md)；并行能力补齐：[RFC-0003](../rfc/0003-native-parity-sync.md)

## 1. 结论与边界

**hub 应成为默认连接入口；单项目使用方式应保留，但不应继续拥有独立的执行状态和另一套生命周期。** 一个项目是一个受限视图，不是另一个产品模式。当前不能直接删除单项目服务：hub 尚以“全部成员”为授权范围，替代前必须补齐项目级授权。

**当前 gld 能完成不少项目的编码—测试循环，但尚不足以承诺完整、可靠、可恢复的开发全流程。** 文件读写、命令执行、Git 检查已有基础；短命令能运行，不等于长期开发服务器、交互调试、浏览器验收、跨连接恢复和产物交付都已经成为受管能力。

产品定位建议保持为：**面向外部 AI 的、多工作区、可验证的开发执行环境。** AI 在外部推理，gld 在已有权限下执行和提供证据。不能为了“完整”再造聊天产品、模型循环或通用 Agent 调度系统。

沿用已确认边界：不部署第三个通用 Workspace Server，不将 gld/ccnm 合并；本地与远端工具不做有损伪统一；共享纯机制继续进入合适的 `toexec-*` 小组件，权限、身份、路由、任务状态留在所属产品。本文只补充后续路线，不恢复已废弃的 RFC-0001 WebCodex 服务替换计划。

## 2. 本次依据及核验范围

本地基线为 `main@fbe7665e1e3d8c068ecc2e418b84e5edb164314a`，领先上游 2 个提交。开始时已有 CLI、bridge、hub、文档的未提交修改；这些是审查输入，不应被实施模型覆盖。上一轮 U 系列修复文档也未提交。

本轮读取 gld 源码、README、RFC 和工具能力表；研究用户提供的 **8 个不同仓库**，`yyjeqhc/webcodex` 的重复链接只计一次。上游证据以 README、架构/协议文档为主，另抽查了四个仓库的相关源码目录树。没有运行上游项目，也没有完成八个仓库的逐文件源码或安全审计。

通过公开 GitHub API 取得参考提交，见第 9 节。网页文档主要读取抓取时的默认分支；**提交定位不代表已逐字验证所有网页内容都属于该提交，也不代表最新 release 的承诺**。实施时须锁定所采用文件的 revision、许可与测试条件。

Fylane 的 GitHub 网页解析未成功，但通过公开 API/Raw HTTP GET 成功读取了元数据与 README，不能因此把它误判为仓库不存在。读取没有使用账号凭据，没有安装或运行上游代码。

### gld 的关键源码事实

| 编号 | 已核对事实 | 定位 |
| --- | --- | --- |
| S1 | hub 的本地成员进入 `call_tool`，已有工具执行实现是共享的，不是两份独立文件/命令引擎。 | `crates/core/src/hub/mod.rs`：`call_local`，约 L481–518 |
| S2 | hub 的 `context_for` 自行创建并缓存 `ToolContext`；配置指纹变化时替换上下文并终止旧会话。 | 同文件：约 L712–777、L811–825 |
| S3 | 单工作区 MCP listener 也自行调用 `build_tool_context`；每个 `ToolContext` 构造自己的 `SessionStore`。 | `crates/core/src/mcp/listener.rs`：约 L155；`tools/context.rs`：约 L105 |
| S4 | 单工作区与 hub 有不同 OAuth audience/客户端注册作用域；hub 授权文案明确覆盖全部成员。 | `mcp/listener.rs`：约 L213–251；`docs/concepts.md`：约 L205–227 |
| S5 | 当前 hub 请求每次显式带 workspace；没有共享“当前项目”。本地与远端工具错用会明确拒绝。 | `hub/mod.rs`：约 L453–476；`docs/concepts.md`：hub 一节 |
| S6 | 本地工具表有 read/search/patch/exec/session/Git/image/history/planning/task，但 compact 的 skills 尚缺，RFC-0003 G2/G3 已覆盖部分缺口。 | `workspace_context` 实际结果；`tools/registry.rs`；RFC-0003 |
| S7 | 单项目 MCP/Actions 仍由独立的 `RuntimeSupervisor` 服务条目管理。 | `runtime/supervisor.rs`：`ServiceKind`、`RuntimeSupervisor` |

行号随工作树变化，以函数名为准。前一轮的补丁、策略、分页问题不在本文重复做复现；本轮没有重跑其 88 项测试，不能把上一轮结果当作本轮新增能力验收。

## 3. hub 与单项目命令如何收敛

### 3.1 保留使用方式，收敛状态所有权

“单项目命令”需要拆开判断：

| 对象 | 建议 |
| --- | --- |
| 登记/查看/配置一个 workspace、从当前目录选择项目 | 保留。它们是必要的资源管理和便捷定位，不与 hub 重复。 |
| 从项目目录运行 `gld start` 一类快捷入口 | 保留便捷性，但目标应是启用统一 Runtime 中的这个项目，不默认再起一套平行会话引擎。 |
| 单项目分享、只读审查、临时凭据 | 保留，并改为 workspace-scoped grant/view；分享一个项目不暴露整个 hub。 |
| 每项目独立 MCP 端口、独立 tunnel、独立 session 状态 | 从默认路径移除。确需独立 listener 时只作为窄适配器，复用同一 Runtime 和授权内核。 |
| MCP 与 GPT Actions | 有实际消费者就保留传输适配器，不能各自维护一套业务状态。无人使用的旧兼容层可在核对后删除。 |
| 显式隔离部署的单项目实例 | 可以保留高级用法：不同 OS 身份/容器/数据目录有真实隔离需求，不等于另一套产品实现。 |

不要把“hub 化”理解成删除所有带 workspace 的 CLI，也不要把共享 `call_tool` 误称为已经共享进程和 session 生命周期。[S1–S4、S7]

### 3.2 建议的所有权结构

```text
外部 AI / 本机 CLI / 后续可选管理界面
                  │
        MCP / Actions / CLI 薄适配器
                  │
       AuthContext + workspace grant
                  │
        WorkspaceRuntimeRegistry
             ├─ Local Runtime
             │    文件、命令、Job、证据、资源生命周期
             │    └─ 已有 gld 工具 + toexec 纯原语
             └─ Remote Member Adapter
                  └─ ccnm 公共 bridge → 远端 Runtime
```

这里的 Registry 是现有服务中的内部所有权组件，不是新网络 Server，也不要求所有主机共享一个进程。

实施时把当前 `ToolContext` 中可共享的工作区执行资源与请求身份/传输视图拆开：**不能直接把携带某个 endpoint 认证信息的上下文全局缓存，然后让其他授权主体共享它。** 工作区资源可统一管理，任务和输出的读取仍逐次检查主体及 grant。

以规范化资源身份定位 Runtime，而不是展示名。不同别名指向同一目录不能获得两套互不知晓的写权限；Git worktree 的文件修改范围与共享 Git common directory 元数据分别处理。跨进程场景不能靠进程内 `Arc` 或 Mutex 宣称已经互斥。

### 3.3 删除旧服务路径之前必须满足的条件

1. 授权限制进入 `list_workspaces`、context、工具调用、Job/output/artifact/history，而不只是 UI 隐藏。项目 A 的主体猜出 B 的 ID 也不能读取 B。
2. 保留现有 audience 隔离；不把旧单项目 token 当作 hub 全权限 token。只给具体主体具体项目的授权交集，不自动扩大既有凭据权限。
3. 同一个项目经 hub、单项目视图和 CLI 操作时，遵循同一执行资源所有权与写入协调。共享资源不意味着每个主体可以读取其他主体的日志。
4. 明确区分“停止分享”“撤销授权”“停项目任务”“停整个服务”。撤销一个分享不能误杀其他主体的合法工作；停止项目执行也不能仅关闭旧端口而让 hub 后门继续执行。
5. 配置变化分类处理：名称/提示词变化不应无理由杀编译进程；权限撤销、root 变化需要明确终止或隔离相关任务。紧急撤销不能依赖下一次模型请求才生效。[S2]
6. 有现有客户端清单、迁移检查和至少一次真实客户端验证后，再删除重复 listener/context 管理代码。没有实际消费者的旧形态不必永久保留兼容层；有消费者则先迁移验证。

具体 CLI flag/子命令在实施阶段统一命名；上表是建议语义，不是声称这些新命令已经可用。README 最终应只有“默认 hub”和“受限单项目分享”两条主要路径，低层 listener/tunnel 选项放入 docs。

## 4. 目前能否完成完整本地开发

### 4.1 不能只按工具数量判断

完整性按三个层次判断：

- **编码闭环**：理解代码→修改→测试/构建→检查 diff。当前已有基础，但上一轮 P0/P1 缺陷降低可靠性。
- **日常项目开发**：还要管理长期进程、重连、交互输入、项目环境、浏览器/接口验收和交付证据。当前缺口主要在这里。
- **外部交付**：push/PR/CI、预览、发布或部署。取决于具体项目与授权，不是每个本地项目都必须部署到生产。

没有专用 Git 写工具，不代表不能 commit；能用 `git`、测试框架或浏览器脚本完成的任务，不必全部包装成新工具。真正应补的是长期生命周期、可恢复状态、权限和证据这些通用保障。

| 开发环节 | 当前判断 | 所需调整 |
| --- | --- | --- |
| 选择项目、读取规范和规划 | 已有 workspace/context/planning/history | 增量、作用域、接手摘要；不再另建计划状态系统 |
| 探索文件、搜索实现 | 已有原生工具，但有路径/glob/隐藏搜索问题 | 执行 U 修复与 RFC-0003 G3；再接语义导航 |
| 修改代码/文档 | 已有 patch；可靠性有明确缺陷 | 先完成版本前置条件、严格解析、失败定位和真实恢复状态 |
| 依赖安装、构建、单测、lint | 已可经受授权 exec 调用真实工具链 | 不混淆项目目录权限与包管理器缓存；补受控环境、输入和诊断 |
| 多服务、watch、dev server | 短时会话基础已有，非完整服务管理 | 独立 Job 生命周期、启动就绪、端口归属、日志、停止和配额 |
| 交互 CLI/调试 | 有 stdin/session，但 tty 不等于 PTY | 先修 EOF/初始输入；真 PTY 与 debugger 可按需独立支持 |
| 前端页面和浏览器验收 | 图片查看已有；未见本地原生浏览器操作面 | 先复用 Playwright 等受控工具，不自研浏览器引擎 |
| Git 审查与提交 | 原生只读 Git；写操作可走授权 exec | 选定路径/变更的审查、写入协调、提交前证据；不改全局身份 |
| 跨连接续作 | 有计划/历史持久化；命令 session 主要在内存 | 持久执行回执、失联对账、明确结果未知与显式恢复 |
| 报告/图片/二进制交付 | 文本、图像基础有；未见通用 artifact 面 | 有界上传/下载、内容 hash、来源、TTL 和项目授权 |
| CI/PR/发布 | 可借外部 CLI；当前命令策略会阻碍部分流程 | 只读 CI 诊断优先；写操作和生产部署单独授权 |
| 多人/多模型并行 | 路由不串项目，不等于不会同时写同一项目 | 单写者/资源租约；有真实需求后再加 managed worktree |

因此不能笼统回答“已经完全可以”或“现在不能开发”。对多数 CLI/库项目，现有工具常能完成实际功能；对长期运行的 Web 全栈项目，仍有明显的人工接续与验证缺口。[S5–S6、前置审查]

**gld 不调用模型，本身就不会在聊天客户端停止后继续推理。** 可以让已启动的编译继续，并保存结果；要让 Agent 自主多轮工作，必须显式接入官方 Agent 的执行路径，不能将后台命令冒充后台自主开发。

## 5. 应增加的核心能力

### 5.1 Execution Job：首先解决长任务与恢复

把一次 MCP 请求、命令进程、开发任务、计划步骤分开。复用已有规划/历史存储，只补执行实体，不创建第二套 Goal/Plan/Acceptance 系统。

最小 Job 应可 start/status/output/stop，并保存所属 workspace、发起主体、输入摘要、状态 revision、输出引用、实际退出状态及证据。通过请求 ID 对账；只读可安全重试，有副作用请求默认不因断线自动重放。

必须区分：请求已接收、进程仍运行、正在终止、已完成、已失败、失联、结果未知。超时和断连不等于没有副作用；服务重启也不意味着旧进程能被重新接管。只在原进程归属可验证时恢复观察，否则保留未知状态。

短命令可内联返回，长命令尽早返回回执。单次等待预算与任务运行预算分离；不要擅自改掉 RFC-0003 已有远端连接/租约语义。本地持久 Job 不自动证明远端 Job 已有同等恢复能力。

dev server 作为同一 Job 的受管类型增加 readiness、端口和停止规则。服务不就绪必须返回失败或仍等待，不能仅凭创建 PID 宣布启动成功。

### 5.2 项目任务、环境与证据

从 `package.json`、Cargo 配置等发现项目声明的 test/build/lint/dev 任务，返回命令来源与环境要求。**发现不执行，项目脚本也不是可信授权。** 不因存在 `npm test` 就绕过执行策略。

扩展已有环境诊断，区分程序未安装、PATH 不可见、策略拒绝、凭据缺失和执行失败。环境快照默认只记录必要工具链信息；不把完整环境变量、Keychain 或用户 HOME 配置送给模型。

验证记录绑定代码版本或工作树内容指纹、命令、退出码、输出完整性及时间。测试后又改代码，原测试证据必须标记过期；模型写的“已通过”不能替代实际记录。

### 5.3 浏览器、接口验收与预览

优先受控接入已有 Playwright MCP/CLI，保留纯 HTTP 测试脚本路径。第一版解决打开项目页面、操作关键流程、截图、console/network 错误和测试报告，不先做全桌面控制。[R9]

浏览器必须运行在能访问该项目服务的执行节点；远端 localhost 不是 hub 的 localhost。首轮使用独立 profile/test account、批准的 origin 和受管端口，不默认复用个人登录浏览器。网络、下载、`file://`、凭据与 DOM 内容的权限单独评估。

本地 dev server 不默认公开。用户查看预览需可撤销、短期授权，限制目的地址与端口，不能把预览代理做成任意内网访问入口。浏览器协议连通本身不构成安全隔离。

### 5.4 Artifact 与可恢复修改

统一产物登记、读取、导入和导出：报告、截图、覆盖率、日志、构建包共用元数据；不把大段 base64 或整个日志塞进对话。返回来源 Job、内容 hash、大小/类型、保存期限和访问范围。

导出只允许已登记产物；导入有大小、目的路径和内容类型限制。归档文件禁止路径穿越与逃逸 symlink。下载引用的持有不自动扩大 workspace 权限，日志脱敏和过期清理必须覆盖实际文件。

文件撤销应针对具体 change 的 before/after 状态；当前内容不再等于该 change 的结果时停止，不能覆盖人的新修改。不使用 `git reset --hard` 充当通用撤销，也不假装命令、数据库迁移和外部 API 副作用都能撤销。

### 5.5 语义导航和受控扩展

先按 RFC-0003 恢复 compact skills、增强搜索；再复用 Serena/LSP 提供 definitions/references/symbols 等能力。索引失效、语言不支持、服务未安装要明确返回，仍可回退文本搜索；不自研多语言语义引擎。[R10]

扩展配置由操作员管理，默认不自动安装/执行仓库里的 MCP 配置。每个扩展绑定节点、workspace、能力集、版本、资源上限和进程生命周期；新增工具或 schema 变化不自动扩大权限。

保持当前远端静态已评审工具的原则。渐进发现用于减少上下文成本，不是增加一个可调用任何上游工具的无约束 `call(name,args)`。核心常用工具保持明确 schema，长尾能力按需描述并在调用时复核。

### 5.6 跨模型接手和可选 Agent 委派

接手包保存目标、已确认决策、workspace/代码版本、变更、运行中 Job、验证结果、阻塞与下一步。只导入与该项目有关的历史，不扫描所有 Codex/Claude 对话；历史是数据，不是授权。

只读审查者和执行者可以是不同模型。审查通过读取真实 diff/验证记录完成，不相信另一个模型的完成宣称。[R7]

未来需要委派官方 Agent 时，优先走 ccnm 的既有公开管理入口，保留原拓扑和凭据边界；不得借鉴一个 repo 就把新模型循环塞进 gld。计划/委派必须显式显示目标节点、执行者和可能的模型调用，不把确定性工具调用偷偷升级为 Agent 调用。

## 6. 八个仓库可以借鉴什么

本节是设计采纳建议，不是产品性能排行；“上游描述有此能力”不等于本轮已经测过安全性与可靠性。

| 仓库 | 上游材料中的设计 | gld 的吸收方式 | 不照搬的部分 |
| --- | --- | --- | --- |
| **uvwt/agentdock** | 独立工具 Runtime；浏览器、长任务、Skills/动态 MCP，并以能力声明限制可选适配器。[R1] | 模块化 Browser/Skill 能力；统一启停、资源限额、发现与证据。 | 不复制多设备平台/ACP 全套，不把所有功能放进默认工具表。 |
| **yyjeqhc/webcodex** | 普通服务和临时单项目分享共用 Runtime；project-scoped credential；Job 与 Workflow Session 分离。[R2] | hub/单项目共用内核与状态；限定授权视图；执行回执与开发证据分开。 | 不部署其 Server/Runner 替换现有架构，不照搬全部 Agent/Goal 域。 |
| **opentokenz/mcpx** | 传输 Session 与持久业务 Session 分开；文件 SHA、执行 Task、Artifact、项目任务发现、按需扩展描述。[R3] | 补执行身份/回执、版本保护、task discovery 与 artifact；传输断开不抹掉工作事实。 | 不把 plan task 与 exec job 再混成 task_id；不因通用网关省 schema 就放弃工具授权。 |
| **leazoot/fylane** | 本机审批；`pending_approval`；可配置审批频率；修改后再变更时撤销停止；远端操作在本机审批。[R4] | 审批凭证绑定 workspace、操作内容和版本；待批操作可观察；冲突安全撤销。 | 不要求所有操作无限期阻塞一个 MCP 请求；不将其沙箱声明直接当 gld 的能力。 |
| **lifei6671/serena-desktop** | 项目符号/引用与 Agent 控制；技术方案以 receipt、revision 和有界 observe 分离调用与执行生命周期。[R5] | 语义导航插件与 revision-based observe；只读审查和明确委派边界。 | README 明确所有客户端共享活动项目：**不采用**；gld 继续每次显式 workspace。不为此恢复桌面大前端。 |
| **Waishnav/devspace** | 隔离 worktree、项目指令/skills、工具结果展示、开发 QA 状态与日用状态隔离。[R6] | 后期 managed worktree；立即采用独立测试数据目录思想，避免 dogfood 污染真实配置。 | 不把 worktree 当安全沙箱，不默认自动合并，不无条件替换现有 Windows 执行契约。 |
| **XiaoDuoYa/codex-with-chatgpt** | 控制面交换短状态，数据面按需读取；MCP 只读；执行后独立检查 diff 和测试记录。[R7] | 接手包、只读 review grant、证据驱动验收；避免在聊天中反复搬运全文。 | 不依赖网页 UI 自动化作为核心控制协议；不把其只读桥误当完整执行器。 |
| **alexanderradahl/mac-developer-bridge** | 真 PTY、分离的后台 Job、持久日志、只读历史查询、audit 与 kill switch。[R8] | 真实交互能力、可列举 Job、受限历史接手和操作员紧急停止。 | 其文档明确无沙箱、无命令/路径白名单：**不采用这一权限模型**；不默认接管个人浏览器全部登录态。 |

### 额外值得加入的维护设计

WebCodex 架构文档把传输适配器与同一 ToolRuntime 分离，并描述了用机器可检查的依赖边界约束 Cargo workspace。[R2] gld 可以采用**轻量模块依赖约束与 CI 检查**，但不为复制分层图大量新建 crate。

常用能力可以在本地/远端分别实现同一业务目标，但 schema、路径、租约和恢复语义不等价时必须保留差异。这比工具名完全一致更重要。

## 7. 后续阶段安排

阶段编号使用 **L0–L6**，避免与现有 U 系列、RFC-0003 G 系列冲突。本文不是新的权威进度存储；实施记录仍写入项目已有机制。每项必须有回归或端到端证据，不只改文档勾选完成。

| 阶段 | 范围 | 必要验收 / 停止条件 |
| --- | --- | --- |
| **L0 可靠性底座** | 先落实前置审查 U0/U1 的数据覆盖、解析、空 only、输出游标等；U2 的错误/版本契约作为共享基础。 | 失败不静默改坏文件；输出可终止；权限不因空配置扩大。未过不能扩大对外暴露。 |
| **L1 hub 收敛** | workspace grants；统一 Runtime 所有权；主体/endpoint 视图分离；stop/revoke 语义；简化默认 CLI。 | 单项目授权不能访问其他成员；同项目不同入口不形成平行执行盲区；撤销即时生效；再清理旧状态路径。 |
| **L2 日常开发执行** | Execution Job、stdin/EOF、必要 PTY、项目任务发现、环境诊断、最小 artifact 和验证证据；吸收 U3–U5 对应项。 | 测试/编译可断开后观察；dev server 可就绪/查询/停止；验证绑定代码快照；不因重连重复执行有副作用的操作。 |
| **L3 前端完整闭环** | 受控 Playwright/HTTP 验收、截图/console/network、报告、受限预览。 | 实际项目完成“启动→操作关键页面→记录错误/截图→修改→复测”；浏览器与服务在正确节点；不泄露登录态。 |
| **L4 上下文与扩展** | 合并 RFC-0003 G2/G3；按需语义导航、接手包、受控插件目录与 schema 变更处理。 | 不重复做 G2/G3；新会话可核对后续作；未授权扩展不启动；索引过期能识别。 |
| **L5 协作与外部交付** | managed worktree、选择性提交、CI 只读诊断、产物导出；需要时加轻量 operator console/审批队列。 | 保留用户修改；验证选定变更；push/merge/release/deploy 单独授权；界面只是同一服务的客户端。 |
| **L6 可选 Agent 委派** | 与 ccnm 管理入口显式对接，任务范围/预算/观察/停止/只读审查。 | 单独立项授权；不新增模型循环，不复制凭据，不并发争抢同一工作区写权。无需求就停在 L5。 |

优先级不由开源仓库功能数量决定。当前最值得先做的是 **L0 + L1 + L2**；前端日用目标再接 L3。Notebook 沿已有 G3 计划处理；PDF、全桌面自动化、插件市场、自动部署系统不因本轮研究自动加入近期范围。

## 8. “完整开发”验收场景

选择一个 TypeScript 前端/全栈 fixture 和一个 Rust CLI fixture，以真实工具调用记录验收。不要用几十个互不相连的单测代替用户流程。

| 场景 | 必须得到的证据 |
| --- | --- |
| 单项目快速接入 | 从目录登记到连接成功，scope 只有该项目；不额外暴露其他 workspace。 |
| 两个客户端交错调用 | A/B 项目不串；同项目写冲突按既定规则处理；Job 和历史不能凭猜 ID 跨主体读取。 |
| 编码与验证 | 搜索→读取→带前置版本修改→运行测试/构建→读取完整结果→审查 diff；后来修改代码使旧验证过期。 |
| 长期服务与中断 | dev server readiness、日志和端口可核实；客户端断线不重复启动；重连/服务重启分别有诚实终态。 |
| 前端验收 | 浏览器操作目标页面，保留截图和关键 console/network 证据；修复后重新验证。 |
| 交付与恢复 | 登记测试报告/构建包；限权下载可用；撤销过期，撤销修改遇到人工新改动时拒绝覆盖。 |
| 本地与远端 | 通过 ccnm 的单独契约运行；localhost、身份、租约和取消语义不混用；远端不可达不回退本地。 |

未具备真实平台条件的项目明确记为未验证；macOS 成功不自动代表 Windows/Linux 成功。OS 沙箱边界、工作区路径策略、命令白名单是不同层次，分别列出实际强制能力。

## 9. 参考快照与来源

以下 SHA 来自本轮公开 GitHub API 查询，用于定位后续复核。只记录与设计有关的事实，不以星数、README 宣称或 CI 绿灯代替实际采用验收。

| 仓库 | 参考提交 |
| --- | --- |
| uvwt/agentdock | `ad51001515a2b1b82baa31281970e0b9f67f28e9` |
| yyjeqhc/webcodex | `bf25761c92c72a68d0d8a3694d459c40a2591786` |
| opentokenz/mcpx | `3c6ba4a5a6cbffdcb7923521eae5bfd7aca4eec9` |
| leazoot/fylane | `6fcd4b3cc92e6f2e138f7d4b50758f225fa4ad0f` |
| lifei6671/serena-desktop | `09d2a697dac526e8ad8007c40dfe0705104b7525` |
| Waishnav/devspace | `531d3f973f09f7b6b4993c9ff58f80a4514b9ba2` |
| XiaoDuoYa/codex-with-chatgpt | `9663b88753e35c76796c5bce000293e0bd22cd9e` |
| alexanderradahl/mac-developer-bridge | `fea70d1a3c5524164f2159f6063ba685fef91324` |

- **R1 AgentDock**：[README](https://github.com/uvwt/agentdock)。核对独立 Runtime、browser、recoverable tasks、Skills/MCP、可选 ACP 范围。
- **R2 WebCodex**：[README](https://github.com/yyjeqhc/webcodex)、[Architecture](https://github.com/yyjeqhc/webcodex/blob/main/docs/ARCHITECTURE.md)、[Quick Trial](https://github.com/yyjeqhc/webcodex/blob/main/docs/QUICK_START.md)。核对统一 Runtime、Project Credential、Job/Workflow Session 和依赖分层。
- **R3 MCPX**：[README](https://github.com/opentokenz/mcpx)。核对传输/持久 Session、SHA/Edit/Task、Artifact 与动态扩展。源码树中可定位 `internal/remotesession`、`internal/projecttask`、`internal/terminal`、`internal/artifact`；本轮不据目录存在宣称实现正确。
- **R4 Fylane**：[README](https://github.com/leazoot/fylane/blob/main/README.md)、[Raw README](https://raw.githubusercontent.com/leazoot/fylane/main/README.md)。原始 HTTP 读取成功记录：gld operation `8765cbe5cbf642609602b2133d698fdc`。源码树存在 `companion/internal/approval/approval.go` 和相关测试，仅作为实施时继续审查的定位。
- **R5 Serena Desktop**：[README](https://github.com/lifei6671/serena-desktop/blob/master/README.md)、[Observe 技术方案](https://github.com/lifei6671/serena-desktop/blob/master/docs/codex-agent-observe.md)、[Runtime 技术文档](https://github.com/lifei6671/serena-desktop/blob/master/docs/codex-agent-runtime.md)。技术方案中的待办不能全部算作现有实现。
- **R6 DevSpace**：[README](https://github.com/Waishnav/devspace)、[Coding Workflow](https://github.com/Waishnav/devspace/blob/main/docs/chatgpt-coding-workflow.md)。源码树存在 `src/git-worktrees.ts`、`src/process-sessions.ts` 及测试。
- **R7 Codex with ChatGPT**：[README](https://github.com/XiaoDuoYa/codex-with-chatgpt)。核对控制/数据面、九个只读工具、独立 review 与单 workspace token。
- **R8 Mac Developer Bridge**：[README](https://github.com/alexanderradahl/mac-developer-bridge)。核对 PTY、后台 Job、历史读取、kill switch，以及明确无沙箱/白名单的限制。
- **R9 浏览器复用依据**：[Microsoft Playwright MCP](https://github.com/microsoft/playwright-mcp)。存在 MCP 与 CLI+Skills 两种接入方向；按 gld 实际客户端和资源边界选择，不固定追踪 latest。
- **R10 语义导航复用依据**：[Serena](https://github.com/oraios/serena)。与 Serena Desktop 分开评估，不把桌面封装当成唯一接入方式。

所有需要直接复用源码的项，先核对实际文件 LICENSE/NOTICE、依赖、支持平台和可维护性；本轮未复制上游代码。不据宣传语承诺“代码不离开本机”：被工具返回给外部模型的内容确实会传到该模型服务。

## 10. 给实施模型的指令

```text
读取前置 U 系列审查、本文、RFC-0002/0003 和当前 git diff；保留全部既有修改。
先修可靠性，再收敛 hub。不要直接删除单工作区命令或把旧凭据扩大为 hub 全权限。
目标是一份执行资源所有权、多种薄入口；请求主体、grant 和资源状态必须分离。
没有第二套 Goal/Plan，没有第三个通用 Workspace Server，没有新模型循环。
先完成 L0–L2，并用真实工具串联验证；前端日用再推进 L3。
现有 G2/G3 和 U3–U5 对应任务合并执行，不建立重复实现和第二份权威状态表。
上游只借鉴必要机制；使用具体源码前锁 revision、查许可、补回归，不照搬完整产品。
不自动安装扩展、复制凭据、修改全局 Git 身份、开放端口、push 或部署生产。
每阶段报告实际完成能力、未验证平台、结果未知项与证据；不要把提案写成已实现。
```

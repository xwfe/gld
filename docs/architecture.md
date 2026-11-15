# 架构

> **新授权的客户端接入仍在分步实现。**统一授权类型、稳定调用方身份、独立授权存储与新执行代契约已接入内核；
> HTTP MCP（包括单工作区、Hub、URL 和隧道入口）及原生节点转发暂以 `AUTH_NOT_READY` 拒绝。
> Actions 的独立认证仍支持。三类材料签发与轮换内核已具备，HTTP 验证待 B3，远端权限与租约待 C，统一 CLI 待 D；
> 这不是客户端接入完成或已上线的声明。范围与后续验收见 [RFC-0015](rfc/0015-unified-client-access.md)。

## 按信任边界组织 crate

| 目录 / crate | 当前职责 |
| --- | --- |
| `crates/policy` / `gld-policy` | 主体、授权范围、调用方键与普通文件写入分类；`no_std + alloc`，只处理显式输入 |
| `crates/protocol` / `gld-protocol` | 配置 DTO、本机 IPC 与执行代消息、版本和纯规则；仅依赖 serde / serde_json |
| `crates/transport` / `gld-transport` | 共用的 MCP Peer、stdio 通道、远端能力表；持有实际通信资源，不决定项目权限 |
| `crates/runtime` / `gld-runtime` | 工作区路径、文件/补丁、命令、会话、输出、写锁与运行记录；配置和存储位置由调用侧显式传入 |
| `crates/agent` / `gld-agent` | Agent 档案、任务存储与委派进程生命周期；消费 runtime 的私有文件、环境过滤与脱敏接口 |
| `crates/core` / `gld-core` | 工具授权、任务/Planning、Agent 审批/审核、授权存储与应用编排，消费五个底层 crate |
| `crates/host` / `gld-host` | 执行代 Router/Link、MCP/Actions HTTP、OAuth、网关、Hub/工作区监听管理与节点连接；复用 core 的执行与凭据存储接口 |
| `crates/daemon` / `gld-daemon` | 本机 IPC、请求分发、守护进程主循环与生命周期，消费共享协议 |
| `crates/cli` / `gld` | 唯一二进制：clap 解析、后端选择、终端渲染 |

依赖为 CLI → daemon/core，daemon → host/core/protocol，host → core/protocol，core → policy/protocol/transport/runtime/agent，agent → runtime，runtime → policy/protocol；五个底层 crate 不反向依赖 core。
`crates/core/tests/crate_boundaries.rs` 用 Cargo metadata 检查底层依赖及 core 不反向依赖宿主，随全仓测试执行。
`core::auth` 重导出 `gld-policy` 的主体与授权类型；`core::tools::caller/write_class` 和
`core::bridge::peer/session` 保留类型重导出，OAuth HTTP 入口归 `gld_host::auth`。
crate 拆分保留的 Rust 导入路径不代表旧授权兼容；D18-A 已移除旧 grant 模型与明文凭据 DTO，协议版本见下文。
Agent 和宿主的归属与迁移顺序见 [RFC-0014](rfc/0014-trust-boundary-crates.md)，状态只看 [task_plan.md](../task_plan.md)。

core 不负责 clap 命令行解析，
但仍包含应用编排、节点客户端、隧道工具和本机 MCP 子进程管理。Agent 进程归 agent，工作区命令执行归 runtime；
daemon 负责守护进程自身的 IPC 与生命周期，cli 负责参数解析、请求组装和终端展示。不能把“core 不依赖 daemon”
写成“core 没有网络或进程代码”。纯规则按已建立的边界下沉，应用编排留在 core；契约测试继续约束入口一致性。

```text
用户
 │  gld start -w api
 ▼
cli::commands::service::start          组装 Request::StartService
 │
cli::backend::Backend                  守护进程在跑？→ 转发；没跑且需要 → 拉起后转发；否则进程内直连
 │
 ├─ Remote ─▶ daemon::client ──socket──▶ daemon::server ─▶ daemon::dispatch ─▶ core::app::App
 └─ Local  ────────────────────────────────────────────────▶ daemon::dispatch ─▶ core::app::App
                                                                                  │
                                                                 core::runtime / tunnel / mcp / actions …
```

两条路径在 `dispatch` 汇合，复用应用用例；但身份、配置、进程寿命和内存会话可能不同。
特别是 CLI 直连模式不能让下一次新进程继承命令会话（只留下运行记录：读得到结局和输出，接不上进程），
不能仅凭共用代码断言端到端行为相同。

## policy：身份与授权

`gld-policy` 只接收显式输入，不访问磁盘、随机源或时钟。`Grant` 包含稳定 ID、展示名称、
`ProjectScope`、`GrantPermissions` 和 `GrantStatus`；凭据摘要、有效期与修订号留在 core。
`AuthContext` 保存已验证的当前权限，不再用“没有 grant”推断全权：

| 身份 | 稳定身份内容 | 权限来源 |
| --- | --- | --- |
| `LocalOperator` | 本机操作员 | 可信本机应用 / IPC 显式构造；网络和执行代身份消息不能声明此变体 |
| `McpClient` | 入口、Grant ID、OAuth client_id / Token / URL / 隧道绑定主体 | 当前有效 grant、凭据种类及本次项目策略 |
| `ActionsClient` | 入口、配置中的 workspace_id、匿名 / API key / OAuth client_id 主体 | 明确的单工作区权限和 write 设置；没有 MCP 服务能力 |

`CallerId` 的相等与哈希只取稳定身份；grant 名称、项目范围、write、服务能力和凭据修订号不进入键。
同一 OAuth grant 的不同 client_id 分开；Actions 同一 client_id 或 API key 主体在不同 workspace_id 下也分开，
同一工作区收紧权限则保持原身份。命令会话、运行记录和任务归属消费同一稳定键，权限仍逐次复核。

默认 `AllProjects` 表示当前及后续登记项目，`Selected` 保存非空、去重的项目 ID；两者都还要与当前成员目录、
项目配置和上游限制相交。默认 write 为 false，服务能力为空。只有 AllProjects 可显式带
`user_prompts`、`machine_mcp`、`managed_workspaces`；调用机器 MCP 与管理托管空间还要求 write。
托管空间仍按调用方所有权检查，不因 AllProjects 变成公共项目。URL 凭据禁止服务能力和 Agent 委派，
OAuth / Token 委派仍要求 write 及项目既有审批规则；Actions 不借用这些 MCP 权能。

## protocol：数据契约与适配边界

| 模块 | 接入面 |
| --- | --- |
| `config` | Hub、代理、FRP、全局入口和任务容量配置；默认值、旧字段别名与 profile 归一化 |
| `daemon` | `Request / Response / RpcError`、协议版本及请求引用的用例 DTO |
| `generation` | `BuildIdentity`、执行代帧、操作、路由与身份/节点快照；保持内层 JSON 字符串编码 |
| `planning / service` | 进程间共用的状态与服务选择类型；账本、服务实例和资源校验留 core |
| `tool_args` | 工具整型参数的默认值与上下界；core 和 runtime 共用，工具注册表和未知参数校验留 core |

数据契约只有一份定义，core 和 daemon 通过 re-export 共享它。下列 Rust 方法入口随职责调整，
工作区内的调用者已同步迁移。D18-A 的授权与身份形状另作不兼容切换，不沿用 D17 拆 crate 时的协议不变承诺。

| 原入口 | 现在的归属 |
| --- | --- |
| `FrpProfile::new` | `core::settings::new_frp_profile` 生成随机 ID，纯 DTO 不读取随机源 |
| `BuildIdentity::current` | `core::generation::current_build_identity` 读取构建、进程和可执行路径 |
| `WireAuth::from / into_auth` | `core/src/generation/wire.rs` 的 `auth_to_wire / auth_from_wire`，在显式 MCP / Actions 变体与已验证上下文间转换 |
| `ServiceKind / TunnelServiceKind::workspace_service` | core 的 `WorkspaceService::from` 适配；资源模型不进入协议 |
| 服务类型的 `parse` 错误 | protocol 的 `ParseServiceKindError`；core 转成 `AppError`，原消息保持 |

`FrameWriter / read_frame` 仍在 core 的执行代管道适配中；节点登记、凭据、撤销与进程生命周期也留实现侧。
`WireAuth` 是已认证结果的数据表示，反序列化它并不完成网络认证。它只有 `Mcp` 与 `Actions` 两个严格变体：
MCP 必须携带 grant、凭据种类与主体；Actions 必须携带 workspace_id、主体与 write。缺字段、旧结构和
不匹配的主体被拒绝，没有 LocalOperator 变体。执行代还核验操作与身份种类一致，以及 Actions 的目标工作区一致。

| 契约 | 当前版本 | 边界 |
| --- | --- | --- |
| daemon `PROTOCOL_VERSION` | 19 | CLI 与 daemon 须匹配；grant DTO 只返回新模型的授权及凭据元数据 |
| `GENERATION_PROTOCOL` / `GENERATION_PROTOCOL_MIN` | 2 / 2 | 只接纳明确主体的新执行代消息，不与协议 1 混跑 |
| `STATE_FORMAT` | 2 | 不匹配格式的执行代不能进入可调用集合；旧授权不会因此被导入 |

版本核验在换代前完成。新增协议字段先改此 crate，再更新分发和拒绝旧版本的测试，不让 protocol 引用实现层来解决依赖。
新存储的 schema_version 独立为 1，不能把它与执行代的 STATE_FORMAT 混为一个版本号。

执行代收到 Hello 后先检查宿主 generation_protocol，再读取 DataStore。宿主 Link 收到首个 Ready 时
立即检查执行代的协议和状态格式，通过后才处理 Reply 或 HostCall；提前发业务消息或重复 Ready 都关闭连接。
Router 保留准备和切换时的身份复核。这是双方的拒绝边界，不增加旧格式转换；Hello 本身不携带 STATE_FORMAT。

## runtime：工作区与命令执行

| 接口 | 职责与接入约束 |
| --- | --- |
| `workspace::Workspace` | 规范化根目录、读写路径与符号链接检查、保护目录过滤、文件状态和版本；默认限制读取范围 |
| `file / image_tool / notebook` | 实际读取、列表、搜索和解码；文件实现按读取、列表、搜索拆分，复用已有有界读取原语 |
| `patch::PatchRequest::from_args` | 解析参数形状；在获取写锁之前执行，保留参数错误的原有优先级 |
| `patch::apply` | 在调用侧持有的写锁内解析补丁、校验版本、暂存和提交/回滚；返回工具结果与提交前版本 |
| `wrote_since / on_commit` | 两个具体回调查询同主体版本链、通知提交成功；由 core 连接同一 `WorkspaceRuntime` |
| `command_spec::CommandSpec` | 统一 cmd/argv 与词法解析；只返回输入错误，core 映射为既有策略原因 |
| `exec::Execution / ExecutableConfig` | 接收工作区、共享执行资源、两个本地入口设置、合成 PATH 和需移除的应用环境变量名；执行入口另接 caller 对应的会话表 |
| `session` | 命令状态、stdin、停止、有界输出和列表；按进程、输出、控制、分页拆模块 |
| `runs::RunLog` | 显式记录根目录、项目 ID、caller；持久化结局和有界日志，提供唯一进程 owner 身份 |
| `workspace_runtime` | 按 canonical 目录持有共享写锁、caller 会话表、写计数、版本链及交接状态 |
| `service / redaction / async_rt` | 原有端口就绪检查、环境/记录脱敏、同步到 Tokio 的执行机制；各只有一份实现 |

core 的 `tools::new_workspace(root)` 解析 GLD_HOME，再调用
`gld_runtime::workspace::Workspace::new(root, protected_read_roots)`。
保护目录在创建时保留原始与 canonical 路径；即使关闭读范围限制，直接读取与遍历仍会挡住工作区之外的受保护路径。
位于显式工作区内的路径沿用原有豁免。runtime 不读取应用配置或自行解析 GLD_HOME；命令执行仍按既有规则读取、过滤进程环境。

原 `core::tools::workspace::Workspace` 类型路径和文件工具路径通过 re-export 保持；
原单参数 `Workspace::new(root)` 的调用者需改用 core 工厂，直接使用 runtime 的调用者必须传入保护目录。
MCP 结果包装继续归 core。runtime 的拆分保持工具结果和执行机制；D18-A 的身份与协议变化以上节契约为准。

网络入口仍先验证身份与项目权限，工具调用经 `call_tool_as` 完成参数策略、Planning、Agent 互斥和任务检查。
core 的补丁适配持有现有同目录写锁，调用 runtime，随后在同一把锁内更新调用方版本链并生成运行命令提醒。
成功落盘后立即增加写计数，再采样新版本；预检和失败不增加计数。
命令、会话、运行记录和 `WorkspaceRuntime` 归 runtime；任务账本继续归 core，输出工具观察到终态后仍由统一分发收尾。

### 命令与运行状态的组合

core 的 `tools::workspace_runtime::runtime_for(root)` 选择写锁目录、运行记录目录和项目 ID，
再调用 runtime 工厂。具体输入为 `RuntimePaths { write_lock_dir, runs }`，其中
`RunStorage { root, project_id }` 使用原 Harness 的 ID 算法；runtime 不复制算法，也不查询任务账本。
同一个 canonical 目录的注册表只有一份，后续上下文复用首次创建时的资源，不因入口或配置重建产生第二把锁。

命令实际解析和启动使用 `Execution` 的具体参数；整份 `PolicySettings`、授权结论、Planning 和 Harness 不进入执行器。
策略与语法错误顺序、原生内建和健康探针的特殊路径、同步等待持锁范围及现有 JSON 均保持。
`CommandSpec::from_args / resolved_parts` 的 Rust 错误类型改为 `CommandSpecError`，
core 显式转换为原 `PolicyError`；其他命令、会话、运行记录的旧导入路径继续可用。

runtime 的 `handoff_blockers()` 只报告运行命令、未排完的输出与在途写入；
core 的执行代组合入口合并 Agent 状态，保留原阻塞顺序。撤销仍分别停止匹配的命令和 Agent；
退出仍写 `interrupted`。未知 owner 的旧结果保持未知写计数，不能当成有效的零写入证据。

Agent 继续消费同一份 owner 锁、私有文件原语和环境过滤实现；其进程生命周期归 agent，审批仍归 core。
全局 Tokio 运行时也由 runtime 单独持有。crate 默认拒绝 unsafe，只有
`session/process_control.rs` 局部允许原有 libc / Win32 系统调用。

新增文件或命令能力在 runtime 实现，再接入 core 的工具表和统一分发；单元测试跟实现走，授权、并发与端到端契约留实际调用侧。
不把 runtime 的直接 Rust 调用当成独立授权入口。Cargo 依赖检查约束代码归属，子进程权限仍按现有机制执行。

## agent：档案、记录与进程生命周期

Agent 执行机制放在 `crates/agent`：`profile` 读取显式指定的档案路径并拼接参数，`store` 保存任务记录，
`run` 持有唯一 `LIVE` 表、子进程与监视线程，负责超时、停止和运行前后的 Git 快照。
它依赖 runtime 的已有环境过滤、脱敏和私有文件原语，不读取 GLD_HOME，也不反向查询应用层。
Unix 进程组信号沿用原实现，仅 `run::signal` 局部允许 unsafe；Windows 委派仍按原行为返回不支持。

core 的 `agents` 保留项目 ID、数据目录和 owner 锁的组合，负责提议、幂等、审批/拒绝、撤销、互斥与审核；
`agents::results` 负责有界结果、报告和 diff 分页。工具入口继续核验主体、项目名单和 Planning；
换代入口继续汇总 Agent 阻塞状态，验收仍只认 gld 自己运行的命令证据。
直接使用 agent crate 不是新的授权入口，不提供 OS 或网络沙箱。

旧 `core::agents::{profile,store,run}` 类型路径通过重导出保留；直接构造 `run::Launch` 的 Rust 调用者
需提供 `remove_env: Vec<String>`。core 显式传入 GLD_HOME，正式委派子进程无条件扣下它，即使档案
`env_pass` 点名也不放回。版本探测和 Git 快照子进程沿用原行为，不把这条规则扩大到所有子进程。
任务 JSON、档案配置、MCP 回包、命令行帮助和 daemon 协议均不因拆分改变。

## core：内核与服务层

| 模块 | 职责 |
| --- | --- |
| `tools/` | 统一工具编排与 runtime 文件/补丁/命令适配、策略预检、Git、History、Planning、Skill，以及项目命令发现（`tools/project_commands/`：只读解析 Cargo、package.json 和 CI 工作流；`installs.rs` 判断哪些命令是在装东西、要不要 `confirm`，`policy` 调它）。运行记录、会话和服务端口机制的旧模块只保留兼容入口。`tools::call_tool`（带主体的是 `call_tool_as`）执行工具，`tools::build_tool_context` 构建上下文。MCP 与 CLI 复用内核，但还需验证各自身份、配置、schema 和结果传递 |
| `mcp/` | MCP 应用分发：工作区的本地请求和执行代共用同一个 JSON-RPC 入口，不负责 HTTP 与 OAuth |
| `machine_mcp/` | 本机装好的 MCP server 经服务转给 AI（RFC-0006）。读配置、握手调用、子进程通道、连接池、结果整理在共享库 `toexec-mcp`（ccnm 同用）；这里是 gld 自己的：怎么起进程和怎么杀、HTTP 通道、`list_mcp_tools` 等三个工具。自成一块，hub 只在列工具、分发、关服务三处调它 |
| `hub/` | **就是那个唯一的 MCP 服务**（命令行里叫"服务"，内部名字没改）：一条连接按每次调用的 `workspace` 参数分到各个项目，每个项目一份独立的 `ToolContext`；隔离规则写在 `hub/mod.rs` 开头。`gld-host::hub_runtime` 管宿主起停，连同服务自己的隧道和原生节点表 |
| `auth/` | 独立 AccessStore、凭据摘要/有效期、撤销与共享授权快照；纯主体和范围来自 `gld-policy`。保留独立 OAuth 客户端注册表，Actions 仍使用它；新 MCP 签发和验证待 B |
| `bridge/`、`nodes/` | CCNM 会话、原生节点的身份/项目权限与连接生命周期；共同使用 `gld-transport` 的 Peer 和能力表，保留各自的租约、单连接和断线语义 |
| `runtime/` | 应用侧监听接口与同步协调锁；实际 RuntimeSupervisor、监听任务和端口状态在 `gld-host` |
| `tunnel/` | frpc / cloudflared 子进程：项目的 GPT Actions 那几条归 `TunnelSupervisor`；服务和旧的全局入口各一条，归 `tunnel/standalone.rs`（跟着各自的监听器起停） |
| `planning/`、`harness/` | Goal / Plan / Execution Ledger；Durable Task |
| `data/`、`settings/`、`workspace/`、`secret/` | `profiles.json` 的组合模型与读写，纯共享配置类型来自 `gld-protocol` |
| `platform/` | 端口占用查询、进程存活 / 终止、可执行文件查找，按 OS 分实现 |
| `home.rs` | 数据目录解析（`GLD_HOME` / `~/.config/gld`），全项目唯一的路径来源 |
| `async_rt.rs` | 重导出 runtime 的 tokio 垫片；全局运行时只有一份，同步工具借它驱动子进程 I/O |
| **`app/`** | **应用服务层**：`App` 持有 `DataStore`、懒加载的唯一 AccessStore 和命令行工具调用的 `ToolContext` 缓存；监听状态由宿主显式传入 ListenerManager，每个子模块是一组用例；`app/doctor.rs` 的纯配置检查不依赖磁盘，可直接单测 |

`app` 是应用用例门面；工具传输另经上表中的工具分发入口。桌面版里的 Tauri command
在这里改成普通的 `impl App` 方法，供命令行、守护进程和测试复用。

`app::workspace_fields` 是一张“可设置字段”表：命令行帮助、`gld fields` 输出和实际写入
都从同一张表来，加字段只改一处。三个约定：

- 字段名不写前缀时补 `mcp.`，所以新增项目字段一律叫 `mcp.<名字>`；GPT Actions 那条线路的叫 `actions.<名字>`。
- 单项目 MCP 监听器那一半（端口、认证、隧道……）已经退役，列在 `RETIRED` 里：`gld set`
  遇到它们报错并指向服务级命令。退役的不能再出现在字段表里，有测试守着。
- 取值要查工作区之外的数据（比如 `frp-profile` 得确认那个 id 存在）时，
  用 `field!` 宏的三参数写法拿 `FieldContext`，别在 `apply` 里直接读全局状态——
  那样就没法只对着一个 `WorkspaceProfile` 做单测了。

`App::set_workspace_fields` 存完会重启受影响且正在运行的服务
（按 `service_config_changed` 逐侧比 JSON 快照）。加了新字段就自动被覆盖，
不需要额外登记"这个字段要不要重启"。

### 应用状态与监听管理

宿主先 `App::load()` 读取数据并补齐共享密钥；`gld_host::listener_manager` 显式创建 ListenerManager 与工作区监督器。
管理器不读配置，应用方法只通过 `ListenerHost` 和 `WorkspaceListeners` 使用实际需要的生命周期操作。
daemon 的状态查询、请求分发、恢复与退出共用一个 Arc，CLI 的直连 Backend 也在第一次调用时创建并缓存一对资源。
运行态相关的 App Rust 方法显式接收 `&ListenerManager`，不在方法里临时创建，也没有隐藏的默认管理器。

App 保留数据文件锁、重读磁盘和工具上下文失效规则。ListenerManager 持有宿主监督器的同一同步锁，另保留
工作区重启锁与一次恢复标记；失败也消耗本次恢复机会。停止先发信号，放锁等端口，再摘表；Hub 的 LIFECYCLE 仍独立，
不把普通 start/stop、恢复或退出放进新的重启大锁。工具/委派的执行位置仍按执行代入口另行组合。

### 授权存储与撤销

`App` 首次授权操作时打开 `AccessStore`，之后复用同一个实例；普通项目配置保存不会提前打开它。
`App::access_reader()` 返回共享快照读取端。授权独立存于 `<数据目录>/access/v1/grants.json`，
格式是 `schema_version: 1` 与记录列表，每条记录包含 Grant 和 CredentialRecord；摘要与时间校验属于 core，
policy 不读取此文件。

存储持有 `grants.lock` 生命周期独占锁；最后一个共享读取端释放前锁也不会释放。单写端序列化修改，
读取端取得当前不可变 `Arc<AccessSnapshot>`，按 grant ID、凭据 ID 和名称索引查询，不逐请求重读 JSON。
写入先构造并验证候选快照，在同目录创建私有临时文件、写入并同步，再以 rename 作为提交点；
提交后发布已准备好的内存快照。提交前失败保留旧文件和旧快照，不能先改内存再尝试保存。

`AccessReader::authenticate_token` 每次按公开凭据 ID 读取当前快照，检查 Token 用途、Active 状态、
期限与秘密摘要，再生成本次 AuthContext。CallerId 只用于归属，旧快照不能作为后续请求的认证缓存。
HTTP 的来源提取、格式分派和三类入口接线仍由 host 完成；此 core 接口不开放网络，也不接受 OAuth、URL 或隧道证明。

新文件严格拒绝未知字段、旧结构、重复 ID、非法有效期、降修订和撤销记录重新激活；读坏文件不会回退成空授权或覆盖原件。
旧 `profiles.json` 的 grants 字段只作为脱敏 Debug 的 opaque JSON 原值保留，普通配置保存保留其内容，
缺省时不创建该字段；它不是验证源，也不会自动写入 AccessStore。切换后的客户端必须重新授权，旧凭据不会恢复全权。

凭据只保存摘要与校验元数据，不保存签发明文。`GrantDto` 只返回 id、name、scope、permissions、status，
以及 credential 的 id、kind、revision、issuedAtUnixSecs、expiresAtUnixSecs；列表和 Debug 不带摘要或秘密。
列表也包含撤销状态，读到一条记录不等于它能通过认证。创建旧 grant / browser grant 的 App 用例暂返回
`AUTH_NOT_READY`，不写旧字段；统一命令与客户端交付留待 D。

`auth/credential.rs` 共用用途格式、期限解析和 ring 随机源；秘密为 32 字节随机值，
公开定位 ID 使用 UUID，解析只接受固定版本/用途及规范 base64url，不尝试其他用途。
`AccessStore::create_grant` 与 `rotate_grant` 通过既有原子提交边界返回 `IssuedAccess`；后者在写锁内
读取当前记录、检查修订溢出并只替换凭据，避免回写过时权限。返回类型不序列化整条存储记录，
`IssuedSecret` 的 Debug 脱敏且不实现 Clone/Deserialize；应用交付单独投影元数据与秘密。
快照另存初始化标记，区分缺失新文件与合法空文件；只有成功提交才将缺失状态改为已初始化。

撤销先提交新存储并发布 revoked 状态，再按 Grant ID 清理对应命令、待审批和运行中的 Agent 任务，
并保留既有延迟复查。其他 grant 不受清理影响；同名重建不能复用已撤销 ID。

### daemon 显式选择执行位置

`DaemonContext` 持有启动时创建的同一个 `Arc<Router>`。工具、批准/停止委派和升级从这里选执行代，
直连请求没有这个上下文，只调用 App 的本地入口；任何路由错误都不转成本地执行。
daemon 把同一 Router 交给 `gld_host::listener_manager`。HTTP 监听在宿主直接选路，宿主 Hub 直接使用 Router
配置各代、清理与读取已发布工具表；不查全局 Router，也不再需要跨 crate 的 ServiceGeneration。

App 的 Rust 本地入口明确命名为 `call_tool_local`、`approve_agent_task_local`、`stop_agent_task_local`、
`remove_grant_local`、`set_planning_mode_local`。宿主转发工具时在阻塞线程里重新读当前目录，不使用较早的 selector 快照；
批准只取任务记录的根目录，不额外读取报告或 diff，返回的任务仍先按原类型反序列化。

撤销和 Planning 分别使用各自的 `*_with_cleanup` 窄回调：App 负责先落盘、错误文案与清理时点，
daemon 闭包只选择实际执行资源。撤销仍立即清一次、35 秒后用同一 Router 再清，每次重新查询可路由执行代；
Plan 清理失败时保留已保存的模式和原报错。

退出由 daemon 按关闭 IPC 监听 → Router interrupt_all → App shutdown_services → 宿主网关 stop → Router shutdown →
清 IPC/进程记录的顺序组合，先记 interrupted 再停服务，不改变启动恢复记录。
App 的初始化、目录、凭据与日志仍消费既有存储适配。

Rust 的具体路由器入口现为 `gld_host::generation::Router`。旧的 `core::generation::{install, router()}`
与依赖全局路由的 `generation::exec` 已移除；调用方从持有的 Router 调用，直连则走显式本地入口。
不保留遇到路由失败时会静默转成本地执行的兼容替身。旧的 `generation::router::root_key` 仍是同一算法的重导出。

### HTTP 监听和凭据读取

`gld-host` 提供 MCP（单工作区和 Hub）与 Actions HTTP 入口并持有实际监听状态。core 的 `ListenerHost` 只暴露现有 Hub 与网关控制，
`WorkspaceListeners` 支持应用在原锁内读快照、推进状态。装配凭据的读取失败与监听器绑定失败仍分别处理。
daemon 和 CLI 直连显式构造实际 adapter；只有 daemon 的 adapter 有执行代。

D18-A 的标准 MCP / AuthMCP 和新 URL 入口统一返回 HTTP 503、`AUTH_NOT_READY`、`Cache-Control: no-store`；
旧服务口令、bearer、旧 OAuth token、loopback、隧道或缺省 grant 都不能绕过它。MCP 的 noauth 配置也拒绝。
这保留监听和路由结构，但不会把未完成的网络验证伪装成已验证 MCP 主体。

Actions 继续走自己的 OAuth / API key / 显式匿名配置，使用独立单工作区 AuthContext；
OAuth 动态注册、PKCE 和刷新仍由宿主与 core 的 `ClientRegistry` 协作，内存授权码只随当前 HTTP 会话存活。
旧 `StoredGrants` 已移除；新 MCP 验证器接通前不以 Actions OAuth 或旧配置替代 grant 验证。

全局网关进程内状态、HTTP 转发和退出归 `gld_host::global_gateway`。App 留网关配置读写；Hub 或工作区服务在绑定之前
通过同一个宿主入口确保已启用的网关启动，失败不绕过网关直接绑定。`public_url` 回写继续使用原 DataStore 文件锁；
宿主网关复用活动 Hub 的 Nodes，不单独启动节点连接。

### 宿主与执行代之间的请求归属

内核的 MCP 路由契约是：验证身份后调用 `Hub::execution_route(auth, body)`，
将明确的 `ExecutionRoute` 传入 `Router::hub_mcp`；Router 只按归属选择执行代，不接收整份 Hub。
D18-A 的 HTTP 验证入口尚未放行这条路径，不能据此声称网络 MCP 已接通。
执行代通过严格 WireAuth 重建上下文，并调用 `Hub::handle_routed` 复核，避免配置变化时同一目录被两代执行。

同目录的归属键由 `generation::root_key` 生成：路径可解析时规范化，无法解析时保留原路径。
`generation::router::root_key` 仅为旧 Rust 导入路径的重导出，没有第二份实现。
完整宿主依赖及迁移依据见
[RFC-0014 的宿主依赖分析](rfc/0014-trust-boundary-crates.md)，实施进度只看 task_plan。

### 原生节点连接的所有者

每次启动 Hub，由宿主的 `HubRuntime` 创建一张 `Arc<Nodes>`，将同一张表交给 Router、HTTP 监听器和
Hub 的 `NodeBridge`。直连 WebSocket 从监听器自己的 state 取得它；全局网关和 daemon 管理操作仍通过
`gld_host::hub_runtime::nodes()` 取得当前运行实例，两个入口不会各建一张表。

应用 Hub 只调用现有 `NodeBridge::{members,call}`，不创建或关闭实际节点连接。宿主用 Nodes 的实现，
执行代用 HostBridge 私有管道；节点自身的 Hub 不转发到其他节点，也不再额外创建空表。
离线成员和读取清单失败的结果仍由原 Nodes 实现提供，不改成空结果或静默忽略。

停服务仍先停监听、收回维护任务和隧道，再关闭执行代 Hub 并摘掉 Router 的节点表。
宿主随后按原顺序结束工作区命令、关闭 CCNM、显式 `Nodes::close_all()`，最后关闭本机 MCP relay。
私有 `shutdown_with` 只为保留这个顺序提供一次节点清理位置；`Hub::shutdown()` 不关闭注入的桥，
NodeBridge 不新增关闭方法，丢弃 Arc 也不能替代显式关闭。执行代替换不关闭宿主的节点连接。

宿主使用 `Hub::new(config, bridge)` 并持有 Nodes，不再经 `Hub::nodes()` 取连接表。
直接监听器与应用桥由同一处装配，使用同一个节点表。服务端登记、配对 HTTP 适配和 socket 接入归
`gld_host::nodes`；节点本地执行、配对持久化、共享传输和成员模型留 core。
D18-A 的原生节点身份转换和转发明确返回 `AUTH_NOT_READY`，没有旧主凭据映射或本机执行回退。
C 必须接通新 grant 与目标机共享权限的交集，并验证旧节点拒绝行为后，才能恢复客户端经节点执行；
CCNM 写入也要先完成按 CallerId 隔离的唯一写租约，不新增独立 RemoteCoding 服务能力。

## daemon：协议与进程

| 模块 | 职责 |
| --- | --- |
| `protocol.rs` | 重导出 `gld-protocol::daemon` 的 `Request`（`op` 标签的枚举）、`Response`、`RpcError` 与版本 |
| `ipc.rs` | Unix domain socket / Windows 命名管道；一行 JSON 一条消息 |
| `dispatch.rs` | `Request → App 调用 → JSON`，协议与业务的唯一交界 |
| `server.rs` | 单实例锁、绑定、每连接一个任务、信号、优雅退出 |
| `client.rs` | 连接、发一行、读一行、超时 |
| `lifecycle.rs` | 文件路径约定、探活、后台拉起（`setsid`）、停止、残留清理 |

设计取舍：

- **一个连接一个请求**：不做多路复用，也就不需要帧格式和请求 id，`nc` 就能调试。
  日志跟随（`-f`）在客户端侧直接读文件，不占用连接。
- **协议是枚举而不是 `method: String`**：编译器保证 `dispatch` 覆盖每个变体，
  新增请求忘了处理会编译失败，而不是运行时 “method not found”。
- **`needs_daemon()` 定义在协议上**：哪些请求必须由守护进程执行是协议本身的属性，
  命令行只是消费它。
- **版本核对在转发前**：`DaemonInfo` 带 `version` 与 `protocol`，不一致就拒绝，
  避免新命令行给旧守护进程发它解析不了的枚举变体。

## cli：只管解析和展示

| 模块 | 职责 |
| --- | --- |
| `cli.rs` | clap 派生定义与全部帮助文本 |
| `backend.rs` | 选 Remote / Local，自动拉起，版本核对，超时策略 |
| `output.rs` | `--json` 直出、表格（按东亚宽度对齐）、脱敏、着色 |
| `commands/*.rs` | 每组子命令：组装 `Request` → 调后端 → 渲染 |
| `error.rs` | 退出码约定（1 失败、3 守护进程未运行、4 版本不一致） |

`commands/daemon.rs` 是唯一不经过 `Backend` 的命令组：它要在守护进程不响应或版本不一致时依然能工作。

## 并发与锁

- `App` 的配置与工具缓存、ListenerManager 的监听表分别使用 `std::sync::Mutex`，同步锁不跨 `await`；
  DataStore 的配置读改写仍在文件锁内先重读磁盘。授权用 AccessStore 的生命周期独占锁和共享不可变快照，
  不套用配置文件逐操作重读的路径。
- 跨 `await` 的串行化用 `tokio::sync::Mutex`：ListenerManager 的工作区重启锁（MCP / Actions 的 stop→start）
  和 `tunnel::supervisor()`（frpc / cloudflared 的进程操作）。
- 守护进程每个连接一个 tokio 任务，`dispatch` 再包一层 `spawn`：业务代码 panic
  只影响那条连接，并转成 `RpcError::Internal` 回给客户端。
- 工具内核是同步函数，被 `spawn_blocking` 包着跑；它内部通过 `async_rt::block_on`
  驱动子进程 I/O。这是从桌面版继承的约束：`block_on` 不能在 tokio worker 线程里调用。
- **改同一个工作区目录的写操作排一个队**，归 `runtime/src/workspace_runtime.rs` 管：
  一个目录一个 `WorkspaceRuntime`，按规范化路径认人，所以 hub、单项目 listener
  和 CLI 三个入口（三个 `ToolContext`）排的是同一个队。跨进程那一层是数据目录
  `write-locks/` 下的文件锁（`fs2`），进程死了内核自动放。等锁最多
  `WRITE_LOCK_WAIT`（30 秒），超了报 `WORKSPACE_BUSY` 而不是挂着——因为
  `exec_command` 也会占这把锁。
  占锁的是：`apply_patch` 落盘，以及 `exec_command` **从起进程到这次调用返回**
  那一段——转后台的那一路（`yield_time_ms: 0`）占的就只有 spawn 那一瞬间，
  为的是命令别看到一个写了一半的文件树（多文件补丁不是原子的）。
  后台那段没有互斥，改用双向可见顶上：命令那边看会话快照的
  `workspace_writes_since_start`，写那边看 `apply_patch` 的 `warnings`
  （自己的命令给 `session_id`，别人的只给数量）。其余管不到的边界——外部编辑器、
  不同 `GLD_HOME` 的另一个 gld、Git common directory——写在那个模块的文档和
  [security.md](security.md) 里，别当成已经隔离了。
- **命令会话表也归同一个 `WorkspaceRuntime`，按「目录 + 调用方主体」分。**
  键是 `gld-policy` 的稳定 `CallerId`（`tools/caller.rs` 重导出），由显式 `AuthContext` 提供。
  MCP 以 grant 和客户端主体隔离，Actions 还包含 workspace_id；只有可信本机应用 / IPC 使用 LocalOperator。
  权限变化不生成新的会话表，也不会因此跳过新权限检查。两个后果：

  - 同一个主体换个 `ToolContext` 进来还是那张表，所以 hub 因为配置指纹变了
    重建上下文时**不再杀掉正在跑的命令**（以前杀，理由是"表跟着旧上下文一起
    没了"，现在理由不成立）。
  - 不同主体的表互不可见，拿别人的 `session_id` 得到 `SESSION_NOT_FOUND`。
    哪些主体分得开、哪些分不开，写在 `crates/policy/src/caller.rs`。

  所以工具内核有两个执行入口：`call_tool`（本机操作员）和 `call_tool_as`
  （带主体，网络进来的都走它）。加新的传输层时**必须用后者**，用前者等于把
  所有连接并成一个人。

## 加一个新命令要改哪里

以“给项目加一个只读的 stats 子命令”为例：

1. `core/src/app/<模块>.rs`：`impl App { pub fn workspace_stats(&self, id) -> AppResult<StatsDto> }`，
   DTO 派生 `Serialize + Deserialize`。
2. `protocol/src/daemon/mod.rs`：`Request::WorkspaceStats { target }`；共享输入 DTO 也放协议层，
   若它必须由守护进程执行，加进 `needs_daemon()`；耗时长加进 `is_slow()`。
3. `daemon/src/dispatch.rs`：新增一个 match 分支（漏了会编译失败）。
4. `cli/src/cli.rs`：加子命令与帮助文本；`cli/src/commands/workspace.rs`：调用并渲染。
   项目用位置参数或 `-w` 指定，没给就按当前目录推断（`service::resolve_current`）。
   子命令叫 `list` / `remove` 就得带上 `visible_alias`（`ls` / `rm`）——
   `crates/cli/tests/aliases_are_consistent.rs` 守着这条，忘了会红，
   同一层里两条命令抢同一个名字也会被它拦下来。
5. `scripts/gen-cli-docs.sh` 的命令表加一行，重新生成 `docs/cli.md`。
6. 测试：core 里给 `App` 方法写单元测试；命令行行为进 `crates/cli/tests/` 下按主题分的文件
   （生命周期进 `daemon_lifecycle.rs`、项目入口进 `start_and_upgrade.rs`……；要真服务就用
   `tests/common/service.rs` 的 `env.serve(…)`），
   新命令名同时会被 `docs_commands_exist.rs` 和 `messages_name_real_commands.rs` 盯上。

不要做的事：在 cli 里直接 `use gld_core::app::App` 绕过 `dispatch`——那会让直连和转发两条路径分叉。

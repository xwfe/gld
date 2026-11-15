# RFC-0014：按信任边界拆分 crate

日期：2026-11-12。依据：用户要求 GLD 像 CCNM 一样，在 crate 层明确信任边界，为后续能力保留可维护的接入面。
实施状态只记在 [task_plan.md](../../task_plan.md)，实际验证追加到[生命周期审查](../reviews/2026-09-23-lifecycle-and-docs-audit.md)。

## 1. 先确定职责，再迁移代码

先抽出已有独立职责的授权模型和通信机制，再处理协议、执行器与宿主的依赖。每一步同时迁移真实调用者和测试，保持二进制能够运行。

起点 `508571c` 只有 core、daemon、cli 三个 crate。core 同时拥有 HTTP/OAuth、节点连接、文件与命令、Agent、任务账本和持久化；
daemon 的协议又引用 core 的应用 DTO。模块名字区分了职责，但 Cargo 无法阻止这些模块互相引用。

本轮按以下证据划分，而不是照搬 CCNM 的目录：

| 实际耦合 | 修改方向 |
| --- | --- |
| `auth/context.rs`、`tools/caller.rs` 只有主体与作用域计算；`auth/grant.rs` 把无凭据范围和凭据记录放在一起 | 把纯模型与权限判断移入 policy；token、时钟和存储继续归认证适配 |
| `bridge/peer.rs::PeerError` 引用会话模块里的 `RemoteGeneration` | 把通信与已有能力协商一起移入 transport；会话租约和项目权限留应用侧 |
| 原生节点和 CCNM 都使用 `Offered` | 两个现有消费者复用同一实现，无需另造第二套协商接口 |
| `daemon/protocol.rs` 引用 app、settings、planning、runtime、tunnel 类型 | 先分开 DTO 与配置解释，再抽协议；禁止用 protocol 反向依赖 core 来完成搬迁 |
| `generation/wire.rs` 同时包含消息、鉴权转换和管道读写 | 纯消息归 protocol；身份转换与管道 I/O 留调用侧适配 |
| `workspace_runtime` 查询 Agent，而 Agent 调用 exec 环境过滤和 runs 文件管理 | 由应用层组合阻塞条件，执行器提供实际执行服务，解除双向依赖 |

这是编译与维护边界的改进。进程权限仍由已有认证、路径校验、策略及平台机制负责；
当前获准子进程仍可能访问网络，拆 crate 不能证明网络隔离或多租户隔离。

## 2. 叶子边界的职责

### gld-policy：确定性的授权模型

内容：`Grant`、`Principal`、`AuthContext`、`Caller`，以及普通文件工具的 `WriteClass / Verdict`。

- 输入是调用侧已核实的主体、授权范围、路径字符串和操作种类。
- 输出是稳定的主体键、范围匹配、委派/Prompt 可见性和文件写入分类。
- 凭据验证、grant 撤销查询、到期时间、文件系统路径解析由原适配层提供。
- 使用 `no_std + alloc`、禁止 unsafe，普通依赖仅有关闭默认特性的 serde；序列化测试用 serde_json。
- `Grant.browser` 的持久化语义保持 `serde(skip)`；跨执行代仍由现有 `WireAuth` 显式传递该标记。

`AuthContext` 是已验证结果的模型；能构造或反序列化一个 Rust 值，不等于请求已通过认证。
新网络入口仍须完成凭据检查，再经 `Caller::from_auth` 进入已有工具入口。
`Caller::local` 只用于本机操作员路径。

`tools/policy.rs` 中读取配置、检查程序和执行环境的部分暂留 core；
此步骤不把整个命令策略系统描述为已经迁出。

### gld-transport：已有的通信机制

内容：`Transport`、`Peer<T>`、`ChildTransport`、`PeerError`，
以及 `Offered / RemoteGeneration` 和工具摘要计算。

- 保留现有 trait 与串行 RPC，不新增通用客户端、驱动框架或连接池。
- 传输拥有请求 ID、deadline、协议解析、有界 stderr 和 stdio 子进程的关闭。
- core 的 CCNM 适配继续拥有成员配置、主体归属、coding 租约、空闲回收和“结果未知”的处理。
- 原生节点继续拥有配对、验签、单连接、显式共享项目和退避重连；它消费相同 Peer 与能力表。
- 普通依赖只有 serde_json；传输不读取 GLD 数据目录，也不自行选择项目或授权身份。

两种已有消费路径共享通信实现；各自会话与权限语义保持独立。

### gld-protocol：纯配置与进程间数据契约

内容：配置 DTO、daemon 请求/响应、执行代帧与操作，以及它们引用的计划状态、服务选择和身份/节点快照。
普通依赖仅 serde 与 serde_json，禁止 unsafe；不读取环境、进程、凭据存储或文件，也不反向依赖 core / policy。

- 配置默认值、字段别名与 profile 归一化随数据类型维护；实际工具集合留 core。
- `FrpProfile` 随机 ID 和 `BuildIdentity` 当前进程信息由 core 工厂提供；管道 I/O 仍归调用侧。
- `WireAuth` 由 core 显式转换，保留 browser 权限标记；类型本身不认证网络请求。
- daemon 的 `RpcError` 只定义错误码和消息，`AppError` 到它的转换放在拥有应用错误的 core。
- 原类型/常量导入路径通过 re-export 保持；副作用方法、身份与资源转换入口调整见[架构](../architecture.md#protocol数据契约与适配边界)。

## 3. 后续归属与允许的依赖

下表是迁移方向；尚未建立的 crate 以 [task_plan.md](../../task_plan.md) 的实施进度为准。

| 边界 | 职责与接入面 | 依赖方向 |
| --- | --- | --- |
| `gld-policy` | 纯主体、范围、权限判定 | serde；不依赖其他 GLD crate |
| `gld-protocol` | 共享配置、daemon/执行代消息、节点数据快照、工具参数范围、版本与纯编解码 | serde / serde_json；身份与资源转换由调用侧适配 |
| `gld-transport` | RPC、stdio、必要的流式传输机制 | 序列化及实际传输库；需要共享 DTO 时可依赖 protocol |
| `gld-runtime` | 工作区路径、文件、补丁、命令、会话、输出、写锁与运行记录 | policy、必要的 protocol、已有 toexec 原语及平台实现 |
| `gld-agent` | 已有 Agent profile/store/run 与进程生命周期 | runtime 的实际执行接口；其余沿用现有序列化、配置解析与进程依赖 |
| `gld-core` | 工具分发、任务/验收/Planning、能力暴露、应用用例与适配组合 | 调用已经抽出的底层能力 |
| 宿主边界 | HTTP/OAuth、凭据/配对、公网入口、节点连接与执行代路由 | 最后从 core 移出；由 daemon 的组合入口装配应用和监听器 |
| `gld-daemon` / CLI | 本机 IPC、进程服务生命周期、命令行与展示 | daemon 依赖 core / protocol；CLI 消费现有应用与 daemon 入口 |

宿主最后收口时再确定单独的 `gld-host` crate 是否值得保留：退出条件是执行器能独立编译、宿主能独立拥有网络连接，
且 core 不反向引用宿主实现。确实需要跨边界调用的地方，围绕已有操作传入窄接口或函数；
不预先建设万能 `Executor`、`Provider`、插件注册中心或 DI 容器。

`common` / `utils` 不能用来接收循环依赖。某个类型归属不清时，先找它的状态所有者和真实消费者。

### 宿主拆分前先解开的依赖

不要把现在的 `Hub`、`App` 或 `generation` 整包搬走。它们同时装着应用状态与宿主资源，
直接建立 `gld-host → gld-core` 会碰上 core 对这些对象的反向引用。

| 现有对象 | 留在应用侧的职责 | 待归宿主或组合入口的职责 |
| --- | --- | --- |
| `Hub` | 可见项目、工具暴露、授权/Planning、工作会话、上下文、结果包装，以及执行前的归属复核 | 原生节点的实际连接；已有 `NodeBridge::{members,call}` 可传递调用，不另造通用接口 |
| `generation` | 执行代 child、工作区状态与交接、身份转换和双方共用的路径归属语义 | Router、子进程 Link、执行代选择与替换 |
| `App` | 数据与工具上下文、业务用例 | 当前内嵌的 RuntimeSupervisor、监听器重启串行化及服务/隧道管理，需要 daemon 明确装配 |
| 认证与持久化 | policy 的已认证主体、授权范围；调用方需要的数据模型 | Bearer/OAuth HTTP 适配、客户端注册表、凭据读取、日志和目录选择；不能把 DataStore/home 伪装成纯配置 |

实际请求顺序保持：HTTP 验证身份 → Hub 计算归属 → Router 选择执行代 → 私有管道 →
child 还原身份 → Hub 再核对归属 → `call_tool_as`。两边名单不同就拒绝执行，不能因拆分省掉复核。

依次处理这些依赖，而不是先建空壳 crate：

1. Router 只接收现有 `ExecutionRoute`，不持有或解析 Hub；同目录归属键由 core 的执行代共享语义提供。
2. 分离原生节点连接的所有者与 NodeBridge 应用调用；保留 CCNM/本机 MCP 会话对旧执行代交接的阻塞。
3. 将本地工具、委派、撤销、服务启停与执行代路由的装配归到 daemon；明确认证存储、日志和目录输入。
4. 再按实际依赖迁移 Router/Link、HTTP/OAuth、网关和监听管理，决定独立 host crate。core 不通过兼容重导出反向绑定宿主。

App/ListenerManager 与执行代 Router 的显式装配见[架构](../architecture.md)。
数据与工具缓存仍由 App 管，宿主传入监听管理和各自的具体清理回调；不引入统一执行器来隐藏直连与执行代分支。

节点连接的具体装配与 Rust 接口见[架构：原生节点连接的所有者](../architecture.md)。
应用侧 NodeBridge 只保留查询和调用；显式关闭继续由实际持有连接的宿主负责，不用丢弃引用来代替。

清理 owner 时仍每轮读当前数据；读失败就保留原 owner。无 Router 的本地调用路径、配置/凭据格式、
关闭顺序与公开协议保持；这些后续职责的迁移需要各自的集成回归，不由第一步的路径测试代替。

## 4. 协议与实现的归属规则

执行代的一条完整协议切片包含 `HostFrame / GenerationFrame / Op / HostOp / WireAuth / WireNative`，
以及它们引用的 `BuildIdentity / ExecutionRoute / HubConfig / WorkSessionLimits`。

1. 配置数据与配置解释分开。`HubConfig.tool_profile` 的反序列化和纯 profile 规则属于 protocol 的配置 DTO 模块；
   实际工具集合归应用层。只改写原来已退役的别名，其余字符串保持原值，默认值与配置格式保持。
2. `BuildIdentity` 的结构和兼容比较归 protocol；`core::generation::current_build_identity()` 读取进程、构建和可执行路径。
3. `WireNative` 与运行时节点登记的转换留调用侧。避免为了满足 Rust 的跨 crate 实现规则把整个节点注册表带进协议。
4. `WireAuth` 继续显式携带浏览器标记；直接序列化 `Grant` 会丢失它，属于权限回归。
5. `FrameWriter / read_frame` 是 I/O，保留在 core 执行代适配中，不与纯消息一起放入 protocol。
6. daemon 的 `Request / Response` 及其 DTO 在 protocol 中保持独立；现有枚举和 daemon 的穷尽分发保留。
7. 服务选择返回 core 资源类型的转换由 core 实现 `From`；纯解析返回协议侧错误，再由 core 映射为原应用错误消息。

保持协议 JSON、字段默认值、错误码、配置格式与主体标识不变。只搬 Rust 类型不递增协议版本；
任何真实请求/响应形状变化按仓库约定升级版本并单独验兼容。

## 5. 执行器和 Agent 如何解耦

先从已直接接收 `&Workspace` 的文件操作进入 runtime，逐步迁入补丁、命令与会话。
应用侧保留 `call_tool_as` 的工具暴露、grant、Planning、Agent 互斥、任务基线与账本编排；
执行侧接收解析完成的工作区、策略、调用主体、受保护路径和运行记录目录。

### D1 的文件执行接口

`gld-runtime` 承载 Workspace、文件/图片/Notebook 读取、补丁解析与事务提交。
构造函数显式接收 `protected_read_roots`，core 工厂解析 GLD_HOME；原始和 canonical 路径的保护语义保持。
数值参数的默认与范围由 `protocol::tool_args` 统一提供，core 的 schema 回归继续扫描实际实现。

补丁使用 `PatchRequest` 和 `PatchOutcome` 两个具体类型。core 先解析参数再取得原有提交锁，
runtime 在该锁内完成文件操作；`wrote_since` 查询调用方已有版本链，`on_commit` 在提交成功后、采样版本前通知写计数。
core 继续在同一锁内记录新版本并生成命令提醒。避免为一次拆分新建锁注册表、复制版本账本或引入通用执行器。

路径和文件工具的旧类型路径通过重导出保持；单参数 Workspace 构造改为 core 的 `new_workspace`，
直接使用 runtime 的调用者必须显式提供保护目录。MCP 包装、权限编排与任务账本留 core。
命令、会话、输出、运行记录与共享运行状态使用下述 D2 接口，保持状态所有者唯一。

### D2 的命令执行与存储输入

`exec::Execution` 接收实际 Workspace、WorkspaceRuntime、程序解析需要的两个设置、合成 PATH 和需移除的应用环境变量名。
会话表由调用侧按已认证 Caller 取得后单独传入；整份 PolicySettings、ToolContext、Planning 和 Harness 保持在应用侧。
`CommandSpec` 统一解析，返回只描述输入问题的错误；core 映射为旧策略错误，保留原先的判断顺序和工具结果。

`RuntimePaths` 显式指定写锁目录和可选 `RunStorage { root, project_id }`；core 解析 GLD_HOME 并调用唯一 Harness 项目 ID 算法。
注册表、会话、写计数和 owner 身份归 runtime，core 兼容入口只注入参数或重导出。全局 Tokio 运行时与脱敏规则也只有一份。
按进程、程序解析、输出、控制与分页拆模块，不引入万能 Executor 或新任务框架。

与 Agent 的依赖按以下方式组合：

- `WorkspaceRuntime::handoff_blockers()` 与 Agent 在跑状态由应用层汇总；执行器只报告自己拥有的写入、命令和输出。
- Agent 的环境过滤和运行资源使用 runtime 现有函数；执行器不再查询 Agent 管理器。
- 项目 ID 由 core 组合处传入；运行记录不向 Harness 查询，也不复制 ID 算法。
- 受保护数据目录与 run-record 目录从组合处传入，保持已有数据目录和写入保护的语义。

必须继续满足：同目录共享写锁；同主体共享会话、不同主体隔离结果；后台写计数与验收快照有效性；
grant 撤销停止相关命令/Agent；换代不漏掉 Agent、运行命令或未收完的输出。

运行时默认拒绝 unsafe，仅在 `session/process_control.rs` 局部保留原有 libc / Win32 进程控制。
禁止直接依赖 axum、OAuth/签名适配、节点路由和 Agent provider；
有实际需要的进程操作与本地端口就绪检查仍属于执行侧。这条约束不宣称子进程无法联网。

### E 的 Agent 执行接口

`gld-agent` 接管 profile/store/run 和唯一 `LIVE` 注册表，复用 runtime 的私有文件、脱敏与环境过滤函数。
core 继续拥有审批、授权、项目 ID、owner 锁及任务审核，组合工作区互斥、grant 撤销和执行代阻塞；
应用的结果回包与 diff 分页留在 core，不为拆分引入第二份状态或新的 provider 接口。

`run::Launch` 接收已有任务、任务目录、档案、提示、总超时，以及显式 `remove_env`。
core 注入 GLD_HOME，正式委派子进程的环境剥离顺序与档案例外保持；版本探测和 Git 子进程不新增过滤。
旧模块导入路径继续重导出，Rust 构造方需补充新字段；任务存储、工具 JSON、配置和协议形状不变。
档案/存储/解析单测随实现迁移，审批、互斥、隔离、撤销和换代测试留在实际调用侧。

## 6. 新功能放在哪里

| 新能力 | 首先改哪里 | 接入时必须保留的边界 |
| --- | --- | --- |
| 新工具或已有工具的新行为 | core 的工具表与统一分发；需要副作用时调用对应执行服务 | 网络主体经 `call_tool_as` 传入，错误与证据按现有契约返回 |
| 新 Agent | agent crate 的现有 profile / run 接口 | 审批、允许范围、互斥和验收继续由应用侧负责 |
| 新客户端认证与凭据 | 宿主认证适配、应用 grant 用例及 policy 的纯授权结果 | OAuth / token / URL 复用授权与稳定调用方；凭据不进入 runtime，见 [RFC-0015](0015-unified-client-access.md) |
| 新传输方式 | transport 的现有实际消费者，确有协议差异再增加适配 | 不从工具 arguments 取得可信身份，不自动重放结果未知的写操作 |
| 新节点/平台能力 | 节点共享与平台适配，按真实功能加入 runtime | 每机一条 WSS、显式项目权限、原有签名和撤销语义 |
| 新诊断 | 组合已有模块的事实与错误 | 不为排障复制一份执行或授权规则 |

D18 的统一客户端授权在本 RFC 的宿主边界内实现，由 daemon 装配认证存储与应用接口。
D17-F 的搬迁仍保持原行为；动态全部项目、API token 共存及统一 CLI 是 D18 的独立功能变更，
不能把新增授权能力写成纯重构已经完成，也不为三种凭据预建三个 crate 或插件注册框架。

桌面功能仍按 M3 后的 L0/L1 队列推进；架构整理不代表产品功能提前完成。

## 7. 验证与停止条件

每片先运行它迁移的单元测试，再验证实际调用路径。涉及的已有集成测试继续留在使用方；
例如通信本身的测试随 transport 迁移，连接租约和拒发未知参数的测试留 core。

`crates/core/tests/crate_boundaries.rs` 使用离线、锁定的 Cargo metadata 检查 policy、protocol、transport、runtime 与 agent 的实际依赖，
包括别名展开、目标平台、可选和 build 依赖。它随现有全仓测试运行，不增加另一套 CI。
新增边界 crate 时同步扩展此表，才算把文字约定变成可检查约束。

收尾运行现有全仓 test / 严格 Clippy / fmt，测试使用隔离 `GLD_HOME`。
检查 Cargo.lock 只新增工作区内部包和边，不升级外部锁定版本。
当前平台通过仅代表当前快照的本机证据；三平台、公网真实连接器、跨执行代验收继续按已有矩阵记录。

无痛升级仍指连接器地址、client_id 与授权保持；沿用 [安装文档](../install.md#升级) 的部署方式。
源码重构、二进制换装、运行中守护进程和正式发布分别报告。

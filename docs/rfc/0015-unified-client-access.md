# RFC-0015：统一客户端授权与动态项目范围

日期：2026-11-15；替代 2026-11-14 版本中的旧代码兼容设计。

本文是实施契约，**不是现有功能说明**。当前排队与完成状态只维护在 [task_plan.md](../../task_plan.md)。
用户明确要求不兼容旧代码，因此 D18 直接建立一套新授权模型，不保留双轨实现。
[RFC-0014](0014-trust-boundary-crates.md) 的 D17 负责行为不变的 crate 拆分；D18 独立改变授权行为。

## 1. 设计决定

所有外部 MCP 请求都关联明确的 GrantId。OAuth、API token 和 URL 是三种接入方式，
共用项目范围、能力判断、调用方身份、轮换、撤销及工具执行路径。
创建默认使用 API token，项目范围默认动态包含 Hub 当前及以后登记的全部项目。

| 旧设计负担 | D18 的单一规则 |
| --- | --- |
| 服务在 OAuth / bearer / noauth 之间切换 | MCP 固定要求认证；OAuth 与专用 token 并存，URL 使用专用入口 |
| `grant add`、`grant browser` 分开维护 | 只保留 `grant add --via token\|oauth\|url`，删除 browser 子命令和旧 DTO |
| 缺少 grant 表示服务全权 | 网络请求必须绑定 grant；可信本机操作员单独建模 |
| 旧 grant、旧令牌、旧 caller key 自动迁移 | 单一新格式；旧授权重新创建，旧凭据与旧主体记录不转换 |
| 旧写者、旧执行代和旧节点互通 | 协议版本不匹配先拒绝，不适配字段、不降级重试 |
| 一份 grant 同时保存口令和 bearer 明文 | 一份 grant 首版只选一种接入方式；只保存验证材料和必要元数据 |
| `--reveal` 找回秘密 | 秘密只在创建或轮换成功时交付一次，丢失后轮换 |

“全部项目”是范围，写入和服务级能力另行显式授予。范围只包含已登记的 Hub 成员，
不扫描硬盘，也不打开节点未共享的目录。动态范围、期限和权限判断不依赖客户端缓存的项目名单。

删除兼容代码不要求重写已经正确的 OAuth 算法、清空有效的客户端注册表或破坏项目文件。
复用什么、拒绝什么由下文明确规定；不再承诺旧 access/refresh token 或旧授权在切换后继续有效。

## 2. 用户操作

### 2.1 创建与管理

以下命令是待实现接口：

```sh
gld grant add automation
gld grant add frontend api web --write
gld grant add chat-client --via oauth
gld grant add superassistant --via url --ttl 8h
gld grant add nightly --ttl none
gld grant add personal-client --via oauth --write --service-access

gld grant ls
gld grant rotate automation
gld grant rotate superassistant --ttl 8h
gld grant rm automation
```

统一语法为 `gld grant add NAME [PROJECT...] --via token|oauth|url`：

| 选项 | 行为 |
| --- | --- |
| 省略 `--via` | 创建专用 API token，不依赖服务认证模式 |
| 省略项目 | 动态 `AllProjects`；零项目时也可创建，以后登记的项目自动进入范围 |
| 点名项目 | `Selected`；创建时解析名称或 ID 前缀，保存稳定项目 ID |
| `--all` | 显式动态全部项目；与点名项目互斥 |
| `--write` | 请求项目写入和命令执行，仍受工具、项目、凭据及节点上限限制 |
| `--service-access` | 显式开放 §3 的现有服务级能力；只允许 AllProjects 的 token/OAuth grant |
| `--ttl` | 设置 API token 或 URL 凭据期限；OAuth 方式明确报参数不适用 |
| `--origin` | 仅 URL 方式接受，使用精确 Origin 白名单；其他方式报参数不适用 |

无旧命令别名、旧输出形状或旧参数适配。一个名字对应一个不可复用的 GrantId；
同名删除后重建获得新 ID。首版不提供一份 grant 绑定多套独立秘密、在线切换接入方式或通用角色管理。

### 2.2 三种凭据与轮换

| 方式 | 创建时交付 | 生命周期 |
| --- | --- | --- |
| token | API token 和标准 `/mcp` 地址 | 默认 30 天；显式 `--ttl none` 可不限期 |
| oauth | OAuth 授权确认口令和服务地址 | 口令仅用于授权页；access 30 天、refresh 90 天，由 OAuth 流程签发 |
| url | 完整的 `/mcp/connect/<secret>` 地址 | 默认 8 小时，最长 24 小时，不接受 `none`；保留 URL 方式的能力上限 |

API token 是独立签发的凭据，不要求用户从 OAuth 回调中复制 access token。
OAuth 确认口令使用生成的高熵随机值，只保存摘要；口令、API token、URL secret 与 access/refresh 不可跨用途使用。
三种方式使用统一的期限解析、验证材料封装和撤销入口，OAuth 的发行、刷新、PKCE 留在自己的流程模块。

`rotate` 对三种方式都保留 GrantId 和 CallerId，原子替换创建凭据并递增凭据修订号。
token/URL 的旧秘密立即拒绝；OAuth 同时使旧确认口令及旧修订号签发的 access/refresh 失效，客户端重新授权。
API token/URL 不给新 `--ttl` 时沿用原有效时长、从轮换时重新计时；不限期 token 仍不限期。
OAuth 不接受 `--ttl`。输出显示替换影响与新秘密，不暗中保留旧值的宽限期。

`rm` 撤销整份 grant，使所有关联凭据和 OAuth access/refresh 拒绝后续调用，并按 GrantId 清理命令与委派。
到期和轮换阻止使用失效凭据的新请求，不以此杀掉已接受的命令；撤销负责终止归属任务。
已经接受的原子文件操作允许完成，撤销与执行的边界要在实际调用入口验证。
`ls` 只显示 ID、名称、接入方式、动态/指定范围、能力、期限与状态，不返回秘密或摘要。

## 3. 一个授权模型，显式身份和权限

| 概念 | 职责 |
| --- | --- |
| Grant | 不可变 ID、可读名称、ProjectScope、项目权限、服务级能力和状态 |
| Credential | 公开凭据 ID、GrantId、固定用途、验证摘要、修订号、期限；URL 另有 Origin 约束 |
| CallerId | 入口作用域与稳定主体；OAuth 主体包含 client_id，不含权限、名字、项目名单或凭据修订号 |
| AuthContext | 本次验证后的 CallerId、授权范围与有效限制；不携带秘密 |
| ProjectScope | `AllProjects` 或非空 `Selected(ProjectId...)`；创建默认值不等于反序列化默认值 |

`CallerIdentity` 明确区分可信本机 `LocalOperator`、必带 GrantId 的 `McpClient` 和独立的 `ActionsClient`。
ActionsClient 由 Actions 认证适配器构造，稳定身份同时包含 workspace_id 与主体；相同主体标签
在不同工作区不能共用会话。它使用明确的 Actions 权限与写锁，不用缺失 grant 表示全权。
本机操作员只从可信 IPC/本地应用入口构造；HTTP、隧道、节点 metadata 和工具参数都不能声明这个身份。
OAuth 的同一 grant 可被不同注册客户端使用，client_id 继续区分各自会话。
稳定主体还区分 token、URL 和受信隧道绑定；隧道与直接 token 接入不能因为共用 GrantId 合并会话。
API token 或 URL 被多人共用时就是同一主体，不将其宣传为自然人隔离。

D18 之前的 Caller Eq/Hash 包含完整 Grant，运行时会话表又以 Caller 为键。
新模型只以稳定 CallerId 分表；动态项目变化、权限内容、OAuth 刷新、密钥轮换和重启均不改变身份。
旧 caller 字符串和历史归属不映射进新模型；同名重建不能读取前一 GrantId 的记录。
撤销按 GrantId 清理所有 client_id 对应的会话；查询仍按完整 CallerId 隔离。

服务主凭据当前独有的功能用明确能力承接，避免移除 `grant=None` 时无声删除现有功能：

| ServiceCapability | 必须同时满足 |
| --- | --- |
| UserPrompts | grant 显式授权，才可读取用户级 Prompt；项目 Prompt 仍按项目范围判断 |
| MachineMcp | grant 显式授权才可发现本机 MCP；实际 relay 调用另需 `--write`，不信任外部 readOnlyHint 放宽授权 |
| ManagedWorkspaces | grant 显式授权、`--write` 与服务开关同时允许；首版沿用整组 work_session 权限，不新增逐 action 角色系统 |

`--service-access` 是以上固定集合的便捷开关，默认集合为空；不是由 AllProjects 推导的管理员角色。
限 AllProjects，避免指定项目授权借托管空间创建绕过范围。托管空间仍按 CallerId 归属，
动态全部项目不包含其他主体的托管空间；本机 MCP 仍受启用列表、禁用工具和结果归属约束。
URL grant 拒绝此开关并禁止 Agent 委派。
OAuth/token 的 Agent 委派仍需要项目写权限和原有审批；`--write` 不是操作系统沙箱。
凭据限制来自已验证的类型，不能从 grant 名称、可改的 browser 布尔值或工具参数推断。

## 4. 统一入口与客户端接入

### 4.1 认证分派

宿主先确定唯一凭据来源，再验证，取得授权快照后交给同一工具分发入口。
明确格式分派后失败即拒绝，不把失败的 OAuth token 再拿去尝试其他凭据。

| 入口 | 验证与身份 |
| --- | --- |
| `/mcp` + Authorization Bearer API token | 识别专用 token 格式，验证摘要、期限、修订号与有效 grant |
| `/mcp` + Authorization Bearer OAuth access token | 验签、用途、受众、期限、明确的 grant/client 绑定与当前凭据修订号 |
| `/mcp/connect/<secret>` | 只验证 URL 用途的短期凭据，附加 Origin 和能力限制 |
| 已有受信隧道 | 验证隧道来源与密钥，并解析显式配置的 GrantId 绑定；没有绑定就拒绝 |
| 单工作区 MCP 监听 | 使用同一认证链和该入口允许的项目上限；不保留 NoGrants 或免授权分支 |

隧道是已有传输入口的适配，不另造第四套权限体系；不能因为带了专用头就成为本机操作员或全权主体。
隧道显式绑定 token 类型 grant 和当时的凭据修订号；轮换后旧绑定拒绝，需显式重新绑定，
不能自动把旧隧道证明提升到新修订。授权与隧道来源能力取交集，绑定关系纳入同一存储。
全部外部 MCP 入口，包括回环地址，都执行相同认证要求。

删除 MCP 全局 `auth_type` 二选一、服务级共享 bearer、匿名 noauth 以及隐式 master bypass。
相应配置项、CLI 选项、旧 Request/Response 和公开重导出随实现一并清理，不保留只转发参数的壳。
OAuth 发现与标准授权流程继续提供，不能因加入 API token 破坏 OAuth 客户端的协议交互。

认证材料只接受规定位置：不接受 query token，不把 URL secret 放进标准 Bearer 通道，
不把 API token 当 OAuth 确认口令。同一请求出现冲突或重复凭据来源时明确拒绝。
HTTP 状态区分未认证与权限不足；标准入口的 401 继续提供 OAuth 发现信息。
OAuth 发现、授权交互与 CORS 预检按各自协议处理，不构造工具 Caller；受保护 MCP 调用始终需要有效 grant。

GPT Actions 是独立协议，不在本 RFC 扩成第四种接入方式。
共享身份类型改造必须覆盖其调用方：沿用 Actions 的网络身份和写锁，不能因缺少 MCP grant 落到 LocalOperator；
不为保留 Actions 而恢复 MCP 的全局模式或全权后门。

### 4.2 图中只接受 URL 的客户端

SuperAssistant 选择 **Streamable HTTP**，Server URI 填创建命令返回的完整 URL。
新规范地址为 `https://<服务入口>/mcp/connect/<secret>`；不保留 `/mcp/browser/...` 别名，
也不把截图里的 `localhost:3006/sse` 当作 GLD 固定路径。
GET 推送流可返回协议允许的 405；验收依据是初始化、发现、实际工具调用和结果使用完成。
本任务不增加传统 HTTP+SSE 或 WebSocket 客户端服务。

URL 本身就是秘密；宿主、网关与错误日志脱敏，响应使用 no-store/no-referrer，不通过 Cookie 保存。
Origin 存在时按精确白名单校验并返回对应 CORS；缺少 Origin 时仍须有效秘密，Origin 不是认证凭据。
默认支持已核对的 SuperAssistant Origin，并允许显式设置；实际值和请求链在真实扩展验收时记录。
URL 是面向这类客户端的专用接入约定，不能对外声称是标准 OAuth。

协议依据：[MCP 授权](https://modelcontextprotocol.io/specification/2025-06-18/basic/authorization)、
[Streamable HTTP](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports)。
本 RFC 固定引用项目已采用的协议版本，不以客户端界面下拉框决定新增传输。

## 5. 动态项目、跨节点权限与性能

AllProjects 每次发现和调用都针对 Hub 当前成员判断，覆盖本机、CCNM 和原生节点项目。
新增登记自动进入范围，移除立即退出；Selected 保存稳定 ID，同名新项目不能继承旧项目授权。
范围列表、名称解析、错误候选、workspace_context 与硬发 tools/call 共用相同策略，
客户端与 Agent 下次 list_workspaces 即可发现新项目，不新增一套通知或自动扫描系统。

有效权限取 grant、凭据限制、入口上限、服务/项目策略与节点共享权限的交集。
AllProjects 中新出现的 `confine-reads=false` 项目仍返回 `GRANT_NEEDS_CONFINED_READS`，
不放开目录外访问，也不使其他合法项目全部不可用。Selected 创建时校验所选项目。
CCNM coding 是项目写/执行能力：项目范围、`--write`、成员 max_mode 与凭据上限取交集，
Selected 与 All 使用同一判断，不新增 RemoteCoding 服务能力或开关。
原先按“是否 grant”整体拒绝的原因是共享写连接会替换其他主体；新模型必须修复租约归属后移除该拒绝，
不能在删除 master 身份后让现有远程写功能整体消失。

复用 `crates/core/src/bridge/session.rs` 的 Lease 和不透明 handle，以稳定 CallerId 为 owner、GrantId 为撤销索引。
每个成员保留唯一写槽，其他主体占用时返回 `writer_busy`，不抢占、关闭或替换其连接。
begin/call/end 在同一槽位原子核对 owner 和成员配置代次；call/end 不得在校验后重新创建槽位，
同一主体重复 begin 复用有效租约，过时的 end 不得关闭新连接。
凭据修订号不作为连接 owner 或成员代次；轮换保留归属，撤销清理租约，成员删除或权限收紧使对应租约失效。
完成不同主体争用、OAuth client_id 隔离、并发 begin/call/end、轮换/撤销和 Selected 远程写验收后，
才允许 D18 完整切换；不建新租约服务，也不保留旧 master 旁路。

原生节点移除现有“拒绝所有 grant”判断的同时，必须完成受信身份与权限传递。
已签名请求携带原 GrantId、稳定 CallerId、本次目标项目 ID 与有效项目权限、凭据限制；
节点再与 shared/writable/runnable 相交，不发送整个 Hub 成员快照。
节点用已验签连接确认的 HubId 与原 CallerId 共同隔离会话，撤销按 HubId 和 GrantId 定位；
请求中的自报 hub_id 不能替换连接确定的发行方身份。
服务级能力留在 Hub，不向节点扩散。外部 metadata 由受信数据替换，不能合并信任；
不把转发主体伪装为本机 OAuthClient 或重建为更宽的 node-share grant。
节点会话、查询及撤销都保留原授权归属；离线或版本不支持时明确拒绝，不改成本机执行。

持久存储由单一应用写入端管理，提交成功后原子发布不可变的授权索引。
热路径按公开凭据 ID 定位验证材料，再按 GrantId 查询当前状态；不为每次请求全盘读文件或遍历全部 grant。
项目范围保存枚举，不展开复制整个成员表；CallerId 不随成员表变化。
撤销和轮换先持久提交再发布，后续请求读取当前修订号，不设置可能延迟撤销的独立认证 TTL 缓存。
首版使用现有锁和进程间调用边界，不新增缓存服务、事件总线或通用认证插件系统。

## 6. crate 职责与单一格式

| 边界 | 负责 | 不负责 |
| --- | --- | --- |
| gld-policy | Grant/范围/能力、稳定身份及纯权限交集 | HTTP、密钥、磁盘、时钟和网络 |
| gld-core | grant 用例、授权存储、成员判断、撤销编排、受信 wire 转换 | 监听 socket 和 HTTP/OAuth 路由 |
| gld-host | 三种接入适配、OAuth 发行与验证、隧道/入口约束、网关脱敏 | 复制项目权限规则或自己执行工作区工具 |
| gld-protocol | 显式版本化的 CLI/daemon、执行代与节点 DTO | 默认放行、凭据验证及副作用 |
| gld-runtime / gld-agent | 消费稳定身份与明确执行参数，隔离会话与执行 | 原始秘密、OAuth 或 HTTP 类型 |
| daemon / CLI | 装配同一授权服务，解析统一命令与显示结果 | 建第二份授权状态或兼容路由 |

现有真实路径：入口在 `crates/host/src/mcp.rs`、`auth/oauth_flow.rs`；
存储与用例在 `crates/core/src/auth`、`app/grant.rs`；
执行代转换在 `core/src/generation/wire.rs`，Router/Link 在 `host/src/generation`；
节点收发在 `host/src/nodes`，节点身份/客户端在 `core/src/nodes`。
按这些职责就地改造，不为每种凭据创建 crate，也不增加只做转发的抽象层。

新授权使用一个版本化存储位置与一种 schema，严格要求范围、用途、修订号等字段。
旧 AppData 写者不可写入该位置；不双写、不读取旧 grant 再猜测转换、不保留旧字段壳。
发现旧授权而新存储未初始化时明确要求重建授权，不自动清除旧文件、初始化全权 grant 或开放匿名监听。
本机 grant add 显式建立第一份新授权；HTTP 未就绪不应阻止可信 IPC 完成配置与授权管理。
未知版本、残缺字段、空 Selected、损坏数据均拒绝；“省略项目默认 All”仅发生在创建用例。

创建、轮换、撤销使用同一原子提交边界：保存失败保留上一份状态，不返回未落盘的新秘密。
重启从当前格式恢复修订号、期限和撤销状态，不让已撤销凭据复活。
新 OAuth claims 强制当前版本、GrantId、client_id、用途、受众和凭据修订号；
旧 access 拒绝，旧 refresh 返回 invalid_grant，不回退到无 grant 全权身份或旧受众匹配。

可直接复用未改变格式的 OAuth 客户端注册、client_id、redirect URI、签名密钥与 PKCE 算法。
保留注册记录不等于接受旧 token；客户端可在同一地址使用原注册重新授权。
项目配置和文件也不为重构故意删除；确需改变的认证配置使用明确的新格式，旧认证项给出不支持错误。

daemon Request/Response、generation 和节点权限 DTO 随真实变化提升版本；涉及持久执行状态则提升 STATE_FORMAT。
宿主、执行代、节点使用支持新安全字段的协议集合，不做旧新转换或混跑支持。
版本检查先于反序列化后的执行，缺字段不能借 Serde 默认值获得权限。
当前格式往返、版本拒绝和隔离存储是新契约的正确性要求，不建立旧版本成功运行矩阵。

## 7. 实施顺序

阶段状态只在 task_plan 维护；以下是每片要交付的契约。每片独立验证与提交，
过渡中的未完成入口明确拒绝，不用旧授权兜底让新旧模型同时对外工作。

| 阶段 | 实施内容 | 完成标准 |
| --- | --- | --- |
| A 模型与存储 | policy 的 Grant/范围/能力/CallerId；core 的单一新存储；runtime 会话键；protocol DTO 与 wire 转换；Actions 调用方适配 | 无 Option 缺失即全权；新格式严格往返和失败关闭；旧格式不导入；保存失败原子性；跨客户端会话隔离 |
| B 认证与生命周期 | host 的 OAuth/token/URL 与隧道绑定；单工作区入口统一；core 签发/轮换/撤销；清理全局认证模式与旧验证分支 | 所有外部 MCP 都有 GrantId；三种入口同服务可用；OAuth 修订号验证；到期/轮换/撤销在所有入口一致 |
| C 动态范围与远端 | core Hub 成员策略；CCNM 唯一写槽与租约；host 节点收发和 core 受信身份 | 当前及未来项目自动生效；Selected 不扩权；CCNM 写按 CallerId 隔离且不抢占；节点权限取交集；URL 限制不丢；离线不回落 |
| D 命令与文档 | 统一 grant add/ls/rotate/rm；默认 token/All；service-access 显式；清理旧命令/选项/DTO/生成文档 | 不保留 browser/reveal 别名；文本/JSON 只交付所选秘密；不适用选项报错；接入和破坏性切换说明一致 |
| E 集成与切换准备 | 全仓检查、三类客户端往返、授权生命周期、支持版本集合与隔离切换演练 | 新契约正确、旧输入严格拒绝、旧原件未破坏；真实平台/客户端未验部分据实记录 |

A 涉及 `crates/policy/src/{grant,context,caller}.rs`、`crates/runtime/src/workspace_runtime.rs`、
`crates/core/src/auth`、`app/grant.rs`、`data`、`generation/wire.rs` 和 protocol 的 config/daemon/generation 定义；
B 的主要适配点在 `host/src/{mcp,auth,hub,listeners}`，同时删除 core 配置中的 MCP 模式分支；
C 覆盖 `core/src/hub`、`core/src/bridge/session.rs`、`core/src/nodes`、`host/src/nodes`；D 覆盖 CLI、daemon 分发及用户文档。
路径表示职责范围，不要求为了文件名相似机械移动代码。

复用并改写真实认证/授权集成测试，不为保留旧断言继续留下旧实现。
每片先做定向测试；收尾在隔离 GLD_HOME 下运行全仓 tests、严格 Clippy 和 rustfmt。
CLI 帮助改变后运行 `scripts/gen-cli-docs.sh`；行为变化同步 concepts、connect-clients、security 和必要发布说明。

### 7.1 过渡阶段的回归责任

A 可以关闭尚未接通新模型的网络入口；拒绝测试不代表该功能已完成。旧 master/noauth、旧 URL
和旧节点 metadata 的成功用例随退休实现一起退出当前测试面，Git 历史保留原证据。
执行代切换、命令和文件、CLI 与 Actions 的独立正向回归继续运行，不得通过伪造网络 LocalOperator 保绿。

| 后续门禁 | 必须用真实新入口恢复的正向场景 |
| --- | --- |
| B 三类凭据 | 签发、用途隔离、OAuth PKCE/access/refresh/注册表重启、token/URL 期限、Origin、轮换与撤销 |
| B/C 授权委派 | 同 grant 不同 client_id 隔离、只读与 URL 禁止委派、撤销停止等待及运行任务 |
| B/C 托管工作区 | 零项目创建幂等、命令阻止重启、保留/到期清扫/关闭重试跨重启、配额与归档幂等、启动自动清扫、PNG 的 MCP image 回填 |
| C 原生节点 | 新 HubId+CallerId 归属、三级权限交集、原子写入/版本前提、命令运行/输出/停止、断线不重放、撤销清理、单连接跨代与重启、代理/配对/分享/自启动 |
| C CCNM | 稳定 owner 唯一写槽、并发 begin/call/end、同主体复用、他人 busy、轮换保归属与撤销回收 |

`AUTH_NOT_READY`、发现文档或 CORS 成功都不能作为上述正向门禁的替代。A 的元数据管理可用，
旧创建/reveal 仅给出停用错误，旧参数定义在 B/D 随新配置和统一命令一并删除，不构成兼容实现。

## 8. 验收与切换边界

| 维度 | 必须证明 |
| --- | --- |
| 默认与范围 | 默认动态 All；零项目后加入可见；移除生效；Selected 不随同名重建扩大；不扫描磁盘 |
| 显式权限 | 默认只读、服务能力为空；write/service-access 各有边界；URL 不取得委派或服务能力 |
| 入口闭合 | OAuth/token/URL、隧道和单工作区入口均绑定有效 grant；本机操作员不可由网络伪造 |
| 身份与归属 | OAuth client_id 隔离；刷新/轮换/重启/成员变化不分裂 CallerId；同名重建不继承记录 |
| 跨边界 | wire/节点完整保留授权、入口上限与凭据限制；节点权限再取交集；服务能力不向节点传播 |
| CCNM 写槽 | Selected/All 均按项目写权限；不同 CallerId 争用明确拒绝；begin/call/end 原子归属；轮换不换 owner，撤销和成员收紧清理 |
| 凭据生命周期 | 用途不串用；过期/轮换/撤销拒绝后续请求；OAuth refresh 同样检查；失败提交不发布新状态 |
| 格式与版本 | 只读写当前 schema；旧格式/命令/token/协议明确拒绝；不转换旧 caller、不静默扩大权限 |
| 实际接入 | OAuth、Header token、URL 客户端完成初始化、发现、调用、结果回填及后续调用；Connected 不是完成 |
| 切换 | 隔离演练证明备份原件不被重写、支持的组件版本匹配、新授权生效、客户端能重新授权 |

D18 是明确的破坏性授权升级：旧 grant、旧静态秘密、旧 URL 与旧 access/refresh 需要重建或重新授权，
不以“连接器完全无感”作为验收条件。原服务地址和未变格式的 OAuth 注册可复用；旧 URL 路径要换成新生成的地址。
旧 caller 记录不成为新授权的会话，切换前须收尾在途任务，不承诺恢复旧进程或转换历史主体。

生产切换时备份当前数据、停止旧写者、安装匹配版本集合、显式初始化新授权并更新客户端。
回退应恢复匹配的程序和授权状态，单独核对备份中的凭据是否已经撤销；禁止直接加载旧备份而复活失效凭据。
这些是部署步骤，本 RFC 的规划或源码提交不执行真实授权重建、部署、重启、推送或发版。

历史方案和当时证据保留在 [RFC-0007](0007-scoped-grants.md) 与审查中；
未来实现以本 RFC 为准，现有版本用法在实现完成前仍按当前用户文档执行。

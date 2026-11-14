# RFC-0015：统一客户端授权与动态项目范围

日期：2026-11-14。

本文记录用户已确认的目标和实施契约，**不是现有功能说明**。下列新命令、默认值、模型和兼容机制均待实现。
当前排队、认领与完成状态只维护在 [task_plan.md](../../task_plan.md)。
实施紧接 [RFC-0014](0014-trust-boundary-crates.md) 的 D17-F 源码边界收口，不等待部署或 M2 长线真机验收。
D17 的纯归属重构保持原行为；本 RFC 的行为变化独立实现、验证和提交。

## 1. 要解决的问题

现有标准 `/mcp` 的 OAuth 和静态 Bearer 由服务认证模式二选一；OAuth 服务不能直接使用普通 grant 的静态令牌。
只接受 URL 的浏览器客户端另有 `gld grant browser` 和短期地址，两条 HTTP 路径已共享工具分发，
但创建方式、凭据生命周期、范围表达和用户操作仍需统一。
当前 grant 只保存项目 ID 快照；`grant browser --all` 也是创建时展开，后加项目不会自动进入范围。

目标是让用户创建一份授权，按客户端能力选择 OAuth、专用 API token 或 URL 凭据；
默认覆盖 Hub 当前及以后登记的全部项目，限制项目时再点名。
三个接入方式共用授权、轮换、撤销、项目判断和审计，不复制权限或执行链路。

“全部项目”只决定项目范围，不代表管理员、任意目录访问、自动扫描硬盘或绕过节点上的共享配置。
写文件和运行命令继续显式选择 `--write`；浏览器凭据仍有自己的能力上限。
本任务不开放 CCNM grant 写会话，不新增多用户目录、托管密钥平台或通用认证插件框架。

## 2. 用户操作契约

### 2.1 统一创建入口

以下为待实现命令；不能按当前版本帮助直接使用：

```sh
gld grant add automation
gld grant add automation api web --via token --ttl 30d
gld grant add chat-client --via oauth
gld grant add superassistant --via url --ttl 8h
gld grant add nightly --via token --all --ttl none
```

统一语法为 `gld grant add NAME [PROJECT...] --via token|oauth|url`：

| 参数 | 新行为 |
| --- | --- |
| 不写 `--via` | 使用专用 API token；不再跟随服务当前认证模式选择输出 |
| 不写项目 | 明确创建动态 `AllProjects`，包括当前及以后登记到 Hub 的项目 |
| 点名项目 | 创建 `Selected`，解析并保存稳定项目 ID；名称或 ID 前缀只用于创建时选择 |
| `--all` | 显式选择动态全部项目；与点名项目互斥 |
| `--write` | 请求写文件和运行命令；实际仍与项目、工具和节点策略取交集 |
| `--ttl` | 只控制 token 或 URL 凭据期限；不冒充 OAuth access/refresh 的发行配置 |

`grant browser NAME [PROJECT...] ...` 保留为 `grant add ... --via url` 的薄兼容入口，
不再有独立授权实现。它新建时省略项目或给 `--all` 都遵循动态全部项目的新语义。
**已保存的旧 grant 一律保持原范围**，包括旧 `--all` 生成的快照，不能在升级时扩大。

当前 `grant add` 按服务模式展示口令或令牌；改成默认 token 是有意的 CLI 行为变化。
依赖旧输出的脚本需显式改用 `--via oauth` 或 `--via token`，JSON 调用方也应明确指定；
升级说明和 CLI 帮助必须写清，不能靠“兼容旧命令”掩盖输出变化。
一次创建只显示所选方式需要的凭据，不同时发三把秘密。首版每份 grant 一种创建方式即可。

### 2.2 管理与期限

```sh
gld grant ls
gld grant rotate automation
gld grant rotate superassistant --ttl 8h
gld grant rm automation
```

| 方式 | 期限与维护 |
| --- | --- |
| API token | 默认 30 天；可显式 `--ttl none` 供无人值守使用，展示为不限期；期限解析使用有界、无溢出的统一实现 |
| URL 凭据 | 默认 8 小时，最长 24 小时；不接受 `none`；保留明确 Origin 与浏览器能力限制 |
| OAuth | 沿用当前 access 30 天、refresh 90 天；`--ttl` 对此方式报参数不适用，不能静默忽略 |

`rotate` 首版支持 token 和 URL：保留 grant 与调用主体，原子替换凭据，旧值从后续请求起失效。
新值只在成功回包中显示一次；同名 grant、项目范围和已存在命令会话不因轮换改变归属。
`rm` 撤销整份 grant，使关联凭据和 OAuth access/refresh 都失效，并沿用按授权归属停止命令与委派的能力。
删除后同名重建必须获得新 ID，不能让旧令牌或旧会话归属复活。
到期拒绝后续请求；已经接受的命令仍按既有超时执行。主动撤销与凭据自然到期不是同一动作。

`ls` 展示方式、动态全部/指定项目、读写级别、到期和状态，不显示新凭据。
新凭据不可恢复明文；旧 `--reveal` 若保留，只对原本可重显的旧记录生效，须明确区分。

## 3. 一个授权模型，分开权限与钥匙

| 对象 | 职责 | 不应承载 |
| --- | --- | --- |
| Grant | 稳定授权 ID、名称、项目范围、操作权限及撤销状态 | 原始 token、URL、浏览器请求头 |
| Credential | 所属 grant、用途、秘密摘要、期限、必要的 Origin 等约束 | 项目执行器、另一份项目权限真相 |
| CallerId | 稳定调用主体与入口归属，用于命令、输出、运行记录和委派隔离 | 可轮换秘密、当前项目列表、可变名称或权限 |
| AuthContext | 本次认证后的主体、grant 与有效限制 | 未验证的客户端声明或原始秘密 |

项目范围使用显式枚举，例如 `AllProjects` 和 `Selected(Vec<ProjectId>)`。
新建默认值在创建入口选择；旧记录的反序列化不使用这个默认值。
旧 `workspaces` 转成 `Selected`；空数组是零权限，缺字段、未知标签或冲突形状按迁移规则拒绝，
绝不能把空数组、解析失败或缺省字段当成全部项目。

`AllProjects` 仍是一份 grant，不能用现有 `AuthContext.grant = None` 代替。
现在 None 还代表服务主凭据，可影响全局 Prompt、本机 MCP、工作空间和远端权限；这些能力不能由项目范围附赠。
服务主凭据的既有权限与身份保留兼容路径，不通过迁移普通 grant 悄悄合并。

当前 `Caller` 的 Eq/Hash 包含完整 Grant，运行时会话表又以 Caller 为键。
实施需把稳定身份与逐请求权限分开：名称、权限调整和动态项目变化不应创建另一张会话表。
不得每次把 AllProjects 展开为当前成员列表再放进 Caller；新增项目不能让已有会话失联。
保持旧 caller key、OAuth client_id 和入口作用域的既有隔离；原始秘密与秘密摘要不进入这些键。

浏览器限制取自**本次已验证的 CredentialKind**，再与 grant 权限相交。
不能因为统一成 Grant 就丢掉“不委派 Agent”等限制，也不能从工具参数或未验证字段相信这个类型。
首版无需多凭据管理界面；把 Credential 与 Grant 分开，是为轮换和权限判断各有唯一归属。

## 4. HTTP 与认证适配

宿主提供一个认证入口函数，明确分派 OAuth access token、专用 API token、URL 凭据，
最终返回同一种 AuthContext，再进入已有 MCP 分发、Hub、策略和执行链路。
各适配器负责解析和验证自己的凭据；授权服务统一检查记录、期限、撤销状态及请求约束。

标准 `Authorization: Bearer ...` 同时接纳本服务签发的 OAuth access token 和本服务专用 API token。
专用 token 采用可明确识别的格式；无效、未知或用途不符时拒绝，不在验证失败后按另一类凭据试到放行为止。
这里不是接收第三方模型服务商的 API key。OAuth 客户端保持原地址、发现、PKCE、client_id 与刷新流程。
在 OAuth 服务上添加专用 token 无需改变全局认证方式，也不影响已有连接器。

URL-only 客户端继续用专用路径携带短期能力凭据，选择 Streamable HTTP。
`/mcp/browser/<secret>` 可作为兼容路由保留，但只能是同一认证与授权服务的薄适配层；
不要把 OAuth access token 或 API token 拼进 query，也不让 URL secret 在标准 Authorization 中通用。
多个认证来源同时出现须明确拒绝歧义；认证来源由服务器路由和已验证凭据决定，不由请求自报。

保留 URL 路由的 Origin 检查、MCP 预检头、无 cookie 认证、no-store/no-referrer 和日志脱敏。
缺省 Origin 的扩展后台请求仍凭秘密认证；Origin 不是客户端身份证明，不能代替秘密。
HTTP GET 推送流是可选能力，合法 405 不能一概当作连接失败；不为适配下拉框新增传统 SSE 或 WebSocket 服务。

既有全服务静态 bearer 和 noauth 是兼容配置，不是本 RFC 新增的三种安全接入方式。
不能把 noauth 宣传成已受 grant 保护；不能为了加入 token 让现有 OAuth 服务降级为静态全权 bearer。
协议背景见 [MCP 授权](https://modelcontextprotocol.io/specification/2025-06-18/basic/authorization)
与 [传输](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports)；URL-only 兼容是 GLD 的明确边界。

## 5. 动态项目与远端权限

AllProjects 在每次发现和调用时针对 Hub 当前成员判断，覆盖本机、CCNM 与原生节点项目。
零项目时可以创建 AllProjects；后来登记的成员下次 `list_workspaces` 或调用即进入范围，不需重建授权。
移除项目立即退出范围；Selected 使用稳定 ID，同名新项目不能继承旧 ID 的授权。
只包含显式登记到 Hub 的项目，不扫描磁盘，也不自动增加节点本机未共享的目录。

实际操作取 grant 权限、凭据上限、服务/项目工具策略及节点共享权限的交集。
点名 Selected 项目时保留现有有效性校验；AllProjects 中新出现的 `confine-reads=false` 项目，
调用时仍明确报 `GRANT_NEEDS_CONFINED_READS`，不能变成目录外读取，也不能让其他合法项目全部不可用。
目录列举、名称解析、候选错误、workspace_context 和硬发 tools/call 必须使用一致的范围判断。

原生节点现在在发现、节点调用和受信身份转换多处拒绝 grant，不能只删这些拒绝。
现有 ForwardedCaller 只带 hub_id/subject；节点重建权限会取本机 writable，且不保留上游 browser 和原 grant 归属。
需版本化受信 DTO，在已签名节点请求中携带稳定授权归属和有效权限；外部 metadata 必须被替换，不能合并信任。
节点再与 shared/writable/runnable 及项目策略相交，并保留按原 grant 精确隔离、查询和撤销的能力。
新 Hub 遇到不理解这些约束的旧节点须明确拒绝，不能去掉限制字段重试或落回本机执行。

CCNM 项目仍可进入 AllProjects/Selected，但已有 grant 不可开启远端写会话的约束保持；
项目范围扩大不自动扩大远端操作能力。节点离线、项目撤销和不支持的操作应报告具体原因。
成员变化不为本任务新增整套 tools/list_changed 通知协议；客户端与 Agent 按当前 `list_workspaces` 发现项目，
不能把初始化时的项目名单当成永久授权快照。

## 6. 凭据存储、迁移与版本边界

新凭据使用高熵随机值，仅持久化验证所需摘要和元数据，明文只在创建/轮换成功时返回。
认证失败关闭；存储损坏、未知版本、未知用途不得当成空配置、服务全权或另一种凭据。
轮换须在同一持久化事务中替换验证材料，保存失败不丢旧凭据，也不把未落盘的新值当成成功交付。

旧 OAuth 注册、签名秘密、grant ID、权限和 caller 身份保持；有效旧凭据不因代码重组被重新签发。
旧数据升级有显式转换与备份，旧范围始终 Selected；不在后台按新默认重建 grant。
历史 URL 凭据不能借迁移进入永久 token/OAuth 通道；已有全服务凭据也不能转换成普通 grant 后改变旧身份。

宿主与执行代的 WireAuth/WireGrant 必须完整携带范围、稳定身份和凭据带来的限制。
缺失或未知安全字段不得回退为可写/非浏览器；新宿主与不理解该形状的旧执行代明确拒绝。
根据实际形状修改 daemon 协议、generation 协议及最低版本；盘上不兼容变化同步提升 STATE_FORMAT；
节点受信 DTO 与握手能力按兼容边界升级，不把 Serde 能读出字段等同于理解权限。

**旧写者风险必须在 A 阶段解决。**当前 AppData 可忽略未知字段，保存又会整份重写，
旧二进制可能把新权限/凭据字段丢掉；只增加字段或 generation 版本握手不足以保护旧 CLI 和独立旧进程。
采用对旧写者安全的版本化存储与迁移策略，在实现前用旧二进制读取、写入和回滚路径验证；
本 RFC 不预定新文件名、不要求双写，也不声称 Serde 默认值能解决问题。
硬条件是旧写者不能覆盖或降级新授权，新版不能把残缺新记录识别为旧全权记录。
若做不到，停止启用新记录写入，保留旧格式与旧授权工作，先完成兼容机制；不能以文档提醒代替保护。
回滚须说明哪些版本可直接读取、何时需恢复迁移前备份，以及如何避免已撤销凭据复活；不能承诺任意降级无损。

## 7. 实施分片

下列顺序表达依赖，不另维护进度状态。路径为本次核对位置；D17-F 迁移后按职责定位，不硬绑定旧路径。
继续复用 gld-policy、gld-protocol 和宿主/应用边界，不为三种方式各建 crate、权限库或执行器。

### A. 授权模型、稳定身份与兼容地基

涉及 `crates/policy/src/{grant,context,caller}.rs`、`crates/runtime/src/workspace_runtime.rs`，
`crates/core/src/{auth/grant,app/grant,data/model,data/migrate,data/store,generation/wire}.rs`，
`crates/protocol/src/{daemon/dto,daemon/mod,generation}.rs` 及相应协议 fixture。
建立显式范围与 Credential/CallerId，保留旧授权语义；完成旧数据转换、失败关闭、旧写者和版本拒绝验证后才允许新记录写入。
复用 policy、generation、配置迁移测试；证明权限字段跨 JSON/wire 往返不丢，旧 caller key 和记录归属不变。

### B. 专用 token 与 OAuth 并存、统一生命周期

涉及认证适配、`mcp/listener.rs`、`auth/oauth_flow.rs`、`app/grant.rs` 与撤销执行入口。
实现统一认证和签发/轮换/撤销；OAuth 模式下专用 token 可用且旧 OAuth 无需重授权。
验证期限、持久化失败原子性、跨用途拒绝、轮换不中断身份、撤销 access/refresh 与命令/委派。
复用 `scoped_grants`、`oauth_connector_flow`、`browser_grants`，新增真正覆盖并存与轮换的契约用例。

### C. 动态范围与原生节点受限转发

涉及 `hub/mod.rs`、`hub/native.rs`、`nodes/{identity,registry,client,config}.rs` 和对应 protocol/bridge 边界。
统一成员选择，补齐节点的上游权限交集、稳定授权归属与撤销链路；CCNM 写拒绝保持。
覆盖零项目后加入、Selected 不扩权、confine-reads 变化、节点三级权限、旧节点拒绝及离线不回落。
复用 native 文件/命令/身份测试，以真实 HTTP 与节点消息验证，不能仅依赖范围枚举单测。

### D. 统一 CLI 默认、兼容命令与用户文档

涉及 `crates/cli/src/cli.rs`、`commands/grant.rs`、daemon 请求处理和 CLI 测试。
接通 canonical 命令、token 默认、动态 All 默认、旧 browser 薄适配、rotate 与期限报错。
更新 connect-clients、security、concepts、CLI 生成文档及必要发布说明；示例使用占位值，不生成真实凭据。
验证旧命令新建语义与已存旧记录语义的区别，JSON/文本输出、空参数和不适用参数都有明确结果。

### E. 整体验证与升级验收

在隔离 GLD_HOME 完成 `cargo test --workspace --all-targets --locked`、
`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo fmt --all -- --check`；
CLI 帮助变化后运行 `scripts/gen-cli-docs.sh`，检查文档与行为一致。
真实客户端验证 OAuth 旧连接、带 Header 的 token 客户端及 URL-only SuperAssistant，
包括初始化、发现、实际工具调用、结果回填、后续调用、轮换/到期/撤销；Connected 不是完整验收。
真实升级须核对部署构建、旧 client_id/原 token/refresh、节点与执行代版本、回滚证据。
本机创建凭据、部署、重启、推送和发版仍按各自已有授权执行；未做真实验收时据实保留缺口。

## 8. 交付验收矩阵

| 维度 | 必须证明 |
| --- | --- |
| 默认范围 | 新授权动态 All；零项目创建可用；后来加入自动可见；移除生效；Selected 与旧快照不扩大 |
| 权限 | 默认只读；显式 --write 仍受工具、项目和节点限制；All 不变服务主凭据 |
| 三种接入 | 同服务并存、共享授权判断；凭据不串用；错误或重复认证来源不降级 |
| 身份 | 刷新/轮换/重启/项目变化不分裂 Caller；不同授权隔离；同名重建不复活 |
| 浏览器 | Origin/期限/脱敏与不委派限制跨宿主、执行代和节点完整保留 |
| 节点与远端 | 本机/CCNM/Native 范围一致；节点上限取交集；CCNM 写拒绝保持；离线不回落 |
| 生命周期 | 保存失败保留旧值；撤销阻止后续调用并清理归属任务；已接受调用的竞态有明确处理 |
| 旧版兼容 | 旧数据不扩权；旧写者不能丢字段；旧执行代/节点不丢权限；回滚不丢身份或复活已撤销凭据 |
| 真实接入 | 三类客户端完成工具往返与结果使用；未验证的浏览器、平台和部署链路单独列出 |

涉及的历史决定见 [RFC-0007](0007-scoped-grants.md) 与生命周期审查 §23–§25、§67；
本 RFC 只替换明确列出的创建默认、项目范围和接入维护方式，其他权限边界沿用并逐项验证。

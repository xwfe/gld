# RFC-0013：一个入口管理多台机器，按需提供基础桌面操作

日期：2026-10-28。本文是经用户要求收敛后的实施设计，**不是已实现功能说明**。
当前认领与完成状态只在 [task_plan.md](../../task_plan.md)；当前可用命令以
[concepts.md](../concepts.md) 和 [cli.md](../cli.md) 为准。下面的 join、machine 和 desktop 接口均为拟议接口。

**实施顺序调整（2026-10-28）：先连通多台机器并操作各机项目，桌面最后做。**
节点只用一条普通 WSS 连接承载标准 JSON-RPC 2.0；复用 `bridge/peer.rs` 和已有
`handle_request`，不实现 RFC 8441、每项目/每主体连接池或自定义消息恢复协议。

**审查补充（2026-10-30，基线 `b066d4f`）：**WSS 单连接的方向不变。新增约束在 §3 入口核对、
§7.2 超时与超大回包、§7.3 Hub 身份、§7.4 代理与证书、§8 截图大小；§7.3 已定为两档都做并实现。

## 1. 决定与用户体验

用户只维护一个 gld 连接；目标机器安装 gld 后主动连回这个 Hub。每台目标机不用再配域名、
入站端口、SSH、ccnm 或隧道。Hub 也可以执行本机任务，使用同一套目标选择规则。
桌面只补打开、截图、点击、滚动、文字、快捷键和显式剪贴板；不扩成完整远程桌面产品。

这次明确替代 [RFC-0011](0011-computer-use.md) 的默认 Cua-first 路线：

- 桌面直接依赖小型 Rust 库，随 gld 发行；Cua、Peekaboo、Mouseless 等仅为用户自选扩展。
- ccnm 的已有远端项目继续工作；它是可选兼容接法，不是新用户的安装前提。
- 保留 W0/W1 的任务空间、产物和清理，复用文件、命令、鉴权及运行记录内核。
- 不做录像、音视频传输、P2P 穿透、云账号平台、第二套任务系统或自动模型循环。

目标流程只有三步：

1. 操作员在 Hub 配一次稳定入口，并把该入口连接到 AI 客户端。
2. Hub 生成一次性配对邀请和对应平台的安装接入命令；目标机执行后自动登记、检测环境、启动连接。
   已安装 gld 的机器只需执行拟议的 `gld join --hub <HTTPS基地址>`；桌面模式加 `--desktop`。
3. 用户说“检查测试服务器上的 api 项目”“修改办公电脑上的 web 项目”，模型查询可见机器和项目后选择内部 ID 执行。
   用户不需要理解 workspace ID、网络端口和 provider。

第一轮交付到列项目、读取/修改文件、运行命令、查询输出和停止命令为止；目标机只共享操作员显式选中的项目。
远端通用任务空间、浏览器和桌面不作为这一轮的前置条件，也不以开放整个主目录来省掉项目登记。

没有 gld 时，生成的安装命令负责识别 OS/CPU、下载已发布且固定版本的包、验证、安装到用户目录、再 join。
不在本 RFC 编造一个已可运行的安装 URL；脚本和正式发布件验收后才放进安装文档。
配对码优先通过终端隐藏输入或 stdin 接收；复制安装命令与配对确认不要求编辑 JSON。
“一条命令”不包含绕过系统录屏/辅助功能确认，也不代表自动取得管理员权限。

## 2. 当前代码能复用到什么程度

本轮基线 `17ea95b`，工作树初始干净；以下是读源码得到的现状，不代表新设计已部署。

| 当前能力 | 证据入口 | 本次处理 |
| --- | --- | --- |
| 一个 MCP 地址、多 workspace、每次显式路由 | `crates/core/src/hub/mod.rs`、RFC-0004 | 保留；增加机器维度，不保存全局“当前机器” |
| 本地与远端成员分型 | `hub/mod.rs::Member` | 当前远端只有 `Remote(CcnmMember)`；增加原生 gld 远端成员，不伪造本机根目录 |
| ccnm 远端项目、独立 remote_* 工具 | `bridge/member.rs`、`bridge/session.rs`、`bridge/tools.rs` | 保留旧协议；不把 ccnm 字段直接改成通用机器字段 |
| 现有 JSON-RPC 客户端与分发 | `bridge/peer.rs::Peer<T: Transport>`、`hub/mod.rs::handle_request`、`mcp/server.rs::handle_request` | 只增加 WebSocket Transport 和已认证节点的接线；握手、请求 ID、超时、工具执行不重写 |
| 同一工具内核及调用方隔离 | `tools/dispatch.rs`、`tools/caller.rs`、`auth/context.rs` | 目标机直接执行；转发携带经 Hub 核验的主体，不将所有调用退化成 Caller::local |
| 非项目目录与保留产物 | `managed_workspaces/`、`hub/work_sessions.rs` | 在目标机使用其实际 GLD_HOME；Hub 只保存路由映射 |
| Hub 入口的 Cloudflare/FRP/自建反代 | `tunnel/`、`app/hub.rs`、`mcp/listener.rs` | 一次配置覆盖 MCP 与节点连接；目标机不安装每机隧道 |
| 按平台下载的原生发行包 | `scripts/package.sh`、`docs/install.md` | 复用发布校验和与来源证明；补安装接入、用户级自启动和卸载 |
| Cua 专用 protocol probe | `computer/probe.rs` | 已有源码保留，不再扩建，也不作为基础桌面或多机前置 |

当前**没有**目标机主动回连、设备配对登记和原生多机桌面路由。现有 bridge 处理的是远端项目，
不能靠新增一条配置就变成机器接入层；新代码集中在接入和路由，不重写工具执行内核。

## 3. 连接拓扑：只暴露 Hub

```text
AI 客户端 ── MCP / OAuth ──> 同一个 gld Hub
                                  ↑          ↑          ↑
                          出站 WSS       出站 WSS      出站 WSS
                            Mac gld    Windows gld    Linux gld
                            桌面/文件    桌面/文件     文件/可选桌面
```

“一个连接”指 AI 客户端只登记一次服务；**每台目标机到同一个 Hub 只维持一条出站 WSS 连接**，
该机全部项目和获准调用方复用它，不按项目、主体或控制/数据用途再开连接。
使用 HTTP/1.1 Upgrade 的 WebSocket over TLS（RFC 6455），其 Text message 中直接放标准
JSON-RPC 2.0；不使用 HTTP/2 Extended CONNECT（RFC 8441），不另外包一层消息信封。[S14][S15]
Hub 复用已有 Axum，节点使用 tokio-tungstenite/rustls；WebSocket/TLS 交给库，
JSON-RPC 的 initialize、tools/list、tools/call、ping 与错误结构复用现有实现。[S1][S2]
这是在 WSS 上使用既有 JSON-RPC/MCP 方法，不宣称 WSS 是 MCP 已标准化的独立 transport。

| Hub 所在位置 | 一次性准备 | 目标机要求 |
| --- | --- | --- |
| 公网服务器 | 固定域名、TLS、转发到 gld | 能出站访问该 HTTPS/WSS 地址 |
| 家庭/办公内网 | 只在 Hub 使用一个固定 Cloudflare Tunnel、FRP 或已有反代 | 同上；不需路由器端口映射 |
| 同一可信 LAN/已有 VPN | 已可达的 Hub 地址和信任配置 | 使用该网络；不强制安装 VPN |

推荐同一个 HTTPS 基地址下分 `/mcp` 与 `/nodes/v1/*`；旧 `/hub` 基路径等前缀要保留，
不能随便从 MCP URL 裁掉路径推导节点入口。两类接口同端口但鉴权完全分开。
反代只开放了 `/mcp` 时，安装助手应报告一次性扩展节点路由的要求，不让每台机器单独改隧道。
Cloudflare 支持 WebSocket，但会因空闲或服务更新断开。[S3] 使用标准 Ping/Pong 和普通退避重连；
连接恢复后重新鉴权、初始化并列项目，不设计 resume/ack/序列号补发等恢复协议，不自动重放旧调用。
TLS 在托管反代终止时，反代运营方处于信任边界内；本方案不声称内容对中继不可见的端到端加密。

**可达性有前提。** 至少一个稳定 Hub/转发入口必须能被节点访问；所有机器都在不互通的私网且
没有共同入口时，不能仅靠一条命令保证互联。Hub 休眠或关机时远端访问也不可用，适合放在常开设备上。
流量首版经过 Hub，有带宽和单点代价；不承诺免费云服务、不自动建立公共中继。

已有 OpenAI Secure MCP Tunnel 是 OpenAI 产品到私有 MCP 的调用路径，不是任意机器可连接的
通用 WSS 入口。[S4] 可以保留这条 AI 连接，但还需一个**统一的节点可达地址**；不能把 tunnel_id 发给节点就说完成了多机网络。
跨网新用户优先选择同时承载 MCP/节点的固定 HTTPS 入口；不自动改掉既有用户连接。

**发邀请前先核对 Hub 入口。** 现在节点地址由目标机操作员在 `gld join --hub` 手填，
`gld node invite` 既不给出地址，也不看 Hub 走的是哪种入口。`gld share` 默认的 Cloudflare 临时隧道
（`*.trycloudflare.com`）每次重启 Hub 都换地址，节点配置里存的仍是旧地址、全部掉线；`--tunnel openai`
则没有任何节点能连的地址。邀请输出应直接给出 Hub 当前固定入口的基地址；入口是临时隧道或只有
OpenAI 隧道时拒绝生成邀请，并说明改用固定域名、FRP 或已有反代。

## 4. 复用社区组件，但不引入另一套产品

| 能力/方案 | 取舍与依据 |
| --- | --- |
| Desktop Commander Remote Device | 官方已有“设备主动 WSS 连接远端 MCP，再调用本机工具”的模式。借鉴接入体验与拓扑；其文档要求 Node 和本机 MCP，gld 已有工具内核，不把整套作为依赖。[S5] |
| Enigo | 默认路径覆盖 macOS、Windows、Linux X11 的键鼠/文字；Wayland/libei 仍标实验性，不能默认算三端等价。[S6] |
| XCap | 截图/窗口采集直接库调用；Wayland 明示特殊场景限制，Linux 编译和运行依赖要在发行件上核对。[S7] |
| Arboard、open | 前者按需补剪贴板，后者打开路径/URL；不把 open 当成完整应用发现/窗口聚焦库。[S8][S9] |
| Iroh | 可直接依赖的 Rust 连通库，提供 QUIC、打洞与中继。但已有统一 Hub 时首版不需要另加发现/中继拓扑；以后有实测带宽或直连需求再评估，不自造 P2P。[S10] |
| RustDesk / MeshCentral 类完整产品 | 适合独立远程桌面/设备运维；本次不整套嵌入，不复制流媒体、资产平台或复杂控制台 |

依赖版本在编码时固定，并核对现有 Rust 1.89 下限、image/windows/rustls 版本、许可证与三端构建。
不因研究页面显示 latest 就升级现有锁文件；不预装 Python、Node、浏览器或视觉模型。

## 5. 两种运行角色，仍然是同一个 gld 产品

**Hub 模式**继续提供 MCP/OAuth、成员路由和现有本机工具。**节点模式**只负责出站连接与执行；
默认不启动公网 MCP、不生成客户端 OAuth、不注册本机隧道，不把当前目录自动登记成共享项目。
避免把“装节点”实现为又启动一个完整 Hub 再套娃转发。

节点优先复用 `handle_request` 及其下的 `build_tool_context`、`call_tool_as` 和 runs；
项目路由沿用 Hub 的成员解析，不把每条工具逻辑再写一份。节点只创建所需的请求分发对象，不起第二个公网 Hub。
Hub 侧保留 `Peer<T: Transport>`，把 stdio 行传输换成一条 WebSocket Text message；
不改造 ccnm 的 `Connections` 为节点池，避免沿用其按 workspace/主体建立多连接的规则。
新接入/连接状态放小型 `nodes/` 模块，`hub` 只新增分发分支；CLI 与 daemon 复用实现。
不借机拆完 4000 行 hub、重构所有 bridge 或创建通用插件框架。

桌面库按 Cargo feature/发行配置选入。纯服务器包不能因没有 X11/PipeWire 库而连文件命令都无法启动。
安装助手根据用户选择取得对应发行件，二进制仍叫 gld，命令和数据格式一致；不是让用户再管理另一款软件。
macOS 如需要稳定签名 App 身份，由同一安装包提供最小原生 bundle，不引入 GUI 框架或要求另装 Cua。
现有发行包未做 macOS 代码签名，不能许诺加几个 crate 后升级永远不用重新授权；发布件必须验证。[S11]

## 6. 机器、工作区和调用方不能混为一体

机器使用随机持久 `machine_id`，名称只作展示；不采集硬件序列号，不用 hostname 当认证。
首版一个节点配置只绑定一个 Hub、一个执行账号；多 Hub 接管和跨用户桌面切换后置。
重复安装、改名、重连保持 ID；复制节点状态到另一机器导致身份冲突时拒绝并指引重新配对。

拟提供只读 `list_machines`，展示调用方可见机器的在线状态、OS、能力与拒绝原因。
目录/命令工具仍按 `workspace` 路由。远端 gld 的 workspace 是 Hub 发出的不透明句柄，内部映射
`machine_id + remote_workspace_id`；远端路径只在目标机解析，不伪装成 Hub 本地目录。
列表给出友好名称 `机器/项目`，同名必须选定目标，离线不回退到 Hub 或同名机器。

非项目任务用拟议 `work_session start machine=<ID>`，在该机器的 GLD_HOME 下创建；后续使用返回的 workspace。
已有省略 machine 的 start 保持本机语义，说明为兼容行为；模型跨机操作必须明确选机器，不能沿用“上次机器”。
任务与产物句柄绑定目标和主体，重连后不能把节点 A 的产物 ID 解到 B；远端通用任务空间在项目链路通过后扩展。

原生 gld 节点经能力校验后复用相同文件/命令工具名，不再给每台机器复制一套工具表。
ccnm 仍走原 `remote_*` 契约；不因为两者都叫远端就强行互换参数。
动态机器列表通过固定工具返回，加入机器不需要每台新建连接器；第一次新增公共工具表可能需要一次 Refresh。

首版节点功能只对完整认证主体开放，已有项目 grant/noauth 不自动扩展成整机权限。
后续需要 scoped machine grant 时再增加显式范围；不先做企业多租户。
节点请求的有效权限是 Hub 授权与目标机本地允许范围的交集，只有目标机操作员能扩大本地范围。
目标端 Caller 按 `hub_id + 已验证主体 + grant范围` 构造，不转发原始 OAuth 令牌或 Hub 管理员密钥。
单连接不等于单主体：调用身份放在由 Hub 填写的 `params._meta` 中，不放进用户工具参数；
Hub 丢弃客户端伪造的内部身份字段，节点只接受已认证 Hub 带来的上下文，不再以不同身份另建 socket。
共享 bearer 仍是共享主体，不能宣称自然人隔离。既有 ccnm 按主体分连接的兼容路径不变。
文件路径限制与 guarded 命令策略不构成 OS 沙箱；任意获准进程仍以节点执行账号的权限运行。
需要强隔离时使用专用系统账号或容器，不因减少配置就默认提升管理员权限。

## 7. 配对、长连接和断线行为

### 7.1 配对与撤销

拟议本机管理命令为 `gld machine invite/list/revoke`；目标命令为 `gld join`、`gld leave`。
不复用现有 `gld agent` 名字：那个命令管理编程 Agent 委派。
邀请只能由操作员生成，默认 10 分钟、一次使用、至少128位随机秘密，服务只存其摘要并限制尝试次数。
配对事务带本机持久请求标识，响应丢失重试只恢复同一个登记，不生成一堆重复设备。
首次接入显示 Hub 身份、执行账号、共享范围和桌面开关，让操作者一次确认；日常动作不重复要求配对。

目标机获得只属于自己的可撤销节点凭据，安全文件仅本账号可读；永久凭据不进 argv、URL 查询、日志或聊天。
邀请凭据与节点凭据不能访问 `/mcp`，MCP 的令牌也不能假冒节点；node socket 只接受该节点的响应和状态，
不能让节点反过来给其他机器或 Hub 发工具调用。撤销后关闭连接、拒绝重新认证，本机 leave 可独立断开。
Hub 怎样向节点证明身份见 §7.3；TLS 校验失败不自动降级，也不做 `--insecure` 静默重试。

### 7.2 标准 JSON-RPC，复用已有处理链

每个 WebSocket Text message 对应一条现有 JSON-RPC 请求、通知或响应，形状不变：

```json
{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"read_file","arguments":{"workspace":"<目标机项目ID>","path":"README.md"}}}
```

`Peer` 继续分配请求 ID、等待匹配响应、判断超时和传播 `result/error`；目标机把解码后的消息
送入已有 `handle_request`。初始化/工具能力由现有 initialize 与 tools/list 获取，不建立第二份能力协议。
请求的 `id` 只关联本次请求和响应，不是跨断线幂等键。[S15] 不新增 operation_id 数据库、收件确认、
持久 outbox、消息补发或断点续传协议；需要主体上下文时只使用上节的受信任 `_meta`。

每机只有一个 `Peer`，第一步沿用其串行请求语义，各机器之间独立执行。长命令仍先返回现有
`session_id/output_ref`，随后使用已有 `list_runs/read_output/kill_session`，不让整段构建占住连接。
握手、调用、写入和响应大小要有界；标准 Ping/Pong 由 socket 层处理，不新增业务心跳方法。
不为未来截图预建媒体通道、控制连接或优先级调度器。

断线使该连接的在途请求结束：发送前明确失败就报告未发送，已发送但没拿到结果则报告结果不明，
**不自动补发**。节点普通退避重连后重新鉴权/initialize/list；只有调用方主动查询已有运行记录，
才回读命令状态与输出。没有拿到 session_id 时按现有 list_runs 检查；无法唯一核实就保留未知，
不把 JSON-RPC 的新 id 当成重复执行保护。已启动进程仍受原有 timeout/资源策略约束。
离线直接返回 `MACHINE_OFFLINE`，不排队等未来突然执行。节点重复连接不另建备用连接；
只有旧连接已确认关闭才接新连接，断线不会回退到 Hub 本机或其他同名项目。

**单次调用超时必须比工具自己的等待长（2026-10-30 已改到 60 秒）。** Hub 等节点回包的上限 `CALL` 原来是
30 秒（`nodes/registry.rs`），`exec_command` 的 `yield_time_ms` 上限也是 30 000 ms，带端口起服务、
不传 yield 时默认等满 30 秒。模型按上限传参时节点要 30 秒多才回，Hub 一定先超时；`Peer` 超时算致命错误，
整条连接被关、节点重连，模型拿到“结果不明”，这段时间该机其他调用先报 `MACHINE_BUSY` 再报离线。
用户认为操作和网络都可能超过 30 秒，定为 60 秒，也就是原方案“工具上限加传输余量”，余量从 15 秒放到 30 秒；
没走“节点路径压 yield”：那会让同一个参数在本机和目标机上限不同。60 秒是上限，不是每次都等。
它不能超过节点的 `IDLE_TIMEOUT`（也是 60 秒）：调用期间 Hub 不发 Ping，节点那么久收不到任何帧会自己断开。
文件写入不受影响：写锁的 30 秒等待只发生在同一进程内，而 Hub 对每台机器同一时刻只有一个在途调用。

单在途调用的代价也要写明：一个调用方的命令还在 yield 等待时，其他调用方对这台机器的请求立刻收到
`MACHINE_BUSY`（未发送，可重试）。首版接受；实际使用中 BUSY 频繁时，再在同一连接上允许少量并发请求
（JSON-RPC 的 id 本来就能区分），仍不加连接。

**关连接不陪等在途调用（2026-10-30 已改）。** 撤销、轮换和停服务时，在途调用最多再等 2 秒，之后被切断，
调用方拿到结果不明。放宽到 60 秒后，全等的话 `gld daemon stop` 最坏要约 90 秒，而它只等 20 秒。
切断靠 `SocketTransport` 上的一个通知：调用方持着 Peer 的锁阻塞在收发里，别的线程没有别的办法叫停它。

**命令权限单独一级（2026-10-30 已做）。** 目标机用 `--run` 点名项目才开 `exec_command`、`write_stdin`、`kill_session`，
自带文件写入：能跑命令就能改任何文件，“能跑不能写”只是看着安全。按项目判断，判定点在节点上；Hub 用目录里的
`tool_profile` 提前拦一次，只为把报错写明白。命令按 `hub_id + 主体` 分开，沿用本机的会话表和运行记录，
没有新的隔离机制。节点退出（Ctrl-C、SIGTERM、撤销后重连被拒）时停掉自己起的命令，记 `interrupted`；
`kill -9` 或崩溃时收不了，记录显示 `unknown`，命令继续跑到有人手动停。
`--write` 只是不开放命令工具：仓库配置本来就指向仓库里的脚本时，改脚本等于能执行（审查 §96）。

**超大回包只让这一次调用失败，节点不退出（2026-10-30 已修）。** 原来节点回包超过 8 MiB 时，`nodes/client.rs`
记为永久失败，`gld node run` 不再重连、直接退出，机器离线到有人手动重启。`view_image` 默认输出里
同一张图的 base64 出现三次（`content[].data`、`structuredContent.base64`、`data_url`），回包约为原图的
4 倍：原图约 2 MiB 以上、又没触发 5 MiB/2000 像素的自动缩放，就会撞上。它在节点只读工具名单里，
M0 起就能调到。现在 `tools/call` 超限时回一条 `NODE_RESPONSE_TOO_LARGE` 工具错误，写明“已执行、结果被丢弃”，
其他方法回 JSON-RPC 内部错误，连接和进程照常。没用 JSON-RPC 错误回 `tools/call`：Hub 会把它显示成
“节点拒绝了请求”，而补丁这类调用其实已经生效。

**节点路径上的图片只留一份编码（2026-10-30 已做）。** 只在节点发回包前裁，本机客户端的输出不变：默认只回图片块，
约为原图 1.33 倍，默认 5 MiB 的图放得下；`output=data_url` 只留 `data_url`，它要在结构化结果和文本块里各一份，约 2.67 倍。
没加新参数：改工具表要所有连接器点 Refresh。

### 7.3 Hub 要向节点证明自己（2026-10-30 用户定为 A 和 B 都做，已实现）

起因：节点原来认 Hub 只靠 TLS 证书链，加上升级响应里的 `x-gld-hub-id`。hub_id 写在邀请里、
每次握手原样回传，不是秘密；节点凭据则在 Hub 证明任何东西之前就放进了升级请求。
所以谁能为 Hub 域名拿到有效证书，谁就能冒充 Hub：劫持 DNS 后用 ACME 签证书、控制 Cloudflare 账号，
或目标机用 `ca_certificate` 信任的私有 CA 被别人拿去签证书。冒充者能收走节点凭据，并用节点信任的
`_meta` 下发调用；M1 接上写入和命令后，等于在每台目标机上以节点执行账号运行任意命令。

| 方案 | 做法 | 挡得住 | 挡不住 | 代价 |
| --- | --- | --- | --- | --- |
| A 钉住 Hub 公钥 | Hub 持有一把 Ed25519 私钥；公钥指纹写进邀请令牌，配对时核对后存进节点配置。每次连接节点在升级请求里带随机 nonce，Hub 在 101 响应里返回对 `nonce + machine_id` 的签名；验不过就断开，不处理任何调用 | 只冒充、不把连接转给真 Hub 的攻击者 | 拿到证书后把连接转给真 Hub、再往里插调用的中间人；托管反代的运营方 | 每次连接签、验各一次。ring 0.17 已在锁文件里，改成直接依赖即可，不加新 crate |
| B 在 A 上逐条签名 | Hub 对每条 `tools/call` 签名，覆盖本连接 nonce、递增序号和请求原文；节点先验签再解析 | 上面两类中间人插入或改写调用，只剩丢弃和延迟 | 中间人仍看得到调用内容；Hub 主机本身被攻破 | 每条调用签、验各一次，微秒级。签名放哪、覆盖哪些字节要先定，不能依赖重新序列化 JSON 后的键序 |

用户不接受“反代运营方或能签出域名证书的人可以在目标机执行命令”，两档都做，并在命令执行接入节点之前完成。
两档都不防 Hub 主机被攻破，也不让反代看不到内容，与 §3 的信任边界一致；对外说明在 security.md。

实现取舍（代码在 `nodes/hub_key.rs`）：

- **钥匙。** Hub 第一次登记、邀请、撤销或轮换时，与 hub_id 一起生成 Ed25519 私钥，存进 `profiles.json` 的
  `node_hub_key`。邀请令牌变成 `hub_id.邀请ID.秘密.Hub公钥` 四段，公钥随邀请走带外渠道，配对本身也不再只凭 TLS；
  预置登记的输出带 `hub_public_key`，由操作员填进配置。配置缺这个字段直接读取失败，不退回只认证书。
- **握手（A）。** 节点每次连接生成 32 字节随机 nonce，放在 `x-gld-node-nonce`；Hub 先认节点凭据再看 nonce，
  在 101 响应的 `x-gld-hub-proof` 里返回对 hub_id、machine_id、nonce 的签名。节点验不过就退出，不再重连。
- **逐条签名（B）。** 签的是 Hub 发出的**每一条**消息，不只 `tools/call`，initialize、tools/list 一样签，
  省得哪类消息漏签。签名覆盖 hub_id、machine_id、本连接 nonce、从 1 递增的序号和消息原文，
  以 `"gld_sig":"序号.签名"` 追加为 JSON 最后一个成员。节点按这个后缀切回原文验签，不重新序列化 JSON，
  所以没有键序和浮点格式的问题；验过才解析，交给 `handle_request` 的也是切回的原文，追加的成员不进工具层。
  超过消息上限的消息不占序号，报告“未发送”。
- **不做的。** 节点发回 Hub 的结果不签：中间人能改结果，但不能借此让节点多执行任何东西。
  节点凭据仍在 Hub 证明身份前发出，被截后用 `rotate-credential` 换。Hub 私钥不支持单独轮换，换了就全部重新配对。

### 7.4 代理与证书

现状不一致：配对用 reqwest，会跟着 `HTTPS_PROXY`/`NO_PROXY` 走代理（Hub 是回环地址时强制直连）；
节点长连接用 tokio-tungstenite 的 `connect_async_tls_with_config`，它自己直连目标地址，不认任何代理变量。
只能经代理出网的机器上，会出现 `gld join` 成功、`gld node run` 一直连接超时。长连接应与配对用同一套代理判断：
先向代理发 HTTP `CONNECT`，再把这条 TCP 交给 `client_async_tls_with_config` 做 TLS 和 WebSocket 握手。
CONNECT 隧道里 TLS 仍是端到端的，代理看不到节点凭据；回环地址照旧不走代理。
暂不支持的代理（SOCKS、PAC）要明确报错，不能落成笼统的超时。

证书根现在只有随包的 webpki 根证书（Mozilla 那一批），不读系统证书库；配了 `ca_certificate`
时配对和长连接都改为只信这一张 DER 证书。自签 Hub 这样够用。公司在系统里装了自己的根证书、
对出网 HTTPS 做解密检查时，节点会校验失败，要用户自己导出 DER 再配置。建议同时接受 PEM，
并在报错里点明“证书不受信、怎么配 `ca_certificate`”；不加 `--insecure`。

## 8. 基础桌面：库封装，不做桌面运行平台

新增一个聚合工具 `desktop`（拟议）即可，不再实施 RFC-0011 原先三套会话接口。
它显式使用 workspace，机器由 workspace 路由唯一确定；本机和远端复用同一实现。

| 动作组 | 底层 | 验收结果 |
| --- | --- | --- |
| status / targets / screenshot | XCap + 少量平台检测 | 明确显示会话/显示器、截图像素、缩放和坐标映射；直接返回 MCP 图片 |
| open | open + 必需的平台启动适配 | 打开指定文件/目录/http(s)地址；应用启动使用平台标识，不执行任意 shell |
| click / scroll / type / key | Enigo | 明确单次输入、中文/换行、组合键；不暴露无限按住和长宏队列 |
| clipboard_read / clipboard_write | Arboard，按需加入 | 只在用户任务明确需要时调用；不自动同步剪贴板，不记录其正文 |

可控前台窗口聚焦如无可直接用库，只补各平台实际需要的少量调用，不扩成完整窗口管理器。
打开 URL/文档仍会运行系统处理程序；限制 URL scheme、路径范围和参数，不能把 read-only 项目权限当桌面授权。[S9]
模型负责理解画面、决定动作并再次截图核实；执行器返回“输入已投递”不能替代业务结果检查。

保留必要且局部的保护：目标机本地桌面开关；按显示会话短期占用避免两个任务互相打字；
截图引用绑定本次目标和坐标变换，显示器/窗口/登录会话变化后拒绝旧引用；动作超时、断线和停止后不再发新输入。
组合键内部配对释放；停止不能确认时标成受阻，不能马上交给下一任务。复用现有任务归属和运行记录，
不另建桌面任务数据库、持续屏幕录像或 provider 调度器。

截图回传要先压到固定上限。节点单条消息上限 8 MiB、写超时 30 秒（`nodes/transport.rs`，2026-10-30 从 5 秒放宽），
发满上限要约 280 KB/s（2.2 Mbps）上行。L0 仍默认把截图缩放压缩到几百 KB（例如长边 1600 像素的 JPEG）：
写得完不等于用着顺，5 Mbps 上行发 4 MB 还是要约 7 秒。

## 9. 跨平台是交付范围，不是所有环境能力相同

| 环境 | 运行方式与基线 | 必须如实报告 |
| --- | --- | --- |
| macOS Intel/Apple 芯片 | 当前登录用户下运行，用户级启动；验证签名/路径与 TCC 授权身份 | 未授权录屏/辅助功能、锁屏、无图形登录；一次必要的系统确认不能自动跳过 [S11] |
| Windows x86_64 | 默认用户登录进程/登录计划任务，不默认提升为 SYSTEM | Session 0 无交互桌面、锁屏、UAC 安全桌面/高权限窗口；普通服务启动不等于能点桌面 [S12] |
| Linux x86_64 X11 | 有图形会话的用户进程/用户服务，检测显示服务与会话变量 | 缺少 X11 授权或运行库明确提示；无 GUI 时文件/命令仍可用 |
| Linux Wayland | 列入专项验收，不标成通用 X11 等价；优先复用 portal/libei 等已有路径 | 按 compositor/portal 分开报告 capture/input/clipboard；选择屏幕/设备可能需用户确认 [S6][S7][S8][S13] |
| 纯服务器、容器、未登录设备 | 节点连接与文件/命令可独立使用；桌面依赖不随 headless 包强制安装 | 返回 desktop_unavailable，不造虚拟显示器、不自动装 VNC |

桌面模式首版只服务当前执行账号的一个已登录会话；不自动切换用户、解锁屏幕或操作安全桌面。
要在未登录时持续跑服务器命令，可显式安装 headless 系统服务；桌面助手与该服务之间的跨账号 IPC 留待真实需求，
不为了“任意场景都行”先做双守护进程。Linux 剪贴板需正确保留所有权，不能调用后立即销毁对象再声称已复制。[S8]

## 10. 文件与清理在哪里发生

目标机自己的 GLD_HOME 保存 work、artifacts、runs 和资源登记，Hub 保存路由与设备认证；不新增跨机请求回执库。
远端截图使用已有有界图片回传；保存只在任务要求时进行，不默认把所有屏幕留在 Hub。
需要下载原件时，按授权产物 ID 进行有界分块读取/中转、校验大小和摘要，复用同一节点通道；不要求目标机再开文件服务器。
Hub 转发缓存临时且有界，不把远端本机路径误称为客户端可下载链接；原件仍在目标机，离线时新取原件必须报告不可用。
首次只做产物交付，不建设文件同步/NAS/任意机器互拷平台。

finish 必须同时核对该节点命令及本次桌面动作；不因 Hub 断线就删除目标机目录。
回执与保留结果沿用 W1，删除临时源前先核验；断线、上传中断、容量不足时保留唯一有效副本。
用户原有应用/浏览器不是 gld 子进程，不在收尾时关闭；只收本任务有明确归属的临时资源。

## 11. 切片与验收

下表只定义交付，不保存当前状态。**先提供能用的小步，不把全平台全部能力做完作为首个可用版本前提。**

| 切片 | 最小交付 | 完成证据 |
| --- | --- | --- |
| D16-M0 | 原生节点窄纵切：隔离 Hub + 两个节点，单 WSS + JSON-RPC，复用 Peer/handle_request 读取多项目 | 同一节点两个项目共用一条连接，两个节点同名项目分别返回正确内容；节点无入站监听、无 ccnm；凭据混用/离线/超时/未知参数负测 |
| D16-M1 | 配对、持久机器/项目登记、项目写入与命令、撤销及普通重连 | 一条客户端连接列举/读取/修改两机项目；运行、查询、停止命令；跨主体结果隔离、失联不重跑、重复加入不新增设备；不另造恢复协议 |
| D16-M2 | 平台安装脚本、用户级自启动、诊断/leave/卸载 | 干净 Mac/Windows/Linux 各用一次生成命令接入；不需 Node/Python/编译器；失败不破坏原配置，卸载默认保留用户产物 |
| D16-M3 | 单一公网入口的三平台项目验收与交付收尾 | 同一域名/隧道、一个连接器列举及操作三类节点的项目；每机恰好一条连接；重启身份不变、路径/权限负测和运行输出正确，不含桌面依赖 |
| D10-L0 | 多机项目通过后再做基础桌面：最小库依赖、状态/截图/打开，不依赖 Cua | 三平台构建；Mac 真实图片与打开验证；权限拒绝、不支持、空显示器准确；记录其他真机尚未验的项 |
| D10-L1 | 基础输入/剪贴板及远端桌面，复用 M1 路由 | 指定机器计算器 12+30=42、中文/换行/标点文件读回；两任务争用、DPI/窗口变化、断线/停止不重放 |

固定顺序为 **M0 → M1 → M2 → M3 → L0 → L1**。前三平台的项目链路不受桌面库、GUI 会话、录屏权限或 Cua 安装阻塞。
M0 先验证传输与现有项目分发的最小接线，再补正式监听鉴权和路由；测试链路通过不冒充配对/安装已可用。
每步先专项再回归、独立 commit；三端构建不是三端真机验收。Windows/Linux 未有真实设备证据时明确列未验证，
不能把 macOS 结果外推。Wayland 单独保留能力缺口；跨平台承诺须有对应真实发行件证据。

验收除功能外还要看用户负担：目标机零入站端口、零单机隧道、零手写配置、零额外 AI 运行时，
只需安装接入/配对确认和系统必需授权。测试日志不保存真实输入、剪贴板、屏幕或凭据。

## 12. 升级和范围控制

已有 Cua probe 不安装也不自动删，保留兼容；暂停其扩建及复杂 provider 会话/模型路线。
后续真需要移除该独立实验命令，再单独删除代码、帮助和测试，不回滚 W0/W1 或其他已验证能力。
ccnm 配置/remote_* 和原有 MCP relay 保持兼容；原生节点不默认把其所有本机 MCP/Skills 暴露给 Hub。

Hub/节点版本握手报告能力；滚动升级允许短暂离线，保持 machine ID、配对凭据、目录归属及客户端 OAuth 地址。
按现有空闲检查换二进制再重启，不承诺跨版本不中断的动作，不把 RFC-0008 设为前置。
安装、自启动、系统权限、实际服务换装与发布均在对应执行时取得授权；本轮只落方案，不修改现网配置或依赖。

## 13. 研究依据

2026-10-28 核对；上游声明不等于本机验收，编码时固定对应版本。

- [S1 Axum WebSocket](https://docs.rs/axum/latest/axum/extract/ws/index.html)：已有 WebSocket upgrade/读写支持。
- [S2 tokio-tungstenite](https://docs.rs/tokio-tungstenite/latest/tokio_tungstenite/)：Tokio WebSocket/TLS 客户端与传输。
- [S3 Cloudflare WebSockets](https://developers.cloudflare.com/network/websockets/)：代理支持、空闲断开与心跳。
- [S4 OpenAI Secure MCP Tunnel](https://developers.openai.com/api/docs/guides/secure-mcp-tunnels)：支持产品到私有 MCP 的出站调用通道。
- [S5 Desktop Commander Remote Device](https://github.com/wonderwhy-er/DesktopCommanderMCP/blob/main/src/remote-device/README.md)：出站 WebSocket、设备配对和本地 MCP 转发。
- [S6 Enigo](https://github.com/enigo-rs/enigo)：跨平台键鼠，Wayland/libei 的实验性边界。
- [S7 XCap](https://github.com/nashaofu/xcap)：显示器/窗口采集矩阵与 Linux 系统依赖。
- [S8 Arboard](https://github.com/1Password/arboard/blob/master/README.md)：Wayland 条件及 Linux 剪贴板所有权生命周期。
- [S9 open](https://docs.rs/open/latest/open/)：系统关联程序打开路径/URL及安全边界。
- [S10 Iroh](https://docs.rs/iroh/latest/iroh/)：QUIC、打洞、身份与中继；不作为本期依赖。
- [S11 Apple 录屏授权](https://support.apple.com/guide/mac-help/control-access-screen-system-audio-recording-mchld6aa7d23/mac)、[辅助功能授权](https://support.apple.com/guide/mac-help/allow-accessibility-apps-to-access-your-mac-mh43185/mac)；发行现状见本仓库 install.md。
- [S12 Microsoft Interactive Services](https://learn.microsoft.com/en-us/windows/win32/services/interactive-services)：服务与用户交互桌面分离。
- [S13 XDG Remote Desktop](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html)、[ScreenCast](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html)：设备/屏幕授权及恢复语义。
- [S14 RFC 6455](https://www.rfc-editor.org/rfc/rfc6455)：普通 WebSocket 握手、Text message、Ping/Pong 和关闭；本期不采用 RFC 8441。
- [S15 JSON-RPC 2.0](https://www.jsonrpc.org/specification)：传输无关的请求/响应、id、通知和标准错误，不提供重连恢复语义。

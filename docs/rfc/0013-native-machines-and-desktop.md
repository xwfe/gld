# RFC-0013：一个入口管理多台机器，按需提供基础桌面操作

日期：2026-10-28。本文是经用户要求收敛后的实施设计，**不是已实现功能说明**。
当前认领与完成状态只在 [task_plan.md](../../task_plan.md)；当前可用命令以
[concepts.md](../concepts.md) 和 [cli.md](../cli.md) 为准。下面的 join、machine 和 desktop 接口均为拟议接口。

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
3. 用户说“在办公电脑打开文件”“检查测试服务器”，模型查询可见机器后自动选择内部 ID、创建任务空间并执行。
   用户不需要理解 workspace ID、网络端口和 provider。

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

“一个连接”指 AI 客户端只登记一次服务；内部每台机器各有一条出站连接，不是所有机器共用一条 TCP。
推荐 WebSocket over TLS（WSS）：Hub 复用已有 Axum，节点用 tokio-tungstenite/rustls，
不自研 WebSocket/TLS，也不把这条内部协议冒充标准 MCP WebSocket 传输。[S1][S2]

| Hub 所在位置 | 一次性准备 | 目标机要求 |
| --- | --- | --- |
| 公网服务器 | 固定域名、TLS、转发到 gld | 能出站访问该 HTTPS/WSS 地址 |
| 家庭/办公内网 | 只在 Hub 使用一个固定 Cloudflare Tunnel、FRP 或已有反代 | 同上；不需路由器端口映射 |
| 同一可信 LAN/已有 VPN | 已可达的 Hub 地址和信任配置 | 使用该网络；不强制安装 VPN |

推荐同一个 HTTPS 基地址下分 `/mcp` 与 `/nodes/v1/*`；旧 `/hub` 基路径等前缀要保留，
不能随便从 MCP URL 裁掉路径推导节点入口。两类接口同端口但鉴权完全分开。
反代只开放了 `/mcp` 时，安装助手应报告一次性扩展节点路由的要求，不让每台机器单独改隧道。
Cloudflare 支持 WebSocket，但会因空闲或服务更新断开；必须有心跳、重连和重连后状态核对。[S3]
TLS 在托管反代终止时，反代运营方处于信任边界内；本方案不声称内容对中继不可见的端到端加密。

**可达性有前提。** 至少一个稳定 Hub/转发入口必须能被节点访问；所有机器都在不互通的私网且
没有共同入口时，不能仅靠一条命令保证互联。Hub 休眠或关机时远端访问也不可用，适合放在常开设备上。
流量首版经过 Hub，有带宽和单点代价；不承诺免费云服务、不自动建立公共中继。

已有 OpenAI Secure MCP Tunnel 是 OpenAI 产品到私有 MCP 的调用路径，不是任意机器可连接的
通用 WSS 入口。[S4] 可以保留这条 AI 连接，但还需一个**统一的节点可达地址**；不能把 tunnel_id 发给节点就说完成了多机网络。
跨网新用户优先选择同时承载 MCP/节点的固定 HTTPS 入口；不自动改掉既有用户连接。

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

节点复用 `build_tool_context`、`call_tool_as`、runs、managed_workspaces；CLI 与 daemon 使用同一份节点服务实现。
新接入/连接状态放小型 `nodes/` 模块，`hub` 只新增分发分支；不借机拆完 4000 行 hub 或重构所有 bridge。
公共协议类型等确有两处消费后再提小模块，不先建立通用插件框架。

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
任务与产物句柄绑定目标、主体和创建代次，重连后不能把节点 A 的产物 ID 解到 B。

原生 gld 节点经能力校验后复用相同文件/命令工具名，不再给每台机器复制一套工具表。
ccnm 仍走原 `remote_*` 契约；不因为两者都叫远端就强行互换参数。
动态机器列表通过固定工具返回，加入机器不需要每台新建连接器；第一次新增公共工具表可能需要一次 Refresh。

首版节点功能只对完整认证主体开放，已有项目 grant/noauth 不自动扩展成整机权限。
后续需要 scoped machine grant 时再增加显式范围；不先做企业多租户。
节点请求的有效权限是 Hub 授权与目标机本地允许范围的交集，只有目标机操作员能扩大本地范围。
目标端 Caller 按 `hub_id + 已验证主体 + grant范围` 构造，不转发原始 OAuth 令牌或 Hub 管理员密钥，
不接受工具参数伪造身份。共享 bearer 仍是共享主体，不能宣称自然人隔离。
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
节点持久身份和 Hub TLS/应用身份保持明确绑定，不做 TLS 校验失败自动降级或 `--insecure` 静默重试。

### 7.2 小型内部协议，不是通用消息平台

沿一个有界 WSS 通道传握手/能力、调用、结果、状态查询、取消和心跳；采用已有 JSON/错误/内容模型。
每个调用携带目标、调用主体、operation_id、连接代次、能力版本与截止时间。协议版本不兼容时拒绝相关调用，
不能忽略未知参数后执行不同动作。独立版本与现有 daemon IPC 分开；改本机 IPC 形状才递增其版本。

节点短调用有超时；长命令复用现有“先返回 session_id，再读输出/停止”，不把一个 HTTP 请求一直挂住。
读写任务并发有上限，单桌面输入串行。心跳/取消不能排在整个大文件后面；媒体分小块、有限队列并有背压。
离线返回 `MACHINE_OFFLINE`，不缓存等待未来突然执行的点击、输入或启动命令。

网络重连可以自动做，**副作用调用不自动重放**。接收确认不等于执行成功：
发送前已知失败可报告未执行，发送后没有终态则报告 unknown，按原 operation_id 查询。
目标端在执行前登记请求意图，重复请求只返回已知状态；崩溃窗口、过期回执或无法核实的结果保持 unknown，
不承诺分布式 exactly-once。桌面恢复先重新截图，再由模型决定下一动作。
已启动命令默认继续受原 timeout/资源策略管理，断线不变成永久 Job；重连按运行记录读取，不重起同一命令。

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
组合键内部配对释放；停止不能确认时标成受阻，不能马上交给下一任务。复用节点 operation_id 和任务生命周期，
不另建桌面任务数据库、持续屏幕录像或 provider 调度器。

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

目标机自己的 GLD_HOME 保存 work、artifacts、runs 和资源登记，Hub 保存路由、设备认证和必要请求回执。
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
| D10-L0 | 本机基础桌面：最小库依赖、状态/截图/打开，不依赖 Cua | 三平台构建；Mac 真实图片到客户端与打开验证；权限拒绝、不支持、空显示器准确；记录其他真机尚未验的项 |
| D16-M0 | 原生节点窄纵切：隔离 Hub + 两个节点，出站认证连接与 workspace 读调用 | 同名目录分别返回各节点内容；节点无入站监听、无 ccnm；跨主体/节点凭据混用/断线/未知参数负测 |
| D16-M1 | 配对、持久身份、命令与托管空间路由、撤销/重连 | 一条客户端连接读写两机；命令结果可查、失联不重跑、重复加入不新增设备、撤销节点不能再连 |
| D16-M2 | 平台安装脚本、用户级自启动、诊断/leave/卸载 | 干净 Mac/Windows/Linux 各用一次生成命令接入；不需 Node/Python/编译器；失败不破坏原配置，卸载默认保留用户产物 |
| D10-L1 | 基础输入/剪贴板及远端桌面，复用 M1 路由 | 指定机器计算器 12+30=42、中文/换行/标点文件读回；两任务争用、DPI/窗口变化、断线/停止不重放 |
| D16-M3 | 单一公网入口真机验收与交付收尾 | 同一个固定域名/隧道支持三类节点；仅一个连接器；重启身份不变、产物分块回传正确，按平台发布已通过能力 |

下一编码切片为 L0（只读观察/打开）；M0 可独立进行，不再等待 Cua 安装。
每步先专项再回归、独立 commit；三端构建不是三端桌面验收。Windows/Linux 未有真实设备证据时明确列未验证，
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

# gld 当前任务与后续规划

更新日期：2026-10-23。**本文件是当前排队与认领入口**；审查与验收过程保留在
[生命周期审查](docs/reviews/2026-09-23-lifecycle-and-docs-audit.md)，用户用法见
[项目开发生命周期](docs/project-lifecycle.md)。已完成项怎么做、怎么验的在审查和 git 历史里，这里不再留勾选清单。
沿用 D / L 原有归属，不新增平行的 Goal、Task 或执行状态系统。

## 当前基线

最新发布 0.8.3（`v0.8.3` = `a4efa44`，下载包校验和、来源证明、macOS 与 Linux 实跑见审查 §21；改了什么见
[发布说明](docs/releases/v0.8.3.md)）。main 上未发版的：`87eba6b` 补丁失败留下原因（审查 §22）；`98a4ef7`–`778fdd3`
通知回 202、OAuth 动态注册写盘保护、`gld grant browser`（SuperAssistant 接 DeepSeek 网页）、升级文档（审查 §23–§25）；
`e8f7f36`–`f9400f7` 点名说明在 compact 下也注入、收尾可重试、命令回显有界、§28 复核出的经 gld 开发的摩擦（审查 §26–§31），
`gld --version` 带构建提交（§35）；`02e1afb`–`fba39d0` 认 Claude Code 开着的插件带的 skill、扫描时市场清单只读一次（§37、§38）；
`0209cd3` Cloudflare Tunnel Token 不进任何命令行、`a034476` 初始化说明补四条看结果的规矩（对照 codex-bridge，§39）；
`825b74a` 有命令在跑时 `gld daemon restart` 不重启、列出来（§40）；`e265ca6` `gld remote add <项目名>` 精简、默认 coding；
`0324602`–`267cdf9` Agent 委派 D1：`agent_manage`、`gld agent`、项目开关 `agent-delegation`（§42）。
**守护进程协议从 7 起**（`85804aa`），工具表多了 `agent_manage`。
- 本机：`c99c8cd`，守护进程 2026-10-23 14:31 换上（审查 §49），带上了 `a5cf900`–`188f60c`。协议 7；工具表 34 项、
  指纹 `e0d37cabe9356c13` → `a0c81b8f6d0580cd`（只有 `agent_manage` 变了），**ChatGPT 要点一次 Refresh**。守护进程是去掉 `CLAUDE*`
  变量后起的；已点名注入 `~/.codex/AGENTS.md`。临时项目 `delegation-test`（`~/xdw/gld-delegation-test`，`agent-delegation=ask`，
  工作区里故意留着前几轮委派没提交的改动）还挂在服务上，不要了就 `gld rm delegation-test`。
  回滚用 `~/.local/opt/gld-59f40ad-20261023-1427`（换之前那版）和 `gld-config-20261023-1427.tgz`（0600，有明文凭据）；
  另有更早的 `gld-a8f9993-…`、`gld-6268bb0-…`（协议 6）、`gld-fba39d0-…`、官方 0.8.3 包和几份旧数据备份，要清的话说一声。
- 另一台 Mac、Linux 服务器的普通账号和 root：还是 `fba39d0`，`gld --version` 打 `gld 0.8.3 (fba39d049e5c)`（审查 §38）；
  只装了命令行，没起守护进程（审查 §33）。
- 全仓：本机 1033 passed / 0 failed（`b89ffe6`，§52）；Linux 服务器 989 是 `fba39d0` 那次的（差一条只在 macOS 编译的）；
  线上 CI 最近一次是 `fba39d0`，8 个 job 全过。`0209cd3` 起的提交只在本地 main，没推送。

**已推送**：2026-10-18 用户看完三台换装和 §34 的实测后让推，main 推到 origin，没发版。CI 头一次跑这些提交红了三个 job（Windows 编译检查、两个平台各一条测试），`4b84a1c`、`d78461d` 修完全绿（审查 §36）。

**从集中修缺陷转入日用稳定与场景验收，不开始另一轮大重构。** 不把"可以执行任意项目脚本"包装成无人值守的完整开发平台。
升级照[安装 · 升级](docs/install.md#升级)做：换二进制、重启守护进程（零点几秒，在跑的命令会被停掉，所以先查有没有），
连接器的 client_id、授权、地址都不动。

## 已交付，不重新立项

| 范围 | 当前结论 | 保留的边界 |
| --- | --- | --- |
| U0–U5、G1–G3、RFC-0004/0005/0006 | 工具可靠性、单服务多项目、Skills、Notebook、MCP 转发已落地 | 具体客户端/远端链路仍按实际证据判断，不重复造功能 |
| D01–D03、D11 | 正式验收、基线复核接纳、命令终态传播、任务存储并发与损坏处理已修 | 退出 0 不证明测试充分；暂停不等于停止已有命令；不是强断电持久性保证 |
| D04–D06、D14 | 工具表核对、结果保真、文档门禁、Git 文件清单已修 | 当前连接的旧 schema 缺失已消除；其他客户端不能直接外推 |
| D07–D08 | 最小项目 grant 和真实安全标注已交付；ChatGPT 上用 grant 口令授权实测过（审查 §9） | grant 默认只读；`--write` 只给可信主体，不是 OS 隔离；grant 无本机 MCP/远端写能力 |
| D09 | 结局与有界输出落盘，重启后可按已知 id 读取；`list_runs` 只列自己的（0.8.1） | 不恢复进程、不重放；跨重启旧结果不自动成为有效验收证据 |
| D10 | Rust、Web、gld 自身三轮实测（审查 §10–§12）；`list_project_commands` 与 `ci_steps`、装依赖要 `confirm`（0.8.2，§13–§17）；服务模式与浏览器配法（0.8.3，§19–§21） | 就绪只到端口连得上，不验业务；浏览器的截图和 console 记录不进项目、不绑任务 |
| D12–D13 | 0.8.0–0.8.3 发布流程、校验和、来源证明已有记录；glibc 包在 Debian 12 容器实跑过；既有协议回退已核对 | 下载包验收必须匹配版本；协议回退不是全面支持新协议 |

`gld tool call` 的 CLI 退出码表示工具调用是否成功，这是已确认的契约，**不要再次当缺陷改掉**；
命令结果看 `command_ok` / `exit_code`。

## 当前待办队列

| 归属 | 状态 | 下一步与完成标准 |
| --- | --- | --- |
| 补丁失败的原因分布 | 2026-10-18 数过一次，维持逐字比对 | 30 个原因里缩进 2 个、行尾空格和标点 0 个，没有放宽的依据；最多的是版本冲突（18 个），`3eb5fe4` 已处理（审查 §31）。再攒一阵，`indentation` 等明显变多再议 |
| SuperAssistant 接 DeepSeek 网页 | 本机已换上（2026-10-17），没实测 | 用户自己在终端 `gld grant browser web-mcp --all`（或点名项目）拿地址（地址就是凭据，不经过 AI），扩展选 Streamable HTTP 连上，在 DeepSeek 页面走一圈 `list_workspaces` → `workspace_context` → `read_file`，看结果填回、DeepSeek 接着说。报 `Failed to fetch` 且 `gld logs` 没有请求，先查 Chrome 的本地网络访问权限。结果记审查 |
| 用户规则送到 ChatGPT | 已部署（2026-10-18，审查 §32） | `gld context -w gld` 里全局规则和仓库 `AGENTS.md` 都打 ✓。看 ChatGPT 下一轮经 gld 开发：提交说明有没有类型前缀和正文、文档有没有照规则写、撞到 gld 的毛病有没有写出来。没改善再考虑工具层提醒（比如回包里报未提交改动量）。见审查 §26 |
| 零中断升级（RFC-0008） | 草案，未实现 | 让在跑的命令和请求跨版本都不断。要不要做、先做哪段由用户定；先确认真实痛点（比如长跑的开发服务器被重启停掉）再立项 |
| 任务收尾与大输入回传 | 源码已修，未部署（2026-10-18） | `8fb5e31`、`63719bc`：收尾可只读重试、附加摘要失败不覆盖终态、命令回显有界；新增 8 条回归先失败后通过，全仓 test / clippy / fmt 通过，见审查 §27，复核见 §28。用户批准后按既有升级流程换二进制、重启守护进程，再从连接器验收；后台写入基线复核保持不变 |
| Claude Code 插件带的 skill | 已推送、三台已装（2026-10-22，审查 §38） | `02e1afb`：Skill 自动发现认 Claude Code 开着的、装着的插件里的 skill，名字带插件前缀（`exa:search`），不扫 `marketplaces/`。新增 6 条单元测试先红后绿，开发机真实主目录上 94 → 97 个，见审查 §37；扫哪些目录现在写在 concepts.md「本机和项目里的 Skill」。复核后补了市场清单只读一次（`f282abe`）；三台换成 `fba39d0`，本机 `gld context -w gld` 已多出 `exa:exa-agent`、`exa:search`、`frontend-design:frontend-design`。managed 策略、插件 `commands/`、claude.ai 同步的插件没做，真遇到再加 |
| 经 gld 开发时的摩擦（审查 §28） | 已部署、ChatGPT 已 Refresh，等下一轮实测（2026-10-18） | §28 的 9 条和复核时另查出的 4 条，能改的都改了（§29–§31），本机已换上（§32），ChatGPT 15:13 已 Refresh（§35）。下一轮看：提交后还有没有 `refresh_baseline`、有没有每秒轮询、还会不会用 `python3 -c` 读文件、`FILE_VERSION_CONFLICT` 多不多。没改的三条和原因在 §31，其中"只改文档也要重跑全仓"等用户定 |
| Agent 委派 | D1 本机已换上；Codex 真跑、ChatGPT 自己发起并审核一轮都通过（2026-10-23，审查 §42–§44） | [RFC-0009](docs/rfc/0009-agent-delegation.md)：`agent_manage`、`gld agent`、`agent-delegation=off/ask/auto`，内置 codex、claude 两份档案，全权跑、做完交回审核。`7f0a87a`（不补回 `.gld/.gitignore`）、`f9bc7f0`（回包给这次任务自己的 diff、在跑时输出尾巴收到 768 字节，§45）10-23 已换上（§46），ChatGPT 再审一轮确认 diff 字段只含这次任务的改动、够它审（§47）。它提的三条 `a5cf900`（每文件增删行数 `file_stats`）、`ba6aaff`（`before_tree` / `after_tree`）、`188f60c`（`action=diff offset=` 分页读完整 diff）10-23 14:31 已换上（§49），ChatGPT Refresh 后委派了一个 175 KB 的改动，三页读完再审、accept，分页和 `task.diff` 逐字节对得上（§50）。§50 末尾 ChatGPT 的五条建议里用户选了按文件读：`0b483f0`（`file_stats` 带每个文件在 `task.diff` 里的位置，`action=diff path=`）、`c8efb4d`（`review` 回包不带 diff）已提交、**没部署**（§51），工具表又变了，换装后要再点 Refresh；`ab28d2b` `gld agent show` 截断时给出全文路径，委派文档对账、README 精简（§52）；大 diff 时 `status` / `wait` 仍附第一页（用户定的）。下一步：换装后让 ChatGPT 用 `path` 审一次大 diff；Claude 真跑一次（花额度，先问）；D2：Pi、agy、Kimi 等档案，放开 `--write` grant |
| OpenAI Secure MCP Tunnel 接入 | 未开始，要用户自己的 Platform 账号 | 第一步不写代码：用户在 Platform 建隧道（勾上要用的 ChatGPT 工作区）、建只开 Tunnels Read + Use 的 Runtime Key，`tunnel-client --mcp-server-url` 指到 gld 本地地址，ChatGPT 里选 Tunnel 建连接器走一圈，重点看 gld 的 OAuth 经隧道通不通。通了再做 `--tunnel openai`（gld 托管 tunnel-client，钉版本、校验摘要）。限制和依据见审查 §39 |
| 重启前等空闲 | 本机已换上、实测通过（2026-10-22，审查 §40、§41） | `825b74a`：守护进程起的命令还在跑时 `gld daemon restart` 不重启、按项目列出来，`--force` 照旧；读盘上的运行记录，换装前用新命令行对着旧守护进程实测拦住了。另外两台换装时用的就是新检查。`gld daemon stop`、launchd 那条路径、`gld restart`（服务重启）没加检查，真有需要再说 |

所有部署、grant 创建/撤销、CI 推送、本机升级和发布仍须取得对应授权。

## 等遇到再做

不排进默认开发，条件出现了再认领。

| 项 | 什么时候做 / 怎么做 |
| --- | --- |
| Windows 下载包真机实跑 | 有 Windows 机器时。CI 的 Windows job 跑过 `daemon_lifecycle.rs` 9 条，跑的是构建产物，不是下载包 |
| Windows CI 下一组测试 | 改到 Windows 相关代码时，从 `run_records.rs`、按 cwd 找项目（8.3 短路径）里挑 |
| ChatGPT grant 的两处没验 | 真实共享场景时：删了 grant 用同名重建、逐字核对 ChatGPT 拿到的回包（审查 §9） |
| 经 ccnm 的真实 SSH 场景 | 接入远端项目时：真实 SSH、会话寿命、节点归属；RFC-0005 远端用户级 Skill 附件先查有没有后续证据，没有再补。本机管道测试不替代它 |
| 远端参数能力协商 | 真实远端需求出现时：`bridge::session::Offered` 现在只比工具名和顶层参数名，可以改成也认 `additionalProperties`，多带的参数在本地就拦住；租约可见性/续租同理。不把远端后台命令升级成永久 Job |
| 项目命令确实要某个密钥 | 有真实需要时再设计显式放行（按项目、按名字）；现在名字像密钥的一律扣下，要密钥的测试让用户自己跑 |
| 单项目监听器的内部代码 | 删之前先确认 GPT Actions 等没有别的消费者（RFC-0004） |
| 代码签名、公证/平台信任体验 | 面向普通用户分发、取得账号与证书后；构建来源证明已完成，不和代码签名混成"未做" |
| D13 新一代协议、动态工具变更通知 | 有明确目标客户端需要且已有兼容用例时；不因版本日期新就迁移 |
| 普通 grant 有效期、范围原地调整、服务端审批 | 浏览器专用短期凭据在本次直连需求中实现；普通 OAuth/Bearer grant 的扩展仍等真实共享/撤销场景需要时单独设计 |
| Harness 锁等待上限、断电耐久性、记录清理/配额可配置 | 出现长时阻塞、恢复或磁盘需求时；当前缺口见审查 §7，不能宣称已有严格上限/强耐久保证 |
| 文件版本号在 Linux 上分不清几毫秒内的等长改动 | 版本号是大小加修改时间，Linux 时间戳 4 毫秒一格（审查 §33）。真遇到漏检，或者要做脚本高频改文件的场景时，和 ccnm 一起把 inode 之类加进版本号（两边的 `version_of` 必须一字不差，不能只改 gld）。要不要做由用户定 |
| 语义导航、managed worktree、CI/PR 写操作 | 有真实用例和授权后分别接入（Agent 委派已排进上面的队列）；并行 worktree 要按"工作树文件"和"共享 Git 元数据"两级协调，不照搬 ccnm 把整个 common directory 串行的做法 |

**用户定了不做：**让调用方传环境变量（`exec_command` 不收 `env`，审查 §16；和"项目命令拿不到像密钥的变量"是两件事）、`migrate` 角色（§15）。
真实 PTY、另起单项目 MCP 主服务、第二套 Goal/Plan/Task、完整 IDE/部署平台也不纳入；
没有必要为了补齐旧路线 L0–L6 的所有格子而无限扩展产品。

## D10 往下走的原则

- **复用现有执行器。**服务模式就是 `exec_command` 加一个参数，不引入第二个执行器；gld 自己的 MCP 服务就绪，
  和被开发项目的业务服务就绪，是两个不同对象。
- **浏览器和本机 MCP 不继承项目沙箱。**不默认接管个人登录态、不公开 dev 端口；证据要留就存成项目文件并关联任务，
  不先造通用上传下载平台。修代码后旧证据失效；截图不能替代错误检查。
- **任务发现只认已声明的入口。**Makefile、pyproject、go.mod 等等真实项目要用时再解析；发现不执行、不自动装依赖。

## 旧路线如何归并

旧路线 L0–L6 的定义见 [hub 路线](docs/reviews/2026-09-19-hub-development-roadmap.md)。L0 的原工具修复收口；L1 的最小
grant / Runtime 所有权已落地但不代表多租户强隔离；L2 已有验收、运行记录和 `list_runs`；L3 的浏览器流程靠本机 MCP 转发加
隔离配法；L4 的 Skills/Notebook/MCP 已做，语义导航按需；L5 的 gld 自身发布流程已有证据，不等于能代管任意项目的发布；
L6 仍是经 ccnm 的可选委派，不是基础版本门禁。

每次接手先读本文件当前队列、审查里目标 D 项的最新一节、当前源码与 diff；认领一个可验收的小项，不按旧缺陷标题批量开工。
当前状态只改本文件；审查追加事实和证据，不再维护另一张独立待办表。版本号由实际交付和兼容性变化决定，
不先许诺一次做完"0.9 全生命周期"。

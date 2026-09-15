# RFC-0001 证据附录：Opus 结论复核与版本边界

核验日期：2026-09-15。原[服务采用提案](0001-shared-workspace-runtime.md)已被用户确认的[共享内核与 hub 远端接入方案](0002-shared-kernel-and-ccnm-hub.md)替代。下面保留已核查事实；旧方案的候选排序、服务准入和实施阶段不再作为当前计划。本附录不维护另一份进度。

## 1. 证据级别与本地基线

本轮读取用户提供的 Opus 总结，并与本地源码、锁文件、官方发布源码、文档和 CI 元数据对照。附件中“子代理已经实测”的内容仍是外部报告，未经本轮复现，不改写成自己的实验结论。

| 对象 | 本轮基线 | 说明 |
| --- | --- | --- |
| gld | `325644c`，检查开始时工作区干净 | Rust 2021、声明 MSRV 1.85；MCP/Actions 项目工具服务，无自有模型循环 |
| ccnm | `8205bc2`，检查开始时工作区干净 | Rust 2024、声明 MSRV 1.89；锁文件 rmcp 3.2.0；管理官方 Agent 并提供 Runtime 工具 |
| ccnm 进度 | `current_task=null`，P0–P12 已结束 | 当前状态与冻结契约优先于旧文档中的 experimental/待真机描述；新链仍须验证 |
| WebCodex 发布版 | `v0.4.1` → `f080c8f3ea70e37bd9f17fdd0e1b4c3a3aa330f8` | 第一候选，不代表本轮运行过 |
| WebCodex 对照 main | `ef21d278596b53ea9506e0a0df89eca09b1bd2c9` | 只用于辨认发布版没有的能力，不能混作同一个实验版本 |

本地依据：`/Users/bing/xdw/gld/Cargo.toml`、`/Users/bing/xdw/ccnm/Cargo.toml`、`/Users/bing/xdw/ccnm/Cargo.lock`、`/Users/bing/xdw/ccnm/docs/plan/status.json`、`/Users/bing/xdw/ccnm/docs/protocol/README.md`。

本轮未运行候选服务、未验证真实模型/故障恢复，也未执行附件中的工具链卸载建议。生产二进制状态不能从仓库 HEAD 推断。

## 2. Opus 本地论断的逐项核验

### 2.1 Git 超时未生效：代码确认

gld `run_git` 接收 `limit: Duration`，实际调用阻塞 `Command::output()`，随后 `let _ = limit`。5 秒/10 秒参数未成为执行期限。这是需要单独修复的行为缺陷，不是通过抽函数就能宣称解决。

依据：`/Users/bing/xdw/gld/crates/core/src/tools/git.rs:476-503`。

需补慢 Git 子进程的超时、错误结构和回收测试。本轮没有制造 Git 挂起复现。

### 2.2 进程树回收：缺少保证，不等于每次都会泄漏

gld 命令启动路径未建立独立 Unix 进程组；超时的 `kill_and_wait` 只对直接 child 调用 `start_kill` 和 `wait`；显式取消也向正 PID 发信号。因此不能保证终止子孙进程。某一次不创建子进程的 sleep 测试不能证明进程树安全。

依据：`/Users/bing/xdw/gld/crates/core/src/tools/exec.rs:250-279`、`/Users/bing/xdw/gld/crates/core/src/tools/session.rs:223-231`、`/Users/bing/xdw/gld/crates/core/src/tools/session.rs:550-559`。

需分别测 inline 超时、yield 后超时、显式取消、子孙持输出管道、Windows 对应行为。本轮没有执行进程泄漏实验。

### 2.3 session 删除：取决于结束路径

inline 正常结束路径取 snapshot 后立即移除 session；timeout 保留 30 秒；yield/background monitor 到 deadline 后再安排清理。不能概括成所有命令结束后 session 都立即删除。

依据：`/Users/bing/xdw/gld/crates/core/src/tools/exec.rs:309-324`、`/Users/bing/xdw/gld/crates/core/src/tools/exec.rs:347-374`。

待复现用例：快速输出超过预览预算，拿返回的 `output_ref` 立即续读；再比较 yield/timeout 路径。统一结果保留期限是可观察行为变化，不是纯重构。

### 2.4 patch hunk 定位：源码可推导错误片段风险

gld unified diff 解析丢弃 `@@` 行位置；应用时每个 hunk 从 `search_at=0` 找第一个匹配。原文件 `old\nmiddle\nold\n`，补丁声明修改第三行的 old，算法仍可能选择第一行。

依据：`/Users/bing/xdw/gld/crates/core/src/tools/patch.rs:198-204`、`/Users/bing/xdw/gld/crates/core/src/tools/patch.rs:335-407`。

这是源码推导，未运行该反例。修复需先明确位置失效后的搜索规则与重复匹配处理；引入更宽松的上游 parser 不能自动视为正确。

### 2.5 read 和 path 不是“ccnm 超集”

| 维度 | gld | ccnm | 迁移后果 |
| --- | --- | --- | --- |
| 非 UTF-8 / BOM | 拒绝非法编码，BOM 原样保留 | lossy 替换并提示，首行 BOM 剥离并标注 | 用户看到的内容和错误会变化 |
| 总行数 | 扫至文件尾计算 | 未读到 EOF 时可能未知 | 不能伪造相同分页元数据 |
| 超长单行 | 固定 64 KiB 块与有界截取 | `read_until` 先读整行，再检查扫描预算 | ccnm 实现不能当作更优内存上界直接替换 |
| 特殊文件 | 读取前先拒目录，FIFO 存在阻塞风险 | 读取前拒非普通文件 | 强化策略应显式记录，不能放过 FIFO |
| 读路径 | 可接受最终仍在 root 内的绝对/含父路径；有外部读开关 | 一律拒绝绝对路径和父路径 | containment 原语可复用，输入契约不可盲目统一 |
| 额外写保护 | `.git`、`.github` 等产品规则 | 专门保护 `.git` | 迁移不能意外放开 gld 原有保护 |

依据：

- `/Users/bing/xdw/gld/crates/core/src/tools/file.rs:26-61`、`:631-734`。
- `/Users/bing/xdw/ccnm/crates/ccnm-core/src/mcp/read.rs:187-205`、`:218-246`、`:292-332`、`:356-367`。
- `/Users/bing/xdw/gld/crates/core/src/tools/workspace.rs:226-264`、`:439-457`。
- `/Users/bing/xdw/ccnm/crates/ccnm-core/src/mcp/path.rs:139-145`、`:225-245`。

### 2.6 提交恢复：ccnm 更完整，但不等于多文件瞬时原子性

gld 有 staging、内存备份和 best-effort rollback，不是完全没有事务处理；该路径缺少 `sync_all` 与持久 journal，备份恢复错误也可能被忽略。

ccnm 有临时文件同步、首次 rename 前的 journal、回滚失败报告和遗留 journal 阻断。但低层调用的 journal 参数可以为空；中断可检测和保留证据不等于所有崩溃都自动回滚，更不等于读者永远看不到部分文件已替换。

依据：`/Users/bing/xdw/gld/crates/core/src/tools/patch.rs:426-520`；`/Users/bing/xdw/ccnm/crates/ccnm-core/src/mcp/patch.rs:206-242`、`:1091-1141`、`:1275-1295`、`:1319-1344`。

### 2.7 ccnm 预算说明漂移：按运行实现和契约证据收敛

文档写 exec 头尾各 16 KiB，实际默认总预算 4 KiB、最大 16 KiB，再由 stdout/stderr 分摊；格式提示另计。文档写 patch 单文件内容 1 MiB/单次编辑 16 MiB，实际 1 MiB 限制整个请求 `content + old + new` 合计，16 MiB 限制单个既有被编辑文件。

依据：`/Users/bing/xdw/ccnm/docs/protocol/remote-workspace-mcp-v1.md:256-258`；`/Users/bing/xdw/ccnm/crates/ccnm-core/src/mcp/exec.rs:69-77`、`:381-385`、`:449-472`；`/Users/bing/xdw/ccnm/crates/ccnm-core/src/mcp/patch.rs:292-305`、`:474-492`。

该项应单独修正文档并跑协议校验，不能为了匹配错误文案扩大执行预算。本轮未修改 ccnm。

## 3. WebCodex 服务复用的证据和限制

### 发布版已经有的能力

固定 `v0.4.1` 包含 Server/Runner、MCP 与统一 REST tool 调用、原生文件/补丁/Git/命令/验证、Job 观察与停止、managed worktree，以及可选 ACP start/observe/cancel。Server/Runner 不依赖 Desktop UI，也不要求普通工具操作调用模型。

来源：[发布记录](https://github.com/yyjeqhc/webcodex/releases/tag/v0.4.1)、[部署](https://github.com/yyjeqhc/webcodex/blob/v0.4.1/docs/DEPLOYMENT.md)、[路由](https://github.com/yyjeqhc/webcodex/blob/v0.4.1/src/route_metadata/runtime.rs)、[dispatch](https://github.com/yyjeqhc/webcodex/blob/v0.4.1/src/tool_runtime/dispatch.rs)。

复用收益来自采用已有服务链，不是它已经透传了 Codex 全部工具。其原生 patch/进程/任务仍有自己的实现，必须用真实边界用例检验。

### 不得把 main 当 release

- `v0.4.1` 的 Job 恢复依赖原 Runner 仍存活等条件；审查 main 才新增普通终态 Job receipt。两种恢复能力分开验收。
- release 的部分专用 REST 路由在审查 main 已移除，而统一 `/api/tools/call` 保留。这是采用薄客户端和固定版本的直接理由，不是推断所有 API 已经稳定。
- ACP permission request 的批准后继续执行链仍缺失；release/main 都存在等候后返回 `Cancelled` 的分支。第一版不启用该能力。

来源：[固定版本 Job 契约](https://github.com/yyjeqhc/webcodex/blob/v0.4.1/docs/agent/job-reliability-and-concurrency.md)、[main receipt](https://github.com/yyjeqhc/webcodex/blob/ef21d278596b53ea9506e0a0df89eca09b1bd2c9/src/job_receipts.rs)、[ACP 分支](https://github.com/yyjeqhc/webcodex/blob/f080c8f3ea70e37bd9f17fdd0e1b4c3a3aa330f8/crates/webcodex-runner/src/webcodex_runner/coding_agent.rs#L1880-L1887)。

该 release 的 [CI](https://github.com/yyjeqhc/webcodex/actions/runs/34468015509)、[构建](https://github.com/yyjeqhc/webcodex/actions/runs/34469470918)、[Server image 发布](https://github.com/yyjeqhc/webcodex/actions/runs/34474026631) 元数据为 success；但[测试说明](https://github.com/yyjeqhc/webcodex/blob/v0.4.1/docs/TESTING.md)明确部分真实进程/时序测试不在普通 CI。不能把发布成功等同自己的部署与恢复已经验证。

## 4. 社区库论断复核

这些是必要时的组件候选，不是本轮新增依赖清单。没有编译验证下列 MSRV，也未重跑附件中的 Codex resolver 实验。

| 对象 | 可确认 | 不能推导成 |
| --- | --- | --- |
| cap-std | capability-relative 文件访问可减少路径逃逸面；目录内 symlink 可以合法；ambient root 必须由宿主授权 | 全部 TOCTOU、并发内容修改、多文件事务已经解决；任意 Rust/子进程都被沙箱化 |
| ignore + grep-* | 成熟搜索积木，可配置 ignore、编码和匹配行为 | 默认配置天然兼容两产品；遍历器给出的路径就是授权凭证 |
| process-wrap | 有 Unix process group/session、Windows Job Object 等机制，由调用者组合 | 一个依赖自动提供相同跨平台行为、超时/排空/环境控制与所有后代清理保证 |
| rmcp | 官方 SDK；3.3.0 的 child-process transport 可选依赖 process-wrap 10 | 文件内核必须依赖 MCP；现在应升级 ccnm 已锁定的 3.2.0 |
| Landlock + seccompiler | Linux 权限/系统调用过滤机制 | 已具备完整、跨平台、默认安全的 sandbox；旧 FD、网络、资源限制可不设计 |
| VT Code | 发布包确有 lib target，不能误说只能装 CLI | 只凭 MIT 标签就解决许可、MSRV 与大依赖闭包问题 |
| Codex 内部 crate | 可参考设计；直接链接须处理内部依赖和消费方根 patch | 固定会解析 823 个包；复制上游 lock 即可解决消费方依赖配置 |

主来源：[cap-std](https://github.com/bytecodealliance/cap-std/blob/main/README.md)、[ignore](https://github.com/BurntSushi/ripgrep/blob/master/crates/ignore/src/walk.rs)、[grep-searcher](https://github.com/BurntSushi/ripgrep/blob/master/crates/searcher/src/searcher/mod.rs)、[process-wrap](https://github.com/watchexec/process-wrap)、[rmcp 3.3.0 依赖](https://crates.io/api/v1/crates/rmcp/3.3.0/dependencies)、[Linux seccomp 边界](https://docs.kernel.org/userspace-api/seccomp_filter.html)、[Landlock](https://docs.kernel.org/userspace-api/landlock.html)。

若后来采用 capability-relative 文件访问，搜索应消费经过授权打开的文件句柄，而不是又走不受该边界控制的绝对路径读取。进程组/Job Object 也须与具体取消、等待、输出排空策略配套。

### 工具链不是一个版本数字解决的问题

本轮发布元数据快照：`process-wrap 10.0.0` 声明 MSRV 1.87；`rmcp 3.3.0`、`ignore 0.4.33` 声明 1.88；`vtcode 0.162.3` 声明 1.93。因此“统一升到 1.89 就能引入上述全部组件”也不成立。

`File::try_lock` 于 1.89 稳定；直接编译使用该 API 的库才要求相应编译器。独立服务的 MSRV 不沿网络/IPC 传播给客户端。Edition 是每 crate 的选项；2021/2024 可互操作，2024 从 Rust 1.85 起支持。

来源：[File::try_lock](https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock)、[Edition 互操作](https://doc.rust-lang.org/edition-guide/editions/index.html)、[2024](https://doc.rust-lang.org/edition-guide/rust-2024/index.html)、[VT Code 发布元数据](https://crates.io/api/v1/crates/vtcode/0.162.3)。

### 依赖与来源结论的强度

Cargo 只采用消费方根 workspace 的 `[patch]`；依赖自身的 patch 不会自动传递。上游 Cargo.lock 不能替代这些 manifest 规则。附件的“823 crates”和编译成功条件未经本轮重跑，不作为选型阈值。[Cargo 官方规则](https://doc.rust-lang.org/cargo/reference/overriding-dependencies.html#the-patch-section)

VT Code 的发布 metadata 声明双许可，但核验包仅附 Apache LICENSE 的材料差异需确认；不能直接写成“MIT 即可复制”。Claw 的来源/停更日期在本轮不作进一步断言；现有 README 对生产用途的自我限制已经足够让它退出短名单，不需要把未复核的泄露来源推断当成事实。

## 5. 文档交付不等于实现验收

本文与 RFC 只记录本轮可复核的判断。上游测试文件存在、CI 成功、本地源码可推导风险，分别是不同级别证据。服务准入、真实进程清理、故障注入、目标平台运行、模型调用和生产部署均留给后续阶段，不以文档提交代替。

本轮文档检查：

- 临时 `GLD_HOME`、已安装 stable 工具链、禁用自动安装，运行 `cargo test --locked --offline -p gld --test docs_commands_exist`：2 passed / 0 failed。
- 检查本轮文档的本地链接、证据路径与行号范围、代码围栏，以及 P0–P5 / G01–G12 编号；README 原有 GitHub Releases 相对链接按网页链接处理，不误作本地文件。
- 独立只读评审后修正两项：按操作区分幂等去重/禁止重放；要求实验前冻结适用矩阵与量化阈值。复核未发现新的文档一致性问题。
- 未运行 Rust 全量门禁、候选服务或模型验证；本次变更仅文档，不据这两项测试推断运行时正确性。

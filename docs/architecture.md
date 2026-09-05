# 架构

## 三层 crate

```text
crates/
├── core/    gld-core     运行时内核 + 应用服务层，不知道命令行和守护进程的存在
├── daemon/  gld-daemon   协议、IPC、守护进程主循环、生命周期
└── cli/     gld          唯一的二进制：clap 解析、后端选择、终端渲染
```

依赖方向只有一个：`cli → daemon → core`。core 里没有任何 `clap`、socket、进程管理的代码；
daemon 里没有任何终端输出；cli 里没有任何业务规则。

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

两条路径在 `dispatch` 汇合，所以命令行直连和经守护进程转发执行的是同一段代码，
行为不可能不一致。

## core：内核与服务层

| 模块 | 职责 |
| --- | --- |
| `tools/` | 统一工具内核：文件、Patch、命令、Git、History、Planning、Skill。两个唯一入口：`tools::call_tool` 执行工具，`tools::build_tool_context` 构建上下文——MCP 监听器和 `gld tool call` 都走它，所以命令行里试出来的行为就是 AI 看到的行为 |
| `mcp/`、`actions/` | 两条 HTTP transport（axum），都调用 `call_tool`，不各自实现工具 |
| `auth/` | Bearer、OAuth Authorization Code + PKCE + DCR + Refresh Token |
| `runtime/` | 进程内 MCP / Actions 监听器的启停、端口检测与释放等待 |
| `tunnel/`、`global_gateway.rs` | frpc / cloudflared 子进程监督，共享公网入口 |
| `planning/`、`harness/` | Goal / Plan / Execution Ledger；Durable Task |
| `data/`、`settings/`、`workspace/`、`secret/` | `profiles.json` 的模型与读写 |
| `platform/` | 端口占用查询、进程存活 / 终止、可执行文件查找，按 OS 分实现 |
| `home.rs` | 数据目录解析（`GLD_HOME` / `~/.gld`），全项目唯一的路径来源 |
| `async_rt.rs` | tokio 运行时垫片：工具内核是同步 API，内部需要 `spawn` / `block_on` |
| **`app/`** | **应用服务层**：`App` 持有 `DataStore`、`RuntimeSupervisor` 和命令行工具调用的 `ToolContext` 缓存，每个子模块是一组用例；`app/doctor.rs` 的纯配置检查不依赖磁盘，可直接单测 |

`app` 是 core 对外的唯一门面。桌面版里这一层是 Tauri command，
这里改成普通的 `impl App` 方法，方便任何调用方（命令行、守护进程、测试）直接用。

`app::workspace_fields` 是一张“可设置字段”表：命令行帮助、`gld ws fields` 输出和实际写入
都从同一张表来，加字段只改一处。两个约定：

- 字段名不写前缀时补 `mcp.`，所以新增 MCP 侧字段一律叫 `mcp.<名字>`；
  Actions 侧字段必须和 MCP 侧同名（只换前缀），有测试守着这条。
- 取值要查工作区之外的数据（比如 `frp-profile` 得确认那个 id 存在）时，
  用 `field!` 宏的三参数写法拿 `FieldContext`，别在 `apply` 里直接读全局状态——
  那样就没法只对着一个 `WorkspaceProfile` 做单测了。

`App::set_workspace_fields` 存完会重启受影响且正在运行的服务
（按 `service_config_changed` 逐侧比 JSON 快照）。加了新字段就自动被覆盖，
不需要额外登记"这个字段要不要重启"。

## daemon：协议与进程

| 模块 | 职责 |
| --- | --- |
| `protocol.rs` | `Request`（`op` 标签的枚举）、`Response`、`RpcError`、`PROTOCOL_VERSION` |
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

- `App` 内部两把 `std::sync::Mutex`（数据、运行时），临界区只做内存操作和一次同步落盘，
  从不在持锁期间 `await`。
- 跨 `await` 的串行化用 `tokio::sync::Mutex`：`App::restart_gate`（MCP / Actions 的 stop→start）
  和 `tunnel::supervisor()`（frpc / cloudflared 的进程操作）。
- 守护进程每个连接一个 tokio 任务，`dispatch` 再包一层 `spawn`：业务代码 panic
  只影响那条连接，并转成 `RpcError::Internal` 回给客户端。
- 工具内核是同步函数，被 `spawn_blocking` 包着跑；它内部通过 `async_rt::block_on`
  驱动子进程 I/O。这是从桌面版继承的约束：`block_on` 不能在 tokio worker 线程里调用。

## 加一个新命令要改哪里

以“给工作区加一个只读的 `gld ws stats`”为例：

1. `core/src/app/<模块>.rs`：`impl App { pub fn workspace_stats(&self, id) -> AppResult<StatsDto> }`，
   DTO 派生 `Serialize + Deserialize`。
2. `daemon/src/protocol.rs`：`Request::WorkspaceStats { target }`；
   若它必须由守护进程执行，加进 `needs_daemon()`；耗时长加进 `is_slow()`。
3. `daemon/src/dispatch.rs`：新增一个 match 分支（漏了会编译失败）。
4. `cli/src/cli.rs`：加子命令与帮助文本；`cli/src/commands/workspace.rs`：调用并渲染。
5. `scripts/gen-cli-docs.sh` 的命令表加一行，重新生成 `docs/cli.md`。
6. 测试：core 里给 `App` 方法写单元测试；命令行行为进 `crates/cli/tests/` 下按主题分的文件
   （生命周期进 `daemon_lifecycle.rs`、工作区入口进 `start_and_upgrade.rs`……），
   新命令名同时会被 `docs_commands_exist.rs` 和 `messages_name_real_commands.rs` 盯上。

不要做的事：在 cli 里直接 `use gld_core::app::App` 绕过 `dispatch`——那会让直连和转发两条路径分叉。

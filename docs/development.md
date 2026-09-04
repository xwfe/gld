# 开发

## 构建

```bash
cargo build                    # target/debug/gld
cargo build --release          # target/release/gld，已开 LTO + strip
```

Rust 1.85+。没有 Node、没有 Tauri 依赖。

## 测试

```bash
GLD_HOME=$(mktemp -d) cargo test --workspace
```

`GLD_HOME` 指向临时目录是为了不碰你真实的 `~/.gld`：core 里有少数测试会写数据文件，
`crates/cli/tests/daemon_lifecycle.rs` 会真的拉起一个守护进程、起 MCP、走 TCP 请求再停掉。
不设也能跑（core 的写文件测试自己会切到临时目录），但集成测试会用你的真实数据目录。

| 测试 | 位置 | 覆盖什么 |
| --- | --- | --- |
| 单元测试 | 各 crate `src/**` 的 `#[cfg(test)]` | 解析、策略、协议编解码、路径回退、字段表 |
| 工具契约 | `crates/core/tests/call_tool_contract.rs` | 每个 profile 暴露的工具集与返回结构 |
| 安全契约 | `crates/core/tests/call_tool_security.rs` | 路径穿越、命令白名单、`.git` 保护、危险操作确认 |
| Harness / History | `crates/core/tests/harness_*.rs`、`history_session.rs` | Durable Task、历史档案的幂等追加与分页读取 |
| 端到端 | `crates/cli/tests/daemon_lifecycle.rs` | 真实二进制：add → 自动拉起守护进程 → start → TCP 请求 → stop → daemon stop |
| 排障命令 | `crates/cli/tests/tool_and_doctor.rs` | `gld tool` 三种参数写法与失败退出码、`gld doctor` 的通过 / 失败判定 |

只跑某一块：

```bash
cargo test -p gld-core tools::
cargo test -p gld-daemon
cargo test -p gld --test daemon_lifecycle
```

## 提交前

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
GLD_HOME=$(mktemp -d) cargo test --workspace
scripts/gen-cli-docs.sh          # 改了帮助文本就重新生成 docs/cli.md
```

### 在 Mac / Linux 上验 Windows 那条编译检查

CI 有一个 `Windows 编译检查` job，跑的是 `cargo check --workspace --all-targets`
且 `RUSTFLAGS: -D warnings`。**它最容易被 dead_code 卡住**：某个函数或常量只在
`#[cfg(not(windows))]` 分支里用到，Windows 上就成了未使用项，警告即错误。
这类问题在 Mac 上怎么跑都发现不了，只能等推上去才知道。

本机就能验，用 GNU 目标（`ring` 等带 C 代码的依赖要 mingw 来编）：

```bash
brew install mingw-w64
rustup target add x86_64-pc-windows-gnu
export CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc
export AR_x86_64_pc_windows_gnu=x86_64-w64-mingw32-ar
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
RUSTFLAGS="-D warnings" cargo check --workspace --all-targets --target x86_64-pc-windows-gnu
```

CI 用的是 msvc 目标，这里用 gnu——两者的 `cfg(windows)` 代码路径一样，
dead_code 这类问题能等价地检出来。msvc 目标在 Mac 上编不了：`ring` 的 build.rs
需要一个面向 MSVC 的 C 编译器，会停在 `failed to run custom build command for ring`。

## 手动冒烟

```bash
export GLD_HOME=$(mktemp -d)                    # 隔离
gld ws add /path/to/some/project --name demo
gld ws set auth=noauth
gld start
curl --noproxy '*' http://127.0.0.1:28766/mcp   # {"name":"coding-tools-mcp",...}
curl --noproxy '*' -X POST http://127.0.0.1:28766/mcp -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"git_status","arguments":{}}}'
gld stop && gld daemon stop
```

`--noproxy '*'` 是因为很多开发环境设了 `HTTP_PROXY`，不加的话 curl 会把 127.0.0.1 也发给代理。

## 目录

```text
crates/core/src/app/        应用服务层（加用例先改这里）
crates/core/src/tools/      工具内核（改工具行为改这里，同时更新契约测试）
crates/daemon/src/          协议 / IPC / 守护进程
crates/cli/src/cli.rs       全部命令与帮助文本
crates/cli/src/commands/    子命令实现
crates/core/tests/fixtures/ 契约测试用的示例项目
docs/                       用户文档；cli.md 由脚本生成
scripts/gen-cli-docs.sh     从 --help 生成 docs/cli.md
scripts/package.sh          构建 + 打包发布件（CI 也调它）
```

加命令的步骤见 [architecture.md](architecture.md#加一个新命令要改哪里)。

## 版本

三个 crate 共用 workspace 版本（根 `Cargo.toml` 的 `[workspace.package].version`）。
守护进程会把版本和协议版本报给命令行，不一致时命令行拒绝转发；
改了 `Request` / `Response` 的形状记得把 `PROTOCOL_VERSION` 加一。

## 打包与发布

打包只有一份实现：`scripts/package.sh`。本地和 CI 跑的是同一个脚本，
所以本地打出来的包和 Release 页上的包，文件名、目录结构、附带文件完全一致。

```bash
scripts/package.sh                          # 当前平台
scripts/package.sh x86_64-apple-darwin      # 指定目标（先 rustup target add）
scripts/package.sh --checksums              # 给 dist/ 里已有的包生成 SHA256SUMS
```

产物是 `dist/gld-<版本>-<目标三元组>.tar.gz`（Windows 目标为 `.zip`），
里面是二进制 + README.md。版本号默认取 `Cargo.toml` 的 `[workspace.package].version`
加个 `v` 前缀，CI 用 `VERSION` 环境变量传 tag 名覆盖。

正式发版靠打 tag：

```bash
git tag v0.3.0 && git push origin v0.3.0
```

`.github/workflows/release.yml` 会跑一遍全量测试（tag 不触发 ci.yml，
所以这里补一道，没测过的不往外发），然后并行构建五个目标、生成 `SHA256SUMS`、
用 `gh release create --generate-notes` 建 Release。

**第一次别拿真 tag 试。** 先用 `workflow_dispatch` 手动跑一次：它会走完构建打包
和上传构件，但 `publish` job 有 `if: startsWith(github.ref, 'refs/tags/v')`，
不会建 Release。跑通了再打 tag。

`x86_64-unknown-linux-musl` 目前标着 `optional`，编不过不影响其他目标发版——
它在 Ubuntu runner 上要靠 `musl-tools` 提供的 musl-gcc 接管 `cc`，这一环还没实跑验证过。
空跑那次通了就把 `optional: true` 去掉。

用户升级后执行 `gld daemon restart` 让守护进程换到新二进制。

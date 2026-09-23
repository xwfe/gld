# 开发

## 构建

```bash
cargo build                    # target/debug/gld
cargo build --release          # target/release/gld，已开 LTO + strip
```

Rust 1.89+（根 `Cargo.toml` 的 `rust-version`，和 ccnm、toexec 保持一致）。没有 Node、没有 Tauri 依赖。

平时用 stable 开发，CI 的 `MSRV 编译检查` 另用 1.89 在 Linux、macOS、Windows 上各编一遍。推之前想先验本机这个平台：

```bash
rustup toolchain install 1.89 --profile minimal
cargo +1.89 check --workspace --all-targets --locked
```

报 `error[E0658]: use of unstable library feature ...` 看着像用了 nightly 特性，实际是那个 std API 在 1.89 之后才稳定，stable 上当然编得过，换个老 API 写。报 `rustc 1.89.0 is not supported by the following package` 是某个依赖要更高版本——**别只在 gld 里调高 `rust-version`**，gld、ccnm、toexec 三个仓库一起升，提交说明写明是哪个依赖要求的（toexec 的 `docs/plan/implementation-plan-v2.md` 第 11 节）。

## 测试

```bash
GLD_HOME=$(mktemp -d) cargo test --workspace --all-targets --locked
```

`GLD_HOME` 指向临时目录是为了不碰你真实的 `~/.config/gld`：core 里有少数测试会写数据文件，
`crates/cli/tests/daemon_lifecycle.rs` 会真的拉起一个守护进程、起 MCP、走 TCP 请求再停掉。
不设也能跑（core 的写文件测试自己会切到临时目录），但集成测试会用你的真实数据目录。

| 测试 | 位置 | 覆盖什么 |
| --- | --- | --- |
| 单元测试 | 各 crate `src/**` 的 `#[cfg(test)]` | 解析、策略、协议编解码、路径回退、字段表 |
| 工具契约 | `crates/core/tests/call_tool_contract.rs` | 每个 profile 暴露的工具集与返回结构 |
| 安全契约 | `crates/core/tests/call_tool_security.rs` | 路径穿越、命令白名单、`.git` 保护、危险操作确认 |
| Harness / History | `crates/core/tests/harness_*.rs`、`history_session.rs` | Durable Task、历史档案的幂等追加与分页读取 |
| 端到端 | `crates/cli/tests/daemon_lifecycle.rs` | 真实二进制：add → 自动拉起守护进程 → start → TCP 请求 → stop → daemon stop |
| 服务与项目 | `crates/cli/tests/hub_one_connection.rs`、`service_lifecycle.rs` | 一条连接按 `workspace` 分到各项目且不串、凭据只有一套、重复 start 不重启、改配置即生效、并发 start |
| 排障命令 | `crates/cli/tests/tool_and_doctor.rs` | `gld tool` 三种参数写法与失败退出码、`gld doctor` 的通过 / 失败判定 |
| 一步到位的入口 | `crates/cli/tests/start_and_upgrade.rs` | `gld start <目录>` 的自动登记与归属判断（非项目目录不登记）、`gld ls` 的服务 / 项目视图、`gld upgrade` 换目录换地址 |
| 公网入口 | `crates/cli/tests/share_one_command.rs`、`named_tunnel_start.rs` | 服务的 `--tunnel` 各种走法（假 cloudflared / 假 frpc）、沿用已配好的入口、缺隧道程序时的报错、固定域名的公网探测 |
| 收摊 | `crates/cli/tests/destroy_and_stop_all.rs` | `gld stop` 不删东西、`gld rm` 删干净且必须确认 |
| 文档与提示 | `docs_commands_exist.rs`、`messages_name_real_commands.rs`、`doctor_fixes_are_real_commands.rs` | 校验提取出的 CLI 命令；不等于所有参数、执行效果、链接或客户端 schema 已验证 |

只跑某一块：

```bash
cargo test -p gld-core tools::
cargo test -p gld-daemon
cargo test -p gld --test daemon_lifecycle
```

## 文档与完成声明的契约

README 只保留定位、安装、使用、关键边界和导航；产品行为写在 `docs/`，设计取舍与历史
证据写在 `docs/rfc/`、`docs/reviews/`。本次对账入口是
[2026-09-23 审查](reviews/2026-09-23-lifecycle-and-docs-audit.md)，它是有日期的快照，不是另一套任务数据库。

| 改了什么 | 必须一起核对 |
| --- | --- |
| 工具或参数 | registry / 运行时 / 契约测试 / 当前用户文档，以及真实客户端发现的 schema |
| 权限、身份或锁 | security / concepts；区分文件工具、子进程、本机 MCP、远端 ccnm 的边界 |
| CLI | `cli.rs` 帮助、生成的 `cli.md`、README 示例；不能手改生成文档掩盖源帮助错误 |
| 阶段完成 | 当前状态入口指向验收证据；旧 RFC 保留当时结论，注明后续决定，不能仍作为当前待办 |
| 验证 | 记录日期、源码提交、平台、命令、退出码、是否真实执行；源码审查、fixture、真实二进制、SSH、公网客户端分别标明 |

`docs_commands_exist.rs` 当前只扫描 README 和顶层 `docs/*.md`（不含生成的 `cli.md`），
**不递归检查 reviews / RFC，也不检查链接**。这些内容要另查，不能以这条测试通过代替。
`ccnm_background_lifecycle` 在没有 ccnm 二进制时会打印跳过后直接返回，测试框架仍可能显示
passed；需要核对实际依赖和 `--nocapture` 日志，不能用 `0 ignored` 证明全都执行。

`scripts/gen-cli-docs.sh` 当前仍有两项风险：直接覆盖输出文件，以及对字段表 / 密钥名表
命令使用 `|| true` 吞错。它还会 unset `GLD_HOME`，只设置该变量不能保证生成过程隔离。
在隔离 HOME 中使用当前构建生成后检查非空章节和 diff；未来应改成失败即停、临时文件成功后
替换、显式隔离后端。源码修复前不能把脚本退出 0 当作完整文档生成成功。

## 提交前

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --locked -- -D warnings
GLD_HOME=$(mktemp -d) cargo test --workspace --all-targets --locked
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
gld start /path/to/some/project --port 28766    # 目录没登记过会自动登记
gld upgrade --auth noauth                       # 省掉 curl 的鉴权头
curl --noproxy '*' http://127.0.0.1:28766/mcp   # {"name":"gld-hub",...}
curl --noproxy '*' -X POST http://127.0.0.1:28766/mcp -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"git_status","arguments":{"workspace":"project"}}}'
gld stop && gld daemon stop
```

`workspace` 填 `gld ls` 项目表里的名字（默认是目录名）。

集成测试里的 `free_port()` 用"连一下"确认端口空闲，**别改回"bind 一下"**：macOS 上
测试进程里的监听 socket 会被并发 spawn 的 `gld` 继承，隔壁测试的端口就被一个无关的
守护进程占住了。原委写在 `crates/cli/tests/common/env.rs` 的注释里。

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

正式发版由操作者确认后打并推送 `v<workspace.package.version>` 的 tag。
不要复用文档里的旧版本号；先确认 tag、Cargo.toml、实际二进制版本和构件摘要一致。
当前 workflow 尚没有替操作者完成全部一致性与供应链验收，文档要求不能写成已经实现的门禁。

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

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

`docs_commands_exist.rs` 只扫描 README 和顶层 `docs/*.md` 里的命令（不含生成的 `cli.md`，
也不进 reviews / RFC：那里记的是当时的命令）。相对链接和 `#锚点` 由
`docs_links_resolve.rs` 查，范围是 README 加 `docs/` 下全部 Markdown；改标题或挪文件时
它会点名哪一处断了。锚点按 GitHub 的规则算。
`ccnm_background_lifecycle` 在没有 ccnm 二进制时会打印跳过后直接返回，测试框架仍可能显示
passed；需要核对实际依赖和 `--nocapture` 日志，不能用 `0 ignored` 证明全都执行。

`scripts/gen-cli-docs.sh` 在一次性的 `HOME` 里跑（不读你真实的 `~/.config/gld`），先写
临时文件；任何一条 `gld` 失败、或帮助段数不对、字段表 / 密钥名表像是空的，就退出 1 并
说清是哪一条，原来的 `docs/cli.md` 一个字不动。这些行为由 `docs_generation.rs` 用假的
`gld` 钉住。

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
加个 `v` 前缀；CI 打 tag 时用 `VERSION` 传 tag 名，必须和它一致。

正式发版由操作者确认后打并推送 `v<workspace.package.version>` 的 tag。发版前：

1. 写好发布说明 `docs/releases/v<版本>.md`，和版本号改动一起进 main。有这个文件 Release 就用它；
   没有才退回 `--generate-notes`——这个仓库直接往 main 提交、不走 PR，自动生成的只有一行
   Full Changelog。说明里的链接写成 `https://github.com/xwfe/gld/blob/main/…` 的绝对地址：
   它显示在 Release 页上，相对链接会指到错的地方。
   **说明里必须有"从上一版升上来"一节，写明 ChatGPT 连接器要不要动**，用户就靠它决定升级后做什么：
   这一版改没改工具表（工具的名字、参数、说明、标注，定义在 `tools/registry.rs`、`hub/`、
   `bridge/tools.rs`、`machine_mcp/relay.rs`）——改了写"要到 chatgpt.com/plugins 点一次 Refresh"，
   没改写"连接器什么都不用做"；守护进程协议号变没变；有没有会让连接器要重新授权或删了重建的改动
   （签名密钥、公网地址、认证方式、数据目录布局），分档见
   [装好的连接器什么时候要动](connect-clients.md#装好的连接器什么时候要动)。拿不准就在本机升级一次，
   按[安装 · 换完核对](install.md#换完核对)比工具表指纹，以它为准。
2. 本机打一次包：`scripts/package.sh`，再按安装文档的做法校验、解压、`--version`、起一次服务。
3. 在 Actions 里手动跑一次 Release（workflow_dispatch）：走完测试、五个目标的构建打包、构建来源证明、
   上传构件，但不建 Release（`publish` 只在 tag 上跑，空跑时它显示 skipped 是对的）。全绿再打 tag。

发版后（推 tag 到 Release 出来 8–10 分钟），从 Release 页下载全部包和 `SHA256SUMS`，
`shasum -a 256 -c SHA256SUMS` 要全部 OK，每个包再 `gh attestation verify <包> --repo xwfe/gld
--signer-workflow xwfe/gld/.github/workflows/release.yml` 验来源证明（要能用 gh 访问 GitHub API），
再把能跑的包解压跑 `--version`。Apple Silicon 上
`arch -x86_64` 能跑 Intel 版；musl 版是静态链接的，随便一个 Linux 容器都能跑。这一步补的是
CI 核对不到的地方：见下一段，交叉编译的两个目标 CI 跑不起来。

`scripts/package.sh` 会核对版本：CI 传进来的 tag 必须等于 `v` 加 `Cargo.toml` 的版本，否则
直接失败；目标就是本机时，还会跑一次打出来的二进制，`--version` 必须报同一个版本。按 runner
的架构，CI 里 `aarch64-apple-darwin`、`x86_64-unknown-linux-gnu`、`x86_64-pc-windows-msvc` 会跑到
这一步，交叉编译的 `x86_64-apple-darwin`、`x86_64-unknown-linux-musl` 跑不到。

**构建来源证明**：build job 打完包之后用 `actions/attest@v4` 给每个包签一份 SLSA 构建来源证明
（Sigstore 签名，记下仓库、提交和流水线），手动空跑也签，所以发版前就能看到这一步是不是绿的。
`SHA256SUMS` 只证明下载到的和 Release 页上的是同一份，说明不了它从哪来；来源证明补的是这一半。
同一个提交本机和 CI 打出来的包哈希不一样（构建不是逐字节可复现的），所以本机打的包没有这份证明。
2026-09-24 加上，v0.7.0 及以前的包没有。同日手动空跑（run 35962328940，提交 `bc8e13a`）第一次签出来：
5 个包按上面那条命令验都退出 0；改过一个字节的包、`--signer-workflow` 换成别的流水线，都退出 1。

**代码签名还没做**：macOS 的 Developer ID 签名和公证、Windows 的 Authenticode 都要付费证书。没有它们，
Gatekeeper / SmartScreen 会拦第一次运行，处理办法见[安装](install.md#下载现成的不需要装-rust)。

`.github/workflows/release.yml` 会跑一遍全量测试（tag 不触发 ci.yml，
所以这里补一道，没测过的不往外发），然后并行构建五个目标、生成 `SHA256SUMS`、建 Release。
五个目标都必须编过：`x86_64-unknown-linux-musl` 以前标着 `optional`，v0.4.0 发版时真跑通了，
已经去掉。

用户升级后执行 `gld daemon restart` 让守护进程换到新二进制。

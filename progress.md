# 进度

> 本轮：验收、失败传播、基线恢复、客户端能力核对（2026-09-23）。
> 上一轮“固定隧道启动检查”的进度在 git 历史里。结论与剩余项见
> [审查 §7](docs/reviews/2026-09-23-lifecycle-and-docs-audit.md#7-处理进展)。

- 先把上一轮审查的文档改动单独提交（5cc3389），再动代码。
- D03：新增 `tools/outcome.rs` 统一“调用成没成 / 命令结果如何”；三本账共用；read_output 回终态字段。
- D01：证据写进任务事件（exec_command 结束时、read_output 等第一次看到后台命令结束时）；
  `finish` 核对证据；`transition` 不能直达 completed；Paused 拒写。
- D02：扫描代码挪到 `harness/scan.rs`，只跳目录、记录读不到的文件；新增 `refresh_baseline`；
  每次记账存逐文件清单（`expected/<task>.json`）。
- D04：子 agent 追查所有 tools/list 路径，服务端无过滤、无 schema 改写；缺失来自客户端缓存。
  补 `server_info.connection`、`gld tool list --served`、hub 与注册表逐项等价的测试。
- 验证：新增 19 条测试；隔离全量 788 passed / 0 failed；fmt、clippy 通过；真实二进制隔离复跑反例全部翻转。
- 未碰真实服务和 `~/.config/gld`；生成 `docs/cli.md` 时把 HOME 指到临时目录。

## 第三轮

- 用户决定 `gld tool call` 退出码不改：写进帮助，`a_failing_command_is_not_a_failed_tool_call` 钉住。
- D14：文件清单改为 `--name-status -z` / `--numstat -z`；真实仓库测试覆盖增删改名、二进制、带空格路径、截断。
- D06：生成脚本隔离 HOME、失败即停、临时文件替换、查空表；新增生成失败路径测试和全文档链接检查（23 个文件、204 个链接）。
- D05：gld 侧过期提示改为"已执行、输出取不回、先核对"；toexec-mcp 0.2.1 本地提交 `bfa809d`，
  path 依赖联调通过、换回 0.2.0 新测试失败；tag 未推送，等批准。
- 中途磁盘满（ENOSPC，剩 159 MiB）：删了我在 scratchpad 里建的 3.4 GB 验证用 target，没动项目 `target/`。
- 用户批准发布：toexec 推送 main 与 tag `toexec-mcp-v0.2.1`，README 当前 tag 更新并推送（`2f22741`）；
  gld 改到新 tag，`cargo update -p toexec-mcp` 只动了这一个包和它自带的 toexec-text；取回 stash 里的契约测试。

## 第四轮

- 磁盘又只剩 435 MiB：删了项目 `target/debug/incremental`（9.9 GB，纯增量编译缓存，已安装的
  `~/.local/bin/gld` 是独立副本不受影响），腾出约 10 GiB。
- D11：先写 8 条故障注入测试走 `call_tool`，旧代码 7 条失败（8 个并发 start 开成 4–7 个任务；
  坏任务文件后 `apply_patch` 照改、能再开任务；坏行后证据丢失）。
- 修：`HarnessStore::lock`（flock）只在 Harness 对外入口拿；`atomic_write_json` 临时名带 pid+序号并
  sync；`append_line` 一次写完、先隔开半行；`read_log` 按字节读、坏行报行号；dispatch 写前检查读任务
  出错即拒写。
- 验证：全量 804 passed / 0 failed；新测试连跑 20 次全过；真实二进制经守护进程复跑全部符合。
- D13 核对：读 2026-07-28 变更与兼容表；实测 gld 对新版请求的回法；子 agent 读四个官方 SDK 的探测
  判定；装官方 TS SDK 2.0 到 scratchpad 实连隔离 gld，退回 initialize 后一切正常。只加测试和排障说明。

## 真机

- 本机升级：先 0.6.0 新构建（`ac0100e`），发现同版本号不提醒重启、D04 漏递增协议号；提到协议 4、
  版本 0.7.0 再升一次，换完未重启时命令行报版本不一致（退出码 4）。两次都备份了二进制和数据目录，
  凭据与 OAuth 注册数据逐项比对指纹一致。
- ChatGPT：未刷新前日志里自 9-18 起没有 `tools/list`；chatgpt.com/plugins 点 Refresh 后拿到新表。
- 真机验收项目 `~/xdw/gld-realtest`（验收完已删）：step 2、step 3 全部符合预期，核对都来自 gld 的
  任务事件、操作记录、Planning 台账和项目文件。
- 途中磁盘又满（119 MiB）：删了 scratchpad 里子 agent 克隆的 SDK、`cargo clean -p` gld 自己的三个
  crate（18.3 GiB 陈旧产物）。

## 收尾

- 删：`~/xdw/gld-realtest` 及它在数据目录的任务记录和日志（`gld rm` 不删这两样）、`target/review-20260923-*`、
  `dist/`（9-11 的 0.3.0 包）、`~/.local/opt` 里两份中间备份、scratchpad。
- 改：`gld rm` 帮助与 concepts.md 的说法、`docs/cli.md` 重新生成（只变这一行）。
- 全量测试通过后推送。
- 推送后 macOS CI 挂在 `a_failing_subcommand_fails_the_run_and_keeps_the_old_file`：先加诊断输出（`446cb3d`）
  拿到 stderr，定位到 bash 3.2 + UTF-8 把中文读进变量名、EXIT trap 把崩溃报成 0；修复 `9007348`，CI 全绿。

## 发布（D12）

- 流水线：`296cc29` 版本核对、手写说明、musl 必须编过；`42643a3` 0.7.0 发布说明。本机打包和回滚演练
  （0.7.0 → 0.6.0 → 0.7.0，隔离数据目录）通过。
- gh 在我的进程和 app 内终端里读不到 keychain 里的 token（`gh auth token` 为空），空跑由用户触发；
  状态和构件走公开 API 看，tag 走 git SSH 推。
- 空跑 run 35863249989 全绿 → 推 `v0.7.0` → run 35872551750 全绿，Release 有 5 个包 + `SHA256SUMS`。
- 下载包：校验和 5 个全 OK；aarch64、x86_64（Rosetta）macOS 版和 musl 版（Oracle Linux 9 容器）实跑报
  0.7.0；glibc、Windows 版没实跑。下载的包在 scratchpad，验完删掉。

## D08（2026-09-24）

- 本机服务是 compact、7 个项目共用一把 OAuth 凭据挂公网，没用 `compat-readonly-all`，退役它不影响现有连接器。
- 新测试先在旧实现上跑：dangerous 模式那条失败（`status=granted`）；stash 恢复后全过。
- 拆成三个提交：`d7f71fc`（request_permissions）、`cacde65`（退役兼容档）、`e9fe757`（confirm 说明）。
  第一个提交里的 `docs/cli.md` 只改权限模式那一行（最后一列，不影响对齐），第二个提交带完整重新生成的版本。
- 验证：全量 808 passed / 0 failed；fmt、clippy 通过；真实二进制在隔离数据目录核对迁移、拒收、落盘。
- `request_permissions` 经 CLI、hub 调不到（不在对客户端公开的表里），只有 GPT Actions 按名字放行；
  真实二进制核对不了它，覆盖靠 dispatch 层的契约测试。

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

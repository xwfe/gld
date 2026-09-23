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

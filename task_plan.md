# 验收、失败传播、基线恢复、客户端能力核对（2026-09-23 起）

> 入口与问题编号见 [2026-09-23 审查](docs/reviews/2026-09-23-lifecycle-and-docs-audit.md)，
> 结果见其中 §7。上一轮“固定隧道启动检查”的勾选在 git 历史里（`5cc3389` 之前）。

- [x] D03 命令失败状态传播：Planning 台账、Harness 操作记录、任务事件按命令终态记
      （running / 非零退出 / 超时 / 取消 / 结果未知），后续 read_output 把终态补回。
- [x] D01 任务正式验收：命令终态作为证据落到任务事件；finish 带证据才到 completed，
      失败/运行中/过期/内容不符的证据拒收；Paused 不再放行写入。
- [x] D02 基线恢复：公开 refresh_baseline（先看变更、写明归属、按指纹接纳）；
      排除规则只排目录；不可读文件报告“不完整”。
- [x] D04 客户端能力核对：原因是客户端缓存旧表；server_info 回 connection 摘要，
      新增 `gld tool list --served`，核对步骤写进 troubleshooting。
- [x] 文档：生命周期指南、概念、排障、README、审查记录；docs/cli.md 重新生成。
- [x] 验证：相关单测 → 隔离全量 cargo test / clippy / fmt → 真实二进制隔离复跑。

不纳入本轮：D07 项目级授权（对外多客户端/生产接入前的硬前置）、D09 持久 Job、
D10 浏览器证据、D05 MCP 结果保真、D06 文档生成门禁。

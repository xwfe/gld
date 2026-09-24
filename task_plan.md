# 验收、失败传播、基线恢复、客户端能力核对、结果保真、文档门禁、任务存储、0.7.0 发布（2026-09-23 起）

> 入口与问题编号见 [2026-09-23 审查](docs/reviews/2026-09-23-lifecycle-and-docs-audit.md)，
> 结果见其中 §7。上一轮“固定隧道启动检查”的勾选在 git 历史里（`5cc3389` 之前）。

## 第二轮（已提交 69321e6 / 2e9fa7e / 103528b）

- [x] D03 命令失败状态传播；D01 任务正式验收；D02 基线恢复；D04 客户端工具表核对。

## 第三轮

- [x] `gld tool call` 命令失败时仍退出 0：按决定不改，写进帮助并用测试钉住。
- [x] D14 `git_diff` / `git_show` 文件清单：直接问 git（`--name-status` / `--numstat -z`）。
- [x] D06 文档生成门禁：失败即停、临时文件后替换、隔离 HOME、查空表；全部文档链接检查。
- [x] D05（gld 侧）：输出过期不再叫人直接重跑。
- [x] D05（toexec 侧）：`shape` 只在确定是副本时省掉结构化结果；toexec 本地提交 `bfa809d`。
- [x] D05 收尾：经批准推送 `toexec-mcp-v0.2.1` tag，gld 升级依赖，提交 relay 契约测试与文档。

## 第四轮

- [x] D11 任务存储：工作区级文件锁、临时文件带进程号并 sync；坏任务文件报 `STORE_CORRUPT` 并停写；
  日志坏行跳过并报行号、半行先隔开；索引坏了按任务文件重算。旧代码上先复现再修。

- [x] D13 核对：新版（2026-07-28）客户端先探测 `server/discover`，gld 回 200 + `-32601`，客户端退回
  `initialize`。官方 TS SDK 2.0 实连通过；不改服务端，用测试钉住回法。

## 真机

- [x] 本机升级到 0.7.0（协议 4），Client ID、口令、凭据、ChatGPT 注册的客户端逐项一致；文档写清升级、
  同版本号不提醒重启、ChatGPT 要到 chatgpt.com/plugins 点 Refresh。
- [x] ChatGPT 在 `~/xdw/gld-realtest` 上走完验收、失败拒收、外部修改后先看再接纳、`git_diff` 文件清单。
- [x] 修掉验收中发现的：被拒调用的操作记录没挂任务。

## 收尾

- [x] 推送全部提交；清理真机项目、审查临时证据、旧打包产物和中间备份；改正 `gld rm` 的说法。

## 发布

- [x] D12：发版前核对版本、手写发布说明、musl 必须编过；本机打包与回滚演练；Release 空跑全绿。
- [x] 经批准推送 `v0.7.0`，Release 出来后下载全部包核对校验和；macOS 两个、musl 实跑 `--version`。

## D08 安全语义（2026-09-24）

- [x] `request_permissions` 任何模式都不发授权（dangerous 下以前回 granted，确认门其实一个没放）。
- [x] 退役 `compat-readonly-all`：设置时报错，老配置读成 advanced、标注照实。
- [x] 四个工具的 `confirm` 参数写明服务端核实不了用户批准。

## D07 项目级授权（2026-09-24，RFC-0007）

- [x] 鉴权层：grant 口令授权、令牌带 grant id、验令牌 / 换码 / 刷新都查 grant 还在；bearer 认 grant 令牌。
- [x] hub：范围外项目看不见；只读 grant 收窄到 read-only 工具集；不给本机 MCP 转发；远端只读。
- [x] `gld grant add / ls / rm`，rm 停掉它起的命令；协议号 5；端到端测试走真的 OAuth 与 bearer。
- [x] 独立审查 9 条：默认只读 + `--write`、回滚后 grant 令牌 401、Git 收窄到项目子目录、confine-reads 项目不给 grant、
  grant 不开远端写会话、撤销后 35 秒再清一次、有 grant 时拒绝改 noauth 等。
- [x] 推送 10 个提交（CI 全绿）；本机服务升级到 `e754c51`，凭据与注册客户端逐项指纹一致；机器重启后 `gld daemon start` 恢复。
- [x] 文档：connect-clients.md 四档表（什么都不用做 / 点 Refresh / 重新授权 / 删了重建）为唯一权威；install.md 升级
  多一步比工具表指纹；development.md 要求发布说明写明连接器要不要动；排障、README、daemon 同步。
- [ ] ChatGPT 点 Refresh 后核对新工具表；grant 口令在 ChatGPT 授权页上实测（要用户在网页上操作）。
- [x] 开机自启：四步命令写进 daemon.md（临时 launchd 任务实测后撤掉），本机由用户自己配。
- [x] 重启后 cargo 找不到：补全局可执行路径；修 `gld tool call` 上下文缓存不随全局设置失效（`432dfd7`）。

## D12 尾巴（2026-09-24）

- [x] v0.7.0 glibc 包在 Debian 12 amd64 容器实跑；Linux 上 IPC 正常。
- [x] 查出并修掉后台命令在守护进程退出 / 直连命令行退出后成孤儿（`4ffd96f`），CI Ubuntu、macOS 全绿。
- [x] 发布流水线加构建来源证明（`4216d21`）。
- [x] 来源证明第一次真正签出来：经批准用 gh 手动空跑 Release（run 35962328940，`bc8e13a`），5 个包都验过，改字节、换签名流水线都被拒。
- [ ] 代码签名（付费证书）、Windows 包实跑（没有 Windows 机器）。

## D09 运行记录（2026-09-24）

- [x] 旧代码上先复现：守护进程 stop/start、`kill -9`、直连下一条命令行，`read_output` 都报 `SESSION_NOT_FOUND`。
- [x] `GLD_HOME/runs/`：结局与分段日志落盘、按主体读、配额 64 条 / 7 天；gld 退出记 `interrupted`，没来得及记的判 `unknown`。
- [x] 查出并修掉：守护进程退出时经服务起的命令被记成 `killed`。
- [x] 文档：concepts、lifecycle、排障、daemon、security、architecture、README。
- [x] 独立审查 9 条逐条处理：从记录读的写入次数按起跑计数现算（重启后为 null）、Windows import、全项目清理与测试隔离、
  补 KILL、kill 与自然退出的竞态、退出提示只数真停掉的等。
- [x] 推送 4 个提交，CI run 35961573998 全绿（Ubuntu、macOS 跑了新端到端测试，Windows 编过）。
- [x] 经批准升级本机服务到 `bc8e13a`（指纹逐项一致，ChatGPT 不用 Refresh）；ChatGPT 实测三段：正常重启后
  `interrupted`、重启前的证据被拒、重跑后 `completed`、`kill -9` 后 `unknown` 且孤儿不被误杀。测试项目已清理。

## 发布 0.8.0（2026-09-25）

- [x] 版本号、发布说明、daemon / install 里的版本；全量测试、本机打包实跑（`09b1ae5`）。
- [x] 经批准推送、空跑 Release 全绿后推 `v0.8.0`；下载 5 个包校验和、来源证明全过，macOS 两个与 musl 实跑。
- [ ] glibc 包实跑（Docker Hub 连不上，等网络好了再拉 amd64 镜像）；本机服务要不要换成 0.8.0 发布包由用户定。

不纳入：
D10 浏览器证据（按真实项目需要）；D12 里的签名与来源证明、glibc / Windows 包真机跑；
D13 里真正实现 2026-07-28（等真有只讲新版的客户端）。

# RFC-0003：跟上 ccnm 的新工具，补回 compact 档的 skills

日期：2026-09-18。状态：**实施中**，进度写在第 4 节，每做完一块补一段。

这是跨仓方案 v3（toexec 仓库 `docs/plan/implementation-plan-v3-native-parity.md`）第 5 节第 5 步在 gld 这一侧的落地记录。v3 要的是：经 gld / ccnm 用 AI，能力不低于在项目机器上直接跑官方 CLI。ccnm 那边的执行面已经补了一批（ccnm P36–P41），gld 要跟上。为什么这样分三块、原生 CLI 实际怎么做，都在 v3 方案和 ccnm 的研究记录里，这里只写 gld 要改什么、怎么算做完。

## 1. 要做的三块

| 块 | 做什么 | 为什么 |
| --- | --- | --- |
| **G1 hub 白名单** | hub 的远端工具加上 ccnm P36–P41 的新工具和新参数；连接打开时读远端 `tools/list`，远端没有的工具或参数在 gld 这边就拒 | 不加的话 Web AI 经 hub 用不到远端的图片、notebook、skills、后台命令。不做后一半的话，Runtime 上是旧版 ccnm 时，`run_in_background: true` 会被它悄悄忽略，命令变成前台跑 |
| **G2 compact 档的 skills** | 默认的 compact 工具集放回 skill 目录和 `list_skills` / `get_skill`；frontmatter 换用 `toexec-skill` | 现在 compact 下 Skill 整个不可用；gld 自己的 frontmatter 读法把多行 `description: >` 读成一个 `>` |
| **G3 本地执行面** | gld 本机的搜索加只列文件 / 计数 / 跨行 / 类型过滤；notebook 按 cell 读写 | ccnm 已有，gld 本机成员没有 |

三块互不依赖，按 G1 → G2 → G3 的顺序做，每块单独提交、单独验。

## 2. 验收

| 编号 | 用例与结果 |
| --- | --- |
| G1.1 | 远端工具表加 `remote_load_skill`、`remote_view_image`、`remote_read_notebook`（read 与 coding 都有）和 `remote_stop_command`（要 coding 句柄）；`remote_search_text` 加 `output_mode` / `multiline` / `type` / `include_hidden`，`remote_exec_command` 加 `shell` / `run_in_background`，`remote_read_output` 加 `wait_ms`，`remote_apply_patch` 的说明写上 `write` / `edit_notebook`。参数名以 ccnm 的 `*Args` 结构体为准 |
| G1.2 | 连接握手后读一次远端 `tools/list`，记下每个工具收哪些参数。调用时远端没有这个工具，或 gld 要转发的参数远端的 schema 里没有，就报明确的错（远端 ccnm 版本太老），不发出去 |
| G1.3 | `remote_view_image` / `remote_read_notebook` 的图片块原样透传（H04 不变） |
| G1.4 | `wait_ms` 超过 hub 单次调用预算时在 gld 这边拒，说清楚为什么；远端前台命令超过 60 秒会被掐断这件事写进工具说明，引导用后台命令 |
| G1.5 | 已有 H01–H08 的测试照过；合成 peer 覆盖新工具、参数拦截、老版本远端；有 ccnm 二进制时用本机真实 ccnm 走一遍 |
| G2.1 | compact 下 `list_skills` / `get_skill` 暴露，skill 目录进说明，但有总长上限，放不下的写明"还有几个，调 list_skills 看" |
| G2.2 | skill frontmatter 用 `toexec-skill` 解析，多行 `description` 读对 |
| G2.3 | `gld context`、文档里"compact 不带 Skill"的说法同步改 |
| G3.* | 开工时再定 |

## 3. 不做什么

- 不做 PDF（v3 方案里用户已定暂不做）。
- 不改 hub 的单次调用预算（60 秒）和 coding 空闲回收（2 分钟）；它们和后台命令的配合写进说明，真实 Web 客户端能等多久另测。
- 不把远端的 skill 目录搬进 hub 的说明：远端 skill 由模型调 `remote_load_skill` 不带名字去看。
- 不替远端改参数：超预算的 `wait_ms` 是拒，不是悄悄改小。

## 4. 进度

（每做完一块补一段。）

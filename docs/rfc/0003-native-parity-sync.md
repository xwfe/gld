# RFC-0003：跟上 ccnm 的新工具，补回 compact 档的 skills

日期：2026-09-18。状态：**G1、G2、G3 全部完成**（2026-09-19），过程写在第 4 节。

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
| G3.1 | 本机 `search_text` 加 `output_mode`（content / files_with_matches / count）、`multiline`、`type`、`include_hidden`；默认行为不变；认不出来的 `output_mode` / `type` 报错而不是当没过滤 |
| G3.2 | notebook 按 cell 读写（照 ccnm P40 的形状：新工具 `read_notebook`，`apply_patch` 加 `edit_notebook`，不改 `read_file` 返回 JSON 文本的既有行为） |

## 3. 不做什么

- 不做 PDF（v3 方案里用户已定暂不做）。
- 不改 hub 的单次调用预算（60 秒）和 coding 空闲回收（2 分钟）；它们和后台命令的配合写进说明，真实 Web 客户端能等多久另测。（**后来改了一半**：调用预算没动，空闲回收在挂着后台命令时变成 10 分钟，理由见下面 2026-09-20 那段。）
- 不把远端的 skill 目录搬进 hub 的说明：远端 skill 由模型调 `remote_load_skill` 不带名字去看。
- 不替远端改参数：超预算的 `wait_ms` 是拒，不是悄悄改小。（**后来开了一个口子**：前台 `exec_command` 不给 `timeout_ms` 时替它填一个，因为远端那个默认值必然打爆 hub 的预算。调用方自己给的值仍然只拒不改，同下。）

## 4. 进度

### 2026-09-18：G1 完成

远端工具表从 7 个（4 只读 + 3 写）变成 13 个：只读那组加了 `remote_load_skill`、
`remote_view_image`、`remote_read_notebook`（ccnm P36 / P39 / P40），coding 那组加了
`remote_stop_command`（P41）；`remote_search_text` 加 `output_mode` / `multiline` /
`type` / `include_hidden`，`remote_exec_command` 加 `shell` / `run_in_background`（`cmd`
因此不再必填），`remote_read_output` 加 `wait_ms`，`remote_apply_patch` 的说明写上了
`write` 和 `edit_notebook`。参数名照 ccnm 的 `*Args` 结构体核过。

**连接时读一次远端的 `tools/list`**（`bridge::session::Offered`）：远端没有的工具、
不收的参数，在 gld 这边就拒，报 `REMOTE_TOOL_UNSUPPORTED`，调用一个字节都不发出去。
必须这么做的理由是 ccnm 的参数结构体不拒绝未知字段——老版本收到 `run_in_background`
会**悄悄忽略**它，命令在前台跑满 timeout，而模型以为自己起了一个后台命令。被拒的调用
没碰传输层，所以 coding 句柄照旧有效（写这条测试时才发现原来的实现会把句柄一起作废）。

**`wait_ms` 有上限 50000**（`bridge::tools::MAX_WAIT_MS`）：hub 单次远端调用的预算是
60 秒，等满会被当成传输层出问题、连接一丢 coding 会话就结束，而远端 ccnm 在连接结束时
会停掉这个会话起的后台命令——模型等于自己把要等的命令弄没了。超了是**拒**，不是替它
改小：改小它会以为自己等过了。

验证：

- `cargo test -p gld-core --lib` 369 passed（新增 3 条连接层 + 4 条 hub 层：远端太老、
  参数会被忽略、`wait_ms` 超预算、图片块原样透传；`bridge::tools` 另加 4 条）。
- 真实链路（[现场证据](evidence/v3-g1-remote-tools.md)）：HTTP 客户端 → gld hub →
  包装脚本 → **真实 ccnm 二进制**，13 个远端工具里跑了 12 个，图片块和 notebook 的
  多块结果原样透传，后台命令起→等→停→读全通，超预算的 `wait_ms` 被 gld 拦住。
- 老版本远端那条路只有合成 peer 的测试：真机上装的 ccnm 是 0.7.0，但本地构建是新版。

顺带修的：`gld hub remote add --mode` 的帮助文本还写着"coding 还没实现"（H3 之后就不
成立了），`docs/concepts.md` 里"远端工具是只读的四个"也过时了。

**已知的不稳定测试**（不是本轮引入）：`tools::git::tests::run_git_kills_a_hung_git_and_what_it_spawned_at_the_limit`
在全量并发下偶发失败（`别名没跑起来`）——它给 git 的预算只有 300 毫秒，机器忙的时候
别名脚本来不及写 pid 文件。单独跑稳定通过。本轮没有动它，记在这里。

**2026-09-20 修了**：预算从 300 毫秒提到 2 秒。别名里是 `sleep 20`，所以照样一定超时，
只是给 `git → sh → echo` 三层 fork/exec 留出了落盘的时间。测试本身从 0.3 秒变成 2 秒。

### 2026-09-19：G2 完成

**compact 档不再关掉 Skill。**以前是目录一条不给、`list_skills` / `get_skill` 也不
暴露——项目把用法写进 `.claude/skills/`，默认配置下的 AI 完全不知道。现在两个工具
照常暴露（compact 的工具数 25 → 27），目录有字符预算：`COMPACT_SKILL_CATALOG_CHARS`
= 1200 字符，每条描述截到 120 字符，放不下的在末尾写
`(N more not listed here; call list_skills to see all M.)`。

不是"截断到放得下为止"就完了——**项目自己的 skill 排在主目录那批前面**。发现顺序
本来按来源走，主目录（`scope=global`）先加，于是预算挤掉的正好是这个项目专有的那些。
排序放在 `discover_skills` 末尾（稳定排序），`list_skills` 也跟着一致。

**frontmatter 换成 `toexec-skill`**（tag `toexec-skill-v0.1.0`，gld 的第三个共享
crate）。原来逐行找 `key:` 前缀，`description: >` 这种折行写法读出来是一个 `>`，
那条 skill 在目录里等于没有描述。`.cursorrules` 的 `alwaysApply` 判定也换过去了，
顺带不再区分大小写和连字符。

**`gld context` 的口径跟着改**：从"注入 / 不注入"改成"扫到 N，目录里列了 M"。
列了几条用的是 MCP 握手时同一个渲染函数（`render_skill_catalog_for_profile`），
分开算迟早会报一个和模型看到的不一样的数。打 `·` 的那些在提示里明说没有失效——
AI 调一次 `list_skills` 照样拿得到，只是不会自己想起来。

验证：

- `cargo test --workspace` 603 passed、0 failed；fmt、clippy 干净。
- 新增单测：折行 `description` 读成正文、compact 目录报出少列了几条且其他档不受
  预算影响、工作区 skill 排在主目录之前。
- CLI 集成测试 `context_marks_what_is_actually_injected` 改成实测：工作区里放一个
  `description: >` 写法的 skill，compact 档起真服务、读 MCP `initialize` 的
  `instructions`，里面必须同时出现 skill 名字和折行描述里的标记串。

G2.1–G2.3 三条验收都做了。下一块是 G3（本地搜索的输出模式 / 跨行 / 类型过滤、
notebook 按 cell 读写），开工时再定细目。

### 2026-09-19：G3.1 完成（G3.2 未开工）

本机 `search_text` 补上四项，都是加可选参数，默认行为一个字节没变：

| 参数 | 做什么 | 为什么这么定 |
| --- | --- | --- |
| `output_mode` | `content`（默认）/ `files_with_matches`（只给路径，在 `files[]`）/ `count`（每个文件几行，在 `counts[]`） | "这个符号在哪几个文件里"回完整内容是浪费。`max_results` 在 content 下限匹配行数、另外两种限文件数——名字没改，语义写进了工具说明 |
| `multiline` | 一处匹配可以跨行，`.` 也匹配换行 | 报的行号是匹配**起点**那一行（和 `rg -U` 一致）。跨行统一走正则，字面量先 `regex::escape`：给字面量单写一套跨行的大小写处理，小写化会改变字节偏移，行号就错了 |
| `type` | `rust` / `py` / `ts` / `md` 这些 ripgrep 类型名 | **不是 rg 的全集**——gld 自己遍历文件，没有那份 100 多条的表。给了不认识的类型是**报错并列出支持的**，不是不过滤：不过滤会让模型以为"这个类型里没有匹配" |
| `include_hidden` | 也搜点开头的路径 | 默认仍然不搜。以前是硬编码 false，要改 `.github/workflows` 时连"现在写的是什么"都搜不出来 |

实现上 `files_with_matches` 命中一次就不再读这个文件，`count` 才读完。gld 自己的
数据目录任何情况下都搜不到——那道门在 `is_ignored_path` 第一句，`include_hidden`
管不着它，已有测试 `walking_into_the_gld_data_home_is_skipped` 钉着。

类型表单独放在 `tools/file_types.rs`：纯数据，好单独测，也好加。

验证：`cargo test --workspace` 610 passed、0 failed；fmt、clippy 干净。新增测试
覆盖三种输出模式在同一份内容上自洽（只列文件的那份就是有匹配的那些、计数加起来
等于匹配行数）、跨行匹配只报一条且行号是起点、类型过滤与 `.github` 的显式搜索、
认不出来的模式和类型都报 `INVALID_ARGUMENT`。

**G3.2（notebook 按 cell 读写）还没开工。**形状照 ccnm P40：新增只读工具
`read_notebook`，`apply_patch` 加 `edit_notebook`，**不改** `read_file` 对 `.ipynb`
返回 JSON 文本的既有行为（已有人照着那份文本改 notebook）。

### 2026-09-19：G3.2 完成，G3 收尾

第十一、十二个本机工具位：新增只读工具 `read_notebook`，`apply_patch` 加
`notebook_edits` 参数。工具数 compact 28 / core 40 / advanced 53 / read-only 21。

**形状照 ccnm P40，一处结构上不得不不同。**ccnm 的 `apply_patch` 收的是结构化
ops 数组，所以 `edit_notebook` 是其中一个 op；gld 收的是文本补丁信封，塞不进
"第几个 cell 换成什么"。所以在 gld 这边是**和 `patch` 并列的一个参数**
`notebook_edits: [{path, cells:[…]}]`，`patch` 因此不再必填（两者至少给一个，
策略层的 `validate_patch` 也跟着放宽）。cell 编辑的字段名和语义与 ccnm 一字不差。

**关键的是它们在同一次事务里**：一次调用可以既改源文件又改 notebook 的 cell，
任何一处失败整批不落盘；版本前置条件、备份回滚、进程内写锁全都照样管着它。
同一个文件同时走 `patch` 和 `notebook_edits` 会被拒——两种改法对"原文是什么"
的理解不一样，混在一起没人说得清。

**`read_file` 一个字节没动。**对 `.ipynb` 照旧返回磁盘上那份 JSON：已经有人照着
那段文本用普通补丁改 notebook，换成 cell 视图是行为变更，不是加法。

**和 ccnm 的已知差异：图片。**ccnm 把 PNG/JPEG 输出当 MCP 图片块发出去；gld 的
工具结果是单块的（`wrap_mcp_tool_result` 只有 `view_image` 走图片块），要发多块
得改所有工具的传输形状。所以这里只标注"有一张 image/png，大约多少字节"，不装作
发了。写进了模块文档和 concepts.md，没有藏着。

**往返用的是和 ccnm 同一份 fixture**（`tests/fixtures/notebook/analysis.ipynb`，
那边拿 nbformat 5.11.1 逐字节核对过）：没动过的 notebook 写回去一个字节不差——
中文不转义、键排序、缩进 1 格、末尾换行。差一点，第一次改动就会在 git diff 里
变成整份文件重写。gld 的 `serde_json` 没开 `preserve_order`，键本来就是排序的，
这一条因此成立；哪天开了 `preserve_order`，这个往返测试会先红。

验证：`cargo test --workspace` 637 passed、0 failed；fmt、clippy 干净。单测覆盖
逐字节往返、四种输出的渲染（stream / display_data 的图片标注 / execute_result /
去掉颜色码的 error）、按 cell 分页、替换清空输出、换类型丢掉代码专有键、老
notebook 用 `cell-N` 定位、认不出的 cell id 会列出真实存在的。集成测试覆盖
读→改→落盘的一整圈、和普通补丁同一次事务一起回滚、不是 notebook 的文件怎么报错。

**G3 到此完成**（G3.1 搜索四项 + G3.2 notebook）。RFC-0003 的 G1、G2、G3 三块
全部做完，跨仓方案 v3 第 5 节第 5 步在 gld 这一侧结束。

### 2026-09-20：X04 的组合问题跑出来了，三条都修了（`85bda77`）

跨仓评审 X04 担心 hub 的 120 秒空闲回收会杀掉远端后台任务，当时只有源码推导。
这次把 G1 那份现场证据里的手工链路做成了自动测试
（`crates/core/tests/ccnm_background_lifecycle.rs`：真实 `Connections` + 包装脚本 +
**真实 ccnm 二进制**，没有 ccnm 就跳过），跑出来三条，前两条比 120 秒那条更早发作。

**一、前台命令的期限比 hub 的调用预算还长。**ccnm 不给 `timeout_ms` 就是 120 秒，
而这边的预算是 60 秒——默认值本身就打得爆它。于是一条在远端正常跑到两分钟的前台
命令，会让连接被当成传输层出问题丢掉，远端随即停掉这个会话起的**所有**后台命令。
出事的是前台那条，陪葬的是后台那些。现在前台命令在这边封顶 50 秒
（`bridge::tools::MAX_FOREGROUND_TIMEOUT_MS`）：不给就填上，要更久就拒并指路
`run_in_background`。这是第 3 节"不替远端改参数"唯一的例外，理由是不改必踩；
调用方**自己给**的值仍然只拒不改。

**二、空闲回收看不见在跑的后台命令。**`run_in_background` 的调用一返回就不再是
"在途调用"：槽位没人持有、锁拿得到、`last_used` 停在启动那一刻，`Slot::is_idle`
那两道判断全都看不见它。所以挂着后台命令时换 `CODING_IDLE_WITH_BACKGROUND`
（10 分钟）。不是"可以永远跑"——会话占着远端工作树的写锁，十分钟是个有尽头的数，
也正好是 ccnm 前台命令的最长期限。计数只看请求、不解析远端结果的文本（那是 ccnm
的契约，措辞一变这边就跟着错），所以它是个上界，长阈值兜底。

**三、真实组合才查得出来的：连接被丢时会在远端留孤儿。**`CLOSE_GRACE` 原来是 5 秒，
而 ccnm 读到 EOF 之后要 TERM→2 秒→KILL 地停掉每条命令，一个信号够不着的等 10 秒
才放弃。机器忙的时候 5 秒不够，gld 到点就 `SIGKILL` 了 ccnm——**它一旦被强杀，起的
进程组没人收**：那个 `sleep 60` 的 ppid 变成 1，留在远端，而远端写锁的 `held` 标记
也还在，要人工恢复。测试在并行跑的时候三次里中两次，`ps -o ppid=` 拍下了证据。
宽限提到 20 秒（正常几百毫秒就收完，几乎从不用满），并且关连接不再占着槽位的锁做。

连带把两句话改准：`CodingError::NoSuchSession` 和 hub 的 `REMOTE_OUTCOME_UNKNOWN`
都明说"这个会话在远端跑着的东西跟着停了"。原来模型只知道"句柄没用了"，会以为它起
的构建还在跑——那正是 X04 说的"不能静默消失"。

**没做的**：租约显示与续租预算（这轮只把回收阈值改对，没有把租约摆到模型面前）；
X06 的语义能力协商；durable Job——那要 ccnm 升 `ccnm.workspace-mcp/2` 或显式协商。
ccnm 那半边的权威语义在它的协议第 6 节（四个时钟、session-bound、取消等待不等于
取消命令、终态只有 Runtime 说了算），本轮就是照着它做的。

验证：`cargo test --workspace` 669 passed / 0 failed（带 `CCNM_BIN` 跑，含两条真实
组合测试）；fmt、clippy、`cargo +1.89 check --locked` 干净。

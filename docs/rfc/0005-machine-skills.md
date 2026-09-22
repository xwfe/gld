# RFC-0005：两台机器上装好的 skills

日期：2026-09-22。状态：**已实施**（跨仓 [v4 方案](https://github.com/xwfe/toexec/blob/main/docs/plan/implementation-plan-v4-machine-skills-mcp.md)第 1 步，gld 这一半）。

## 1. 用户的要求

"gld 或 ccnm 都可以使用 agent 和 runtime 机器上的已经安装的 skills 和 mcp……最好为 gld 完善一套合理的 mcp/skills 工具来专门操作它们；ccnm 也是，但是默认全开……抽取共用模块进 toexec"。先做 skills。

gld 跑在它服务的项目所在的机器上，没有单独的 Agent 机器：本机装的 skills gld 早就读（`list_skills` / `get_skill`），远端 ccnm 项目那台机器上装的经 hub 的 `remote_load_skill` 过来。gld 的默认值不变——它可能挂在公网隧道上，主目录里的 skill 默认只给正文、附件要明确配置（跨仓评审 X08 定的），这正是它和 ccnm"默认全开"的区别。

## 2. 改了什么

| 改动 | 为什么 |
| --- | --- |
| 主目录和自定义路径里扫 skill 时**跟符号链接** | 自己写的 skill 常链到一个 git 仓库（这台开发机上 `clarify`、`prc-helm`、`learn-it` 等 6 个都是），以前 gld 整个看不见，原生客户端看得见。工作区里的不跟：仓库里一个指到外面的链接不该让 gld 替它读工作区外的文件 |
| skill 的附件目录按**发现时的路径**判定"在根里面"、按真实路径读 | 以前拿解析后的路径去比根，链接进来的 skill 永远"不在根里"，`filesUnavailable` 还给错理由。真实目录是主目录本身或文件系统根的不给 |
| `get_skill` 的 `file` 和 `files` 改用 `toexec-skill` 0.3.0 的 `dir` 模块 | ccnm 的 `load_skill` 这次也要同一套规则，抽到共享库；错误码和措辞照旧（`SKILL_FILE_OUTSIDE`、`NOT_FOUND`、`INVALID_ARGUMENT`），原有测试一条没改 |
| `gld cfg runtime --hidden-skills a,b` | 按名字藏本机装的 skill（不分大小写），藏掉的和没装一样，不进 `skipped`。只管工作区以外的 |
| hub 的 `remote_load_skill` 多了 `file`、`line` | ccnm P48 起，远端的 `load_skill` 也交出那台机器执行账号装的 skills，它们的附件只能这么读。老版本 ccnm 收到这两个参数会在结果里写明忽略了，不会报错 |

## 3. 没做的

- 按项目分别藏：只有全局一份。
- 远端装好的 skill 经 hub 读附件没跑端到端（只有 ccnm 那边的中立客户端测试和这边的参数表测试）。
- MCP server 的聚合（v4 第 2 步）。

## 4. 验证

`cargo test --workspace` 741 passed / 0 failed（原 738，新增 3 条：链接进来的 skill 找得到且目录是链接目标、指向主目录的不给目录；工作区里的链接不跟；藏掉的 skill 列表和点名都拿不到）；fmt、clippy `-D warnings` 通过。`docs/cli.md` 只加了 `--hidden-skills` 那两行：`scripts/gen-cli-docs.sh` 要连守护进程生成字段表，开发机上跑着的是旧版守护进程（协议不一致），脚本会把那两张表生成成空的，所以没有整份重生成。

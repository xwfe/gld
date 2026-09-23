# 事实与边界

> 本轮（2026-09-23 第二轮）的发现；上一轮隧道任务的发现在 git 历史里。

- 顶层 `ok` 只说明工具调用成功；命令退出码、超时、后台运行都在 `command_ok` / `termination_reason`。
- 以前 read_output 只回 running 与否，后台命令的退出码没有任何工具能拿到。
- 任务 JSON 只存指纹，说不出哪些文件变了；基线复核需要逐文件清单，所以另存 `expected/`。
- `transition` 是公开 Rust API，若允许 Active→Completed 就绕过了证据，故 Completed 只能经 `finish`。
- 验收证据只能证明命令在当前内容上退出 0，不能证明测试充分；命令原文保留给人判断。
- 服务端 tools/list 全部来自同一个 `list_tools_for_profile` + `input_schema`；hub 只加
  list_workspaces / workspace_context / 远端 / 中继，去掉 get/set_default_cwd，加 workspace 参数。
- 服务端声明 `listChanged: false`；客户端缓存旧表时服务端无从通知，只能靠核对。
- zsh 里 `$SID:stdout` 会被当成变量修饰符，复跑脚本要写 `${SID}`。

## 第三轮

- deepwiki 三个工具的 `outputSchema` 都是 FastMCP 包装：`{"result": string}` 加 `x-fastmcp-wrap-result: true`。
  判"副本"用这个精确规则就够，不必猜。
- toexec 规矩：产品原有断言要改才能过就是行为变更；tag 推送后不能移动；产品不能提交 path 依赖。
- `MCP_RESULT_GONE` 对别人的、编造的 ref 也会报，不能说"调用已执行"，只能有条件地说。
- 命令会话输出保留 5 分钟（`SESSION_RETENTION`），troubleshooting 里原来写的 30 秒是错的。
- 本机磁盘接近满（926 GiB 用了约 902 GiB）；另建完整 target 目录会把盘写满。

## 第四轮

- 旧 Harness 写任务文件用固定的 `x.json.tmp`：并发写者一个改名走了，另一个改名时 ENOENT。
- 写前检查是 `current_task().ok().flatten()`：任何读错误都等于"没有任务"，门禁静默失效。
- `BufRead::lines()` 遇到非 UTF-8 行返回 Err，旧代码会让整次读取失败；要按字节 `read_until`。
- flock 按打开的文件描述符算：同进程两个线程各开一次也互斥；同一线程套着拿会自己等自己。
- `task_context` 的回包有 `max_bytes` 预算，坏行明细不能不计预算地塞进去，只回个数。
- MCP 2026-07-28 去掉 initialize；支持它的官方 SDK 客户端先发 `server/discover`，除 -32022 外的错误
  （含 200 + -32601）都判为旧服务器并退回 initialize。405 / 5xx / id 对不上 / 假的成功结果会连不上。
- gld 的指令只在 initialize 结果里，客户端不握手就拿不到——所以"让新客户端退回握手"比"答上它的请求"重要。

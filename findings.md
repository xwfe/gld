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

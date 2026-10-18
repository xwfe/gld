

## 36. 推送到 origin，CI 头一次跑这些提交就红了三个 job（2026-10-18）

**要求：**用户说"推送吧"。`git fetch` 后远端没动，`54bacbf..6b91d7b` 快进推上去（60 个提交，没发版）。推之前扫了一遍要公开的内容：
diff 里没有家目录路径、个人域名、邮箱，作者都是 noreply 地址；有 4 个提交说明里写了 Linux 服务器的 SSH 别名
（指向内网 Tailscale 地址，外面连不上；远端历史里原本也有这类别名），改提交说明要改写历史，没动。

**CI（run 39104838448，`6b91d7b`）：**格式与 Clippy、MSRV 三个平台、命令参考都过，三个 job 红：

| job | 报错 | 根因 | 修 |
| --- | --- | --- | --- |
| Windows 编译检查 | `command_write_paths` 测试里 4 个 `dead_code`（`-D warnings`） | `44d4679` 只给用例加了 `#[cfg(unix)]`，辅助函数和夹具字段在 Windows 上没人用 | `4b84a1c`：文件级 `#![cfg(unix)]`，和 `repo_maintenance_replay.rs` 同一个办法 |
| 测试 (ubuntu) | `scoped_grants::a_grant_needs_real_auth_and_real_projects`：`gld upgrade --port 28765` 报"已经是全局入口的本地端口" | 测试取端口的 `free_port` 会发 gld 自己的默认端口（全局入口 28765、工作区 MCP 从 28766 往上），这些号写进配置时还没人在听，"连一下"查不出来；起点按 pid 错开，只在某些机器上撞 | `d78461d`：`free_port` 跳过 28700–28899，新增 `test_ports.rs`（连取 12000 个都不落进这一段；换回旧实现第一个就发了 28700） |
| 测试 (macOS) | `actions_and_gateway::global_gateway_routes_only_what_opted_in`：`actions.port=28766` 报"与工作区的 MCP 服务重复" | 同上 | 同上 |

后两条是 2026-09-22 起就在的测试辅助缺陷，以前 CI 碰巧没撞上；本机（28767 被常驻服务占着，自动分配会跳开）和 Linux 服务器上跑都没撞过，
所以推之前的两个平台全量都是绿的。Windows 那条本机验不了：没有 Windows 头文件，`ring` 的 C 代码编不过（`--target x86_64-pc-windows-msvc`
实测），只能靠 CI；推之前我对这轮改过的测试文件逐个看了 `cfg(unix)` 的用法，只有这一个文件有问题。

**修完：**本机 50 个测试二进制 984 passed / 0 failed，clippy、fmt 通过；推上去的 `d78461d` 在 CI（run 39105359359）8 个 job 全过（Windows 编译检查、macOS 和 ubuntu 测试、三个平台 MSRV、格式与 Clippy、命令参考）。

**记下的教训：**CI 的 Windows 编译检查和"端口是不是碰巧空着"这两类问题，本机全绿也挡不住。以后改了带 `#[cfg(unix)]` 的测试文件，
推之前按 `docs/development.md`「在 Mac / Linux 上验 Windows 那条编译检查」装 mingw 跑一遍（本机目前没装，装要用户点头）。

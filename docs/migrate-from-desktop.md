# 从桌面版迁移

数据文件格式没变，迁移就是复制一个文件。

## 步骤

1. 退出桌面版（否则它退出时会把内存里的配置再写回去）。
2. 找到桌面版的数据文件：

   | 系统 | 路径 |
   | --- | --- |
   | macOS | `~/Library/Application Support/coding-tools-mcp-desktop/data/profiles.json` |
   | Linux | `~/.config/coding-tools-mcp-desktop/data/profiles.json` |
   | Windows | `%APPDATA%\coding-tools-mcp-desktop\data\profiles.json` |

3. 复制到 gld 的数据目录：

   ```bash
   mkdir -p ~/.config/gld/data
   cp "~/Library/Application Support/coding-tools-mcp-desktop/data/profiles.json" ~/.config/gld/data/profiles.json
   chmod 600 ~/.config/gld/data/profiles.json
   gld start                 # 在任意一个项目目录里：起服务，把桌面版的工作区都加进来
   gld ls
   ```

**gld 不会自己去桌面版的目录里找数据**——它只读自己的数据目录（`~/.config/gld`，
或 `GLD_HOME` 指的地方）。所以这一步的复制是必须的，没有"自动导入"这回事。

工作区（在 gld 里叫项目）、密钥、FRP 配置、全局入口设置全部保留。**有一处不一样**：
桌面版是一个工作区一个服务，gld 只有一个服务、项目都挂在它下面，所以客户端要改连
服务的地址（`gld ls` 里那一行），凭据也换成服务的（`gld ls --reveal`）。
公网入口要重新配一次：`gld share --tunnel …`（见 [connect-clients.md](connect-clients.md)）。
frpc / cloudflared 需要你自己装（`brew install frpc` / `brew install cloudflared`）。
gld 不再代管这两个程序——只要它们在 PATH 里就会被自动认出来。
桌面版下到 `~/.config/gld/bin` 的那份也不会再被使用，可以直接删掉。

## 行为差异

| 桌面版 | gld |
| --- | --- |
| 一个工作区一个服务、一条连接 | 一个服务，项目都挂在它下面，AI 每次调用带 `workspace` 选项目（[concepts.md](concepts.md#服务和项目)） |
| `read_file` 等读工具可以给绝对路径读工作区外的任何文件 | **默认只读项目目录内**（`confine-reads=true`）。桌面版那样的服务能挂公网给 ChatGPT 用，仓库里一段注入文字就能让模型去读 `~/.ssh/id_rsa`。要恢复旧行为：`gld set <项目> confine-reads=false` |
| 关掉窗口 = 服务停止（除非最小化到托盘） | 服务由守护进程持有，关终端不影响；`gld daemon stop` 才会停 |
| 新工作区默认隧道类型 frp | 服务默认没有公网入口，需要时一条 `gld share`（见 [connect-clients.md](connect-clients.md)） |
| 应用启动时按设置恢复上次运行的服务 | MCP 服务没被 `gld stop` 过就会随守护进程回来；GPT Actions 看 `gld cfg runtime --restore-on-launch true` |
| 界面里的“历史上下文”多选 | `gld set <项目> history-context=1,3` |
| Goal / Plan 人工验收按钮 | `gld planning goal accept <id>`、`gld planning plan accept <id>` |
| 更新检查、托盘、WebView 内存释放 | 无 |
| Windows 单实例互斥 | 改为数据目录下的 `daemon.lock` 文件锁 |
| macOS 上端口被上一个自己的实例占着时，会自动杀掉它 | 不杀任何进程，只报告占用者的路径和 pid，由你决定。所以迁移时桌面版必须先退出，否则 `gld start` 会报「端口已被占用：…coding-tools-mcp-desktop」 |

MCP 协议、工具集、OAuth 流程、`docs/history-session/` 档案格式、`.gld/planning/state.json`
都没有变化；但连接器指向的是服务，不是某个工作区，所以已经连上的 ChatGPT 连接器要按
服务的地址和口令重新配一次。

## 两者能同时用吗

不建议。它们各自有一份 `profiles.json`，改了一边另一边不知道；端口也会互相冲突。
迁移完成后把桌面版卸载或至少不要再启动。

# gld 命令参考

> 本文件由 `scripts/gen-cli-docs.sh` 从 `gld --help` 自动生成，请勿手改；改帮助文本请改 `crates/cli/src/cli.rs`。

退出码：0 成功；1 操作失败；2 参数错误；3 守护进程未运行；4 守护进程版本与命令行不一致。

## 目录

- [gld](#gld)
- [gld daemon](#gld-daemon)
- [gld daemon start](#gld-daemon-start)
- [gld daemon stop](#gld-daemon-stop)
- [gld daemon restart](#gld-daemon-restart)
- [gld daemon status](#gld-daemon-status)
- [gld daemon run](#gld-daemon-run)
- [gld daemon logs](#gld-daemon-logs)
- [gld workspace](#gld-workspace)
- [gld workspace add](#gld-workspace-add)
- [gld workspace list](#gld-workspace-list)
- [gld workspace show](#gld-workspace-show)
- [gld workspace remove](#gld-workspace-remove)
- [gld workspace set](#gld-workspace-set)
- [gld workspace fields](#gld-workspace-fields)
- [gld workspace use](#gld-workspace-use)
- [gld start](#gld-start)
- [gld stop](#gld-stop)
- [gld restart](#gld-restart)
- [gld status](#gld-status)
- [gld ps](#gld-ps)
- [gld logs](#gld-logs)
- [gld list](#gld-list)
- [gld share](#gld-share)
- [gld upgrade](#gld-upgrade)
- [gld destroy](#gld-destroy)
- [gld health](#gld-health)
- [gld doctor](#gld-doctor)
- [gld tool](#gld-tool)
- [gld tool list](#gld-tool-list)
- [gld tool schema](#gld-tool-schema)
- [gld tool call](#gld-tool-call)
- [gld tunnel](#gld-tunnel)
- [gld tunnel start](#gld-tunnel-start)
- [gld tunnel stop](#gld-tunnel-stop)
- [gld tunnel restart](#gld-tunnel-restart)
- [gld tunnel test](#gld-tunnel-test)
- [gld tunnel status](#gld-tunnel-status)
- [gld tunnel snippet](#gld-tunnel-snippet)
- [gld gateway](#gld-gateway)
- [gld gateway show](#gld-gateway-show)
- [gld gateway set](#gld-gateway-set)
- [gld gateway start](#gld-gateway-start)
- [gld gateway stop](#gld-gateway-stop)
- [gld gateway health](#gld-gateway-health)
- [gld secret](#gld-secret)
- [gld secret show](#gld-secret-show)
- [gld secret set](#gld-secret-set)
- [gld secret regenerate](#gld-secret-regenerate)
- [gld secret shared](#gld-secret-shared)
- [gld secret keys](#gld-secret-keys)
- [gld frp](#gld-frp)
- [gld frp list](#gld-frp-list)
- [gld frp add](#gld-frp-add)
- [gld frp update](#gld-frp-update)
- [gld frp remove](#gld-frp-remove)
- [gld settings](#gld-settings)
- [gld settings show](#gld-settings-show)
- [gld settings proxy](#gld-settings-proxy)
- [gld settings runtime](#gld-settings-runtime)
- [gld planning](#gld-planning)
- [gld planning show](#gld-planning-show)
- [gld planning mode](#gld-planning-mode)
- [gld planning goal](#gld-planning-goal)
- [gld planning goal create](#gld-planning-goal-create)
- [gld planning goal update](#gld-planning-goal-update)
- [gld planning goal accept](#gld-planning-goal-accept)
- [gld planning goal reject](#gld-planning-goal-reject)
- [gld planning plan](#gld-planning-plan)
- [gld planning plan create](#gld-planning-plan-create)
- [gld planning plan update](#gld-planning-plan-update)
- [gld planning plan accept](#gld-planning-plan-accept)
- [gld planning plan reject](#gld-planning-plan-reject)
- [gld history](#gld-history)
- [gld usage](#gld-usage)
- [gld context](#gld-context)
- [gld completions](#gld-completions)

## gld

```text
gld 管理一组“工作区”（本地项目目录），为每个工作区提供 MCP Streamable HTTP 服务和可选的 GPT Actions OpenAPI 网关，并能通过 FRP /
Cloudflare 隧道暴露到公网。

服务运行在一个后台守护进程里：第一次执行 gld start 时自动拉起，之后关闭终端也不受影响；gld daemon status 可以随时查看它是否在跑。

Usage: gld [OPTIONS] <COMMAND>

Commands:
  daemon       管理后台守护进程（启动 / 停止 / 状态 / 日志）
  workspace    管理工作区（登记项目目录、查看、修改配置、删除） [alias: ws]
  start        启动 MCP（默认）或 Actions 服务；目录没登记过会自动登记为工作区
  stop         停止服务（默认停当前工作区的全部服务）
  restart      重启工作区的服务（默认全部）
  status       查看服务与隧道状态：不带工作区时列出全部，带工作区时显示详情
  ps           只列出正在运行的服务
  logs         查看工作区日志尾部，或用 -f 持续跟随
  list         列出工作区的连接信息：地址、认证方式、凭据、隧道 [alias: ls]
  share        一条命令拿到公网 HTTPS 地址（ChatGPT 只能连公网，127.0.0.1 填进去连不上）
  destroy      销毁工作区：停掉服务与隧道，删掉它的配置和密钥（项目文件一个字节都不动）
  upgrade      改工作区配置（目录 / 公网入口 / 端口 / 认证 / 名称），改完自动重启服务
  health       逐项检查本地 / 公网端点与 OAuth 元数据是否可达
  doctor       体检：检查配置是否自洽，并给出每个问题的修复命令
  tool         直接调用工具内核：不接 AI 客户端也能验证 Agent 会看到什么
  tunnel       管理公网隧道（FRP / Cloudflare）
  gateway      管理全局共享公网入口（多个工作区共用一个域名，按 /w/<id> 路由）
  secret       查看 / 设置 / 重新生成密钥（Bearer Token、OAuth 口令、Actions API Key…）
  frp          管理 FRP 服务器配置（多个工作区可复用同一台 frps）
  settings     全局设置：出站代理、局域网访问、启动时恢复、全局 Agent 说明
  planning     Goal / Plan 规划状态与人工验收
  history      列出工作区的历史会话档案（docs/history-session）
  usage        查看本次守护进程运行期间的请求次数与 Token 估算
  context      查看会注入给 Agent 的说明文件与 Skill（--global 看用户级来源）
  completions  生成 shell 补全脚本

Options:
  -w, --workspace <WS>
          目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断
          
          [env: GLD_WORKSPACE=]

      --json
          以 JSON 输出结果（脚本友好；提示信息仍走 stderr）

      --no-autostart
          守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）

      --timeout <SECS>
          等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）

      --home <DIR>
          数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld）
          
          [env: GLD_HOME=]

      --no-color
          关闭彩色输出（也可设置环境变量 NO_COLOR）

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

快速上手：
  gld start ~/code/my-project             启动 MCP；目录没登记过会自动登记（守护进程自动在后台拉起）
  gld start                               同上，作用于当前目录
  gld list                                看地址、凭据与隧道；不指定工作区时列出全部
  gld share                               要接 ChatGPT 时用：一条命令拿到公网 HTTPS 地址
  gld upgrade --tunnel https://x.com/mcp  改目录 / 公网入口 / 端口 / 认证，改完自动重启
  gld stop                                停止服务（--all 停所有工作区的）；配置不动
  gld destroy                             销毁工作区：连配置和密钥一起删（项目文件不动）

公网入口（--tunnel 在 start / share / upgrade 里通用）：
  --tunnel https://mcp.example.com/mcp    已有公网地址（自建反代等），只登记不起隧道
  --tunnel cf                             Cloudflare 临时地址，零配置，重启会变
  --tunnel cf:mcp.example.com             Cloudflare 固定域名（先 gld secret set cloudflare_token <token>）
  --tunnel frp:公司                       FRP 固定域名，子域名默认取工作区名
  --tunnel off                            关掉公网入口，只留本地地址

工作区定位：
  大多数命令接受 -w/--workspace <id|id前缀|名称|路径>。不给时按当前目录归属推断；
  只有一个工作区时直接使用它。

数据目录：
  默认 ~/.gld，可用 --home 或环境变量 GLD_HOME 覆盖。里面有配置、密钥、日志，
  以及守护进程的 socket 与 pid 文件。frpc / cloudflared 由你自己安装，gld 从 PATH 里找。

更多：docs/cli.md（完整命令参考）、docs/daemon.md（后台进程说明）
```

## gld daemon

```text
管理后台守护进程（启动 / 停止 / 状态 / 日志）

Usage: gld daemon [OPTIONS] <COMMAND>

Commands:
  start    在后台启动守护进程（已在运行则什么都不做）
  stop     请求守护进程退出，并等待它停掉所有服务
  restart  停止后重新启动（升级二进制后用它）
  status   显示守护进程是否在运行、pid、运行时长、日志位置
  run      在前台运行守护进程（给 systemd / launchd 或排障用；Ctrl-C 优雅退出）
  logs     查看守护进程自身日志

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld daemon start

```text
在后台启动守护进程（已在运行则什么都不做）

Usage: gld daemon start [OPTIONS]

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld daemon stop

```text
请求守护进程退出，并等待它停掉所有服务

Usage: gld daemon stop [OPTIONS]

Options:
      --force           超时后强制结束进程树
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --wait <SECS>     等待退出的秒数 [default: 20]
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld daemon restart

```text
停止后重新启动（升级二进制后用它）

Usage: gld daemon restart [OPTIONS]

Options:
      --force           
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld daemon status

```text
显示守护进程是否在运行、pid、运行时长、日志位置

Usage: gld daemon status [OPTIONS]

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld daemon run

```text
在前台运行守护进程（给 systemd / launchd 或排障用；Ctrl-C 优雅退出）

Usage: gld daemon run [OPTIONS]

Options:
      --no-restore      不恢复上次运行的服务
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld daemon logs

```text
查看守护进程自身日志

Usage: gld daemon logs [OPTIONS]

Options:
  -n, --lines <LINES>   显示最后 N 行 [default: 50]
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
  -f, --follow          持续跟随
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld workspace

```text
管理工作区（登记项目目录、查看、修改配置、删除）

Usage: gld workspace [OPTIONS] <COMMAND>

Commands:
  add     把一个项目目录登记为工作区（自动分配空闲端口并生成密钥）
  list    列出所有工作区 [alias: ls]
  show    显示一个工作区的完整配置
  remove  删除工作区（会先停掉它的服务与隧道；不会动项目目录本身） [alias: rm]
  set     修改配置字段：gld ws set port=30000 auth=bearer（字段见 gld ws fields）
  fields  列出 set 支持的字段及取值（默认只列 MCP 侧，--all 连 Actions 一起列）
  use     记住一个“最近使用”的工作区（供脚本或习惯用）

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld workspace add

```text
把一个项目目录登记为工作区（自动分配空闲端口并生成密钥）

Usage: gld workspace add [OPTIONS] [PATH]

Arguments:
  [PATH]  项目根目录（默认当前目录） [default: .]

Options:
      --name <NAME>          显示名称（默认目录名）
  -w, --workspace <WS>       目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json                 以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --mcp-port <PORT>      MCP 端口（默认从 28766 起找空闲）
      --actions-port <PORT>  Actions 端口（默认从 8787 起找空闲）
      --no-autostart         守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>       等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>           数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color             关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help                 Print help
  -V, --version              Print version
```

## gld workspace list

```text
列出所有工作区

Usage: gld workspace list [OPTIONS]

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld workspace show

```text
显示一个工作区的完整配置

Usage: gld workspace show [OPTIONS]

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld workspace remove

```text
删除工作区（会先停掉它的服务与隧道；不会动项目目录本身）

Usage: gld workspace remove [OPTIONS]

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
  -y, --yes             不询问，直接删除
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld workspace set

```text
修改配置字段：gld ws set port=30000 auth=bearer（字段见 gld ws fields）

不写前缀就是改 MCP：port 等价于 mcp.port。改 Actions 那条线路要写全 actions.port。改完会自动重启受影响且正在运行的服务，不用再敲 gld restart。

Usage: gld workspace set [OPTIONS] <KEY=VALUE>...

Arguments:
  <KEY=VALUE>...
          KEY=VALUE，可多个

Options:
  -w, --workspace <WS>
          目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断
          
          [env: GLD_WORKSPACE=]

      --json
          以 JSON 输出结果（脚本友好；提示信息仍走 stderr）

      --no-autostart
          守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）

      --timeout <SECS>
          等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）

      --home <DIR>
          数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld）
          
          [env: GLD_HOME=]

      --no-color
          关闭彩色输出（也可设置环境变量 NO_COLOR）

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

## gld workspace fields

```text
列出 set 支持的字段及取值（默认只列 MCP 侧，--all 连 Actions 一起列）

Usage: gld workspace fields [OPTIONS]

Options:
      --all             连 actions.* 一起列出
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld workspace use

```text
记住一个“最近使用”的工作区（供脚本或习惯用）

Usage: gld workspace use [OPTIONS]

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld start

```text
启动 MCP（默认）或 Actions 服务；目录没登记过会自动登记为工作区

  gld start                          当前目录
  gld start ~/code/api               指定目录
  gld start ~/code/api --tunnel https://mcp.example.com/mcp
                                     顺带配好公网入口，起完直接打印连接信息

守护进程没在跑会自动拉起，之后关掉终端服务也照常在。

Usage: gld start [OPTIONS] [PATH]

Arguments:
  [PATH]
          项目目录（默认当前目录）；没登记过会自动登记为工作区

Options:
      --tunnel <TUNNEL>
          公网入口：https://… | cf | cf:<域名> | frp:<配置名> | off

  -w, --workspace <WS>
          目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断
          
          [env: GLD_WORKSPACE=]

      --json
          以 JSON 输出结果（脚本友好；提示信息仍走 stderr）

      --subdomain <SUB>
          FRP 子域名（配合 --tunnel frp:<配置名>）；不给则取工作区名

      --no-autostart
          守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）

      --port <PORT>
          本地监听端口（默认自动挑一个空闲的；端口被别的程序占了时用它换一个）

  -s, --service <SERVICE>
          启动哪个服务（默认 mcp）
          
          [possible values: mcp, actions, all]

      --timeout <SECS>
          等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）

      --home <DIR>
          数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld）
          
          [env: GLD_HOME=]

      --no-color
          关闭彩色输出（也可设置环境变量 NO_COLOR）

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

## gld stop

```text
停止服务（默认停当前工作区的全部服务）

  gld stop              当前工作区的 MCP 和 Actions
  gld stop -s mcp       只停 MCP
  gld stop --all        所有工作区的所有服务（守护进程留着，下次 start 照常用）

配置和密钥都不动；连守护进程一起退出用 gld daemon stop。

Usage: gld stop [OPTIONS]

Options:
  -s, --service <SERVICE>
          停哪个服务（默认 all）
          
          [possible values: mcp, actions, all]

  -w, --workspace <WS>
          目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断
          
          [env: GLD_WORKSPACE=]

  -a, --all
          停所有工作区的服务，而不只是当前这个

      --json
          以 JSON 输出结果（脚本友好；提示信息仍走 stderr）

      --no-autostart
          守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）

      --timeout <SECS>
          等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）

      --home <DIR>
          数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld）
          
          [env: GLD_HOME=]

      --no-color
          关闭彩色输出（也可设置环境变量 NO_COLOR）

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

## gld restart

```text
重启工作区的服务（默认全部）

改端口 / 认证 / 密钥不需要它——ws set 和 secret set 会自己重启受影响的服务。
用得上它的场景：改了全局设置（gld settings runtime），或服务卡住了想踢一脚。

Usage: gld restart [OPTIONS]

Options:
  -s, --service <SERVICE>
          操作哪个服务；stop / restart 默认 all
          
          [possible values: mcp, actions, all]

  -w, --workspace <WS>
          目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断
          
          [env: GLD_WORKSPACE=]

      --json
          以 JSON 输出结果（脚本友好；提示信息仍走 stderr）

      --no-autostart
          守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）

      --timeout <SECS>
          等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）

      --home <DIR>
          数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld）
          
          [env: GLD_HOME=]

      --no-color
          关闭彩色输出（也可设置环境变量 NO_COLOR）

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

## gld status

```text
查看服务与隧道状态：不带工作区时列出全部，带工作区时显示详情

Usage: gld status [OPTIONS]

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld ps

```text
只列出正在运行的服务

Usage: gld ps [OPTIONS]

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld logs

```text
查看工作区日志尾部，或用 -f 持续跟随

Usage: gld logs [OPTIONS]

Options:
  -s, --service <SERVICE>  看哪个服务的日志 [default: mcp] [possible values: mcp, actions]
  -w, --workspace <WS>     目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json               以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
  -n, --lines <LINES>      显示最后 N 行 [default: 40]
  -f, --follow             持续跟随（Ctrl-C 退出）
      --no-autostart       守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>     等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>         数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color           关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help               Print help
  -V, --version            Print version
```

## gld list

```text
列出工作区的连接信息：地址、认证方式、凭据、隧道

  gld list                不指定工作区时列出全部；在工作区目录里则显示这一个的详情
  gld list -w api         看指定工作区的详情
  gld list --all          在工作区目录里也强制列出全部
  gld list --reveal       凭据显示明文（默认脱敏）

敲惯了 ls 的话，gld ls 是同一条命令。

Usage: gld list [OPTIONS]

Options:
      --reveal
          明文显示密钥（默认脱敏）

  -w, --workspace <WS>
          目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断
          
          [env: GLD_WORKSPACE=]

  -a, --all
          列出全部工作区（在工作区目录里执行时用它看全局）

      --json
          以 JSON 输出结果（脚本友好；提示信息仍走 stderr）

      --no-autostart
          守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）

      --timeout <SECS>
          等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）

      --home <DIR>
          数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld）
          
          [env: GLD_HOME=]

      --no-color
          关闭彩色输出（也可设置环境变量 NO_COLOR）

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

## gld share

```text
一条命令拿到公网 HTTPS 地址（ChatGPT 只能连公网，127.0.0.1 填进去连不上）

它把「配隧道 → 启动服务 → 查连接信息」三步合成一步：
  gld share                             Cloudflare 临时地址（等价 --tunnel cf）
  gld share --tunnel cf:mcp.example.com Cloudflare 固定域名（先 gld secret set cloudflare_token <token>）
  gld share --tunnel frp:公司           FRP 固定域名，子域名默认取工作区名
  gld share --tunnel https://x.com/mcp  已经有公网地址（自建反代等），只登记不起隧道
  gld share --off                       关掉公网入口，只留本地地址

公网入口意味着"在你电脑上跑命令"这件事对外可达，开之前请读 docs/security.md。

Usage: gld share [OPTIONS] [PATH]

Arguments:
  [PATH]
          项目目录（默认当前目录）；没登记过会自动登记为工作区

Options:
      --tunnel <TUNNEL>
          公网入口：https://… | cf | cf:<域名> | frp:<配置名> | off（默认 cf）

  -w, --workspace <WS>
          目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断
          
          [env: GLD_WORKSPACE=]

      --json
          以 JSON 输出结果（脚本友好；提示信息仍走 stderr）

      --subdomain <SUB>
          FRP 子域名，公网地址为 https://<子域名>.<frps 域名>；不给则取工作区名

      --no-autostart
          守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）

      --off
          关掉公网入口，只留本地地址（等价 --tunnel off）

  -s, --service <SERVICE>
          暴露哪个服务
          
          [default: mcp]
          [possible values: mcp, actions]

      --timeout <SECS>
          等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）

      --home <DIR>
          数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld）
          
          [env: GLD_HOME=]

      --no-color
          关闭彩色输出（也可设置环境变量 NO_COLOR）

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

## gld upgrade

```text
改工作区配置（目录 / 公网入口 / 端口 / 认证 / 名称），改完自动重启服务

  gld upgrade --tunnel https://new.example.com/mcp   换公网地址
  gld upgrade --path ~/code/api-v2                   项目搬了目录
  gld upgrade api --port 30001 --auth bearer         按名称指定工作区
  gld upgrade --off                                  关掉公网入口

只改这几项常用配置；全部字段见 gld workspace fields 与 gld workspace set。

Usage: gld upgrade [OPTIONS] [WS]

Arguments:
  [WS]
          要更新哪个工作区：目录 / 名称 / id（默认按当前目录推断）

Options:
      --path <DIR>
          换项目根目录（目录要已存在）

  -w, --workspace <WS>
          目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断
          
          [env: GLD_WORKSPACE=]

      --json
          以 JSON 输出结果（脚本友好；提示信息仍走 stderr）

      --tunnel <TUNNEL>
          换公网入口：https://… | cf | cf:<域名> | frp:<配置名> | off

      --no-autostart
          守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）

      --subdomain <SUB>
          FRP 子域名（配合 --tunnel frp:<配置名>）

      --off
          关掉公网入口（等价 --tunnel off）

      --timeout <SECS>
          等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）

      --home <DIR>
          数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld）
          
          [env: GLD_HOME=]

      --name <NAME>
          换显示名称

      --no-color
          关闭彩色输出（也可设置环境变量 NO_COLOR）

      --port <PORT>
          换 MCP 端口

      --actions-port <PORT>
          换 Actions 端口

      --auth <AUTH>
          换 MCP 认证方式：oauth | bearer | noauth

  -s, --service <SERVICE>
          改哪个服务的公网入口
          
          [default: mcp]
          [possible values: mcp, actions]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

## gld destroy

```text
销毁工作区：停掉服务与隧道，删掉它的配置和密钥（项目文件一个字节都不动）

  gld destroy              当前目录对应的工作区
  gld destroy api          按名称 / 路径 / id 指定
  gld destroy --all        全部工作区
  gld destroy -y           不询问

只是想停服务用 gld stop——那个不删任何东西。
密钥删了就没了，客户端里存的 token / 口令会全部失效。

Usage: gld destroy [OPTIONS] [WS]

Arguments:
  [WS]
          要销毁哪个工作区：目录 / 名称 / id（默认按当前目录推断）

Options:
  -a, --all
          销毁全部工作区

  -w, --workspace <WS>
          目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断
          
          [env: GLD_WORKSPACE=]

      --json
          以 JSON 输出结果（脚本友好；提示信息仍走 stderr）

  -y, --yes
          不询问，直接销毁

      --no-autostart
          守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）

      --timeout <SECS>
          等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）

      --home <DIR>
          数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld）
          
          [env: GLD_HOME=]

      --no-color
          关闭彩色输出（也可设置环境变量 NO_COLOR）

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

## gld health

```text
逐项检查本地 / 公网端点与 OAuth 元数据是否可达

Usage: gld health [OPTIONS]

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld doctor

```text
体检：检查配置是否自洽，并给出每个问题的修复命令

Usage: gld doctor [OPTIONS]

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld tool

```text
直接调用工具内核：不接 AI 客户端也能验证 Agent 会看到什么

Usage: gld tool [OPTIONS] <COMMAND>

Commands:
  list    列出当前工作区暴露给 AI 的工具（取决于 mcp.tool-profile） [alias: ls]
  schema  显示某个工具的完整定义与参数 Schema
  call    调用一个工具，打印结构化结果

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld tool list

```text
列出当前工作区暴露给 AI 的工具（取决于 mcp.tool-profile）

Usage: gld tool list [OPTIONS]

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld tool schema

```text
显示某个工具的完整定义与参数 Schema

Usage: gld tool schema [OPTIONS] <NAME>

Arguments:
  <NAME>  工具名，例如 read_file

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld tool call

```text
调用一个工具，打印结构化结果

参数三种写法： key=value    字符串；true / false / null / 数字 / [ 或 { 开头会按 JSON 解析 key:=json    强制按 JSON 解析，例如
limit:=100 key=@文件    读取文件内容作为字符串，适合 apply_patch 的补丁正文

例： gld tool call read_file path=src/main.rs gld tool call exec_command cmd='cargo test'
timeout_ms:=120000 gld tool call git_status

工具返回 ok=false 时退出码为 1，结构化结果照常打印，可以接 jq。 长命令留下的 exec 会话只在守护进程运行时才能被下一次调用读到 （直连模式每次都是新进程）。

Usage: gld tool call [OPTIONS] <NAME> [ARG]...

Arguments:
  <NAME>
          工具名

  [ARG]...
          参数，见上面三种写法

Options:
      --args-json <JSON>
          直接给一段 JSON 对象作为参数，与上面的写法合并（这个优先）

  -w, --workspace <WS>
          目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断
          
          [env: GLD_WORKSPACE=]

      --json
          以 JSON 输出结果（脚本友好；提示信息仍走 stderr）

      --no-autostart
          守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）

      --timeout <SECS>
          等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）

      --home <DIR>
          数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld）
          
          [env: GLD_HOME=]

      --no-color
          关闭彩色输出（也可设置环境变量 NO_COLOR）

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

## gld tunnel

```text
管理公网隧道（FRP / Cloudflare）

Usage: gld tunnel [OPTIONS] <COMMAND>

Commands:
  start    启动隧道（服务启动时通常已自动启动，这里用于单独重连）
  stop     停止隧道
  restart  重启隧道（FRP 会原子替换线路，失败自动回滚配置）
  test     验证隧道配置：拿到公网地址即通过；本地服务没在跑则测完自动断开
  status   查看隧道状态与公网地址
  snippet  打印这个工作区的完整 frpc 配置（存成 frpc.toml 就能自己跑）

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld tunnel start

```text
启动隧道（服务启动时通常已自动启动，这里用于单独重连）

Usage: gld tunnel start [OPTIONS]

Options:
  -s, --service <SERVICE>  哪个服务的隧道 [default: mcp] [possible values: mcp, actions]
  -w, --workspace <WS>     目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json               以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart       守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>     等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>         数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color           关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help               Print help
  -V, --version            Print version
```

## gld tunnel stop

```text
停止隧道

Usage: gld tunnel stop [OPTIONS]

Options:
  -s, --service <SERVICE>  哪个服务的隧道 [default: mcp] [possible values: mcp, actions]
  -w, --workspace <WS>     目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json               以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart       守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>     等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>         数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color           关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help               Print help
  -V, --version            Print version
```

## gld tunnel restart

```text
重启隧道（FRP 会原子替换线路，失败自动回滚配置）

Usage: gld tunnel restart [OPTIONS]

Options:
  -s, --service <SERVICE>  哪个服务的隧道 [default: mcp] [possible values: mcp, actions]
  -w, --workspace <WS>     目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json               以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart       守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>     等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>         数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color           关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help               Print help
  -V, --version            Print version
```

## gld tunnel test

```text
验证隧道配置：拿到公网地址即通过；本地服务没在跑则测完自动断开

Usage: gld tunnel test [OPTIONS]

Options:
  -s, --service <SERVICE>  哪个服务的隧道 [default: mcp] [possible values: mcp, actions]
  -w, --workspace <WS>     目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json               以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart       守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>     等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>         数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color           关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help               Print help
  -V, --version            Print version
```

## gld tunnel status

```text
查看隧道状态与公网地址

Usage: gld tunnel status [OPTIONS]

Options:
  -s, --service <SERVICE>  哪个服务的隧道 [default: mcp] [possible values: mcp, actions]
  -w, --workspace <WS>     目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json               以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart       守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>     等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>         数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color           关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help               Print help
  -V, --version            Print version
```

## gld tunnel snippet

```text
打印这个工作区的完整 frpc 配置（存成 frpc.toml 就能自己跑）

Usage: gld tunnel snippet [OPTIONS]

Options:
  -s, --service <SERVICE>  哪个服务的隧道 [default: mcp] [possible values: mcp, actions]
  -w, --workspace <WS>     目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json               以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --reveal             输出真实的 frps token（默认是占位符，避免贴聊天窗口时泄露）
      --no-autostart       守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>     等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>         数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color           关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help               Print help
  -V, --version            Print version
```

## gld gateway

```text
管理全局共享公网入口（多个工作区共用一个域名，按 /w/<id> 路由）

Usage: gld gateway [OPTIONS] <COMMAND>

Commands:
  show    显示全局入口配置与运行状态
  set     修改配置（只改给出的项）
  start   启动全局入口（含它的隧道）
  stop    停止全局入口
  health  检查本地与公网 /health

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld gateway show

```text
显示全局入口配置与运行状态

Usage: gld gateway show [OPTIONS]

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld gateway set

```text
修改配置（只改给出的项）

Usage: gld gateway set [OPTIONS]

Options:
      --enabled <true|false>    是否启用 [possible values: true, false]
  -w, --workspace <WS>          目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json                    以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --port <PORT>             本地端口
      --no-autostart            守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --tunnel <TYPE>           隧道类型：cf（Cloudflare）| frp | off（配合 --public-url 用现成地址）
      --public-url <URL>        手动公网地址（tunnel=none 时使用）
      --timeout <SECS>          等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --frp-profile <ID>        FRP 服务器配置 id
      --home <DIR>              数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --frp-subdomain <SUB>     FRP 子域名
      --no-color                关闭彩色输出（也可设置环境变量 NO_COLOR）
      --use-proxy <true|false>  隧道是否套用全局代理 [possible values: true, false]
  -h, --help                    Print help
  -V, --version                 Print version
```

## gld gateway start

```text
启动全局入口（含它的隧道）

Usage: gld gateway start [OPTIONS]

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld gateway stop

```text
停止全局入口

Usage: gld gateway stop [OPTIONS]

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld gateway health

```text
检查本地与公网 /health

Usage: gld gateway health [OPTIONS]

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld secret

```text
查看 / 设置 / 重新生成密钥（Bearer Token、OAuth 口令、Actions API Key…）

Usage: gld secret [OPTIONS] <COMMAND>

Commands:
  show        显示工作区密钥（默认脱敏，--reveal 明文）
  set         设置工作区密钥；正在运行且用到它的服务会自动重启
  regenerate  重新生成工作区密钥并返回新值；相关服务自动重启 [alias: regen]
  shared      操作共享密钥池（多个工作区勾选 shared-secrets 时共用）
  keys        列出所有合法的密钥名及用途

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld secret show

```text
显示工作区密钥（默认脱敏，--reveal 明文）

Usage: gld secret show [OPTIONS] <KEY>

Arguments:
  <KEY>  

Options:
      --reveal          
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld secret set

```text
设置工作区密钥；正在运行且用到它的服务会自动重启

Usage: gld secret set [OPTIONS] <KEY> <VALUE>

Arguments:
  <KEY>    
  <VALUE>  

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld secret regenerate

```text
重新生成工作区密钥并返回新值；相关服务自动重启

Usage: gld secret regenerate [OPTIONS] <KEY>

Arguments:
  <KEY>  

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld secret shared

```text
操作共享密钥池（多个工作区勾选 shared-secrets 时共用）

Usage: gld secret shared [OPTIONS] <COMMAND>

Commands:
  show        
  set         
  regenerate  [alias: regen]

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld secret keys

```text
列出所有合法的密钥名及用途

Usage: gld secret keys [OPTIONS]

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld frp

```text
管理 FRP 服务器配置（多个工作区可复用同一台 frps）

Usage: gld frp [OPTIONS] <COMMAND>

Commands:
  list    列出 FRP 服务器配置
  add     新增
  update  修改（只改给出的项）
  remove  删除（还被工作区引用时会拒绝，除非加 --force） [alias: rm]

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld frp list

```text
列出 FRP 服务器配置

Usage: gld frp list [OPTIONS]

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld frp add

```text
新增

Usage: gld frp add [OPTIONS] --name <NAME> --server <SERVER>

Options:
      --name <NAME>      
  -w, --workspace <WS>   目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json             以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --server <SERVER>  frps 地址，例如 frp.example.com
      --no-autostart     守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --port <PORT>      [default: 7000]
      --timeout <SECS>   等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --token <TOKEN>    frps token（保存在数据目录，不会出现在 list 输出里）
      --home <DIR>       数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color         关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help             Print help
  -V, --version          Print version
```

## gld frp update

```text
修改（只改给出的项）

Usage: gld frp update [OPTIONS] <ID>

Arguments:
  <ID>  

Options:
      --name <NAME>      
  -w, --workspace <WS>   目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json             以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --server <SERVER>  
      --no-autostart     守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --port <PORT>      
      --timeout <SECS>   等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --token <TOKEN>    
      --home <DIR>       数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color         关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help             Print help
  -V, --version          Print version
```

## gld frp remove

```text
删除（还被工作区引用时会拒绝，除非加 --force）

Usage: gld frp remove [OPTIONS] <ID>

Arguments:
  <ID>  

Options:
      --force           照删不误，留下悬空引用（那些工作区下次 start 会报"引用的 FRP 配置不存在"）
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld settings

```text
全局设置：出站代理、局域网访问、启动时恢复、全局 Agent 说明

Usage: gld settings [OPTIONS] <COMMAND>

Commands:
  show     显示全部全局设置
  proxy    全局出站代理（隧道进程使用）；不带参数时显示当前值
  runtime  运行时全局项：局域网访问、启动时恢复、可执行路径、全局 Agent 说明

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld settings show

```text
显示全部全局设置

Usage: gld settings show [OPTIONS]

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld settings proxy

```text
全局出站代理（隧道进程使用）；不带参数时显示当前值

Usage: gld settings proxy [OPTIONS]

Options:
      --mode <MODE>     none | system | manual
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --url <URL>       manual 模式的代理地址，例如 http://127.0.0.1:7890
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld settings runtime

```text
运行时全局项：局域网访问、启动时恢复、可执行路径、全局 Agent 说明

Usage: gld settings runtime [OPTIONS]

Options:
      --lan-access <true|false>
          允许 MCP / Actions / 全局入口监听 0.0.0.0（默认只监听 127.0.0.1） [possible values: true, false]
  -w, --workspace <WS>
          目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json
          以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --restore-on-launch <true|false>
          守护进程启动时恢复上次运行的服务 [possible values: true, false]
      --executable-paths <EXECUTABLE_PATHS>
          全局可执行文件搜索路径（换行或分号分隔）
      --no-autostart
          守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --ai-instructions <AI_INSTRUCTIONS>
          注入给所有工作区 Agent 的全局说明
      --timeout <SECS>
          等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>
          数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --instruction-sources <INSTRUCTION_SOURCES>
          全局说明文件来源，逗号分隔（如 cursor,claude,codex）
      --no-color
          关闭彩色输出（也可设置环境变量 NO_COLOR）
      --skill-sources <SKILL_SOURCES>
          全局 Skill 来源，逗号分隔
      --custom-instruction-paths <CUSTOM_INSTRUCTION_PATHS>
          
      --custom-skill-paths <CUSTOM_SKILL_PATHS>
          
  -h, --help
          Print help
  -V, --version
          Print version
```

## gld planning

```text
Goal / Plan 规划状态与人工验收

Usage: gld planning [OPTIONS] <COMMAND>

Commands:
  show  显示当前模式、Goal / Plan 与执行台账
  mode  切换模式：direct（自由改）| plan（只读，AI 先出计划）| goal（写操作须绑定 Goal）
  goal  
  plan  

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld planning show

```text
显示当前模式、Goal / Plan 与执行台账

Usage: gld planning show [OPTIONS]

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld planning mode

```text
切换模式：direct（自由改）| plan（只读，AI 先出计划）| goal（写操作须绑定 Goal）

Usage: gld planning mode [OPTIONS] <MODE>

Arguments:
  <MODE>  

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld planning goal

```text
Usage: gld planning goal [OPTIONS] <COMMAND>

Commands:
  create  
  update  
  accept  人工验收通过并归档
  reject  驳回验收，Goal 回到 active

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld planning goal create

```text
Usage: gld planning goal create [OPTIONS] --title <TITLE> --objective <OBJECTIVE>

Options:
      --title <TITLE>             
  -w, --workspace <WS>            目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json                      以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --objective <OBJECTIVE>     
      --criterion <CRITERIA>      可多次给出
      --no-autostart              守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --constraint <CONSTRAINTS>  
      --timeout <SECS>            等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>                数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color                  关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help                      Print help
  -V, --version                   Print version
```

## gld planning goal update

```text
Usage: gld planning goal update [OPTIONS] <GOAL_ID>

Arguments:
  <GOAL_ID>  

Options:
      --title <TITLE>             
  -w, --workspace <WS>            目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json                      以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --objective <OBJECTIVE>     
      --no-autostart              守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --status <STATUS>           active | paused | completed | awaiting_acceptance | archived |
                                  cancelled
      --constraint <CONSTRAINTS>  
      --timeout <SECS>            等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --done <DONE>               已完成的验收项 id，逗号分隔
      --home <DIR>                数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --focus <FOCUS>             [possible values: true, false]
      --no-color                  关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help                      Print help
  -V, --version                   Print version
```

## gld planning goal accept

```text
人工验收通过并归档

Usage: gld planning goal accept [OPTIONS] <GOAL_ID>

Arguments:
  <GOAL_ID>  

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld planning goal reject

```text
驳回验收，Goal 回到 active

Usage: gld planning goal reject [OPTIONS] <GOAL_ID>

Arguments:
  <GOAL_ID>  

Options:
      --feedback <FEEDBACK>  
  -w, --workspace <WS>       目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json                 以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart         守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>       等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>           数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color             关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help                 Print help
  -V, --version              Print version
```

## gld planning plan

```text
Usage: gld planning plan [OPTIONS] <COMMAND>

Commands:
  create  
  update  
  accept  
  reject  

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld planning plan create

```text
Usage: gld planning plan create [OPTIONS] --title <TITLE> --objective <OBJECTIVE>

Options:
      --title <TITLE>          
  -w, --workspace <WS>         目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json                   以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --objective <OBJECTIVE>  
      --goal <GOAL>            
      --no-autostart           守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --step <STEPS>           可多次给出
      --timeout <SECS>         等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>             数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color               关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help                   Print help
  -V, --version                Print version
```

## gld planning plan update

```text
Usage: gld planning plan update [OPTIONS] <PLAN_ID>

Arguments:
  <PLAN_ID>  

Options:
      --status <STATUS>  draft | active | paused | completed | awaiting_acceptance | archived |
                         cancelled
  -w, --workspace <WS>   目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json             以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --step <STEPS>     STEP_ID=STATUS[:备注]，可多次；STATUS 为
                         pending|in_progress|completed|blocked|skipped
      --focus <FOCUS>    [possible values: true, false]
      --no-autostart     守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>   等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>       数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color         关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help             Print help
  -V, --version          Print version
```

## gld planning plan accept

```text
Usage: gld planning plan accept [OPTIONS] <PLAN_ID>

Arguments:
  <PLAN_ID>  

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld planning plan reject

```text
Usage: gld planning plan reject [OPTIONS] <PLAN_ID>

Arguments:
  <PLAN_ID>  

Options:
      --feedback <FEEDBACK>  
  -w, --workspace <WS>       目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json                 以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart         守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>       等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>           数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color             关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help                 Print help
  -V, --version              Print version
```

## gld history

```text
列出工作区的历史会话档案（docs/history-session）

Usage: gld history [OPTIONS]

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld usage

```text
查看本次守护进程运行期间的请求次数与 Token 估算

Usage: gld usage [OPTIONS]

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld context

```text
查看会注入给 Agent 的说明文件与 Skill（--global 看用户级来源）

Usage: gld context [OPTIONS]

Options:
      --global          扫描用户主目录下各 IDE / Agent 的全局说明与 Skill 来源
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld completions

```text
生成 shell 补全脚本

Usage: gld completions [OPTIONS] <SHELL>

Arguments:
  <SHELL>  bash | zsh | fish | powershell | elvish [possible values: bash, elvish, fish, powershell,
           zsh]

Options:
  -w, --workspace <WS>  目标工作区：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld workspace set 支持的字段

```text
字段                      取值                                                         说明
name                      文本                                                         显示名称
path                      已存在的目录                                                 项目根目录；换目录后服务会重启到新目录（旧目录里的历史档案留在原地）
port                      1-65535                                                      MCP 本地监听端口
auth                      oauth | bearer | noauth                                      MCP 认证方式
oauth-client-id           文本                                                         MCP OAuth 静态 Client ID
shared-secrets            true | false                                                 MCP 使用共享密钥池而非工作区密钥
tool-profile              compact | core | advanced | read-only | compat-readonly-all  暴露给客户端的工具集（compact 为稳定聚合 API；core / advanced 保留兼容旧工具名）
permission-mode           trusted | dangerous                                          工具权限模式；两者的写入边界完全一样（都只能写工作区内），见 docs/concepts.md
history-recording         true | false                                                 是否允许把会话检查点写入 docs/history-session
history-context           逗号分隔的编号，或空                                         新会话注入哪些历史档案（有界快照）
allowed-commands          逗号分隔                                                     在默认白名单之外追加的命令；写成 only:cargo,git 则表示只允许这些
confine-reads             true | false                                                 读工具只许读 Workspace 内（默认 true；关掉才能读隔壁仓库等外部路径）
executable-paths          路径列表（换行或分号分隔）                                   额外的可执行文件搜索路径
ai-instructions           文本                                                         注入 Agent 的工作区级说明
tunnel                    frp | cf | none                                              MCP 公网隧道类型（cf 即 cloudflare，两种写法都收）
frp-profile               FRP 配置的名称或 id，或空                                    使用哪个 FRP 服务器配置（见 gld frp list）
frp-subdomain             子域名（小写字母 / 数字 / 连字符）                           FRP 子域名，公网地址为 https://<子域名>.<服务器>
cloudflare-mode           quick | named                                                Cloudflare 隧道模式
public-url                https:// 开头的 URL，或空                                    手动指定公网地址（隧道类型 none 时使用）
use-proxy                 true | false                                                 启动隧道时是否套用全局代理
global-gateway            true | false                                                 通过全局共享入口 /w/<id> 暴露而不是独立隧道
actions.port              1-65535                                                      Actions 本地监听端口
actions.auth              api_key | oauth | none                                       Actions 认证方式
actions.oauth-client-id   文本                                                         Actions OAuth Client ID
actions.shared-secrets    true | false                                                 Actions 使用共享密钥池
actions.confine-reads     true | false                                                 Actions 侧同上（默认 true）
actions.allowed-commands  逗号分隔                                                     Actions 侧同上（追加；only: 前缀表示只允许这些）
actions.tunnel            frp | cf | none                                              Actions 公网隧道类型（cf 即 cloudflare）
actions.frp-profile       FRP 配置的名称或 id，或空                                    Actions 使用的 FRP 服务器配置
actions.frp-subdomain     子域名（小写字母 / 数字 / 连字符）                           Actions FRP 子域名
actions.cloudflare-mode   quick | named                                                Actions Cloudflare 隧道模式
actions.public-url        https:// 开头的 URL，或空                                    Actions 手动公网地址
actions.use-proxy         true | false                                                 Actions 隧道是否套用全局代理
actions.global-gateway    true | false                                                 Actions 通过全局共享入口暴露

用法：gld workspace set port=30000 auth=bearer
```

## gld secret keys 密钥名一览

```text
密钥名                       作用域         用途
bearer_token                 工作区 / 共享  MCP 认证方式为 bearer 时客户端携带的 Token
oauth_client_id              共享           MCP OAuth Client ID（仅共享池；工作区级用 gld ws set mcp.oauth-client-id）
oauth_client_secret          工作区 / 共享  MCP OAuth 静态 Client Secret（可选；ChatGPT 走 PKCE 不需要）
oauth_password               工作区 / 共享  MCP OAuth 授权页输入的口令
oauth_token_secret           工作区 / 共享  签发 MCP Access / Refresh Token 用的密钥
cloudflare_token             工作区         MCP Named Cloudflare Tunnel 的 token
frp_token                    工作区         覆盖 MCP 隧道使用的 frps token（通常配在 FRP 配置里）
actions_api_key              工作区 / 共享  Actions 认证方式为 api_key 时的 Key
actions_oauth_client_secret  工作区 / 共享  Actions OAuth Client Secret
actions_oauth_password       工作区 / 共享  Actions OAuth 授权口令
actions_oauth_token_secret   工作区 / 共享  签发 Actions Token 的密钥
actions_cloudflare_token     工作区         Actions Named Cloudflare Tunnel 的 token
actions_frp_token            工作区         覆盖 Actions 隧道使用的 frps token

工作区级：gld secret show|set|regen <KEY>     共享池：gld secret shared show|set|regen <KEY>
```

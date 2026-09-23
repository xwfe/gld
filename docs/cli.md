# gld 命令参考

> 本文件由 `scripts/gen-cli-docs.sh` 从 `gld --help` 自动生成，请勿手改；改帮助文本请改 `crates/cli/src/cli.rs`。

退出码：0 成功；1 操作失败；2 参数错误；3 守护进程未运行；4 守护进程版本与命令行不一致。

RFC-0004 之前的命令（`ws`、`destroy`、`hub`、`ps`、`tunnel`、`gateway`、各处的 `show`）还能敲，只是不进帮助，这里也不列；新旧对照见 [RFC-0004](rfc/0004-one-service-many-projects.md) 第 2 节。

## 目录

- [gld](#gld)
- [gld start](#gld-start)
- [gld stop](#gld-stop)
- [gld restart](#gld-restart)
- [gld status](#gld-status)
- [gld list](#gld-list)
- [gld add](#gld-add)
- [gld remove](#gld-remove)
- [gld set](#gld-set)
- [gld fields](#gld-fields)
- [gld share](#gld-share)
- [gld upgrade](#gld-upgrade)
- [gld remote](#gld-remote)
- [gld remote add](#gld-remote-add)
- [gld remote remove](#gld-remote-remove)
- [gld mcp](#gld-mcp)
- [gld mcp list](#gld-mcp-list)
- [gld mcp on](#gld-mcp-on)
- [gld mcp off](#gld-mcp-off)
- [gld mcp test](#gld-mcp-test)
- [gld logs](#gld-logs)
- [gld health](#gld-health)
- [gld doctor](#gld-doctor)
- [gld tool](#gld-tool)
- [gld tool list](#gld-tool-list)
- [gld tool schema](#gld-tool-schema)
- [gld tool call](#gld-tool-call)
- [gld secret](#gld-secret)
- [gld secret list](#gld-secret-list)
- [gld secret set](#gld-secret-set)
- [gld secret regenerate](#gld-secret-regenerate)
- [gld secret keys](#gld-secret-keys)
- [gld frp](#gld-frp)
- [gld frp list](#gld-frp-list)
- [gld frp add](#gld-frp-add)
- [gld frp update](#gld-frp-update)
- [gld frp remove](#gld-frp-remove)
- [gld settings](#gld-settings)
- [gld settings list](#gld-settings-list)
- [gld settings proxy](#gld-settings-proxy)
- [gld settings runtime](#gld-settings-runtime)
- [gld planning](#gld-planning)
- [gld planning list](#gld-planning-list)
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
- [gld daemon](#gld-daemon)
- [gld daemon start](#gld-daemon-start)
- [gld daemon stop](#gld-daemon-stop)
- [gld daemon restart](#gld-daemon-restart)
- [gld daemon status](#gld-daemon-status)
- [gld daemon run](#gld-daemon-run)
- [gld daemon logs](#gld-daemon-logs)
- [gld completions](#gld-completions)

## gld

```text
gld 在后台跑一个 MCP Streamable HTTP 服务，把登记进来的项目目录都挂在它下面：客户端只配一条连接，AI 每次调用用 workspace 参数选项目，项目之间互不串。需要时能通过
FRP / Cloudflare 隧道暴露到公网。

服务运行在一个后台守护进程里：第一次执行 gld start 时自动拉起，之后关闭终端也不受影响；gld daemon status 可以随时查看它是否在跑。

Usage: gld [OPTIONS] <COMMAND>

Commands:
  start        启动 MCP 服务；给了目录（或当前目录就是项目）会顺带把它加进来
  stop         停止服务（项目、配置、凭据都不动）
  restart      重启服务
  status       服务、公网入口和项目的一览
  list         客户端要填的地址、凭据，和所有项目；给项目名就看这个项目的配置 [alias: ls]
  add          加项目：登记目录并加入服务（服务在跑就立即生效，不用重启）
  remove       删项目：从服务里拿掉，并删掉它在 gld 这边的配置和凭据（项目文件一个字节都不动） [alias: rm]
  set          改项目配置：gld set api tool-profile=read-only（字段见 gld fields）
  fields       列出 set 支持的项目字段及取值（--all 连 GPT Actions 那条线路一起列）
  share        给服务拿一个公网 HTTPS 地址（ChatGPT 只能连公网，127.0.0.1 填进去连不上）
  upgrade      改服务配置（端口 / 认证 / 工具集 / 公网入口），改完自动重启；也能改项目的目录和名称
  remote       远端项目：另一台机器上由 ccnm 管着的 workspace，经 ccnm mcp bridge 访问
  mcp          本机装好的 MCP server：看装了哪些、开哪几个经服务转给 AI、试着起一个
  logs         查看服务日志尾部，或用 -f 持续跟随（-w 看某个项目自己的请求日志）
  health       逐项检查本地 / 公网端点与 OAuth 元数据是否可达
  doctor       体检：检查配置是否自洽，并给出每个问题的修复命令
  tool         直接调用工具内核：不接 AI 客户端也能验证 Agent 会看到什么
  secret       服务的凭据：看 / 自己定 / 重新生成（Bearer Token、OAuth 口令、Tunnel Token…）
  frp          管理 FRP 服务器配置（--tunnel frp:<配置名> 引用它）
  settings     全局设置：出站代理、局域网访问、启动时恢复、全局 Agent 说明 [alias: cfg]
  planning     Goal / Plan 规划状态与人工验收（按项目）
  history      列出项目的历史会话档案（docs/history-session）
  usage        查看本次守护进程运行期间的请求次数与 Token 估算
  context      查看会注入给 Agent 的说明文件与 Skill（--global 看用户级来源）
  daemon       管理后台守护进程（启动 / 停止 / 状态 / 日志）
  completions  生成 shell 补全脚本

Options:
  -w, --workspace <WS>
          目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断
          
          [env: GLD_WORKSPACE=]

      --json
          以 JSON 输出结果（脚本友好；提示信息仍走 stderr）

      --no-autostart
          守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）

      --timeout <SECS>
          等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）

      --home <DIR>
          数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld）
          
          [env: GLD_HOME=]

      --no-color
          关闭彩色输出（也可设置环境变量 NO_COLOR）

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

快速上手：
  gld start ~/code/api          启动 MCP 服务，并把这个目录加进来（守护进程自动在后台拉起）
  gld add ~/code/web            再加一个项目；服务在跑就立即生效
  gld ls                        客户端要填的地址和凭据，和所有项目
  gld share                     要接 ChatGPT 时用：给服务拿一个公网 HTTPS 地址
  gld set web tool-profile=read-only   改某个项目的配置（字段见 gld fields）
  gld rm web                    删掉一个项目（只删 gld 这边的配置，项目文件不动）
  gld stop                      停服务；项目、配置和凭据都留着

只有一个服务：客户端里只配一条连接，AI 每次调用带 workspace 参数（项目名或 id）选项目。
拿到服务凭据就能访问全部项目——只想单独给出去的项目别加进来。

公网入口（--tunnel 在 start / share / upgrade 里通用）：
  --tunnel https://mcp.example.com/mcp    已有公网地址（自建反代等），只登记不起隧道
  --tunnel cf                             Cloudflare 临时地址，零配置，重启会变
  --tunnel cf:mcp.example.com             Cloudflare 固定域名，要 Tunnel Token（没配过会当场问）
  --tunnel frp:公司                       FRP 固定域名，子域名默认 gld
  --tunnel off                            关掉公网入口，只留本地地址

项目定位：
  改项目的命令接受项目名（或 -w <id|id前缀|名称|路径>）。不给时按当前目录归属推断；
  只有一个项目时直接使用它。

数据目录：
  默认 ~/.config/gld，可用 --home 或环境变量 GLD_HOME 覆盖。里面有配置、密钥、日志，
  以及守护进程的 socket 与 pid 文件。frpc / cloudflared 由你自己安装，gld 从 PATH 里找。

更多：docs/cli.md（完整命令参考）、docs/daemon.md（后台进程说明）
```

## gld start

```text
启动 MCP 服务；给了目录（或当前目录就是项目）会顺带把它加进来

  gld start                          起服务；当前目录是项目、或者还一个项目都没有时，顺带加当前目录
  gld start ~/code/api               起服务，并把这个目录加进来
  gld start --tunnel cf:mcp.example.com
                                     顺带配好公网入口，起完直接打印连接信息

守护进程没在跑会自动拉起，之后关掉终端服务也照常在。
-s actions 起的是这个项目的 GPT Actions（自定义 GPT 用），它还是一个项目一个。

Usage: gld start [OPTIONS] [PATH]

Arguments:
  [PATH]
          顺带加进来的项目目录；不给时见上面的说明

Options:
      --tunnel <TUNNEL>
          公网入口：https://… | cf | cf:<域名> | frp:<配置名> | off

  -w, --workspace <WS>
          目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断
          
          [env: GLD_WORKSPACE=]

      --json
          以 JSON 输出结果（脚本友好；提示信息仍走 stderr）

      --token <TOKEN>
          Cloudflare Tunnel Token（配合 --tunnel cf:<域名>）；不给会当场问

      --no-autostart
          守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）

      --subdomain <SUB>
          FRP 子域名（配合 --tunnel frp:<配置名>）；不给则是 gld

      --port <PORT>
          服务的本地端口；Cloudflare 固定隧道需与云端回源端口一致（不会自动修改云端配置）

      --timeout <SECS>
          等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）

      --home <DIR>
          数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld）
          
          [env: GLD_HOME=]

  -s, --service <SERVICE>
          起哪个：mcp（默认，服务）| actions（这个项目的 GPT Actions）| all
          
          [possible values: mcp, actions, all]

      --no-color
          关闭彩色输出（也可设置环境变量 NO_COLOR）

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

## gld stop

```text
停止服务（项目、配置、凭据都不动）

  gld stop              MCP 服务，连同各项目的 GPT Actions
  gld stop -s actions   只停当前项目的 GPT Actions

连守护进程一起退出用 gld daemon stop；要删项目用 gld rm。

Usage: gld stop [OPTIONS]

Options:
  -s, --service <SERVICE>
          停哪个：mcp（服务）| actions（当前项目的 GPT Actions）| all（默认，全部）
          
          [possible values: mcp, actions, all]

  -w, --workspace <WS>
          目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断
          
          [env: GLD_WORKSPACE=]

      --json
          以 JSON 输出结果（脚本友好；提示信息仍走 stderr）

      --no-autostart
          守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）

      --timeout <SECS>
          等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）

      --home <DIR>
          数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld）
          
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
重启服务

改端口 / 认证 / 凭据不需要它——upgrade 和 secret set 会自己重启服务。
用得上它的场景：改了全局设置（gld cfg runtime），或服务卡住了想踢一脚。

Usage: gld restart [OPTIONS]

Options:
  -s, --service <SERVICE>
          操作哪个服务；stop / restart 默认 all
          
          [possible values: mcp, actions, all]

  -w, --workspace <WS>
          目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断
          
          [env: GLD_WORKSPACE=]

      --json
          以 JSON 输出结果（脚本友好；提示信息仍走 stderr）

      --no-autostart
          守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）

      --timeout <SECS>
          等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）

      --home <DIR>
          数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld）
          
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
服务、公网入口和项目的一览

Usage: gld status [OPTIONS]

Options:
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld list

```text
客户端要填的地址、凭据，和所有项目；给项目名就看这个项目的配置

  gld ls                  服务的地址、凭据（脱敏）和项目表
  gld ls api              项目 api 的配置
  gld ls --reveal         凭据显示明文

Usage: gld list [OPTIONS] [PROJECT]

Arguments:
  [PROJECT]
          只看这个项目的配置（名称、id、id 前缀或路径）

Options:
      --reveal
          明文显示密钥（默认脱敏）

  -w, --workspace <WS>
          目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断
          
          [env: GLD_WORKSPACE=]

      --json
          以 JSON 输出结果（脚本友好；提示信息仍走 stderr）

      --no-autostart
          守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）

      --timeout <SECS>
          等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）

      --home <DIR>
          数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld）
          
          [env: GLD_HOME=]

      --no-color
          关闭彩色输出（也可设置环境变量 NO_COLOR）

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

## gld add

```text
加项目：登记目录并加入服务（服务在跑就立即生效，不用重启）

  gld add                 当前目录
  gld add ~/code/api ~/code/web
  gld add . --name api    起个名字（默认是目录名）

另一台机器上 ccnm 管着的项目用 gld remote add。

Usage: gld add [OPTIONS] [PATH]...

Arguments:
  [PATH]...
          项目目录，可以一次给多个（默认当前目录）

Options:
      --name <NAME>
          显示名称（默认目录名；只给一个目录时能用）

  -w, --workspace <WS>
          目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断
          
          [env: GLD_WORKSPACE=]

      --json
          以 JSON 输出结果（脚本友好；提示信息仍走 stderr）

      --no-autostart
          守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）

      --timeout <SECS>
          等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）

      --home <DIR>
          数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld）
          
          [env: GLD_HOME=]

      --no-color
          关闭彩色输出（也可设置环境变量 NO_COLOR）

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

## gld remove

```text
删项目：从服务里拿掉，并删掉它在 gld 这边的配置和凭据（项目文件一个字节都不动）

  gld rm api              按名称 / 路径 / id 指定，可以一次给多个
  gld rm                  当前目录对应的项目
  gld rm --all -y         全部项目，不询问

远端项目（gld remote add 加的）也用它删。

Usage: gld remove [OPTIONS] [PROJECT]...

Arguments:
  [PROJECT]...
          要删的项目：名称 / 路径 / id，可以一次给多个（默认按当前目录推断）

Options:
  -a, --all
          删掉全部本地项目

  -w, --workspace <WS>
          目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断
          
          [env: GLD_WORKSPACE=]

      --json
          以 JSON 输出结果（脚本友好；提示信息仍走 stderr）

  -y, --yes
          不询问，直接删

      --no-autostart
          守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）

      --timeout <SECS>
          等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）

      --home <DIR>
          数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld）
          
          [env: GLD_HOME=]

      --no-color
          关闭彩色输出（也可设置环境变量 NO_COLOR）

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

## gld set

```text
改项目配置：gld set api tool-profile=read-only（字段见 gld fields）

  gld set tool-profile=read-only              当前目录对应的项目
  gld set api allowed-commands=rg,gh          按名称指定项目

下一次调用就生效，不用重启。服务本身的端口、认证、公网入口用 gld upgrade / gld share。

Usage: gld set [OPTIONS] <ARG>...

Arguments:
  <ARG>...
          [项目] KEY=VALUE…：第一个不带 = 的是项目，其余是要改的字段

Options:
  -w, --workspace <WS>
          目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断
          
          [env: GLD_WORKSPACE=]

      --json
          以 JSON 输出结果（脚本友好；提示信息仍走 stderr）

      --no-autostart
          守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）

      --timeout <SECS>
          等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）

      --home <DIR>
          数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld）
          
          [env: GLD_HOME=]

      --no-color
          关闭彩色输出（也可设置环境变量 NO_COLOR）

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

## gld fields

```text
列出 set 支持的项目字段及取值（--all 连 GPT Actions 那条线路一起列）

Usage: gld fields [OPTIONS]

Options:
      --all             连 actions.* 一起列出
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld share

```text
给服务拿一个公网 HTTPS 地址（ChatGPT 只能连公网，127.0.0.1 填进去连不上）

它把「配公网入口 → 起服务 → 查连接信息」三步合成一步：
  gld share                             沿用已配好的入口；一个都没配就用 Cloudflare 临时地址
  gld share --tunnel cf:mcp.example.com Cloudflare 固定域名，要 Tunnel Token（没配过会当场问）
  gld share --tunnel frp:公司           FRP 固定域名，子域名默认 gld
  gld share --tunnel https://x.com/mcp  已经有公网地址（自建反代等），只登记不起隧道
  gld share --off                       关掉公网入口，只留本地地址

公网入口意味着"在你电脑上跑命令"这件事对外可达，而且一把凭据能进全部项目。
开之前请读 docs/security.md。

Usage: gld share [OPTIONS] [PATH]

Arguments:
  [PATH]
          顺带加进来的项目目录（可选，规则同 gld start）

Options:
      --tunnel <TUNNEL>
          公网入口：https://… | cf | cf:<域名> | frp:<配置名> | off（默认沿用已配好的，没有就 cf）

  -w, --workspace <WS>
          目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断
          
          [env: GLD_WORKSPACE=]

      --json
          以 JSON 输出结果（脚本友好；提示信息仍走 stderr）

      --token <TOKEN>
          Cloudflare Tunnel Token（配合 --tunnel cf:<域名>）；不给会当场问

      --no-autostart
          守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）

      --subdomain <SUB>
          FRP 子域名，公网地址为 https://<子域名>.<frps 域名>；不给则是 gld

      --off
          关掉公网入口，只留本地地址（等价 --tunnel off）

      --timeout <SECS>
          等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）

      --home <DIR>
          数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld）
          
          [env: GLD_HOME=]

  -s, --service <SERVICE>
          暴露哪个：mcp（默认，服务）| actions（当前项目的 GPT Actions）
          
          [default: mcp]
          [possible values: mcp, actions]

      --no-color
          关闭彩色输出（也可设置环境变量 NO_COLOR）

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

## gld upgrade

```text
改服务配置（端口 / 认证 / 工具集 / 公网入口），改完自动重启；也能改项目的目录和名称

  gld upgrade --port 30001 --auth bearer            服务的端口和认证
  gld upgrade --tunnel https://new.example.com/mcp  换公网地址
  gld upgrade --off                                 关掉公网入口
  gld upgrade api --path ~/code/api-v2              项目搬了目录

项目的其余字段见 gld fields 与 gld set。

Usage: gld upgrade [OPTIONS] [PROJECT]

Arguments:
  [PROJECT]
          --path / --name 改的是哪个项目：目录 / 名称 / id（默认按当前目录推断）

Options:
      --path <DIR>
          把项目根目录换成这个（要已存在）；挑哪个项目用上面的 PROJECT 或 -w，不是它

  -w, --workspace <WS>
          目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断
          
          [env: GLD_WORKSPACE=]

      --json
          以 JSON 输出结果（脚本友好；提示信息仍走 stderr）

      --tunnel <TUNNEL>
          换公网入口：https://… | cf | cf:<域名> | frp:<配置名> | off

      --no-autostart
          守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）

      --token <TOKEN>
          Cloudflare Tunnel Token（配合 --tunnel cf:<域名>）；不给会当场问

      --subdomain <SUB>
          FRP 子域名（配合 --tunnel frp:<配置名>）

      --timeout <SECS>
          等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）

      --home <DIR>
          数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld）
          
          [env: GLD_HOME=]

      --off
          关掉公网入口（等价 --tunnel off）

      --name <NAME>
          换项目的显示名称

      --no-color
          关闭彩色输出（也可设置环境变量 NO_COLOR）

      --port <PORT>
          换服务的端口

      --actions-port <PORT>
          换项目的 GPT Actions 端口

      --auth <AUTH>
          换服务的认证方式：oauth | bearer | noauth

      --tool-profile <PROFILE>
          换服务列给客户端的工具集；项目自己的工具集照样生效，两边取交集

  -s, --service <SERVICE>
          --tunnel / --off 改哪个：mcp（默认，服务）| actions（项目的 GPT Actions）
          
          [default: mcp]
          [possible values: mcp, actions]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

## gld remote

```text
远端项目：另一台机器上由 ccnm 管着的 workspace，经 ccnm mcp bridge 访问

Usage: gld remote [OPTIONS] <COMMAND>

Commands:
  add     登记一个远端 workspace 并加进服务（立即生效，不用重启）
  remove  删掉一个远端项目（按名字、id 或 id 前缀；gld rm 也能删） [alias: rm]

Options:
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld remote add

```text
登记一个远端 workspace 并加进服务（立即生效，不用重启）

  gld remote add prod --node work --remote-workspace server

两个值填的都是 **ccnm 配置里的名字**，不是 host 也不是路径。在那台机器上
跑 ccnm workspace list 能看到有哪些。

叫 --remote-workspace 是因为 --workspace / -w 已经被全局参数占了，
那个说的是"本机哪个工作区"，两回事。

Usage: gld remote add [OPTIONS] --node <NODE> --remote-workspace <WS> <NAME>

Arguments:
  <NAME>
          给人看的名字，调用时 workspace 参数也能用它

Options:
      --node <NODE>
          ccnm 配置里的 node 别名（一台机器的名字）

  -w, --workspace <WS>
          目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断
          
          [env: GLD_WORKSPACE=]

      --json
          以 JSON 输出结果（脚本友好；提示信息仍走 stderr）

      --remote-workspace <WS>
          ccnm 配置里的 workspace 名

      --ccnm <PATH>
          本机 ccnm 可执行程序（默认用 PATH 里的 ccnm）

      --no-autostart
          守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）

      --mode <MODE>
          访问上限：read（默认）| coding。coding 的成员多六个工具（改文件、跑命令、读输出、停后台命令、用那台机器上的 MCP server），用之前先
          remote_coding_begin 拿句柄

      --timeout <SECS>
          等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）

      --home <DIR>
          数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld）
          
          [env: GLD_HOME=]

      --no-color
          关闭彩色输出（也可设置环境变量 NO_COLOR）

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

## gld remote remove

```text
删掉一个远端项目（按名字、id 或 id 前缀；gld rm 也能删）

Usage: gld remote remove [OPTIONS] <NAME>

Arguments:
  <NAME>  

Options:
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld mcp

```text
本机装好的 MCP server：看装了哪些、开哪几个经服务转给 AI、试着起一个

  gld mcp ls                     ~/.claude.json 和 ~/.codex/config.toml 里装了哪些、开了哪些
  gld mcp on context7 deepwiki   开：连上服务的 AI 用 list_mcp_tools / call_mcp_tool 调它们
  gld mcp off context7           关（--all 全关）
  gld mcp test context7          在守护进程里起一次：起不起得来、有哪些工具

默认一个都不开。AI 经服务调它们，和你在本机 Claude Code 里调一样：Filesystem、
desktop-commander 这类能读写整个主目录。服务挂了公网入口时尤其想清楚再开。

Usage: gld mcp [OPTIONS] <COMMAND>

Commands:
  list  装了哪些、开了哪些（读 ~/.claude.json 的 mcpServers 和 ~/.codex/config.toml 的 mcp_servers） [alias: ls]
  on    开：经服务转给 AI，下一次调用就生效，不用重启服务
  off   关：正开着的连接在下一次调用时收掉
  test  在守护进程里起一次、握手、列工具（用的是服务起它时的 PATH 和环境变量）

Options:
  -w, --workspace <WS>
          目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断
          
          [env: GLD_WORKSPACE=]

      --json
          以 JSON 输出结果（脚本友好；提示信息仍走 stderr）

      --no-autostart
          守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）

      --timeout <SECS>
          等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）

      --home <DIR>
          数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld）
          
          [env: GLD_HOME=]

      --no-color
          关闭彩色输出（也可设置环境变量 NO_COLOR）

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

## gld mcp list

```text
装了哪些、开了哪些（读 ~/.claude.json 的 mcpServers 和 ~/.codex/config.toml 的 mcp_servers）

Usage: gld mcp list [OPTIONS]

Options:
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld mcp on

```text
开：经服务转给 AI，下一次调用就生效，不用重启服务

Usage: gld mcp on [OPTIONS] <NAME>...

Arguments:
  <NAME>...  gld mcp ls 里的名字，区分大小写，可以一次给多个

Options:
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld mcp off

```text
关：正开着的连接在下一次调用时收掉

Usage: gld mcp off [OPTIONS] [NAME]...

Arguments:
  [NAME]...  要关的名字，可以一次给多个

Options:
      --all             全关
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld mcp test

```text
在守护进程里起一次、握手、列工具（用的是服务起它时的 PATH 和环境变量）

Usage: gld mcp test [OPTIONS] <NAME>

Arguments:
  <NAME>  

Options:
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld logs

```text
查看服务日志尾部，或用 -f 持续跟随（-w 看某个项目自己的请求日志）

Usage: gld logs [OPTIONS]

Options:
  -s, --service <SERVICE>  mcp：服务的日志（给了 -w 就是那个项目的请求日志）| actions：项目的 GPT Actions [default: mcp]
                           [possible values: mcp, actions]
  -w, --workspace <WS>     目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json               以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
  -n, --lines <LINES>      显示最后 N 行 [default: 40]
  -f, --follow             持续跟随（Ctrl-C 退出）
      --no-autostart       守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>     等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>         数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
      --no-color           关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help               Print help
  -V, --version            Print version
```

## gld health

```text
逐项检查本地 / 公网端点与 OAuth 元数据是否可达

Usage: gld health [OPTIONS]

Options:
  -s, --service <SERVICE>  mcp（默认）：服务 | actions：当前项目的 GPT Actions [default: mcp] [possible values:
                           mcp, actions]
  -w, --workspace <WS>     目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json               以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart       守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>     等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>         数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
      --no-color           关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help               Print help
  -V, --version            Print version
```

## gld doctor

```text
体检：检查配置是否自洽，并给出每个问题的修复命令

Usage: gld doctor [OPTIONS]

Options:
      --probe           再实地探一次本地 / 公网端点和 OAuth 元数据（同 gld health）；不加它一个网络请求都不发
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld tool

```text
直接调用工具内核：不接 AI 客户端也能验证 Agent 会看到什么

Usage: gld tool [OPTIONS] <COMMAND>

Commands:
  list    列出当前项目暴露给 AI 的工具（取决于它的 tool-profile） [alias: ls]
  schema  显示某个工具的完整定义与参数 Schema
  call    调用一个工具，打印结构化结果

Options:
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld tool list

```text
列出当前项目暴露给 AI 的工具（取决于它的 tool-profile）

Usage: gld tool list [OPTIONS]

Options:
      --served          改看正在跑的服务此刻给客户端的 tools/list（参数、指纹、构建提交），用来查客户端是否缓存了旧表
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
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
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
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

工具返回 ok=false 时退出码为 1，结构化结果照常打印，可以接 jq。 exec_command 跑的命令自己失败（退出非零、超时）不算 ok=false，退出码仍是 0：
脚本要判断命令结果，读结果里的 command_ok / exit_code。 长命令留下的 exec 会话只在守护进程运行时才能被下一次调用读到 （直连模式每次都是新进程）。

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
          目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断
          
          [env: GLD_WORKSPACE=]

      --json
          以 JSON 输出结果（脚本友好；提示信息仍走 stderr）

      --no-autostart
          守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）

      --timeout <SECS>
          等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）

      --home <DIR>
          数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld）
          
          [env: GLD_HOME=]

      --no-color
          关闭彩色输出（也可设置环境变量 NO_COLOR）

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

## gld secret

```text
服务的凭据：看 / 自己定 / 重新生成（Bearer Token、OAuth 口令、Tunnel Token…）

Usage: gld secret [OPTIONS] <COMMAND>

Commands:
  list        列出凭据（默认脱敏，--reveal 明文）；给了 KEY 只看那一项 [alias: ls]
  set         自己定一项凭据（记得住的授权口令、Cloudflare Tunnel Token）；服务在跑会自动重启
  regenerate  重新生成一项凭据并返回新值；服务在跑会自动重启 [alias: regen]
  keys        列出所有凭据名及用途

Options:
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld secret list

```text
列出凭据（默认脱敏，--reveal 明文）；给了 KEY 只看那一项

Usage: gld secret list [OPTIONS] [KEY]

Arguments:
  [KEY]  

Options:
      --reveal          
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld secret set

```text
自己定一项凭据（记得住的授权口令、Cloudflare Tunnel Token）；服务在跑会自动重启

Usage: gld secret set [OPTIONS] <KEY> <VALUE>

Arguments:
  <KEY>    
  <VALUE>  

Options:
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld secret regenerate

```text
重新生成一项凭据并返回新值；服务在跑会自动重启

Usage: gld secret regenerate [OPTIONS] <KEY>

Arguments:
  <KEY>  

Options:
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld secret keys

```text
列出所有凭据名及用途

Usage: gld secret keys [OPTIONS]

Options:
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld frp

```text
管理 FRP 服务器配置（--tunnel frp:<配置名> 引用它）

Usage: gld frp [OPTIONS] <COMMAND>

Commands:
  list    列出 FRP 服务器配置 [alias: ls]
  add     新增
  update  修改（只改给出的项）
  remove  删除（还被工作区引用时会拒绝，除非加 --force） [alias: rm]

Options:
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld frp list

```text
列出 FRP 服务器配置

Usage: gld frp list [OPTIONS]

Options:
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
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
  -w, --workspace <WS>   目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json             以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --server <SERVER>  frps 地址，例如 frp.example.com
      --no-autostart     守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --port <PORT>      [default: 7000]
      --timeout <SECS>   等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --token <TOKEN>    frps token（保存在数据目录，不会出现在 list 输出里）
      --home <DIR>       数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
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
  -w, --workspace <WS>   目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json             以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --server <SERVER>  
      --no-autostart     守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --port <PORT>      
      --timeout <SECS>   等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --token <TOKEN>    
      --home <DIR>       数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
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
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld settings

```text
全局设置：出站代理、局域网访问、启动时恢复、全局 Agent 说明

Usage: gld settings [OPTIONS] <COMMAND>

Commands:
  list     显示全部全局设置 [alias: ls]
  proxy    全局出站代理（隧道进程使用）；不带参数时显示当前值
  runtime  运行时全局项：局域网访问、启动时恢复、可执行路径、全局 Agent 说明 [alias: set]

Options:
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld settings list

```text
显示全部全局设置

Usage: gld settings list [OPTIONS]

Options:
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
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
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --url <URL>       manual 模式的代理地址，例如 http://127.0.0.1:7890
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
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
          允许服务 / Actions / 全局入口监听 0.0.0.0（默认只监听 127.0.0.1） [possible values: true, false]
  -w, --workspace <WS>
          目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json
          以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --restore-on-launch <true|false>
          守护进程启动时恢复上次运行的 GPT Actions（MCP 服务不看它：没被 gld stop 过就恢复） [possible values: true, false]
      --executable-paths <EXECUTABLE_PATHS>
          全局可执行文件搜索路径（换行或分号分隔）
      --no-autostart
          守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --ai-instructions <AI_INSTRUCTIONS>
          注入给所有项目 Agent 的全局说明
      --timeout <SECS>
          等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>
          数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
      --instruction-sources <INSTRUCTION_SOURCES>
          全局说明文件来源，逗号分隔（如 cursor,claude,codex）
      --no-color
          关闭彩色输出（也可设置环境变量 NO_COLOR）
      --skill-sources <SKILL_SOURCES>
          全局 Skill 来源，逗号分隔
      --custom-instruction-paths <CUSTOM_INSTRUCTION_PATHS>
          
      --custom-skill-paths <CUSTOM_SKILL_PATHS>
          
      --hidden-skills <NAMES>
          本机装的 Skill 里不交给 AI 的，按名字，逗号分隔；传 "" 清空。项目自己的不受影响
  -h, --help
          Print help
  -V, --version
          Print version
```

## gld planning

```text
Goal / Plan 规划状态与人工验收（按项目）

Usage: gld planning [OPTIONS] <COMMAND>

Commands:
  list  显示当前模式、Goal / Plan 与执行台账 [alias: ls]
  mode  切换模式：direct（自由改）| plan（只读，AI 先出计划）| goal（写操作须绑定 Goal）
  goal  
  plan  

Options:
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld planning list

```text
显示当前模式、Goal / Plan 与执行台账

Usage: gld planning list [OPTIONS]

Options:
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
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
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
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
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld planning goal create

```text
Usage: gld planning goal create [OPTIONS] --title <TITLE> --objective <OBJECTIVE>

Options:
      --title <TITLE>             
  -w, --workspace <WS>            目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json                      以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --objective <OBJECTIVE>     
      --criterion <CRITERIA>      可多次给出
      --no-autostart              守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --constraint <CONSTRAINTS>  
      --timeout <SECS>            等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>                数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
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
  -w, --workspace <WS>            目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json                      以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --objective <OBJECTIVE>     
      --no-autostart              守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --status <STATUS>           active | paused | completed | awaiting_acceptance | archived |
                                  cancelled
      --constraint <CONSTRAINTS>  
      --timeout <SECS>            等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --done <DONE>               已完成的验收项 id，逗号分隔
      --home <DIR>                数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
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
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
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
  -w, --workspace <WS>       目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json                 以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart         守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>       等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>           数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
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
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld planning plan create

```text
Usage: gld planning plan create [OPTIONS] --title <TITLE> --objective <OBJECTIVE>

Options:
      --title <TITLE>          
  -w, --workspace <WS>         目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json                   以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --objective <OBJECTIVE>  
      --goal <GOAL>            
      --no-autostart           守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --step <STEPS>           可多次给出
      --timeout <SECS>         等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>             数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
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
  -w, --workspace <WS>   目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json             以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --step <STEPS>     STEP_ID=STATUS[:备注]，可多次；STATUS 为
                         pending|in_progress|completed|blocked|skipped
      --focus <FOCUS>    [possible values: true, false]
      --no-autostart     守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>   等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>       数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
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
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
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
  -w, --workspace <WS>       目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json                 以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart         守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>       等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>           数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
      --no-color             关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help                 Print help
  -V, --version              Print version
```

## gld history

```text
列出项目的历史会话档案（docs/history-session）

Usage: gld history [OPTIONS]

Options:
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld usage

```text
查看本次守护进程运行期间的请求次数与 Token 估算

Usage: gld usage [OPTIONS]

Options:
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
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
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
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
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld daemon start

```text
在后台启动守护进程（已在运行则什么都不做）

Usage: gld daemon start [OPTIONS]

Options:
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
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
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --wait <SECS>     等待退出的秒数 [default: 20]
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
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
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld daemon status

```text
显示守护进程是否在运行、pid、运行时长、日志位置

Usage: gld daemon status [OPTIONS]

Options:
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
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
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
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
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
  -f, --follow          持续跟随
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
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
  -w, --workspace <WS>  目标项目：id、id 前缀（≥4 位）、名称或路径；省略时按当前目录推断 [env: GLD_WORKSPACE=]
      --json            以 JSON 输出结果（脚本友好；提示信息仍走 stderr）
      --no-autostart    守护进程未运行时不要自动拉起（需要它时以退出码 3 报错）
      --timeout <SECS>  等待守护进程响应的秒数（默认 30，启动服务 / 隧道类为 180）
      --home <DIR>      数据目录（等价于环境变量 GLD_HOME，默认 ~/.config/gld） [env: GLD_HOME=]
      --no-color        关闭彩色输出（也可设置环境变量 NO_COLOR）
  -h, --help            Print help
  -V, --version         Print version
```

## gld set 支持的字段

```text
字段                      取值                                                         说明
name                      文本                                                         显示名称
path                      已存在的目录                                                 项目根目录；换目录后服务会重启到新目录（旧目录里的历史档案留在原地）
tool-profile              compact | core | advanced | read-only | compat-readonly-all  暴露给客户端的工具集（compact 为稳定聚合 API；core / advanced 保留兼容旧工具名）
permission-mode           trusted | dangerous                                          工具权限模式；两个值现在行为完全一样（留着是为了老配置照样能读），见 docs/concepts.md
history-recording         true | false                                                 是否允许把会话检查点写入 docs/history-session
history-context           逗号分隔的编号，或空                                         新会话注入哪些历史档案（有界快照）
allowed-commands          逗号分隔                                                     在默认白名单之外追加的命令（如 rg、gh）；写成 only:cargo,git 则表示只允许这些。gh 只开只读诊断子命令，ssh 加进来等于放开任意远端 shell，见 docs/security.md
confine-reads             true | false                                                 读工具只许读 Workspace 内（默认 true；关掉才能读隔壁仓库等外部路径）
executable-paths          路径列表（换行或分号分隔）                                   额外的可执行文件搜索路径
ai-instructions           文本                                                         注入 Agent 的工作区级说明
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

用法：gld set <项目> tool-profile=read-only allowed-commands=rg,gh
服务本身的端口、认证、公网入口不在这里：gld upgrade --port / --auth，gld share --tunnel
```

## gld secret keys 密钥名一览

```text
凭据名                       属于        用途
oauth_password               服务        OAuth 授权页输入的口令
oauth_client_id              服务        OAuth 静态 Client ID（ChatGPT 这类自动注册的客户端用不到）
oauth_token_secret           服务        签发访问令牌的密钥；换了所有已授权的客户端都要重新授权
bearer_token                 服务        认证方式为 bearer 时客户端携带的 Token
cloudflare_token             服务        Cloudflare 固定域名的 Tunnel Token（只能 set，不能 regen）
actions_api_key              项目（-w）  Actions 认证方式为 api_key 时的 Key
actions_oauth_client_secret  项目（-w）  Actions OAuth Client Secret
actions_oauth_password       项目（-w）  Actions OAuth 授权口令
actions_oauth_token_secret   项目（-w）  签发 Actions Token 的密钥
actions_cloudflare_token     项目（-w）  Actions Named Cloudflare Tunnel 的 token
actions_frp_token            项目（-w）  覆盖 Actions 隧道使用的 frps token

服务的：gld secret ls|set|regen <KEY>     项目的 GPT Actions：gld secret ls|set|regen <KEY> -w <项目>
```

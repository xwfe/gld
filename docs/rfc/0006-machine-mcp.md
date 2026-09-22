# RFC-0006：本机装好的 MCP server 经服务转给 AI

日期：2026-09-22。状态：**已实施**（跨仓 [v4 方案](https://github.com/xwfe/toexec/blob/main/docs/plan/implementation-plan-v4-machine-skills-mcp.md)第 2 步）。实测脚本和结果在 toexec 的 [`evidence/v4-mcp/machine-mcp/`](https://github.com/xwfe/toexec/tree/main/evidence/v4-mcp/machine-mcp)。

## 1. 要解决什么

用户的话："gld 或 ccnm 都可以使用 agent 和 runtime 机器上的已经安装的 skills 和 mcp……最好为 gld 完善一套合理的 mcp/skills 工具来专门操作它们……runtime 传递过程是否会丢信息，数据量是否过大……代码模块化，解耦，如果后期这块功能不需要，能简单的删除"。

ChatGPT 这类 Web AI 只能连一个公网 MCP 地址，用不上本机 Claude Code、Codex 里装好的 context7、deepwiki、exa。gld 本来就是那个地址，这次让它把这些 server 也转过去。

## 2. 怎么用

```bash
gld mcp ls                        # 装了哪些（读 ~/.claude.json 和 ~/.codex/config.toml）、开了哪些
gld mcp test context7             # 在守护进程里起一次：起不起得来、有哪些工具
gld mcp on context7 deepwiki      # 开
gld mcp off context7              # 关（--all 全关）
```

开了之后，连上服务的 AI 多三个工具（一个都没开时不出现）：

| 工具 | 做什么 |
| --- | --- |
| `list_mcp_tools` | 不带参数：开着的 server 和状态（没起 / 在跑 / 配置有问题）；带 `server`：它的工具、参数表和它自己的说明；再带 `tool`：一个工具的完整定义 |
| `call_mcp_tool` | `server`、`tool`、`arguments`，结果原样交回 |
| `read_mcp_result` | 一个结果太大、分段交的，用它读后面的段 |

三个都不带 `workspace`：这些 server 属于这台机器，不属于哪个项目。

## 3. 决定和理由

**读现成的配置，不另建清单。** 用户已经在 Claude Code、Codex 里配过一遍，让他在 gld 里再抄一份，两边迟早对不上。只读 user 级：`~/.claude.json` 顶层的 `mcpServers`、`$CODEX_HOME/config.toml`（默认 `~/.codex/config.toml`）的 `[mcp_servers.*]`；项目级的只在那个项目里生效，算不上"装在这台机器上"。同名时 `~/.claude.json` 那份生效，`gld mcp ls` 会说出另一份（内容一样的不提：开发机上 15 个同名里 13 个一样）。`${VAR}` / `${VAR:-默认值}` 照 Claude Code 的规矩展开；Codex 的 `bearer_token_env_var`、`env_http_headers`、`enabled_tools` / `disabled_tools`、`startup_timeout_sec`、`tool_timeout_sec`、`cwd` 都照它的意思用。

**三个工具去操作它们，不把每个 server 的工具平铺进工具表。** 平铺要在 `tools/list` 时把开着的 server 全拉起来（冷启动慢、一个起不来拖累整张表），工具表也跟着胀（实测 playwright 一家 25 个工具 21 KB、github 17 KB），而 ChatGPT 只在连上时读一次工具表，每开一个新 server 都得去重建连接。代价是模型要多问一次 `list_mcp_tools`，ChatGPT 的确认框上看到的是 `call_mcp_tool` 而不是具体工具名。

**默认一个都不开，按名字开。** gld 的服务可能挂在公网上，装好的 server 里有能读写整个主目录的（Filesystem、desktop-commander、playwright）。方案里原来写的是"默认只放网络类"，实测做不到：context7 在这台机器上是 `npx` 起的本机进程，跟 Filesystem 在配置里长得一模一样，gld 分不出谁只走网络。与其猜，不如让人点名。这和 ccnm 的"默认全开"不同——ccnm 的会话在受管机器上、不挂公网。

**来源配置里的"关"只提示、不拦。** Codex 的 `enabled = false`、`~/.claude.json` 里的 `disabled: true`，`gld mcp ls` 会写出来，但开不开看 gld 自己的名单：你在 gld 里点名开了就是开了。

**工具集 read-only 的服务一个都不转。** 转过去的工具能做什么由 server 决定，只读管不住它；`compat-readonly-all` 照它的本意把这三个的标注也改成只读。

**连接按"server + 调用方"分，用到才开，闲 5 分钟收（每分钟看一次），停服务全收。** 和远端项目的 bridge 同一套规矩：playwright 这种有状态的 server，两个 OAuth 客户端共用一条就能看到对方的页面；`noauth` 和共用一条 bearer 令牌的算同一个调用方。stdio server 起在自己的进程组里，关的时候先关 stdin 等 3 秒，再连同它起的子进程一起杀（`npx` 起的 server，干活的是它下面那个 `node`）。

**起 server 的环境。** `PATH` 是 `gld cfg runtime --executable-paths` 加上守护进程自己的，和 `exec_command` 一个口径；工作目录是主目录（Codex 写了 `cwd` 的用它，相对路径按主目录算）。守护进程由 launchd / systemd 拉起时 `PATH` 常常只有 `/usr/bin:/bin`，`npx`、`uvx` 找不到——所以 `gld mcp test` 在守护进程里跑，测的就是服务用的那个环境。

**协议。** 握手报 2025-06-18，server 回 2024-11-05 到 2025-11-25 之间的都接（实测本机 12 个里 2 个还是 2024-11-05）。stdout 上不是 JSON 的行跳过（官方 TypeScript SDK 也这么做）。server 反过来问的只答 `ping`。HTTP 用 streamable HTTP：带会话号和协议版本头，回复是 JSON 或 SSE 都能读；**总带 User-Agent**（exa 不带就被 Cloudflare 回 403）；远端地址照 gld 的全局出站代理，**本机地址一律不走代理**（开着 `HTTP_PROXY` 时连 `127.0.0.1` 会被送进代理，回 502）；401 报"要登录"。老的 HTTP+SSE 传输不支持。

## 4. 会不会丢信息、数据量大不大

| 环节 | 上限 | 实测 | 超了怎样 |
| --- | --- | --- | --- |
| 工具描述里的目录 | 只列开着的 server 名字和类别 | — | — |
| `list_mcp_tools server=…` | 64 KiB | 最大的 Filesystem 20 KB（gld 的结果信封里 JSON 算两遍） | 先去掉参数表，再去掉描述，说明去掉了什么；`tool=<名字>` 拿单个的完整定义 |
| server 的说明 | 4 KiB | DeepWiki 3 KB | 截断并写明 |
| 一次调用直接交回的文字 | 64 KiB | deepwiki `read_wiki_contents` 407 KB | 交前 64 KiB（尽量断在换行），末尾写明用 `read_mcp_result` 从哪接着读 |
| 留着分段读的全文 | 单条 16 MiB，共 64 MiB，10 分钟 | — | 单条超 16 MiB 的只留前面，并写明后面没了；总量超了先扔最早的 |
| 重复的 `structuredContent` | 有文字时不带 | deepwiki 那次 420 KB 和正文一模一样 | 只有它没有文字时，转成文字交 |
| 图片、音频 | 单个 5 MiB（base64） | — | 换成一句"放不下，多大" |
| 一条消息 | 32 MiB | — | 这次调用报错，不交半截 |

实测经 gld 调 deepwiki 那次：客户端第一次收到 67.9 KB，再调两次 `read_mcp_result` 拿回全部 406,840 字节，一个字节不少。

**会丢的，都说出来**：超过 16 MiB 的结果的后半截；超过 5 MiB 的单张图片；10 分钟没读完的段；老 SSE 传输的 server、要 OAuth 登录的 server 整个用不了。

## 5. 不要了怎么删

| 删什么 | 在哪 |
| --- | --- |
| 整个目录 | `crates/core/src/machine_mcp/` |
| 服务里的三处 | `hub/mod.rs` 里 `machine_mcp` 的列工具、分发、关服务（`relay`、`installed` 两个字段和 `relay_definitions` / `call_relay`） |
| 设置 | `AppSettings::relayed_mcp_servers`、`AppData::relayed_mcp_servers` |
| 命令 | `crates/core/src/app/mcp.rs`、`crates/cli/src/commands/mcp.rs`、`cli.rs` 的 `Mcp`、守护进程的 `McpServers` / `SwitchMcpServers` / `TestMcpServer`、`scripts/gen-cli-docs.sh` 那一行 |
| 依赖 | 对 `toexec-mcp` 的依赖（它还要留给 ccnm 用，别删 toexec 那边的 crate） |

别的模块不依赖这里。数据文件里留下的 `relayed_mcp_servers` 键，老版本读的时候忽略。

## 6. 没做的

- upstream server 的 resources、prompts 不转，只转工具。
- 要 OAuth 登录的 server、老的 HTTP+SSE 传输。
- 按项目、按客户端分别开：只有服务一份名单。
- 按工具名藏：用来源配置自己的 `enabled_tools` / `disabled_tools`（Codex 的写法），gld 不另设一套。
- Windows 上没有进程组，关 server 靠杀进程树（没在 Windows 上跑过）。

## 7. 后来：共用代码进了 toexec（2026-09-22，v4 第 3 步）

这一步刚做完时共用代码没进 toexec：toexec 的规矩是"两个产品都真的在用才进来、不加依赖"，而那时只有 gld 读这两份配置。第 3 步 ccnm 也要同一套（它在项目那台机器上转 server，外加项目的 `.mcp.json`），就把读配置、握手调用、子进程通道、连接池、结果整理整块搬成了 `toexec-mcp` 0.1.0，gld 删掉自己那份改链它；toexec 那条"不加依赖"为它破了例（serde_json、toml），理由写在 toexec 的开发规矩里。gld 留下的是自己的决定：`machine_mcp/open.rs`（`PATH`、工作目录、进程组、怎么杀）、`http.rs`、`relay.rs`（三个工具、结果交法、留着分段读的全文）。行为没变；跟着代码搬走的测试在 toexec 里接着跑。v4 第 4 步 ccnm 在 Agent 上转 server 也要"留着分段读的全文"和"拆 SSE 回复"这两块，于是又搬成了 `toexec-mcp` 0.2.0 的 `kept`、`sse`，gld 的 `relay.rs`、`http.rs` 改用它们（`6d3acc1`），行为同样没变；`http.rs` 发请求那一半（reqwest、代理）仍是 gld 自己的。

同一天 hub 加了 `remote_call_mcp_tool`：远端 ccnm 项目那台机器上的 server 经它用，和 `remote_exec_command` 一样要 coding 句柄。远端没有可转的 server 时 ccnm 不列那个工具，hub 这边报"那边没东西可转"，不报"升级 ccnm"。

## 8. 验证

- `cargo test --workspace` 781 passed / 0 failed（原 741；新增 40 条：读配置 6、通道 4、握手与调用 6、HTTP 4、连接池 5、三个工具 10、服务 3、`gld mcp` 2）；fmt、clippy `-D warnings`、`cargo +1.89 check` 通过。搬进 toexec 之后是 764 passed / 0 failed：搬走的测试在 `toexec-mcp` 里（30 条），这边新增 `open.rs` 3 条、hub 的远端工具 2 条、连真实 ccnm 的组合测试 1 条。
- 真机（开发机，隔离的 `GLD_HOME`、端口 28990）：context7（`npx`）、mcp-time（`uvx`）、Filesystem（`npx`）、deepwiki、exa（远端 HTTP）经服务都调得通；deepwiki 407 KB 分三段读全；停服务后 server 进程连同下面的 `node` / `python` 一个不剩；不停服务、也不再调用时，最后一次调用后 6 分钟（闲 5 分钟 + 每分钟扫一次）它们也全收掉了。`gld mcp test` 在守护进程里对 context7、deepwiki、Puppeteer（2024-11-05）都成功，对一个程序不在的报出找的是哪条 `PATH`。
- `docs/cli.md` 用 `HOME` 指到空目录跑 `scripts/gen-cli-docs.sh` 重新生成（绕开开发机上跑着的旧守护进程），只多了 `gld mcp` 那几节。

**没验的**：真实 ChatGPT 会不会先列再调、会不会照说明读后面的段（要额度和公网入口）；Linux、Windows；真实的 OAuth 登录墙（只有测试里的假 401）。

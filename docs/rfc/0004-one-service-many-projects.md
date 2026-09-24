# RFC-0004：只留多项目模式

日期：2026-09-22。状态：**已实施**（用户 2026-09-22 定："gld 只需要存在多项目模式，默认就是；默认 start 就是多项目模式，不需要显示 hub 命令；合理调整现有命令，让增删改查简单"）。

## 1. 结论

gld 对外只有**一个 MCP 服务**：一个端口、一套凭据、一个公网入口，项目都挂在它下面，AI 每次调用带 `workspace` 参数选项目。这就是原来的聚合入口（hub），内部名字不改，命令行里不再出现 `hub` 这个词。

以前的两条路——"一个项目一个服务"和"hub 聚合几个项目"——合成这一条。单项目的 MCP 服务不再有命令能起。

## 2. 命令怎么变

增删改查都是顶层的一个词：

| 做什么 | 现在 | 以前 |
| --- | --- | --- |
| 起服务 | `gld start [目录]`：起服务；目录（不给就是当前目录）没登记过就顺带加进来 | `gld start` 起这一个项目的服务；`gld hub start` 起聚合入口 |
| 加项目 | `gld add [目录…]`：登记即加入，服务在跑就立即生效 | `gld ws add` 登记 + `gld hub add` 加入，两步 |
| 看 | `gld ls`：服务地址、凭据、公网入口、项目表；`gld ls <项目>` 看一个项目的配置 | `gld list`、`gld ws list`、`gld ws show`、`gld hub show` 四处 |
| 改项目 | `gld set [项目] key=value`（字段 `gld fields`） | `gld ws set` |
| 改服务 | `gld upgrade --port/--auth/--tool-profile/--tunnel/--off` | `gld hub set …` |
| 删项目 | `gld rm <项目…>`：删 gld 这边的配置，项目文件不动；远端项目也用它删 | `gld destroy`、`gld ws rm`、`gld hub rm`、`gld hub remote rm` |
| 公网 | `gld share [--tunnel …]`：给**服务**配公网入口并起起来 | 给一个项目起一条隧道 |
| 停 | `gld stop`：停服务（项目、配置、凭据都留着） | 停一个项目的服务 |
| 凭据 | `gld secret ls / set / regen`：服务的凭据 | 项目的凭据、共享池 |
| 远端项目 | `gld remote add / rm` | `gld hub remote add / rm` |
| 全局设置 | `gld settings`，短写 `gld cfg` | `gld settings` |

"显示"类子命令统一叫 `list`（短写 `ls`）：`settings ls`、`secret ls`、`planning ls`。原来的 `show` 还认，只是不在帮助里出现。

**旧命令都还能敲**（不进帮助）：`gld ws …`、`gld destroy`、`gld hub …`、`gld ps`、`gld tunnel …`、`gld gateway …`、各处的 `show`。写在脚本里的不会一升级就断；`ws` 那组照新语义走（`ws add` 也会加入服务）。

## 3. 几个取舍

**登记 = 加入。** 以前一个目录可以"登记了但不在 hub 里"。只剩一个服务之后，这种状态只会让人困惑（"我加了它，AI 为什么看不见"），所以 `add` / `start <目录>` 登记完就加入，`rm` 一起删。老数据里登记了没加入的，`gld start` 时补加并逐个打出名字——不在后台悄悄扩大 AI 能碰的范围。

**服务自己的公网入口。** 以前 hub 上公网只有两条路：经全局入口（`/hub/mcp`，而全局入口的 Cloudflare 只有临时地址）或自己反代。现在服务有自己的隧道，写法和原来项目的 `--tunnel` 一样：`cf`（临时地址）、`cf:<域名>`（固定域名，要 Tunnel Token）、`frp:<配置名>`、`https://…`（已有公网地址）、`off`。公网地址就是 `<入口>/mcp`，Cloudflare 回源端口就是服务端口。全局入口那条路老配置还能用，不再出现在帮助里。

`gld share` 不带 `--tunnel` 时**沿用已经配好的入口**，一个都没配才用 Cloudflare 临时地址。以前每次都改成临时地址——对一个配了固定域名的服务，那等于顺手把固定地址抹了。

**Actions（自定义 GPT）还是一个项目一个。** 聚合入口没有 OpenAPI 版，自定义 GPT 导入的也是一个项目的 OpenAPI 文档。所以 `-s actions` 留在 `start` / `stop` / `restart` / `share` / `logs` 上，照旧按项目起。不用它的人看不到任何区别。

**项目字段里去掉了单项目服务的那一半。** `port`、`auth`、`tunnel`、`public-url`、`frp-*`、`cloudflare-mode`、`shared-secrets`、`global-gateway`、`oauth-client-id`、`use-proxy` 这些是"这个项目自己那个监听器"的配置，监听器没了它们就没有作用。`gld set` 遇到它们直接报错并指出服务级的对应命令，而不是收下一个不起作用的值。`actions.*` 照旧。

## 4. 代价（和跨仓评审 X07 的关系）

跨仓评审 X07 说"hub 默认化要以项目级授权为前置"：拿到服务凭据的人能进**全部**项目，而项目级授权（给某个客户端只开某几个项目）还没做。这次是用户明确要求先收敛命令，所以代价照实写进文档：

- 一把钥匙开所有项目的门。只想单独给出去的项目，现在没有办法单独给——别加进来。
- 挂公网时仍然拒绝 `noauth`（沿用 hub 的规则）。
- 项目级授权（X07 的 grant / scoped view）仍是 L1 没做完的那一半，见[路线](../reviews/2026-09-19-hub-development-roadmap.md)第 7 节。（2026-09-24 由 [RFC-0007](0007-scoped-grants.md) 补上：grant 绑在凭据上，只开部分项目或只读。）

单项目监听器的内部代码（`mcp/listener.rs` 的工作区路径、工作区隧道、全局入口的 `/w/<id>` 路由）这次没删：Actions 还用着工作区隧道那一套，而删掉它们要先确认没有别的消费者。命令行已经起不了它们。

## 5. 实施记录（2026-09-22）

**core**

- `HubConfig` 加服务自己的隧道字段（`tunnelType` / `cloudflareMode` / `frpProfileId` / `frpSubdomain` / `useProxy`）；隧道进程跟着监听器起停。和全局入口共用一份"不属于任何工作区的隧道"代码（`tunnel/standalone.rs`），全局入口那段是机械搬过去的。
- 隧道起不来不拦服务：本机客户端照样能用，原因记在运行时，`gld ls` / `gld status` 显示，`gld share` / `gld start --tunnel` 报非零退出。Cloudflare 临时地址跑着的时候以运行时拿到的为准，配置里推不出来。
- 登记即加入（`create_workspace` 里加进成员表）；`join_all_workspaces` 给老数据补加。
- 服务凭据可以 `set`（以前只能看和重新生成）；`cloudflare_token` 只能 set、不能 regen。
- `ensure_hub_started`：没在跑才起，跑得好好的什么都不做。`gld start` 走它——以前 `HubStart` 的语义是"按当前配置重启"，重复敲一次 `start` 就掉一次客户端连接、换一次临时地址。
- 改配置后重启失败时报"新配置已经保存，但服务重启失败、现在是停的"。起、停、重启加了一把锁，并发的 `gld start` 不会在"旧的停了、新的还没起"那一刻抢端口。
- `gld doctor` 换口径：服务一组（认证、公网入口、有没有项目不在服务里、端口），项目只查目录、Git 和在用的 GPT Actions；项目自己那个 MCP 端口没在跑就不查。
- 项目字段表去掉单项目 MCP 监听器的 11 个字段，`set` 遇到它们报错并指出服务级命令；FRP 配置删除检查改成看服务的入口和项目的 Actions。
- 守护进程协议版本 2 → 3：加了 `hub_ensure_started`、`set_hub_secret`、`hub_join_all`、`hub_health`、`hub_usage`、`hub_logs`。不递增的话，升级二进制后旧守护进程还在跑时报的是"看不懂的请求"而不是"请 gld daemon restart"。

**命令行**：见第 2 节。`gld ls` 把登记了但不在服务里的项目标成"不在服务里"，不藏起来。

**测试**：CLI 集成测试原本按"一个项目一个 MCP 端口"写（`ws add --mcp-port`、`ws set auth=…`、直连项目端口），66 条照新语义改：测的关注点不变（认证、读限制、日志、用量、OAuth、就绪、并发、改配置即生效），入口换成服务、调用带 `workspace`。只对单项目监听器成立的断言（项目 token 和服务 token 互不通、项目自报的 `serverInfo.name`）改成服务上的对应物。新增：服务自己的 Cloudflare 临时 / 固定域名 / FRP 隧道（假 cloudflared、假 frpc）、在非项目目录 `gld start` 不登记、老数据的项目标出来并由 `start` 补加、重复 `start` 不重启。

**顺手查出的一个老问题**：测试辅助 `free_port()` 在测试进程里 bind 一下确认空闲，macOS 上这个监听 socket 会在 close-on-exec 补上之前被并发 spawn 的 `gld` 继承，隔壁测试的端口于是被无关的守护进程占着（并发三十次左右红一次）。改成"连一下"，单独提交。

验证：`cargo test --workspace` 738 passed / 0 failed；fmt、clippy（`-D warnings`）、`cargo +1.89 check --locked` 干净；`docs/cli.md` 由脚本重新生成。另在隔离的 `GLD_HOME` 里手动走了一遍 start / add / ls / set / upgrade / secret / rm / share / doctor / health / usage / logs，并用 curl 经服务调 `list_workspaces`。**没验的**：真实 Cloudflare / FRP 账号上的隧道（只用了假二进制）、真实客户端连接。

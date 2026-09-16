# V2-H 远端链的现场证据

H1 用合成 peer 和本机真 ccnm；H2 只读链和 H3 写链都跨两台真实机器。

2026-09-16。走的是真的 HTTP MCP 客户端 → 真的 bearer 鉴权 → 真的
`ccnm mcp bridge` 子进程，不是单元测试里的合成通道。

复现环境隔离在 `GLD_HOME` 下的一个临时目录里，没有碰用户自己的 gld 数据，
也没有改 `~/.config/ccnm/config.toml`。

## 配好长什么样

```
$ gld hub add api
$ gld hub remote add prod --node work --remote-workspace server
$ gld hub set --auth bearer && gld hub start
$ gld hub show
成员（2）
名称  ID        工具集 / 模式  路径 / 位置
api   b5753bd6  compact        /…/scratchpad/localproj
prod  d053616d  read（远端）   work:server
```

远端那行没有本机路径：那是对面机器上的目录，gld 不知道，编一个比留空更糟。

## 客户端看到什么

`tools/list` 里多了四个远端工具，本地工具一个没少：

```
远端工具: ['remote_workspace_info', 'remote_read_file',
          'remote_list_files', 'remote_search_text']
本地 read_file 还在: True
```

`list_workspaces` 分得清两种成员：

```json
[ { "id": "b5753bd6…", "name": "api",  "kind": "local",
    "path": "/…/localproj", "tool_profile": "compact" },
  { "id": "d053616d…", "name": "prod", "kind": "remote", "mode": "read",
    "tools": ["remote_workspace_info", "remote_read_file",
              "remote_list_files", "remote_search_text"] } ]
```

## 参数白名单是真的在拦

调用时故意多带两个 ccnm 那边**真实存在**的参数：

```
remote_read_file  workspace=prod path=src/main.rs max_lines=20
                  max_bytes=99 end_line=3
```

远端收到的（桩把 arguments 原样回显在 `echoed_args` 里）：

```json
"echoed_args": { "max_lines": 20, "path": "src/main.rs" }
```

`workspace` 是 gld 的路由字段，摘掉了；`max_bytes` 和 `end_line` 这边没声明，
**没往外发**。这条对应验收项 H03「新增上游工具不自动放行」的参数那一半。

## 用错工具的两个方向都不执行

```
read_file        workspace=prod  → TOOL_IS_LOCAL_ONLY
remote_read_file workspace=api   → TOOL_IS_FOR_REMOTE_WORKSPACES
```

两条都带着「那该用哪个」。第二条的返回里搜不到本地文件内容，证明它没有
顺手在本机读一遍。

## 连接生命周期

三次远端调用，`pgrep` 只看到**一个** bridge 进程：

```
34028 …/ccnm mcp bridge server --node work --mode read
```

argv 完全来自配置，没有 shell 拼接。`gld hub stop` 之后：

```
停掉之后还剩的 bridge 进程数: 0
```

关的方式是先关 stdin 让对面读到 EOF，不是 kill——ccnm 的 `mcp-serve` 读到
EOF 才会正常释放 Runtime 那边的写锁。

## 日志里有鉴权方式，没有凭据

```
[rpc] request id=7 method=tools/call tool=remote_read_file auth=bearer:hub
```

远端成员自己的日志里也有一份，带 `[hub]` 前缀：

```
[hub] [rpc] completed id=7 method=tools/call tool=remote_read_file …
```

## 起不来的时候说人话——这段是真 ccnm

把桩从 PATH 里拿掉，让 gld 去调本机真正的 `ccnm`（0.7 系列）：

```json
{ "code": "REMOTE_BRIDGE_CLOSED", "retryable": true,
  "message": "remote_workspace_info on remote workspace prod: the bridge
              closed the connection during initialize; it said:
              CCNM_E_CONFIG:\nno node named work in this machine's config" }
```

**这一条是真实的 ccnm 二进制答的。**它证明了三件事：gld 拼的 argv 真 ccnm 认；
ccnm 的 `CCNM_E_*` 诊断只写在 stderr 上，而 gld 的有界 stderr 收集把它原样带到了
MCP 客户端手里；连不上和工具报错分得开（前者是 gld 的错误码，后者会原样透传
远端的 `isError`）。

没有这段 stderr，客户端只会看到「连接断了」，而真正的原因——配置里没有叫 `work`
的 node——谁也查不出来。

---

# H2：跨真实网络的只读链

同日晚些时候补上。这一段**没有桩**：真 SSH、真两台机器、真文件。

## 拓扑，以及怎么绕开角色冲突

| 机器 | 本来的角色 | 探针里当什么 |
| --- | --- | --- |
| 这台 Mac | ccnm 的 **Runtime**（项目都在它上面） | MCP **Host**：gld hub 跑在这儿 |
| fodelf | ccnm 的 **Agent Node**（跑 Claude） | **Runtime**：放着被读的那个项目 |

两台机器原有的角色**一个字都没改**。`ccnm workspace add` 在 fodelf 上直接被拒：

```
CCNM_E_CONFIG: runtime_node says this machine keeps no workspace list,
但 [workspaces.*] is not empty
a project's root is defined on exactly one machine
```

这条拒绝是对的——fodelf 作为 Agent Node 把 workspace 列表委托给了 Runtime，
不能同时自己有一份。所以探针走**另一份配置文件**（ccnm 支持 `CCNM_CONFIG`）：

- fodelf：新建 `~/.config/ccnm/gldprobe.toml` 和包装脚本 `~/.local/bin/ccnm-gldprobe`
  （`export CCNM_CONFIG=…gldprobe.toml; exec ccnm "$@"`）。原来的 `config.toml`
  md5 前后一致，另外还留了一份 `.bak-gld-<时间戳>`。
- 这台 Mac：`GLD_HOME` 下的临时目录 + 一个同样形状的 Host 侧包装脚本，用
  `gld hub remote add --ccnm <包装脚本>` 指过去。`~/.config/ccnm/config.toml` 没碰。

**踩到的坑**：包装脚本一开始写的是 `ccnm --config X "$@"`，握手直接被拒
`workspace gldprobe is not available to external MCP`。因为
`Server::open_external` 走的是 `paths::effective_config_path()`，读的是**环境变量**
`CCNM_CONFIG`，不是 CLI 的 `--config` 旗标。改成 export 就通了。

## 配置

```bash
$ gld hub remote add fodelf --node remote --remote-workspace gldprobe \
      --ccnm /…/scratchpad/ccnm-host
$ gld hub show
名称    ID        工具集 / 模式  路径 / 位置
api     b5753bd6  compact        /…/localproj
fodelf  421ac212  read（远端）   remote:gldprobe
```

## 四个只读工具，全部在真机上

HTTP MCP 客户端 → gld hub（bearer）→ `ccnm mcp bridge` → ssh → fodelf：

```
=== workspace_info  (465 ms, isError=False)
   workspace gldprobe (not a git repository, macos/aarch64); …
   [server pid 95752, call 1]

=== read_file  (60 ms)
   1→fn main() {
   2→    println!("hello from fodelf");
   3→    // TODO: 这一行是给 remote_search_text 找的
   4→}
   [end of file, 4 lines; version 104-18d5b616b80e1b25]

=== list_files  (59 ms)
   README.md / notes.txt / src/        [3 entries in ., 1 directory]

=== search_text  (118 ms)
   src/main.rs  3:    // TODO: 这一行是给 remote_search_text 找的
   [1 match in 1 file]

=== 分页 read  start_line=2 max_lines=1  (34 ms)
   2→line two
   [stopped at max_lines=1; continue with start_line=3; …]
```

`macos/aarch64` 和 `hello from fodelf` 都是对面机器上的事实，这台 Mac 上没有这个
目录——所以这条链确实跨了网。

## 越界路径：ccnm 的拒绝原样到客户端

```
remote_read_file  path=../../etc/passwd   →  isError: true
CCNM_E_POLICY: ../../etc/passwd contains `..`; ccnm only reads inside the workspace
```

gld 没有改写它，也没有翻译成自己的错误码——远端工具说这次没成，就原样传出去
（验收项 H04）。

## 一条连接服务了全部调用

ccnm 自己在每条结果末尾报服务进程的 pid 和这条连接上的第几次调用：

```
[server pid 95752, call 1]   …   [server pid 95752, call 8]   [server pid 95752, call 9]
```

pid 不变、序号连着涨，跨了好几个独立的 HTTP 请求。延迟也看得出来：第一次
465 ms（SSH 握手 + MCP 握手），之后 34–60 ms。

本机这边 gld 守护进程的子进程只有一个——`ccnm mcp bridge` 会 `exec` 成 ssh 本身，
不留中间进程：

```
32108 /usr/bin/ssh -o SendEnv=-* -o SetEnv=CCNM_TRANSPORT=1 -o ForwardAgent=no
                   -o ClearAllForwardings=yes -o BatchMode=y…
```

## 关掉之后两边都干净

```
$ gld hub stop
本机残留 ssh 子进程: 0
fodelf 上的 internal mcp-serve: count: 0
```

关的路径是 gld 关 stdin → ssh 收到 EOF 退出 → fodelf 上的 `mcp-serve` 读到 EOF
正常收尾。不是 kill，所以远端不会留下写锁的 `held` 标记。

## 顺手发现的一条 ccnm 问题

第一次握手被拒的原因是这个（**只读模式**下）：

```
CCNM_E_POLICY: the runtime is running as fodelf and is not confined,
so exec_command is refused: …
fix: remove the runtime account from those groups / use a Runtime identity
     that holds no outbound SSH credential / …
```

只读会话根本没有 `exec_command`。这道闸在 `crates/ccnm-core/src/mcp/server.rs`
的 `Server::new`（约 300–305 行）无条件执行，而紧接着 306–309 行的注释恰好说明
作者想过只读会话（「不拿写锁，因为这个会话没有任何会改文件的工具」）。

结果是：想开一个只读远端 workspace 的人，收到的是一屏 exec 和凭据的告警，
并被告知去设 `allow_unconfined_exec`——那个名字听起来比他实际在做的事危险得多。
这是 ccnm 那边的事，不在本次范围内，记在这里免得丢。

---

# H3：跨真实网络的写链

同日。拓扑和 H2 一样，只把探针配置里那一行 `external_mcp` 从 `read` 改成
`coding`（验完改回去了），成员改成 `gld hub remote add … --mode coding`。

## 一轮完整的写

```
begin           (455 ms)  rc-5ea24a6655864fce9d46eff13cfdf4bb
apply_patch      (42 ms)  add from-gld.txt (28 bytes) version 28-18d5b7a05d771933
                          [1 file changed]
exec_command     (44 ms)  $ echo hello from a remote coding session
                          ok in 6 ms, 35 B stdout, 0 B stderr
                          hello from a remote coding session
                          [output_ref r-9ed62f4fe0ee4a48]
read_file       (456 ms)  1→written through the gld hub
end              (77 ms)  closed
再用同一个句柄    (17 ms)  REMOTE_CODING_HANDLE_UNKNOWN
```

`from-gld.txt` 是真的落在 fodelf 上的：`ls ~/gld-remote-probe` 能看到它。

**那个 456 ms 是证据不是噪声。**同一轮里 apply_patch 和 exec 都是 40 ms 上下，
偏偏读文件花了 456——因为只读工具走的是**另一条**连接，那一下是新开了一条
SSH。这正是"read 和 coding 是两条独立连接"的现场证明；合成一条的话，这次
只读调用就会把写会话连同 `output_ref` 一起挤掉。

## output_ref 的分页和归属

用一条产量大点的命令（`/bin/sh -c "seq 1 200"`，692 字节）验分页：

```
read_output offset=0  limit=40  →  1..16 和半个 17，[40 of 692 bytes; continue with offset=40]
read_output offset=40 limit=40  →  接着那半个 17，[80 of 692 bytes; continue with offset=80]
```

偏移是字节偏移而且稳定：第二页从上一页断开的那半个数字接着来。

归属也对：

```
拿另一个句柄读同一个 ref   →  REMOTE_CODING_HANDLE_UNKNOWN
会话关掉之后再读那个 ref   →  REMOTE_CODING_HANDLE_UNKNOWN
```

> 顺带说明一件事：那条命令写成 `["/bin/sh", "-c", "seq 1 200"]` 才跑得起来。
> ccnm 的 `cmd` 是 argv，想要 shell 必须自己显式写出来——**gld 绝不替你加**。
> 这就是验收项 H04「绝不将 gld 字符串 cmd 当 ccnm argv」在现场的样子。

## 写锁被别人占着

手工另起一条 `ccnm mcp bridge … --mode coding` 占住远端写锁，再让 gld 去
`remote_coding_begin`：

```
code      : REMOTE_WRITE_LOCK_BUSY
retryable : True
message   : … CCNM_E_POLICY:
            workspace write guard is busy; another session still owns this working tree
            who holds it, on the Runtime Node: the `held <session> <work…
```

分类对上了，ccnm 自己那句"谁占着、去哪儿看"也原样带到了调用方。同一时刻
**只读照常通**——ccnm 的 read 模式不碰这把锁（协议 4.4）。

状态说不清的那一种（`REMOTE_WRITE_LOCK_UNKNOWN`，不可重试）没有在真机上
造过：那要人为弄坏锁文件或者杀掉一个持有者留下 `held` 标记，是在别人机器上
制造故障，没做。它的分类由单元测试按 ccnm 源码里的原话钉住。

## exec 的安全提示原样透传

远端每次 exec 都在结果里附一段：

```
[this runtime is NOT confined (running as fodelf) and this workspace has
 allow_unconfined_exec set; a command here has the access that account has;
 it also has allow_unisolated_credentials set, so that account can read a
 known Agent login and so can anything the model runs]
```

gld 一个字没改。这句话是给模型和看日志的人看的，吞掉它等于替远端隐瞒风险。

## 清理

探针留下的东西，都可以整个删掉：

- fodelf：`~/gld-remote-probe/`（含这次写进去的 `from-gld.txt`）、
  `~/.config/ccnm/gldprobe.toml`（和它的 `.read-only-bak`）、
  `~/.local/bin/ccnm-gldprobe`、`~/.config/ccnm/config.toml.bak-gld-*`。
  探针配置已经改回 `external_mcp = "read"`。用户自己那份 `config.toml`
  全程没动，md5 前后一致。
- 这台 Mac：只有 `GLD_HOME` 指向的那个临时目录和两个包装脚本，都在 scratchpad 里

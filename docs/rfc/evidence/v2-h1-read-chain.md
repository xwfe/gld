# V2-H1 只读链的现场证据

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

## 还没验到的：一次真实的 SSH 跳

上面这台机器在 ccnm 的配置里是 **Runtime**（`this = "runtime"`，项目都在它上面），
唯一带 `ssh` 别名的节点是 Agent Node。`ccnm mcp bridge` 要的是「从 Host 机器
SSH 到 Runtime」，本机自己连自己需要：

- `ssh localhost` 能免密登录——实测 `Permission denied (publickey…)`，要往
  `~/.ssh/authorized_keys` 里加公钥；
- 对面的 workspace 用 `external_mcp` 显式开放给外部 MCP Host。

第一条是改用户的 SSH 信任配置，第二条是改用户的 ccnm 配置，都不是能顺手做的，
**没有做**。所以 RFC-0002 的 H2（真实只读链）目前停在这里：协议、路由、连接
生命周期和错误分类都验过了，缺的是最后那一跳网络。

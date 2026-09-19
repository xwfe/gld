# V3-G1：远端工具跟上 ccnm P36–P41 的现场证据

2026-09-18。走的是真的 HTTP MCP 客户端 → 真的 bearer 鉴权 → 真的 gld 守护进程 →
**真实 ccnm 二进制**（含 P41 的那份构建）。省掉的只有 SSH 那一跳：这台机器上没有
可 ssh 的 Runtime，所以 `--ccnm` 指向一个包装脚本，由它直接起 ccnm 的 server。

隔离在 `GLD_HOME` 下的临时目录里，没碰用户自己的 gld 数据，也没碰
`~/.config/ccnm/config.toml`（ccnm 那边同样是临时的 `CCNM_CONFIG`、`HOME` 和
`XDG_STATE_HOME`）。

## 怎么配的

`ccnm mcp bridge <ws> --node <n> --mode <mode>` 的参数由成员配置决定，程序本身
可以换（`--ccnm`）。包装脚本把 `--mode` 原样读出来，再起对应模式的 server：

```sh
#!/bin/sh
mode=read
while [ $# -gt 0 ]; do
  case "$1" in
    --mode) mode="$2"; shift 2 ;;
    *) shift ;;
  esac
done
if [ "$mode" = coding ]; then payload=<coding 的 payload>; else payload=<read 的>; fi
export HOME=…; export XDG_STATE_HOME=…; export CCNM_CONFIG=…
exec <真 ccnm> internal mcp-serve --payload "$payload"
```

```bash
gld ws add <本地目录> --name local --mcp-port <free>
gld hub add local
gld hub remote add prod --node runtime --remote-workspace demo \
    --ccnm <包装脚本> --mode coding
gld hub set --port <free> --auth bearer && gld hub start
```

## 客户端看到的远端工具（13 个）

```
remote_workspace_info  remote_read_file   remote_list_files  remote_search_text
remote_load_skill      remote_view_image  remote_read_notebook          ← P36 / P39 / P40
remote_coding_begin    remote_coding_end
remote_apply_patch     remote_exec_command  remote_read_output  remote_stop_command  ← P41
```

## 每一步的实际结果

| 调用 | 耗时 | 结果 |
| --- | --- | --- |
| `remote_workspace_info` | 572 ms | `workspace demo (not a git repository, macos/aarch64)…[server pid 30602, call 1]`——第一次要起进程，之后都是几十毫秒 |
| `remote_load_skill`（不带名字） | 23 ms | `1 skill(s)…- deploy: Deploy the service. Use after the tests pass.`——多行 `description: >` 读对了 |
| `remote_load_skill name=deploy` | 22 ms | 正文，带 `${CLAUDE_SKILL_DIR} is .claude/skills/deploy` |
| `remote_view_image path=shot.png` | 22 ms | 内容块是 `['text', 'image']`：**图片原样过 hub**（H04 的"content 一个字不改"） |
| `remote_read_notebook path=analysis.ipynb` | 25 ms | 内容块是 `['text', 'image', 'text']`，cell 和输出按顺序，输出里的 PNG 在中间 |
| `remote_search_text output_mode=files_with_matches` | 77 ms | `hello.txt` / `[1 file with matches]` |
| `remote_search_text type=txt include_hidden=true` | 58 ms | 带上下文的命中行 |
| `remote_coding_begin` | 399 ms | 句柄 `rc-…`，另开一条 coding 连接（拿远端写锁） |
| `remote_exec_command shell=… run_in_background=true` | 101 ms | `running in the background as output_ref r-0210d81918614b27` |
| `remote_read_output wait_ms=1500` | 1528 ms | `ready on :5173` + `[15 bytes so far and the command is still running…]` + `[running for 1.5 s]`——等满了才回，命令还在跑 |
| `remote_stop_command` | 70 ms | `stopped by stop_command after 1.6 s, 15 B stdout, 0 B stderr` |
| `remote_read_output`（停掉之后） | 18 ms | `[end of stdout at 15 bytes]` + `[stopped by stop_command after 1.6 s]` |
| `remote_read_output wait_ms=600000` | 15 ms | **gld 自己拒**：`ARGUMENT_OUT_OF_RANGE`，`details.max = 50000`，没发给远端 |
| `remote_apply_patch op=add` | 39 ms | `add new.txt (2 bytes) version 2-…`，文件真的建出来了 |
| `remote_coding_end` | 45 ms | `The write lock on that machine is released.` |

## 这几条证明了什么

1. 白名单里的工具名和参数名和真实 ccnm 对得上——参数名写错的话，ccnm 会拒，这里
   一次都没拒。
2. 图片块和 notebook 的多块结果经 hub 原样透传，没被压成文本。
3. 后台命令那一套（起、等、停、读）经 hub 跑通，`wait_ms` 真的在等。
4. 超过 hub 调用预算的 `wait_ms` 在 gld 这边就拦住了，远端连这次调用都没收到。

## 没测的

- SSH 那一跳（本机没有可 ssh 的 Runtime）；跨机的那一半在
  [V2-H 的记录](v2-h-read-chain.md)里，本轮没有重跑。
- 老版本 ccnm 的拒绝路径（`REMOTE_TOOL_UNSUPPORTED`）：真机上装的 ccnm 是 0.7.0，
  但这份构建是本地的新版，所以老版本那条路由**合成 peer 的单元测试**覆盖
  （`bridge::session` 里那两条、`hub` 里那两条）。
- Web AI 那一端（ChatGPT）实际怎么显示图片块、一次调用能等多久：没测。

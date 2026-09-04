# 守护进程（daemon）

## 先说怎么用

平时不需要管它：`gld start` 会在它没跑时自动拉起。要看它在不在：

```bash
gld daemon status
```

```text
状态          运行中
pid           9363
版本          0.3.0（协议 1）
运行时长      2h 13m
运行中的服务  2
数据目录      /Users/you/.gld
socket        /Users/you/.gld/daemon.sock
日志          /Users/you/.gld/logs/daemon.log
```

不在跑时打印一行提示，**退出码 3**——写脚本时用它判断，不用解析文字。

| 命令 | 做什么 |
| --- | --- |
| `gld daemon start` | 后台启动；已在跑就什么都不做 |
| `gld daemon stop [--force] [--wait 20]` | 先停所有服务和隧道，再退出；20 秒没退可加 `--force` 杀进程树 |
| `gld daemon restart` | stop + start，升级 `gld` 二进制后用 |
| `gld daemon run [--no-restore]` | 前台运行，Ctrl-C 优雅退出；给 launchd / systemd 或排障用 |
| `gld daemon logs [-n 50] [-f]` | 它自己的日志（每个请求一行，含耗时） |

## 为什么需要它

MCP 监听器和 frpc / cloudflared 子进程必须活在某个进程里。命令行一执行完就退出，
所以服务不能由命令行本身持有。桌面版靠常驻的窗口进程，命令行版就靠这个守护进程。

命令行和它之间是本机 socket 上的一行 JSON 请求、一行 JSON 响应
（协议见 `crates/daemon/src/protocol.rs`）。可以手工调试：

```bash
printf '{"op":"ping"}\n' | nc -U ~/.gld/daemon.sock
# {"status":"ok","result":"pong"}
```

## 哪些命令会自动拉起它

规则只有一条：**会改变“正在运行什么”的命令需要守护进程**，其余不需要。

| 需要（没跑就自动拉起） | 不需要（没跑时进程内直接执行） |
| --- | --- |
| `start` `stop` `restart` | `workspace *` `settings *` `secret *` `frp *` |
| `tunnel start/stop/restart/test` | `status` `ps` `logs` `health` `connect` |
| `gateway start/stop` | `planning *` `history` `usage` `context` `software *` |

守护进程在跑时，**所有**命令都转发给它——它是内存里运行状态和 `profiles.json` 的唯一写入者，
这样不会出现命令行改了文件、守护进程用旧数据把它覆盖回去的情况。

不想自动拉起（比如 CI 里）：加 `--no-autostart`，需要守护进程时以退出码 3 报错。

## 文件

都在数据目录（默认 `~/.gld`）下：

| 文件 | 作用 | 什么时候会出问题 |
| --- | --- | --- |
| `daemon.sock` | IPC 入口，权限 600 | 数据目录路径超过约 100 字节时 Unix socket 放不下，会改用 `$TMPDIR/gld-<hash>.sock`，`daemon status` 里能看到实际位置 |
| `daemon.lock` | `flock` 排他锁，同一数据目录只允许一个守护进程 | 文件本身一直存在，靠锁而不是靠文件有无判断；不要手动删除正在被持有的锁 |
| `daemon.json` | pid、版本、协议版本、启动时间、socket 与日志路径 | socket 连不上时靠它判断“僵尸还是没跑”；异常退出会残留，`gld daemon start` 自动清理 |
| `logs/daemon.log` | stdout / stderr 重定向到这里 | 超过 4 MiB 会在下次 `daemon start` 时轮转为 `daemon.log.1`；只保留一代 |

“是否在运行”只信 socket：能连上并回应 `daemon_info` 才算活着。
pid 文件只用于展示和补充判断。

## 日志有多大

每个日志文件（守护进程的、以及每个工作区的 `mcp-requests.log` / `stdout.log` /
`stderr.log` / 隧道日志）超过 **4 MiB** 就轮转成 `<名字>.1`，只保留一代。
所以单个名字最多占 8 MiB，不会把磁盘写满，也不需要配 logrotate。

工作区日志在写入时检查大小；守护进程自己的日志是启动时重定向的文件描述符，
进程内插不了手，只能在每次 `daemon start` 前检查一次——所以一个连续跑几周
不重启的守护进程，它的 `daemon.log` 可能超过 4 MiB。真的很大时停掉它、
删掉文件、再启动即可。

`gld logs` 只读当前那一代；上一代要自己去数据目录看 `.1` 文件。

## 启动时发生什么

1. 拿 `daemon.lock` 排他锁，拿不到直接退出（另一个实例在跑）。
2. 清理残留的 socket 文件，绑定新的。
3. 读取 `data/profiles.json`。文件损坏会在这一步失败，错误写进 `daemon.log`，
   命令行侧表现为“守护进程在 15 秒内没有就绪”。
4. 写 `daemon.json`。
5. 如果 `gld settings runtime --restore-on-launch true` 打开了，恢复上次退出前正在跑的
   MCP / Actions（清单在 `profiles.json` 的 `restore_*_workspace_ids`）。
6. 进入接受连接的循环，每个连接一个任务，互不阻塞。

## 退出时发生什么

收到 `shutdown` 请求（`gld daemon stop`）或 SIGTERM / SIGINT / SIGHUP 后：

1. 停止所有 MCP / Actions 监听器，等端口真正释放（最多 3 秒，超时强制 abort）；
2. 停掉每个工作区的 frpc / cloudflared 子进程；
3. 停掉全局入口；
4. 删除 `daemon.sock` 与 `daemon.json`，释放锁，进程退出。

“下次恢复”清单不会被清空：下一次守护进程启动、且开了 restore-on-launch，服务会回来。
`kill -9` 跳过以上全部步骤，frpc / cloudflared 可能变成孤儿进程，
下次启动同一工作区时 supervisor 会按 pid 文件回收它们。

## 后台进程和终端的关系

`gld daemon start` 用 `setsid` 起新会话（Windows 用 `DETACHED_PROCESS`），
stdin 关闭，stdout / stderr 追加到 `logs/daemon.log`，工作目录切到数据目录。
所以关闭终端、退出 SSH 都不会带走它。

环境变量只显式传递 `GLD_HOME`；`PATH` 等继承自拉起它的那个 shell。
如果你的 frpc 装在只有某个 shell 才有的 PATH 里（比如只在 `.zshrc` 里加过），
用 `gld settings runtime --executable-paths` 补上那个目录，让路径不依赖是谁拉起的守护进程。

## 开机自启

用 `gld daemon run` 交给系统的进程管理器即可，不要用 `daemon start`（那会 fork 一个
管理器不认识的子进程）。

macOS launchd（`~/Library/LaunchAgents/dev.gld.daemon.plist`）：

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>dev.gld.daemon</string>
  <key>ProgramArguments</key><array>
    <string>/Users/you/.cargo/bin/gld</string><string>daemon</string><string>run</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>/Users/you/.gld/logs/daemon.log</string>
  <key>StandardErrorPath</key><string>/Users/you/.gld/logs/daemon.log</string>
</dict></plist>
```

```bash
launchctl load ~/Library/LaunchAgents/dev.gld.daemon.plist
```

Linux systemd 用户单元（`~/.config/systemd/user/gld.service`）：

```ini
[Unit]
Description=gld daemon

[Service]
ExecStart=%h/.cargo/bin/gld daemon run
Restart=on-failure

[Install]
WantedBy=default.target
```

```bash
systemctl --user enable --now gld
```

这两种方式下 `gld daemon stop` 仍然可用，但管理器会按 KeepAlive / Restart 策略把它拉回来；
要彻底停用请用 `launchctl unload` / `systemctl --user disable --now gld`。

## 升级

守护进程加载的是旧二进制的代码。升级 `gld` 后：

```bash
gld daemon restart
```

没重启时执行任何命令都会提示“守护进程版本 x 与命令行版本 y 不一致”（退出码 4），
`daemon status` / `daemon stop` 不受影响。

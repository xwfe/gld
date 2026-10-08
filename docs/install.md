# 安装

`gld` 主程序是单个可执行文件，不需要 Node 或 Tauri 运行时。
Git、项目构建工具、隧道程序和启用的外部 MCP server 仍需按实际用途另行安装。

## 下载现成的（不需要装 Rust）

到 [Releases](https://github.com/xwfe/gld/releases) 拿对应平台的包：

| 平台 | 文件名 |
| --- | --- |
| macOS（Apple 芯片） | `gld-<版本>-aarch64-apple-darwin.tar.gz` |
| macOS（Intel） | `gld-<版本>-x86_64-apple-darwin.tar.gz` |
| Linux x86_64 | `gld-<版本>-x86_64-unknown-linux-gnu.tar.gz` |
| Linux x86_64（静态） | `gld-<版本>-x86_64-unknown-linux-musl.tar.gz` |
| Windows x86_64 | `gld-<版本>-x86_64-pc-windows-msvc.zip` |

```bash
tar xzf gld-*-aarch64-apple-darwin.tar.gz
mkdir -p "$HOME/.local/bin"
install -m 755 gld-*/gld "$HOME/.local/bin/gld"
export PATH="$HOME/.local/bin:$PATH"
gld --version
```

上例应在只解压了一份目标安装包的目录执行。`export` 只管当前终端：新开终端会报 `command not found: gld`，
看着像没装上，其实是 PATH 没带上。要一直生效，写进 shell 配置：

```bash
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc    # macOS 默认是 zsh；用 bash 的写 ~/.bashrc
```

**Windows**：解压 zip，把里面的 `gld.exe` 放进一个固定目录，再把这个目录加进用户的 Path，新开一个 PowerShell
窗口跑 `gld --version`（这几条命令没在 Windows 真机跑过）：

```powershell
Expand-Archive gld-*-x86_64-pc-windows-msvc.zip -DestinationPath .
New-Item -ItemType Directory -Force "$env:USERPROFILE\.local\bin" | Out-Null
Copy-Item gld-*-x86_64-pc-windows-msvc\gld.exe "$env:USERPROFILE\.local\bin\gld.exe"
[Environment]::SetEnvironmentVariable("Path", [Environment]::GetEnvironmentVariable("Path", "User") + ";$env:USERPROFILE\.local\bin", "User")
```

**Linux 选哪个：** `gnu` 要 glibc 2.35 以上（Ubuntu 22.04+ / Debian 12+ / RHEL 9+）；`musl` 是静态链接的，
不挑发行版和 glibc 版本，Alpine 上也能跑。实测过哪些见[平台支持的实际情况](#平台支持的实际情况)。

**macOS 首次运行可能被 Gatekeeper 拦**：包没有代码签名，用浏览器下载的会带"下载隔离"标记，第一次运行
通常弹窗说无法验证开发者、命令跑不起来（用 `gh release download`、`curl` 下载的不带这个标记）。核对发布来源
和校验和后，确认信任该文件，再决定是否移除这个标记；这不是安全验证的替代：

```bash
xattr -d com.apple.quarantine "$HOME/.local/bin/gld"
```

**核对下载没被掉包**（可选）：每个 Release 旁边有 `SHA256SUMS`。

```bash
sha256sum -c --ignore-missing SHA256SUMS    # macOS 上是 shasum -a 256 -c --ignore-missing SHA256SUMS
```

`SHA256SUMS` 列着全部 5 个包。不加 `--ignore-missing` 的话，只下了一个包时另外 4 个会报找不到、退出码 1，
看着像被掉包了，其实只是没下载。

**核对它是这个仓库的发布流水线编出来的**（可选，v0.8.0 起才有）：每个包带一份构建来源证明
（GitHub artifact attestation，Sigstore 签名，记着是哪个仓库、哪个提交、哪条流水线编的）。装了
[GitHub CLI](https://cli.github.com/) 且它能访问 GitHub API 的话：

```bash
gh attestation verify gld-<版本>-aarch64-apple-darwin.tar.gz --repo xwfe/gld \
  --signer-workflow xwfe/gld/.github/workflows/release.yml
```

命令成功退出（退出码 0）才算过。`SHA256SUMS` 只证明下载到的和 Release 页上的是同一份，说明不了它从哪来；
来源证明补的是这一半。v0.7.0 及以前的包没有这份证明，这条命令会报找不到。

## 从源码装

需要 Rust 1.89+，没有 Node、没有 Tauri 依赖。

```bash
git clone <本仓库> && cd gld
cargo install --path crates/cli        # 装到 ~/.cargo/bin/gld
gld --version
```

只想本地试试：`cargo build --release`，二进制在 `target/release/gld`。

## 升级

**ChatGPT 连接器不用删，也不用重新授权。** 升级只是换二进制、重启守护进程；连接器靠的口令、令牌签名
密钥、ChatGPT 动态注册的客户端（`data/oauth-clients/hub.json`）、公网地址都在数据目录 `~/.config/gld`
和服务配置里，换二进制碰不到它们。**唯一可能要做的是点一次 Refresh**：新版改了工具表时 ChatGPT 不会
自己重拉。要不要点，比一下升级前后的工具表指纹就知道（下面第 2 步和"换完核对"）。
除了升级，还有哪些操作会碰到连接器，见[装好的连接器什么时候要动](connect-clients.md#装好的连接器什么时候要动)。

本机从 0.6.0 起每次升级都逐项比对过口令、签名密钥、Client ID、ChatGPT 注册的客户端、公网地址、项目表的指纹，
全部一致，守护进程重启 0.2–0.3 秒，ChatGPT 用原来注册的客户端直接连上（每次的记录在[审查](reviews/2026-09-23-lifecycle-and-docs-audit.md)里）。

### 升级前先看两件事

- **公网地址是固定的吗？** `gld ls` 的"公网入口"一行写着 Cloudflare 临时地址的话，服务一重启地址就换，
  连接器只能删了重建。长期用先换固定地址（[办法二～四](connect-clients.md#办法二cloudflare-固定域名)），再升级。
- **数据目录别动。** 升级不需要删 `~/.config/gld`，也别换 `GLD_HOME`：口令和签名密钥没有第二份。

### 步骤

```bash
# 1. 拿到新二进制。从源码：
cd gld && git pull && cargo build --release --locked -p gld   # 产物 target/release/gld
#    用发行包的，解压出来的 gld-*/gld 就是，下面第 3 步换成它

# 2. 记下现在的样子（都不含明文凭据，可以放心存），再留两份备份好回滚
gld tool list --served | sed -n 2p > /tmp/gld-tools-before.txt   # tools/list：N 个工具  指纹 …
gld ls > /tmp/gld-before.txt
mkdir -p ~/.local/opt
cp -p "$(command -v gld)" ~/.local/opt/gld-$(date +%Y%m%d-%H%M)
tar -czf ~/.local/opt/gld-config-$(date +%Y%m%d-%H%M).tgz -C ~/.config --exclude gld/daemon.sock gld

# 3. 换二进制：先写成新文件，再改名盖上去
GLD="$(command -v gld)"
install -m 755 target/release/gld "$GLD.new" && mv "$GLD.new" "$GLD"

# 4. 重启守护进程。版本号没变也要做，见下面
gld daemon restart
#    配了 launchd 开机自启的换成下面这句，守护进程继续归 launchd 管（见守护进程 · 开机自启）：
#    gld daemon stop && launchctl kickstart gui/$(id -u)/dev.gld.daemon
```

`gld tool list --served` 是 0.7.0 加的；从更老的版本升上来时没有它，第 2 步那行跳过，换完直接按
"不一样"处理（点一次 Refresh 没有坏处）。

**别用 `cp` 直接盖正在跑的那个文件。** 在 Apple Silicon 上，往一个已经执行过的
Mach-O 里写东西会让它的代码签名失效，之后每次 exec 都被 SIGKILL（退出码 137），
而还在跑的老进程一切正常——症状是 `gld --version` 变成 `Killed: 9`。写新文件再
改名换的是 inode，跑着的进程留着自己那份，下一次 exec 拿到的是完整、签名正确的。

**中断有多久**：`gld daemon restart` 就是顺序的 stop + start，没有 fd 交接，所以新旧进程
不会并存（单实例靠 `daemon.lock` 的 flock）。在途请求最多有 3 秒宽限，之后强断；服务起来
就恢复。隧道是 gld 起的话跟着一起重起；自建反代 / 自己跑的 cloudflared 不受影响，只是那几秒
回源会 502。MCP 服务会跟着守护进程自己回来；项目的 GPT Actions 要各自再
`gld start -s actions`，见[守护进程 · 升级](daemon.md#升级)。

### 换完核对

```bash
gld daemon status                        # pid 变了，运行时长从头算，协议号是新版的
gld tool list --served | head -1         # "构建提交"要等于你编译的那个：git rev-parse --short=12 HEAD
gld tool list --served | sed -n 2p | diff /tmp/gld-tools-before.txt - && echo "工具表没变，ChatGPT 什么都不用做"
gld ls | diff /tmp/gld-before.txt -      # Client ID、口令（脱敏后的前后几位）、公网地址、项目表都不该变
gld health                               # 本地、公网 /mcp，OAuth 元数据都是 ✓
```

**工具表那行（第三条）是决定 ChatGPT 要不要动的那一步：**

- 打出"工具表没变"：连接器什么都不用做。
- diff 出了不一样（工具数或指纹变了）：新版改了工具表，到 chatgpt.com/plugins 点一次 **Refresh**、再开
  新对话，做法和怎么核对见[点 Refresh](connect-clients.md#装好的连接器什么时候要动)。不点也能接着用，
  只是新加的工具和参数 AI 看不见。每一版的[发布说明](releases/)也会写这一版要不要点。

**版本号没变时，命令行可能不提醒你重启。** 命令行只比版本号和协议号：两样都没变的新构建，换完二进制
不重启，守护进程接着跑旧代码，**不报任何错**，只有新加的命令会报一句 `` unknown variant `…` ``
（2026-09-23 升级时实际碰到的）。所以不管版本号变没变，换完都 `gld daemon restart`，再看上面那行
构建提交。版本号或协议号变了的时候，命令行会直接拒绝并提示重启（退出码 4）。

只核对 `0.7.0` 这样的版本号不够，源码、构建、运行的服务、客户端拿到的工具表是四层不同的证据，见
[生命周期指南](project-lifecycle.md#接入前先确认四层能力)。

**回滚**：把备份的二进制按同样的"写新文件再改名"放回去，再 `gld daemon restart`。数据目录一般
不用回滚；真要回，先 `gld daemon stop`，再把备份的 `tgz` 解回 `~/.config`。回滚到的版本比当前的旧、
工具表不一样时，ChatGPT 也要点一次 Refresh。

> 注意别和 `gld upgrade` 搞混：那条命令改的是**服务和项目的配置**（端口、认证、公网入口、
> 项目目录），不升级 gld 自己。升级 gld 只有"换二进制 + `gld daemon restart`"这一条路。

### 从 0.3.0 之前升级：数据目录搬了家

数据目录从 `~/.gld` 换到了 `~/.config/gld`，**没有兼容读取**。不搬的话 gld 会
当成全新安装：`gld ls` 说没有项目，而配置和密钥还在旧目录里躺着。

```bash
gld daemon stop            # socket 和锁文件正被占着，先停
mv ~/.gld ~/.config/gld
gld ls                     # 项目应该都回来了
```

密钥没有第二份副本，搬之前别删旧目录。

## 平台支持的实际情况

| 平台 | 状态 |
| --- | --- |
| macOS（Apple 芯片 / Intel） | CI 在 macOS 上跑全量测试；v0.8.2 两个包在 Apple 芯片的 Mac 上实跑过（Intel 版经 Rosetta），本机常驻服务用的就是 Apple 芯片版发布包 |
| Linux x86_64（gnu / musl） | 两个都是每次发版必须编过的目标；CI 在 Ubuntu 上跑全量测试；v0.8.2 两个包在 amd64 的 Debian 12 容器里起停守护进程、调工具正常，v0.7.0 的 musl 包在 Oracle Linux 9 容器里跑过；别的发行版没实测 |
| Windows x86_64 | CI 跑编译、补丁 / 写锁测试，和 9 条守护进程端到端测试（命名管道起停、单实例锁、命令终态、运行记录）；下载包没在 Windows 真机跑过 |

Windows 上守护进程走的是命名管道，和 Unix domain socket 是两套独立实现。
遇到问题请提 issue，带上 `gld daemon status` 的输出。

## 卸载

配过[开机自启](daemon.md#开机自启)的先撤掉，不然删了二进制之后系统还会反复去拉一个不存在的程序：macOS 跑
`launchctl bootout gui/$(id -u)/dev.gld.daemon && rm ~/Library/LaunchAgents/dev.gld.daemon.plist`，
Linux 跑 `systemctl --user disable --now gld`。然后：

```bash
gld daemon stop            # 先停掉后台进程和它持有的服务、隧道
rm "$HOME/.local/bin/gld" # 按实际安装位置调整；源码安装可用 cargo uninstall gld
rm -rf ~/.config/gld              # 配置和密钥，删了就找不回来了
```

`~/.config/gld/data/profiles.json` 是所有密钥的唯一副本，删之前想清楚——
细节见 [security.md](security.md#密钥存在哪丢了会怎样)。
项目目录里的 `.gld/` 和 `docs/history-session/` 属于项目本身，不在这里删。

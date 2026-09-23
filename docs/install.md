# 安装

`gld` 主程序是单个可执行文件，不需要 Node 或 Tauri 运行时。
Git、项目构建工具、隧道程序和启用的外部 MCP server 仍需按实际用途另行安装。

## 下载现成的（不需要装 Rust）

到 [Releases](../../releases) 拿对应平台的包：

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

上例应在只解压了一份目标安装包的目录执行；后续终端也需配置相同的 PATH。

**Linux 选哪个：** `gnu` 构建基线是 glibc 2.35；`musl` 是静态链接目标，减少对发行版
glibc 的依赖，但发行包仍以该次 Release 实际产物和验收平台为准。musl 构建目前是可选
目标，不保证每次 Release 都有，不能把构建目标列表当成已验证的兼容矩阵。

**macOS 首次运行可能被 Gatekeeper 拦。** 核对发布来源和校验和后，确认信任该文件，
再决定是否移除下载隔离属性；这不是安全验证的替代：

```bash
xattr -d com.apple.quarantine "$HOME/.local/bin/gld"
```

**核对下载没被掉包**（可选）：每个 Release 旁边有 `SHA256SUMS`。

```bash
sha256sum -c SHA256SUMS      # macOS 上是 shasum -a 256 -c SHA256SUMS
```

## 从源码装

需要 Rust 1.89+，没有 Node、没有 Tauri 依赖。

```bash
git clone <本仓库> && cd gld
cargo install --path crates/cli        # 装到 ~/.cargo/bin/gld
gld --version
```

只想本地试试：`cargo build --release`，二进制在 `target/release/gld`。

## 升级

**ChatGPT 连接器不用删，也不用重新授权。** 升级只是换二进制、重启守护进程；连接器靠的东西都在
数据目录 `~/.config/gld` 里，换二进制碰不到它们：

| 连接器靠的 | 存在哪 | 什么操作才会动它 |
| --- | --- | --- |
| Client ID、授权口令 | 数据目录里的服务凭据 | 只有 `gld secret set` / `gld secret regen` |
| 令牌签名密钥：已经发给 ChatGPT 的令牌靠它验 | 同上 | `gld secret regen oauth_token_secret`，之后要重新授权 |
| ChatGPT 自己注册的客户端（日志里 `auth=oauth:hub:dcr-…` 那个） | `data/oauth-clients/hub.json` | 删掉数据目录 |
| 公网地址 | 服务配置 | 用 `--tunnel cf` 临时地址时，每次重启都换，连接器只能删了重建 |

实测（2026-09-23，两次都是 0.6.0 的构建之间）：上表各项升级前后逐项比对指纹，全部一致；
守护进程重启 0.3 秒，公网 `/mcp` 和 OAuth 元数据照常，服务自己回来。

### 步骤

```bash
# 1. 拿到新二进制。从源码：
cd gld && git pull && cargo build --release --locked -p gld   # 产物 target/release/gld
#    用发行包的，解压出来的 gld-*/gld 就是，下面第 3 步换成它

# 2. 记下现在的样子（凭据是脱敏的，可以放心存），再留两份备份好回滚
gld ls > /tmp/gld-before.txt
mkdir -p ~/.local/opt
cp -p "$(command -v gld)" ~/.local/opt/gld-$(date +%Y%m%d-%H%M)
tar -czf ~/.local/opt/gld-config-$(date +%Y%m%d-%H%M).tgz -C ~/.config --exclude gld/daemon.sock gld

# 3. 换二进制：先写成新文件，再改名盖上去
GLD="$(command -v gld)"
install -m 755 target/release/gld "$GLD.new" && mv "$GLD.new" "$GLD"

# 4. 重启守护进程。版本号没变也要做，见下面
gld daemon restart
```

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
gld daemon status                  # pid 变了，运行时长从头算
gld tool list --served | head -2   # "构建提交"要等于你编译的那个：git rev-parse --short=12 HEAD
gld ls | diff /tmp/gld-before.txt -   # Client ID、口令（脱敏后的前后几位）、公网地址、项目表都不该变
gld health                         # 本地、公网 /mcp，OAuth 元数据都是 ✓
```

**版本号没变时，命令行不会提醒你重启。** 命令行只比版本号和协议号：同样叫 0.6.0 的新构建，
换完二进制不重启，守护进程接着跑旧代码，**不报任何错**，只有新加的命令会报一句
`` unknown variant `served_tools` ``（2026-09-23 升级时实际碰到的）。所以不管版本号变没变，
换完都 `gld daemon restart`，再看上面那行构建提交。版本号变了的时候，命令行会直接拒绝并提示
重启（退出码 4）。

**ChatGPT 那边**：授权不用动。新版加了工具或参数时，ChatGPT 可能还拿着旧的工具表——服务声明了
`listChanged: false`，不会通知它重拉。在 ChatGPT 里让它调一次 `server_info`：`build_commit`
应该和上面那行一样；`connection.tools_fingerprint` 和 `gld tool list --served` 的指纹对不上，
按[核对客户端拿到的工具表](troubleshooting.md#核对客户端拿到的工具表)让它重拉。只核对 `0.6.0`
这样的版本号不够，源码、构建、运行的服务、客户端拿到的工具表是四层不同的证据，见
[生命周期指南](project-lifecycle.md#接入前先确认四层能力)。

**回滚**：把备份的二进制按同样的"写新文件再改名"放回去，再 `gld daemon restart`。数据目录一般
不用回滚；真要回，先 `gld daemon stop`，再把备份的 `tgz` 解回 `~/.config`。

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
| macOS（Apple 芯片 / Intel） | 有历史实机证据与 CI 测试配置；本次审查仅在当前 macOS 开发机重跑，不外推到所有构件 |
| Linux x86_64（gnu / musl） | 有历史实机证据，CI 配置包含 Linux 测试；musl 发布构建为可选目标 |
| Windows x86_64 | CI 配置包含编译及部分补丁 / 写锁测试；尚不能据此宣称完整命名管道、服务、MCP 与进程生命周期已实机验收 |

Windows 上守护进程走的是命名管道，和 Unix domain socket 是两套独立实现。
遇到问题请提 issue，带上 `gld daemon status` 的输出。

## 卸载

```bash
gld daemon stop            # 先停掉后台进程和它持有的服务、隧道
rm "$HOME/.local/bin/gld" # 按实际安装位置调整；源码安装可用 cargo uninstall gld
rm -rf ~/.config/gld              # 配置和密钥，删了就找不回来了
```

`~/.config/gld/data/profiles.json` 是所有密钥的唯一副本，删之前想清楚——
细节见 [security.md](security.md#密钥存在哪丢了会怎样)。
项目目录里的 `.gld/` 和 `docs/history-session/` 属于项目本身，不在这里删。

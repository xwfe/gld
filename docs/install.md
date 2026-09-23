# 安装

`gld` 是单个可执行文件，没有运行时依赖，放进 PATH 就能用。

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
sudo mv gld-*/gld /usr/local/bin/
gld --version
```

**Linux 选哪个：** `gnu` 那个需要 glibc 2.35+（Ubuntu 22.04 / Debian 12 / RHEL 9 起）；
`musl` 那个是静态链接，不挑发行版，Alpine 和老系统上也能跑。拿不准就选 musl。

**macOS 第一次运行会被 Gatekeeper 拦**（二进制没有 Apple 签名），报的是
“无法打开，因为无法验证开发者”。执行一次就好：

```bash
xattr -d com.apple.quarantine /usr/local/bin/gld
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

换掉二进制，然后重启守护进程：

```bash
cp ~/.local/bin/gld ~/.local/opt/gld-$(gld --version | awk '{print $2}')  # 留一份好回滚
install -m 755 target/release/gld ~/.local/bin/gld.new                    # 先写成新文件
mv ~/.local/bin/gld.new ~/.local/bin/gld                                  # 再改名盖上去
gld daemon restart
```

**别用 `cp` 直接盖正在跑的那个文件。** 在 Apple Silicon 上，往一个已经执行过的
Mach-O 里写东西会让它的代码签名失效，之后每次 exec 都被 SIGKILL（退出码 137），
而还在跑的老进程一切正常——症状是 `gld --version` 变成 `Killed: 9`。写新文件再
改名换的是 inode，跑着的进程留着自己那份，下一次 exec 拿到的是完整、签名正确的。

MCP 服务会跟着守护进程自己回来；项目的 GPT Actions 要各自再 `gld start -s actions`，见[守护进程 · 升级](daemon.md#升级)。

**中断有多久**：`gld daemon restart` 就是顺序的 stop + start，没有 fd 交接，所以新旧进程
不会并存（单实例靠 `daemon.lock` 的 flock）。在途请求最多有 3 秒宽限，之后强断；服务起来
就恢复。隧道是 gld 起的话跟着一起重起；自建反代 / 自己跑的 cloudflared 不受影响，只是那几秒
回源会 502。

**客户端那边不用动**：地址、凭据、OAuth 动态注册（`data/oauth-clients/hub.json`）都在磁盘上，
换二进制不碰它们。唯一会逼你删掉 ChatGPT 连接器重建的是**公网地址变了**，而那只发生在用
Cloudflare 临时地址（`--tunnel cf`）的时候，见[连接客户端](connect-clients.md#什么时候要重新授权什么时候要删了重建)。

换完核对三件事：

```bash
gld daemon status    # 版本、协议号是新的，"运行中的服务" ≥ 1
gld ls               # 公网地址、Client ID、项目表和升级前一样
gld doctor --probe   # 隧道此刻在不在，本地 / 公网端点和 OAuth 元数据通不通
```

回滚就是把备份的那个二进制按同样的"改名"方式放回去，再 `gld daemon restart`。

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

**这一步不能省。** 服务住在一个常驻的守护进程里，换了二进制它还在跑旧代码。
命令行会核对版本，不一致时直接报错并提示重启，而不是发一个对方不认识的请求
（退出码 4）。

## 平台支持的实际情况

| 平台 | 状态 |
| --- | --- |
| macOS（Apple 芯片 / Intel） | 实机验证过 |
| Linux x86_64（gnu / musl） | 实机验证过 |
| Windows x86_64 | 每次发版都构建，CI 也做编译检查，但**没有在真机上跑过** |

Windows 上守护进程走的是命名管道，和 Unix domain socket 是两套独立实现。
遇到问题请提 issue，带上 `gld daemon status` 的输出。

## 卸载

```bash
gld daemon stop            # 先停掉后台进程和它持有的服务、隧道
rm /usr/local/bin/gld      # 或 cargo uninstall gld
rm -rf ~/.config/gld              # 配置和密钥，删了就找不回来了
```

`~/.config/gld/data/profiles.json` 是所有密钥的唯一副本，删之前想清楚——
细节见 [security.md](security.md#密钥存在哪丢了会怎样)。
项目目录里的 `.gld/` 和 `docs/history-session/` 属于项目本身，不在这里删。

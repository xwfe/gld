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

需要 Rust 1.85+，没有 Node、没有 Tauri 依赖。

```bash
git clone <本仓库> && cd gld
cargo install --path crates/cli        # 装到 ~/.cargo/bin/gld
gld --version
```

只想本地试试：`cargo build --release`，二进制在 `target/release/gld`。

## 升级

覆盖掉旧的二进制，然后：

```bash
gld daemon restart
```

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
rm -rf ~/.gld              # 配置和密钥，删了就找不回来了
```

`~/.gld/data/profiles.json` 是所有密钥的唯一副本，删之前想清楚——
细节见 [security.md](security.md#密钥存在哪丢了会怎样)。
项目目录里的 `.coding-tools/` 和 `docs/history-session/` 属于项目本身，不在这里删。

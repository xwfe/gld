#!/bin/sh
# 下载 gld 发布包、核对校验和、装到用户目录。不要管理员权限，不碰 gld 的数据和节点配置。
#
#   curl -fsSL https://github.com/xwfe/gld/releases/download/v0.10.0/install.sh | GLD_VERSION=v0.10.0 sh
#
# 环境变量：
#   GLD_VERSION      要装的版本，如 v0.10.0；不设就装最新发布
#   GLD_INSTALL_DIR  装到哪，默认 ~/.local/bin
#   GLD_BASE_URL     下载地址前缀，默认 GitHub Release；内网镜像和测试用
#
# 经管道执行，要写成 POSIX sh：Debian、Ubuntu 上 sh 是 dash，没有 bash 的语法。
# 变量后面紧跟中文一律写 ${var}：macOS 的 sh 是 bash 3.2，UTF-8 下会把中文字节当成变量名的一部分。
set -eu

REPO="https://github.com/xwfe/gld"

fail() {
  echo "gld 安装失败：$*" >&2
  exit 1
}

command -v curl >/dev/null 2>&1 || fail "需要 curl，先用系统的包管理器装上。"
command -v tar >/dev/null 2>&1 || fail "需要 tar，先用系统的包管理器装上。"

detect_target() {
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$os" in
    Darwin)
      case "$arch" in
        arm64 | aarch64) echo "aarch64-apple-darwin" ;;
        x86_64)
          # Rosetta 里的终端也报 x86_64，但 Apple 芯片的机器该用原生包。
          if [ "$(sysctl -n hw.optional.arm64 2>/dev/null || echo 0)" = "1" ]; then
            echo "aarch64-apple-darwin"
          else
            echo "x86_64-apple-darwin"
          fi
          ;;
        *) fail "没有 macOS ${arch} 的发布包。" ;;
      esac
      ;;
    Linux)
      case "$arch" in
        # 用静态链接的 musl 包：不挑发行版和 glibc 版本，老系统和 Alpine 都能跑。
        x86_64 | amd64) echo "x86_64-unknown-linux-musl" ;;
        *) fail "没有 Linux ${arch} 的发布包（现在只有 x86_64）。可以从源码装，见 docs/install.md。" ;;
      esac
      ;;
    *) fail "没有 ${os} 的发布包。Windows 用 install.ps1。" ;;
  esac
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d ' ' -f 1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d ' ' -f 1
  else
    fail "需要 sha256sum 或 shasum 来核对下载的文件。"
  fi
}

target="$(detect_target)"

version="${GLD_VERSION:-}"
if [ -z "$version" ]; then
  latest="$(curl -fsSLI -o /dev/null -w '%{url_effective}' "$REPO/releases/latest")" ||
    fail "查不到最新版本，网络能访问 GitHub 吗？也可以用 GLD_VERSION 指定版本。"
  version="${latest##*/}"
fi
case "$version" in
  v*) ;;
  *) version="v$version" ;;
esac

base="${GLD_BASE_URL:-$REPO/releases/download/$version}"
archive="gld-$version-$target.tar.gz"
install_dir="${GLD_INSTALL_DIR:-$HOME/.local/bin}"

tmp="$(mktemp -d)"
staged=""
cleanup() {
  rm -rf "$tmp"
  [ -z "$staged" ] || rm -f "$staged"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

echo "下载 $archive ..."
curl -fsSL --retry 2 -o "$tmp/$archive" "$base/$archive" ||
  fail "下载不了 $base/${archive}。看版本 $version 是否存在、网络能否访问 GitHub。"
curl -fsSL --retry 2 -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" ||
  fail "下载不了 $base/SHA256SUMS，没法核对安装包，没有安装。"

expected="$(awk -v file="$archive" '$2 == file { print $1 }' "$tmp/SHA256SUMS")"
[ -n "$expected" ] || fail "SHA256SUMS 里没有 ${archive}，没有安装。"
actual="$(sha256_of "$tmp/$archive")"
[ "$actual" = "$expected" ] ||
  fail "$archive 的校验和对不上（应为 ${expected}，实际 ${actual}）。下载可能被改过或没下完，没有安装。"

tar xzf "$tmp/$archive" -C "$tmp" || fail "解压 $archive 失败，没有安装。"
binary="$tmp/gld-$version-$target/gld"
[ -f "$binary" ] || fail "$archive 里没有 gld，没有安装。"
chmod 755 "$binary"
# 先跑一次：跑不起来的二进制（架构不对、缺运行库）不能盖掉原来能用的那个。
reported="$("$binary" --version 2>&1)" || fail "下载的 gld 在这台机器上跑不起来：${reported}。没有安装。"

previous=""
if [ -x "$install_dir/gld" ]; then
  previous="$("$install_dir/gld" --version 2>/dev/null || echo 旧版本)"
fi
mkdir -p "$install_dir" || fail "建不了目录 ${install_dir}。可以用 GLD_INSTALL_DIR 换一个。"
staged="$install_dir/.gld.installing.$$"
cp "$binary" "$staged" || fail "写不了 ${install_dir}。可以用 GLD_INSTALL_DIR 换一个。"
chmod 755 "$staged"
# 同目录改名是原子的：中途断掉只会留下旧版本，不会留下半个文件。正在跑的旧进程不受影响。
mv -f "$staged" "$install_dir/gld"
staged=""

echo "已安装 $reported 到 $install_dir/gld"
if [ -n "$previous" ]; then
  echo "替换了 ${previous}。节点已在后台跑的话，跑 gld node autostart enable 换上新版。"
fi
case ":$PATH:" in
  *":$install_dir:"*) ;;
  *)
    profile="$HOME/.profile"
    case "${SHELL:-}" in
      */zsh) profile="$HOME/.zshrc" ;;
      */bash) profile="$HOME/.bashrc" ;;
    esac
    echo "$install_dir 不在 PATH 里，新开的终端会找不到 gld。要一直能用："
    echo "  echo 'export PATH=\"$install_dir:\$PATH\"' >> $profile"
    ;;
esac

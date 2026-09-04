#!/usr/bin/env bash
# 构建并打包一个平台的发布件。
#
# 本地和 CI 走的是同一份逻辑——`.github/workflows/release.yml` 直接调这个脚本。
# 这不是洁癖：打包逻辑一旦分成两份，本地打出来的包和 Release 页上的包
# 会在文件名、目录结构、附带文件上慢慢分家，而两边都"看起来正常"，
# 只有用户下载解压之后才发现对不上。
#
# 用法：
#   scripts/package.sh                          当前平台
#   scripts/package.sh x86_64-apple-darwin      指定目标（需先 rustup target add）
#   VERSION=v0.3.0 scripts/package.sh           覆盖版本号（CI 传 tag 名）
#   scripts/package.sh --checksums              给 dist/ 里已有的包生成 SHA256SUMS
#
# 产物：dist/gld-<版本>-<目标>.tar.gz（Windows 目标为 .zip）
set -euo pipefail

# 二进制名。改名时这里、Cargo.toml 的 [[bin]] name、release.yml 必须同时改；
# crates/cli/tests/naming_is_consistent.rs 会盯着三者一致。
BIN="gld"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIST="$ROOT/dist"
cd "$ROOT"

# ---------------------------------------------------------------- 校验和

# sha256sum 是 GNU coreutils 的，macOS 上没有，只有 shasum。
# 两者输出格式一致（`<hash>  <文件名>`），所以 `-c` 校验能互通。
sha256_all() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$@"
  else
    shasum -a 256 "$@"
  fi
}

if [[ "${1:-}" == "--checksums" ]]; then
  [[ -d "$DIST" ]] || { echo "没有 dist/，先跑一次 scripts/package.sh" >&2; exit 1; }
  cd "$DIST"
  # 只匹配 gld-*，不写 *：后者会把正在生成的 SHA256SUMS 自己也算进去。
  shopt -s nullglob
  packages=(gld-*)
  (( ${#packages[@]} )) || { echo "dist/ 里没有 gld-* 包" >&2; exit 1; }
  sha256_all "${packages[@]}" > SHA256SUMS
  cat SHA256SUMS
  exit 0
fi

# ---------------------------------------------------------------- 目标与版本

TARGET="${1:-$(rustc -vV | awk '/^host:/ {print $2}')}"
[[ -n "$TARGET" ]] || { echo "取不到目标三元组（rustc -vV 没有 host 行？）" >&2; exit 1; }

# CI 传 tag 名进来；本地没有 tag，就用 Cargo.toml 里的版本号加个 v。
if [[ -z "${VERSION:-}" ]]; then
  # 只读 [workspace.package] 那一段的 version，避免撞上依赖里的同名键。
  VERSION="v$(awk '
    /^\[workspace\.package\]/ { in_section = 1; next }
    /^\[/                     { in_section = 0 }
    in_section && /^version[[:space:]]*=/ {
      # 按引号切而不是 gsub：`.*"` 是贪婪的，会一路吃到行尾那个引号，
      # 结果拿到空串——脚本随后报"没找到版本号"，看着像 Cargo.toml 有问题。
      split($0, parts, "\"")
      print parts[2]
      exit
    }
  ' Cargo.toml)"
  [[ "$VERSION" != "v" ]] || { echo "Cargo.toml 里没找到 [workspace.package].version" >&2; exit 1; }
fi

NAME="${BIN}-${VERSION}-${TARGET}"

# ---------------------------------------------------------------- 构建

echo "构建 $TARGET …"
cargo build --release --locked --target "$TARGET" -p "$BIN"

BUILT="target/${TARGET}/release/${BIN}"
[[ -f "${BUILT}.exe" ]] && BUILT="${BUILT}.exe"
[[ -f "$BUILT" ]] || { echo "构建产物不在 $BUILT" >&2; exit 1; }

# ---------------------------------------------------------------- 打包

rm -rf "${DIST:?}/${NAME}"
mkdir -p "$DIST/$NAME"
cp "$BUILT" "$DIST/$NAME/"
# README 一起打进去：用户解压后手边就有安装说明和 Gatekeeper 那一步。
cp README.md "$DIST/$NAME/"

cd "$DIST"
rm -f "${NAME}.tar.gz" "${NAME}.zip"
if [[ "$BUILT" == *.exe ]]; then
  # Windows runner 上有 7z；本地用 zip。两者产出的 zip 都能被资源管理器直接打开。
  if command -v 7z >/dev/null 2>&1; then
    7z a "${NAME}.zip" "$NAME" > /dev/null
  elif command -v zip >/dev/null 2>&1; then
    zip -qr "${NAME}.zip" "$NAME"
  else
    echo "打 Windows 包需要 7z 或 zip，两个都没找到" >&2
    exit 1
  fi
  ARCHIVE="${NAME}.zip"
else
  tar czf "${NAME}.tar.gz" "$NAME"
  ARCHIVE="${NAME}.tar.gz"
fi
rm -rf "$NAME"

echo "已生成 dist/${ARCHIVE}  ($(du -h "$ARCHIVE" | cut -f1))"

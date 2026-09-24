#!/usr/bin/env bash
# 从 `gld ... --help` 生成 docs/cli.md，保证文档和实际帮助永远一致。
# 用法：scripts/gen-cli-docs.sh   （需要先 cargo build）
#
# 任何一条命令失败、或生成出来缺了哪一节，就整体失败、原来的 docs/cli.md 一个字不动。
# 以前字段表和密钥名表用 `|| true` 吞错、直接往目标文件里写：守护进程版本对不上时
# 脚本照样退出 0，留下两张空表（RFC-0005 记过一次），CI 的比对也拦不住提交进去的空表。
#
# 环境变量：GLD 换要用的二进制；GLD_CLI_DOC_OUT 换输出位置（测试用）。
#
# 变量后面紧跟中文时一律写 `${var}`：macOS 自带的 bash 3.2 在 UTF-8 locale 下会把中文
# 标点的字节当成变量名的一部分，`$status，` 读的是一个叫 `status\xef` 的变量，set -u
# 当场报 unbound variable（2026-09-23 macOS CI 上就是这么挂的）。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GLD="${GLD:-$ROOT/target/debug/gld}"
OUT="${GLD_CLI_DOC_OUT:-$ROOT/docs/cli.md}"

if [[ ! -x "$GLD" ]]; then
  echo "找不到 ${GLD}，先执行 cargo build" >&2
  exit 1
fi

# 隔离：HOME 指到一次性目录。gld 没设 GLD_HOME 时用 $HOME/.config/gld，只 unset
# GLD_HOME 的话生成过程会读你真实的配置、项目和凭据名单。
SANDBOX="$(mktemp -d)"
# 临时文件和目标放同一个目录，最后一步 mv 才是原子的替换。
TMP_OUT="$(mktemp "$OUT.XXXXXX")"
# 走到最后才置 1。bash 3.2 在 set -u 这类致命错误之后跑 EXIT trap 时，$? 已经是 0，
# 脚本会以 0 退出——崩溃被报成成功，CI 和调用方都看不出来。trap 按这个标记兜底退出 1。
finished=0
trap 'rm -rf "$SANDBOX"; rm -f "$TMP_OUT"; [[ $finished == 1 ]] || exit 1' EXIT
export HOME="$SANDBOX"
export NO_COLOR=1
export COLUMNS=100
# clap 会把 env 参数的**当前值**印进帮助里（`[env: GLD_HOME=/你的/路径]`）。
# 在设过这些变量的 shell 里生成，产出的 cli.md 就带上了本机路径：CI 那道
# `git diff --exit-code docs/cli.md` 会挂，而且等于把自己的家目录提交进文档。
unset GLD_HOME GLD_WORKSPACE

# 命令树：一行一个，子命令用空格分隔。
COMMANDS=(
  ""
  "start" "stop" "restart" "status" "list" "add" "remove" "set" "fields" "share" "upgrade"
  "remote" "remote add" "remote remove"
  "mcp" "mcp list" "mcp on" "mcp off" "mcp test"
  "grant" "grant add" "grant list" "grant remove"
  "logs" "health" "doctor"
  "tool" "tool list" "tool schema" "tool call"
  "secret" "secret list" "secret set" "secret regenerate" "secret keys"
  "frp" "frp list" "frp add" "frp update" "frp remove"
  "settings" "settings list" "settings proxy" "settings runtime"
  "planning" "planning list" "planning mode" "planning goal" "planning goal create" "planning goal update" "planning goal accept" "planning goal reject" "planning plan" "planning plan create" "planning plan update" "planning plan accept" "planning plan reject"
  "history" "usage" "context"
  "daemon" "daemon start" "daemon stop" "daemon restart" "daemon status" "daemon run" "daemon logs"
  "completions"
)

# 跑一条 gld，失败就停，并说清是哪一条。
run() {
  local status=0
  "$GLD" "$@" || status=$?
  if [[ $status -ne 0 ]]; then
    echo "生成失败：gld $* 退出 ${status}，docs 没有改动" >&2
    exit 1
  fi
}

{
  echo "# gld 命令参考"
  echo
  echo "> 本文件由 \`scripts/gen-cli-docs.sh\` 从 \`gld --help\` 自动生成，请勿手改；改帮助文本请改 \`crates/cli/src/cli.rs\`。"
  echo
  echo "退出码：0 成功；1 操作失败；2 参数错误；3 守护进程未运行；4 守护进程版本与命令行不一致。"
  echo
  echo "RFC-0004 之前的命令（\`ws\`、\`destroy\`、\`hub\`、\`ps\`、\`tunnel\`、\`gateway\`、各处的 \`show\`）还能敲，只是不进帮助，这里也不列；新旧对照见 [RFC-0004](rfc/0004-one-service-many-projects.md) 第 2 节。"
  echo
  echo "## 目录"
  echo
  for cmd in "${COMMANDS[@]}"; do
    if [[ -z "$cmd" ]]; then
      echo "- [gld](#gld)"
    else
      anchor="gld-${cmd// /-}"
      echo "- [gld $cmd](#$anchor)"
    fi
  done
  echo
  for cmd in "${COMMANDS[@]}"; do
    if [[ -z "$cmd" ]]; then
      echo "## gld"
      echo
      echo '```text'
      run --help
      echo '```'
    else
      echo "## gld $cmd"
      echo
      echo '```text'
      # shellcheck disable=SC2086
      run $cmd --help
      echo '```'
    fi
    echo
  done
  echo "## gld set 支持的字段"
  echo
  echo '```text'
  # --all：文档要全集，命令行默认给的是精简版（省掉与 MCP 同名的 actions.*）。
  run fields --all
  echo '```'
  echo
  echo "## gld secret keys 密钥名一览"
  echo
  echo '```text'
  run secret keys
  echo '```'
} > "$TMP_OUT"

# 退出 0 不等于内容完整：每条命令的帮助都得有 Usage，两张表都得有表头和内容。
expected=${#COMMANDS[@]}
usages=$(grep -c '^Usage: gld' "$TMP_OUT" || true)
if [[ "$usages" -ne "$expected" ]]; then
  echo "生成失败：应有 $expected 段帮助，只找到 $usages 段 Usage，docs 没有改动" >&2
  exit 1
fi
table_rows() {
  awk -v title="$1" '
    $0 == title { inside = 1; next }
    inside && /^```text$/ { fence = 1; next }
    inside && fence && /^```$/ { exit }
    inside && fence && NF { rows++ }
    END { print rows + 0 }
  ' "$TMP_OUT"
}
for section in "## gld set 支持的字段" "## gld secret keys 密钥名一览"; do
  rows=$(table_rows "$section")
  if [[ "$rows" -lt 3 ]]; then
    echo "生成失败：「${section#\#\# }」只有 $rows 行，像是空表，docs 没有改动" >&2
    exit 1
  fi
done

chmod 644 "$TMP_OUT"
mv "$TMP_OUT" "$OUT"
finished=1
echo "已生成 $OUT"

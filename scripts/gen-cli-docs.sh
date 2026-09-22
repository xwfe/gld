#!/usr/bin/env bash
# 从 `gld ... --help` 生成 docs/cli.md，保证文档和实际帮助永远一致。
# 用法：scripts/gen-cli-docs.sh   （需要先 cargo build）
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GLD="${GLD:-$ROOT/target/debug/gld}"
OUT="$ROOT/docs/cli.md"

if [[ ! -x "$GLD" ]]; then
  echo "找不到 $GLD，先执行 cargo build" >&2
  exit 1
fi

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
      "$GLD" --help
      echo '```'
    else
      echo "## gld $cmd"
      echo
      echo '```text'
      # shellcheck disable=SC2086
      "$GLD" $cmd --help
      echo '```'
    fi
    echo
  done
  echo "## gld set 支持的字段"
  echo
  echo '```text'
  # --all：文档要全集，命令行默认给的是精简版（省掉与 MCP 同名的 actions.*）。
  "$GLD" fields --all 2>/dev/null || true
  echo '```'
  echo
  echo "## gld secret keys 密钥名一览"
  echo
  echo '```text'
  "$GLD" secret keys 2>/dev/null || true
  echo '```'
} > "$OUT"

echo "已生成 $OUT"

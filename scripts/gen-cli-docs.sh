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

# 命令树：一行一个，子命令用空格分隔。
COMMANDS=(
  ""
  "daemon" "daemon start" "daemon stop" "daemon restart" "daemon status" "daemon run" "daemon logs"
  "workspace" "workspace add" "workspace list" "workspace show" "workspace remove" "workspace set" "workspace fields" "workspace use"
  "start" "stop" "restart" "status" "ps" "logs" "ls" "share" "upgrade" "destroy" "health" "doctor"
  "tool" "tool list" "tool schema" "tool call"
  "tunnel" "tunnel start" "tunnel stop" "tunnel restart" "tunnel test" "tunnel status" "tunnel snippet"
  "gateway" "gateway show" "gateway set" "gateway start" "gateway stop" "gateway health"
  "secret" "secret show" "secret set" "secret regenerate" "secret shared" "secret keys"
  "frp" "frp list" "frp add" "frp update" "frp remove"
  "settings" "settings show" "settings proxy" "settings runtime"
  "planning" "planning show" "planning mode" "planning goal" "planning goal create" "planning goal update" "planning goal accept" "planning goal reject" "planning plan" "planning plan create" "planning plan update" "planning plan accept" "planning plan reject"
  "history" "usage" "context" "completions"
)

{
  echo "# gld 命令参考"
  echo
  echo "> 本文件由 \`scripts/gen-cli-docs.sh\` 从 \`gld --help\` 自动生成，请勿手改；改帮助文本请改 \`crates/cli/src/cli.rs\`。"
  echo
  echo "退出码：0 成功；1 操作失败；2 参数错误；3 守护进程未运行；4 守护进程版本与命令行不一致。"
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
  echo "## gld workspace set 支持的字段"
  echo
  echo '```text'
  # --all：文档要全集，命令行默认给的是精简版（省掉与 MCP 同名的 actions.*）。
  "$GLD" workspace fields --all 2>/dev/null || true
  echo '```'
  echo
  echo "## gld secret keys 密钥名一览"
  echo
  echo '```text'
  "$GLD" secret keys 2>/dev/null || true
  echo '```'
} > "$OUT"

echo "已生成 $OUT"

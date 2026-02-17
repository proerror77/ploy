#!/usr/bin/env bash
set -euo pipefail

cmd="${1:-}"
shift || true

repo_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_dir"

pid_file="${PLOY_DAEMON_PID_FILE:-data/state/pattern_memory_dryrun.pid}"
log_file="${PLOY_DAEMON_LOG_FILE:-data/logs/pattern_memory_dryrun.log}"

ensure_dirs() {
  mkdir -p "$(dirname "$pid_file")" "$(dirname "$log_file")"
}

is_running() {
  if [[ ! -f "$pid_file" ]]; then
    return 1
  fi
  local pid
  pid="$(cat "$pid_file" 2>/dev/null || true)"
  [[ -n "$pid" ]] || return 1
  kill -0 "$pid" 2>/dev/null
}

start() {
  local config_path="${1:-config/strategies/pattern_memory_default.toml}"

  if is_running; then
    echo "already running (pid=$(cat "$pid_file"))"
    exit 0
  fi

  ensure_dirs

  # Build (incremental; ensures the binary includes latest strategy changes).
  cargo build --release >/dev/null

  # Ensure file-logging doesn't try to use /var/log/ploy on dev machines.
  local logs_dir
  logs_dir="$(dirname "$log_file")"

  # Start detached, log to file, store pid.
  umask 077
  nohup env \
    "RUST_LOG=${RUST_LOG:-info}" \
    "PLOY_LOG_DIR=${PLOY_LOG_DIR:-$logs_dir}" \
    ./target/release/ploy strategy start pattern_memory \
      --config "$config_path" \
      --dry-run \
      --foreground \
      >>"$log_file" 2>&1 </dev/null &

  echo $! >"$pid_file"
  echo "started (pid=$!) log=$log_file"
}

stop() {
  if ! [[ -f "$pid_file" ]]; then
    echo "not running"
    exit 0
  fi

  local pid
  pid="$(cat "$pid_file" 2>/dev/null || true)"
  if [[ -z "$pid" ]]; then
    rm -f "$pid_file"
    echo "not running"
    exit 0
  fi

  if kill -0 "$pid" 2>/dev/null; then
    # The strategy CLI waits for Ctrl+C (SIGINT) to shutdown gracefully.
    kill -INT "$pid" 2>/dev/null || true
    for _ in {1..40}; do
      if ! kill -0 "$pid" 2>/dev/null; then
        break
      fi
      sleep 0.25
    done
    if kill -0 "$pid" 2>/dev/null; then
      kill -9 "$pid" 2>/dev/null || true
    fi
  fi

  rm -f "$pid_file"
  echo "stopped"
}

status() {
  if is_running; then
    echo "running (pid=$(cat "$pid_file"))"
  else
    echo "stopped"
    exit 1
  fi
}

logs() {
  ensure_dirs
  touch "$log_file"
  tail -n "${1:-200}" "$log_file"
}

follow() {
  ensure_dirs
  touch "$log_file"
  tail -f "$log_file"
}

usage() {
  cat <<'EOF'
Usage:
  scripts/pattern_memory_dryrun.sh start [config_path]
  scripts/pattern_memory_dryrun.sh stop
  scripts/pattern_memory_dryrun.sh status
  scripts/pattern_memory_dryrun.sh logs [n]
  scripts/pattern_memory_dryrun.sh follow

Examples:
  scripts/pattern_memory_dryrun.sh start
  scripts/pattern_memory_dryrun.sh start config/strategies/pattern_memory_default.toml

Env overrides:
  RUST_LOG=info
  PLOY_BINANCE_KLINE_BACKFILL_LIMIT=300
  PLOY_DAEMON_PID_FILE=...
  PLOY_DAEMON_LOG_FILE=...
  PLOY_LOG_DIR=...
EOF
}

case "$cmd" in
  start) start "${1:-}" ;;
  stop) stop ;;
  status) status ;;
  logs) logs "${1:-200}" ;;
  follow) follow ;;
  ""|-h|--help|help) usage ;;
  *) echo "unknown command: $cmd" >&2; usage; exit 2 ;;
esac

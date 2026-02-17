#!/usr/bin/env bash
set -euo pipefail

cmd="${1:-}"
shift || true

repo_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_dir"

pid_file="${PLOY_DAEMON_PID_FILE:-data/state/event_edge_agent.pid}"
log_file="${PLOY_DAEMON_LOG_FILE:-data/logs/event_edge_agent.log}"

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
  local trade="${1:-}"
  local dry_run="${2:-}"
  local event_ids="${3:-}"

  if is_running; then
    echo "already running (pid=$(cat "$pid_file"))"
    exit 0
  fi

  ensure_dirs

  # Optional overrides (require restart to take effect).
  local -a envs=("PLOY_EVENT_EDGE_AGENT__ENABLED=true")
  if [[ -n "$trade" ]]; then
    envs+=("PLOY_EVENT_EDGE_AGENT__TRADE=$trade")
  fi
  if [[ -n "$dry_run" ]]; then
    envs+=("PLOY_DRY_RUN__ENABLED=$dry_run")
  fi
  if [[ -n "$event_ids" ]]; then
    # Use single-underscore key here to avoid config crate deserialization
    # mismatch on Vec<String> env parsing; AppConfig applies this override manually.
    envs+=("PLOY_EVENT_EDGE_AGENT_EVENT_IDS=$event_ids")
  fi

  # Build if needed (fast path when already built).
  if [[ ! -x target/release/ploy ]]; then
    local features="${PLOY_CARGO_FEATURES:-onnx}"
    if [[ -n "$features" ]]; then
      cargo build --release --features "$features" >/dev/null
    else
      cargo build --release >/dev/null
    fi
  fi

  # Start detached, log to file, store pid.
  (
    umask 077
    exec env "${envs[@]}" ./target/release/ploy run >>"$log_file" 2>&1
  ) &

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
    kill "$pid" 2>/dev/null || true
    # Give it a moment, then SIGKILL if still alive.
    for _ in {1..20}; do
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
  scripts/event_edge_daemon.sh start [trade] [dry_run] [event_ids_csv]
  scripts/event_edge_daemon.sh stop
  scripts/event_edge_daemon.sh status
  scripts/event_edge_daemon.sh logs [n]
  scripts/event_edge_daemon.sh follow

Examples:
  scripts/event_edge_daemon.sh start false true    # safe observe
  scripts/event_edge_daemon.sh start true false    # live (requires auth env)
  scripts/event_edge_daemon.sh start false true 123,456

Env overrides:
  PLOY_DAEMON_PID_FILE=...
  PLOY_DAEMON_LOG_FILE=...
EOF
}

case "$cmd" in
  start) start "${1:-}" "${2:-}" "${3:-}" ;;
  stop) stop ;;
  status) status ;;
  logs) logs "${1:-200}" ;;
  follow) follow ;;
  ""|-h|--help|help) usage ;;
  *) echo "unknown command: $cmd" >&2; usage; exit 2 ;;
esac

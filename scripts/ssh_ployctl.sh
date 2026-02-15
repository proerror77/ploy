#!/usr/bin/env bash
set -euo pipefail

# Remote-safe command dispatcher for controlling this repo's daemon wrapper over SSH.
#
# Intended usage (recommended): add to `~/.ssh/authorized_keys` on the trading machine as a forced command:
#
# command="/path/to/repo/scripts/ssh_ployctl.sh",no-port-forwarding,no-agent-forwarding,no-X11-forwarding,no-pty <SSH_PUBKEY>
#
# Then from a remote controller (e.g., OpenClaw gateway):
#   ssh ploy@TRADING_HOST "status"
#   ssh ploy@TRADING_HOST "start false true"
#   ssh ploy@TRADING_HOST "start false true 123,456"
#   ssh ploy@TRADING_HOST "logs 200"
#   cat request.json | ssh ploy@TRADING_HOST "rpc"
#   ssh ploy@TRADING_HOST "stop"
#
# This script only allows a small allowlist of subcommands and never echoes secrets.

repo_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_dir"

orig="${SSH_ORIGINAL_COMMAND:-}"
orig="${orig#"${orig%%[![:space:]]*}"}" # ltrim

if [[ -z "$orig" ]]; then
  echo "missing command" >&2
  exit 2
fi

read -r cmd arg1 arg2 arg3 extra <<<"$orig"

if [[ -n "${extra:-}" ]]; then
  echo "too many arguments" >&2
  exit 2
fi

resolve_service() {
  local input="$1"
  case "$input" in
    sports|ploy-sports-pm) echo "ploy-sports-pm" ;;
    crypto|ploy-crypto-dryrun) echo "ploy-crypto-dryrun" ;;
    core|ploy) echo "ploy" ;;
    *)
      echo "invalid service: $input" >&2
      echo "allowed: sports|crypto|core" >&2
      return 1
      ;;
  esac
}

case "$cmd" in
  status)
    exec scripts/event_edge_daemon.sh status
    ;;
  stop)
    exec scripts/event_edge_daemon.sh stop
    ;;
  rpc)
    if [[ -n "${arg1:-}" || -n "${arg2:-}" ]]; then
      echo "rpc takes no arguments; pass JSON-RPC via stdin" >&2
      exit 2
    fi
    if [[ ! -x target/release/ploy ]]; then
      echo "ploy binary not found at target/release/ploy (build it first)" >&2
      exit 2
    fi
    exec ./target/release/ploy rpc
    ;;
  logs)
    # Optional: logs [n]
    if [[ -n "${arg1:-}" ]]; then
      [[ "$arg1" =~ ^[0-9]+$ ]] || { echo "logs: n must be integer" >&2; exit 2; }
      exec scripts/event_edge_daemon.sh logs "$arg1"
    fi
    exec scripts/event_edge_daemon.sh logs 200
    ;;
  start)
    # start [trade] [dry_run] [event_ids_csv]
    # trade: true|false
    # dry_run: true|false
    if [[ -z "${arg1:-}" || -z "${arg2:-}" ]]; then
      echo "start requires: start <trade:true|false> <dry_run:true|false>" >&2
      exit 2
    fi
    if [[ "$arg1" != "true" && "$arg1" != "false" ]]; then
      echo "start: trade must be true|false" >&2
      exit 2
    fi
    if [[ "$arg2" != "true" && "$arg2" != "false" ]]; then
      echo "start: dry_run must be true|false" >&2
      exit 2
    fi
    if [[ -n "${arg3:-}" ]]; then
      if ! [[ "$arg3" =~ ^[A-Za-z0-9,_-]+$ ]]; then
        echo "start: event_ids must match [A-Za-z0-9,_-]+" >&2
        exit 2
      fi
      exec scripts/event_edge_daemon.sh start "$arg1" "$arg2" "$arg3"
    fi
    exec scripts/event_edge_daemon.sh start "$arg1" "$arg2"
    ;;
  svc-status)
    if [[ -z "${arg1:-}" || -n "${arg2:-}" ]]; then
      echo "svc-status requires: svc-status <sports|crypto|core>" >&2
      exit 2
    fi
    service_name="$(resolve_service "$arg1")" || exit 2
    exec sudo systemctl --no-pager --full status "$service_name"
    ;;
  svc-start)
    if [[ -z "${arg1:-}" || -n "${arg2:-}" ]]; then
      echo "svc-start requires: svc-start <sports|crypto|core>" >&2
      exit 2
    fi
    service_name="$(resolve_service "$arg1")" || exit 2
    exec sudo systemctl restart "$service_name"
    ;;
  svc-stop)
    if [[ -z "${arg1:-}" || -n "${arg2:-}" ]]; then
      echo "svc-stop requires: svc-stop <sports|crypto|core>" >&2
      exit 2
    fi
    service_name="$(resolve_service "$arg1")" || exit 2
    exec sudo systemctl stop "$service_name"
    ;;
  svc-restart)
    if [[ -z "${arg1:-}" || -n "${arg2:-}" ]]; then
      echo "svc-restart requires: svc-restart <sports|crypto|core>" >&2
      exit 2
    fi
    service_name="$(resolve_service "$arg1")" || exit 2
    exec sudo systemctl restart "$service_name"
    ;;
  svc-logs)
    if [[ -z "${arg1:-}" ]]; then
      echo "svc-logs requires: svc-logs <sports|crypto|core> [lines]" >&2
      exit 2
    fi
    if [[ -n "${arg2:-}" ]] && ! [[ "$arg2" =~ ^[0-9]+$ ]]; then
      echo "svc-logs: lines must be integer" >&2
      exit 2
    fi
    service_name="$(resolve_service "$arg1")" || exit 2
    lines="${arg2:-200}"
    exec sudo journalctl -u "$service_name" -n "$lines" --no-pager
    ;;
  *)
    echo "unsupported command" >&2
    echo "allowed: status | start <trade> <dry_run> [event_ids_csv] | logs [n] | rpc | stop | svc-status <sports|crypto|core> | svc-start <sports|crypto|core> | svc-stop <sports|crypto|core> | svc-restart <sports|crypto|core> | svc-logs <sports|crypto|core> [n]" >&2
    exit 2
    ;;
esac

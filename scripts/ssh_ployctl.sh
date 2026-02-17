#!/usr/bin/env bash
set -euo pipefail

# Remote-safe command dispatcher for controlling this repo's daemon wrapper over SSH.
#
# Intended usage (recommended): add to `~/.ssh/authorized_keys` on the trading machine as a forced command:
#
# command="/path/to/repo/scripts/ssh_ployctl.sh",no-port-forwarding,no-agent-forwarding,no-X11-forwarding,no-pty <SSH_PUBKEY>
#
# Then from a remote controller (e.g., OpenClaw gateway):
#   ssh ploy@TRADING_HOST "svc-status sports"
#   ssh ploy@TRADING_HOST "svc-restart crypto"
#   ssh ploy@TRADING_HOST "svc-logs obh 200"
#   cat request.json | ssh ploy@TRADING_HOST "rpc"
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
    obh|orderbook-history|collector|ploy-orderbook-history) echo "ploy-orderbook-history" ;;
    maint|maintenance|ploy-maintenance.timer) echo "ploy-maintenance.timer" ;;
    core|ploy) echo "ploy" ;;
    *)
      echo "invalid service: $input" >&2
      echo "allowed: sports|crypto|obh|maint|core" >&2
      return 1
      ;;
  esac
}

case "$cmd" in
  rpc)
    if [[ -n "${arg1:-}" || -n "${arg2:-}" ]]; then
      echo "rpc takes no arguments; pass JSON-RPC via stdin" >&2
      exit 2
    fi
    ctl_cfg="${PLOY_CTL_CONFIG:-/opt/ploy/config/crypto_dry_run.toml}"
    if [[ ! -x /opt/ploy/bin/ploy ]]; then
      echo "ploy binary not found at /opt/ploy/bin/ploy" >&2
      exit 2
    fi
    exec /opt/ploy/bin/ploy --config "$ctl_cfg" rpc
    ;;
  svc-status)
    if [[ -z "${arg1:-}" || -n "${arg2:-}" ]]; then
      echo "svc-status requires: svc-status <sports|crypto|obh|maint|core>" >&2
      exit 2
    fi
    service_name="$(resolve_service "$arg1")" || exit 2
    exec sudo systemctl --no-pager --full status "$service_name"
    ;;
  svc-start)
    if [[ -z "${arg1:-}" || -n "${arg2:-}" ]]; then
      echo "svc-start requires: svc-start <sports|crypto|obh|maint|core>" >&2
      exit 2
    fi
    service_name="$(resolve_service "$arg1")" || exit 2
    exec sudo systemctl restart "$service_name"
    ;;
  svc-stop)
    if [[ -z "${arg1:-}" || -n "${arg2:-}" ]]; then
      echo "svc-stop requires: svc-stop <sports|crypto|obh|maint|core>" >&2
      exit 2
    fi
    service_name="$(resolve_service "$arg1")" || exit 2
    exec sudo systemctl stop "$service_name"
    ;;
  svc-restart)
    if [[ -z "${arg1:-}" || -n "${arg2:-}" ]]; then
      echo "svc-restart requires: svc-restart <sports|crypto|obh|maint|core>" >&2
      exit 2
    fi
    service_name="$(resolve_service "$arg1")" || exit 2
    exec sudo systemctl restart "$service_name"
    ;;
  svc-logs)
    if [[ -z "${arg1:-}" ]]; then
      echo "svc-logs requires: svc-logs <sports|crypto|obh|maint|core> [lines]" >&2
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
    echo "allowed: rpc | svc-status <sports|crypto|obh|maint|core> | svc-start <sports|crypto|obh|maint|core> | svc-stop <sports|crypto|obh|maint|core> | svc-restart <sports|crypto|obh|maint|core> | svc-logs <sports|crypto|obh|maint|core> [n]" >&2
    exit 2
    ;;
esac

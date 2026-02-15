#!/usr/bin/env bash
set -euo pipefail

# Manage multi-account ploy services on an EC2 host.
# Each account maps to:
# - /opt/ploy/config/<account>.toml
# - /opt/ploy/env/<account>.env
# - systemd service: ploy@<account>.service

if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
  SUDO=""
else
  SUDO="sudo"
fi

ROOT_DIR="/opt/ploy"
SYSTEMD_DIR="/etc/systemd/system"
SERVICE_TEMPLATE_SRC="${ROOT_DIR}/deployment/ploy@.service"
SERVICE_TEMPLATE_DST="${SYSTEMD_DIR}/ploy@.service"

usage() {
  cat <<'EOF'
Usage:
  scripts/ploy_accountctl.sh init
  scripts/ploy_accountctl.sh add <account> [market_slug] [dry_run:true|false] [health_port] [api_port]
  scripts/ploy_accountctl.sh start <account>
  scripts/ploy_accountctl.sh stop <account>
  scripts/ploy_accountctl.sh restart <account>
  scripts/ploy_accountctl.sh status <account>
  scripts/ploy_accountctl.sh logs <account> [lines]
  scripts/ploy_accountctl.sh list
  scripts/ploy_accountctl.sh remove <account>

Examples:
  scripts/ploy_accountctl.sh init
  scripts/ploy_accountctl.sh add acc1 sol-updown-15m true 8080 8081
  scripts/ploy_accountctl.sh start acc1
  scripts/ploy_accountctl.sh logs acc1 200
EOF
}

require_account() {
  local account="$1"
  if [[ ! "$account" =~ ^[A-Za-z0-9_-]+$ ]]; then
    echo "invalid account name: $account (allowed: A-Z a-z 0-9 _ -)" >&2
    exit 2
  fi
}

ensure_base() {
  if [[ ! -x "${ROOT_DIR}/bin/ploy" ]]; then
    echo "missing binary: ${ROOT_DIR}/bin/ploy" >&2
    echo "deploy/build it first (EC2): scripts/aws_ec2_deploy.sh ..." >&2
    exit 2
  fi
  if [[ ! -f "${ROOT_DIR}/config/default.toml" ]]; then
    echo "missing config template: ${ROOT_DIR}/config/default.toml" >&2
    exit 2
  fi
  if [[ ! -f "${SERVICE_TEMPLATE_SRC}" ]]; then
    echo "missing service template: ${SERVICE_TEMPLATE_SRC}" >&2
    exit 2
  fi

  $SUDO mkdir -p "${ROOT_DIR}/"{bin,config,env,data/logs,data/rpc,logs,deployment}
  if ! id -u ploy >/dev/null 2>&1; then
    $SUDO useradd --system --home "${ROOT_DIR}" --shell /usr/sbin/nologin --no-create-home ploy
  fi
  $SUDO chown -R ploy:ploy "${ROOT_DIR}"
}

set_value_in_section() {
  local file="$1"
  local section="$2"
  local key="$3"
  local value="$4"
  local tmp
  tmp="$(mktemp)"
  $SUDO cat "$file" | awk -v section="$section" -v key="$key" -v value="$value" '
    BEGIN { in_section = 0 }
    {
      if ($0 ~ ("^\\[" section "\\]$")) in_section = 1
      else if ($0 ~ /^\\[/) in_section = 0

      if (in_section == 1 && $0 ~ ("^" key "[[:space:]]*=")) {
        $0 = key " = " value
      }
      print
    }
  ' >"$tmp"
  $SUDO install -m 0644 "$tmp" "$file"
  rm -f "$tmp"
}

ensure_top_level_key() {
  local file="$1"
  local key="$2"
  local value="$3"
  local tmp
  tmp="$(mktemp)"
  if $SUDO grep -qE "^${key}[[:space:]]*=" "$file"; then
    $SUDO cat "$file" | awk -v key="$key" -v value="$value" '
      {
        if ($0 ~ ("^" key "[[:space:]]*=")) {
          $0 = key " = " value
        }
        print
      }
    ' >"$tmp"
  else
    $SUDO cat "$file" >"$tmp"
    printf "\n%s = %s\n" "$key" "$value" >>"$tmp"
  fi
  $SUDO install -m 0644 "$tmp" "$file"
  rm -f "$tmp"
}

install_template() {
  ensure_base
  $SUDO install -m 0644 "${SERVICE_TEMPLATE_SRC}" "${SERVICE_TEMPLATE_DST}"
  $SUDO systemctl daemon-reload
  echo "installed ${SERVICE_TEMPLATE_DST}"
}

add_account() {
  local account="$1"
  local market_slug="${2:-sol-updown-15m}"
  local dry_run="${3:-true}"
  local health_port="${4:-8080}"
  local api_port="${5:-8081}"
  local cfg_file="${ROOT_DIR}/config/${account}.toml"
  local env_file="${ROOT_DIR}/env/${account}.env"
  local market_quoted

  require_account "$account"
  if [[ "$dry_run" != "true" && "$dry_run" != "false" ]]; then
    echo "dry_run must be true|false" >&2
    exit 2
  fi

  install_template

  if [[ ! -f "$cfg_file" ]]; then
    $SUDO cp "${ROOT_DIR}/config/default.toml" "$cfg_file"
  fi

  market_quoted="\"${market_slug}\""
  set_value_in_section "$cfg_file" "market" "market_slug" "$market_quoted"
  set_value_in_section "$cfg_file" "dry_run" "enabled" "$dry_run"
  ensure_top_level_key "$cfg_file" "health_port" "$health_port"
  ensure_top_level_key "$cfg_file" "api_port" "$api_port"

  if [[ ! -f "$env_file" ]]; then
    local tmp
    tmp="$(mktemp)"
    cat >"$tmp" <<EOF
# Account: ${account}
# Fill these before going live.
POLYMARKET_PRIVATE_KEY=
# Optional proxy wallet funder:
# POLYMARKET_FUNDER=

PLOY_RPC_STATE_DIR=${ROOT_DIR}/data/rpc/${account}
RUST_LOG=info,ploy=info,sqlx=warn
EOF
    $SUDO install -m 0600 "$tmp" "$env_file"
    rm -f "$tmp"
  fi

  $SUDO mkdir -p "${ROOT_DIR}/data/rpc/${account}"
  $SUDO chown -R ploy:ploy "${ROOT_DIR}/"{config,env,data,logs}

  echo "account prepared: ${account}"
  echo "  config: ${cfg_file}"
  echo "  env:    ${env_file}"
  echo "start with: sudo systemctl enable --now ploy@${account}"
}

start_account() {
  local account="$1"
  require_account "$account"
  $SUDO systemctl enable --now "ploy@${account}"
  $SUDO systemctl --no-pager status "ploy@${account}"
}

stop_account() {
  local account="$1"
  require_account "$account"
  $SUDO systemctl stop "ploy@${account}" || true
  $SUDO systemctl disable "ploy@${account}" || true
}

restart_account() {
  local account="$1"
  require_account "$account"
  $SUDO systemctl restart "ploy@${account}"
  $SUDO systemctl --no-pager status "ploy@${account}"
}

status_account() {
  local account="$1"
  require_account "$account"
  $SUDO systemctl --no-pager status "ploy@${account}"
}

logs_account() {
  local account="$1"
  local lines="${2:-200}"
  require_account "$account"
  $SUDO journalctl -u "ploy@${account}" -n "$lines" --no-pager
}

list_accounts() {
  $SUDO systemctl list-unit-files "ploy@*.service" --no-pager || true
  echo
  $SUDO systemctl list-units "ploy@*.service" --no-pager || true
}

remove_account() {
  local account="$1"
  require_account "$account"
  stop_account "$account"
  $SUDO rm -f "${ROOT_DIR}/config/${account}.toml" "${ROOT_DIR}/env/${account}.env"
  $SUDO rm -rf "${ROOT_DIR}/data/rpc/${account}"
  echo "removed account: ${account}"
}

cmd="${1:-}"
case "$cmd" in
  init)
    install_template
    ;;
  add)
    [[ $# -ge 2 ]] || {
      usage
      exit 2
    }
    add_account "$2" "${3:-}" "${4:-}" "${5:-}" "${6:-}"
    ;;
  start)
    [[ $# -ge 2 ]] || {
      usage
      exit 2
    }
    start_account "$2"
    ;;
  stop)
    [[ $# -ge 2 ]] || {
      usage
      exit 2
    }
    stop_account "$2"
    ;;
  restart)
    [[ $# -ge 2 ]] || {
      usage
      exit 2
    }
    restart_account "$2"
    ;;
  status)
    [[ $# -ge 2 ]] || {
      usage
      exit 2
    }
    status_account "$2"
    ;;
  logs)
    [[ $# -ge 2 ]] || {
      usage
      exit 2
    }
    logs_account "$2" "${3:-200}"
    ;;
  list)
    list_accounts
    ;;
  remove)
    [[ $# -ge 2 ]] || {
      usage
      exit 2
    }
    remove_account "$2"
    ;;
  -h|--help|help|"")
    usage
    ;;
  *)
    echo "unknown command: ${cmd}" >&2
    usage
    exit 2
    ;;
esac

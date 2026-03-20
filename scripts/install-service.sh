#!/bin/bash
set -euo pipefail

# Install Ploy host support services on EC2.
# Legacy single-binary runtime units now live under deployment/archive/.

echo "==> Installing host support services..."

# Ensure runtime user exists
if ! id -u ploy >/dev/null 2>&1; then
  sudo useradd --system --home /opt/ploy --shell /usr/sbin/nologin --no-create-home ploy
fi

# Ensure required directories exist
sudo mkdir -p /opt/ploy/{config,env,data,logs,deployment,run}
sudo chown -R ploy:ploy /opt/ploy

# Copy service files that remain active in the workspace runtime path.
if [[ -f /opt/ploy/deployment/ploy-maintenance.service ]]; then
  sudo install -m 0644 /opt/ploy/deployment/ploy-maintenance.service /etc/systemd/system/ploy-maintenance.service
fi
if [[ -f /opt/ploy/deployment/ploy-maintenance.timer ]]; then
  sudo install -m 0644 /opt/ploy/deployment/ploy-maintenance.timer /etc/systemd/system/ploy-maintenance.timer
fi
if [[ -f /opt/ploy/deployment/ploy-platform-watchdog.service ]]; then
  sudo install -m 0644 /opt/ploy/deployment/ploy-platform-watchdog.service /etc/systemd/system/ploy-platform-watchdog.service
fi
if [[ -f /opt/ploy/deployment/ploy-platform-watchdog.timer ]]; then
  sudo install -m 0644 /opt/ploy/deployment/ploy-platform-watchdog.timer /etc/systemd/system/ploy-platform-watchdog.timer
fi

# Install workload configs/env templates if missing
if [[ -f /opt/ploy/deployment/config/sports_pm.toml && ! -f /opt/ploy/config/sports_pm.toml ]]; then
  sudo cp /opt/ploy/deployment/config/sports_pm.toml /opt/ploy/config/sports_pm.toml
fi
if [[ -f /opt/ploy/deployment/config/crypto_dry_run.toml && ! -f /opt/ploy/config/crypto_dry_run.toml ]]; then
  sudo cp /opt/ploy/deployment/config/crypto_dry_run.toml /opt/ploy/config/crypto_dry_run.toml
fi
if [[ -f /opt/ploy/deployment/config/crypto_live.toml && ! -f /opt/ploy/config/crypto_live.toml ]]; then
  sudo cp /opt/ploy/deployment/config/crypto_live.toml /opt/ploy/config/crypto_live.toml
fi
if [[ -f /opt/ploy/deployment/config/sports_live.toml && ! -f /opt/ploy/config/sports_live.toml ]]; then
  sudo cp /opt/ploy/deployment/config/sports_live.toml /opt/ploy/config/sports_live.toml
fi
if [[ -f /opt/ploy/deployment/config/platform_live.toml && ! -f /opt/ploy/config/platform_live.toml ]]; then
  sudo cp /opt/ploy/deployment/config/platform_live.toml /opt/ploy/config/platform_live.toml
fi
sudo mkdir -p /opt/ploy/data/state
if [[ ! -f /opt/ploy/data/state/deployments.json ]]; then
  if [[ -f /opt/ploy/deployment/deployments.json ]]; then
    sudo cp /opt/ploy/deployment/deployments.json /opt/ploy/data/state/deployments.json
  elif [[ -f /opt/ploy/data/state/deployments.json.sample ]]; then
    sudo cp /opt/ploy/data/state/deployments.json.sample /opt/ploy/data/state/deployments.json
  fi
fi
if [[ -f /opt/ploy/deployment/env.sports-pm.example && ! -f /opt/ploy/env/sports-pm.env ]]; then
  sudo cp /opt/ploy/deployment/env.sports-pm.example /opt/ploy/env/sports-pm.env
fi
if [[ -f /opt/ploy/deployment/env.crypto-dryrun.example && ! -f /opt/ploy/env/crypto-dryrun.env ]]; then
  sudo cp /opt/ploy/deployment/env.crypto-dryrun.example /opt/ploy/env/crypto-dryrun.env
fi
if [[ -f /opt/ploy/deployment/env.crypto-live.example && ! -f /opt/ploy/env/crypto-live.env ]]; then
  sudo cp /opt/ploy/deployment/env.crypto-live.example /opt/ploy/env/crypto-live.env
fi
if [[ -f /opt/ploy/deployment/env.sports-live.example && ! -f /opt/ploy/env/sports-live.env ]]; then
  sudo cp /opt/ploy/deployment/env.sports-live.example /opt/ploy/env/sports-live.env
fi
if [[ -f /opt/ploy/deployment/env.platform-live.example && ! -f /opt/ploy/env/platform-live.env ]]; then
  sudo cp /opt/ploy/deployment/env.platform-live.example /opt/ploy/env/platform-live.env
fi

# Avoid empty placeholders in workload env overlays overriding real values from /opt/ploy/.env.
sanitize_env_overlay() {
  local env_file="$1"
  [[ -f "$env_file" ]] || return 0
  sudo sed -i.bak -E '/^[A-Za-z_][A-Za-z0-9_]*=$/d; /^[A-Za-z_][A-Za-z0-9_]*=\"\"$/d' "$env_file"
}
sanitize_env_overlay /opt/ploy/env/sports-pm.env
sanitize_env_overlay /opt/ploy/env/crypto-dryrun.env
sanitize_env_overlay /opt/ploy/env/crypto-live.env
sanitize_env_overlay /opt/ploy/env/sports-live.env
sanitize_env_overlay /opt/ploy/env/platform-live.env

# Keep SQLx migration runner enabled by default to prevent startup on stale schema.
ensure_env_true() {
  local env_file="$1"
  local key="$2"
  if sudo grep -qE "^${key}=" "$env_file"; then
    sudo sed -i "s/^${key}=.*/${key}=true/" "$env_file"
  else
    echo "${key}=true" | sudo tee -a "$env_file" >/dev/null
  fi
}

ensure_env_default() {
  local env_file="$1"
  local key="$2"
  local value="$3"
  [[ -f "$env_file" ]] || return 0
  if ! sudo grep -qE "^${key}=" "$env_file"; then
    echo "${key}=${value}" | sudo tee -a "$env_file" >/dev/null
  fi
}

ensure_sqlx_migrations_enabled() {
  local env_file="$1"
  [[ -f "$env_file" ]] || return 0
  ensure_env_true "$env_file" "PLOY_RUN_SQLX_MIGRATIONS"
  ensure_env_true "$env_file" "PLOY_REQUIRE_SQLX_MIGRATIONS"
}

ensure_account_budget_defaults() {
  local env_file="$1"
  [[ -f "$env_file" ]] || return 0
  ensure_env_default "$env_file" "PLOY_RISK__ACCOUNT_RESERVE_PCT" "0.15"
  ensure_env_default "$env_file" "PLOY_RISK__CRYPTO_ALLOCATION_PCT" "0.6667"
  ensure_env_default "$env_file" "PLOY_RISK__SPORTS_ALLOCATION_PCT" "0.3333"
  ensure_env_default "$env_file" "PLOY_RISK__CIRCUIT_BREAKER_AUTO_RECOVER" "true"
  ensure_env_default "$env_file" "PLOY_RISK__CIRCUIT_BREAKER_COOLDOWN_SECS" "300"
}

ensure_coordinator_heartbeat_defaults() {
  local env_file="$1"
  [[ -f "$env_file" ]] || return 0
  ensure_env_default "$env_file" "PLOY_COORDINATOR__HEARTBEAT_STALE_WARN_COOLDOWN_SECS" "300"
}

ensure_sports_allocator_defaults() {
  local env_file="$1"
  [[ -f "$env_file" ]] || return 0
  ensure_env_default "$env_file" "PLOY_COORDINATOR__SPORTS_ALLOCATOR_ENABLED" "true"
  ensure_env_default "$env_file" "PLOY_COORDINATOR__SPORTS_AUTO_SPLIT_BY_ACTIVE_MARKETS" "true"
  ensure_env_default "$env_file" "PLOY_COORDINATOR__SPORTS_MARKET_CAP_PCT" "0.35"
}

ensure_kelly_defaults() {
  local env_file="$1"
  [[ -f "$env_file" ]] || return 0
  # Keep the system active under conservative caps: floor tiny-but-positive Kelly sizes.
  ensure_env_default "$env_file" "PLOY_COORDINATOR__KELLY_MIN_SHARES" "1"
}

ensure_venue_minimum_defaults() {
  local env_file="$1"
  [[ -f "$env_file" ]] || return 0
  # Prevent deterministic 400s from Polymarket (min shares / min notional).
  ensure_env_default "$env_file" "PLOY_COORDINATOR__MIN_ORDER_SHARES" "5"
  ensure_env_default "$env_file" "PLOY_COORDINATOR__MIN_ORDER_NOTIONAL_USD" "1"
}

ensure_sqlx_migrations_enabled /opt/ploy/.env
ensure_sqlx_migrations_enabled /opt/ploy/env/sports-pm.env
ensure_sqlx_migrations_enabled /opt/ploy/env/crypto-dryrun.env
ensure_sqlx_migrations_enabled /opt/ploy/env/crypto-live.env
ensure_sqlx_migrations_enabled /opt/ploy/env/sports-live.env
ensure_sqlx_migrations_enabled /opt/ploy/env/platform-live.env
ensure_account_budget_defaults /opt/ploy/.env
ensure_account_budget_defaults /opt/ploy/env/sports-pm.env
ensure_account_budget_defaults /opt/ploy/env/crypto-dryrun.env
ensure_account_budget_defaults /opt/ploy/env/crypto-live.env
ensure_account_budget_defaults /opt/ploy/env/sports-live.env
ensure_account_budget_defaults /opt/ploy/env/platform-live.env
ensure_coordinator_heartbeat_defaults /opt/ploy/.env
ensure_coordinator_heartbeat_defaults /opt/ploy/env/sports-pm.env
ensure_coordinator_heartbeat_defaults /opt/ploy/env/crypto-dryrun.env
ensure_coordinator_heartbeat_defaults /opt/ploy/env/crypto-live.env
ensure_coordinator_heartbeat_defaults /opt/ploy/env/sports-live.env
ensure_coordinator_heartbeat_defaults /opt/ploy/env/platform-live.env
ensure_kelly_defaults /opt/ploy/env/sports-pm.env
ensure_kelly_defaults /opt/ploy/env/crypto-dryrun.env
ensure_kelly_defaults /opt/ploy/env/crypto-live.env
ensure_kelly_defaults /opt/ploy/env/sports-live.env
ensure_kelly_defaults /opt/ploy/env/platform-live.env
ensure_venue_minimum_defaults /opt/ploy/env/sports-pm.env
ensure_venue_minimum_defaults /opt/ploy/env/crypto-dryrun.env
ensure_venue_minimum_defaults /opt/ploy/env/crypto-live.env
ensure_venue_minimum_defaults /opt/ploy/env/sports-live.env
ensure_venue_minimum_defaults /opt/ploy/env/platform-live.env
ensure_env_default "/opt/ploy/.env" "PLOY_DEPLOYMENTS_FILE" "/opt/ploy/data/state/deployments.json"
ensure_env_default /opt/ploy/env/sports-pm.env "PLOY_DEPLOYMENTS_FILE" "/opt/ploy/data/state/deployments.json"
ensure_env_default /opt/ploy/env/crypto-dryrun.env "PLOY_DEPLOYMENTS_FILE" "/opt/ploy/data/state/deployments.json"
ensure_env_default /opt/ploy/env/crypto-live.env "PLOY_DEPLOYMENTS_FILE" "/opt/ploy/data/state/deployments.json"
ensure_env_default /opt/ploy/env/sports-live.env "PLOY_DEPLOYMENTS_FILE" "/opt/ploy/data/state/deployments.json"
ensure_env_default /opt/ploy/env/platform-live.env "PLOY_DEPLOYMENTS_FILE" "/opt/ploy/data/state/deployments.json"
ensure_sports_allocator_defaults /opt/ploy/.env
ensure_sports_allocator_defaults /opt/ploy/env/sports-pm.env
ensure_sports_allocator_defaults /opt/ploy/env/sports-live.env
ensure_sports_allocator_defaults /opt/ploy/env/platform-live.env

sudo chmod 600 /opt/ploy/env/*.env 2>/dev/null || true
sudo chown ploy:ploy /opt/ploy/config/*.toml /opt/ploy/env/*.env 2>/dev/null || true

# Reload systemd
sudo systemctl daemon-reload

# Enable host support timers on boot
if [[ -f /etc/systemd/system/ploy-platform-watchdog.timer ]]; then
  sudo systemctl enable --now ploy-platform-watchdog.timer
fi
if [[ -f /etc/systemd/system/ploy-maintenance.timer ]]; then
  sudo systemctl enable ploy-maintenance.timer
fi

echo "==> Host support services installed"
echo ""
echo "Commands:"
echo "  sudo systemctl status ploy-platform-watchdog.timer  # Inspect watchdog loop"
echo "  sudo systemctl start ploy-maintenance.service       # Run maintenance once"
echo "  sudo systemctl status ploy-maintenance.timer        # Inspect maintenance schedule"

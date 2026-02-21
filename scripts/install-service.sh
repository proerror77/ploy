#!/bin/bash
set -euo pipefail

# Install Ploy systemd service on EC2
# Run this on the EC2 instance after first deploy

echo "==> Installing systemd service..."

# Ensure runtime user exists
if ! id -u ploy >/dev/null 2>&1; then
  sudo useradd --system --home /opt/ploy --shell /usr/sbin/nologin --no-create-home ploy
fi

# Ensure required directories exist
sudo mkdir -p /opt/ploy/{config,env,data,logs,deployment,run}
sudo chown -R ploy:ploy /opt/ploy

# Copy service files
sudo install -m 0644 /opt/ploy/deployment/ploy.service /etc/systemd/system/ploy.service
if [[ -f /opt/ploy/deployment/ploy@.service ]]; then
  sudo install -m 0644 /opt/ploy/deployment/ploy@.service /etc/systemd/system/ploy@.service
fi
if [[ -f /opt/ploy/deployment/ploy-sports-pm.service ]]; then
  sudo install -m 0644 /opt/ploy/deployment/ploy-sports-pm.service /etc/systemd/system/ploy-sports-pm.service
fi
if [[ -f /opt/ploy/deployment/ploy-crypto-dryrun.service ]]; then
  sudo install -m 0644 /opt/ploy/deployment/ploy-crypto-dryrun.service /etc/systemd/system/ploy-crypto-dryrun.service
fi
if [[ -f /opt/ploy/deployment/ploy-strategy-pattern-memory-dryrun.service ]]; then
  sudo install -m 0644 /opt/ploy/deployment/ploy-strategy-pattern-memory-dryrun.service /etc/systemd/system/ploy-strategy-pattern-memory-dryrun.service
fi
if [[ -f /opt/ploy/deployment/ploy-strategy-momentum-dryrun.service ]]; then
  sudo install -m 0644 /opt/ploy/deployment/ploy-strategy-momentum-dryrun.service /etc/systemd/system/ploy-strategy-momentum-dryrun.service
fi
if [[ -f /opt/ploy/deployment/ploy-strategy-split-arb-dryrun.service ]]; then
  sudo install -m 0644 /opt/ploy/deployment/ploy-strategy-split-arb-dryrun.service /etc/systemd/system/ploy-strategy-split-arb-dryrun.service
fi

# Install workload configs/env templates if missing
if [[ -f /opt/ploy/deployment/config/sports_pm.toml && ! -f /opt/ploy/config/sports_pm.toml ]]; then
  sudo cp /opt/ploy/deployment/config/sports_pm.toml /opt/ploy/config/sports_pm.toml
fi
if [[ -f /opt/ploy/deployment/config/crypto_dry_run.toml && ! -f /opt/ploy/config/crypto_dry_run.toml ]]; then
  sudo cp /opt/ploy/deployment/config/crypto_dry_run.toml /opt/ploy/config/crypto_dry_run.toml
fi
if [[ -f /opt/ploy/deployment/env.sports-pm.example && ! -f /opt/ploy/env/sports-pm.env ]]; then
  sudo cp /opt/ploy/deployment/env.sports-pm.example /opt/ploy/env/sports-pm.env
fi
if [[ -f /opt/ploy/deployment/env.crypto-dryrun.example && ! -f /opt/ploy/env/crypto-dryrun.env ]]; then
  sudo cp /opt/ploy/deployment/env.crypto-dryrun.example /opt/ploy/env/crypto-dryrun.env
fi

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

ensure_sports_allocator_defaults() {
  local env_file="$1"
  [[ -f "$env_file" ]] || return 0
  ensure_env_default "$env_file" "PLOY_COORDINATOR__SPORTS_ALLOCATOR_ENABLED" "true"
  ensure_env_default "$env_file" "PLOY_COORDINATOR__SPORTS_AUTO_SPLIT_BY_ACTIVE_MARKETS" "true"
  ensure_env_default "$env_file" "PLOY_COORDINATOR__SPORTS_MARKET_CAP_PCT" "0.35"
}

ensure_sqlx_migrations_enabled /opt/ploy/.env
ensure_sqlx_migrations_enabled /opt/ploy/env/sports-pm.env
ensure_sqlx_migrations_enabled /opt/ploy/env/crypto-dryrun.env
ensure_account_budget_defaults /opt/ploy/.env
ensure_account_budget_defaults /opt/ploy/env/sports-pm.env
ensure_account_budget_defaults /opt/ploy/env/crypto-dryrun.env
ensure_sports_allocator_defaults /opt/ploy/.env
ensure_sports_allocator_defaults /opt/ploy/env/sports-pm.env

sudo chmod 600 /opt/ploy/env/*.env 2>/dev/null || true
sudo chown ploy:ploy /opt/ploy/config/*.toml /opt/ploy/env/*.env 2>/dev/null || true

# Reload systemd
sudo systemctl daemon-reload

# Enable service to start on boot
sudo systemctl enable ploy

echo "==> Service installed"
echo ""
echo "Commands:"
echo "  sudo systemctl start ploy   # Start"
echo "  sudo systemctl stop ploy    # Stop"
echo "  sudo systemctl status ploy  # Status"
echo "  sudo systemctl start ploy-sports-pm       # Start sports PM workload"
echo "  sudo systemctl start ploy-crypto-dryrun   # Start crypto dry-run workload"
echo "  sudo systemctl start ploy@acc1   # Start account acc1 (multi-account)"
echo "  sudo systemctl status ploy@acc1  # Status account acc1"
echo "  journalctl -u ploy -f       # View logs"

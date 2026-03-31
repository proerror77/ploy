#!/usr/bin/env bash
# Deploy ploy-runner and switch directional strategy from old monolith to new binary.
# Run on tango-1-1 as root.
set -euo pipefail

DEPLOY_DIR="/root/ploy"
BIN_DIR="${DEPLOY_DIR}/bin"
CONFIG_DIR="${DEPLOY_DIR}/config/strategies"
SERVICE_NAME="ploy-strategy-directional-dryrun"

echo "=== Step 1: Install ploy-runner binary ==="
install -m 755 /tmp/ploy-runner "${BIN_DIR}/ploy-runner"
ls -la "${BIN_DIR}/ploy-runner"

echo "=== Step 2: Install unified config ==="
cp "${CONFIG_DIR}/pm_5m_directional_dryrun.toml" "${CONFIG_DIR}/pm_5m_directional_dryrun.toml.legacy"
cat > "${CONFIG_DIR}/02-pm5d.unified.toml" << 'TOML'
# Unified pm_5m_directional config for StrategyRuntime
# Same file drives backtest, dry-run, and live — change [runtime].mode only
#
# Params aligned to backtest report (2026-03-26): 696 trades, 67.1% WR, Sharpe 10.15

[runtime]
mode = "dryrun"         # "backtest" | "dryrun" | "live"

[strategy]
symbols = ["BTCUSDT", "ETHUSDT", "SOLUSDT", "XRPUSDT", "DOGEUSDT", "HYPEUSDT", "BNBUSDT"]

# Probability model
vol_floor = 0.001
min_probability = 0.62
min_z_score = 0.35

# Price gates
min_entry_price = 0.15
max_entry_price = 0.85
no_trade_zone_min = 0.45
no_trade_zone_max = 0.55

# Edge threshold (5% = backtest entry_threshold)
min_edge = 0.05

# Timing gates
min_time_remaining_secs = 60
max_time_remaining_secs = 300
cooldown_secs = 60

# Position sizing
quantity = 25.0
max_positions = 3
max_daily_trades = 1000

[execution]
# Execution simulation (backtest + dry-run modes)
use_spread = true
spread_pct = 0.02
enable_partial_fills = true
min_fill_pct = 0.5
enable_market_impact = true
impact_coefficient = 0.1
default_depth_shares = 500
TOML

echo "=== Step 3: Update systemd service ==="
cat > "/etc/systemd/system/${SERVICE_NAME}.service" << 'UNIT'
[Unit]
Description=Ploy S2: pm_5m_directional dry-run (unified StrategyRuntime)
After=network-online.target postgresql@16-main.service
Wants=network-online.target

[Service]
Type=simple
WorkingDirectory=/root/ploy
EnvironmentFile=/root/ploy/config/live.env
Environment=PLOY_CONFIG_DIR=/root/ploy/config
Environment=RUST_LOG=info,hyper_util=off,hyper=off,reqwest=off,h2=off,rustls=off
ExecStart=/root/ploy/bin/ploy-runner --config /root/ploy/config/strategies/02-pm5d.unified.toml --dry-run --foreground
Restart=always
RestartSec=5
MemoryHigh=1280M
MemoryMax=1536M
OOMPolicy=kill
KillSignal=SIGINT
TimeoutStopSec=20
User=root

[Install]
WantedBy=multi-user.target
UNIT

echo "=== Step 4: Reload and restart ==="
systemctl daemon-reload
systemctl restart "${SERVICE_NAME}"
sleep 3
systemctl status "${SERVICE_NAME}" --no-pager

echo "=== Step 5: Verify logs ==="
journalctl -u "${SERVICE_NAME}" --no-pager -n 30

echo "=== Step 6: Verify hardening ==="
systemctl show "${SERVICE_NAME}" -p MemoryHigh,MemoryMax,OOMPolicy,Restart,RestartSec

echo "=== Done ==="

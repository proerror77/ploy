#!/bin/bash
set -euo pipefail

# Ploy Trading Bot - AWS EC2 Installation Script
# Run as root or with sudo

PLOY_USER="ploy"
PLOY_HOME="/opt/ploy"
PLOY_BIN="${PLOY_HOME}/bin/ploy"

echo "=========================================="
echo "Ploy Trading Bot - Installation Script"
echo "=========================================="

# Check if running as root
if [[ $EUID -ne 0 ]]; then
   echo "This script must be run as root (use sudo)"
   exit 1
fi

# Create ploy user if not exists
if ! id -u ${PLOY_USER} &>/dev/null; then
    echo "Creating user: ${PLOY_USER}"
    useradd -r -m -s /bin/bash ${PLOY_USER}
fi

# Create directory structure
echo "Creating directory structure..."
mkdir -p ${PLOY_HOME}/{bin,config,data,logs,models}
chown -R ${PLOY_USER}:${PLOY_USER} ${PLOY_HOME}

# Install binary (expects binary to be uploaded to /tmp/ploy)
if [[ -f /tmp/ploy ]]; then
    echo "Installing ploy binary..."

    # Backup existing binary
    if [[ -f ${PLOY_BIN} ]]; then
        cp ${PLOY_BIN} ${PLOY_BIN}.bak
        echo "Backed up existing binary to ${PLOY_BIN}.bak"
    fi

    cp /tmp/ploy ${PLOY_BIN}
    chmod +x ${PLOY_BIN}
    chown ${PLOY_USER}:${PLOY_USER} ${PLOY_BIN}
    rm /tmp/ploy
    echo "Binary installed successfully"
else
    echo "Warning: No binary found at /tmp/ploy"
fi

# Install systemd service
echo "Installing systemd service..."
cp /opt/ploy/deployment/ploy.service /etc/systemd/system/ploy.service
systemctl daemon-reload

# Create .env file template if not exists
if [[ ! -f ${PLOY_HOME}/.env ]]; then
    echo "Creating .env template..."
    cat > ${PLOY_HOME}/.env << 'EOF'
# Ploy Trading Bot Environment Configuration
# Fill in the values before starting the service

# Database connection
DATABASE_URL=postgres://ploy:PASSWORD@RDS_ENDPOINT:5432/ploy

# Wallet (REQUIRED for live trading)
WALLET_PRIVATE_KEY=

# Trading parameters (optional, defaults shown)
TRADING_SYMBOL=BTCUSDT
TRADING_MARKET=will-btc-go-up-15m
TRADE_SIZE=1.0
MAX_POSITION=50.0
MIN_CONFIDENCE=0.6

# Logging
RUST_LOG=info

# Optional: Grok API for agent mode
# GROK_API_KEY=
EOF
    chown ${PLOY_USER}:${PLOY_USER} ${PLOY_HOME}/.env
    chmod 600 ${PLOY_HOME}/.env
    echo "Created .env template at ${PLOY_HOME}/.env"
    echo "IMPORTANT: Edit this file with your actual credentials!"
fi

# Create production config if not exists
if [[ ! -f ${PLOY_HOME}/config/production.toml ]]; then
    echo "Creating production config..."
    cat > ${PLOY_HOME}/config/production.toml << 'EOF'
[market]
ws_url = "wss://ws-subscriptions-clob.polymarket.com/ws/market"
rest_url = "https://clob.polymarket.com"

[strategy]
shares = 20
window_min = 2
move_pct = 0.15
sum_target = 0.95
fee_buffer = 0.005
slippage_buffer = 0.02
profit_buffer = 0.01

[execution]
order_timeout_ms = 5000
max_retries = 3
max_spread_bps = 500
poll_interval_ms = 500

[risk]
max_single_exposure_usd = 100
min_remaining_seconds = 30
max_consecutive_failures = 3
daily_loss_limit_usd = 500

[database]
max_connections = 5

[dry_run]
enabled = false

[logging]
level = "info"
json = true

[health]
port = 8080
EOF
    chown ${PLOY_USER}:${PLOY_USER} ${PLOY_HOME}/config/production.toml
fi

# Fetch secrets from AWS Secrets Manager (if configured)
echo "Checking for AWS Secrets Manager configuration..."
if command -v aws &> /dev/null; then
    REGION=$(curl -s http://169.254.169.254/latest/meta-data/placement/region 2>/dev/null || echo "us-east-1")

    # Try to get database password
    DB_SECRET=$(aws secretsmanager get-secret-value --secret-id ploy/db-password --region ${REGION} 2>/dev/null || true)
    if [[ -n "${DB_SECRET}" ]]; then
        DB_HOST=$(echo ${DB_SECRET} | jq -r '.SecretString | fromjson | .host')
        DB_USER=$(echo ${DB_SECRET} | jq -r '.SecretString | fromjson | .username')
        DB_PASS=$(echo ${DB_SECRET} | jq -r '.SecretString | fromjson | .password')
        DB_NAME=$(echo ${DB_SECRET} | jq -r '.SecretString | fromjson | .dbname')

        # Update .env with database URL
        sed -i "s|DATABASE_URL=.*|DATABASE_URL=postgres://${DB_USER}:${DB_PASS}@${DB_HOST}:5432/${DB_NAME}|" ${PLOY_HOME}/.env
        echo "Updated DATABASE_URL from Secrets Manager"
    fi

    # Try to get wallet key
    WALLET_SECRET=$(aws secretsmanager get-secret-value --secret-id ploy/wallet-key --region ${REGION} 2>/dev/null || true)
    if [[ -n "${WALLET_SECRET}" ]]; then
        WALLET_KEY=$(echo ${WALLET_SECRET} | jq -r '.SecretString')
        sed -i "s|WALLET_PRIVATE_KEY=.*|WALLET_PRIVATE_KEY=${WALLET_KEY}|" ${PLOY_HOME}/.env
        echo "Updated WALLET_PRIVATE_KEY from Secrets Manager"
    fi
fi

echo ""
echo "=========================================="
echo "Installation complete!"
echo "=========================================="
echo ""
echo "Next steps:"
echo "1. Edit ${PLOY_HOME}/.env with your credentials"
echo "2. Upload trained model to ${PLOY_HOME}/models/leadlag/"
echo "3. Enable and start the service:"
echo "   sudo systemctl enable ploy"
echo "   sudo systemctl start ploy"
echo ""
echo "Monitor logs with:"
echo "   sudo journalctl -u ploy -f"
echo ""

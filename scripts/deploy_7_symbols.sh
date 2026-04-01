#!/bin/bash
# Deploy new ploy binary and restart services

set -e

echo "=== Deploying new ploy binary with 7 crypto symbols ==="

# Stop services
echo "Stopping ploy-platform..."
systemctl stop ploy-platform

# Backup old binary
echo "Backing up old binary..."
cp /root/ploy/bin/ploy /root/ploy/bin/ploy.backup-$(date +%Y%m%d-%H%M%S)

# Deploy new binary
echo "Deploying new binary..."
mv /tmp/ploy.new /root/ploy/bin/ploy
chmod +x /root/ploy/bin/ploy

# Verify binary
echo "Verifying binary..."
ls -lh /root/ploy/bin/ploy
/root/ploy/bin/ploy --version

# Restart services
echo "Restarting ploy-platform..."
systemctl start ploy-platform

# Wait for startup
sleep 10

# Check status
echo "Checking service status..."
systemctl status ploy-platform --no-pager -l | head -20

echo ""
echo "=== Checking Binance WebSocket subscriptions ==="
sleep 5
tail -50 /var/log/ploy/platform.log | grep -i "binance.*symbol\|Starting Binance" | tail -10

echo ""
echo "=== Deployment complete ==="
echo "New symbols: BTC, ETH, SOL, XRP, DOGE, HYPE, BNB"

#!/bin/bash
set -euo pipefail

# Install Ploy systemd service on EC2
# Run this on the EC2 instance after first deploy

echo "==> Installing systemd service..."

# Copy service file
sudo cp /opt/ploy/deployment/ploy.service /etc/systemd/system/ploy.service

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
echo "  journalctl -u ploy -f       # View logs"

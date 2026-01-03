#!/bin/bash
set -euo pipefail

# Ploy Deployment Script for AWS EC2
# Usage: ./deploy.sh <ec2-host> [--first-run]

EC2_HOST="${1:-}"
FIRST_RUN="${2:-}"

if [[ -z "$EC2_HOST" ]]; then
    echo "Usage: ./deploy.sh <ec2-host> [--first-run]"
    echo "Example: ./deploy.sh ubuntu@ec2-xx-xx-xx-xx.compute.amazonaws.com"
    exit 1
fi

echo "==> Building release binary..."
cargo build --release

echo "==> Uploading binary to $EC2_HOST..."
scp target/release/ploy "$EC2_HOST:/tmp/ploy"

if [[ "$FIRST_RUN" == "--first-run" ]]; then
    echo "==> First run setup..."
    ssh "$EC2_HOST" << 'REMOTE'
        set -e
        
        # Create ploy user
        sudo useradd -r -s /bin/false ploy || true
        
        # Create directories
        sudo mkdir -p /opt/ploy/{data,logs,config}
        sudo chown -R ploy:ploy /opt/ploy
        
        # Install PostgreSQL
        sudo apt-get update
        sudo apt-get install -y postgresql postgresql-contrib
        
        # Create database
        sudo -u postgres createuser ploy || true
        sudo -u postgres createdb -O ploy ploy || true
        
        echo "==> First run setup complete"
        echo "==> Please create /opt/ploy/.env with your secrets"
        echo "==> Then create /opt/ploy/config/config.toml"
REMOTE
fi

echo "==> Deploying binary..."
ssh "$EC2_HOST" << 'REMOTE'
    set -e
    
    # Stop service if running
    sudo systemctl stop ploy || true
    
    # Install binary
    sudo mv /tmp/ploy /opt/ploy/ploy
    sudo chmod +x /opt/ploy/ploy
    sudo chown ploy:ploy /opt/ploy/ploy
    
    # Start service
    sudo systemctl start ploy
    
    echo "==> Deployment complete"
    sudo systemctl status ploy --no-pager
REMOTE

echo "==> Done!"

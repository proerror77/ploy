#!/bin/bash

# Ploy Trading System - EC2 部署腳本（使用 AWS SSM）
# 目標：tango_1_1 (i-0b29ca671375dad53)

set -e

INSTANCE_ID="i-0b29ca671375dad53"
EC2_IP="13.113.155.16"

echo "🚀 開始部署 Ploy Trading System 到 EC2..."
echo "   實例 ID: $INSTANCE_ID"
echo "   IP 地址: $EC2_IP"
echo ""

# 檢查 SSM Agent 是否可用
echo "1. 檢查 SSM 連接..."
if aws ssm describe-instance-information --filters "Key=InstanceIds,Values=$INSTANCE_ID" --query 'InstanceInformationList[0].PingStatus' --output text 2>/dev/null | grep -q "Online"; then
    echo "   ✅ SSM 連接可用"
    USE_SSM=true
else
    echo "   ⚠️  SSM 不可用，需要 SSH 密鑰"
    echo ""
    echo "請選擇部署方式："
    echo "1. 手動部署（我會提供詳細步驟）"
    echo "2. 退出並配置 SSH 密鑰"
    echo ""
    read -p "請選擇 (1/2): " choice

    if [ "$choice" = "1" ]; then
        echo ""
        echo "📋 手動部署步驟："
        echo ""
        echo "=== 步驟 1：準備文件 ==="
        echo "前端構建文件已在：./dist/"
        echo ""
        echo "=== 步驟 2：連接到 EC2 ==="
        echo "使用 AWS Console 的 EC2 Instance Connect 或配置 SSH 密鑰"
        echo ""
        echo "=== 步驟 3：在 EC2 上執行 ==="
        echo ""
        cat << 'MANUAL_STEPS'
# 1. 創建目錄
mkdir -p ~/ploy/{frontend,backend}

# 2. 安裝依賴
sudo apt-get update
sudo apt-get install -y nginx

# 安裝 Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source $HOME/.cargo/env

# 3. 配置 Nginx
sudo tee /etc/nginx/sites-available/ploy > /dev/null << 'EOF'
server {
    listen 80;
    server_name _;

    location / {
        root /home/ubuntu/ploy/frontend;
        try_files $uri $uri/ /index.html;
    }

    location /api {
        proxy_pass http://localhost:8080;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection 'upgrade';
        proxy_set_header Host $host;
        proxy_cache_bypass $http_upgrade;
    }
}
EOF

sudo ln -sf /etc/nginx/sites-available/ploy /etc/nginx/sites-enabled/
sudo rm -f /etc/nginx/sites-enabled/default
sudo nginx -t
sudo systemctl restart nginx

# 4. 上傳文件（從本地執行）
# 使用 scp 或 AWS S3 上傳 dist/ 目錄到 ~/ploy/frontend/
# 上傳後端代碼到 ~/ploy/backend/

# 5. 構建後端
cd ~/ploy/backend
cargo build --release

# 6. 創建服務
sudo tee /etc/systemd/system/ploy-backend.service > /dev/null << 'EOF'
[Unit]
Description=Ploy Trading Backend
After=network.target

[Service]
Type=simple
User=ubuntu
WorkingDirectory=/home/ubuntu/ploy/backend
ExecStart=/home/ubuntu/ploy/backend/target/release/ploy
Restart=always

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl start ploy-backend
sudo systemctl enable ploy-backend

# 7. 檢查狀態
sudo systemctl status ploy-backend
sudo systemctl status nginx
MANUAL_STEPS

        echo ""
        echo "=== 步驟 4：使用 S3 傳輸文件（推薦）==="
        echo ""
        echo "# 在本地執行："
        echo "aws s3 cp dist/ s3://your-bucket/ploy-frontend/ --recursive"
        echo "tar czf ploy-backend.tar.gz --exclude='target' --exclude='node_modules' --exclude='ploy-frontend' --exclude='.git' Cargo.toml Cargo.lock src/ examples/"
        echo "aws s3 cp ploy-backend.tar.gz s3://your-bucket/"
        echo ""
        echo "# 在 EC2 上執行："
        echo "aws s3 cp s3://your-bucket/ploy-frontend/ ~/ploy/frontend/ --recursive"
        echo "aws s3 cp s3://your-bucket/ploy-backend.tar.gz ~/ploy/backend/"
        echo "cd ~/ploy/backend && tar xzf ploy-backend.tar.gz"
        echo ""

        exit 0
    else
        echo "請配置 SSH 密鑰後重試"
        exit 1
    fi
fi

# 使用 SSM 部署
echo "2. 使用 SSM 部署..."

# 創建部署腳本
cat > /tmp/deploy_commands.sh << 'DEPLOY_SCRIPT'
#!/bin/bash
set -e

echo "開始部署..."

# 創建目錄
mkdir -p ~/ploy/{frontend,backend}

# 安裝依賴
sudo apt-get update -qq
sudo apt-get install -y nginx

# 安裝 Rust
if ! command -v cargo &> /dev/null; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source $HOME/.cargo/env
fi

# 配置 Nginx
sudo tee /etc/nginx/sites-available/ploy > /dev/null << 'EOF'
server {
    listen 80;
    server_name _;

    location / {
        root /home/ubuntu/ploy/frontend;
        try_files $uri $uri/ /index.html;
    }

    location /api {
        proxy_pass http://localhost:8080;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection 'upgrade';
        proxy_set_header Host $host;
        proxy_cache_bypass $http_upgrade;
    }
}
EOF

sudo ln -sf /etc/nginx/sites-available/ploy /etc/nginx/sites-enabled/
sudo rm -f /etc/nginx/sites-enabled/default
sudo nginx -t
sudo systemctl restart nginx

echo "部署腳本執行完成"
DEPLOY_SCRIPT

# 執行部署腳本
echo "3. 在 EC2 上執行部署腳本..."
aws ssm send-command \
    --instance-ids "$INSTANCE_ID" \
    --document-name "AWS-RunShellScript" \
    --parameters "commands=[$(cat /tmp/deploy_commands.sh | jq -Rs .)]" \
    --output text \
    --query 'Command.CommandId'

echo "   ✅ 部署命令已發送"
echo ""
echo "⚠️  注意：文件傳輸需要手動完成"
echo ""
echo "請使用以下方法之一上傳文件："
echo ""
echo "方法 1：使用 S3（推薦）"
echo "  1. 上傳到 S3："
echo "     aws s3 cp dist/ s3://your-bucket/ploy-frontend/ --recursive"
echo "  2. 在 EC2 上下載："
echo "     aws s3 cp s3://your-bucket/ploy-frontend/ ~/ploy/frontend/ --recursive"
echo ""
echo "方法 2：使用 EC2 Instance Connect"
echo "  1. 在 AWS Console 中使用 EC2 Instance Connect"
echo "  2. 手動上傳文件"
echo ""
echo "方法 3：配置 SSH 密鑰"
echo "  1. 下載 bn-watcher-key.pem"
echo "  2. 使用 scp 上傳文件"
echo ""

echo "部署完成後訪問："
echo "  http://$EC2_IP"
echo ""

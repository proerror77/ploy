#!/bin/bash

# Ploy Trading System - EC2 部署腳本
# 目標：tango_1_1 (13.113.155.16)

set -e

EC2_IP="13.113.155.16"
EC2_USER="ubuntu"
SSH_KEY="~/.ssh/tango_1_1.pem"  # 請確保你有正確的 SSH 密鑰

echo "🚀 開始部署 Ploy Trading System 到 EC2..."
echo ""

# 1. 測試 SSH 連接
echo "1. 測試 SSH 連接..."
if ssh -i $SSH_KEY -o StrictHostKeyChecking=no $EC2_USER@$EC2_IP "echo '連接成功'" 2>/dev/null; then
    echo "   ✅ SSH 連接成功"
else
    echo "   ❌ SSH 連接失敗"
    echo "   請確保："
    echo "   - SSH 密鑰路徑正確：$SSH_KEY"
    echo "   - EC2 安全組允許 SSH (port 22)"
    echo "   - 使用正確的用戶名：$EC2_USER"
    exit 1
fi

# 2. 在 EC2 上創建目錄
echo "2. 在 EC2 上創建目錄..."
ssh -i $SSH_KEY $EC2_USER@$EC2_IP "mkdir -p ~/ploy/{frontend,backend}"
echo "   ✅ 目錄創建完成"

# 3. 上傳前端構建文件
echo "3. 上傳前端構建文件..."
scp -i $SSH_KEY -r dist/* $EC2_USER@$EC2_IP:~/ploy/frontend/
echo "   ✅ 前端文件上傳完成"

# 4. 上傳後端代碼
echo "4. 上傳後端代碼..."
# 排除不需要的文件
tar czf /tmp/ploy-backend.tar.gz \
    --exclude='target' \
    --exclude='node_modules' \
    --exclude='ploy-frontend' \
    --exclude='.git' \
    --exclude='data' \
    --exclude='results' \
    Cargo.toml Cargo.lock src/ examples/ migrations/

scp -i $SSH_KEY /tmp/ploy-backend.tar.gz $EC2_USER@$EC2_IP:~/ploy/backend/
ssh -i $SSH_KEY $EC2_USER@$EC2_IP "cd ~/ploy/backend && tar xzf ploy-backend.tar.gz && rm ploy-backend.tar.gz"
echo "   ✅ 後端代碼上傳完成"

# 5. 安裝依賴和配置環境
echo "5. 在 EC2 上安裝依賴..."
ssh -i $SSH_KEY $EC2_USER@$EC2_IP << 'ENDSSH'
    # 更新系統
    sudo apt-get update -qq

    # 安裝 Nginx（如果還沒安裝）
    if ! command -v nginx &> /dev/null; then
        echo "   安裝 Nginx..."
        sudo apt-get install -y nginx
    fi

    # 安裝 Rust（如果還沒安裝）
    if ! command -v cargo &> /dev/null; then
        echo "   安裝 Rust..."
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
        source $HOME/.cargo/env
    fi

    # 安裝 PostgreSQL 客戶端（如果需要）
    if ! command -v psql &> /dev/null; then
        echo "   安裝 PostgreSQL 客戶端..."
        sudo apt-get install -y postgresql-client
    fi

    echo "   ✅ 依賴安裝完成"
ENDSSH

# 6. 配置 Nginx
echo "6. 配置 Nginx..."
ssh -i $SSH_KEY $EC2_USER@$EC2_IP << 'ENDSSH'
    # 創建 Nginx 配置
    sudo tee /etc/nginx/sites-available/ploy > /dev/null << 'EOF'
server {
    listen 80;
    server_name _;

    # 前端
    location / {
        root /home/ubuntu/ploy/frontend;
        try_files $uri $uri/ /index.html;
    }

    # 後端 API
    location /api {
        proxy_pass http://localhost:8080;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection 'upgrade';
        proxy_set_header Host $host;
        proxy_cache_bypass $http_upgrade;
    }

    # WebSocket
    location /ws {
        proxy_pass http://localhost:8080;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "Upgrade";
        proxy_set_header Host $host;
    }
}
EOF

    # 啟用配置
    sudo ln -sf /etc/nginx/sites-available/ploy /etc/nginx/sites-enabled/
    sudo rm -f /etc/nginx/sites-enabled/default

    # 測試配置
    sudo nginx -t

    # 重啟 Nginx
    sudo systemctl restart nginx
    sudo systemctl enable nginx

    echo "   ✅ Nginx 配置完成"
ENDSSH

# 7. 構建後端
echo "7. 構建後端..."
ssh -i $SSH_KEY $EC2_USER@$EC2_IP << 'ENDSSH'
    source $HOME/.cargo/env
    cd ~/ploy/backend
    cargo build --release
    echo "   ✅ 後端構建完成"
ENDSSH

# 8. 創建 systemd 服務
echo "8. 創建後端服務..."
ssh -i $SSH_KEY $EC2_USER@$EC2_IP << 'ENDSSH'
    # 創建 systemd 服務文件
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
RestartSec=10

[Install]
WantedBy=multi-user.target
EOF

    # 重新加載 systemd
    sudo systemctl daemon-reload

    echo "   ✅ 後端服務創建完成"
ENDSSH

# 9. 啟動服務
echo "9. 啟動服務..."
ssh -i $SSH_KEY $EC2_USER@$EC2_IP << 'ENDSSH'
    # 啟動後端服務
    sudo systemctl start ploy-backend
    sudo systemctl enable ploy-backend

    # 檢查狀態
    sleep 2
    sudo systemctl status ploy-backend --no-pager

    echo "   ✅ 服務啟動完成"
ENDSSH

# 10. 驗證部署
echo ""
echo "10. 驗證部署..."
echo "   前端地址：http://$EC2_IP"
echo "   後端 API：http://$EC2_IP/api"
echo ""

# 測試前端
if curl -s -o /dev/null -w "%{http_code}" http://$EC2_IP | grep -q "200"; then
    echo "   ✅ 前端訪問成功"
else
    echo "   ⚠️  前端訪問失敗，請檢查 Nginx 配置"
fi

echo ""
echo "🎉 部署完成！"
echo ""
echo "訪問地址："
echo "  前端：http://$EC2_IP"
echo "  NBA Swing：http://$EC2_IP/nba-swing"
echo ""
echo "管理命令："
echo "  查看後端日誌：ssh -i $SSH_KEY $EC2_USER@$EC2_IP 'sudo journalctl -u ploy-backend -f'"
echo "  重啟後端：ssh -i $SSH_KEY $EC2_USER@$EC2_IP 'sudo systemctl restart ploy-backend'"
echo "  重啟 Nginx：ssh -i $SSH_KEY $EC2_USER@$EC2_IP 'sudo systemctl restart nginx'"
echo ""

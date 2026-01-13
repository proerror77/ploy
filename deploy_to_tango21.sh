#!/bin/bash

# Ploy Trading System - 部署到 tango-2-1
# 目標：tango-2-1 (i-01de34df55726073d, 3.112.247.26)

set -e

INSTANCE_ID="i-01de34df55726073d"
EC2_IP="3.112.247.26"
BUCKET_NAME="ploy-deployment-$(date +%s)"

echo "🚀 Ploy Trading System - 部署到 tango-2-1"
echo ""
echo "目標 EC2："
echo "  實例 ID: $INSTANCE_ID"
echo "  實例名稱: tango-2-1"
echo "  IP 地址: $EC2_IP"
echo "  實例類型: t3.micro (1 GB RAM)"
echo "  S3 Bucket: $BUCKET_NAME"
echo ""

# 步驟 1：創建 S3 bucket
echo "📦 步驟 1/5：創建 S3 bucket..."
if aws s3 mb s3://$BUCKET_NAME 2>/dev/null; then
    echo "   ✅ S3 bucket 創建成功"
else
    echo "   ⚠️  S3 bucket 可能已存在，繼續..."
fi

# 步驟 2：上傳前端文件
echo ""
echo "📤 步驟 2/5：上傳前端文件到 S3..."
if [ -d "ploy-frontend/dist" ]; then
    aws s3 cp ploy-frontend/dist/ s3://$BUCKET_NAME/frontend/ --recursive --quiet
    echo "   ✅ 前端文件上傳完成"
elif [ -d "dist" ]; then
    aws s3 cp dist/ s3://$BUCKET_NAME/frontend/ --recursive --quiet
    echo "   ✅ 前端文件上傳完成"
else
    echo "   ❌ dist 目錄不存在，請先構建前端"
    echo "   運行：cd ploy-frontend && npm run build"
    exit 1
fi

# 步驟 3：打包並上傳後端代碼
echo ""
echo "📤 步驟 3/5：打包並上傳後端代碼..."
tar czf /tmp/ploy-backend.tar.gz \
    --exclude='target' \
    --exclude='node_modules' \
    --exclude='ploy-frontend' \
    --exclude='.git' \
    --exclude='data' \
    --exclude='results' \
    --exclude='dist' \
    Cargo.toml Cargo.lock src/ examples/ migrations/ 2>/dev/null

aws s3 cp /tmp/ploy-backend.tar.gz s3://$BUCKET_NAME/ --quiet
rm /tmp/ploy-backend.tar.gz
echo "   ✅ 後端代碼上傳完成"

# 步驟 4：創建部署腳本
echo ""
echo "📝 步驟 4/5：創建部署腳本..."
cat > /tmp/deploy_on_tango21.sh << DEPLOY_SCRIPT
#!/bin/bash
set -e

echo "🚀 開始在 tango-2-1 上部署..."

# 1. 備份現有配置（如果有）
if [ -d ~/ploy ]; then
    echo "發現現有 ploy 目錄，創建備份..."
    mv ~/ploy ~/ploy.backup.\$(date +%Y%m%d_%H%M%S)
fi

# 2. 創建目錄
mkdir -p ~/ploy/{frontend,backend}

# 3. 從 S3 下載文件
echo "📥 下載文件..."
aws s3 cp s3://$BUCKET_NAME/frontend/ ~/ploy/frontend/ --recursive --quiet
aws s3 cp s3://$BUCKET_NAME/ploy-backend.tar.gz ~/ploy/backend/ --quiet

# 4. 解壓後端代碼
cd ~/ploy/backend
tar xzf ploy-backend.tar.gz
rm ploy-backend.tar.gz

# 5. 安裝依賴
echo "📦 安裝依賴..."
sudo apt-get update -qq
sudo apt-get install -y nginx build-essential pkg-config libssl-dev -qq

# 6. 安裝 Rust（如果還沒安裝）
if ! command -v cargo &> /dev/null; then
    echo "📦 安裝 Rust..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
fi
source \$HOME/.cargo/env

# 7. 配置 Nginx
echo "⚙️  配置 Nginx..."
sudo tee /etc/nginx/sites-available/ploy > /dev/null << 'EOF'
server {
    listen 80;
    server_name _;

    # 前端
    location / {
        root /home/ubuntu/ploy/frontend;
        try_files \$uri \$uri/ /index.html;
    }

    # 後端 API
    location /api {
        proxy_pass http://localhost:8080;
        proxy_http_version 1.1;
        proxy_set_header Upgrade \$http_upgrade;
        proxy_set_header Connection 'upgrade';
        proxy_set_header Host \$host;
        proxy_cache_bypass \$http_upgrade;
    }

    # WebSocket
    location /ws {
        proxy_pass http://localhost:8080;
        proxy_http_version 1.1;
        proxy_set_header Upgrade \$http_upgrade;
        proxy_set_header Connection "Upgrade";
        proxy_set_header Host \$host;
    }
}
EOF

sudo ln -sf /etc/nginx/sites-available/ploy /etc/nginx/sites-enabled/
sudo rm -f /etc/nginx/sites-enabled/default
sudo nginx -t
sudo systemctl restart nginx
sudo systemctl enable nginx

# 8. 構建後端
echo "🔨 構建後端..."
cd ~/ploy/backend
cargo build --release

# 9. 創建 systemd 服務
echo "⚙️  創建服務..."
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
Environment="RUST_LOG=info"

[Install]
WantedBy=multi-user.target
EOF

# 10. 啟動服務
sudo systemctl daemon-reload
sudo systemctl restart ploy-backend
sudo systemctl enable ploy-backend

# 11. 檢查狀態
echo ""
echo "📊 服務狀態："
sudo systemctl status nginx --no-pager | head -5
sudo systemctl status ploy-backend --no-pager | head -5

echo ""
echo "✅ 部署完成！"
echo ""
echo "訪問地址："
echo "  前端：http://$EC2_IP"
echo "  NBA Swing：http://$EC2_IP/nba-swing"
echo ""
DEPLOY_SCRIPT

# 上傳部署腳本到 S3
aws s3 cp /tmp/deploy_on_tango21.sh s3://$BUCKET_NAME/ --quiet
echo "   ✅ 部署腳本創建完成"

# 步驟 5：在 EC2 上執行部署
echo ""
echo "🚀 步驟 5/5：在 EC2 上執行部署..."
echo ""
echo "請選擇執行方式："
echo "1. 使用 SSM Session Manager（推薦）"
echo "2. 手動執行"
echo ""
read -p "請選擇 (1/2): " choice

if [ "$choice" = "1" ]; then
    echo ""
    echo "正在通過 SSM 執行部署..."

    # 檢查 SSM 是否可用
    if aws ssm describe-instance-information --filters "Key=InstanceIds,Values=$INSTANCE_ID" --query 'InstanceInformationList[0].PingStatus' --output text 2>/dev/null | grep -q "Online"; then
        # 創建執行命令
        COMMAND_ID=$(aws ssm send-command \
            --instance-ids "$INSTANCE_ID" \
            --document-name "AWS-RunShellScript" \
            --parameters "commands=['aws s3 cp s3://$BUCKET_NAME/deploy_on_tango21.sh /tmp/ && chmod +x /tmp/deploy_on_tango21.sh && /tmp/deploy_on_tango21.sh']" \
            --output text \
            --query 'Command.CommandId')

        echo "   命令 ID: $COMMAND_ID"
        echo "   正在執行，請稍候..."

        # 等待命令完成
        sleep 5

        # 獲取命令輸出
        aws ssm get-command-invocation \
            --command-id "$COMMAND_ID" \
            --instance-id "$INSTANCE_ID" \
            --query 'StandardOutputContent' \
            --output text

        echo ""
        echo "✅ 部署完成！"
    else
        echo "   ⚠️  SSM 不可用，請使用手動方式"
        choice="2"
    fi
fi

if [ "$choice" = "2" ]; then
    echo ""
    echo "📋 手動執行步驟："
    echo ""
    echo "1. 連接到 EC2："
    echo "   aws ssm start-session --target $INSTANCE_ID"
    echo "   或使用 AWS Console 的 EC2 Instance Connect"
    echo ""
    echo "2. 在 EC2 上執行："
    echo "   aws s3 cp s3://$BUCKET_NAME/deploy_on_tango21.sh /tmp/"
    echo "   chmod +x /tmp/deploy_on_tango21.sh"
    echo "   /tmp/deploy_on_tango21.sh"
    echo ""
fi

echo ""
echo "🎉 部署流程完成！"
echo ""
echo "訪問地址："
echo "  前端：http://$EC2_IP"
echo "  NBA Swing：http://$EC2_IP/nba-swing"
echo "  策略監控：http://$EC2_IP/monitor-strategy"
echo ""
echo "管理命令："
echo "  連接 EC2：aws ssm start-session --target $INSTANCE_ID"
echo "  查看日誌：sudo journalctl -u ploy-backend -f"
echo "  重啟服務：sudo systemctl restart ploy-backend"
echo ""
echo "S3 Bucket：$BUCKET_NAME"
echo "（部署完成後可以刪除：aws s3 rb s3://$BUCKET_NAME --force）"
echo ""

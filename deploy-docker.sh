#!/bin/bash

# Ploy Trading System - Docker 部署腳本
# 在 EC2 上執行此腳本以部署完整系統

set -e

echo "🐳 Ploy Trading System - Docker 部署"
echo "====================================="
echo ""

# 顏色定義
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color

# 檢查是否為 root
if [ "$EUID" -eq 0 ]; then
    echo -e "${RED}❌ 請不要以 root 用戶運行此腳本${NC}"
    echo "使用: ./deploy-docker.sh"
    exit 1
fi

# 步驟 1: 安裝 Docker
echo -e "${YELLOW}📦 步驟 1/6: 安裝 Docker...${NC}"
if ! command -v docker &> /dev/null; then
    echo "  安裝 Docker..."
    curl -fsSL https://get.docker.com -o get-docker.sh
    sudo sh get-docker.sh
    sudo usermod -aG docker $USER
    rm get-docker.sh
    echo -e "${GREEN}  ✅ Docker 安裝完成${NC}"
else
    echo -e "${GREEN}  ✅ Docker 已安裝${NC}"
fi

# 步驟 2: 安裝 Docker Compose
echo ""
echo -e "${YELLOW}📦 步驟 2/6: 安裝 Docker Compose...${NC}"
if ! command -v docker-compose &> /dev/null; then
    echo "  安裝 Docker Compose..."
    sudo curl -L "https://github.com/docker/compose/releases/latest/download/docker-compose-$(uname -s)-$(uname -m)" -o /usr/local/bin/docker-compose
    sudo chmod +x /usr/local/bin/docker-compose
    echo -e "${GREEN}  ✅ Docker Compose 安裝完成${NC}"
else
    echo -e "${GREEN}  ✅ Docker Compose 已安裝${NC}"
fi

# 驗證安裝
echo ""
echo "  驗證安裝..."
docker --version
docker-compose --version

# 步驟 3: 下載項目文件
echo ""
echo -e "${YELLOW}📥 步驟 3/6: 下載項目文件...${NC}"

# 備份現有部署
if [ -d ~/ploy ]; then
    echo "  備份現有部署..."
    mv ~/ploy ~/ploy.backup.$(date +%Y%m%d_%H%M%S)
fi

# 創建目錄
mkdir -p ~/ploy
cd ~/ploy

# 從 S3 下載（如果可用）或從 Git 克隆
if command -v aws &> /dev/null; then
    echo "  從 S3 下載文件..."
    # 這裡可以從 S3 下載預構建的文件
    # aws s3 cp s3://your-bucket/ploy.tar.gz .
    # tar xzf ploy.tar.gz

    # 暫時使用 git clone
    echo "  從 GitHub 克隆項目..."
    git clone https://github.com/proerror77/ploy.git .
else
    echo "  從 GitHub 克隆項目..."
    git clone https://github.com/proerror77/ploy.git .
fi

echo -e "${GREEN}  ✅ 項目文件已下載${NC}"

# 步驟 4: 配置環境變量
echo ""
echo -e "${YELLOW}⚙️  步驟 4/6: 配置環境變量...${NC}"

# 複製環境變量文件
if [ -f .env.production ]; then
    cp .env.production .env
    echo -e "${GREEN}  ✅ 環境變量已配置${NC}"
else
    echo -e "${YELLOW}  ⚠️  .env.production 不存在，使用默認配置${NC}"
    cat > .env << 'EOF'
POLYMARKET_PRIVATE_KEY=
THE_ODDS_API_KEY=
GROK_API_KEY=
DATABASE_URL=postgresql://ploy:ploy@postgres:5432/ploy
RUST_LOG=info,ploy=debug,sqlx=warn
EOF
fi

# 步驟 5: 構建前端
echo ""
echo -e "${YELLOW}🔨 步驟 5/6: 構建前端...${NC}"
if [ -d ploy-frontend ]; then
    cd ploy-frontend

    # 安裝 Node.js（如果需要）
    if ! command -v node &> /dev/null; then
        echo "  安裝 Node.js..."
        curl -fsSL https://deb.nodesource.com/setup_18.x | sudo -E bash -
        sudo apt-get install -y nodejs
    fi

    echo "  安裝依賴..."
    npm ci --quiet

    echo "  構建前端..."
    npm run build

    cd ..
    echo -e "${GREEN}  ✅ 前端構建完成${NC}"
else
    echo -e "${YELLOW}  ⚠️  前端目錄不存在，跳過${NC}"
fi

# 步驟 6: 啟動 Docker 容器
echo ""
echo -e "${YELLOW}🚀 步驟 6/6: 啟動 Docker 容器...${NC}"

# 停止現有容器
echo "  停止現有容器..."
docker-compose -f docker-compose.prod.yml down 2>/dev/null || true

# 構建並啟動
echo "  構建 Docker 鏡像（這可能需要 5-10 分鐘）..."
docker-compose -f docker-compose.prod.yml build --no-cache

echo "  啟動容器..."
docker-compose -f docker-compose.prod.yml up -d

# 等待服務啟動
echo ""
echo "  等待服務啟動..."
sleep 10

# 檢查容器狀態
echo ""
echo -e "${YELLOW}📊 容器狀態:${NC}"
docker-compose -f docker-compose.prod.yml ps

# 檢查日誌
echo ""
echo -e "${YELLOW}📋 最近的日誌:${NC}"
docker-compose -f docker-compose.prod.yml logs --tail=20

# 測試連接
echo ""
echo -e "${YELLOW}🔍 測試服務...${NC}"
sleep 5

# 獲取 EC2 公網 IP
EC2_IP=$(curl -s http://169.254.169.254/latest/meta-data/public-ipv4 2>/dev/null || echo "localhost")

# 測試前端
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" http://localhost || echo "000")
if [ "$HTTP_CODE" = "200" ]; then
    echo -e "${GREEN}  ✅ 前端服務正常 (HTTP $HTTP_CODE)${NC}"
else
    echo -e "${YELLOW}  ⚠️  前端服務響應: HTTP $HTTP_CODE${NC}"
fi

# 完成
echo ""
echo "=========================================="
echo -e "${GREEN}✅ 部署完成！${NC}"
echo "=========================================="
echo ""
echo "🌐 訪問地址:"
echo "  前端: http://$EC2_IP"
echo "  NBA Swing: http://$EC2_IP/nba-swing"
echo "  API 健康檢查: http://$EC2_IP/health"
echo ""
echo "📊 管理命令:"
echo "  查看日誌: docker-compose -f docker-compose.prod.yml logs -f"
echo "  查看狀態: docker-compose -f docker-compose.prod.yml ps"
echo "  重啟服務: docker-compose -f docker-compose.prod.yml restart"
echo "  停止服務: docker-compose -f docker-compose.prod.yml down"
echo ""
echo "🗄️  數據庫:"
echo "  連接: docker exec -it ploy-postgres psql -U ploy -d ploy"
echo "  查看表: docker exec -it ploy-postgres psql -U ploy -d ploy -c '\\dt'"
echo ""
echo "💡 提示:"
echo "  - 如果需要重新構建: docker-compose -f docker-compose.prod.yml up -d --build"
echo "  - 查看後端日誌: docker-compose -f docker-compose.prod.yml logs -f ploy-backend"
echo "  - 查看 Nginx 日誌: docker-compose -f docker-compose.prod.yml logs -f nginx"
echo ""

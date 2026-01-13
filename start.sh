#!/bin/bash
# 快速启动 Ploy Trading 系统（API + 前端）

set -e

echo "🚀 Ploy Trading System - Quick Start"
echo "===================================="
echo ""

# 检查 Docker
if ! command -v docker &> /dev/null; then
    echo "❌ Docker not found. Please install Docker first."
    exit 1
fi

# 检查 Node.js
if ! command -v node &> /dev/null; then
    echo "❌ Node.js not found. Please install Node.js first."
    exit 1
fi

# 检查 Rust
if ! command -v cargo &> /dev/null; then
    echo "❌ Rust not found. Please install Rust first."
    exit 1
fi

echo "✅ All dependencies found"
echo ""

# 步骤 1: 启动数据库
echo "📦 Step 1: Starting PostgreSQL database..."
if docker ps | grep -q ploy-postgres; then
    echo "   Database already running"
else
    docker run -d \
        --name ploy-postgres \
        -e POSTGRES_DB=ploy \
        -e POSTGRES_USER=ploy \
        -e POSTGRES_PASSWORD=password \
        -p 5432:5432 \
        postgres:16-alpine

    echo "   Waiting for database to be ready..."
    sleep 5
fi

# 步骤 2: 运行数据库迁移
echo ""
echo "🔧 Step 2: Running database migrations..."
export DATABASE_URL="postgresql://ploy:password@localhost:5432/ploy"

if command -v sqlx &> /dev/null; then
    sqlx migrate run
    echo "   ✅ Migrations completed"
else
    echo "   ⚠️  sqlx-cli not found. Install with: cargo install sqlx-cli"
    echo "   Skipping migrations..."
fi

# 步骤 3: 编译并启动 API 服务器
echo ""
echo "🔨 Step 3: Building and starting API server..."
echo "   This may take a few minutes on first run..."

# 在后台启动 API 服务器
cargo run --example api_server > api_server.log 2>&1 &
API_PID=$!

echo "   API server starting (PID: $API_PID)..."
echo "   Waiting for API server to be ready..."
sleep 10

# 检查 API 服务器是否运行
if kill -0 $API_PID 2>/dev/null; then
    echo "   ✅ API server running on http://localhost:8080"
else
    echo "   ❌ API server failed to start. Check api_server.log"
    exit 1
fi

# 步骤 4: 安装前端依赖（如果需要）
echo ""
echo "📦 Step 4: Setting up frontend..."
cd ploy-frontend

if [ ! -d "node_modules" ]; then
    echo "   Installing frontend dependencies..."
    npm install
else
    echo "   Dependencies already installed"
fi

# 步骤 5: 启动前端
echo ""
echo "🎨 Step 5: Starting frontend..."
npm run dev > ../frontend.log 2>&1 &
FRONTEND_PID=$!

echo "   Frontend starting (PID: $FRONTEND_PID)..."
sleep 5

cd ..

# 完成
echo ""
echo "✅ System started successfully!"
echo ""
echo "📊 Access the dashboard:"
echo "   Frontend: http://localhost:3000"
echo "   API:      http://localhost:8080"
echo "   WebSocket: ws://localhost:8080/ws"
echo ""
echo "📝 Logs:"
echo "   API:      tail -f api_server.log"
echo "   Frontend: tail -f frontend.log"
echo ""
echo "🛑 To stop the system:"
echo "   kill $API_PID $FRONTEND_PID"
echo "   docker stop ploy-postgres"
echo ""
echo "💡 Test the API:"
echo "   curl http://localhost:8080/api/system/status"
echo ""

# 保存 PIDs 到文件
echo "$API_PID" > .api_pid
echo "$FRONTEND_PID" > .frontend_pid

echo "Press Ctrl+C to stop monitoring logs..."
echo ""

# 监控日志
tail -f api_server.log frontend.log

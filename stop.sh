#!/bin/bash
# 停止 Ploy Trading 系统

echo "🛑 Stopping Ploy Trading System..."

# 停止 API 服务器
if [ -f .api_pid ]; then
    API_PID=$(cat .api_pid)
    if kill -0 $API_PID 2>/dev/null; then
        echo "   Stopping API server (PID: $API_PID)..."
        kill $API_PID
    fi
    rm .api_pid
fi

# 停止前端
if [ -f .frontend_pid ]; then
    FRONTEND_PID=$(cat .frontend_pid)
    if kill -0 $FRONTEND_PID 2>/dev/null; then
        echo "   Stopping frontend (PID: $FRONTEND_PID)..."
        kill $FRONTEND_PID
    fi
    rm .frontend_pid
fi

# 停止数据库（可选）
read -p "Stop PostgreSQL database? (y/N) " -n 1 -r
echo
if [[ $REPLY =~ ^[Yy]$ ]]; then
    echo "   Stopping database..."
    docker stop ploy-postgres
fi

echo "✅ System stopped"

#!/bin/bash

# NBA Swing Strategy - 快速啟動腳本

echo "🚀 NBA Swing Strategy - 啟動中..."
echo ""

# 檢查是否在正確的目錄
if [ ! -d "ploy-frontend" ]; then
    echo "❌ 錯誤：請在項目根目錄運行此腳本"
    exit 1
fi

# 進入前端目錄
cd ploy-frontend

# 檢查 node_modules 是否存在
if [ ! -d "node_modules" ]; then
    echo "📦 首次運行，正在安裝依賴..."
    npm install
    echo ""
fi

echo "✅ 依賴已就緒"
echo ""
echo "🌐 啟動前端開發服務器..."
echo ""
echo "📍 訪問地址："
echo "   主頁：http://localhost:5173"
echo "   NBA Swing：http://localhost:5173/nba-swing"
echo ""
echo "💡 提示："
echo "   - 在左側邊欄點擊 'NBA Swing' 查看策略監控"
echo "   - 按 Ctrl+C 停止服務器"
echo ""
echo "---"
echo ""

# 啟動開發服務器
npm run dev

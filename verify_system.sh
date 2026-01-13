#!/bin/bash

# Ploy Trading System - 驗證腳本

echo "🔍 Ploy Trading System - 系統驗證"
echo ""

# 檢查前端目錄
echo "1. 檢查前端目錄..."
if [ -d "ploy-frontend" ]; then
    echo "   ✅ 前端目錄存在"
else
    echo "   ❌ 前端目錄不存在"
    exit 1
fi

# 檢查 node_modules
echo "2. 檢查依賴..."
if [ -d "ploy-frontend/node_modules" ]; then
    echo "   ✅ 依賴已安裝"
else
    echo "   ⚠️  依賴未安裝，正在安裝..."
    cd ploy-frontend && npm install && cd ..
fi

# 檢查關鍵文件
echo "3. 檢查關鍵文件..."
files=(
    "ploy-frontend/src/App.tsx"
    "ploy-frontend/src/pages/NBASwingMonitor.tsx"
    "ploy-frontend/src/components/Layout.tsx"
    "ploy-frontend/package.json"
)

for file in "${files[@]}"; do
    if [ -f "$file" ]; then
        echo "   ✅ $file"
    else
        echo "   ❌ $file 不存在"
    fi
done

# 檢查後端文件
echo "4. 檢查後端組件..."
backend_files=(
    "src/strategy/nba_winprob.rs"
    "src/strategy/nba_filters.rs"
    "src/strategy/nba_entry.rs"
    "src/strategy/nba_exit.rs"
    "src/strategy/nba_state_machine.rs"
    "src/strategy/nba_data_collector.rs"
)

for file in "${backend_files[@]}"; do
    if [ -f "$file" ]; then
        echo "   ✅ $file"
    else
        echo "   ❌ $file 不存在"
    fi
done

# 檢查文檔
echo "5. 檢查文檔..."
docs=(
    "QUICK_OVERVIEW.md"
    "START_HERE.md"
    "COMPLETE_SYSTEM_SUMMARY.md"
    "MASTER_INDEX.md"
    "FINAL_INTEGRATION_REPORT.md"
)

for doc in "${docs[@]}"; do
    if [ -f "$doc" ]; then
        echo "   ✅ $doc"
    else
        echo "   ❌ $doc 不存在"
    fi
done

# 嘗試構建前端
echo "6. 測試前端構建..."
cd ploy-frontend
if npm run build > /dev/null 2>&1; then
    echo "   ✅ 前端構建成功"
else
    echo "   ❌ 前端構建失敗"
    cd ..
    exit 1
fi
cd ..

echo ""
echo "✅ 系統驗證完成！"
echo ""
echo "🚀 啟動前端："
echo "   ./start_frontend.sh"
echo ""
echo "📚 查看文檔："
echo "   cat QUICK_OVERVIEW.md"
echo ""

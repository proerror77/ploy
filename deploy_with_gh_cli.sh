#!/bin/bash

# Ploy Trading System - 使用 GitHub CLI 部署到 tango-2-1

set -e

echo "🚀 使用 GitHub CLI 部署到 tango-2-1"
echo ""

# 檢查 GitHub CLI
if ! command -v gh &> /dev/null; then
    echo "❌ GitHub CLI 未安裝"
    echo "安裝：brew install gh"
    exit 1
fi

# 檢查登錄狀態
if ! gh auth status &> /dev/null; then
    echo "❌ 未登錄 GitHub"
    echo "登錄：gh auth login"
    exit 1
fi

echo "✅ GitHub CLI 已就緒"
echo ""

# 步驟 1：設置 AWS Secrets
echo "📝 步驟 1/5：設置 AWS Secrets"
echo ""
echo "需要設置以下 Secrets："
echo "  - AWS_ACCESS_KEY_ID"
echo "  - AWS_SECRET_ACCESS_KEY"
echo ""

read -p "是否需要設置 AWS Secrets？(y/n): " setup_secrets

if [ "$setup_secrets" = "y" ]; then
    echo ""
    echo "請輸入 AWS 憑證："
    read -p "AWS_ACCESS_KEY_ID: " aws_key_id
    read -sp "AWS_SECRET_ACCESS_KEY: " aws_secret_key
    echo ""

    # 設置 secrets
    echo "$aws_key_id" | gh secret set AWS_ACCESS_KEY_ID
    echo "$aws_secret_key" | gh secret set AWS_SECRET_ACCESS_KEY

    echo "✅ AWS Secrets 已設置"
else
    echo "⏭️  跳過 Secrets 設置（假設已設置）"
fi

echo ""

# 步驟 2：檢查 git 狀態
echo "📊 步驟 2/5：檢查 git 狀態"
git status --short
echo ""

# 步驟 3：提交並推送代碼
echo "📤 步驟 3/5：提交並推送代碼"
echo ""

read -p "是否提交並推送代碼？(y/n): " push_code

if [ "$push_code" = "y" ]; then
    # 添加所有文件
    git add .

    # 提交
    read -p "提交信息（默認：Deploy with GitHub Actions）: " commit_msg
    commit_msg=${commit_msg:-"Deploy with GitHub Actions"}
    git commit -m "$commit_msg" || echo "沒有需要提交的更改"

    # 推送
    git push origin main

    echo "✅ 代碼已推送"
else
    echo "⏭️  跳過推送代碼"
fi

echo ""

# 步驟 4：觸發 GitHub Actions 工作流
echo "🚀 步驟 4/5：觸發部署工作流"
echo ""

# 列出可用的工作流
echo "可用的工作流："
gh workflow list

echo ""
read -p "是否觸發 deploy-tango21.yml 工作流？(y/n): " trigger_workflow

if [ "$trigger_workflow" = "y" ]; then
    # 觸發工作流
    gh workflow run deploy-tango21.yml

    echo "✅ 工作流已觸發"
    echo ""
    echo "⏳ 等待工作流開始..."
    sleep 5
else
    echo "⏭️  跳過觸發工作流"
    exit 0
fi

echo ""

# 步驟 5：監控部署進度
echo "📊 步驟 5/5：監控部署進度"
echo ""

# 獲取最新的運行
echo "最近的工作流運行："
gh run list --workflow=deploy-tango21.yml --limit 5

echo ""
read -p "是否查看最新運行的日誌？(y/n): " view_logs

if [ "$view_logs" = "y" ]; then
    # 查看最新運行的日誌
    echo ""
    echo "📋 查看部署日誌..."
    gh run view --log

    echo ""
    echo "💡 提示：使用以下命令查看實時日誌："
    echo "   gh run watch"
fi

echo ""
echo "🎉 部署已啟動！"
echo ""
echo "📊 查看部署狀態："
echo "   gh run list"
echo "   gh run view"
echo "   gh run watch  # 實時監控"
echo ""
echo "🌐 部署完成後訪問："
echo "   http://3.112.247.26"
echo "   http://3.112.247.26/nba-swing"
echo ""

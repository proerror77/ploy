# 🚀 使用 GitHub Actions 自動部署

## ⚡ 快速開始（3 步驟）

### 步驟 1：添加 AWS Secrets

在 GitHub 倉庫中添加 Secrets：

1. Settings → Secrets and variables → Actions
2. 添加以下 secrets：
   - `AWS_ACCESS_KEY_ID`
   - `AWS_SECRET_ACCESS_KEY`

### 步驟 2：推送代碼

```bash
git add .
git commit -m "Add GitHub Actions deployment"
git push origin main
```

### 步驟 3：查看部署

1. 進入 GitHub → Actions
2. 查看 "Deploy to tango-2-1" 工作流
3. 等待部署完成（10-15 分鐘）
4. 訪問 http://3.112.247.26

---

## 📋 或者手動觸發

1. GitHub → Actions
2. Deploy to tango-2-1
3. Run workflow → Run workflow

---

## 🔍 查看詳細指南

```bash
cat GITHUB_ACTIONS_SETUP.md
```

---

## 🌐 部署目標

- **實例**：tango-2-1 (i-01de34df55726073d)
- **IP**：3.112.247.26
- **訪問**：http://3.112.247.26

---

## 📊 工作流文件

`.github/workflows/deploy-tango21.yml`

**功能**：
- ✅ 自動構建前端
- ✅ 上傳到 S3
- ✅ 通過 SSM 部署到 EC2
- ✅ 自動驗證部署
- ✅ 顯示部署摘要

---

## 🎯 優勢

- **自動化**：推送即部署
- **可追蹤**：完整日誌
- **安全**：使用 Secrets
- **快速**：10-15 分鐘

---

**查看完整指南**：`GITHUB_ACTIONS_SETUP.md`

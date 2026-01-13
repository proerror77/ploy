# 🚀 EC2 部署 - 快速開始

**目標**：將 Ploy Trading System 部署到 tango_1_1 EC2

---

## ⚡ 最快部署方式

```bash
./deploy_quick.sh
```

然後選擇選項 1（使用 SSM）或 2（手動執行）

---

## 📋 部署狀態

### EC2 信息
- **實例 ID**：i-0b29ca671375dad53
- **IP 地址**：13.113.155.16
- **狀態**：✅ 運行中
- **密鑰對**：bn-watcher-key

### 準備狀態
- ✅ EC2 已啟動
- ✅ 前端已構建（dist/ 目錄）
- ✅ 後端代碼已準備
- ✅ 部署腳本已創建

---

## 🎯 三種部署方式

### 方式 1：一鍵部署（最簡單）✅

```bash
./deploy_quick.sh
```

**特點**：
- 自動創建 S3 bucket
- 自動上傳文件
- 自動在 EC2 上部署
- 支持 SSM 或手動執行

### 方式 2：使用部署指南（最詳細）

查看完整指南：
```bash
cat EC2_DEPLOYMENT_GUIDE.md
```

**特點**：
- 詳細的步驟說明
- 多種部署方式
- 完整的故障排除
- 管理和監控命令

### 方式 3：手動部署（最靈活）

#### 步驟 1：上傳文件到 S3

```bash
# 創建 bucket
BUCKET=ploy-deployment-$(date +%s)
aws s3 mb s3://$BUCKET

# 上傳前端
aws s3 cp dist/ s3://$BUCKET/frontend/ --recursive

# 上傳後端
tar czf ploy-backend.tar.gz Cargo.toml Cargo.lock src/ examples/ migrations/
aws s3 cp ploy-backend.tar.gz s3://$BUCKET/
```

#### 步驟 2：連接到 EC2

```bash
# 使用 SSM
aws ssm start-session --target i-0b29ca671375dad53

# 或使用 AWS Console 的 EC2 Instance Connect
```

#### 步驟 3：在 EC2 上執行

```bash
# 下載文件
mkdir -p ~/ploy/{frontend,backend}
aws s3 cp s3://$BUCKET/frontend/ ~/ploy/frontend/ --recursive
aws s3 cp s3://$BUCKET/ploy-backend.tar.gz ~/ploy/backend/
cd ~/ploy/backend && tar xzf ploy-backend.tar.gz

# 安裝依賴
sudo apt-get update
sudo apt-get install -y nginx build-essential pkg-config libssl-dev
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source $HOME/.cargo/env

# 配置 Nginx（見 EC2_DEPLOYMENT_GUIDE.md）

# 構建並啟動
cd ~/ploy/backend
cargo build --release

# 創建服務（見 EC2_DEPLOYMENT_GUIDE.md）
```

---

## 🔍 驗證部署

### 檢查服務

```bash
# 連接到 EC2
aws ssm start-session --target i-0b29ca671375dad53

# 檢查服務狀態
sudo systemctl status nginx
sudo systemctl status ploy-backend

# 查看日誌
sudo journalctl -u ploy-backend -f
```

### 測試訪問

在瀏覽器中訪問：
- **前端**：http://13.113.155.16
- **NBA Swing**：http://13.113.155.16/nba-swing
- **策略監控**：http://13.113.155.16/monitor-strategy

---

## 📚 相關文檔

| 文檔 | 用途 |
|------|------|
| **EC2_DEPLOYMENT_GUIDE.md** | 完整部署指南 |
| **deploy_quick.sh** | 一鍵部署腳本 |
| **deploy_to_ec2.sh** | SSH 部署腳本（需要密鑰）|
| **deploy_to_ec2_ssm.sh** | SSM 部署腳本 |

---

## 🛠️ 常用命令

```bash
# 連接到 EC2
aws ssm start-session --target i-0b29ca671375dad53

# 檢查 EC2 狀態
aws ec2 describe-instances --instance-ids i-0b29ca671375dad53 \
  --query 'Reservations[0].Instances[0].[State.Name,PublicIpAddress]' \
  --output text

# 重啟服務（在 EC2 上）
sudo systemctl restart ploy-backend nginx

# 查看日誌（在 EC2 上）
sudo journalctl -u ploy-backend -f

# 停止 EC2（節省成本）
aws ec2 stop-instances --instance-ids i-0b29ca671375dad53

# 啟動 EC2
aws ec2 start-instances --instance-ids i-0b29ca671375dad53
```

---

## 🆘 遇到問題？

### 無法連接到 EC2

**解決方案**：
1. 檢查 EC2 是否運行：
   ```bash
   aws ec2 describe-instances --instance-ids i-0b29ca671375dad53 \
     --query 'Reservations[0].Instances[0].State.Name' --output text
   ```

2. 如果停止，啟動它：
   ```bash
   aws ec2 start-instances --instance-ids i-0b29ca671375dad53
   ```

### SSM 不可用

**解決方案**：
使用 AWS Console 的 EC2 Instance Connect：
1. 打開 AWS Console
2. EC2 → Instances → tango_1_1
3. Connect → EC2 Instance Connect
4. Connect

### 前端無法訪問

**解決方案**：
```bash
# 連接到 EC2
aws ssm start-session --target i-0b29ca671375dad53

# 檢查 Nginx
sudo systemctl status nginx
sudo nginx -t

# 檢查文件
ls -la ~/ploy/frontend/

# 重啟 Nginx
sudo systemctl restart nginx
```

### 後端無法啟動

**解決方案**：
```bash
# 查看日誌
sudo journalctl -u ploy-backend -n 100

# 手動運行測試
cd ~/ploy/backend
./target/release/ploy

# 重新構建
cargo clean
cargo build --release
```

---

## 🎉 部署完成後

### 訪問應用

- **前端主頁**：http://13.113.155.16
- **NBA Swing**：http://13.113.155.16/nba-swing
- **策略監控**：http://13.113.155.16/monitor-strategy
- **交易歷史**：http://13.113.155.16/trades
- **實時日誌**：http://13.113.155.16/monitor
- **系統控制**：http://13.113.155.16/control
- **安全審計**：http://13.113.155.16/security

### 管理應用

```bash
# 連接到 EC2
aws ssm start-session --target i-0b29ca671375dad53

# 查看日誌
sudo journalctl -u ploy-backend -f

# 重啟服務
sudo systemctl restart ploy-backend

# 更新代碼
cd ~/ploy/backend
# 上傳新代碼...
cargo build --release
sudo systemctl restart ploy-backend
```

---

**版本**：v1.0.0
**日期**：2026-01-13
**狀態**：✅ 準備就緒，可以開始部署

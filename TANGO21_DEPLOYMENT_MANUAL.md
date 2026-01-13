# 🚀 tango-2-1 部署 - 手動執行指南

**狀態**：✅ 文件已上傳到 S3，等待在 EC2 上執行

---

## 📊 當前狀態

### 已完成
- ✅ tango_1_1 已關閉
- ✅ ploy-tandgo-1-1-jp 已改名為 tango-2-1
- ✅ 前端文件已上傳到 S3
- ✅ 後端代碼已上傳到 S3
- ✅ 部署腳本已上傳到 S3

### 待完成
- ⏳ 在 EC2 上執行部署腳本
- ⏳ 驗證部署成功

---

## 🎯 目標 EC2 信息

- **實例名稱**：tango-2-1
- **實例 ID**：i-01de34df55726073d
- **IP 地址**：3.112.247.26
- **實例類型**：t3.micro (1 GB RAM)
- **SSH 密鑰**：ploy-jp-key
- **S3 Bucket**：ploy-deployment-1768267790

---

## 🚀 部署步驟

### 方式 1：使用 AWS Console（最簡單）✅

#### 步驟 1：連接到 EC2

1. 打開 AWS Console
2. 進入 EC2 → Instances
3. 選擇 **tango-2-1** (i-01de34df55726073d)
4. 點擊 "Connect"
5. 選擇 "EC2 Instance Connect"
6. 點擊 "Connect"

#### 步驟 2：在瀏覽器終端中執行

複製並粘貼以下命令：

```bash
# 1. 從 S3 下載部署腳本
aws s3 cp s3://ploy-deployment-1768267790/deploy_on_tango21.sh /tmp/

# 2. 賦予執行權限
chmod +x /tmp/deploy_on_tango21.sh

# 3. 執行部署腳本
/tmp/deploy_on_tango21.sh
```

#### 步驟 3：等待部署完成

部署過程大約需要 5-10 分鐘，包括：
- 下載文件
- 安裝依賴（Nginx, Rust）
- 配置 Nginx
- 構建後端
- 啟動服務

---

### 方式 2：使用 SSH（如果有密鑰）

如果你有 ploy-jp-key.pem 文件：

```bash
# 1. 連接到 EC2
ssh -i ~/.ssh/ploy-jp-key.pem ubuntu@3.112.247.26

# 2. 執行部署命令
aws s3 cp s3://ploy-deployment-1768267790/deploy_on_tango21.sh /tmp/
chmod +x /tmp/deploy_on_tango21.sh
/tmp/deploy_on_tango21.sh
```

---

### 方式 3：使用 AWS CLI（如果 SSM 可用）

```bash
# 嘗試使用 SSM
aws ssm start-session --target i-01de34df55726073d

# 然後執行部署命令
aws s3 cp s3://ploy-deployment-1768267790/deploy_on_tango21.sh /tmp/
chmod +x /tmp/deploy_on_tango21.sh
/tmp/deploy_on_tango21.sh
```

---

## 📋 部署腳本做什麼？

部署腳本會自動執行以下操作：

1. **備份現有配置**（如果有）
   ```bash
   mv ~/ploy ~/ploy.backup.YYYYMMDD_HHMMSS
   ```

2. **創建目錄**
   ```bash
   mkdir -p ~/ploy/{frontend,backend}
   ```

3. **從 S3 下載文件**
   - 前端文件 → ~/ploy/frontend/
   - 後端代碼 → ~/ploy/backend/

4. **安裝依賴**
   - Nginx
   - Rust
   - 構建工具

5. **配置 Nginx**
   - 前端：http://3.112.247.26/
   - API：http://3.112.247.26/api
   - WebSocket：http://3.112.247.26/ws

6. **構建後端**
   ```bash
   cargo build --release
   ```

7. **創建 systemd 服務**
   - ploy-backend.service

8. **啟動服務**
   - Nginx
   - ploy-backend

---

## 🔍 驗證部署

### 在 EC2 上檢查

```bash
# 檢查服務狀態
sudo systemctl status nginx
sudo systemctl status ploy-backend

# 查看日誌
sudo journalctl -u ploy-backend -f

# 檢查端口
sudo lsof -i :80
sudo lsof -i :8080
```

### 在瀏覽器中訪問

- **前端主頁**：http://3.112.247.26
- **NBA Swing**：http://3.112.247.26/nba-swing
- **策略監控**：http://3.112.247.26/monitor-strategy
- **交易歷史**：http://3.112.247.26/trades
- **實時日誌**：http://3.112.247.26/monitor
- **系統控制**：http://3.112.247.26/control
- **安全審計**：http://3.112.247.26/security

---

## 🛠️ 管理命令

### 重啟服務

```bash
# 重啟後端
sudo systemctl restart ploy-backend

# 重啟 Nginx
sudo systemctl restart nginx

# 重啟所有服務
sudo systemctl restart ploy-backend nginx
```

### 查看日誌

```bash
# 後端日誌（實時）
sudo journalctl -u ploy-backend -f

# 後端日誌（最近 100 行）
sudo journalctl -u ploy-backend -n 100

# Nginx 訪問日誌
sudo tail -f /var/log/nginx/access.log

# Nginx 錯誤日誌
sudo tail -f /var/log/nginx/error.log
```

### 更新代碼

```bash
# 1. 停止服務
sudo systemctl stop ploy-backend

# 2. 更新代碼（從 S3 或 git）
cd ~/ploy/backend
# ... 更新代碼 ...

# 3. 重新構建
cargo build --release

# 4. 啟動服務
sudo systemctl start ploy-backend
```

---

## 🆘 故障排除

### 前端無法訪問

```bash
# 檢查 Nginx
sudo systemctl status nginx
sudo nginx -t

# 檢查文件
ls -la ~/ploy/frontend/

# 重啟 Nginx
sudo systemctl restart nginx
```

### 後端無法啟動

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

### 構建失敗

```bash
# 檢查 Rust
rustc --version
cargo --version

# 更新 Rust
rustup update

# 檢查依賴
sudo apt-get install -y build-essential pkg-config libssl-dev
```

---

## 🧹 清理 S3

部署完成後，可以刪除 S3 bucket 以節省成本：

```bash
aws s3 rb s3://ploy-deployment-1768267790 --force
```

---

## 📊 部署完成檢查清單

- [ ] 連接到 EC2（AWS Console 或 SSH）
- [ ] 執行部署腳本
- [ ] 等待部署完成（5-10 分鐘）
- [ ] 檢查服務狀態
- [ ] 在瀏覽器中訪問前端
- [ ] 測試各個頁面
- [ ] 查看後端日誌
- [ ] 清理 S3 bucket

---

## 🎯 快速命令參考

```bash
# 連接到 EC2（AWS Console）
# EC2 → Instances → tango-2-1 → Connect → EC2 Instance Connect

# 部署命令（在 EC2 上執行）
aws s3 cp s3://ploy-deployment-1768267790/deploy_on_tango21.sh /tmp/ && \
chmod +x /tmp/deploy_on_tango21.sh && \
/tmp/deploy_on_tango21.sh

# 檢查狀態（在 EC2 上執行）
sudo systemctl status nginx ploy-backend

# 查看日誌（在 EC2 上執行）
sudo journalctl -u ploy-backend -f

# 清理 S3（在本地執行）
aws s3 rb s3://ploy-deployment-1768267790 --force
```

---

## 📞 需要幫助？

如果遇到問題：
1. 查看日誌：`sudo journalctl -u ploy-backend -n 100`
2. 檢查服務：`sudo systemctl status ploy-backend nginx`
3. 測試連接：`curl http://localhost`

---

**版本**：v1.0.0
**日期**：2026-01-13
**狀態**：⏳ 等待在 EC2 上執行部署腳本

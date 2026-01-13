# 🚀 安全修復快速開始指南

## 📋 前置條件

- PostgreSQL 數據庫（已安裝並運行）
- Rust 工具鏈（已安裝）
- 數據庫連接配置（DATABASE_URL）

---

## ⚡ 5 分鐘快速部署

### 1. 備份數據庫（30 秒）
```bash
pg_dump ploy > backup_$(date +%Y%m%d_%H%M%S).sql
```

### 2. 運行數據庫遷移（1 分鐘）
```bash
# 設置數據庫 URL
export DATABASE_URL="postgresql://user:password@localhost/ploy"

# 運行遷移
sqlx migrate run

# 驗證遷移成功
psql -d ploy -c "SELECT * FROM nonce_state;"
psql -d ploy -c "\d order_idempotency"
```

### 3. 編譯項目（2 分鐘）
```bash
cargo build --release
```

### 4. 運行測試（1 分鐘）
```bash
# 運行單元測試（不需要數據庫）
cargo test --lib

# 運行集成測試（需要數據庫）
cargo test --test integration
```

### 5. 部署（30 秒）
```bash
# 停止舊版本
systemctl stop ploy

# 部署新版本
sudo cp target/release/ploy /usr/local/bin/

# 啟動新版本
systemctl start ploy

# 檢查狀態
systemctl status ploy
```

---

## 🔍 驗證部署

### 檢查數據庫表
```bash
psql -d ploy << EOF
-- 檢查冪等性表
SELECT COUNT(*) FROM order_idempotency;

-- 檢查 nonce 狀態
SELECT * FROM nonce_state;

-- 檢查 cycles 版本列
SELECT id, version FROM cycles LIMIT 5;

-- 檢查報價新鮮度表
SELECT COUNT(*) FROM quote_freshness;

-- 檢查安全審計日誌
SELECT COUNT(*) FROM security_audit_log;
EOF
```

### 檢查日誌
```bash
# 查看最近的日誌
journalctl -u ploy -n 100 --no-pager

# 實時監控日誌
journalctl -u ploy -f
```

### 測試 Nonce 生成
```bash
# 連接到數據庫
psql -d ploy

-- 測試 nonce 生成
SELECT get_next_nonce();
SELECT get_next_nonce();
SELECT get_next_nonce();

-- 應該看到遞增的數字
```

---

## 🧪 功能測試

### 測試冪等性保護
```rust
// 在 Rust 代碼中測試
use ploy::strategy::idempotency::IdempotencyManager;

let manager = IdempotencyManager::new(store);
let key = IdempotencyManager::generate_key(&request);

// 第一次提交
let result1 = manager.check_or_create(&key, &request).await?;
assert!(matches!(result1, IdempotencyResult::New));

// 第二次提交（應該被檢測為重複）
let result2 = manager.check_or_create(&key, &request).await?;
assert!(matches!(result2, IdempotencyResult::Duplicate { .. }));
```

### 測試樂觀鎖
```sql
-- 在數據庫中測試
BEGIN;

-- 讀取當前版本
SELECT id, version, state FROM cycles WHERE id = 1;

-- 嘗試更新（應該成功）
SELECT update_cycle_with_version(1, 1, 'LEG1_PENDING');

-- 再次嘗試相同版本（應該失敗）
SELECT update_cycle_with_version(1, 1, 'LEG1_FILLED');

ROLLBACK;
```

### 測試報價新鮮度
```sql
-- 插入測試報價
INSERT INTO quote_freshness (token_id, side, best_bid, best_ask)
VALUES ('test_token', 'UP', 0.50, 0.51);

-- 立即查詢（應該返回結果）
SELECT * FROM get_fresh_quote('test_token', 'UP', 30);

-- 等待 31 秒後查詢（應該返回空）
SELECT pg_sleep(31);
SELECT * FROM get_fresh_quote('test_token', 'UP', 30);
```

---

## 📊 監控指標

### 關鍵指標
```sql
-- 冪等性統計
SELECT
    status,
    COUNT(*) as count,
    AVG(EXTRACT(EPOCH FROM (NOW() - created_at))) as avg_age_seconds
FROM order_idempotency
GROUP BY status;

-- Nonce 使用情況
SELECT
    current_nonce,
    last_updated,
    EXTRACT(EPOCH FROM (NOW() - last_updated)) as seconds_since_update
FROM nonce_state;

-- 報價新鮮度統計
SELECT
    COUNT(*) as total_quotes,
    COUNT(*) FILTER (WHERE is_stale = false) as fresh_quotes,
    COUNT(*) FILTER (WHERE is_stale = true) as stale_quotes,
    AVG(EXTRACT(EPOCH FROM (NOW() - received_at))) as avg_age_seconds
FROM quote_freshness;

-- 安全事件統計
SELECT
    severity,
    COUNT(*) as count
FROM security_audit_log
WHERE timestamp > NOW() - INTERVAL '1 hour'
GROUP BY severity
ORDER BY severity;
```

---

## 🚨 故障排除

### 問題 1：遷移失敗
```bash
# 檢查遷移狀態
sqlx migrate info

# 回滾最後一次遷移
sqlx migrate revert

# 重新運行遷移
sqlx migrate run
```

### 問題 2：Nonce 衝突
```sql
-- 檢查當前 nonce
SELECT * FROM nonce_state;

-- 重置 nonce（緊急情況）
UPDATE nonce_state
SET current_nonce = EXTRACT(EPOCH FROM NOW())::BIGINT * 1000
WHERE id = 1;
```

### 問題 3：冪等性記錄過多
```sql
-- 手動清理過期記錄
SELECT cleanup_expired_idempotency_keys();

-- 檢查清理結果
SELECT COUNT(*) FROM order_idempotency;
```

### 問題 4：版本衝突頻繁
```sql
-- 檢查版本衝突頻率
SELECT
    COUNT(*) as total_updates,
    COUNT(*) FILTER (WHERE version > 1) as version_conflicts
FROM cycles
WHERE updated_at > NOW() - INTERVAL '1 hour';

-- 如果衝突率 > 5%，考慮優化並發控制
```

---

## 📈 性能優化建議

### 1. 數據庫索引
```sql
-- 確保所有索引都已創建
\di order_idempotency*
\di cycles*
\di quote_freshness*
\di nonce_state*

-- 如果缺失，手動創建
CREATE INDEX IF NOT EXISTS idx_order_idempotency_key
ON order_idempotency(idempotency_key);
```

### 2. 連接池配置
```rust
// 在 config.toml 中調整
[database]
max_connections = 20  # 根據負載調整
min_connections = 5
connect_timeout = 30
idle_timeout = 600
```

### 3. 緩存優化
```rust
// NonceManager 已實現內存緩存
// 無需額外配置
```

---

## 🔄 回滾計劃

### 如果需要回滾

#### 1. 停止服務
```bash
systemctl stop ploy
```

#### 2. 恢復舊版本
```bash
sudo cp /usr/local/bin/ploy.backup /usr/local/bin/ploy
```

#### 3. 回滾數據庫（可選）
```bash
# 只回滾最後一次遷移
sqlx migrate revert

# 或完全恢復備份
psql -d ploy < backup_YYYYMMDD_HHMMSS.sql
```

#### 4. 重啟服務
```bash
systemctl start ploy
```

---

## ✅ 部署檢查清單

### 部署前
- [ ] 備份數據庫
- [ ] 在測試環境驗證
- [ ] 編譯發布版本
- [ ] 運行所有測試
- [ ] 準備回滾計劃

### 部署中
- [ ] 停止服務
- [ ] 運行數據庫遷移
- [ ] 部署新版本
- [ ] 啟動服務
- [ ] 檢查日誌

### 部署後
- [ ] 驗證數據庫表
- [ ] 測試關鍵功能
- [ ] 監控性能指標
- [ ] 檢查錯誤日誌
- [ ] 通知團隊

---

## 📞 支持

### 遇到問題？

1. **檢查日誌：** `journalctl -u ploy -n 100`
2. **查看文檔：** `SECURITY_FIXES_STATUS.md`
3. **運行診斷：** `cargo test --test diagnostics`
4. **聯繫團隊：** 提供日誌和錯誤信息

### 有用的命令

```bash
# 檢查服務狀態
systemctl status ploy

# 查看實時日誌
journalctl -u ploy -f

# 檢查數據庫連接
psql -d ploy -c "SELECT 1"

# 運行健康檢查
curl http://localhost:8080/health

# 查看 Prometheus 指標
curl http://localhost:8080/metrics
```

---

**快速開始指南版本：** 1.0
**最後更新：** 2026-01-10
**適用版本：** Phase 1 安全修復

**祝部署順利！** 🚀

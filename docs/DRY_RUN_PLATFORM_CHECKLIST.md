# Dry-Run Platform Checklist

本文件用于每次上線前的「非下單」檢核，所有項目 pass 後再進入 live dry-run/staging。

## 0. 前置條件

- `PLOY_DRY_RUN__ENABLED=true`
- `PLOY_DEPLOYMENTS_FILE` 指向有效矩陣檔（建議 `deployment/deployments.json`）
- `PLOY_RUN_SQLX_MIGRATIONS=false` 僅限本機實驗，不建議上線前保留
- `.env` / workload env 已備齊且 `ploy-sidecar` 相關憑證不可用於 dry-run

## 1. 工具與快照檢查

- `cargo fmt --check`
- `cargo check -q`
- `cargo run --bin ploy -- platform --help`
- `cargo run --bin ploy -- platform start --dry-run --crypto --sports`

## 2. 策略矩陣檢查

- `deployment/deployments.json` 存在且可讀
- 每筆 deployment 至少包含：`id`、`strategy`、`domain`、`market_selector`、`timeframe`、`enabled`
- 時間週期策略包含 `5m` 與 `15m`（依需求可調）

## 3. 風險控管啟用檢查

- `PLOY_REQUIRE_SQLX_MIGRATIONS=true`
- `PLOY_RUN_SQLX_MIGRATIONS=true`
- `PLOY_RISK__ACCOUNT_RESERVE_PCT`、`PLOY_RISK__CRYPTO_ALLOCATION_PCT`、`PLOY_RISK__SPORTS_ALLOCATION_PCT` 有預期值
- `PLOY_COORDINATOR__HEARTBEAT_STALE_WARN_COOLDOWN_SECS` 設定符合噪音要求（建議 300）

## 4. 部署腳本固化檢查

- `scripts/aws_ec2_deploy.sh` / `scripts/install-service.sh` 會在 env 補齊：
  - `PLOY_RUN_SQLX_MIGRATIONS=true`
  - `PLOY_REQUIRE_SQLX_MIGRATIONS=true`
  - `PLOY_COORDINATOR__HEARTBEAT_STALE_WARN_COOLDOWN_SECS=300`
  - `PLOY_DEPLOYMENTS_FILE=/opt/ploy/data/state/deployments.json`
- 遠端首次部署若 `data/state/deployments.json` 不存在，會從 `deployment/deployments.json` 複製

## 5. 通過條件

- 以上檢查無 error/stacktrace
- `ploy platform start --dry-run --crypto --sports` 能在 timeout 時穩定結束（通常 `timeout` 會回傳 124）
- API 事件可見性可用（`/ws` 有 trade/position/market 任一實際推播）

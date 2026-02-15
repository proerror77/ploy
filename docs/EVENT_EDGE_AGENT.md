# EventEdgeAgent（主動循環交易）

`EventEdgeAgent` 是一個長駐背景的「事件/資料源驅動」掃描與交易迴圈：

- 從公開資料源（目前支援 Arena Text leaderboard）推估保守的 `p_true`
- 讀取 Polymarket order book（best ask）
- 當 `p_true - ask >= min_edge` 且 `ask <= max_entry` 時自動下單（若 `trade=true`）

它不需要你盯盤，也不需要你一直手動啟動 `event-edge --watch`。只要啟動一次 `ploy run`（或交給系統服務管理），它就會自己循環跑。

## 1) 開啟設定

在 `config/default.toml`（或你的環境檔）加入：

```toml
[event_edge_agent]
enabled = true
framework = "deterministic" # 或 "claude_agent_sdk"
trade = true
interval_secs = 30
min_edge = 0.08
max_entry = 0.75
shares = 100
cooldown_secs = 120
max_daily_spend_usd = 50
model = ""          # 可選：指定 Claude model
claude_max_turns = 20
titles = ["Which company has the best AI model end of February?"]
event_ids = []
```

說明：
- `titles` 會用 Gamma `title_contains` 自動找最匹配的 event（適合 event 會換 id 的情況）
- 若你已知 `event_id`，用 `event_ids` 會更穩
- `framework`：
  - `deterministic`：不用 LLM，固定規則掃描與下單（速度快、可預測）
  - `event_driven`：事件驅動 + 狀態持久化（Arena `last_updated` 不變就不交易，重啟不會失憶）
  - `claude_agent_sdk`：使用 `claude-agent-sdk-rs` 走「工具調用」的 agent（LLM 決策 + MCP tools；需要本機已安裝並登入 Claude Code CLI）
- `cooldown_secs` 是每個 token 的下單冷卻（避免反覆追同一邊）
- `max_daily_spend_usd` 是簡單安全閥（以 `shares * ask` 粗估）

## 2) 啟用真實下單（Live）

`EventEdgeAgent` 會遵守全域 `dry_run.enabled`：
- `dry_run.enabled=true`：只會模擬下單（不需要金鑰）
- `dry_run.enabled=false`：會嘗試用環境變數建立 authenticated client 送真單

需要的環境變數：
- `POLYMARKET_PRIVATE_KEY`（或 `PRIVATE_KEY`）
- 若是 proxy/Magic 錢包：再加 `POLYMARKET_FUNDER`

## 3) 啟動（一次就好）

```bash
ploy run
```

啟動後它會跟著主程序一起常駐循環，Ctrl+C 才停止。

## 4) 讓它自動常駐（macOS launchd）

本 repo 已包含 `deployment/com.ploy.trading.plist`，它會在開機/登入後自動跑 `ploy run` 並 KeepAlive。

你只需要：
1. 確保你的 `config/default.toml`（或 `PLOY_ENV` 對應設定）已開啟 `[event_edge_agent]`
2. 把必要的環境變數寫進 plist 的 `EnvironmentVariables`
3. `launchctl load` 該 plist

（launchd 的詳細安裝/路徑請依你的部署方式調整）

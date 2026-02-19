# OpenClaw（GitHub: openclaw/openclaw）整合方式

你貼的 `https://github.com/openclaw/openclaw` 是一個 **Node.js Gateway + agent runtime**（不是 Rust crate）。
crates.io 上的 `openclaw`（`openclaw = "0.1.0"`）目前只是 stub，和 GitHub 的 OpenClaw 專案不是同一個可直接嵌入的 framework。

在本 repo，建議用下列方式把 OpenClaw 變成「永遠主動」的 orchestrator，而由 `ploy` 實際負責下單：

## A) 最快：OpenClaw 直接呼叫 `ploy`（bash/skills）

1. 先把 `ploy` 編譯好並放進 PATH：

```bash
cargo build --release
export PATH="$(pwd)/target/release:$PATH"
```

2. 推薦用本 repo 內建的 daemon wrapper（給 OpenClaw 呼叫更穩）：

```bash
scripts/event_edge_daemon.sh start false true   # safe observe (trade=false, dry_run=true)
scripts/event_edge_daemon.sh start false true 123,456  # optional event_ids CSV override
scripts/event_edge_daemon.sh status
scripts/event_edge_daemon.sh logs 200
scripts/event_edge_daemon.sh stop
```

（它會把 PID/Logs 放到 `data/state/`、`data/logs/`，方便 OpenClaw 做 health/status。）

3. 遠端 gateway 控制這台機器（推薦：SSH forced command allowlist）

在交易機器上（跑 `ploy` 的那台）：

- 建一個專用使用者（例如 `ploy`），並把 repo 放在固定路徑
- 把你的 SSH public key 加到 `~ploy/.ssh/authorized_keys`，用 forced command 綁死可執行的指令（只允許 start/stop/status/logs/rpc）：

```text
command="/ABS/PATH/TO/ploy/scripts/ssh_ployctl.sh",no-port-forwarding,no-agent-forwarding,no-X11-forwarding,no-pty ssh-ed25519 AAAA...
```

然後在遠端（OpenClaw gateway 所在機器）就可以安全地只呼叫 allowlist：

```bash
ssh ploy@TRADING_HOST "status"
ssh ploy@TRADING_HOST "start false true"
ssh ploy@TRADING_HOST "start false true 123,456"
ssh ploy@TRADING_HOST "logs 200"
ssh ploy@TRADING_HOST "rpc" < request.json
ssh ploy@TRADING_HOST "stop"
# systemd workloads (sports / crypto dry-run)
ssh ploy@TRADING_HOST "svc-status sports"
ssh ploy@TRADING_HOST "svc-start sports"
ssh ploy@TRADING_HOST "svc-logs sports 200"
ssh ploy@TRADING_HOST "svc-status crypto"
ssh ploy@TRADING_HOST "svc-restart crypto"
ssh ploy@TRADING_HOST "svc-logs crypto 200"
```

這樣 OpenClaw 只要有 SSH 連線能力，就能「遠端永遠主動」地控這台交易機器，但不會變成任意 RCE。

4. 在 OpenClaw 裡建立一個自訂 skill，內容用 bash 直接跑：

- 掃描一次（不下單）：`ploy event-edge --title "Which company has the best AI model end of February?"`
- 常駐自動循環：`ploy run`（由 `config/default.toml` 的 `[event_edge_agent]` 控制）
- 或改用 wrapper：`scripts/event_edge_daemon.sh start false true`

這樣 OpenClaw 可以用自己的 always-on daemon + channel inbox 來觸發、監控、或切換策略；而交易邏輯仍由 `ploy` 控制（含 `dry_run` / risk guard）。

（可直接用本 repo 提供的 OpenClaw skill 模板：`examples/openclaw/skill-ploy-rpc/`）

### RPC（給 agent 用的工具介面）

交易機器提供 `ploy rpc`（JSON-RPC 2.0，stdin→stdout），可透過 forced-command 的 allowlist 安全轉發：

```bash
cat <<'JSON' | ssh ploy@TRADING_HOST "rpc"
{"jsonrpc":"2.0","id":1,"method":"pm.get_balance","params":{}}
JSON
```

注意：
- `pm.submit_limit` / `pm.cancel_order` / `events.upsert` / `events.update_status` 這類「寫入」操作預設會被拒絕，必須在交易機器環境設 `PLOY_RPC_WRITE_ENABLED=true` 才會放行。
- 寫入操作現在要求 `params.idempotency_key`（建議用 UUID）。
- `pm.submit_limit` / `gateway.submit_intent` 會改走 Coordinator ingestion API（預設 `http://127.0.0.1:8081/api/sidecar/intents`），所以交易機器必須有平台 API 正在運行；可用 `PLOY_RPC_COORDINATOR_INTENT_URL` 覆寫。
- live 直連 `submit_order` 預設禁用（防旁路風控）；如需暫時回退可設 `PLOY_ALLOW_LEGACY_DIRECT_SUBMIT=true`（不建議 production）。
- `ploy strategy start ...` 的 legacy live runtime 預設也會被擋下（避免繞過 Coordinator）。如需緊急回退才設 `PLOY_ALLOW_LEGACY_STRATEGY_LIVE=true`。
- 若設定 `PLOY_SIDECAR_AUTH_TOKEN`，所有 sidecar `POST` 端點都需帶 `x-ploy-sidecar-token`（或 `Authorization: Bearer ...`）。
- 若你要強制「只有 coordinator/gateway 能送單」，在交易機器加上 `PLOY_GATEWAY_ONLY=true`。
  在這模式下，live order 需帶 `idempotency_key`，且 `client_order_id` 必須是 `intent:` 前綴（Coordinator 已自動帶入）。
- 寫入審計會落地在 `data/rpc/audit/*.jsonl`（可用 `PLOY_RPC_STATE_DIR` 覆寫）。
- 若你要強制每筆 intent 都必須命中已註冊 deployment，可設 `PLOY_DEPLOYMENT_GATE_REQUIRED=true`。

### Deployment Matrix API

控制面新增 deployment matrix API（記憶體態，支援一次批量上傳）：

- `GET /api/deployments`
- `PUT /api/deployments`（body: `{ "deployments":[...], "replace":true|false }`）
- `GET /api/deployments/:id`
- `POST /api/deployments/:id/enable`
- `POST /api/deployments/:id/disable`
- `DELETE /api/deployments/:id`

已支援的 method（起步集合）：
- `pm.get_balance`
- `pm.get_positions`
- `pm.get_open_orders`
- `pm.get_order`（params: `order_id`）
- `pm.cancel_order`（params: `order_id`, `idempotency_key`）
- `pm.search_markets`（params: `query`）
- `pm.get_event_details`（params: `event_id`）
- `pm.get_market`（params: `condition_id`）
- `pm.get_order_book`（params: `token_id`）
- `pm.submit_limit`（params: `token_id`, `order_side`=`BUY|SELL`, `shares`, `limit_price`, `market_side`=`UP|DOWN`(optional), `idempotency_key`）
- `gateway.submit_intent`（params: `deployment_id`, `domain`, `market_slug`, `token_id`, `side`, `order_side`, `size`, `price_limit`, `idempotency_key`）
- `event_edge.scan`（params: `event_id` 或 `title`）
- `multi_outcome.analyze`（params: `event_id`；回傳 outcome summary + 偵測到的套利訊號）
- `events.upsert`（params: upsert 欄位 + `idempotency_key`）
- `events.update_status`（params: `id`, `status`, `idempotency_key`）

#### OpenClaw skill（bash）建議寫法

在 OpenClaw 的自訂 skill 裡（bash），把 `TRADING_HOST` 固定成你的交易機器，然後每個工具都只是送一個 JSON：

```bash
TRADING_HOST="ploy@YOUR_IP_OR_HOSTNAME"

cat <<'JSON' | ssh "$TRADING_HOST" "rpc"
{"jsonrpc":"2.0","id":1,"method":"event_edge.scan","params":{"title":"Which company has the best AI model end of February?"}}
JSON
```

## OpenClaw-only Runtime Lockdown

若要在交易機器強制禁用內建 agent runtime（改由 OpenClaw 全接管），可設定：

```toml
[agent_framework]
mode = "openclaw"
hard_disable_internal_agents = true
```

## B) 深度：讓 OpenClaw 以 MCP Tool 方式控制交易（下一步）

OpenClaw 支援 MCP Servers；下一步可以做：

- 在本 repo 新增 `ploy mcp`（stdio JSON-RPC）提供工具：
  - `event_edge_targets`
  - `event_edge_scan`
  - `event_edge_buy_yes`
- 然後在 OpenClaw gateway 的 MCP config 註冊這個 server，讓 OpenClaw 的 agent 可以「工具調用」而不是純 bash。

如果你要走 B) 路線，告訴我你希望 OpenClaw 用哪個 provider（Claude CLI / OpenAI / 其他），我會把 MCP server binary + 範例 config 補齊。

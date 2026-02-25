# Ploy Trading System Heartbeat

Run these checks every heartbeat cycle. Reply HEARTBEAT_OK if everything is normal.

## 1. System Health
- Check backend health: `curl -sf $PLOY_API_BASE/health | jq .`
- If status is not "ok" or DB is "disconnected", alert immediately.

## 2. Risk State
- Get risk state: `curl -sf $PLOY_API_BASE/api/sidecar/risk -H "x-ploy-sidecar-token: $PLOY_SIDECAR_AUTH_TOKEN" | jq .`
- Alert if:
  - `risk_state` is not "normal"
  - `daily_pnl_usd` is worse than -50% of `daily_loss_limit_usd`
  - Any circuit breaker events in the last 10 minutes
  - `current_drawdown_usd` exceeds 70% of `drawdown_limit_usd`

## 3. Positions Review
- Get positions: `curl -sf $PLOY_API_BASE/api/sidecar/positions -H "x-ploy-sidecar-token: $PLOY_SIDECAR_AUTH_TOKEN" | jq .`
- Alert if:
  - Any position has unrealized PnL worse than -$30
  - Total position count exceeds 20
  - Any position has been open for more than 48 hours

## 4. Governance State
- Get governance: `curl -sf $PLOY_API_BASE/api/governance/status -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" | jq .`
- Note any paused agents or blocked domains.
- If `block_new_intents` is true and risk looks normal, consider resuming.

## 5. Market Regime (Quick Check)
- Get BTC price from Binance: `curl -sf 'https://api.binance.com/api/v3/klines?symbol=BTCUSDT&interval=1m&limit=60' | jq '[.[].[-3] | tonumber] | {avg: (add / length), min: min, max: max, range: (max - min)}'`
- If 60-min range > 3% of avg price, note elevated volatility.
- If range < 0.5% of avg price, note very low volatility.

## Decision Rules
- If any check fails to connect, retry once. If still failing, alert "Backend unreachable".
- If risk state is critical, immediately use the governance-controller skill to block new intents.
- Write notable findings to memory for trend analysis.

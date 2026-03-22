# Platform Startup Runbook

## Goal

Start the current single-host trading platform runtime with:

- one `ployd` daemon
- one or more managed deployment workers
- one operator client path through `ployctl`
- one optional terminal console through `ploytui`

This runbook is only for the current workspace platform path.

For offline research, backtests, and dataset prep, use
[`research-backtest-routing.md`](research-backtest-routing.md) instead.

## Shared Daemon Boot

Start `ployd` first:

```bash
cargo run -p ployd
```

Recommended auth and operator hardening:

```bash
export PLOY_ADMIN_TOKEN=change-me
export PLOY_API_AUTH_COOKIE_SECRET=replace-me-with-randomness
export PLOY_SIDECAR_AUTH_TOKEN=read-only-token
```

Optional runtime tuning:

```bash
export PLOY_REQUEST_RATE_LIMIT_PER_MINUTE=240
export PLOY_CLAIM_TICK_INTERVAL_MS=30000
export PLOY_CLAIM_BACKOFF_BASE_MS=5000
export PLOY_CLAIM_BACKOFF_MAX_MS=120000
export PLOY_LIVE_RECONCILE_BACKOFF_BASE_MS=1000
export PLOY_LIVE_RECONCILE_BACKOFF_MAX_MS=30000
```

The frontend login flow uses `/auth/login`, which sets an `HttpOnly` same-site
signed session cookie so `/api/events/stream` stays authenticated.

## Paper Smoke Flow

Use this first when validating a fresh local environment:

```bash
cargo run -p ployctl -- system status
cargo run -p ployctl -- system audit
cargo run -p ployctl -- deployments apply config/deployments/example.paper.json
cargo run -p ployctl -- deployments inspect example.paper
cargo run -p ployctl -- trading inspect example.paper
cargo run -p ployctl -- claims list
cargo run -p ploytui -- --watch
curl -N http://127.0.0.1:8081/api/events/stream
rtk cargo test --test platform_smoke -- --nocapture
```

## Minimal Live Flow

Set the live credentials before applying a live deployment:

```bash
export POLYMARKET_PRIVATE_KEY=0x...
export POLYMARKET_API_KEY=...
export POLYMARKET_API_SECRET=...
export POLYMARKET_PASSPHRASE=...
export POLY_SIGNATURE_TYPE=proxy
export POLY_FUNDER=0x...
export POLY_RELAYER_URL=https://relayer-v2.polymarket.com
export POLY_BUILDER_API_KEY=...
export POLY_BUILDER_SECRET=...
export POLY_BUILDER_PASSPHRASE=...
export POLYGON_RPC_URL=https://polygon.drpc.org
```

If you use a SAFE wallet instead of a proxy wallet, set
`POLY_SIGNATURE_TYPE=gnosis_safe`.

Then use the checked-in live manifest:

```bash
cargo run -p ployctl -- deployments apply config/deployments/example.live.json
cargo run -p ployctl -- deployments inspect example.live
cargo run -p ployctl -- trading inspect example.live
cargo run -p ployctl -- claims inspect acct-live
cargo run -p ployctl -- claims run acct-live
```

For a full host-oriented checklist, use
[`live-deployment-checklist.md`](live-deployment-checklist.md).

## Intended Operator Contract

The current operator flow is:

```bash
ployd
ployctl system status
ployctl system audit
ployctl claims list
ployctl deployments list
ployctl deployments apply config/deployments/example.paper.json
ployctl deployments apply config/deployments/example.live.json
ployctl deployments inspect example.paper
ployctl deployments inspect example.live
ployctl trading inspect example.paper
ployctl trading inspect example.live
ployctl trading cancel example.live <order-id>
ployctl deployments drain example.live
ployctl deployments pause example.live
ployctl deployments resume example.live
ployctl deployments stop example.live
ploytui
curl -N http://127.0.0.1:8081/api/events/stream
```

`ployd` is the default long-running daemon entrypoint and `ployctl` is an
HTTP-first operator client with snapshot fallback. `ploytui` is the thin
terminal console on top of the same control-plane API.

`ployd` also keeps an append-only `run/platform/audit-log.jsonl` for
authenticated control-plane actions, and `ployctl system audit` reads the
latest entries back over `/api/audit/logs`.

`ployctl system status` now also shows:

- `live_reconcile_failures`
- `next_live_reconcile_at`
- `last_live_reconcile_error`

These are the primary live-venue outage signals.

Account-level auto-claim is default-on for live accounts, and `ployctl claims`
is the manual inspection / override surface. Auto-claim submits redeem
transactions through the Polymarket relayer and resolves final receipts via
Polygon RPC.

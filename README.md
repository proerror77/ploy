# Ploy

A high-performance Polymarket trading bot focused on crypto and sports prediction markets. Ships with a terminal dashboard, multi-agent coordinator, AI-assisted analysis, and optional reinforcement learning.

## Trading Platform Workspace

The workspace now also includes the platform-refactor spine:

- `ployd`: daemon entrypoint
- `ployctl`: operator client entrypoint
- `ploytui`: thin terminal operator console
- `crates/ploy-platform`: control-plane core
- `crates/ploy-trading`: canonical trading lifecycle
- `crates/ploy-deployments`: worker protocol and supervisor
- `crates/ploy-operator-contracts`: shared DTO and event contracts
- `crates/ploy-strategy-bundles`: signal-to-intent runtime
- `crates/ploy-research`: replay and backtesting consumer of trading models

Current smoke path:

```bash
cargo run -p ployd
cargo run -p ployctl -- system status
cargo run -p ployctl -- system audit
cargo run -p ployctl -- trading status
cargo run -p ployctl -- deployments apply config/deployments/example.paper.json
cargo run -p ployctl -- deployments list
cargo run -p ployctl -- deployments inspect example.paper
cargo run -p ployctl -- claims list
cargo run -p ploytui
# realtime operator stream
curl -N http://127.0.0.1:8081/api/events/stream
rtk cargo test --test platform_smoke -- --nocapture
```

Optional admin auth:

- Set `PLOY_ADMIN_TOKEN` or `PLOY_API_ADMIN_TOKEN` before booting `ployd` to require a bearer token on the control-plane API.
- Set `PLOY_SIDECAR_AUTH_TOKEN` if you want a read-only agent/sidecar credential for system status, deployment snapshots, trading snapshots, and the SSE event stream.
- Set `PLOY_API_AUTH_COOKIE_SECRET` as well if you want browser auth cookies to stay valid across daemon restarts or multiple instances.
- Set `PLOY_REQUEST_RATE_LIMIT_PER_MINUTE` if you need to tighten or relax the daemon HTTP request throttle. `0` disables the limiter.
- Set `PLOY_LIVE_RECONCILE_BACKOFF_BASE_MS` and `PLOY_LIVE_RECONCILE_BACKOFF_MAX_MS` if you want to tune venue outage retry backoff for live fill reconciliation.
- `ployctl` will automatically reuse `PLOY_ADMIN_TOKEN`, `PLOY_API_ADMIN_TOKEN`, or `PLOY_API_KEY` for authenticated requests.
- Browser operator surfaces authenticate through `/auth/login`, which now sets an `HttpOnly` same-site signed session cookie so the frontend event stream can stay authenticated without storing the raw admin token in browser storage.
- `ployd` appends authenticated control-plane activity to `run/platform/audit-log.jsonl`, and `ployctl system audit` reads that stream back through the daemon API.
- `ployctl system status` now surfaces live reconcile failure count, next retry time, and the last reconcile error so venue outages are visible without digging through logs.

Runbooks:

- [`docs/runbooks/platform-startup.md`](docs/runbooks/platform-startup.md)
- [`docs/runbooks/platform-deploy.md`](docs/runbooks/platform-deploy.md)
- [`docs/runbooks/live-deployment-checklist.md`](docs/runbooks/live-deployment-checklist.md)
- [`docs/runbooks/research-backtest-routing.md`](docs/runbooks/research-backtest-routing.md)

Default release workflow:

- `.github/workflows/release-platform.yml`

Compatibility note:

- `ployd`, `ployctl`, and `ploytui` are the default workspace entrypoints for the trading platform spine.
- The old root runtime tree has been retired from the compiled workspace.
- The root `ploy` binary in this workspace is only a compatibility shim.
- Historical `ploy ...` research and backfill commands remain archive/reference material until a dedicated research CLI returns.

## Features

- **Two runtime domains** -- Crypto (BTC/ETH/SOL UP/DOWN), Sports (NBA/NFL live odds)
- **Multiple strategies** -- Momentum, Split-Arb, Event-Edge mispricing scanner, NBA Q3-Q4 comeback, market making
- **Coordinator-managed runtime** -- Canonical strategy runtime with central order queue, governance gate, risk gate, and position aggregation
- **Event registry** -- Automated DISCOVER -> RESEARCH -> MONITOR -> TRADE pipeline for new markets
- **TUI dashboard** -- Ratatui-based terminal UI with live positions, quotes, Binance price feed, and trade log
- **Claude AI agent** -- Advisory, autonomous, and chat modes for market analysis and trade execution
- **Reinforcement learning** -- PPO training, lead-lag strategies, ONNX inference (optional `rl` / `onnx` feature flags)
- **Persistence** -- PostgreSQL event store, checkpoints, dead-letter queue, and crash recovery
- **Risk management** -- Position limits, circuit breaker, daily loss limit, slippage protection, emergency stop

## Architecture (Layered Live Runtime)

Production runtime now uses a 4-plane model:

- **Strategy Plane**: `crates/ploy-strategy-bundles` turns market signals into intents.
- **Capital Governance Plane**: deployment- and account-level controls live in `crates/ploy-platform`.
- **Execution Plane**: `crates/ploy-trading` and `crates/ploy-connectivity` own the canonical order/fill/position lifecycle.
- **Control Plane**: `apps/ployd` exposes deployment lifecycle, health, audit, and operator APIs.

Key rule: OpenClaw does not sit in the synchronous per-order decision path for HFT. It governs boundaries; strategies decide entries/exits inside those boundaries.

Live and paper strategies now start only through managed deployment resources.

Collector / backfill command routing is documented in [docs/COLLECTOR_RUNBOOK.md](docs/COLLECTOR_RUNBOOK.md).

New live strategies should land as deployment-backed bundle runtime logic rather
than as direct single-binary live entrypoints.

The default workspace control plane is:
- `GET /api/system/status`
- `GET /api/audit/logs`
- `GET /api/deployments`
- `GET /api/deployments/:id`
- `POST /api/deployments/:id/control`
- `GET /api/trading/state`
- `POST /api/deployments/:id/intents`
- `POST /api/deployments/:id/orders/:order_id/cancel`
- `POST /api/deployments/:id/orders/:order_id/replace`
- `GET /api/events/stream`

The older `strategies/control`, `strategy-evaluations`, `/api/sidecar/*`, and
`ploy rpc` surfaces are historical reference only in this branch and are no
longer part of the default operator path.

## Prerequisites

- **Rust** 1.75+ (2021 edition)
- **PostgreSQL** 15+ with an active database for event store, checkpoints, and strategy state
- **Polymarket account** with API credentials and a funded wallet on Polygon
- (Optional) `ANTHROPIC_API_KEY` for Claude AI agent commands
- (Optional) `GROK_API_KEY` for Grok-based sports analysis

## Installation

```bash
# Clone and build
git clone https://github.com/proerror77/ploy.git
cd ploy
cargo build --release

# Build with optional feature flags
cargo build --release --features rl        # Reinforcement learning (burn + ndarray)
cargo build --release --features onnx      # ONNX model inference (tract)
cargo build --release --features analysis  # DuckDB parquet analysis
```

Run database migrations before first use:

```bash
export DATABASE_URL="postgres://localhost/ploy"
sqlx migrate run
```

## Configuration

### Environment Variables

The table below covers the current workspace platform path only.

| Variable | Required | Description |
|----------|----------|-------------|
| `PLOY_LISTEN_ADDR` | No | Control-plane listen address for `ployd` (default `127.0.0.1:8081`) |
| `PLOY_API_ADMIN_TOKEN` | Recommended | Admin token for protected control-plane routes and operator actions |
| `PLOY_ADMIN_TOKEN` | Recommended | CLI/browser fallback token name if `PLOY_API_ADMIN_TOKEN` is unset |
| `PLOY_SIDECAR_AUTH_TOKEN` | No | Read-only token for sidecar and event consumers |
| `PLOY_API_AUTH_COOKIE_SECRET` | Recommended | Keeps browser auth cookies valid across daemon restarts |
| `PLOY_REQUEST_RATE_LIMIT_PER_MINUTE` | No | Daemon-side HTTP rate limit (`0` disables it) |
| `PLOY_ACCOUNT_CLAIM_STATE_FILE` | No | Override account auto-claim snapshot path |
| `PLOY_CLAIM_TICK_INTERVAL_MS` | No | Account auto-claim loop interval in milliseconds |
| `PLOY_CLAIM_BACKOFF_BASE_MS` | No | Base backoff for claim retries in milliseconds |
| `PLOY_CLAIM_BACKOFF_MAX_MS` | No | Max backoff for claim retries in milliseconds |
| `PLOY_LIVE_RECONCILE_BACKOFF_BASE_MS` | No | Base backoff for live fill reconciliation |
| `PLOY_LIVE_RECONCILE_BACKOFF_MAX_MS` | No | Max backoff for live fill reconciliation |
| `POLYMARKET_PRIVATE_KEY` | Yes for live | Wallet private key for live order signing and auto-claim |
| `POLYMARKET_API_KEY` | Yes for live | Polymarket CLOB API key |
| `POLYMARKET_API_SECRET` | Yes for live | Polymarket CLOB API secret |
| `POLYMARKET_PASSPHRASE` | Yes for live | Polymarket CLOB passphrase |
| `POLY_SIGNATURE_TYPE` | Yes for live | `proxy` or `gnosis_safe` for the current live claim path |
| `POLY_FUNDER` | Required for proxy live wallets | Proxy/Magic wallet funder address |
| `POLY_RELAYER_URL` | Yes for live auto-claim | Polymarket relayer base URL |
| `POLY_BUILDER_API_KEY` | Yes for live auto-claim | Builder API key for relayer submit auth |
| `POLY_BUILDER_SECRET` | Yes for live auto-claim | Builder secret used to sign relayer submit headers |
| `POLY_BUILDER_PASSPHRASE` | Yes for live auto-claim | Builder passphrase for relayer submit auth |
| `POLYGON_RPC_URL` | Yes for live auto-claim | Polygon RPC used to resolve relayed tx receipts |

For offline research/backtest paths, see
[`docs/runbooks/research-backtest-routing.md`](docs/runbooks/research-backtest-routing.md).
Archived single-binary runtime flags are intentionally omitted here.

### Config Directories

- `config/deployments/`: current paper/live deployment manifests for `ployd`
- `config/platform/`: daemon-level host configuration notes
- `config/strategies/`: research/backtest and compatibility strategy profiles

## Usage

### Current Platform Path

Use the workspace platform for any paper/live deployment runtime:

```bash
cargo run -p ployd
cargo run -p ployctl -- system status
cargo run -p ployctl -- system audit
cargo run -p ployctl -- deployments apply config/deployments/example.paper.json
cargo run -p ployctl -- deployments inspect example.paper
cargo run -p ployctl -- deployments apply config/deployments/example.live.json
cargo run -p ployctl -- deployments inspect example.live
cargo run -p ployctl -- trading inspect example.live
cargo run -p ployctl -- claims list
cargo run -p ployctl -- claims inspect acct-live
cargo run -p ployctl -- claims run acct-live
cargo run -p ploytui -- --watch
```

For a host-oriented version of the same flow, use:

- [`docs/runbooks/platform-startup.md`](docs/runbooks/platform-startup.md)
- [`docs/runbooks/live-deployment-checklist.md`](docs/runbooks/live-deployment-checklist.md)
- [`docs/runbooks/platform-deploy.md`](docs/runbooks/platform-deploy.md)

The workspace platform supports default-on account-level auto-claim for live
Polymarket accounts. `proxy` wallets redeem directly through the relayer.
`gnosis_safe` wallets use the same relayer-backed redeem path and auto-submit
`SAFE-CREATE` on the first claim if the SAFE has not been deployed yet.

The operator surface for this lives on `ployd` / `ployctl claims ...`, not on a
retired single-binary claimer path.

### Research And Backtest Path

Use the research/backtest side of the repo when you want datasets, replay
tables, or backtest prep:

- `crates/ploy-research`
- `config/strategies/*.toml`
- archived collector/backfill workflows kept outside the default `ployd`
  runtime path

The current workspace root `ploy` binary is only a compatibility shim. Treat
historical `ploy collect`, `ploy orderbook-history`, and `ploy strategy
backfill-*` examples as archive/reference material, not as live entrypoints on
this branch.

See:

- [`docs/runbooks/research-backtest-routing.md`](docs/runbooks/research-backtest-routing.md)
- [`docs/COLLECTOR_RUNBOOK.md`](docs/COLLECTOR_RUNBOOK.md)

Do not try to run backtests inside `ployd`, and do not point
`ployctl deployments apply` at files under `config/strategies/`.

### Archived Compatibility Surfaces

The old single-binary `ploy platform start ...`, `ploy rpc`, and direct
strategy live-entry paths are compatibility references only in this branch.
They are not the default operator path.

## Architecture

Ploy is organized around a multi-domain platform where each prediction market category (currently crypto and sports/NBA) has a dedicated trading agent. The agents submit orders through a central coordinator that applies risk checks, queues orders, and dispatches them to the Polymarket CLOB via authenticated API calls.

Strategies run independently and can be managed as daemons (start/stop/status). The event registry continuously discovers new markets, scores them for edge, and promotes them through a funnel from discovery to active trading. Persistence is handled by PostgreSQL with an event store for auditability, a checkpoint system for crash recovery, and a dead-letter queue for failed operations.

```
apps/
  ployd/         Trading platform daemon entrypoint
  ployctl/       Operator client entrypoint
  ploytui/       Thin terminal operator console
crates/
  ploy-platform/ Control-plane core
  ploy-trading/  Canonical trading lifecycle
  ploy-deployments/ Deployment worker protocol + supervisor
  ploy-operator-contracts/ Shared API and event contracts
  ploy-strategy-bundles/ Signal-to-intent runtime
  ploy-research/ Replay and backtest consumers
ploy-frontend/   Web operator console
ploy-sidecar/    Agent-facing sidecar client
config/          Platform and deployment configuration
docs/            Design docs, runbooks, and migration notes
```

## Development

```bash
cargo run -p ployd                   # Boot the platform daemon
cargo run -p ployctl -- system status
cargo run -p ployctl -- trading status
cargo run -p ployctl -- deployments apply config/deployments/example.paper.json
cargo run -p ployctl -- deployments list
cargo run -p ployctl -- deployments inspect example.paper
cargo run -p ploytui
curl -N http://127.0.0.1:8081/api/events/stream
rtk cargo check -p ployd             # Fast daemon type-check loop
rtk cargo check -p ployctl           # Fast client type-check loop
rtk cargo check -p ploytui           # Fast terminal console type-check loop
rtk cargo test --test platform_smoke platform_smoke_registers_and_starts_one_deployment -- --nocapture
cargo fmt --check                    # Check formatting
cargo clippy -- -D warnings          # Lint
rtk cargo build -p ployd             # Build the daemon binary
rtk cargo build -p ployctl           # Build the operator client binary
rtk cargo build -p ploytui           # Build the terminal console binary
```

See [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md) for the contributor guide.

## License

MIT

## Disclaimer

This software is for educational and research purposes only. Trading on prediction markets carries substantial risk of financial loss. Always start with `dry_run.enabled = true` and verify behavior before committing real funds. Use at your own risk.

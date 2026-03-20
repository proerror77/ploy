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
cargo run -p ployctl -- trading status
cargo run -p ployctl -- deployments apply config/deployments/example.paper.json
cargo run -p ployctl -- deployments list
cargo run -p ployctl -- deployments inspect example.paper
cargo run -p ployctl -- trading cancel example.live <order-id>
cargo run -p ploytui
# realtime operator stream
curl -N http://127.0.0.1:8081/api/events/stream
rtk cargo test --test platform_smoke -- --nocapture
```

Optional admin auth:

- Set `PLOY_ADMIN_TOKEN` or `PLOY_API_ADMIN_TOKEN` before booting `ployd` to require a bearer token on the control-plane API.
- `ployctl` will automatically reuse `PLOY_ADMIN_TOKEN`, `PLOY_API_ADMIN_TOKEN`, or `PLOY_API_KEY` for authenticated requests.

Runbooks:

- [`docs/runbooks/platform-startup.md`](docs/runbooks/platform-startup.md)
- [`docs/runbooks/platform-deploy.md`](docs/runbooks/platform-deploy.md)

Default release workflow:

- `.github/workflows/release-platform.yml`

Compatibility note:

- `ployd`, `ployctl`, and `ploytui` are the default workspace entrypoints for the trading platform spine.
- The old root runtime tree has been retired from the compiled workspace.
- Remaining `ploy ...` examples below are historical reference only and are not runnable entrypoints in this branch.

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

- **Strategy Plane**: canonical `Strategy` implementations decide direction, timing, sizing, and state transitions.
- **Capital Governance Plane**: OpenClaw-style governance agents manage budget, pause/resume, throttle, and deployment-scoped policy.
- **Execution Plane**: the coordinator is the only live order ingress (`StrategyIntent -> Governance/Risk Gate -> Queue -> Executor`), plus audit trail and recovery.
- **Control Plane**: deployment/config projection, lifecycle control, health, observability, and rollout/shutdown wiring.

Key rule: OpenClaw does not sit in the synchronous per-order decision path for HFT. It governs boundaries; strategies decide entries/exits inside those boundaries.

Live strategies now start only through the canonical managed `Strategy` runtime.

Collector / backfill command routing is documented in [docs/COLLECTOR_RUNBOOK.md](docs/COLLECTOR_RUNBOOK.md).

New live strategies should implement the canonical `Strategy` contract.
`TradingAgent` / `DomainAgent` are retired and only remain in historical design docs.
For machine-readable control-plane discovery, query `GET /api/capabilities`.
For plugin/runtime lifecycle visibility, query `GET /api/system/capabilities`; it now reports deployment state counts (`enabled|draining|disabled|archived`) plus builtin plugin summaries.
For account-scoped lifecycle visibility, query `GET /api/system/accounts`; it now reports per-account deployment state counts and the runtime budget snapshot.
For deployment/runtime control projection, query `GET /api/strategies/control` (admin token).
For targeted deployment control patch, use `PUT /api/strategies/control/:id`.
`strategies/control` now includes `strategy_version`, `lifecycle_stage` (`backtest|paper|shadow|live`), `product_type` (`binary_option` default), and evaluation snapshots.
Live sidecar ingress enforces `lifecycle_stage=live` by default (temporary migration override: `PLOY_ALLOW_NON_LIVE_DEPLOYMENT_INGRESS=true`).
Traceable strategy evidence ledger is available via `GET/POST /api/strategy-evaluations` and `GET /api/strategy-evaluations/:deployment_id/latest`.
Operator terminal control is exposed via `GET /api/operator/status` and `POST /api/operator/actions` (admin token). The first version is intentionally limited to coordinator-backed ops actions: `pause`, `resume`, `force_close`, `claim_check`, and `claim_run`.

Governance agents live under `crate::agents`; canonical live strategy runtime ownership lives under `crate::strategy`, `crate::coordinator`, and `crate::plugins`.

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

| Variable | Required | Description |
|----------|----------|-------------|
| `POLYMARKET_PRIVATE_KEY` | Yes | Ethereum private key for order signing |
| `POLYMARKET_API_KEY` | Yes | Polymarket CLOB API key |
| `POLYMARKET_API_SECRET` | Yes | Polymarket CLOB API secret |
| `POLYMARKET_PASSPHRASE` | Yes | Polymarket CLOB passphrase |
| `POLYMARKET_FUNDER` | No | Proxy/Magic wallet address |
| `DATABASE_URL` | Yes | PostgreSQL connection string (overrides config) |
| `ANTHROPIC_API_KEY` | No | Required for `agent` and AI-powered commands |
| `ANTHROPIC_BASE_URL` | No | Optional Anthropic-compatible base URL (examples: MiniMax `https://api.minimaxi.com/anthropic` or `https://api.minimax.io/anthropic`) |
| `ANTHROPIC_DEFAULT_OPUS_MODEL` | No | Optional alias override (example: map `opus` → `MiniMax-M2.5`) |
| `ANTHROPIC_CUSTOM_HEADERS` | No | Optional custom headers in newline-separated `Header: Value` format (example: `Authorization: Bearer <key>`) |
| `GROK_API_KEY` | No | Required for Grok-based sports analysis |
| `PLOY_RISK__CRYPTO_ALLOCATION_PCT` | No | Domain capital split (0..1), used to derive crypto exposure cap |
| `PLOY_RISK__SPORTS_ALLOCATION_PCT` | No | Domain capital split (0..1), used to derive sports exposure cap |
| `PLOY_RISK__CRYPTO_MAX_EXPOSURE_USD` | No | Hard crypto domain exposure cap (overrides pct-derived cap) |
| `PLOY_RISK__SPORTS_MAX_EXPOSURE_USD` | No | Hard sports domain exposure cap (overrides pct-derived cap) |
| `PLOY_RISK__CRYPTO_DAILY_LOSS_LIMIT_USD` | No | Hard crypto domain daily loss stop |
| `PLOY_RISK__SPORTS_DAILY_LOSS_LIMIT_USD` | No | Hard sports domain daily loss stop |
| `PLOY_RISK__MAX_DRAWDOWN_USD` | No | Hard drawdown stop (runtime cumulative realized curve) |
| `PLOY_ACCOUNT_ID` | No | Runtime account scope identifier (default `default`) |
| `PLOY_API_PORT` | No | API listen port for `ploy serve` and dashboard operator polling (default `8081`) |
| `PLOY_API_ADMIN_TOKEN` | No | Admin token for protected API routes, including operator terminal actions |
| `PLOY_ADMIN_TOKEN` | No | Dashboard-side fallback token name for operator polling/actions if `PLOY_API_ADMIN_TOKEN` is unset |
| `PLOY_DRY_RUN__ENABLED` | No | Force runtime dry-run mode (`true`/`false`) |
| `PLOY_DEPLOYMENTS_REQUIRE_EVIDENCE` | No | Require strategy evidence before enabling deployments (`true`/`false`) |
| `PLOY_DEPLOYMENTS_REQUIRED_STAGES` | No | Required evidence stages CSV (default `backtest,paper`) |
| `PLOY_DEPLOYMENTS_MAX_EVIDENCE_AGE_HOURS` | No | Max evidence staleness window in hours (default `168`) |
| `PLOY_ALLOW_DIRECT_LIVE` | No | Allow direct (non-Coordinator) live order paths. Not recommended. |
| `PLOY_ALLOW_DIRECT_STRATEGY_LIVE` | No | Archived compatibility flag for the retired single-binary runtime; not part of the workspace default path. |

### Config File

The default configuration lives in `config/default.toml`. Override the path with `--config` / `-c`.

| Section | Key examples |
|---------|-------------|
| `[market]` | `ws_url`, `rest_url`, `market_slug` |
| `[strategy]` | `shares`, `window_min`, `move_pct`, `sum_target`, `fee_buffer`, `slippage_buffer`, `profit_buffer` |
| `[execution]` | `order_timeout_ms`, `max_retries`, `max_spread_bps`, `poll_interval_ms` |
| `[risk]` | `max_single_exposure_usd`, `min_remaining_seconds`, `max_consecutive_failures`, `daily_loss_limit_usd`, `leg2_force_close_seconds` |
| `[database]` | `url`, `max_connections` |
| `[dry_run]` | `enabled` (defaults to `true`) |
| `[logging]` | `level`, `json` |
| `[event_edge_agent]` | `enabled`, `trade`, `interval_secs`, `min_edge`, `max_entry`, `shares`, `cooldown_secs`, `max_daily_spend_usd`, `titles` |
| `[nba_comeback]` | `enabled`, `min_edge`, `max_entry_price`, `shares`, `min_deficit`, `max_deficit`, `target_quarter`, `espn_poll_interval_secs` |

See the inline comments in `config/default.toml` for a full explanation of every field.

## Usage

### Live Trading (Recommended)

Ploy uses a **Coordinator-only** live execution plane. For live orders, use the platform entry point:

Historical reference only: the old `ploy platform start ...` path has been retired from this branch. Use the `ployd` / `ployctl` workspace runbook instead.

Legacy commands that can place orders (example: `ploy run`, `ploy momentum`, `ploy split-arb`, `ploy crypto split-arb`, `ploy sports split-arb`, `ploy event-edge --trade`, `ploy agent --enable-trading`) are **blocked for live execution by default**.

If you need an explicit override (not recommended), set:

```bash
export PLOY_ALLOW_DIRECT_LIVE=true
```

### Global Flags

```
--dry-run  / -d    Override dry-run mode (no real orders)
--market   / -m    Override market slug from config
--config   / -c    Config file path (default: config/default.toml)
```

### Core Commands

```bash
ploy run                                       # Legacy bot loop (dry-run unless PLOY_ALLOW_DIRECT_LIVE=true)
ploy test                                      # Test Polymarket API connectivity
ploy serve --port 8081                         # API server for dashboards / control-plane clients
ploy dashboard --demo                          # TUI dashboard with sample data
ploy dashboard                                 # TUI dashboard with live data
ploy search "bitcoin"                          # Search Polymarket for markets
ploy book <token_id>                           # Show order book for a token
ploy current <series_id>                       # Show active market for a series
ploy watch --series 10423                      # Watch live market data in terminal
ploy account --positions                       # Show account balance and positions
ploy claim --check-only                        # Check claimable resolved positions
ploy history --limit 50                        # View recent trading history
ploy ev --price 95 --probability 97            # Calculate expected value for near-settlement bets
```

### Collector And Backfill

Use the right command for the right data job:

- `ploy collect` for continuous live/raw synchronized capture
- `ploy collect --check-only` for a lightweight freshness / duplicate report
- `ploy orderbook-history` for historical PM L2 snapshots by token ID
- `ploy deribit-iv-backfill` for historical Deribit IV bars
- `ploy strategy backfill-*` for offline replay / settlement / kline prep

See [docs/COLLECTOR_RUNBOOK.md](docs/COLLECTOR_RUNBOOK.md) for examples and workflow guidance.

### Strategies

```bash
ploy trade --series 10423 --shares 50 --dry-run          # Two-leg arbitrage on a price series
ploy momentum --symbols BTCUSDT --shares 100 --dry-run   # Binance BTCUSDT is the underlying signal feed; execution is PM YES/NO tokens
ploy momentum --predictive --min-time 300 --dry-run      # Predictive mode: early entry with TP/SL
ploy split-arb --max-entry 35 --shares 100 --dry-run     # Split arbitrage (time-separated hedge)
ploy market-make --token <token_id>            # Market making opportunity analysis
ploy scan --series 10423 --watch               # Continuous arbitrage scan
ploy analyze --event <event_id>                # Analyze multi-outcome market
ploy paper --symbols BTCUSDT,ETHUSDT           # Paper mode using Binance underlyings (signals only, no PM orders)
```

Live momentum mode now supports automatic post-settlement claims (redeem winning positions) when keys are configured:

```bash
export PLOY_AUTO_CLAIM=true                    # default true in live momentum mode
export CLAIMER_CHECK_INTERVAL_SECS=60          # optional
export CLAIMER_MIN_CLAIM_SIZE=1                # optional (USDC)
export CLAIMER_IGNORE_CONDITION_IDS=0xabc,0xdef # optional ignore list (prefix match)
export POLYGON_RPC_URL=https://polygon-rpc.com # optional RPC override
```

Recommended for gasless redeem via Polymarket Builder Relayer:

```bash
# Official Rust relayer client path is enabled by default
cargo run -- momentum --live

export CLAIMER_RELAYER_ENABLED=true
export POLY_BUILDER_API_KEY=xxx
export POLY_BUILDER_SECRET=base64_secret
export POLY_BUILDER_PASSPHRASE=xxx

# Keep false in production to avoid falling back to direct on-chain redeem.
# If true, fallback path requires native MATIC gas.
export CLAIMER_RELAYER_FALLBACK_ONCHAIN=false
```

If relayer credentials are incomplete, claimer will warn and require native MATIC for direct on-chain fallback.

Example: split 100u capital into crypto/sports 50/50 and hard-stop each domain at 45u daily loss:

```bash
export PLOY_RISK__CRYPTO_ALLOCATION_PCT=0.5
export PLOY_RISK__SPORTS_ALLOCATION_PCT=0.5
export PLOY_RISK__CRYPTO_DAILY_LOSS_LIMIT_USD=45
export PLOY_RISK__SPORTS_DAILY_LOSS_LIMIT_USD=45
```

### Event-Edge Scanner

```bash
ploy event-edge --title "Which company has the best AI model?"   # One-shot mispricing scan
ploy event-edge --title "..." --watch --interval-secs 30         # Continuous monitoring
ploy event-edge --event <id> --watch --trade --min-edge 0.08     # Auto-trade when +EV
```

### AI Agent

```bash
ploy agent --mode advisory                     # Get trading recommendations
ploy agent --mode autonomous --enable-trading  # (blocked by default; prefer platform mode)
ploy agent --chat                              # Interactive conversation
ploy agent --mode sports --sports-url <url>    # Sports-specific analysis
ploy rpc                                       # JSON-RPC 2.0 server over stdin/stdout
```

### Domain: Crypto

```bash
ploy crypto split-arb --coins SOL,ETH,BTC --dry-run      # Split-arb on crypto UP/DOWN markets
ploy crypto monitor --coins SOL,ETH             # Monitor crypto markets
```

### Domain: Sports

```bash
ploy sports split-arb --leagues NBA --dry-run              # Split-arb on sports markets
ploy sports monitor --leagues NBA                # Monitor sports markets
ploy sports draftkings --sport nba --min-edge 5  # DraftKings odds comparison
ploy sports analyze --team1 LAL --team2 BOS      # Analyze a specific matchup
ploy sports polymarket --league nba --live       # Browse Polymarket sports markets
ploy sports chain --team1 LAL --team2 BOS        # Full decision chain (Grok -> Claude -> DK -> PM)
ploy sports live-scan --sport nba --min-edge 3   # Continuous live edge scanner
```

### Strategy Management

```bash
ploy strategy list                              # List all strategies and status
ploy strategy start momentum --dry-run          # Start a strategy
ploy strategy stop momentum                     # Stop a running strategy
ploy strategy status                            # Show status of all strategies
ploy strategy logs momentum --follow            # Tail strategy logs
ploy strategy reload momentum                   # Hot-reload strategy config
ploy strategy nba-seed-stats --season 2025-26   # Seed NBA comeback stats into DB
ploy strategy nba-comeback --dry-run            # Run NBA comeback agent standalone
ploy strategy accuracy --lookback-hours 12      # Report prediction accuracy
```

### Archived Single-Binary Platform CLI

The old `ploy platform start ...` commands are no longer runnable in this branch.
Use [`docs/runbooks/platform-startup.md`](docs/runbooks/platform-startup.md) for the workspace daemon/client flow.

### Operator Terminal

The dashboard now includes an `Operator` tab backed by the admin API. It is meant for runtime operations only and does not introduce a direct live order path.

Start the control plane and dashboard together:

```bash
export PLOY_API_ADMIN_TOKEN=change-me
ploy serve --port 8081
ploy dashboard
```

Behavior:

- `ploy dashboard` polls `http://127.0.0.1:${PLOY_API_PORT:-8081}` for `GET /api/operator/status`
- the dashboard sends operator actions with `x-ploy-admin-token`
- if `PLOY_API_ADMIN_TOKEN` is unset in the dashboard shell, it falls back to `PLOY_ADMIN_TOKEN`
- the first version supports only global/domain ops actions: `pause`, `resume`, `force_close`, `claim_check`, and `claim_run`
- all operator actions still flow through the existing coordinator/control plane

If the admin token is missing, the Operator tab remains visible but action requests fail closed.
See [docs/runbooks/operator-terminal.md](docs/runbooks/operator-terminal.md) for the minimal operator flow.

Deployment matrix entries support runtime scope controls:

```json
{
  "id": "crypto-momentum-5m",
  "strategy": "momentum",
  "domain": "Crypto",
  "enabled": true,
  "account_ids": ["acct-main", "acct-paper"],
  "execution_mode": "any"
}
```

- `account_ids`: optional allow-list. Empty means all accounts.
- `execution_mode`: `any` | `dry_run_only` | `live_only`.

Strategy evidence is stored in `strategy_evaluations` and supports `BACKTEST` / `PAPER` / `LIVE` stages with auditable payloads (`evidence_ref`, `evidence_hash`, `evidence_payload`).  
Sidecar/API can write and query evidence via:
- `POST /api/sidecar/strategy-evaluations`
- `GET /api/sidecar/strategy-evaluations`

### RL Commands (requires `--features rl`)

```bash
ploy rl train --episodes 1000 --series 10423        # Train RL model
ploy rl run --model ./models/best --series 10423     # Live trading with RL
ploy rl eval --model ./models/best --data test.csv   # Evaluate model
ploy rl info --model ./models/best                   # Inspect model stats
ploy rl export --model ./models/best -o model.onnx   # Export for deployment
ploy rl backtest --episodes 100                      # Backtest on sample data
ploy rl lead-lag --episodes 1000 --symbol BTCUSDT    # Train lead-lag RL
ploy rl lead-lag-live --symbol BTCUSDT --market btc-price-series-15m  # Live lead-lag
ploy rl agent --symbol BTCUSDT --market btc-price-series-15m \
    --up-token <id> --down-token <id>                # Full RL agent integration
```

### Data Collection

```bash
ploy collect --symbols BTCUSDT --duration 60         # Collect data for lag analysis
ploy orderbook-history --asset-ids <ids>             # Backfill L2 orderbook history
```

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

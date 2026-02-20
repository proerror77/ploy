# Ploy

A high-performance Polymarket trading bot covering crypto, sports, and political prediction markets. Ships with a terminal dashboard, multi-agent coordinator, AI-assisted analysis, and optional reinforcement learning.

## Features

- **Three trading domains** -- Crypto (BTC/ETH/SOL UP/DOWN), Sports (NBA/NFL live odds), Politics (elections, approval ratings)
- **Multiple strategies** -- Momentum, Split-Arb, Event-Edge mispricing scanner, NBA Q3-Q4 comeback, market making
- **Multi-agent platform** -- Coordinator with central order queue, per-domain agents, risk gate, and position aggregation
- **Event registry** -- Automated DISCOVER -> RESEARCH -> MONITOR -> TRADE pipeline for new markets
- **TUI dashboard** -- Ratatui-based terminal UI with live positions, quotes, Binance price feed, and trade log
- **Claude AI agent** -- Advisory, autonomous, and chat modes for market analysis and trade execution
- **Reinforcement learning** -- PPO training, lead-lag strategies, ONNX inference (optional `rl` / `onnx` feature flags)
- **Persistence** -- PostgreSQL event store, checkpoints, dead-letter queue, and crash recovery
- **Risk management** -- Position limits, circuit breaker, daily loss limit, slippage protection, emergency stop

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
| `[event_edge_agent]` | `enabled`, `framework`, `trade`, `interval_secs`, `min_edge`, `max_entry`, `shares`, `cooldown_secs`, `max_daily_spend_usd`, `titles` |
| `[nba_comeback]` | `enabled`, `min_edge`, `max_entry_price`, `shares`, `min_deficit`, `max_deficit`, `target_quarter`, `espn_poll_interval_secs` |
| `[event_registry]` | `enabled`, `scan_interval_secs`, `sports_keywords`, `general_keywords` |

See the inline comments in `config/default.toml` for a full explanation of every field.

## Usage

### Global Flags

```
--dry-run  / -d    Override dry-run mode (no real orders)
--market   / -m    Override market slug from config
--config   / -c    Config file path (default: config/default.toml)
```

### Core Commands

```bash
ploy run                                       # Start the main trading loop
ploy test                                      # Test Polymarket API connectivity
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

### Strategies

```bash
ploy trade --series 10423 --shares 50          # Two-leg arbitrage on a price series
ploy momentum --symbols BTCUSDT --shares 100   # CEX momentum strategy
ploy momentum --predictive --min-time 300      # Predictive mode: early entry with TP/SL
ploy split-arb --max-entry 35 --shares 100     # Split arbitrage (time-separated hedge)
ploy market-make --token <token_id>            # Market making opportunity analysis
ploy scan --series 10423 --watch               # Continuous arbitrage scan
ploy analyze --event <event_id>                # Analyze multi-outcome market
ploy paper --symbols BTCUSDT,ETHUSDT           # Paper trading mode (signals only)
```

Live momentum mode now supports automatic post-settlement claims (redeem winning positions) when keys are configured:

```bash
export PLOY_AUTO_CLAIM=true                    # default true in live momentum mode
export CLAIMER_CHECK_INTERVAL_SECS=60          # optional
export CLAIMER_MIN_CLAIM_SIZE=1                # optional (USDC)
export CLAIMER_IGNORE_CONDITION_IDS=0xabc,0xdef # optional ignore list (prefix match)
export POLYGON_RPC_URL=https://polygon-rpc.com # optional RPC override
```

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
ploy agent --mode autonomous --enable-trading  # AI-controlled trading
ploy agent --chat                              # Interactive conversation
ploy agent --mode sports --sports-url <url>    # Sports-specific analysis
ploy rpc                                       # JSON-RPC 2.0 server over stdin/stdout
```

### Domain: Crypto

```bash
ploy crypto split-arb --coins SOL,ETH,BTC      # Split-arb on crypto UP/DOWN markets
ploy crypto monitor --coins SOL,ETH             # Monitor crypto markets
```

### Domain: Sports

```bash
ploy sports split-arb --leagues NBA,NFL          # Split-arb on sports markets
ploy sports monitor --leagues NBA                # Monitor sports markets
ploy sports draftkings --sport nba --min-edge 5  # DraftKings odds comparison
ploy sports analyze --team1 LAL --team2 BOS      # Analyze a specific matchup
ploy sports polymarket --league nba --live       # Browse Polymarket sports markets
ploy sports chain --team1 LAL --team2 BOS        # Full decision chain (Grok -> Claude -> DK -> PM)
ploy sports live-scan --sport nba --min-edge 3   # Continuous live edge scanner
```

### Domain: Politics

```bash
ploy politics markets --category presidential   # Browse political markets
ploy politics search "election"                 # Search political markets
ploy politics analyze --candidate "Trump"       # Analyze a candidate's markets
ploy politics trump --market-type favorability  # Trump-specific markets
ploy politics elections --year 2026             # Election markets by year
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

### Multi-Agent Platform

```bash
ploy platform start --crypto --sports --politics   # Start all domain agents
ploy platform start --crypto --dry-run             # Crypto agent only, dry-run
ploy platform start --sports --pause sports        # Start paused
```

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

Ploy is organized around a multi-domain platform where each prediction market category (crypto, sports, politics) has a dedicated trading agent. The agents submit orders through a central coordinator that applies risk checks, queues orders, and dispatches them to the Polymarket CLOB via authenticated API calls.

Strategies run independently and can be managed as daemons (start/stop/status). The event registry continuously discovers new markets, scores them for edge, and promotes them through a funnel from discovery to active trading. Persistence is handled by PostgreSQL with an event store for auditability, a checkpoint system for crash recovery, and a dead-letter queue for failed operations.

```
src/
  adapters/      Polymarket CLOB, WebSocket, Binance WS
  agents/        Domain trading agents (crypto, sports, politics)
  agent/         Claude AI agent integration
  coordinator/   Multi-agent coordinator + order queue
  domain/        Core types (Market, Order, Quote)
  persistence/   Event store, checkpoints, DLQ
  services/      Discovery, metrics, health
  signing/       Wallet, order signing, nonce manager
  strategy/      Trading strategies + risk + registry
  supervisor/    Watchdog, emergency stop, shutdown
  tui/           Terminal dashboard (ratatui)
config/          TOML configuration files
migrations/      PostgreSQL schema migrations
docs/            Extended documentation
examples/        Example integrations (OpenClaw RPC)
```

## Development

```bash
cargo test                           # Run test suite
cargo fmt --check                    # Check formatting
cargo clippy -- -D warnings          # Lint
cargo build --features rl,onnx       # Build with all optional features
```

See [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md) for the contributor guide.

## License

MIT

## Disclaimer

This software is for educational and research purposes only. Trading on prediction markets carries substantial risk of financial loss. Always start with `dry_run.enabled = true` and verify behavior before committing real funds. Use at your own risk.

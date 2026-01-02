# Ploy

A high-performance Polymarket trading bot with a cyberpunk-style terminal dashboard.

## Features

- **Real-time TUI Dashboard** - Monitor positions, market analysis, and transactions with a cyberpunk aesthetic
- **Multiple Trading Strategies**
  - Momentum trading based on CEX price movements
  - Split arbitrage (time-separated entry for hedged positions)
  - Two-leg arbitrage for binary markets
- **Claude AI Agent** - AI-powered trading assistance and market analysis
- **Live Data Feeds**
  - Polymarket CLOB WebSocket for quotes
  - Binance WebSocket for BTC prices
- **Order Execution** - Authenticated order placement with retry logic

## Installation

```bash
# Clone the repository
git clone https://github.com/proerror77/ploy.git
cd ploy

# Build the project
cargo build --release

# Run with demo data
cargo run -- dashboard --demo
```

## Configuration

Set environment variables for live trading:

```bash
export POLYMARKET_PRIVATE_KEY="your_private_key"
export POLYMARKET_API_KEY="your_api_key"
export POLYMARKET_API_SECRET="your_api_secret"
export POLYMARKET_PASSPHRASE="your_passphrase"
```

## Usage

### TUI Dashboard

```bash
# Demo mode with sample data
ploy dashboard --demo

# Live dashboard monitoring btc-15m series
ploy dashboard

# Monitor specific series
ploy dashboard -s btc-15m
```

**Dashboard Layout:**
```
┌── POSITIONS ────────────────────────────────────────────────────────┐
│  ▲ UP   [████████████░░░░░░░]  36,598  @0.4820  PnL: $-36.47       │
│         Cost: $17,657.51 | Avg: $0.4830                            │
│  ▼ DOWN [████████████████░░░]  36,317  @0.5420  PnL: $+2,458.33    │
│         Cost: $17,225.68 | Avg: $0.4743                            │
├── MARKET ANALYSIS ──────────────────────────────────────────────────┤
│  UP: $0.4816   DOWN: $0.5423   Combined: $1.0239   Spread: -2.39%  │
│  Pairs: 36,317 | Delta: +281 | Total PnL: $+2,417.00               │
├── RECENT TRANSACTIONS ──────────────────────────────────────────────┤
│  TIME          SIDE      PRICE    SIZE     BTC PRICE   TX HASH     │
│  09:54:32.12   ▲ UP      $0.4602  287 $    97,136     0x90ba9f5c...│
│  09:54:27.12   ▼ DOWN    $0.4983  337 $    97,236     0x56af6665...│
├─────────────────────────────────────────────────────────────────────┤
│  Trades: 127 │ Volume: $34,902.87 │ ⏱ 0:27 │ DRY RUN │ watching    │
└─────────────────────────────────────────────────────────────────────┘
```

**Keyboard Controls:**
- `q` / `Esc` - Quit
- `↑` / `k` - Scroll up
- `↓` / `j` - Scroll down
- `?` - Help

### Trading Strategies

```bash
# Momentum strategy - trade based on CEX price movements
ploy momentum -s BTCUSDT --shares 100 --threshold 0.5

# Split arbitrage - time-separated hedged entries
ploy split-arb -s btc-15m --shares 500 --max-entry 0.48

# Dry run mode (no real orders)
ploy -d momentum -s BTCUSDT
```

### AI Agent

```bash
# Advisory mode - get trading recommendations
ploy agent --mode advisory

# Autonomous mode - AI-controlled trading
ploy agent --mode autonomous --enable-trading

# Chat mode - interactive conversation
ploy agent --chat
```

### Other Commands

```bash
# Test API connection
ploy test

# Search markets
ploy search "bitcoin"

# Show order book
ploy book <token_id>

# View account balance and positions
ploy account

# Analyze market making opportunities
ploy market-make <token_id>
```

## Architecture

```
src/
├── adapters/          # External service integrations
│   ├── binance_ws.rs  # Binance WebSocket client
│   ├── polymarket_clob.rs  # Polymarket CLOB API
│   └── polymarket_ws.rs    # Polymarket WebSocket
├── agent/             # Claude AI integration
│   ├── advisor.rs     # Advisory agent
│   ├── autonomous.rs  # Autonomous trading agent
│   └── client.rs      # Claude API client
├── domain/            # Core domain models
│   ├── market.rs      # Market, Quote, Round
│   ├── order.rs       # Order, OrderRequest
│   └── state.rs       # Strategy states
├── strategy/          # Trading strategies
│   ├── momentum.rs    # Momentum trading
│   ├── split_arb.rs   # Split arbitrage
│   └── executor.rs    # Order execution
├── tui/               # Terminal UI
│   ├── app.rs         # Application state
│   ├── runner.rs      # Live data integration
│   ├── widgets/       # UI components
│   └── theme.rs       # Cyberpunk color scheme
└── signing/           # Wallet & authentication
    ├── wallet.rs      # Ethereum wallet
    └── order.rs       # Order signing
```

## Dependencies

- **ratatui** - Terminal UI framework
- **tokio** - Async runtime
- **ethers** - Ethereum wallet/signing
- **reqwest** - HTTP client
- **tokio-tungstenite** - WebSocket client

## License

MIT

## Disclaimer

This software is for educational purposes only. Trading cryptocurrencies and prediction markets involves substantial risk of loss. Use at your own risk.

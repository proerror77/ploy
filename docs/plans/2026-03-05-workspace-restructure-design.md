# Workspace Restructure Design

> Date: 2026-03-05
> Branch: `refactor/workspace-restructure`
> Approach: Big-bang, test-backed

## Problem

Single-crate monolith (160K lines, 26 modules, 93 strategy files) with:
- 3 different strategy startup methods (platform mode / strategy CLI / collector)
- 12 systemd services, 6 env templates (70% duplication)
- No shared backtest framework (4 independent engines)
- Risk logic scattered across 3 locations
- Dead code (~6K lines) mixed with production code
- God files (bootstrap.rs 7.4K, coordinator.rs 6.5K, cli/strategy.rs 6.3K)
- 107 pub exports from strategy/mod.rs

## Target Architecture

```
ploy/                              # Cargo workspace root
├── Cargo.toml                     # [workspace] members
│
├── crates/
│   ├── ploy-core/                 # Shared types + traits (~5K lines)
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── domain/            # Position, Order, Market, FeeModel
│   │       ├── config/            # Config traits + common config types
│   │       ├── error.rs           # PloyError
│   │       └── strategy/          # Strategy trait, StrategyAction, DataFeed trait
│   │
│   ├── ploy-data/                 # Data source layer (read-only quotes)
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── binance/           # Kline WS, depth stream
│   │       ├── deribit/           # IV surface
│   │       ├── espn/              # Sports event data
│   │       ├── chainlink/         # On-chain prices
│   │       └── traits.rs          # DataSource trait
│   │
│   ├── ploy-polymarket/           # Polymarket execution + data (first-class)
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── clob.rs            # CLOB REST (place/cancel/query orders)
│   │       ├── ws.rs              # WebSocket (orderbook, trades)
│   │       ├── signing/           # Wallet, nonce, auth, HMAC
│   │       ├── ctf.rs             # CTF contract interaction
│   │       ├── markets.rs         # Market discovery, search
│   │       └── types.rs           # Polymarket-specific types
│   │
│   ├── ploy-backtest/             # Unified backtest framework
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── engine.rs          # Shared BacktestEngine (replay + sim)
│   │       ├── feed.rs            # Historical data feeds (CSV, Parquet, DB)
│   │       ├── recorder.rs        # Trade recording + report generation
│   │       ├── report.rs          # Report analysis + suggestions
│   │       ├── execution_sim.rs   # Execution simulator (fill model)
│   │       ├── strategies/        # Per-strategy signal implementations
│   │       │   ├── mod.rs
│   │       │   ├── directional.rs
│   │       │   ├── staggered_arb.rs
│   │       │   ├── liquidity_vacuum.rs
│   │       │   ├── garch_probability.rs
│   │       │   └── momentum.rs
│   │       └── bin/
│   │           └── backtest.rs    # Backtest CLI entry point
│   │
│   └── ploy-risk/                 # Unified risk management
│       └── src/
│           ├── lib.rs
│           ├── risk_manager.rs    # Centralized risk checks
│           ├── slippage.rs        # Slippage protection
│           ├── validation.rs      # Validation chain
│           └── circuit_breaker.rs # Circuit breaker pattern
│
├── src/                           # ploy-app main binary (~80K lines)
│   ├── main.rs
│   ├── cli/                       # Unified CLI (slimmed down)
│   │   ├── mod.rs
│   │   ├── runtime.rs             # Clap definitions
│   │   ├── strategy.rs            # → split into strategy/ directory
│   │   └── ...
│   ├── coordinator/               # Platform mode (single entry point)
│   │   ├── bootstrap.rs           # → split into phases
│   │   ├── coordinator.rs         # → extract handlers
│   │   └── ...
│   ├── strategy/                  # Live strategies only
│   │   ├── mod.rs                 # Minimal re-exports
│   │   ├── staggered_arb/         # Primary strategy (directory)
│   │   ├── gamma_scalping/
│   │   ├── momentum/              # Split from god file
│   │   ├── nba_comeback/
│   │   ├── event_edge/
│   │   ├── deribit_probability_arb/
│   │   ├── pattern_memory/
│   │   ├── execution/             # Execution engine
│   │   ├── feeds.rs
│   │   └── adapters.rs            # Strategy trait adapters
│   ├── account/                   # Account management (claimer moved here)
│   ├── services/
│   ├── persistence/
│   ├── agents/                    # Pull-based agents (coordinator-driven)
│   ├── coordination/              # Lifecycle primitives
│   └── tui/
│
├── deployment/                    # Simplified deployment
│   ├── ploy-platform.service      # Single platform service template
│   ├── ploy-collector.service     # Single collector service template
│   ├── env.base.example           # Shared base env
│   ├── env.crypto.example         # Crypto overlay
│   └── env.sports.example         # Sports overlay
│
├── ploy-sidecar/                  # Unchanged
├── ploy-openclaw/                 # Unchanged
└── ploy-frontend/                 # Unchanged
```

## Dependency Graph

```
ploy-core           ← serde, rust_decimal, thiserror (zero runtime deps)
ploy-data           ← ploy-core, tokio, tokio-tungstenite, reqwest
ploy-polymarket     ← ploy-core, tokio, ethers, reqwest, hmac
ploy-risk           ← ploy-core, rust_decimal
ploy-backtest       ← ploy-core, ploy-data, sqlx (optional, for DB feeds)
ploy-app (src/)     ← all of the above + clap, ratatui, axum, sqlx
```

## Key Design Decisions

1. **Polymarket is first-class** — not "one of many exchanges", but THE execution venue
2. **Data sources are read-only** — Binance/Deribit/ESPN only provide quotes, never execute
3. **Platform mode is the only deployment model** — strategy CLI deprecated
4. **Backtest is isolated** — can't accidentally depend on live adapters
5. **Risk is centralized** — one crate, one source of truth
6. **Claimer → account management** — not a strategy, it's account ops
7. **Dead code deleted** — dump_hedge, live_arbitrage, validation.rs, platform/agents/

## What Gets Deleted

| Item | Lines | Reason |
|------|-------|--------|
| `src/strategy/dump_hedge.rs` | 915 | Unused, no references |
| `src/strategy/live_arbitrage.rs` | 649 | Unused, not even exported |
| `src/validation.rs` | ~200 | Orphaned module, zero references |
| `src/platform/agents/` | ~800 | Superseded by `src/agents/` (pull-based) |
| `src/strategy/strategies/` | ~300 | Legacy trait impls, unused |
| 8 of 12 systemd services | — | Replaced by 2 templates |
| 4 of 6 env templates | — | Replaced by base + overlay pattern |

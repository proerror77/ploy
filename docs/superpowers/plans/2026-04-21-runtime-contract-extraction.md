# Runtime Event Contract Extraction — Refactoring Blueprint

> **Goal:** Break the reverse dependency (ploy-market-data → ploy-strategy-bundles) by extracting runtime event contracts into a standalone crate. Establish PredictionFamily / InstrumentKind / VenueKind as first-class abstractions.

## Phase 1 — New crate: `ploy-market-contracts` (minimal, day 1)

### What moves out of `ploy-strategy-bundles/src/traits.rs`

| Type | Current location | New home |
|------|-----------------|----------|
| `MarketUpdate` enum | `traits.rs:23-133` | `ploy-market-contracts/src/events.rs` |
| `Feed` trait | `traits.rs:135-140` | `ploy-market-contracts/src/feed.rs` |
| `StrategyLogic` trait | `traits.rs:142+` | stays in `ploy-strategy-bundles` |
| `StrategyDecision` | `traits.rs` | stays in `ploy-strategy-bundles` |
| `SignalRecord` | `traits.rs` | stays in `ploy-strategy-bundles` |

### New crate structure

```
crates/ploy-market-contracts/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── events.rs        # MarketUpdate, EventFamily
    ├── feed.rs          # Feed trait
    ├── family.rs        # PredictionFamily enum
    ├── instrument.rs    # InstrumentKind enum
    └── venue.rs         # VenueKind enum
```

### `Cargo.toml` — ultra-lightweight

```toml
[package]
name = "ploy-market-contracts"
version.workspace = true
edition.workspace = true

[dependencies]
chrono = { workspace = true }
rust_decimal = { workspace = true }
serde = { workspace = true, features = ["derive"] }
async-trait = { workspace = true }
```

No sqlx, no reqwest, no polars. This crate compiles in < 2 seconds.

### New types

```rust
// family.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PredictionFamily {
    CryptoExpiry,      // BTC/ETH 5-min binary options
    SportsPregame,     // pre-match moneyline/spread/total
    SportsLive,        // in-play with score/clock
    Politics,          // election markets
    Custom(u16),       // extensible
}

// instrument.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstrumentKind {
    UpDown,            // crypto binary: will price be above X?
    YesNo,             // generic binary
    Moneyline,         // sports: who wins?
    Spread,            // sports: margin of victory
    Total,             // sports: over/under
}

// venue.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VenueKind {
    Polymarket,
    Kalshi,
    Sportsbook,
}
```

### Dependency graph after Phase 1

```
ploy-market-contracts  (new, ultra-light)
    ↑               ↑
ploy-market-data    ploy-strategy-bundles
    ↑                    ↑
ploy-research       ploy-strategy-runtime
                         ↑
                    apps/new-ploy-runner
```

No more reverse dependency. `ploy-market-data` produces `MarketUpdate` from `ploy-market-contracts`. `ploy-strategy-bundles` consumes it.

---

## Phase 2 — Migrate consumers (day 2-3)

### Step 1: `ploy-strategy-bundles`

```diff
- use crate::traits::{MarketUpdate, Feed};
+ use ploy_market_contracts::{MarketUpdate, Feed};
```

Keep `StrategyLogic`, `StrategyDecision`, `SignalRecord` in `ploy-strategy-bundles/src/traits.rs`. These are strategy-layer contracts, not market-layer.

Re-export from `ploy-strategy-bundles/src/lib.rs`:
```rust
pub use ploy_market_contracts::{MarketUpdate, Feed};
```

This preserves backward compatibility for existing consumers.

### Step 2: `ploy-market-data`

```diff
- use ploy_strategy_bundles::traits::MarketUpdate;
+ use ploy_market_contracts::MarketUpdate;
```

Files to change:
- `feeds.rs:12`
- `scanner.rs:14`
- `sports_feed.rs:7`

Remove `ploy-strategy-bundles` from `ploy-market-data/Cargo.toml` dependencies.

### Step 3: `ploy-research`

```diff
- use ploy_strategy_bundles::MarketUpdate;
+ use ploy_market_contracts::MarketUpdate;
```

### Step 4: `ploy-strategy-runtime`

No change needed — it depends on `ploy-strategy-bundles` which re-exports.

---

## Phase 3 — Sports family separation (day 4-5)

### `sports_pregame` vs `sports_live`

| Aspect | sports_pregame | sports_live |
|--------|---------------|-------------|
| Input | schedule, team stats, odds, PM moneyline | score, period, clock, live PM quote |
| Time horizon | hours to days before event | seconds to minutes during event |
| Signal | model probability vs market price | momentum, score delta, clock pressure |
| Existing code | new (to build) | `nba_comeback` pattern (exists) |
| DB tables | `nba_schedule_calendar` | `nba_live_observations` |

### New MarketUpdate variants for sports

```rust
// In ploy-market-contracts/src/events.rs
pub enum MarketUpdate {
    // ... existing variants ...

    /// Pre-game sports state (schedule, odds, team stats)
    SportsPregame {
        game_id: Arc<str>,
        league: Arc<str>,
        home_team: Arc<str>,
        away_team: Arc<str>,
        start_time: DateTime<Utc>,
        home_odds: f64,
        away_odds: f64,
        model_home_prob: Option<f64>,
        ts: DateTime<Utc>,
    },

    /// Live sports state (score, clock, momentum)
    SportsLive {
        game_id: Arc<str>,
        league: Arc<str>,
        period: Arc<str>,
        home_score: u32,
        away_score: u32,
        clock_remaining_secs: Option<u32>,
        momentum: f64,  // positive = home momentum
        ts: DateTime<Utc>,
    },
}
```

### Strategy registration

```toml
# config/strategies/sports-pregame-moneyline.toml
[runtime]
strategy_variant = "sports_pregame_moneyline"
prediction_family = "sports_pregame"
instrument_kind = "moneyline"
venue = "polymarket"
```

---

## Phase 4 — Retire legacy paths (day 6-7)

### Remove

| Path | Reason |
|------|--------|
| `Dockerfile` (root) | Uses old `ploy` binary with `--features rl,api` |
| `Dockerfile.collector` | Replaced by systemd services on Tango-1-1 |
| Old `ploy` binary references | Superseded by `new-ploy-runner` + `new-ployd` |

### Keep

| Path | Reason |
|------|--------|
| `release-platform.yml` | Correct per-binary build |
| `deploy-tango-1-1.yml` | Needs sudo fix but structure is right |
| `backtest.yml` | Working, uses Parquet pipeline |

---

## Execution order (minimal disruption)

1. **Create `ploy-market-contracts`** with `MarketUpdate` + `Feed` + new enums
2. **Add re-export** in `ploy-strategy-bundles` (backward compat)
3. **Migrate `ploy-market-data`** imports → remove reverse dependency
4. **Migrate `ploy-research`** imports
5. **Add `PredictionFamily` to config** (optional field, default `crypto_expiry`)
6. **Add sports variants** to `MarketUpdate`
7. **Build `sports_pregame` strategy** as new variant
8. **Retire old Dockerfile** and root binary

Each step compiles and tests independently. No big-bang migration.

# Phase 1A: Code Quality Review — Ploy Trading System

**Date**: 2026-03-08
**Scope**: Full Rust codebase (~165K lines, 260+ source files)
**Branch**: `hotfix/staggered-arb-release-20260306`

---

## Executive Summary

The Ploy codebase is a sophisticated trading system with strong domain modeling and good use of Rust's type system. However, it suffers from several structural issues that increase maintenance cost: a 7,761-line bootstrap god-module, pervasive series-ID magic-number duplication across 6+ files, inline DDL scattered across 9 files instead of proper migrations, and multiple 3,000-5,000 line strategy files with high cyclomatic complexity. Error handling is generally solid (thiserror, proper Result propagation), but two `let _ =` patterns in the coordinator silently discard position-tracking failures that could cause state drift in production.

**Finding counts by severity:**
- Critical: 2
- High: 7
- Medium: 8
- Low: 4

---

## 1. Code Complexity

### 1.1 [Critical] `bootstrap.rs` is a 7,761-line god module

**File**: `src/coordinator/bootstrap.rs`
**Lines**: 7,761

This single file contains:
- 15+ `ensure_*_table()` DDL functions with inline SQL (~600 lines of DDL)
- Schema repair logic (`ensure_schema_repairs`)
- Trade alert detection config and state machines
- Environment variable parsing helpers (`env_u64`, `env_bool`, `env_usize`, `env_i64`, `env_decimal`, `env_decimal_opt`)
- Market resolution detection
- Strategy deployment persistence
- Coin/selector parsing
- Account upsert logic
- The actual platform bootstrap orchestration

This violates the Single Responsibility Principle severely. Any change to schema, env parsing, or bootstrap logic requires touching this file, creating merge conflicts and cognitive overload.

**Fix**: Split into at least 4 modules:
```
src/coordinator/
  bootstrap.rs          # Only start_platform() orchestration (~500 lines)
  schema.rs             # All ensure_*_table() DDL functions
  env_helpers.rs        # env_u64, env_bool, env_decimal, etc.
  trade_alerts.rs       # TradeAlertConfig, TradeAlertState, TradeBurstAlert
```

### 1.2 [High] `staggered_arb_live.rs` — 5,173 lines, deeply nested entry logic

**File**: `src/strategy/staggered_arb_live.rs`
**Lines**: 5,173

The `try_entry_for_window()` method (lines ~1200-1670) is approximately 470 lines with 15+ sequential filter gates, each calling `bump_entry_reject_for_symbol()`. The `check_leg2_opportunities()` method (lines ~1677-1800+) iterates positions with 6+ levels of nesting.

The `StaggeredArbAdapter` struct itself has 25+ fields (lines 176-243), many of which are parallel HashMap tracking structures (`entry_reject_counts`, `entry_reject_counts_by_symbol`, `leg2_skip_counts`, `leg2_skip_counts_by_symbol`).

**Fix**: Extract entry signal evaluation into a pure function:
```rust
struct EntrySignal {
    direction: Direction,
    leg1_ask: Decimal,
    shares: u64,
    p_hat: f64,
    sigma: f64,
    obi: f64,
}

enum EntryRejection {
    TimeRemainingTooLow,
    MissingPmQuotes,
    SigmaBelowMin,
    // ... etc
}

fn evaluate_entry(
    window: &LiveWindow,
    config: &StaggeredArbBacktestConfig,
    spot: &SpotPrice,
    pm_asks: (Option<Decimal>, Option<Decimal>),
    obi: f64,
    ts: DateTime<Utc>,
) -> Result<EntrySignal, EntryRejection> { ... }
```

This makes the logic testable in isolation and reduces the method to ~50 lines of orchestration.

### 1.3 [High] `coordinator.rs` — 6,508 lines

**File**: `src/coordinator/coordinator.rs`
**Lines**: 6,508

Contains the Coordinator struct, governance policy logic, intent handling, queue draining, execution, position tracking, risk state persistence, and deployment ledger management. The `handle_order_intent()` method alone is ~120 lines of sequential guard checks.

**Fix**: Extract governance policy into `src/coordinator/governance.rs` and position/execution tracking into `src/coordinator/execution.rs`.

### 1.4 [Medium] `momentum.rs` — 3,838 lines with 170-field config

**File**: `src/strategy/momentum.rs`
**Lines**: 3,838

`MomentumConfig` has 30+ fields (lines 42-170). The `Default` impl is 60 lines. This config struct tries to cover every possible mode (confirmatory, directional, VWAP, Kelly sizing, OBI confirmation) in a single flat struct.

**Fix**: Use nested config groups:
```rust
pub struct MomentumConfig {
    pub entry: EntryConfig,
    pub risk: RiskControlConfig,
    pub detection: DetectionConfig,
    pub sizing: SizingConfig,
    pub vwap: VwapConfig,
    pub directional: DirectionalConfig,
}
```

---

## 2. Code Duplication

### 2.1 [High] Series ID magic numbers duplicated across 6+ files

The Polymarket series-to-symbol mapping (`"10684" => "BTCUSDT"`, `"10683" => "ETHUSDT"`, etc.) is hardcoded in:

1. `src/strategy/staggered_arb_live.rs` — `default_staggered_series_ids()` (line 50) and `series_to_symbol()` (line 379)
2. `src/strategy/adapters.rs` — `required_feeds()` (line 1107) and inline match (line 1446)
3. `src/strategy/momentum.rs` — `EventMatcher::new()` (line 342) and `horizon_for_series()` (line 391)
4. `src/main_modes/collector_modes.rs` — hardcoded array (line 16)
5. `src/analysis/updown_backtest.rs` — `horizon_for_series()` (line 148)

Adding a new symbol (e.g., DOGEUSDT) or a new series requires editing 6+ files.

**Fix**: Create a single source of truth:
```rust
// src/strategy/series_registry.rs
pub struct SeriesInfo {
    pub series_id: &'static str,
    pub symbol: &'static str,
    pub window_secs: u64,
    pub horizon: &'static str,
}

pub const SERIES_REGISTRY: &[SeriesInfo] = &[
    SeriesInfo { series_id: "10684", symbol: "BTCUSDT", window_secs: 300, horizon: "5m" },
    SeriesInfo { series_id: "10683", symbol: "ETHUSDT", window_secs: 300, horizon: "5m" },
    // ...
];

pub fn series_to_symbol(id: &str) -> Option<(&'static str, u64)> { ... }
pub fn default_series_ids() -> Vec<String> { ... }
pub fn horizon_for_series(id: &str) -> &'static str { ... }
```

### 2.2 [High] Environment variable parsing helpers duplicated across 7+ files

Variants of `env_bool`, `env_u64`, `env_usize`, `env_decimal` exist in:

1. `src/coordinator/bootstrap.rs` — `env_u64`, `env_bool`, `env_usize`, `env_i64`, `env_decimal`, `env_decimal_opt`
2. `src/config.rs` — `env_bool(keys: &[&str])`
3. `src/adapters/polymarket_clob.rs` — `env_bool(keys: &[&str])`
4. `src/api/handlers/capabilities.rs` — `env_bool_default_true`, `env_bool_default_false`
5. `src/api/handlers/sidecar.rs` — `env_bool(keys: &[&str])`
6. `src/agents/sports.rs` — `env_usize`
7. `src/strategy/claimer.rs` — `env_u64_any(keys: &[&str])`

Each has slightly different signatures and semantics (some take `&str`, some take `&[&str]`, some have defaults, some don't).

**Fix**: Create `src/util/env.rs`:
```rust
pub fn env_bool(name: &str, default: bool) -> bool { ... }
pub fn env_bool_any(keys: &[&str], default: bool) -> bool { ... }
pub fn env_u64(name: &str, default: u64) -> u64 { ... }
pub fn env_decimal(name: &str, default: Decimal) -> Decimal { ... }
// etc.
```

### 2.3 [Medium] `fetch_orders_paginated` / `fetch_trades_paginated` near-identical

**File**: `src/adapters/polymarket_clob.rs`, lines 635-698

These two methods have identical pagination logic (cursor loop, limit check, terminal cursor detection). Only the SDK method call and return type differ.

**Fix**: Extract a generic paginator:
```rust
async fn paginate<T, F, Fut>(fetch_page: F, limit: Option<usize>) -> Result<Vec<T>>
where
    F: Fn(Option<String>) -> Fut,
    Fut: Future<Output = Result<(Vec<T>, String)>>,
{ ... }
```

### 2.4 [Medium] `database_url_from_env()` pattern

**File**: `src/strategy/adapters.rs`, line 24

This function checks 4 env var names for the database URL. The same pattern exists in `src/config.rs` and `src/coordinator/bootstrap.rs`. Consolidate into the env helpers module.

---

## 3. Clean Code / SOLID Violations

### 3.1 [Critical] Silently discarded position-tracking errors in coordinator

**File**: `src/coordinator/coordinator.rs`, lines 2734 and 4229

```rust
let _ = self
    .positions
    .open_position(...)
    .await;
```

Both occurrences discard the `Result` from `open_position()`. If position tracking fails (e.g., due to a data race or internal error), the coordinator will believe the order executed successfully but have no record of the open position. This creates invisible state drift — the risk gate will undercount exposure, and sell-side reduce-only guards will reject valid exits.

**Fix**: Log and handle the error:
```rust
if let Err(e) = self.positions.open_position(...).await {
    error!(
        %agent_id, %intent_id,
        "failed to track open position after fill: {}; exposure accounting may be stale",
        e
    );
    // Optionally: trigger a position reconciliation
}
```

### 3.2 [High] Inline DDL instead of SQL migrations

**Files**: 9 files contain `CREATE TABLE IF NOT EXISTS` statements

- `src/coordinator/bootstrap.rs` (15 ensure_* functions)
- `src/cli/strategy.rs`
- `src/collector/sync_collector.rs`
- `src/collector/polymarket_orderbook_history.rs`
- `src/platform/persistence_schema.rs`
- `src/agents/sports.rs`
- `src/strategy/pattern_memory/persistence.rs`
- `src/collector/token_targets.rs`
- `src/main_modes/deribit_iv_backfill_mode.rs`

This means:
- Schema changes are scattered and hard to audit
- No migration ordering or rollback capability
- `ensure_schema_repairs()` (bootstrap.rs line 661) is a growing pile of ALTER TABLE patches
- Risk of schema drift between environments

**Fix**: Consolidate all DDL into `migrations/` using sqlx-migrate. The `ensure_*` functions become a single `sqlx::migrate!("./migrations").run(pool).await?` call.

### 3.3 [Medium] `StaggeredArbAdapter` struct has 25+ fields — data clump smell

**File**: `src/strategy/staggered_arb_live.rs`, lines 176-243

The adapter mixes market state, position tracking, order tracking, balance management, and diagnostics counters in a single flat struct. The `new()` constructor is 35 lines of `HashMap::new()` calls.

**Fix**: Group into sub-structs:
```rust
struct MarketState {
    spot_prices: HashMap<String, SpotPrice>,
    binance_l2_obi_5: HashMap<String, Decimal>,
    // ...
}

struct OrderTracker {
    live_orders: HashMap<String, LiveOrderTrack>,
    archived_live_orders: HashMap<String, LiveOrderTrack>,
    pending_leg1_events: HashSet<String>,
    pending_leg2_positions: HashSet<usize>,
}

struct DiagnosticCounters {
    entry_reject_counts: HashMap<String, u64>,
    entry_reject_counts_by_symbol: HashMap<String, HashMap<String, u64>>,
    // ...
}
```

### 3.4 [Medium] `MomentumStrategyAdapter` wraps everything in `Arc<RwLock<>>`

**File**: `src/strategy/adapters.rs`, lines 52-86

Every field is wrapped in `Arc<RwLock<>>`:
```rust
positions: Arc<RwLock<HashMap<String, MomentumPosition>>>,
cex_prices: Arc<RwLock<HashMap<String, CexPriceState>>>,
pm_quotes: Arc<RwLock<HashMap<String, PmQuoteState>>>,
events: Arc<RwLock<HashMap<String, Vec<EventState>>>>,
cooldowns: Arc<RwLock<HashMap<String, DateTime<Utc>>>>,
daily_trades: Arc<RwLock<u32>>,
```

But the `Strategy` trait takes `&mut self`, meaning the adapter already has exclusive access. The `Arc<RwLock<>>` wrappers add unnecessary overhead and lock contention risk.

**Fix**: Remove `Arc<RwLock<>>` wrappers since `&mut self` guarantees exclusive access:
```rust
positions: HashMap<String, MomentumPosition>,
cex_prices: HashMap<String, CexPriceState>,
// ...
```

### 3.5 [Medium] `on_market_update` deeply nested match arms

**File**: `src/strategy/adapters.rs`, lines 1124-1298

The `BinancePrice` arm of `on_market_update` has 7 levels of nesting (match → if → if → match → if → if → if). The `None` arm for `get_entry_price` (lines 1251-1296) is 45 lines of debug logging that obscures the main flow.

**Fix**: Use early returns and extract the debug logging into a helper:
```rust
MarketUpdate::BinancePrice { symbol, price, timestamp } => {
    self.update_cex_price(symbol, *price, *timestamp).await;
    if !self.enabled || self.daily_limit_reached().await || self.in_cooldown(symbol).await {
        return Ok(actions);
    }
    // ... flat structure with early returns
}
```

---

## 4. Technical Debt

### 4.1 [High] 45 `#[allow(dead_code)]` annotations across 19 files

**Files**: See full list in analysis

Key concentrations:
- `src/strategy/adapters.rs` — 8 occurrences (structs and fields)
- `src/strategy/pattern_memory/strategy.rs` — 4 occurrences
- `src/platform/agents/crypto_agent.rs` — 4 occurrences
- `src/agents/crypto_rl_policy.rs` — 4 occurrences
- `src/strategy/gamma_scalping/strategy.rs` — 3 occurrences
- `src/strategy/staggered_arb_live.rs` — 3 occurrences

These indicate either:
- Incomplete implementations (fields declared but never read)
- Abandoned features that should be removed
- Premature abstractions

**Fix**: Audit each `#[allow(dead_code)]`. For each:
- If the field/function is planned for future use, add a `// TODO(username): needed for X` comment
- If it's abandoned, remove it
- If it's used only in tests, gate with `#[cfg(test)]`

### 4.2 [Medium] TODO/FIXME comments indicate unfinished work

**Files**: Multiple

Key TODOs:
- `src/adapters/postgres.rs:882` — `// TODO: use config` (hardcoded 500ms staleness threshold)
- `src/cli/strategy.rs:1873` — `// TODO: Implement actual uptime calculation`
- `src/ai_clients/autonomous.rs:565` — `// TODO: Integrate with RiskManager`
- `src/main_commands/rl/lead_lag.rs:407-440` — 4x `// TODO: Execute real order via PolymarketClient`
- `src/platform/platform.rs:460` — `// TODO: 從 report 獲取` (hardcoded domain)

The `autonomous.rs` TODO is concerning — the autonomous trading module bypasses the RiskManager entirely.

**Fix**: Create issues for each TODO. The `autonomous.rs` risk integration should be prioritized as it's a safety gap.

### 4.3 [Medium] `PriceCache` name collision

**File**: `src/strategy/split_arb.rs`, lines 28-50

`split_arb.rs` defines its own `PriceCache` struct that shadows the adapter-level `PriceCache` from `src/adapters/`. This creates confusion when reading imports and can lead to using the wrong cache type.

**Fix**: Rename to `SplitArbPriceCache` or use the shared adapter cache.

### 4.4 [Low] Deprecated `private_key_hex()` still exists

**File**: `src/signing/wallet.rs`, lines 89-98

The method is marked `#[deprecated]` and returns an empty string, but it still exists in the public API. Any caller would get silent incorrect behavior (empty string instead of a key).

**Fix**: Remove the method entirely. If any callers exist, they'll get a compile error (which is the correct behavior for a security-sensitive removal).

### 4.5 [Low] `config.rs` at 1,168 lines is a flat config monolith

**File**: `src/config.rs`

All config structs (`AppConfig`, `MarketConfig`, `StrategyConfig`, `ExecutionConfig`, `RiskConfig`, `DatabaseConfig`, `DryRunConfig`, `KalshiConfig`, `LoggingConfig`, `AgentFrameworkConfig`, `EventEdgeAgentConfig`, `NbaComebackConfig`, `DiscoveryConfig`) live in a single file with their defaults and validation.

**Fix**: Split into `src/config/` module with one file per config domain.

---

## 5. Error Handling

### 5.1 [Medium] `PloyError::Other(anyhow::Error)` catch-all variant

**File**: `src/error.rs`, line 114

```rust
#[error("{0}")]
Other(#[from] anyhow::Error),
```

This allows any `anyhow::Error` to be silently converted into a `PloyError`, bypassing the typed error hierarchy. Code using `anyhow::anyhow!("...")` instead of specific `PloyError` variants loses error categorization.

**Fix**: Audit usages of `anyhow::anyhow!()` in non-test code and replace with specific `PloyError` variants. Consider removing the `From<anyhow::Error>` impl and using explicit conversions.

### 5.2 [Low] `to_f64().unwrap_or(0.0)` pattern used extensively

**Files**: `src/strategy/staggered_arb_live.rs` (12+ occurrences), `src/strategy/adapters.rs`, `src/platform/risk.rs`

Example from staggered_arb_live.rs line 1324:
```rust
let displacement = ((st - s0) / s0).to_f64().unwrap_or(0.0);
```

While `unwrap_or(0.0)` is safe, silently converting a failed Decimal→f64 to 0.0 can mask precision issues. In a trading context, treating a failed conversion as "no displacement" could cause missed entries or incorrect signals.

**Fix**: For critical calculations, log a warning when the conversion fails:
```rust
let displacement = ((st - s0) / s0).to_f64().unwrap_or_else(|| {
    warn!("Decimal→f64 conversion failed for displacement; defaulting to 0.0");
    0.0
});
```

### 5.3 [Low] `OrderError` and `RiskError` lose type information when converted to `PloyError`

**File**: `src/error.rs`, lines 176-186

```rust
impl From<OrderError> for PloyError {
    fn from(err: OrderError) -> Self {
        PloyError::OrderSubmission(err.to_string())
    }
}
```

The structured `OrderError::SlippageExceeded { limit, actual }` becomes a flat string `"Price slippage exceeded: limit 0.50, actual 0.55"`. Callers cannot pattern-match on the specific error variant after conversion.

**Fix**: Embed the typed error:
```rust
#[error("Order error: {0}")]
Order(#[from] OrderError),

#[error("Risk error: {0}")]
Risk(#[from] RiskError),
```

---

## 6. Positive Observations

- **Wallet security**: Private key zeroization is properly implemented (`src/signing/wallet.rs`). The `PrivateKeySigner` is the only holder of key material, and the deprecated `private_key_hex()` returns empty string.
- **Strategy trait design**: The `Strategy` trait (`src/strategy/traits.rs`) is clean and well-designed with clear separation of market updates, order updates, and periodic ticks.
- **Coordinator main loop**: The `select!` loop in `coordinator.rs` (lines 3077-3135) is well-structured with clear separation of concerns (control commands, order intents, state updates, drain, refresh, shutdown).
- **Error types**: `PloyError` uses `thiserror` consistently with good error messages. The `OrderError` and `RiskError` sub-types provide structured error information.
- **Risk gate**: The `RiskGate` (`src/platform/risk.rs`) has comprehensive multi-layer checks (agent-level, platform-level, domain-level, daily loss, drawdown, circuit breaker).
- **Optimistic locking**: The `StrategyEngine` uses version numbers to prevent concurrent order submissions (`src/strategy/execution/engine.rs`).
- **Gateway-only mode**: The `PolymarketClient` enforces coordinator-routed execution when configured, preventing rogue direct order submissions.

---

## Priority Recommendations

1. **Immediate** (Critical): Fix the two `let _ =` position-tracking discards in `coordinator.rs` — these can cause invisible exposure drift in production.
2. **This sprint** (High): Extract series ID registry into a single module — every new symbol change is currently a 6-file shotgun surgery.
3. **This sprint** (High): Consolidate env helpers into a shared utility module.
4. **Next sprint** (High): Split `bootstrap.rs` into focused modules (schema, env, alerts, bootstrap).
5. **Next sprint** (High): Migrate inline DDL to proper SQL migrations.
6. **Backlog** (Medium): Refactor `StaggeredArbAdapter` entry logic into pure functions for testability.
7. **Backlog** (Medium): Audit and resolve all 45 `#[allow(dead_code)]` annotations.

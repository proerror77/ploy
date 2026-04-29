# ThreeLayerStrategy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement a regime-aware `ThreeLayerStrategy` for 5-minute Polymarket crypto binaries that explicitly separates the trade decision into three layers — Direction, Confirmation, Worth-It — with per-regime weight tuning, based on the 3D research framework in `docs/strategies/binary_option_3d_research_framework.md`.

**Architecture:** New strategy file `three_layer.rs` follows the `ReversalStrategy` pattern: independent `ThreeLayerConfig` struct with `From<DirectionalConfig>` conversion. `DirectionalConfig` gains `three_layer_*` prefixed fields (`#[serde(default)]`). The strategy maintains its own `ReturnBuffer` (drift/volatility), `MicrostructureState` (OBI/trade imbalance), and `MpriceDriftAccumulator` (5-min microprice window). Entry evaluation runs three sequential gates whose thresholds shift by `Regime` (Early/Middle/Late/Expiry). Exit uses time-stop + take-profit + stop-loss, same pattern as `ReversalStrategy`.

**Tech Stack:** Rust, `rust_decimal`, `chrono`, `tracing`, `serde` — no new dependencies.

---

## File Map

| File | Change |
|------|--------|
| `crates/ploy-strategy-bundles/src/strategies/three_layer.rs` | Create: full strategy implementation |
| `crates/ploy-strategy-bundles/src/strategies/directional.rs` | Modify: add `three_layer_*` fields to `DirectionalConfig` (~line 80-180) |
| `crates/ploy-strategy-bundles/src/strategies/mod.rs` | Modify: add `pub mod three_layer;` and re-export |
| `crates/ploy-strategy-bundles/examples/optimize_backtest.rs` | Modify: add `"three_layer"` to `canonical_strategy_variant()` and `build_strategy()` |

---

## Phase 1 — Config Plumbing

### Task 1: Add `three_layer_*` fields to `DirectionalConfig`

**Files:**
- Modify: `crates/ploy-strategy-bundles/src/strategies/directional.rs:80-180`

- [ ] **Step 1: Add `three_layer_*` fields after the existing `reversal_*` block**

In `directional.rs`, find the block ending with `reversal_stop_distance_pct` (around line 155). Add immediately after:

```rust
    // ── Three-Layer strategy parameters ──────────────────────────────

    /// Direction gate: minimum effective probability to consider a trade.
    /// Per-regime defaults: early=0.58, middle=0.56, late=0.54, expiry=0.60.
    /// This is the global floor; regime logic may tighten it.
    #[serde(default = "default_tl_min_direction_prob")]
    pub three_layer_min_direction_prob: f64,

    /// Direction gate: minimum |distance_over_sigma| to consider a trade.
    #[serde(default = "default_tl_min_distance_over_sigma")]
    pub three_layer_min_distance_over_sigma: f64,

    /// Confirmation gate: minimum absolute confirmation score to pass.
    /// Score is a weighted combination of drift, OBI, depth signals.
    #[serde(default = "default_tl_min_confirmation_score")]
    pub three_layer_min_confirmation_score: f64,

    /// Confirmation gate: in late/expiry regimes, require drift_30s
    /// to agree with direction. This is the minimum |drift| threshold.
    #[serde(default = "default_tl_min_drift_confirmation")]
    pub three_layer_min_drift_confirmation: f64,

    /// Worth-it gate: minimum edge after fees.
    #[serde(default = "default_tl_min_edge")]
    pub three_layer_min_edge: f64,

    /// Worth-it gate: minimum reward/risk ratio.
    #[serde(default = "default_tl_min_reward_risk")]
    pub three_layer_min_reward_risk: f64,

    /// Take-profit: exit when token ask reaches this level.
    #[serde(default = "default_tl_take_profit_ask")]
    pub three_layer_take_profit_ask: f64,

    /// Stop-loss: exit when spot moves against direction by this pct.
    #[serde(default = "default_tl_stop_distance_pct")]
    pub three_layer_stop_distance_pct: f64,

    /// Maximum PM quote staleness (seconds) before rejecting entry.
    #[serde(default = "default_tl_max_pm_lag_secs")]
    pub three_layer_max_pm_lag_secs: u64,
```

- [ ] **Step 2: Add default value functions**

Add after the existing `default_reversal_*` functions:

```rust
fn default_tl_min_direction_prob() -> f64 { 0.56 }
fn default_tl_min_distance_over_sigma() -> f64 { 0.3 }
fn default_tl_min_confirmation_score() -> f64 { 0.10 }
fn default_tl_min_drift_confirmation() -> f64 { 0.0002 }
fn default_tl_min_edge() -> f64 { 0.03 }
fn default_tl_min_reward_risk() -> f64 { 1.2 }
fn default_tl_take_profit_ask() -> f64 { 0.70 }
fn default_tl_stop_distance_pct() -> f64 { 0.020 }
fn default_tl_max_pm_lag_secs() -> u64 { 15 }
```

- [ ] **Step 3: Verify build**

```bash
cargo build -p ploy-strategy-bundles 2>&1 | tail -10
```
Expected: compiles without error.

- [ ] **Step 4: Commit**

```bash
git add crates/ploy-strategy-bundles/src/strategies/directional.rs
git commit -m "feat(strategy-bundles): add three_layer_* fields to DirectionalConfig"
```

---

### Task 2: Register `three_layer` module and wire into build_strategy

**Files:**
- Modify: `crates/ploy-strategy-bundles/src/strategies/mod.rs`
- Modify: `crates/ploy-strategy-bundles/examples/optimize_backtest.rs`

- [ ] **Step 1: Add module declaration in `strategies/mod.rs`**

Find the existing module declarations and add:

```rust
pub mod three_layer;
pub use three_layer::ThreeLayerStrategy;
```

- [ ] **Step 2: Add `"three_layer"` to `canonical_strategy_variant()` in `optimize_backtest.rs`**

Find the match block (line ~66-74) and add a new arm before `other =>`:

```rust
        "three_layer" | "3layer" | "pm5d_three_layer" | "pm-5m-three-layer" => {
            "three_layer".to_string()
        }
```

- [ ] **Step 3: Add `"three_layer"` to `build_strategy()` in `optimize_backtest.rs`**

Find the match block (line ~76-82) and add a new arm before `other =>`:

```rust
        "three_layer" => Box::new(ThreeLayerStrategy::new(config.into())),
```

Add the import at the top of the file:

```rust
use ploy_strategy_bundles::ThreeLayerStrategy;
```

Note: This will not compile yet — `ThreeLayerStrategy` doesn't exist. That's expected; Task 3 creates it.

- [ ] **Step 4: Commit (will not build yet — that's fine)**

```bash
git add crates/ploy-strategy-bundles/src/strategies/mod.rs \
       crates/ploy-strategy-bundles/examples/optimize_backtest.rs
git commit -m "chore(strategy-bundles): wire three_layer module into strategy dispatch"
```

---

## Phase 2 — ThreeLayerConfig and From<DirectionalConfig>

### Task 3: Create `ThreeLayerConfig` and conversion

**Files:**
- Create: `crates/ploy-strategy-bundles/src/strategies/three_layer.rs`

- [ ] **Step 1: Write failing test**

Create `three_layer.rs` with the test at the bottom:

```rust
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::collections::{HashMap, VecDeque};
use tracing::{info, warn};

use crate::strategies::directional::DirectionalConfig;
use crate::traits::*;

// ── Config ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Regime {
    Early,   // 181..=300s
    Middle,  // 61..=180s
    Late,    // 6..=60s
    Expiry,  // 0..=5s
}

impl Regime {
    pub fn from_secs(t: i64) -> Self {
        match t {
            181..=300 => Regime::Early,
            61..=180  => Regime::Middle,
            6..=60    => Regime::Late,
            _         => Regime::Expiry,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Regime::Early  => "early",
            Regime::Middle => "middle",
            Regime::Late   => "late",
            Regime::Expiry => "expiry",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ThreeLayerConfig {
    pub symbols: Vec<String>,

    // Direction gate
    pub min_direction_prob: f64,
    pub min_distance_over_sigma: f64,

    // Confirmation gate
    pub min_confirmation_score: f64,
    pub min_drift_confirmation: f64,

    // Worth-it gate
    pub min_edge: f64,
    pub min_reward_risk: f64,

    // Exit
    pub take_profit_ask: f64,
    pub stop_distance_pct: f64,

    // Timing / sizing (shared)
    pub max_pm_lag_secs: u64,
    pub min_time_remaining_secs: u64,
    pub max_time_remaining_secs: u64,
    pub cooldown_secs: u64,
    pub stake_usd: Decimal,
    pub max_positions: usize,
    pub max_daily_trades: u32,
    pub allowed_window_secs: Vec<u64>,

    // Price filter
    pub min_entry_price: f64,
    pub max_entry_price: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regime_from_secs_boundaries() {
        assert_eq!(Regime::from_secs(300), Regime::Early);
        assert_eq!(Regime::from_secs(181), Regime::Early);
        assert_eq!(Regime::from_secs(180), Regime::Middle);
        assert_eq!(Regime::from_secs(61),  Regime::Middle);
        assert_eq!(Regime::from_secs(60),  Regime::Late);
        assert_eq!(Regime::from_secs(6),   Regime::Late);
        assert_eq!(Regime::from_secs(5),   Regime::Expiry);
        assert_eq!(Regime::from_secs(0),   Regime::Expiry);
    }

    #[test]
    fn config_from_directional_preserves_fields() {
        let dc = DirectionalConfig::default();
        let tlc: ThreeLayerConfig = dc.into();
        assert_eq!(tlc.min_edge, 0.03);
        assert_eq!(tlc.min_reward_risk, 1.2);
        assert_eq!(tlc.take_profit_ask, 0.70);
        assert!(!tlc.symbols.is_empty());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo test -p ploy-strategy-bundles three_layer::tests 2>&1 | tail -5
```
Expected: compile error — `From<DirectionalConfig>` not implemented.

- [ ] **Step 3: Add `From<DirectionalConfig>` implementation**

Add after the `ThreeLayerConfig` struct definition:

```rust
impl From<DirectionalConfig> for ThreeLayerConfig {
    fn from(c: DirectionalConfig) -> Self {
        Self {
            symbols: c.symbols,
            min_direction_prob: c.three_layer_min_direction_prob,
            min_distance_over_sigma: c.three_layer_min_distance_over_sigma,
            min_confirmation_score: c.three_layer_min_confirmation_score,
            min_drift_confirmation: c.three_layer_min_drift_confirmation,
            min_edge: c.three_layer_min_edge,
            min_reward_risk: c.three_layer_min_reward_risk,
            take_profit_ask: c.three_layer_take_profit_ask,
            stop_distance_pct: c.three_layer_stop_distance_pct,
            max_pm_lag_secs: c.three_layer_max_pm_lag_secs,
            min_time_remaining_secs: c.min_time_remaining_secs,
            max_time_remaining_secs: c.max_time_remaining_secs,
            cooldown_secs: c.cooldown_secs,
            stake_usd: c.stake_usd,
            max_positions: c.max_positions,
            max_daily_trades: c.max_daily_trades,
            allowed_window_secs: c.allowed_window_secs,
            min_entry_price: c.min_entry_price,
            max_entry_price: c.max_entry_price,
        }
    }
}
```

- [ ] **Step 4: Run to verify tests pass**

```bash
cargo test -p ploy-strategy-bundles three_layer::tests 2>&1 | tail -5
```
Expected: 2 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ploy-strategy-bundles/src/strategies/three_layer.rs
git commit -m "feat(strategy-bundles): add ThreeLayerConfig with From<DirectionalConfig>"
```

---

## Phase 3 — Internal State Types

### Task 4: Add internal state structs to `three_layer.rs`

**Files:**
- Modify: `crates/ploy-strategy-bundles/src/strategies/three_layer.rs`

- [ ] **Step 1: Write failing test for MpriceDriftAccumulator**

Add to the `tests` module:

```rust
    #[test]
    fn mprice_drift_accumulator_evicts_old_entries() {
        use chrono::TimeZone;
        let mut acc = MpriceDriftAccumulator::new(300.0); // 5-min window
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        acc.push(t0, 1.5);
        acc.push(t0 + chrono::Duration::seconds(100), 2.0);
        acc.push(t0 + chrono::Duration::seconds(301), 3.0);
        // t0 entry should be evicted (301s > 300s window)
        assert!((acc.cum_drift() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn drift_tracker_detects_direction() {
        use chrono::TimeZone;
        let mut tracker = DriftTracker::new();
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        // Simulate rising prices over 30 seconds
        for i in 0..30 {
            let t = t0 + chrono::Duration::seconds(i);
            let price = 50000.0 + i as f64 * 10.0;
            tracker.push(t, price);
        }
        assert!(tracker.drift_30s() > 0.0);
    }
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo test -p ploy-strategy-bundles three_layer::tests 2>&1 | tail -5
```
Expected: compile error — types not defined.

- [ ] **Step 3: Implement internal state types**

Add after the `From<DirectionalConfig>` impl, before the `#[cfg(test)]` block:

```rust
// ── Internal State ──────────────────────────────────────────────────

const DRIFT_WINDOW_SECS: f64 = 30.0;
const VOL_WINDOW_SECS: f64 = 120.0;
const MIN_VOL_POINTS: usize = 5;
const TIME_STOP_SECS: i64 = 3;

/// Tracks 30-second price drift and realized volatility.
struct DriftTracker {
    /// (timestamp, ln_price)
    history: VecDeque<(DateTime<Utc>, f64)>,
}

impl DriftTracker {
    fn new() -> Self {
        Self { history: VecDeque::new() }
    }

    fn push(&mut self, ts: DateTime<Utc>, price: f64) {
        if price <= 0.0 { return; }
        self.history.push_back((ts, price.ln()));
        while self.history.len() > 1 {
            let oldest = self.history.front().unwrap().0;
            if (ts - oldest).num_milliseconds() as f64 / 1000.0 > VOL_WINDOW_SECS {
                self.history.pop_front();
            } else {
                break;
            }
        }
    }

    /// Log-return drift over the last 30 seconds.
    fn drift_30s(&self) -> f64 {
        if self.history.len() < 2 { return 0.0; }
        let now = self.history.back().unwrap();
        let cutoff = now.0 - chrono::Duration::seconds(30);
        let anchor = self.history.iter()
            .find(|(ts, _)| *ts >= cutoff)
            .unwrap_or(self.history.front().unwrap());
        now.1 - anchor.1
    }

    /// Annualized realized volatility scaled to `horizon_secs`.
    fn sigma_horizon(&self, horizon_secs: f64) -> f64 {
        if self.history.len() < MIN_VOL_POINTS { return 0.0; }
        let contiguous = self.history.make_contiguous();
        let returns: Vec<f64> = contiguous.windows(2)
            .map(|w| w[1].1 - w[0].1)
            .collect();
        if returns.is_empty() { return 0.0; }
        let mean = returns.iter().sum::<f64>() / returns.len() as f64;
        let var = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>()
            / returns.len() as f64;
        let avg_dt = {
            let total_secs = (self.history.back().unwrap().0
                - self.history.front().unwrap().0)
                .num_milliseconds() as f64 / 1000.0;
            total_secs / returns.len() as f64
        };
        if avg_dt <= 0.0 { return 0.0; }
        let var_per_sec = var / avg_dt;
        (var_per_sec * horizon_secs).sqrt()
    }
}

/// Accumulates microprice offset over a sliding window (default 5 min).
struct MpriceDriftAccumulator {
    entries: VecDeque<(DateTime<Utc>, f64)>,
    window_secs: f64,
}

impl MpriceDriftAccumulator {
    fn new(window_secs: f64) -> Self {
        Self { entries: VecDeque::new(), window_secs }
    }

    fn push(&mut self, ts: DateTime<Utc>, microprice_offset_bps: f64) {
        self.entries.push_back((ts, microprice_offset_bps));
        while self.entries.len() > 1 {
            let oldest = self.entries.front().unwrap().0;
            if (ts - oldest).num_milliseconds() as f64 / 1000.0 > self.window_secs {
                self.entries.pop_front();
            } else {
                break;
            }
        }
    }

    fn cum_drift(&self) -> f64 {
        self.entries.iter().map(|e| e.1).sum()
    }
}

/// LOB microstructure snapshot.
#[derive(Clone, Copy, Default)]
struct LobState {
    obi: f64,
    obi_prev: f64,
    spread_bps: u32,
    bid_depth_near: f64,
    ask_depth_near: f64,
    signed_trade_imbalance: f64,
    last_aggtrade_ts: Option<DateTime<Utc>>,
    ts: Option<DateTime<Utc>>,
}

impl LobState {
    fn depth_imbalance(&self) -> f64 {
        let total = self.bid_depth_near + self.ask_depth_near;
        if total <= 0.0 { return 0.0; }
        (self.bid_depth_near - self.ask_depth_near) / total
    }

    fn obi_delta(&self) -> f64 {
        self.obi - self.obi_prev
    }

    fn apply_l2(&mut self, obi: f64, spread_bps: u32, bid_near: f64, ask_near: f64, ts: DateTime<Utc>) {
        self.obi_prev = self.obi;
        self.obi = obi;
        self.spread_bps = spread_bps;
        self.bid_depth_near = bid_near;
        self.ask_depth_near = ask_near;
        self.ts = Some(ts);
    }

    fn apply_aggtrade(&mut self, quantity: f64, is_buyer_maker: bool, ts: DateTime<Utc>) {
        let seconds = self.last_aggtrade_ts
            .map(|last| (ts - last).num_milliseconds() as f64 / 1000.0)
            .unwrap_or(0.0)
            .max(0.0);
        let decay = if seconds > 0.0 { (-seconds / 30.0).exp() } else { 1.0 };
        let signed_qty = if is_buyer_maker { -quantity } else { quantity };
        self.signed_trade_imbalance = self.signed_trade_imbalance * decay + signed_qty;
        self.last_aggtrade_ts = Some(ts);
    }

    /// Confirmation score: weighted combination of LOB signals.
    /// Positive = bullish pressure, negative = bearish pressure.
    fn confirmation_score(&self) -> f64 {
        let trade_score = (self.signed_trade_imbalance / 50.0).clamp(-1.0, 1.0) * 0.30;
        let obi_score = self.obi.clamp(-1.0, 1.0) * 0.25;
        let obi_delta_score = self.obi_delta().clamp(-1.0, 1.0) * 0.25;
        let depth_score = self.depth_imbalance().clamp(-1.0, 1.0) * 0.20;
        trade_score + obi_score + obi_delta_score + depth_score
    }
}

#[derive(Clone, Copy)]
struct QuoteState {
    bid: Option<Decimal>,
    ask: Option<Decimal>,
    ts: DateTime<Utc>,
}

#[derive(Clone)]
struct EventWindow {
    event_id: String,
    symbol: String,
    up_token: String,
    down_token: String,
    end_time: DateTime<Utc>,
    window_secs: u64,
    price_to_beat: Option<Decimal>,
}

fn crypto_fee_cost(ask: f64) -> f64 {
    0.02 * ask * (1.0 - ask)
}
```

- [ ] **Step 4: Run to verify tests pass**

```bash
cargo test -p ploy-strategy-bundles three_layer::tests 2>&1 | tail -5
```
Expected: 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ploy-strategy-bundles/src/strategies/three_layer.rs
git commit -m "feat(strategy-bundles): add ThreeLayerStrategy internal state types"
```

---

## Phase 4 — Three-Gate Entry Evaluation

### Task 5: Implement `evaluate_entry()` with regime-aware three-layer gates

**Files:**
- Modify: `crates/ploy-strategy-bundles/src/strategies/three_layer.rs`

- [ ] **Step 1: Write failing test for evaluate_entry**

Add to the `tests` module:

```rust
    #[test]
    fn evaluate_entry_rejects_when_direction_too_weak() {
        // distance_over_sigma below threshold → None
        let config = ThreeLayerConfig {
            symbols: vec!["BTCUSDT".into()],
            min_direction_prob: 0.56,
            min_distance_over_sigma: 0.3,
            min_confirmation_score: 0.10,
            min_drift_confirmation: 0.0002,
            min_edge: 0.03,
            min_reward_risk: 1.2,
            take_profit_ask: 0.70,
            stop_distance_pct: 0.020,
            max_pm_lag_secs: 15,
            min_time_remaining_secs: 60,
            max_time_remaining_secs: 300,
            cooldown_secs: 0,
            stake_usd: Decimal::new(25, 0),
            max_positions: 10,
            max_daily_trades: 100,
            allowed_window_secs: vec![300],
            min_entry_price: 0.15,
            max_entry_price: 0.85,
        };
        let result = evaluate_direction(
            0.1,   // distance_over_sigma: too small
            0.02,  // sigma_horizon
            0.0,   // cum_mprice_drift_5m
            0.0,   // drift_30s
            Regime::Early,
            &config,
        );
        assert!(result.is_none(), "should reject weak direction signal");
    }

    #[test]
    fn evaluate_entry_passes_strong_early_signal() {
        let config = ThreeLayerConfig {
            symbols: vec!["BTCUSDT".into()],
            min_direction_prob: 0.56,
            min_distance_over_sigma: 0.3,
            min_confirmation_score: 0.10,
            min_drift_confirmation: 0.0002,
            min_edge: 0.03,
            min_reward_risk: 1.2,
            take_profit_ask: 0.70,
            stop_distance_pct: 0.020,
            max_pm_lag_secs: 15,
            min_time_remaining_secs: 60,
            max_time_remaining_secs: 300,
            cooldown_secs: 0,
            stake_usd: Decimal::new(25, 0),
            max_positions: 10,
            max_daily_trades: 100,
            allowed_window_secs: vec![300],
            min_entry_price: 0.15,
            max_entry_price: 0.85,
        };
        let result = evaluate_direction(
            1.5,   // distance_over_sigma: strong
            0.02,  // sigma_horizon
            0.0,   // cum_mprice_drift_5m
            0.0,   // drift_30s
            Regime::Early,
            &config,
        );
        assert!(result.is_some(), "should pass strong early signal");
        let (dir, prob) = result.unwrap();
        assert!(dir > 0.0);
        assert!(prob > 0.56);
    }
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo test -p ploy-strategy-bundles three_layer::tests 2>&1 | tail -5
```
Expected: compile error — `evaluate_direction` not defined.

- [ ] **Step 3: Implement `evaluate_direction()` — Gate 1**

Add after the internal state types, before `#[cfg(test)]`:

```rust
// ── Gate Functions ──────────────────────────────────────────────────

/// Normal CDF approximation (Abramowitz & Stegun).
fn norm_cdf(x: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.2316419 * x.abs());
    let d = 0.3989422804014327 * (-x * x / 2.0).exp();
    let p = d * t * (0.3193815 + t * (-0.3565638 + t * (1.781478 + t * (-1.821256 + t * 1.330274))));
    if x >= 0.0 { 1.0 - p } else { p }
}

/// Gate 1: Direction.
/// Returns Some((direction_sign, effective_probability)) or None.
///
/// - `distance_over_sigma`: (spot - price_to_beat) / (sigma * price_to_beat)
///   Positive = spot above strike, negative = spot below.
/// - In Early regime: direction driven by distance_over_sigma + model_prob.
/// - In Middle regime: cum_mprice_drift_5m co-drives direction.
/// - In Late/Expiry: drift_30s becomes primary directional signal.
fn evaluate_direction(
    distance_over_sigma: f64,
    sigma_horizon: f64,
    cum_mprice_drift_5m: f64,
    drift_30s: f64,
    regime: Regime,
    config: &ThreeLayerConfig,
) -> Option<(f64, f64)> {
    if distance_over_sigma.abs() < config.min_distance_over_sigma
        && regime == Regime::Early
    {
        return None;
    }

    // Base probability from distance_over_sigma via normal CDF
    let model_prob_up = norm_cdf(distance_over_sigma);

    // Regime-dependent direction score
    let direction_prob = match regime {
        Regime::Early => {
            // Option-state dominates; LOB is filter only
            model_prob_up
        }
        Regime::Middle => {
            // Option-state + LOB mixture
            let lob_nudge = (cum_mprice_drift_5m / 100.0).clamp(-0.08, 0.08);
            (model_prob_up + lob_nudge).clamp(0.01, 0.99)
        }
        Regime::Late => {
            // LOB / drift-led; option-state still contributes
            let drift_nudge = (drift_30s * 500.0).clamp(-0.12, 0.12);
            let lob_nudge = (cum_mprice_drift_5m / 80.0).clamp(-0.06, 0.06);
            (model_prob_up + drift_nudge + lob_nudge).clamp(0.01, 0.99)
        }
        Regime::Expiry => {
            // Almost pure endgame direction
            let drift_nudge = (drift_30s * 800.0).clamp(-0.15, 0.15);
            (model_prob_up + drift_nudge).clamp(0.01, 0.99)
        }
    };

    // Direction: bet on whichever side has higher probability
    let (direction_sign, effective_p) = if direction_prob >= 0.5 {
        (1.0_f64, direction_prob)
    } else {
        (-1.0_f64, 1.0 - direction_prob)
    };

    if effective_p < config.min_direction_prob {
        return None;
    }

    Some((direction_sign, effective_p))
}

/// Gate 2: Confirmation.
/// Returns true if LOB microstructure agrees with the chosen direction.
///
/// - In Early: confirmation is a loose filter (low threshold).
/// - In Middle: confirmation is co-equal with direction.
/// - In Late/Expiry: confirmation is strict; drift must agree.
fn evaluate_confirmation(
    direction_sign: f64,
    lob: &LobState,
    cum_mprice_drift_5m: f64,
    drift_30s: f64,
    regime: Regime,
    config: &ThreeLayerConfig,
) -> bool {
    let raw_score = lob.confirmation_score() + (cum_mprice_drift_5m / 200.0).clamp(-0.15, 0.15);
    let aligned_score = direction_sign * raw_score;

    let threshold = match regime {
        Regime::Early  => config.min_confirmation_score * 0.5,  // loose
        Regime::Middle => config.min_confirmation_score,         // medium
        Regime::Late   => config.min_confirmation_score * 1.5,  // strict
        Regime::Expiry => config.min_confirmation_score * 2.0,  // very strict
    };

    if aligned_score < threshold {
        return false;
    }

    // In Late/Expiry, also require drift to agree with direction
    if matches!(regime, Regime::Late | Regime::Expiry) {
        let drift_agrees = (direction_sign > 0.0 && drift_30s > config.min_drift_confirmation)
            || (direction_sign < 0.0 && drift_30s < -config.min_drift_confirmation);
        if !drift_agrees {
            return false;
        }
    }

    true
}

/// Gate 3: Worth-It.
/// Returns Some((entry_price, edge, reward_risk)) or None.
///
/// Checks:
/// 1. Entry price is within allowed range
/// 2. Edge after fees >= min_edge
/// 3. Reward/risk ratio >= min_reward_risk
fn evaluate_worth_it(
    effective_p: f64,
    ask: f64,
    regime: Regime,
    config: &ThreeLayerConfig,
) -> Option<(f64, f64, f64)> {
    if ask < config.min_entry_price || ask > config.max_entry_price {
        return None;
    }

    let fee = crypto_fee_cost(ask);
    let edge = effective_p - ask - fee;

    let min_edge = match regime {
        Regime::Early  => config.min_edge,
        Regime::Middle => config.min_edge,
        Regime::Late   => config.min_edge * 1.2,   // stricter late
        Regime::Expiry => config.min_edge * 1.5,    // very strict expiry
    };

    if edge < min_edge {
        return None;
    }

    // reward/risk: potential payout vs potential loss
    let reward = 1.0 - ask - fee;  // win payout
    let risk = ask + fee;           // loss
    let rr = if risk > 0.0 { reward / risk } else { 0.0 };

    if rr < config.min_reward_risk {
        return None;
    }

    Some((ask, edge, rr))
}
```

- [ ] **Step 4: Run to verify tests pass**

```bash
cargo test -p ploy-strategy-bundles three_layer::tests 2>&1 | tail -5
```
Expected: 6 tests pass.

- [ ] **Step 5: Add confirmation gate test**

Add to the `tests` module:

```rust
    #[test]
    fn confirmation_rejects_opposing_lob_in_late_regime() {
        let config = ThreeLayerConfig {
            symbols: vec!["BTCUSDT".into()],
            min_direction_prob: 0.56,
            min_distance_over_sigma: 0.3,
            min_confirmation_score: 0.10,
            min_drift_confirmation: 0.0002,
            min_edge: 0.03,
            min_reward_risk: 1.2,
            take_profit_ask: 0.70,
            stop_distance_pct: 0.020,
            max_pm_lag_secs: 15,
            min_time_remaining_secs: 60,
            max_time_remaining_secs: 300,
            cooldown_secs: 0,
            stake_usd: Decimal::new(25, 0),
            max_positions: 10,
            max_daily_trades: 100,
            allowed_window_secs: vec![300],
            min_entry_price: 0.15,
            max_entry_price: 0.85,
        };
        let lob = LobState {
            obi: -0.5,  // bearish OBI
            obi_prev: -0.3,
            spread_bps: 10,
            bid_depth_near: 50.0,
            ask_depth_near: 100.0,  // bearish depth
            signed_trade_imbalance: -20.0,  // bearish trades
            last_aggtrade_ts: None,
            ts: None,
        };
        // Direction is UP (+1.0) but LOB is bearish → should reject in Late
        let pass = evaluate_confirmation(
            1.0, &lob, -5.0, -0.001, Regime::Late, &config,
        );
        assert!(!pass, "should reject opposing LOB in late regime");
    }

    #[test]
    fn worth_it_rejects_low_edge() {
        let config = ThreeLayerConfig {
            symbols: vec!["BTCUSDT".into()],
            min_direction_prob: 0.56,
            min_distance_over_sigma: 0.3,
            min_confirmation_score: 0.10,
            min_drift_confirmation: 0.0002,
            min_edge: 0.03,
            min_reward_risk: 1.2,
            take_profit_ask: 0.70,
            stop_distance_pct: 0.020,
            max_pm_lag_secs: 15,
            min_time_remaining_secs: 60,
            max_time_remaining_secs: 300,
            cooldown_secs: 0,
            stake_usd: Decimal::new(25, 0),
            max_positions: 10,
            max_daily_trades: 100,
            allowed_window_secs: vec![300],
            min_entry_price: 0.15,
            max_entry_price: 0.85,
        };
        // effective_p=0.55, ask=0.50 → edge ≈ 0.55 - 0.50 - fee ≈ 0.04
        let result = evaluate_worth_it(0.55, 0.50, Regime::Early, &config);
        assert!(result.is_some());
        // effective_p=0.52, ask=0.50 → edge ≈ 0.52 - 0.50 - fee ≈ 0.01 < 0.03
        let result = evaluate_worth_it(0.52, 0.50, Regime::Early, &config);
        assert!(result.is_none(), "should reject low edge");
    }
```

- [ ] **Step 6: Run to verify all tests pass**

```bash
cargo test -p ploy-strategy-bundles three_layer::tests 2>&1 | tail -10
```
Expected: 8 tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/ploy-strategy-bundles/src/strategies/three_layer.rs
git commit -m "feat(strategy-bundles): add three-gate entry evaluation (direction, confirmation, worth-it)"
```

---

## Phase 5 — ThreeLayerStrategy Struct and StrategyLogic Impl

### Task 6: Implement `ThreeLayerStrategy` struct and `on_update()`

**Files:**
- Modify: `crates/ploy-strategy-bundles/src/strategies/three_layer.rs`

- [ ] **Step 1: Implement `ThreeLayerStrategy` struct**

Add after the gate functions, before `#[cfg(test)]`:

```rust
// ── Strategy ────────────────────────────────────────────────────────

pub struct ThreeLayerStrategy {
    config: ThreeLayerConfig,
    // Per-symbol state
    drift: HashMap<String, DriftTracker>,
    lob: HashMap<String, LobState>,
    mprice_acc: HashMap<String, MpriceDriftAccumulator>,
    spot: HashMap<String, Decimal>,
    // Quote / event state
    quotes: HashMap<String, QuoteState>,
    token_symbol: HashMap<String, String>,
    token_event: HashMap<String, String>,
    events: HashMap<String, Vec<EventWindow>>,
    // Cooldown / counters
    last_entry: HashMap<String, DateTime<Utc>>,
    daily_trade_count: u32,
    daily_trade_date: Option<chrono::NaiveDate>,
    feed_time: Option<DateTime<Utc>>,
}

impl ThreeLayerStrategy {
    pub fn new(config: ThreeLayerConfig) -> Self {
        Self {
            config,
            drift: HashMap::new(),
            lob: HashMap::new(),
            mprice_acc: HashMap::new(),
            spot: HashMap::new(),
            quotes: HashMap::new(),
            token_symbol: HashMap::new(),
            token_event: HashMap::new(),
            events: HashMap::new(),
            last_entry: HashMap::new(),
            daily_trade_count: 0,
            daily_trade_date: None,
            feed_time: None,
        }
    }

    fn now(&self) -> DateTime<Utc> {
        self.feed_time.unwrap_or_else(Utc::now)
    }

    fn reset_daily_counter(&mut self, ts: DateTime<Utc>) {
        let date = ts.date_naive();
        if self.daily_trade_date != Some(date) {
            self.daily_trade_date = Some(date);
            self.daily_trade_count = 0;
        }
    }

    fn window_allowed(&self, window_secs: u64) -> bool {
        self.config.allowed_window_secs.is_empty()
            || self.config.allowed_window_secs.contains(&window_secs)
    }

    fn candidate_events(&self, symbol: &str, now: DateTime<Utc>) -> Vec<EventWindow> {
        self.events.get(symbol).into_iter().flatten()
            .filter(|e| {
                let remaining = (e.end_time - now).num_seconds();
                remaining >= self.config.min_time_remaining_secs as i64
                    && remaining <= self.config.max_time_remaining_secs as i64
            })
            .cloned()
            .collect()
    }

    fn entry_quantity(&self, entry_price: Decimal) -> Decimal {
        if entry_price <= Decimal::ZERO { return Decimal::ZERO; }
        (self.config.stake_usd / entry_price)
            .round_dp(2)
            .max(Decimal::new(1, 0))
    }

    fn try_entry(
        &mut self,
        symbol: &str,
        now: DateTime<Utc>,
        positions: &PositionLedger,
        orders: &OrderLedger,
    ) -> Vec<StrategyDecision> {
        if self.daily_trade_count >= self.config.max_daily_trades {
            return Vec::new();
        }
        if positions.positions().count() >= self.config.max_positions {
            return Vec::new();
        }

        let spot_price = match self.spot.get(symbol).and_then(|d| d.to_f64()) {
            Some(p) if p > 0.0 => p,
            _ => return Vec::new(),
        };

        let candidates = self.candidate_events(symbol, now);

        for event in &candidates {
            let price_to_beat = match event.price_to_beat.and_then(|d| d.to_f64()) {
                Some(p) if p > 0.0 => p,
                _ => continue,
            };

            let time_remaining = (event.end_time - now).num_seconds();
            let regime = Regime::from_secs(time_remaining);

            // Compute direction inputs
            let drift_tracker = match self.drift.get(symbol) {
                Some(d) => d,
                None => continue,
            };
            let sigma_h = drift_tracker.sigma_horizon(time_remaining as f64);
            if sigma_h <= 0.0 { continue; }
            let distance_over_sigma = (spot_price - price_to_beat) / (sigma_h * price_to_beat);
            let drift_30s = drift_tracker.drift_30s();
            let cum_mprice = self.mprice_acc.get(symbol)
                .map(|a| a.cum_drift()).unwrap_or(0.0);

            // ── Gate 1: Direction ──
            let (direction_sign, effective_p) = match evaluate_direction(
                distance_over_sigma, sigma_h, cum_mprice, drift_30s, regime, &self.config,
            ) {
                Some(v) => v,
                None => continue,
            };

            // Determine token
            let betting_up = direction_sign > 0.0;
            let token_id = if betting_up { &event.up_token } else { &event.down_token };

            // Check existing position / order
            if positions.net_qty(token_id) > Decimal::ZERO { continue; }
            if orders.iter().any(|o| o.token_id == *token_id && o.is_open()) { continue; }

            // Get quote
            let quote = match self.quotes.get(token_id) {
                Some(q) => q,
                None => continue,
            };
            let ask = match quote.ask.and_then(|d| d.to_f64()) {
                Some(a) if a > 0.0 => a,
                _ => continue,
            };
            // Quote freshness
            if (now - quote.ts).num_seconds() > self.config.max_pm_lag_secs as i64 {
                continue;
            }

            // ── Gate 2: Confirmation ──
            let lob = self.lob.get(symbol).copied().unwrap_or_default();
            if !evaluate_confirmation(
                direction_sign, &lob, cum_mprice, drift_30s, regime, &self.config,
            ) {
                continue;
            }

            // ── Gate 3: Worth-It ──
            let (entry_price_f, edge, rr) = match evaluate_worth_it(
                effective_p, ask, regime, &self.config,
            ) {
                Some(v) => v,
                None => continue,
            };

            // Build intent
            let entry_price = Decimal::try_from(entry_price_f).unwrap_or(Decimal::ZERO);
            let quantity = self.entry_quantity(entry_price);
            if quantity <= Decimal::ZERO { continue; }

            let direction_str = if betting_up { "UP" } else { "DOWN" };
            let intent_id = format!(
                "tl_{}_{}_{}",
                event.event_id, direction_str, now.timestamp_millis()
            );

            info!(
                strategy = "three_layer",
                event_id = %event.event_id,
                symbol = %symbol,
                regime = regime.as_str(),
                direction = direction_str,
                effective_p = format!("{:.4}", effective_p),
                edge = format!("{:.4}", edge),
                rr = format!("{:.2}", rr),
                ask = format!("{:.4}", ask),
                "three_layer entry signal"
            );

            let intent = ploy_trading::TradingIntent {
                intent_id: intent_id.clone(),
                deployment_id: String::new(),
                market_id: event.event_id.clone(),
                token_id: token_id.clone(),
                side: ploy_trading::TradeSide::Buy,
                quantity,
                limit_price: Some(entry_price),
                purpose: ploy_trading::IntentPurpose::Entry,
                created_at: now,
            };

            let signal = SignalRecord {
                strategy: "three_layer".into(),
                event_id: Some(event.event_id.clone()),
                token_id: Some(token_id.clone()),
                intent_id: Some(intent_id),
                symbol: symbol.into(),
                direction: direction_str.into(),
                p_hat: effective_p,
                edge,
                entry_price,
                decision: format!("enter:{}:{}", regime.as_str(), direction_str),
                ts: now,
            };

            self.last_entry.insert(symbol.to_string(), now);
            self.daily_trade_count += 1;

            return vec![StrategyDecision::Enter {
                intent,
                signal: Some(signal),
            }];
        }

        Vec::new()
    }

    fn exit_decisions_for_symbol(
        &self,
        symbol: &str,
        now: DateTime<Utc>,
        spot: Option<f64>,
        positions: &PositionLedger,
    ) -> Vec<StrategyDecision> {
        let mut decisions = Vec::new();

        for event in self.events.get(symbol).into_iter().flatten() {
            let time_remaining = (event.end_time - now).num_seconds();

            for (token_id, is_up) in [(&event.up_token, true), (&event.down_token, false)] {
                let qty = positions.net_qty(token_id);
                if qty <= Decimal::ZERO { continue; }

                // Time stop
                if time_remaining < TIME_STOP_SECS {
                    decisions.push(StrategyDecision::Exit(ploy_trading::TradingIntent {
                        intent_id: format!("tl_time_exit_{}_{}", token_id, now.timestamp_millis()),
                        deployment_id: String::new(),
                        market_id: event.event_id.clone(),
                        token_id: token_id.clone(),
                        side: ploy_trading::TradeSide::Sell,
                        quantity: qty,
                        limit_price: None,
                        purpose: ploy_trading::IntentPurpose::Exit,
                        created_at: now,
                    }));
                    continue;
                }

                // Take profit
                if let Some(quote) = self.quotes.get(token_id) {
                    if let Some(ask) = quote.ask.and_then(|v| v.to_f64()) {
                        if ask >= self.config.take_profit_ask {
                            decisions.push(StrategyDecision::Exit(ploy_trading::TradingIntent {
                                intent_id: format!("tl_tp_{}_{}", token_id, now.timestamp_millis()),
                                deployment_id: String::new(),
                                market_id: event.event_id.clone(),
                                token_id: token_id.clone(),
                                side: ploy_trading::TradeSide::Sell,
                                quantity: qty,
                                limit_price: quote.bid.or(quote.ask),
                                purpose: ploy_trading::IntentPurpose::Exit,
                                created_at: now,
                            }));
                            continue;
                        }
                    }
                }

                // Stop loss
                if let (Some(ptb), Some(sp)) = (
                    event.price_to_beat.and_then(|v| v.to_f64()),
                    spot,
                ) {
                    let dist = (sp - ptb) / ptb;
                    let wrong = if is_up {
                        dist < -self.config.stop_distance_pct
                    } else {
                        dist > self.config.stop_distance_pct
                    };
                    if wrong {
                        decisions.push(StrategyDecision::Exit(ploy_trading::TradingIntent {
                            intent_id: format!("tl_sl_{}_{}", token_id, now.timestamp_millis()),
                            deployment_id: String::new(),
                            market_id: event.event_id.clone(),
                            token_id: token_id.clone(),
                            side: ploy_trading::TradeSide::Sell,
                            quantity: qty,
                            limit_price: None,
                            purpose: ploy_trading::IntentPurpose::Exit,
                            created_at: now,
                        }));
                    }
                }
            }
        }

        decisions
    }

    fn build_settlement_exits(
        &self,
        event: &EventWindow,
        up_won: bool,
        now: DateTime<Utc>,
        positions: &PositionLedger,
    ) -> Vec<StrategyDecision> {
        let mut exits = Vec::new();
        for (token_id, settles_at) in [
            (&event.up_token, if up_won { Decimal::ONE } else { Decimal::ZERO }),
            (&event.down_token, if up_won { Decimal::ZERO } else { Decimal::ONE }),
        ] {
            let qty = positions.net_qty(token_id);
            if qty > Decimal::ZERO {
                exits.push(StrategyDecision::Exit(ploy_trading::TradingIntent {
                    intent_id: format!("tl_settle_{}_{}", token_id, event.event_id),
                    deployment_id: String::new(),
                    market_id: event.event_id.clone(),
                    token_id: token_id.clone(),
                    side: ploy_trading::TradeSide::Sell,
                    quantity: qty,
                    limit_price: Some(settles_at),
                    purpose: ploy_trading::IntentPurpose::Exit,
                    created_at: now,
                }));
            }
        }
        exits
    }

    fn resolve_up_won(&self, event: &EventWindow, resolved: Option<bool>) -> Option<bool> {
        if let Some(up_won) = resolved {
            return Some(up_won);
        }
        let spot = self.spot.get(&event.symbol)?.to_f64()?;
        let ptb = event.price_to_beat?.to_f64()?;
        Some(spot >= ptb)
    }
}
```

- [ ] **Step 2: Implement `StrategyLogic` trait**

Add after the `ThreeLayerStrategy` impl block:

```rust
impl StrategyLogic for ThreeLayerStrategy {
    fn on_update(
        &mut self,
        update: &MarketUpdate,
        positions: &PositionLedger,
        orders: &OrderLedger,
    ) -> Vec<StrategyDecision> {
        match update {
            MarketUpdate::SpotPrice { symbol, price, ts } => {
                if !self.config.symbols.iter().any(|s| s == symbol) {
                    return Vec::new();
                }
                self.feed_time = Some(*ts);
                self.reset_daily_counter(*ts);

                let price_f = match price.to_f64() {
                    Some(p) if p > 0.0 => p,
                    _ => return Vec::new(),
                };
                self.drift.entry(symbol.clone()).or_insert_with(DriftTracker::new)
                    .push(*ts, price_f);
                self.spot.insert(symbol.clone(), *price);

                // Set price_to_beat for events that don't have one yet
                if let Some(events) = self.events.get_mut(symbol) {
                    for event in events.iter_mut() {
                        if event.price_to_beat.is_none() {
                            event.price_to_beat = Some(*price);
                        }
                    }
                }

                let spot = price.to_f64();
                let exits = self.exit_decisions_for_symbol(symbol, *ts, spot, positions);
                if !exits.is_empty() { return exits; }

                // Cooldown check
                if let Some(last) = self.last_entry.get(symbol) {
                    if (*ts - *last).num_seconds() < self.config.cooldown_secs as i64 {
                        return Vec::new();
                    }
                }

                self.try_entry(symbol, *ts, positions, orders)
            }

            MarketUpdate::Quote { token_id, bid, ask, ts, .. } => {
                self.quotes.insert(token_id.clone(), QuoteState {
                    bid: *bid, ask: *ask, ts: *ts,
                });
                let Some(symbol) = self.token_symbol.get(token_id).cloned() else {
                    return Vec::new();
                };
                let spot = self.spot.get(&symbol).and_then(|d| d.to_f64());
                self.exit_decisions_for_symbol(&symbol, *ts, spot, positions)
            }

            MarketUpdate::AggTrade { symbol, quantity, is_buyer_maker, ts, .. } => {
                if !self.config.symbols.iter().any(|s| s == symbol) {
                    return Vec::new();
                }
                self.feed_time = Some(*ts);
                let qty = quantity.to_f64().unwrap_or(0.0);
                self.lob.entry(symbol.clone()).or_default()
                    .apply_aggtrade(qty, *is_buyer_maker, *ts);
                Vec::new()
            }

            MarketUpdate::L2Depth { symbol, obi, spread_bps, bid_depth_near, ask_depth_near, ts } => {
                if !self.config.symbols.iter().any(|s| s == symbol) {
                    return Vec::new();
                }
                self.feed_time = Some(*ts);
                self.lob.entry(symbol.clone()).or_default()
                    .apply_l2(*obi, *spread_bps, *bid_depth_near, *ask_depth_near, *ts);

                // Feed microprice offset into accumulator
                let mid = (*bid_depth_near + *ask_depth_near) / 2.0;
                if mid > 0.0 {
                    let microprice_offset = (*bid_depth_near - *ask_depth_near) / mid;
                    self.mprice_acc.entry(symbol.clone())
                        .or_insert_with(|| MpriceDriftAccumulator::new(300.0))
                        .push(*ts, microprice_offset);
                }
                Vec::new()
            }

            MarketUpdate::L2 { symbol, obi, spread_bps, ts } => {
                if !self.config.symbols.iter().any(|s| s == symbol) {
                    return Vec::new();
                }
                let state = self.lob.entry(symbol.clone()).or_default();
                state.obi_prev = state.obi;
                state.obi = *obi;
                state.spread_bps = *spread_bps;
                state.ts = Some(*ts);
                Vec::new()
            }

            MarketUpdate::EventDiscovered {
                event_id, symbol, up_token, down_token,
                end_time, window_secs, price_to_beat, ..
            } => {
                if !self.config.symbols.iter().any(|s| s == symbol)
                    || !self.window_allowed(*window_secs)
                {
                    return Vec::new();
                }
                self.token_symbol.insert(up_token.clone(), symbol.clone());
                self.token_symbol.insert(down_token.clone(), symbol.clone());
                self.token_event.insert(up_token.clone(), event_id.clone());
                self.token_event.insert(down_token.clone(), event_id.clone());
                self.events.entry(symbol.clone()).or_default().push(EventWindow {
                    event_id: event_id.clone(),
                    symbol: symbol.clone(),
                    up_token: up_token.clone(),
                    down_token: down_token.clone(),
                    end_time: *end_time,
                    window_secs: *window_secs,
                    price_to_beat: *price_to_beat,
                });
                Vec::new()
            }

            MarketUpdate::EventExpired { event_id, end_time, resolved_up_won } => {
                let mut decisions = Vec::new();
                let mut resolved = Vec::new();
                for events in self.events.values() {
                    for event in events {
                        if event.event_id != *event_id { continue; }
                        if let Some(up_won) = self.resolve_up_won(event, *resolved_up_won) {
                            decisions.extend(
                                self.build_settlement_exits(event, up_won, *end_time, positions)
                            );
                            resolved.push(event.event_id.clone());
                        }
                    }
                }
                for events in self.events.values_mut() {
                    events.retain(|e| !resolved.contains(&e.event_id));
                }
                decisions
            }

            _ => Vec::new(),
        }
    }

    fn on_fill(&mut self, _fill: &FillRecord) {}

    fn on_reject(&mut self, _intent: &ploy_trading::TradingIntent, _reason: &str) {}

    fn name(&self) -> &str { "three_layer" }
}
```

- [ ] **Step 3: Verify build**

```bash
cargo build -p ploy-strategy-bundles 2>&1 | tail -10
```
Expected: compiles without error.

- [ ] **Step 4: Commit**

```bash
git add crates/ploy-strategy-bundles/src/strategies/three_layer.rs
git commit -m "feat(strategy-bundles): implement ThreeLayerStrategy with StrategyLogic trait"
```

---

## Phase 6 — Integration and Smoke Test

### Task 7: Full build and test suite

**Files:**
- No new files — verification only.

- [ ] **Step 1: Run full crate test suite**

```bash
cargo test -p ploy-strategy-bundles 2>&1 | tail -20
```
Expected: all tests pass, no regressions.

- [ ] **Step 2: Run workspace build**

```bash
cargo build --workspace 2>&1 | tail -10
```
Expected: compiles without error.

- [ ] **Step 3: Verify strategy dispatch works**

```bash
grep -n "three_layer" crates/ploy-strategy-bundles/examples/optimize_backtest.rs
grep -n "three_layer" crates/ploy-strategy-bundles/src/strategies/mod.rs
```
Expected: both files contain the `three_layer` variant.

- [ ] **Step 4: Commit (if any fixups needed)**

```bash
git add -A && git diff --cached --stat
# Only commit if there are changes
git commit -m "fix(strategy-bundles): fixups from integration testing"
```

---

## Self-Review Checklist

- [x] **Spec coverage:** All three layers from `binary_option_3d_research_framework.md` implemented:
  - Direction gate: `distance_over_sigma` + `model_prob_up` (via `norm_cdf`) + regime-dependent LOB nudges
  - Confirmation gate: `obi` + `depth_imbalance` + `signed_trade_imbalance` + `cum_mprice_drift_5m` + `drift_30s` agreement in late/expiry
  - Worth-it gate: edge after fees + reward/risk ratio
- [x] **Regime table:** Early (option-state dominates), Middle (mixture), Late (LOB-led), Expiry (strict) — matches research framework exactly
- [x] **No-trade conditions:** Direction variables disagree → Gate 1 rejects; LOB mixed in late → Gate 2 rejects; edge ≤ 0 → Gate 3 rejects; reward/risk poor → Gate 3 rejects
- [x] **Placeholder scan:** No TBD/TODO. All code is complete and compilable.
- [x] **Type consistency:** `ThreeLayerConfig` defined in Task 3, used throughout. `Regime` defined in Task 3, used in gates. `LobState` defined in Task 4, used in Gate 2. `evaluate_direction/confirmation/worth_it` defined in Task 5, called in Task 6.
- [x] **Pattern match:** Follows `ReversalStrategy` pattern exactly — `ThreeLayerConfig` + `From<DirectionalConfig>` + same `on_update` dispatch structure + same exit logic (time-stop, take-profit, stop-loss, settlement).
- [x] **No new dependencies:** Uses only existing crate deps (`rust_decimal`, `chrono`, `tracing`, `serde`).
- [x] **`cum_mprice_drift_5m`:** Computed via `MpriceDriftAccumulator` fed from `L2Depth` updates, matching `LobFlowAccumulator` pattern from `ploy-research/src/factors.rs`.

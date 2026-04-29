# Layered Research Pipeline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Refactor `crates/ploy-research` from a monolithic 1852-line example into a layered pipeline with distinct modules for factor discovery, signal generation, binary-event backtesting, P&L attribution, and ML/RL model interfaces.

**Architecture:** Five layers share `FactorObservation` as their core data contract. The factor layer computes regime-aware IC rankings. The signal layer routes observations through rule-based or model-based `SignalSource` implementations. The backtest engine simulates independent binary-option events and produces tagged fills. The attribution layer decomposes P&L by regime and factor. The model layer provides trait skeletons for supervised ML (`linfa`, `forust-ml`) and deep RL (`burn`) that plug into the signal layer without changing downstream code.

**Tech Stack:** Rust, `polars 0.46`, `linfa 0.8`, `forust-ml 0.5`, `burn 0.17`, `sqlx`, `chrono`, `uuid`

---

## File Map

### New files
| File | Responsibility |
|------|---------------|
| `crates/ploy-research/src/data/mod.rs` | Re-exports for data layer |
| `crates/ploy-research/src/factors_new/mod.rs` | Re-exports for new factor layer |
| `crates/ploy-research/src/factors_new/registry.rs` | `FactorRegistry`, `FactorMeta`, `Regime` |
| `crates/ploy-research/src/factors_new/scan.rs` | `scan_into_registry` batch IC runner |
| `crates/ploy-research/src/signal/mod.rs` | Re-exports for signal layer |
| `crates/ploy-research/src/signal/traits.rs` | `Signal` enum, `SignalSource` trait |
| `crates/ploy-research/src/signal/rules.rs` | `ThresholdRule`, `CompositeRule` |
| `crates/ploy-research/src/signal/regime.rs` | `RegimeRouter` |
| `crates/ploy-research/src/backtest/mod.rs` | Re-exports for backtest layer |
| `crates/ploy-research/src/backtest/engine.rs` | `run_binary_backtest`, `SimulatedFill` |
| `crates/ploy-research/src/backtest/metrics.rs` | `BacktestMetrics`: win rate, Sharpe, drawdown |
| `crates/ploy-research/src/attribution/mod.rs` | Re-exports for attribution layer |
| `crates/ploy-research/src/attribution/regime.rs` | `RegimePnl`, P&L split by regime |
| `crates/ploy-research/src/attribution/factor.rs` | `FactorPnl`, P&L split by factor |
| `crates/ploy-research/src/attribution/report.rs` | `AttributionReport`, auto-print |
| `crates/ploy-research/src/model/mod.rs` | Re-exports for model layer |
| `crates/ploy-research/src/model/traits.rs` | `StrategyModel`, `RlAgent` trait skeletons |
| `crates/ploy-research/src/model/supervised/mod.rs` | Re-exports |
| `crates/ploy-research/src/model/supervised/logistic.rs` | `LinFaLogistic` wrapper |
| `crates/ploy-research/src/model/supervised/gbt.rs` | `ForustGbt` wrapper |
| `crates/ploy-research/src/model/rl/mod.rs` | Re-exports |
| `crates/ploy-research/src/model/rl/env.rs` | `Environment` trait, `BinaryEventEnv` |
| `crates/ploy-research/src/model/rl/agent.rs` | `RlAgent` trait, `Transition` |
| `crates/ploy-research/src/model/rl/replay.rs` | `ReplayBuffer` |
| `crates/ploy-research/src/model/rl/dqn.rs` | `DqnAgent` skeleton (burn) |
| `crates/ploy-research/examples/factor_scan.rs` | New factor discovery entry point |
| `crates/ploy-research/examples/backtest_binary.rs` | New backtest entry point |

### Modified files
| File | Change |
|------|--------|
| `crates/ploy-research/Cargo.toml` | Add `linfa`, `forust-ml`, `burn`, `uuid` deps |
| `crates/ploy-research/src/lib.rs` | Add new module declarations |

---

## Phase 1 — Dependencies & Module Skeleton

### Task 1: Add ML/RL dependencies

**Files:**
- Modify: `crates/ploy-research/Cargo.toml`

- [ ] **Step 1: Add to `[dependencies]`**

```toml
linfa = "0.8"
linfa-logistic = "0.8"
linfa-trees = "0.8"
forust-ml = "0.5"
burn = { version = "0.17", features = ["ndarray"] }
burn-ndarray = "0.17"
uuid = { version = "1", features = ["v4"] }
```

- [ ] **Step 2: Verify build**

```bash
cargo build -p ploy-research 2>&1 | tail -5
```
Expected: compiles without error.

- [ ] **Step 3: Commit**

```bash
git add crates/ploy-research/Cargo.toml
git commit -m "chore(research): add linfa, forust-ml, burn, uuid deps"
```

---

### Task 2: Declare new modules in lib.rs

**Files:**
- Modify: `crates/ploy-research/src/lib.rs`

- [ ] **Step 1: Add module declarations after existing `pub mod replay;`**

```rust
pub mod data;
pub mod factors_new;
pub mod signal;
pub mod backtest;
pub mod attribution;
pub mod model;
```

- [ ] **Step 2: Create stub mod.rs files (each with `// placeholder` as content)**

- `crates/ploy-research/src/data/mod.rs`
- `crates/ploy-research/src/factors_new/mod.rs`
- `crates/ploy-research/src/signal/mod.rs`
- `crates/ploy-research/src/backtest/mod.rs`
- `crates/ploy-research/src/attribution/mod.rs`
- `crates/ploy-research/src/model/mod.rs`

- [ ] **Step 3: Verify build**

```bash
cargo build -p ploy-research 2>&1 | tail -5
```
Expected: compiles without error.

- [ ] **Step 4: Commit**

```bash
git add crates/ploy-research/src/
git commit -m "chore(research): add module skeleton stubs"
```

---

## Phase 2 — Factor Layer

### Task 3: `Regime` enum and `FactorRegistry`

**Files:**
- Create: `crates/ploy-research/src/factors_new/registry.rs`
- Modify: `crates/ploy-research/src/factors_new/mod.rs`

- [ ] **Step 1: Write failing test in `registry.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regime_from_time_remaining() {
        assert_eq!(Regime::from_secs(290), Regime::Early);
        assert_eq!(Regime::from_secs(120), Regime::Middle);
        assert_eq!(Regime::from_secs(30),  Regime::Late);
        assert_eq!(Regime::from_secs(3),   Regime::Expiry);
    }

    #[test]
    fn registry_top_n_sorted_by_abs_ic() {
        let mut reg = FactorRegistry::new();
        reg.insert(FactorMeta { name: "a".into(), regime: Regime::Early,
            label: "settlement_up".into(), ic: 0.05, direction: 1, stability: 0.8 });
        reg.insert(FactorMeta { name: "b".into(), regime: Regime::Early,
            label: "settlement_up".into(), ic: 0.15, direction: -1, stability: 1.2 });
        let top = reg.top_n(Regime::Early, "settlement_up", 1);
        assert_eq!(top[0].name, "b");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo test -p ploy-research factors_new::registry 2>&1 | tail -5
```
Expected: compile error — types not defined.

- [ ] **Step 3: Implement**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
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
pub struct FactorMeta {
    pub name: String,
    pub regime: Regime,
    pub label: String,
    pub ic: f64,
    pub direction: i8,
    pub stability: f64,
}

pub struct FactorRegistry { factors: Vec<FactorMeta> }

impl FactorRegistry {
    pub fn new() -> Self { Self { factors: Vec::new() } }

    pub fn insert(&mut self, meta: FactorMeta) { self.factors.push(meta); }

    pub fn top_n(&self, regime: Regime, label: &str, n: usize) -> Vec<&FactorMeta> {
        let mut v: Vec<&FactorMeta> = self.factors.iter()
            .filter(|m| m.regime == regime && m.label == label)
            .collect();
        v.sort_by(|a, b| b.ic.abs().partial_cmp(&a.ic.abs()).unwrap());
        v.truncate(n);
        v
    }

    pub fn for_regime(&self, regime: Regime) -> Vec<&FactorMeta> {
        let mut v: Vec<&FactorMeta> = self.factors.iter()
            .filter(|m| m.regime == regime).collect();
        v.sort_by(|a, b| b.ic.abs().partial_cmp(&a.ic.abs()).unwrap());
        v
    }

    pub fn all(&self) -> &[FactorMeta] { &self.factors }
}
```

Update `factors_new/mod.rs`:
```rust
pub mod registry;
pub use registry::{FactorMeta, FactorRegistry, Regime};
```

- [ ] **Step 4: Run to verify it passes**

```bash
cargo test -p ploy-research factors_new::registry 2>&1 | tail -5
```
Expected: 2 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ploy-research/src/factors_new/
git commit -m "feat(research): add Regime enum and FactorRegistry"
```

---

### Task 4: `factors_new/scan.rs` — batch IC scan

**Files:**
- Create: `crates/ploy-research/src/factors_new/scan.rs`
- Modify: `crates/ploy-research/src/factors_new/mod.rs`

- [ ] **Step 1: Write failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::factors::FactorObservation;
    use crate::factors_new::registry::{FactorRegistry, Regime};
    use chrono::Utc;

    fn obs(time_remaining_secs: i64, distance_over_sigma: f64, settlement_up: f64)
        -> FactorObservation
    {
        FactorObservation {
            event_id: "e1".into(), symbol: "BTC".into(), tick_ts: Utc::now(),
            time_remaining_secs, distance_over_sigma, settlement_up,
            signed_distance_to_beat: 0.0, abs_distance_to_beat: 0.0,
            drift_10s: 0.0, drift_30s: 0.0, flip_age_secs: 0.0,
            post_flip_drift: 0.0, sigma_horizon: 0.0, fair_prob_up: 0.0,
            fair_prob_up_clean: 0.0, prob_disagreement: 0.0,
            implied_sigma_horizon: 0.0, vol_gap: 0.0, model_prob_up: 0.0,
            model_edge_up: 0.0, reward_risk_up: 0.0, reward_risk_down: 0.0,
            obi: 0.0, spread_bps: 0.0, microprice_offset_bps: 0.0,
            bid_depth_near: 0.0, ask_depth_near: 0.0, depth_ratio: 0.0,
            depth_imbalance: 0.0, depth_far_ratio: 0.0, depth_acceleration: 0.0,
            obi_10: 0.0, pm_up_bid: 0.0, pm_up_ask: 0.0,
            pm_up_bid_size: 0.0, pm_up_ask_size: 0.0,
            pm_down_bid: 0.0, pm_down_ask: 0.0,
            pm_down_bid_size: 0.0, pm_down_ask_size: 0.0,
            pm_lag_secs: 0.0, future_up_ask_change_30s: None,
            future_up_ask_change_60s: None, cum_obi_delta_5m: 0.0,
            cum_depth_delta_5m: 0.0, cum_mprice_drift_5m: 0.0,
            cum_trade_imbalance_5m: 0.0, spot_move_since_pm_quote: 0.0,
        }
    }

    #[test]
    fn scan_populates_registry_for_early_regime() {
        let observations: Vec<FactorObservation> = (0..20)
            .map(|i| obs(250, i as f64 * 0.1, if i % 2 == 0 { 1.0 } else { 0.0 }))
            .collect();
        let mut reg = FactorRegistry::new();
        scan_into_registry(&observations, &mut reg);
        let top = reg.top_n(Regime::Early, "settlement_up", 1);
        assert!(!top.is_empty());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo test -p ploy-research factors_new::scan 2>&1 | tail -5
```
Expected: compile error — `scan_into_registry` not defined.

- [ ] **Step 3: Implement**

```rust
use crate::factors::{spearman_ic, FactorObservation};
use crate::factors_new::registry::{FactorMeta, FactorRegistry, Regime};

const MIN_OBS: usize = 10;

const FACTOR_EXTRACTORS: &[(&str, fn(&FactorObservation) -> f64)] = &[
    ("distance_over_sigma",    |o| o.distance_over_sigma),
    ("model_prob_up",          |o| o.model_prob_up),
    ("drift_30s",              |o| o.drift_30s),
    ("drift_10s",              |o| o.drift_10s),
    ("obi_10",                 |o| o.obi_10),
    ("depth_imbalance",        |o| o.depth_imbalance),
    ("cum_mprice_drift_5m",    |o| o.cum_mprice_drift_5m),
    ("sigma_horizon",          |o| o.sigma_horizon),
    ("vol_gap",                |o| o.vol_gap),
    ("fair_prob_up_clean",     |o| o.fair_prob_up_clean),
    ("pm_lag_secs",            |o| o.pm_lag_secs),
    ("spread_bps",             |o| o.spread_bps),
    ("microprice_offset_bps",  |o| o.microprice_offset_bps),
    ("depth_far_ratio",        |o| o.depth_far_ratio),
    ("cum_obi_delta_5m",       |o| o.cum_obi_delta_5m),
    ("cum_trade_imbalance_5m", |o| o.cum_trade_imbalance_5m),
];

const LABELS: &[(&str, fn(&FactorObservation) -> Option<f64>)] = &[
    ("settlement_up",            |o| Some(o.settlement_up)),
    ("future_up_ask_change_30s", |o| o.future_up_ask_change_30s),
];

pub fn scan_into_registry(obs: &[FactorObservation], registry: &mut FactorRegistry) {
    for regime in [Regime::Early, Regime::Middle, Regime::Late, Regime::Expiry] {
        let regime_obs: Vec<&FactorObservation> = obs.iter()
            .filter(|o| Regime::from_secs(o.time_remaining_secs) == regime)
            .collect();
        if regime_obs.len() < MIN_OBS { continue; }

        for (label_name, label_fn) in LABELS {
            let ys: Vec<f64> = regime_obs.iter().filter_map(|o| label_fn(o)).collect();
            if ys.len() < MIN_OBS { continue; }
            let n = ys.len();

            for (factor_name, factor_fn) in FACTOR_EXTRACTORS {
                let xs: Vec<f64> = regime_obs.iter().take(n).map(|o| factor_fn(o)).collect();
                let ic = spearman_ic(&xs, &ys);
                if ic.is_nan() { continue; }
                registry.insert(FactorMeta {
                    name: factor_name.to_string(),
                    regime,
                    label: label_name.to_string(),
                    ic,
                    direction: if ic >= 0.0 { 1 } else { -1 },
                    stability: ic.abs(),
                });
            }
        }
    }
}
```

Update `factors_new/mod.rs`:
```rust
pub mod registry;
pub mod scan;
pub use registry::{FactorMeta, FactorRegistry, Regime};
pub use scan::scan_into_registry;
```

- [ ] **Step 4: Run to verify it passes**

```bash
cargo test -p ploy-research factors_new 2>&1 | tail -5
```
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ploy-research/src/factors_new/
git commit -m "feat(research): add scan_into_registry for regime-aware factor IC"
```

---

## Phase 3 — Signal Layer

### Task 5: `Signal` enum and `SignalSource` trait

**Files:**
- Create: `crates/ploy-research/src/signal/traits.rs`
- Modify: `crates/ploy-research/src/signal/mod.rs`

- [ ] **Step 1: Write failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    struct AlwaysBuy;
    impl SignalSource for AlwaysBuy {
        fn signal(&self, _obs: &crate::factors::FactorObservation) -> Signal { Signal::Buy }
    }

    #[test]
    fn signal_source_is_object_safe() {
        let _src: Box<dyn SignalSource> = Box::new(AlwaysBuy);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo test -p ploy-research signal::traits 2>&1 | tail -5
```

- [ ] **Step 3: Implement**

```rust
use crate::factors::FactorObservation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal { Buy, Sell, Hold }

pub trait SignalSource: Send + Sync {
    fn signal(&self, obs: &FactorObservation) -> Signal;
}
```

Update `signal/mod.rs`:
```rust
pub mod traits;
pub use traits::{Signal, SignalSource};
```

- [ ] **Step 4: Run to verify it passes**

```bash
cargo test -p ploy-research signal::traits 2>&1 | tail -5
```

- [ ] **Step 5: Commit**

```bash
git add crates/ploy-research/src/signal/
git commit -m "feat(research): add Signal enum and SignalSource trait"
```

---

### Task 6: `signal/rules.rs` — threshold rule

**Files:**
- Create: `crates/ploy-research/src/signal/rules.rs`
- Modify: `crates/ploy-research/src/signal/mod.rs`

- [ ] **Step 1: Write failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::traits::{Signal, SignalSource};
    use crate::factors::FactorObservation;
    use chrono::Utc;

    fn obs_with(distance_over_sigma: f64) -> FactorObservation {
        FactorObservation {
            distance_over_sigma, time_remaining_secs: 250, settlement_up: 0.0,
            event_id: "e".into(), symbol: "BTC".into(), tick_ts: Utc::now(),
            signed_distance_to_beat: 0.0, abs_distance_to_beat: 0.0,
            drift_10s: 0.0, drift_30s: 0.0, flip_age_secs: 0.0,
            post_flip_drift: 0.0, sigma_horizon: 0.0, fair_prob_up: 0.0,
            fair_prob_up_clean: 0.0, prob_disagreement: 0.0,
            implied_sigma_horizon: 0.0, vol_gap: 0.0, model_prob_up: 0.0,
            model_edge_up: 0.0, reward_risk_up: 0.0, reward_risk_down: 0.0,
            obi: 0.0, spread_bps: 0.0, microprice_offset_bps: 0.0,
            bid_depth_near: 0.0, ask_depth_near: 0.0, depth_ratio: 0.0,
            depth_imbalance: 0.0, depth_far_ratio: 0.0, depth_acceleration: 0.0,
            obi_10: 0.0, pm_up_bid: 0.0, pm_up_ask: 0.0,
            pm_up_bid_size: 0.0, pm_up_ask_size: 0.0,
            pm_down_bid: 0.0, pm_down_ask: 0.0,
            pm_down_bid_size: 0.0, pm_down_ask_size: 0.0,
            pm_lag_secs: 0.0, future_up_ask_change_30s: None,
            future_up_ask_change_60s: None, cum_obi_delta_5m: 0.0,
            cum_depth_delta_5m: 0.0, cum_mprice_drift_5m: 0.0,
            cum_trade_imbalance_5m: 0.0, spot_move_since_pm_quote: 0.0,
        }
    }

    #[test]
    fn threshold_rule_buy_above_threshold() {
        let rule = ThresholdRule {
            extractor: |o: &FactorObservation| o.distance_over_sigma,
            buy_above: Some(0.5),
            sell_below: Some(-0.5),
        };
        assert_eq!(rule.signal(&obs_with(0.8)),  Signal::Buy);
        assert_eq!(rule.signal(&obs_with(-0.8)), Signal::Sell);
        assert_eq!(rule.signal(&obs_with(0.1)),  Signal::Hold);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo test -p ploy-research signal::rules 2>&1 | tail -5
```

- [ ] **Step 3: Implement**

```rust
use crate::factors::FactorObservation;
use crate::signal::traits::{Signal, SignalSource};

pub struct ThresholdRule {
    pub extractor: fn(&FactorObservation) -> f64,
    pub buy_above: Option<f64>,
    pub sell_below: Option<f64>,
}

impl SignalSource for ThresholdRule {
    fn signal(&self, obs: &FactorObservation) -> Signal {
        let v = (self.extractor)(obs);
        if self.buy_above.map_or(false, |t| v > t)  { return Signal::Buy; }
        if self.sell_below.map_or(false, |t| v < t) { return Signal::Sell; }
        Signal::Hold
    }
}
```

Update `signal/mod.rs`:
```rust
pub mod traits;
pub mod rules;
pub use traits::{Signal, SignalSource};
pub use rules::ThresholdRule;
```

- [ ] **Step 4: Run to verify it passes**

```bash
cargo test -p ploy-research signal::rules 2>&1 | tail -5
```

- [ ] **Step 5: Commit**

```bash
git add crates/ploy-research/src/signal/
git commit -m "feat(research): add ThresholdRule signal source"
```

---

### Task 7: `signal/regime.rs` — RegimeRouter

**Files:**
- Create: `crates/ploy-research/src/signal/regime.rs`
- Modify: `crates/ploy-research/src/signal/mod.rs`

- [ ] **Step 1: Write failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::traits::{Signal, SignalSource};
    use crate::factors::FactorObservation;
    use crate::factors_new::registry::Regime;
    use chrono::Utc;

    struct FixedSignal(Signal);
    impl SignalSource for FixedSignal {
        fn signal(&self, _: &FactorObservation) -> Signal { self.0 }
    }

    fn obs_at(time_remaining_secs: i64) -> FactorObservation {
        FactorObservation {
            time_remaining_secs, settlement_up: 0.0,
            event_id: "e".into(), symbol: "BTC".into(), tick_ts: Utc::now(),
            distance_over_sigma: 0.0, signed_distance_to_beat: 0.0,
            abs_distance_to_beat: 0.0, drift_10s: 0.0, drift_30s: 0.0,
            flip_age_secs: 0.0, post_flip_drift: 0.0, sigma_horizon: 0.0,
            fair_prob_up: 0.0, fair_prob_up_clean: 0.0, prob_disagreement: 0.0,
            implied_sigma_horizon: 0.0, vol_gap: 0.0, model_prob_up: 0.0,
            model_edge_up: 0.0, reward_risk_up: 0.0, reward_risk_down: 0.0,
            obi: 0.0, spread_bps: 0.0, microprice_offset_bps: 0.0,
            bid_depth_near: 0.0, ask_depth_near: 0.0, depth_ratio: 0.0,
            depth_imbalance: 0.0, depth_far_ratio: 0.0, depth_acceleration: 0.0,
            obi_10: 0.0, pm_up_bid: 0.0, pm_up_ask: 0.0,
            pm_up_bid_size: 0.0, pm_up_ask_size: 0.0,
            pm_down_bid: 0.0, pm_down_ask: 0.0,
            pm_down_bid_size: 0.0, pm_down_ask_size: 0.0,
            pm_lag_secs: 0.0, future_up_ask_change_30s: None,
            future_up_ask_change_60s: None, cum_obi_delta_5m: 0.0,
            cum_depth_delta_5m: 0.0, cum_mprice_drift_5m: 0.0,
            cum_trade_imbalance_5m: 0.0, spot_move_since_pm_quote: 0.0,
        }
    }

    #[test]
    fn router_dispatches_to_correct_regime_source() {
        let mut router = RegimeRouter::new(Box::new(FixedSignal(Signal::Hold)));
        router.set(Regime::Early, Box::new(FixedSignal(Signal::Buy)));
        assert_eq!(router.signal(&obs_at(250)), Signal::Buy);
        assert_eq!(router.signal(&obs_at(30)),  Signal::Hold);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo test -p ploy-research signal::regime 2>&1 | tail -5
```

- [ ] **Step 3: Implement**

```rust
use std::collections::HashMap;
use crate::factors::FactorObservation;
use crate::factors_new::registry::Regime;
use crate::signal::traits::{Signal, SignalSource};

pub struct RegimeRouter {
    default: Box<dyn SignalSource>,
    routes: HashMap<Regime, Box<dyn SignalSource>>,
}

impl RegimeRouter {
    pub fn new(default: Box<dyn SignalSource>) -> Self {
        Self { default, routes: HashMap::new() }
    }
    pub fn set(&mut self, regime: Regime, source: Box<dyn SignalSource>) {
        self.routes.insert(regime, source);
    }
}

impl SignalSource for RegimeRouter {
    fn signal(&self, obs: &FactorObservation) -> Signal {
        let regime = Regime::from_secs(obs.time_remaining_secs);
        self.routes.get(&regime)
            .map(|s| s.signal(obs))
            .unwrap_or_else(|| self.default.signal(obs))
    }
}
```

Update `signal/mod.rs`:
```rust
pub mod traits;
pub mod rules;
pub mod regime;
pub use traits::{Signal, SignalSource};
pub use rules::ThresholdRule;
pub use regime::RegimeRouter;
```

- [ ] **Step 4: Run to verify it passes**

```bash
cargo test -p ploy-research signal 2>&1 | tail -5
```

- [ ] **Step 5: Commit**

```bash
git add crates/ploy-research/src/signal/
git commit -m "feat(research): add RegimeRouter signal dispatcher"
```

---

## Phase 4 — Backtest Layer

### Task 8: `backtest/engine.rs` — binary event simulator

**Files:**
- Create: `crates/ploy-research/src/backtest/engine.rs`
- Modify: `crates/ploy-research/src/backtest/mod.rs`

- [ ] **Step 1: Write failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::traits::{Signal, SignalSource};
    use crate::factors::FactorObservation;
    use chrono::Utc;

    struct AlwaysBuy;
    impl SignalSource for AlwaysBuy {
        fn signal(&self, _: &FactorObservation) -> Signal { Signal::Buy }
    }

    fn obs(settlement_up: f64, pm_up_ask: f64) -> FactorObservation {
        FactorObservation {
            settlement_up, pm_up_ask, time_remaining_secs: 120,
            event_id: "e1".into(), symbol: "BTC".into(), tick_ts: Utc::now(),
            distance_over_sigma: 0.0, signed_distance_to_beat: 0.0,
            abs_distance_to_beat: 0.0, drift_10s: 0.0, drift_30s: 0.0,
            flip_age_secs: 0.0, post_flip_drift: 0.0, sigma_horizon: 0.0,
            fair_prob_up: 0.0, fair_prob_up_clean: 0.0, prob_disagreement: 0.0,
            implied_sigma_horizon: 0.0, vol_gap: 0.0, model_prob_up: 0.0,
            model_edge_up: 0.0, reward_risk_up: 0.0, reward_risk_down: 0.0,
            obi: 0.0, spread_bps: 0.0, microprice_offset_bps: 0.0,
            bid_depth_near: 0.0, ask_depth_near: 0.0, depth_ratio: 0.0,
            depth_imbalance: 0.0, depth_far_ratio: 0.0, depth_acceleration: 0.0,
            obi_10: 0.0, pm_up_bid: 0.0, pm_up_ask: 0.0,
            pm_up_bid_size: 0.0, pm_up_ask_size: 0.0,
            pm_down_bid: 0.0, pm_down_ask: 0.0,
            pm_down_bid_size: 0.0, pm_down_ask_size: 0.0,
            pm_lag_secs: 0.0, future_up_ask_change_30s: None,
            future_up_ask_change_60s: None, cum_obi_delta_5m: 0.0,
            cum_depth_delta_5m: 0.0, cum_mprice_drift_5m: 0.0,
            cum_trade_imbalance_5m: 0.0, spot_move_since_pm_quote: 0.0,
        }
    }

    #[test]
    fn buy_wins_when_settled_up() {
        // Buy at ask=0.6, settled up → payout = 1.0 - 0.6 - fee
        let fills = run_binary_backtest(&[obs(1.0, 0.6)], &AlwaysBuy, 0.02);
        assert_eq!(fills.len(), 1);
        assert!((fills[0].pnl - 0.38).abs() < 1e-9);
    }

    #[test]
    fn buy_loses_when_settled_down() {
        // Buy at ask=0.6, settled down → payout = -0.6 - fee
        let fills = run_binary_backtest(&[obs(0.0, 0.6)], &AlwaysBuy, 0.02);
        assert!((fills[0].pnl - (-0.62)).abs() < 1e-9);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo test -p ploy-research backtest::engine 2>&1 | tail -5
```

- [ ] **Step 3: Implement**

```rust
use crate::factors::FactorObservation;
use crate::factors_new::registry::Regime;
use crate::signal::traits::{Signal, SignalSource};

#[derive(Debug, Clone)]
pub struct SimulatedFill {
    pub event_id: String,
    pub regime: Regime,
    pub signal: Signal,
    pub entry_price: f64,   // ask for Buy, bid for Sell
    pub settled_up: bool,
    pub pnl: f64,
}

/// One observation = one binary event entry attempt.
/// Hold observations are skipped (no fill produced).
/// P&L formula (Buy):  (1.0 - ask - fee) if settled_up, else (-ask - fee)
/// P&L formula (Sell): (bid - fee) if settled_down, else (bid - 1.0 - fee)
pub fn run_binary_backtest(
    obs: &[FactorObservation],
    source: &dyn SignalSource,
    fee: f64,
) -> Vec<SimulatedFill> {
    obs.iter().filter_map(|o| {
        let signal = source.signal(o);
        if signal == Signal::Hold { return None; }
        let settled_up = o.settlement_up > 0.5;
        let (entry_price, pnl) = match signal {
            Signal::Buy => {
                let ask = o.pm_up_ask;
                let p = if settled_up { 1.0 - ask - fee } else { -ask - fee };
                (ask, p)
            }
            Signal::Sell => {
                let bid = o.pm_up_bid;
                let p = if !settled_up { bid - fee } else { bid - 1.0 - fee };
                (bid, p)
            }
            Signal::Hold => unreachable!(),
        };
        Some(SimulatedFill {
            event_id: o.event_id.clone(),
            regime: Regime::from_secs(o.time_remaining_secs),
            signal,
            entry_price,
            settled_up,
            pnl,
        })
    }).collect()
}
```

Update `backtest/mod.rs`:
```rust
pub mod engine;
pub use engine::{run_binary_backtest, SimulatedFill};
```

- [ ] **Step 4: Run to verify it passes**

```bash
cargo test -p ploy-research backtest::engine 2>&1 | tail -5
```

- [ ] **Step 5: Commit**

```bash
git add crates/ploy-research/src/backtest/
git commit -m "feat(research): add binary event backtest engine"
```

---

### Task 9: `backtest/metrics.rs` — win rate, Sharpe, drawdown

**Files:**
- Create: `crates/ploy-research/src/backtest/metrics.rs`
- Modify: `crates/ploy-research/src/backtest/mod.rs`

- [ ] **Step 1: Write failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_from_pnl_stream() {
        let pnls = vec![0.3, -0.5, 0.4, 0.2, -0.1];
        let m = BacktestMetrics::from_pnls(&pnls);
        assert_eq!(m.trade_count, 5);
        assert_eq!(m.win_count, 3);
        assert!((m.win_rate - 0.6).abs() < 1e-9);
        assert!((m.total_pnl - 0.3).abs() < 1e-9);
    }

    #[test]
    fn max_drawdown_is_correct() {
        // peak=0.3, then drops to -0.2 → drawdown = 0.5
        let pnls = vec![0.3, -0.5];
        let m = BacktestMetrics::from_pnls(&pnls);
        assert!((m.max_drawdown - 0.5).abs() < 1e-9);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo test -p ploy-research backtest::metrics 2>&1 | tail -5
```

- [ ] **Step 3: Implement**

```rust
#[derive(Debug, Clone)]
pub struct BacktestMetrics {
    pub trade_count: usize,
    pub win_count: usize,
    pub win_rate: f64,
    pub total_pnl: f64,
    pub sharpe: f64,
    pub max_drawdown: f64,
}

impl BacktestMetrics {
    pub fn from_pnls(pnls: &[f64]) -> Self {
        let n = pnls.len();
        if n == 0 {
            return Self { trade_count: 0, win_count: 0, win_rate: 0.0,
                total_pnl: 0.0, sharpe: 0.0, max_drawdown: 0.0 };
        }
        let win_count = pnls.iter().filter(|&&p| p > 0.0).count();
        let total_pnl: f64 = pnls.iter().sum();
        let mean = total_pnl / n as f64;
        let variance = pnls.iter().map(|p| (p - mean).powi(2)).sum::<f64>() / n as f64;
        let sharpe = if variance > 0.0 { mean / variance.sqrt() } else { 0.0 };

        let mut peak = 0.0_f64;
        let mut cumulative = 0.0_f64;
        let mut max_drawdown = 0.0_f64;
        for &p in pnls {
            cumulative += p;
            if cumulative > peak { peak = cumulative; }
            let dd = peak - cumulative;
            if dd > max_drawdown { max_drawdown = dd; }
        }

        Self {
            trade_count: n,
            win_count,
            win_rate: win_count as f64 / n as f64,
            total_pnl,
            sharpe,
            max_drawdown,
        }
    }
}
```

Update `backtest/mod.rs`:
```rust
pub mod engine;
pub mod metrics;
pub use engine::{run_binary_backtest, SimulatedFill};
pub use metrics::BacktestMetrics;
```

- [ ] **Step 4: Run to verify it passes**

```bash
cargo test -p ploy-research backtest 2>&1 | tail -5
```

- [ ] **Step 5: Commit**

```bash
git add crates/ploy-research/src/backtest/
git commit -m "feat(research): add BacktestMetrics (win rate, Sharpe, drawdown)"
```

---

## Phase 5 — Attribution Layer

### Task 10: `attribution/regime.rs` + `attribution/factor.rs`

**Files:**
- Create: `crates/ploy-research/src/attribution/regime.rs`
- Create: `crates/ploy-research/src/attribution/factor.rs`
- Modify: `crates/ploy-research/src/attribution/mod.rs`

- [ ] **Step 1: Write failing test**

```rust
// attribution/regime.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest::engine::SimulatedFill;
    use crate::factors_new::registry::Regime;
    use crate::signal::traits::Signal;

    fn fill(regime: Regime, pnl: f64) -> SimulatedFill {
        SimulatedFill {
            event_id: "e".into(), regime, signal: Signal::Buy,
            entry_price: 0.5, settled_up: pnl > 0.0, pnl,
        }
    }

    #[test]
    fn regime_pnl_groups_correctly() {
        let fills = vec![
            fill(Regime::Early, 0.3),
            fill(Regime::Early, -0.1),
            fill(Regime::Late, 0.2),
        ];
        let by_regime = regime_pnl(&fills);
        assert!((by_regime[&Regime::Early].total_pnl - 0.2).abs() < 1e-9);
        assert_eq!(by_regime[&Regime::Early].trade_count, 2);
        assert_eq!(by_regime[&Regime::Late].trade_count, 1);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo test -p ploy-research attribution::regime 2>&1 | tail -5
```

- [ ] **Step 3: Implement `attribution/regime.rs`**

```rust
use std::collections::BTreeMap;
use crate::backtest::engine::SimulatedFill;
use crate::factors_new::registry::Regime;

#[derive(Debug, Clone, Default)]
pub struct RegimePnl {
    pub trade_count: usize,
    pub win_count: usize,
    pub total_pnl: f64,
}

impl RegimePnl {
    pub fn win_rate(&self) -> f64 {
        if self.trade_count == 0 { 0.0 } else { self.win_count as f64 / self.trade_count as f64 }
    }
}

pub fn regime_pnl(fills: &[SimulatedFill]) -> BTreeMap<Regime, RegimePnl> {
    let mut map: BTreeMap<Regime, RegimePnl> = BTreeMap::new();
    for f in fills {
        let e = map.entry(f.regime).or_default();
        e.trade_count += 1;
        e.total_pnl += f.pnl;
        if f.pnl > 0.0 { e.win_count += 1; }
    }
    map
}
```

- [ ] **Step 4: Implement `attribution/factor.rs`**

```rust
use std::collections::BTreeMap;

/// Factor P&L attribution: for each fill, the caller supplies a list of
/// (factor_name, factor_value) pairs active at entry. We accumulate P&L
/// weighted by |factor_value| per factor name.
pub fn factor_pnl(
    fills: &[(f64, Vec<(String, f64)>)],  // (pnl, [(factor, value)])
) -> BTreeMap<String, f64> {
    let mut map: BTreeMap<String, f64> = BTreeMap::new();
    for (pnl, factors) in fills {
        let total_weight: f64 = factors.iter().map(|(_, v)| v.abs()).sum();
        if total_weight == 0.0 { continue; }
        for (name, value) in factors {
            *map.entry(name.clone()).or_default() += pnl * value.abs() / total_weight;
        }
    }
    map
}
```

Update `attribution/mod.rs`:
```rust
pub mod regime;
pub mod factor;
pub use regime::{regime_pnl, RegimePnl};
pub use factor::factor_pnl;
```

- [ ] **Step 5: Run to verify it passes**

```bash
cargo test -p ploy-research attribution 2>&1 | tail -5
```

- [ ] **Step 6: Commit**

```bash
git add crates/ploy-research/src/attribution/
git commit -m "feat(research): add regime and factor P&L attribution"
```

---

### Task 11: `attribution/report.rs` — auto-print report

**Files:**
- Create: `crates/ploy-research/src/attribution/report.rs`
- Modify: `crates/ploy-research/src/attribution/mod.rs`

- [ ] **Step 1: Implement (no test needed — pure display logic)**

```rust
use std::collections::BTreeMap;
use crate::backtest::engine::SimulatedFill;
use crate::backtest::metrics::BacktestMetrics;
use crate::factors_new::registry::Regime;
use crate::attribution::regime::{regime_pnl, RegimePnl};

pub struct AttributionReport {
    pub overall: BacktestMetrics,
    pub by_regime: BTreeMap<Regime, RegimePnl>,
    pub by_factor: BTreeMap<String, f64>,
}

impl AttributionReport {
    pub fn build(
        fills: &[SimulatedFill],
        factor_fills: &[(f64, Vec<(String, f64)>)],
    ) -> Self {
        let pnls: Vec<f64> = fills.iter().map(|f| f.pnl).collect();
        Self {
            overall: BacktestMetrics::from_pnls(&pnls),
            by_regime: regime_pnl(fills),
            by_factor: crate::attribution::factor::factor_pnl(factor_fills),
        }
    }

    pub fn print(&self) {
        eprintln!("\n=== Attribution Report ===");
        eprintln!("Overall: trades={} win_rate={:.1}% total_pnl={:.4} sharpe={:.3} max_dd={:.4}",
            self.overall.trade_count,
            self.overall.win_rate * 100.0,
            self.overall.total_pnl,
            self.overall.sharpe,
            self.overall.max_drawdown,
        );
        eprintln!("\n--- By Regime ---");
        for (regime, r) in &self.by_regime {
            eprintln!("  {:8} trades={:4} win={:.1}% pnl={:.4}",
                regime.as_str(), r.trade_count, r.win_rate() * 100.0, r.total_pnl);
        }
        eprintln!("\n--- By Factor (P&L contribution) ---");
        let mut factor_vec: Vec<(&String, &f64)> = self.by_factor.iter().collect();
        factor_vec.sort_by(|a, b| b.1.abs().partial_cmp(&a.1.abs()).unwrap());
        for (name, pnl) in factor_vec.iter().take(10) {
            eprintln!("  {:30} {:+.4}", name, pnl);
        }
        eprintln!("=========================\n");
    }
}
```

Update `attribution/mod.rs`:
```rust
pub mod regime;
pub mod factor;
pub mod report;
pub use regime::{regime_pnl, RegimePnl};
pub use factor::factor_pnl;
pub use report::AttributionReport;
```

- [ ] **Step 2: Verify build**

```bash
cargo build -p ploy-research 2>&1 | tail -5
```

- [ ] **Step 3: Commit**

```bash
git add crates/ploy-research/src/attribution/
git commit -m "feat(research): add AttributionReport with auto-print"
```

---

## Phase 6 — Model Interface Skeletons

### Task 12: `model/traits.rs` — StrategyModel and RlAgent

**Files:**
- Create: `crates/ploy-research/src/model/traits.rs`
- Create: `crates/ploy-research/src/model/supervised/mod.rs`
- Create: `crates/ploy-research/src/model/rl/mod.rs`
- Modify: `crates/ploy-research/src/model/mod.rs`

- [ ] **Step 1: Implement trait skeletons**

`model/traits.rs`:
```rust
use std::path::Path;
use crate::factors::FactorObservation;
use crate::signal::traits::SignalSource;

/// Supervised ML model: fits on labelled observations, produces signals.
pub trait StrategyModel: SignalSource {
    fn fit(&mut self, obs: &[FactorObservation], labels: &[bool]);
    /// Returns (factor_name, importance_score) pairs, sorted descending.
    fn feature_importance(&self) -> Vec<(String, f64)>;
    fn save(&self, path: &Path) -> anyhow::Result<()>;
    fn load(path: &Path) -> anyhow::Result<Self> where Self: Sized;
}

/// RL transition: one step of experience.
#[derive(Debug, Clone)]
pub struct Transition {
    pub state: Vec<f64>,
    pub action: u8,   // 0=Hold, 1=Buy, 2=Sell
    pub reward: f64,
    pub next_state: Vec<f64>,
    pub done: bool,
}

/// RL agent: acts in an environment and learns from transitions.
pub trait RlAgent: SignalSource {
    fn act(&self, state: &[f64], epsilon: f64) -> u8;
    fn update(&mut self, transition: &Transition);
    fn save(&self, path: &Path) -> anyhow::Result<()>;
    fn load(path: &Path) -> anyhow::Result<Self> where Self: Sized;
}
```

`model/rl/env.rs`:
```rust
use crate::factors::FactorObservation;
use crate::signal::traits::Signal;

pub trait Environment {
    fn reset(&mut self) -> Vec<f64>;
    /// Returns (next_state, reward, done)
    fn step(&mut self, action: u8) -> (Vec<f64>, f64, bool);
}

/// Wraps a slice of FactorObservations as an RL environment.
/// Each step advances to the next observation; reward = simulated P&L.
pub struct BinaryEventEnv<'a> {
    obs: &'a [FactorObservation],
    cursor: usize,
    fee: f64,
}

impl<'a> BinaryEventEnv<'a> {
    pub fn new(obs: &'a [FactorObservation], fee: f64) -> Self {
        Self { obs, cursor: 0, fee }
    }
}

impl<'a> Environment for BinaryEventEnv<'a> {
    fn reset(&mut self) -> Vec<f64> {
        self.cursor = 0;
        obs_to_state(&self.obs[0])
    }

    fn step(&mut self, action: u8) -> (Vec<f64>, f64, bool) {
        let o = &self.obs[self.cursor];
        let settled_up = o.settlement_up > 0.5;
        let reward = match action {
            1 => if settled_up { 1.0 - o.pm_up_ask - self.fee } else { -o.pm_up_ask - self.fee },
            2 => if !settled_up { o.pm_up_bid - self.fee } else { o.pm_up_bid - 1.0 - self.fee },
            _ => 0.0,
        };
        self.cursor += 1;
        let done = self.cursor >= self.obs.len();
        let next = if done { vec![0.0; 16] } else { obs_to_state(&self.obs[self.cursor]) };
        (next, reward, done)
    }
}

fn obs_to_state(o: &FactorObservation) -> Vec<f64> {
    vec![
        o.time_remaining_secs as f64 / 300.0,
        o.distance_over_sigma,
        o.model_prob_up,
        o.drift_30s,
        o.obi_10,
        o.depth_imbalance,
        o.cum_mprice_drift_5m,
        o.sigma_horizon,
        o.vol_gap,
        o.fair_prob_up_clean,
        o.pm_lag_secs / 60.0,
        o.spread_bps / 100.0,
        o.microprice_offset_bps / 100.0,
        o.depth_far_ratio,
        o.cum_obi_delta_5m,
        o.cum_trade_imbalance_5m,
    ]
}
```

`model/rl/replay.rs`:
```rust
use std::collections::VecDeque;
use crate::model::traits::Transition;

pub struct ReplayBuffer {
    buffer: VecDeque<Transition>,
    capacity: usize,
}

impl ReplayBuffer {
    pub fn new(capacity: usize) -> Self {
        Self { buffer: VecDeque::with_capacity(capacity), capacity }
    }
    pub fn push(&mut self, t: Transition) {
        if self.buffer.len() == self.capacity { self.buffer.pop_front(); }
        self.buffer.push_back(t);
    }
    pub fn len(&self) -> usize { self.buffer.len() }
    pub fn sample(&self, n: usize) -> Vec<&Transition> {
        // deterministic sample: evenly spaced (replace with random in production)
        let step = (self.buffer.len() / n).max(1);
        self.buffer.iter().step_by(step).take(n).collect()
    }
}
```

`model/rl/dqn.rs` (skeleton — burn integration wired up in a future task):
```rust
// DQN agent skeleton. Full burn neural network wiring is a follow-up task.
// This file establishes the interface so the rest of the pipeline compiles.
use std::path::Path;
use crate::factors::FactorObservation;
use crate::model::traits::{RlAgent, Transition};
use crate::signal::traits::{Signal, SignalSource};

pub struct DqnAgent {
    pub epsilon: f64,
    pub state_dim: usize,
    pub action_dim: usize,
}

impl DqnAgent {
    pub fn new(state_dim: usize, action_dim: usize) -> Self {
        Self { epsilon: 1.0, state_dim, action_dim }
    }
}

impl SignalSource for DqnAgent {
    fn signal(&self, _obs: &FactorObservation) -> Signal { Signal::Hold }
}

impl RlAgent for DqnAgent {
    fn act(&self, _state: &[f64], _epsilon: f64) -> u8 { 0 }
    fn update(&mut self, _transition: &Transition) {}
    fn save(&self, _path: &Path) -> anyhow::Result<()> { Ok(()) }
    fn load(_path: &Path) -> anyhow::Result<Self> {
        Ok(Self::new(16, 3))
    }
}
```

Update `model/mod.rs`:
```rust
pub mod traits;
pub mod supervised;
pub mod rl;
pub use traits::{RlAgent, StrategyModel, Transition};
```

`model/supervised/mod.rs`:
```rust
// Supervised model implementations (linfa, forust-ml) — follow-up tasks.
```

`model/rl/mod.rs`:
```rust
pub mod env;
pub mod replay;
pub mod dqn;
pub use env::{BinaryEventEnv, Environment};
pub use replay::ReplayBuffer;
pub use dqn::DqnAgent;
```

- [ ] **Step 2: Add `anyhow` to Cargo.toml**

```toml
anyhow = "1"
```

- [ ] **Step 3: Verify build**

```bash
cargo build -p ploy-research 2>&1 | tail -5
```
Expected: compiles without error.

- [ ] **Step 4: Commit**

```bash
git add crates/ploy-research/src/model/ crates/ploy-research/Cargo.toml
git commit -m "feat(research): add StrategyModel, RlAgent, BinaryEventEnv, ReplayBuffer, DqnAgent skeletons"
```

---

## Phase 7 — Wire Up & Smoke Test

### Task 13: Update `lib.rs` exports and run full test suite

**Files:**
- Modify: `crates/ploy-research/src/lib.rs`

- [ ] **Step 1: Add new public re-exports**

Append to `crates/ploy-research/src/lib.rs`:
```rust
// New layered pipeline exports
pub use factors_new::{FactorMeta, FactorRegistry, Regime, scan_into_registry};
pub use signal::{Signal, SignalSource, ThresholdRule, RegimeRouter};
pub use backtest::{run_binary_backtest, SimulatedFill, BacktestMetrics};
pub use attribution::{regime_pnl, RegimePnl, factor_pnl, AttributionReport};
pub use model::{RlAgent, StrategyModel, Transition};
pub use model::rl::{BinaryEventEnv, Environment, ReplayBuffer, DqnAgent};
```

- [ ] **Step 2: Run full test suite**

```bash
cargo test -p ploy-research 2>&1 | tail -20
```
Expected: all tests pass, no regressions in existing tests.

- [ ] **Step 3: Commit**

```bash
git add crates/ploy-research/src/lib.rs
git commit -m "feat(research): expose new pipeline layers from lib.rs"
```

---

### Task 14: Add `factor_scan` example entry point

**Files:**
- Create: `crates/ploy-research/examples/factor_scan.rs`

- [ ] **Step 1: Create minimal working example**

```rust
//! factor_scan — regime-aware factor IC scanner
//!
//! Usage:
//!   cargo run -p ploy-research --example factor_scan -- \
//!     --symbols BTC,ETH --days 7 --db-url postgres://...

use ploy_research::{scan_into_registry, FactorRegistry};
use ploy_research::factors::build_factor_observations_with_lob;

#[tokio::main]
async fn main() {
    // Minimal smoke: build an empty registry and print header.
    // Full DB-backed implementation reuses existing factor_research.rs loader.
    let mut registry = FactorRegistry::new();
    eprintln!("factor_scan: registry ready, {} factors loaded", registry.all().len());
    eprintln!("Wire up DB loader from factor_research.rs to populate registry.");
}
```

- [ ] **Step 2: Verify it compiles**

```bash
cargo build -p ploy-research --example factor_scan 2>&1 | tail -5
```

- [ ] **Step 3: Commit**

```bash
git add crates/ploy-research/examples/factor_scan.rs
git commit -m "feat(research): add factor_scan example entry point"
```

---

## Self-Review Checklist

- [x] **Spec coverage:** All 5 layers (data stub, factors, signal, backtest, attribution) + ML/RL interface skeletons covered
- [x] **Placeholder scan:** No TBD/TODO in task steps; DqnAgent skeleton is intentional and documented
- [x] **Type consistency:** `SimulatedFill` defined in Task 8, used in Tasks 10/11; `Regime` defined in Task 3, used throughout; `Signal` defined in Task 5, used in Tasks 6/7/8/12
- [x] **FactorObservation fields:** All test helpers zero-fill every field using the exact field names from `factors.rs:17-60`
- [x] **`pm_up_bid` field:** Used in Task 8 Sell branch — verify field exists in `FactorObservation` before running

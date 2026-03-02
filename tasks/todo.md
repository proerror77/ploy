# Todo

- [x] Rewire `strategy::risk_mgmt::RiskManager` to delegate runtime state/circuit logic to `platform::RiskGate`
- [x] Keep `RiskManager` public API stable for engine/services (`state`, `can_trade`, `daily_stats`, `halt_reason`, etc.)
- [x] Route `check_leg1_entry` through unified `RiskGate::check_order` pre-trade gate
- [x] Introduce `services::RiskView` trait for risk observability consumers
- [x] Refactor `services::health` and `services::metrics` to depend on `RiskView` instead of concrete `RiskManager`
- [x] Fix signed/unsigned time conversion edge case in `check_leg1_entry` and `must_force_leg2`
- [x] Run targeted tests (`strategy::risk_mgmt::risk::tests`, `strategy::execution::engine::tests`, `services::health::tests`)
- [x] Run `cargo build` for regression check
- [x] Update issue #38 work log with this migration slice

## Review

- Planned execution target: GitHub issue #38 (Phase 3 risk unification).
- Delivered in this slice:
  - `RiskManager` remains adapter over shared `RiskGate` runtime.
  - `StrategyEngine` pre-trade path uses `RiskGate::check_order` through adapter.
  - Added `src/services/risk_view.rs` with `RiskView` abstraction and impls for both `RiskManager` and `RiskGate`.
  - `HealthState` / `Metrics` now consume `RiskView`, enabling coordinator and single-strategy paths to share observability plumbing.
  - `i64 -> u64` time conversion pitfall removed in `check_leg1_entry`/`must_force_leg2`.
- Validation executed:
  - `cargo test strategy::risk_mgmt::risk::tests -- --nocapture` (5/5)
  - `cargo test strategy::execution::engine::tests -- --nocapture` (12/12)
  - `cargo test services::health::tests -- --nocapture` (3/3)
  - `cargo build` (pass)
- Notes:
  - Existing unrelated warning may appear in test invocations: `src/strategy/directional_backtest.rs:1594` (`unused variable: results`).

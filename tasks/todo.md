# Todo

- [x] Rewire `strategy::risk_mgmt::RiskManager` to delegate runtime state/circuit logic to `platform::RiskGate`
- [x] Keep `RiskManager` public API stable for engine/services (`state`, `can_trade`, `daily_stats`, `halt_reason`, etc.)
- [x] Route `check_leg1_entry` through unified `RiskGate::check_order` pre-trade gate
- [x] Introduce `services::RiskView` trait for risk observability consumers
- [x] Refactor `services::health` and `services::metrics` to depend on `RiskView` instead of concrete `RiskManager`
- [x] Add `RiskView` trait tests for both `RiskGate` and `RiskManager` impls
- [x] Fix signed/unsigned time conversion edge case in `check_leg1_entry` and `must_force_leg2`
- [x] Run targeted tests (`strategy::risk_mgmt::risk::tests`, `strategy::execution::engine::tests`, `services::health::tests`, `services::risk_view::tests`)
- [x] Run `cargo build` for regression check
- [x] Update issue #38 work log with this migration slice

## Review

- Planned execution target: GitHub issue #38 (Phase 3 risk unification).
- Delivered in this slice:
  - `RiskManager` remains adapter over shared `RiskGate` runtime.
  - `StrategyEngine` pre-trade path uses `RiskGate::check_order` through adapter.
  - Added `src/services/risk_view.rs` with `RiskView` abstraction and impls for both `RiskManager` and `RiskGate`.
  - `HealthState` / `Metrics` now consume `RiskView`, and `HealthState::with_risk_gate` allows direct coordinator wiring with `RiskGate`.
  - Added direct tests validating `RiskView` state/stat mapping semantics.
  - `i64 -> u64` time conversion pitfall removed in `check_leg1_entry`/`must_force_leg2`.
- Validation executed:
  - `cargo test strategy::risk_mgmt::risk::tests -- --nocapture` (5/5)
  - `cargo test strategy::execution::engine::tests -- --nocapture` (12/12)
  - `cargo test services::health::tests -- --nocapture` (4/4)
  - `cargo test services::risk_view::tests -- --nocapture` (2/2)
  - `cargo build` (pass)
- Notes:
  - Cleared prior `unused variable: results` warning in `src/strategy/directional_backtest.rs`.

---

## 2026-03-03 Issue Sweep (#20 / #21)

- [x] Audit open issues and identify remaining gaps for closure
- [x] Extend baseline collector to support accelerated virtual-time capture (`--time-scale`)
- [x] Add deterministic mock metrics endpoint for reproducible seed baselines
- [x] Generate and commit Phase 0 seed baseline artifacts under `docs/data_plane_baseline/`
- [x] Fix rollback validator to honor virtual timestamps from accelerated captures
- [x] Update Phase 0 runbook with production and accelerated baseline workflows
- [x] Re-run baseline collection + rollback validation to verify artifact consistency
- [x] Close remaining open issues after publishing completion notes

## Review (2026-03-03)

- Remaining open GitHub issues were `#20` (epic) and `#21` (Phase 0).
- Implemented deterministic + accelerated baseline workflow to produce reproducible Phase 0 comparison artifacts without waiting a full wall-clock day.
- Committed baseline outputs and rollback report:
  - `docs/data_plane_baseline/phase0-seed-20260303.*`
- Rollback validation now supports accelerated captures by preferring `virtual_epoch_s` when present.
- Runbook now documents both:
  - production 24h capture on live metrics endpoint
  - accelerated seed baseline path for immediate verification/comparison.

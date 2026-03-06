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

---

## 2026-03-06 Workspace Restructure Slice (Phase 5.1 / 5.3 bridge)

- [x] Add `crates/ploy-backtest` to the workspace and keep it building independently
- [x] Move shared backtest infrastructure into `ploy-backtest` (`engine`, `feed`, `recorder`, `report`, `execution_sim`)
- [x] Restore root-crate compatibility modules under `src/strategy/` so existing strategy code still compiles
- [x] Keep legacy volatility-arb `BacktestEngine` and `PaperTrader` in the app layer while shared types live in `ploy-backtest`
- [x] Add `HistoricalFeed::new(...)` in `ploy-backtest` and switch root tests off private-field construction
- [x] Update CLI backtest output path to use shared `BacktestResults::report()`
- [x] Run formatting and validation for the migrated slice

## Review (2026-03-06)

- Delivered in this slice:
  - Created the standalone `ploy-backtest` crate and wired it into the workspace.
  - Moved generic backtest infrastructure into the new crate.
  - Added thin compatibility modules at `src/strategy/backtest*.rs` and `src/strategy/execution_sim.rs` so the main app can keep compiling during the larger migration.
  - Kept legacy volatility-arb-only helpers in [`src/strategy/backtest.rs`](/Users/proerror/Documents/ploy-refactor/src/strategy/backtest.rs) because they still depend on app-local `volatility_arb` logic.
  - Added a public `HistoricalFeed::new` constructor in [`crates/ploy-backtest/src/feed.rs`](/Users/proerror/Documents/ploy-refactor/crates/ploy-backtest/src/feed.rs) to preserve test ergonomics without exposing internals.
- Validation executed:
  - `cargo build -p ploy-backtest`
  - `cargo test -p ploy-backtest`
  - `cargo build`
  - `cargo test test_engine_empty_feed -- --nocapture`

---

## 2026-03-06 Workspace Restructure Slice (Phase 6 export cleanup)

- [x] Stop re-exporting shared backtest infrastructure from `strategy::impls`
- [x] Update `paper_runner` to import `PaperTrader` and related types from their owning modules
- [x] Keep user-facing paper-mode entrypoints stable while shrinking `impls` surface area
- [x] Re-run compile checks after the export boundary cleanup

## Review (2026-03-06, export cleanup)

- Delivered in this slice:
  - Removed legacy backtest infrastructure re-exports from [`src/strategy/impls.rs`](/Users/proerror/Documents/ploy-refactor/src/strategy/impls.rs).
  - Moved [`src/strategy/paper_runner.rs`](/Users/proerror/Documents/ploy-refactor/src/strategy/paper_runner.rs) off the `impls` aggregate for `PaperTrader` and `PaperTradingStats`.
  - Left higher-level paper trading entrypoints (`PaperTradingConfig`, `PaperTradingRunner`, `run_paper_trading`) intact so CLI wiring does not change yet.
- Validation executed:
  - `cargo build`
  - `cargo test paper_runner -- --nocapture`

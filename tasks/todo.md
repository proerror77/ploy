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

---

## 2026-03-06 Workspace Restructure Slice (Phase 6 direct ploy-backtest imports)

- [x] Point strategy backtest engines at `ploy-backtest` instead of `strategy::backtest_*` wrappers
- [x] Point strategy CLI backtest runtime at `ploy-backtest` recorder/feed/report modules
- [x] Keep the legacy `strategy::backtest` compatibility module only for vol-arb paper trading
- [x] Re-run compile checks after the import boundary cleanup

## Review (2026-03-06, direct imports)

- Delivered in this slice:
  - Updated [`src/strategy/directional_backtest.rs`](/Users/proerror/Documents/ploy-refactor/src/strategy/directional_backtest.rs), [`src/strategy/momentum_backtest.rs`](/Users/proerror/Documents/ploy-refactor/src/strategy/momentum_backtest.rs), and [`src/strategy/staggered_arb_backtest.rs`](/Users/proerror/Documents/ploy-refactor/src/strategy/staggered_arb_backtest.rs) to import shared backtest types directly from `ploy_backtest`.
  - Updated [`src/cli/strategy.rs`](/Users/proerror/Documents/ploy-refactor/src/cli/strategy.rs) to use `ploy_backtest` feed/recorder/report types directly in the backtest CLI path.
  - Left `src/strategy/backtest_feed.rs`, `src/strategy/backtest_recorder.rs`, `src/strategy/backtest_report.rs`, and `src/strategy/execution_sim.rs` in place as compatibility shims for now.
- Validation executed:
  - `cargo build`
  - `cargo test paper_runner -- --nocapture`

---

## 2026-03-06 Workspace Restructure Slice (Phase 6 remove obsolete shims)

- [x] Remove obsolete `strategy` backtest shim modules that no longer have internal callers
- [x] Trim `src/strategy/mod.rs` to stop exposing deleted shim modules
- [x] Confirm no internal references remain to the removed shim paths
- [x] Re-run compile checks after the module surface cleanup

## Review (2026-03-06, remove obsolete shims)

- Delivered in this slice:
  - Removed `src/strategy/backtest_feed.rs`, `src/strategy/backtest_recorder.rs`, `src/strategy/backtest_report.rs`, and `src/strategy/execution_sim.rs`.
  - Updated [`src/strategy/mod.rs`](/Users/proerror/Documents/ploy-refactor/src/strategy/mod.rs) so those shim modules are no longer part of the `strategy` public surface.
  - Kept [`src/strategy/backtest.rs`](/Users/proerror/Documents/ploy-refactor/src/strategy/backtest.rs) because it still owns the volatility-arb compatibility helpers used by paper trading.
- Validation executed:
  - `cargo build`
  - `cargo test paper_runner -- --nocapture`

---

## 2026-03-06 Workspace Restructure Slice (agent-team handoff)

- [ ] Phase 6.4 split `src/strategy/momentum.rs` into `src/strategy/momentum/`
- [ ] Keep the public `crate::strategy::momentum::*` surface stable with `mod.rs` re-exports
- [ ] Phase 5.2 migrate the first concrete backtest strategy into `crates/ploy-backtest/src/strategies/`
- [ ] Add only the smallest new `ploy-backtest::strategies` API needed for that first migration
- [ ] Re-run targeted build/tests for each slice before cherry-picking into `refactor/workspace-restructure`

## Review (2026-03-06, agent-team handoff)

- Worker ownership:
  - `session/momentum-split` at `../ploy-momentum-split/` owns `src/strategy/momentum*`
  - `session/backtest-migrate` at `../ploy-backtest-migrate/` owns `crates/ploy-backtest/src/strategies/*` plus the minimum caller updates for the first migrated strategy
- Integrator keeps ownership of `tasks/todo.md`, final cherry-picks, and validation on `refactor/workspace-restructure`.

---

## 2026-03-06 Workspace Restructure Slice (Phase 6.4 momentum split)

- [x] Convert `src/strategy/momentum.rs` to directory form
- [x] Extract `MomentumConfig` and `ExitConfig` into `src/strategy/momentum/config.rs`
- [x] Extract `Position`, `ExitReason`, and `ExitManager` into `src/strategy/momentum/position.rs`
- [x] Preserve `crate::strategy::momentum::*` imports via `mod.rs` re-exports
- [x] Re-run build and focused momentum tests after the split

## Review (2026-03-06, momentum split)

- Delivered in this slice:
  - Renamed [`src/strategy/momentum.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/strategy/momentum.rs) into directory form at [`src/strategy/momentum/mod.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/strategy/momentum/mod.rs).
  - Moved config/default definitions into [`src/strategy/momentum/config.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/strategy/momentum/config.rs).
  - Moved position/exit logic into [`src/strategy/momentum/position.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/strategy/momentum/position.rs).
  - Kept `MomentumEngine`, `EventMatcher`, `MomentumDetector`, and the existing tests in `mod.rs` to avoid changing async/runtime behavior in the same commit.
- Validation executed:
  - `cargo build`
  - `cargo test strategy::momentum::tests -- --nocapture`
- Agent review:
  - `No findings` from reviewer.
  - Residual risk only: no direct regression test yet for full `MomentumConfig` / `ExitConfig` defaults or the `TrailingStop` / `TimeExit` branches.

---

## 2026-03-06 Workspace Restructure Slice (Phase 5.2 momentum backtest bridge)

- [x] Add the first concrete strategy bridge under `crates/ploy-backtest/src/strategies/`
- [x] Keep `src/strategy/momentum_backtest.rs` in place while moving pure result-aggregation logic to `ploy-backtest`
- [x] Re-run `ploy-backtest` tests and focused momentum-backtest validation
- [x] Avoid introducing an app-to-crate dependency cycle through `MomentumConfig`

## Review (2026-03-06, momentum backtest bridge)

- Delivered in this slice:
  - Added [`crates/ploy-backtest/src/strategies/momentum.rs`](/Users/proerror/Documents/ploy-refactor-integrate/crates/ploy-backtest/src/strategies/momentum.rs) with momentum-specific closed-trade and result aggregation helpers.
  - Exported the new bridge from [`crates/ploy-backtest/src/strategies/mod.rs`](/Users/proerror/Documents/ploy-refactor-integrate/crates/ploy-backtest/src/strategies/mod.rs).
  - Updated [`src/strategy/momentum_backtest.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/strategy/momentum_backtest.rs) to delegate result-building to the new crate-owned helper while keeping the live detector and execution loop local.
  - Explicitly did not move `MomentumBacktestConfig` yet because it embeds app-local `MomentumConfig`, which would create a dependency cycle if moved directly.
- Validation executed:
  - `cargo build`
  - `cargo test -p ploy-backtest`
  - `cargo test test_sharpe_calculation -- --nocapture`
- Agent review:
  - `No findings` from reviewer.
  - Residual risk only: the new helper is covered indirectly; there is still no direct non-empty trade parity test for field-by-field `BacktestResults` construction.

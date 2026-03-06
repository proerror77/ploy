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
  - Residual risk was reduced with follow-up direct tests for `TrailingStop` / `TimeExit`.
  - Remaining gap: no direct regression test yet for full `MomentumConfig` / `ExitConfig` defaults.

---

## 2026-03-06 Workspace Restructure Slice (Phase 6.4 momentum window risk split)

- [x] Extract `DailyTradeCounter`, `PendingSignal`, and `WindowRiskTracker` into `src/strategy/momentum/window_risk.rs`
- [x] Keep `MomentumEngine` call sites unchanged except for module imports
- [x] Add direct tests for window rounding and ready/best-edge signal selection
- [x] Re-run build plus momentum regressions after the split

## Review (2026-03-06, momentum window risk split)

- Delivered in this slice:
  - Moved window-exposure and pending-signal state from [`src/strategy/momentum/mod.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/strategy/momentum/mod.rs) into [`src/strategy/momentum/window_risk.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/strategy/momentum/window_risk.rs).
  - Kept the engine-facing API stable by importing the extracted private types back into `mod.rs`.
  - Added direct tests for 15-minute window rounding and delayed best-edge selection in the new module.
- Validation executed:
  - `cargo build`
  - `cargo test strategy::momentum::window_risk::tests -- --nocapture`
  - `cargo test strategy::momentum::tests -- --nocapture`

---

## 2026-03-06 Workspace Restructure Slice (Phase 6.4 momentum event/signal split)

- [x] Extract `EventInfo` and `EventMatcher` into `src/strategy/momentum/event_matcher.rs`
- [x] Extract `Direction`, `MomentumSignal`, and `MomentumDetector` into `src/strategy/momentum/signal.rs`
- [x] Keep `crate::strategy::momentum::*` stable with `mod.rs` re-exports
- [x] Add direct default-value tests for `MomentumConfig` and `ExitConfig`
- [x] Re-run momentum regression tests after the split

## Review (2026-03-06, momentum event/signal split)

- Delivered in this slice:
  - Moved event discovery and Polymarket series mapping from [`src/strategy/momentum/mod.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/strategy/momentum/mod.rs) into [`src/strategy/momentum/event_matcher.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/strategy/momentum/event_matcher.rs).
  - Moved signal-direction, signal payload, and momentum detection logic into [`src/strategy/momentum/signal.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/strategy/momentum/signal.rs).
  - Preserved the existing external import surface by re-exporting those types from `mod.rs`.
  - Added direct default regression coverage in [`src/strategy/momentum/config.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/strategy/momentum/config.rs).
- Validation executed:
  - `cargo build`
  - `cargo test strategy::momentum::config::tests -- --nocapture`
  - `cargo test strategy::momentum::tests -- --nocapture`
- Residual risk:
  - `momentum/mod.rs` still contains the main `MomentumEngine` runtime and its helper methods; the directory split is structurally close to complete, but the engine body is still the remaining god-file segment.

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
  - Residual risk was reduced with follow-up direct tests for non-empty trade aggregation and zero-trade Sharpe.

---

## 2026-03-06 Workspace Restructure Slice (Phase 6.4 momentum engine split)

- [x] Move the remaining `MomentumEngine` runtime and helper methods out of `src/strategy/momentum/mod.rs`
- [x] Keep `crate::strategy::momentum::*` stable with `mod.rs` as a thin assembly and re-export layer
- [x] Preserve the existing focused module tests after the move
- [x] Re-run build plus focused momentum validation after the split

## Review (2026-03-06, momentum engine split)

- Delivered in this slice:
  - Moved the remaining `MomentumEngine` runtime and helper methods from [`src/strategy/momentum/mod.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/strategy/momentum/mod.rs) into [`src/strategy/momentum/engine.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/strategy/momentum/engine.rs).
  - Reduced `mod.rs` to assembly/re-export responsibilities so future splits can avoid editing a giant mixed file.
- Validation executed:
  - `cargo build`
  - `cargo test strategy::momentum::config::tests -- --nocapture`
  - `cargo test strategy::momentum::window_risk::tests -- --nocapture`
  - `cargo test strategy::momentum::tests -- --nocapture`

---

## 2026-03-06 Workspace Restructure Slice (Phase 5.2 / 6 momentum backtest persistence split)

- [x] Convert `src/strategy/momentum_backtest.rs` to directory form
- [x] Move result persistence into `src/strategy/momentum_backtest/persistence.rs`
- [x] Keep `crate::strategy::momentum_backtest::*` stable via `mod.rs` re-export
- [x] Add direct tests for Sharpe status band mapping
- [x] Re-run build plus focused momentum-backtest validation

## Review (2026-03-06, momentum backtest persistence split)

- Delivered in this slice:
  - Renamed [`src/strategy/momentum_backtest.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/strategy/momentum_backtest.rs) into directory form at [`src/strategy/momentum_backtest/mod.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/strategy/momentum_backtest/mod.rs).
  - Moved the DB write path into [`src/strategy/momentum_backtest/persistence.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/strategy/momentum_backtest/persistence.rs) so the engine file now owns runtime logic only.
  - Preserved the existing external call surface by re-exporting `save_backtest_results` from `mod.rs`.
  - Added a direct unit test for `FAIL` / `WARN` / `PASS` Sharpe status thresholds in the new persistence module.
- Validation executed:
  - `cargo test strategy::momentum_backtest -- --nocapture`
  - `cargo build`

---

## 2026-03-06 Workspace Restructure Slice (Phase 5.2 / 6 momentum backtest config split)

- [x] Move `MomentumBacktestConfig` into `src/strategy/momentum_backtest/config.rs`
- [x] Keep `crate::strategy::momentum_backtest::*` stable via `mod.rs` re-export
- [x] Add a direct config regression test for symbol propagation/defaults
- [x] Re-run build plus focused momentum-backtest validation

## Review (2026-03-06, momentum backtest config split)

- Delivered in this slice:
  - Moved [`src/strategy/momentum_backtest/mod.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/strategy/momentum_backtest/mod.rs) config definitions into [`src/strategy/momentum_backtest/config.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/strategy/momentum_backtest/config.rs).
  - Kept the public import surface stable by re-exporting `MomentumBacktestConfig` from `mod.rs`.
  - Added a direct regression test that checks top-level symbols, nested `momentum_config.symbols`, and default concurrency/cooldown values stay aligned.
- Validation executed:
  - `cargo test strategy::momentum_backtest -- --nocapture`
  - `cargo build`

---

## 2026-03-06 Workspace Restructure Slice (Phase 5.2 / 6 momentum backtest engine split)

- [x] Move `MomentumBacktestEngine` and its private position-tracking helpers into `src/strategy/momentum_backtest/engine.rs`
- [x] Keep `crate::strategy::momentum_backtest::*` stable via `mod.rs` re-export
- [x] Leave tests at the module root so external call paths remain the exercised surface
- [x] Re-run build plus focused momentum-backtest validation

## Review (2026-03-06, momentum backtest engine split)

- Delivered in this slice:
  - Moved the runtime-heavy engine body from [`src/strategy/momentum_backtest/mod.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/strategy/momentum_backtest/mod.rs) into [`src/strategy/momentum_backtest/engine.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/strategy/momentum_backtest/engine.rs).
  - Reduced `mod.rs` to module wiring, public re-exports, and the focused root-level tests.
  - Kept the public import surface unchanged for callers such as [`src/cli/strategy/backtest.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/cli/strategy/backtest.rs).
- Validation executed:
  - `cargo test strategy::momentum_backtest -- --nocapture`
  - `cargo build`

---

## 2026-03-06 Workspace Restructure Slice (Phase 5.2 / 6 staggered-arb backtest config split)

- [x] Convert `src/strategy/staggered_arb_backtest.rs` to directory form
- [x] Move `StaggeredArbBacktestConfig` into `src/strategy/staggered_arb_backtest/config.rs`
- [x] Keep `crate::strategy::staggered_arb_backtest::*` stable via `mod.rs` re-export
- [x] Add a direct config regression test for `with_symbols(...)`
- [x] Re-run build plus focused staggered-arb-backtest validation

## Review (2026-03-06, staggered-arb backtest config split)

- Delivered in this slice:
  - Renamed [`src/strategy/staggered_arb_backtest.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/strategy/staggered_arb_backtest.rs) into directory form at [`src/strategy/staggered_arb_backtest/mod.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/strategy/staggered_arb_backtest/mod.rs).
  - Moved config definitions into [`src/strategy/staggered_arb_backtest/config.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/strategy/staggered_arb_backtest/config.rs).
  - Preserved the existing caller surface for both [`src/cli/strategy/backtest.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/cli/strategy/backtest.rs) and [`src/strategy/staggered_arb_live.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/strategy/staggered_arb_live.rs) by re-exporting `StaggeredArbBacktestConfig` from `mod.rs`.
  - Added a direct regression test that checks `with_symbols(...)` only overrides the symbol list and keeps the expected defaults.
- Validation executed:
  - `cargo test strategy::staggered_arb_backtest -- --nocapture`
  - `cargo build`

- Delivered in this slice:
  - Moved the remaining runtime-heavy `MomentumEngine` implementation from [`src/strategy/momentum/mod.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/strategy/momentum/mod.rs) into [`src/strategy/momentum/engine.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/strategy/momentum/engine.rs).
  - Reduced `mod.rs` to module wiring plus public re-exports for config, positions, signals, event matching, and the engine entry type.
  - Kept private `window_risk` internals scoped to the engine module instead of re-exporting them from the public module surface.
  - Preserved the existing focused test modules by moving them with the engine implementation.
- Validation executed:
  - `cargo build`
  - `cargo test strategy::momentum::config::tests -- --nocapture`
  - `cargo test strategy::momentum::window_risk::tests -- --nocapture`
  - `cargo test strategy::momentum::tests -- --nocapture`

---

## 2026-03-06 Workspace Restructure Slice (Phase 6 CLI backtest runtime split)

- [x] Move strategy CLI backtest runtime helpers out of `src/cli/strategy.rs`
- [x] Keep the `StrategyCommands` clap surface and dispatch behavior stable
- [x] Preserve settlement-mode, verify-run, save, and Gamma verification behavior
- [x] Re-run formatting and compile validation after the extraction

## Review (2026-03-06, CLI backtest runtime split)

- Delivered in this slice:
  - Added [`src/cli/strategy_backtest.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/cli/strategy_backtest.rs) to own the strategy CLI backtest runtime path, including replay execution, saved-run verification, Gamma verification, run diff/list reporting, and kline backfill.
  - Updated [`src/cli/mod.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/cli/mod.rs) to wire the new sibling module into the CLI namespace.
  - Reduced [`src/cli/strategy.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/cli/strategy.rs) to clap command definitions, dispatch, and non-backtest helper logic.
  - Kept `StrategyBacktestMode`, `backtest_directional_signals_pm_settlement`, and `is_market_resolved` in `strategy.rs` so dataset export and settlement scoring do not widen scope in the same commit.
- Validation executed:
  - `cargo build`

---

## 2026-03-06 Workspace Restructure Slice (Phase 6 CLI strategy directory split)

- [x] Convert `src/cli/strategy.rs` into `src/cli/strategy/mod.rs`
- [x] Move the extracted backtest runtime sibling into `src/cli/strategy/backtest.rs`
- [x] Keep `crate::cli::strategy::*` imports and clap dispatch stable
- [x] Fix relative `include_str!` paths after the module move
- [x] Re-run formatting and compile validation after the directory conversion

## Review (2026-03-06, CLI strategy directory split)

- Delivered in this slice:
  - Renamed [`src/cli/strategy.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/cli/strategy.rs) into directory form at [`src/cli/strategy/mod.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/cli/strategy/mod.rs).
  - Renamed [`src/cli/strategy_backtest.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/cli/strategy_backtest.rs) to [`src/cli/strategy/backtest.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/cli/strategy/backtest.rs) and made it a proper child module.
  - Removed the temporary sibling-module wiring from [`src/cli/mod.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/cli/mod.rs).
  - Fixed the moved module's `include_str!` paths for bundled default strategy configs.
- Validation executed:
  - `cargo build`

---

## 2026-03-06 Workspace Restructure Slice (Phase 5.2 directional helper migration)

- [x] Add crate-owned directional backtest helpers under `crates/ploy-backtest/src/strategies/`
- [x] Re-export the directional helper API from `ploy_backtest::strategies`
- [x] Retain `src/strategy/directional_backtest.rs` as the owning engine while delegating pure helper logic to the crate
- [x] Re-run crate-side and app-side directional regression tests

## Review (2026-03-06, directional helper migration)

- Delivered in this slice:
  - Added [`crates/ploy-backtest/src/strategies/directional.rs`](/Users/proerror/Documents/ploy-refactor-integrate/crates/ploy-backtest/src/strategies/directional.rs) with the crate-owned directional config type, closed-trade type, fair-value helpers, Sharpe calculation, and generic result builder.
  - Exported the directional helper surface from [`crates/ploy-backtest/src/strategies/mod.rs`](/Users/proerror/Documents/ploy-refactor-integrate/crates/ploy-backtest/src/strategies/mod.rs).
  - Updated [`src/strategy/directional_backtest.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/strategy/directional_backtest.rs) to keep the engine local to the app while delegating pure helper logic and data types to `ploy_backtest::strategies`.
  - Explicitly did not move the engine itself yet because it still depends on app-local `SpotPrice`, `FeeModel`, and `Direction`.
- Validation executed:
  - `cargo build`
  - `cargo test -p ploy-backtest directional -- --nocapture`
  - `cargo test strategy::directional_backtest::tests -- --nocapture`

---

## 2026-03-06 Workspace Restructure Slice (Phase 5.2 staggered-arb helper migration)

- [x] Add crate-owned staggered-arb trade/result helpers under `crates/ploy-backtest/src/strategies/`
- [x] Re-export the staggered-arb helper API from `ploy_backtest::strategies`
- [x] Keep `StaggeredArbBacktestConfig` and the engine in the app layer so live code stays untouched
- [x] Re-run crate-side and app-side staggered-arb regression tests

## Review (2026-03-06, staggered-arb helper migration)

- Delivered in this slice:
  - Added [`crates/ploy-backtest/src/strategies/staggered_arb.rs`](/Users/proerror/Documents/ploy-refactor-integrate/crates/ploy-backtest/src/strategies/staggered_arb.rs) with the crate-owned closed-trade type, Sharpe calculation, and generic result builder for staggered-arb runs.
  - Exported the staggered-arb helper surface from [`crates/ploy-backtest/src/strategies/mod.rs`](/Users/proerror/Documents/ploy-refactor-integrate/crates/ploy-backtest/src/strategies/mod.rs).
  - Updated [`src/strategy/staggered_arb_backtest.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/strategy/staggered_arb_backtest.rs) to re-export the crate-owned closed-trade type and delegate result aggregation to `ploy_backtest::strategies`.
  - Explicitly kept `StaggeredArbBacktestConfig` in the app crate because [`src/strategy/staggered_arb_live.rs`](/Users/proerror/Documents/ploy-refactor-integrate/src/strategy/staggered_arb_live.rs) still depends on it.
- Validation executed:
  - `cargo build`
  - `cargo test -p ploy-backtest staggered_arb -- --nocapture`
  - `cargo test strategy::staggered_arb_backtest::tests -- --nocapture`

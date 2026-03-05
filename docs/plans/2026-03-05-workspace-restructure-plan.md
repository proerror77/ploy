# Workspace Restructure — Implementation Plan

> Design: [2026-03-05-workspace-restructure-design.md](2026-03-05-workspace-restructure-design.md)
> Branch: `refactor/workspace-restructure`
> Worktree: `../ploy-refactor/`

---

## Phase 0: Cleanup & Preparation
> Goal: Remove dead code, reduce surface area before restructuring.
> Validation: `cargo build && cargo test` pass after each commit.

- [ ] 0.1 Delete dead strategy files
  - [ ] Remove `src/strategy/dump_hedge.rs`
  - [ ] Remove `src/strategy/live_arbitrage.rs`
  - [ ] Remove `src/strategy/strategies/` directory (legacy trait impls)
  - [ ] Remove all re-exports of deleted items from `src/strategy/mod.rs`
  - [ ] `cargo build` — fix any broken references
  - [ ] Commit: `cleanup: remove dead strategy code (dump_hedge, live_arbitrage, strategies/)`

- [ ] 0.2 Delete dead top-level modules
  - [ ] Remove `src/validation.rs` and its `pub mod` in `lib.rs`
  - [ ] Remove `src/platform/agents/` directory (push-based, superseded)
  - [ ] Remove platform/agents re-exports from `src/platform/mod.rs`
  - [ ] `cargo build && cargo test`
  - [ ] Commit: `cleanup: remove dead modules (validation, platform/agents)`

- [ ] 0.3 Remove dead feature flag
  - [ ] Remove `tcn_db` feature from `Cargo.toml`
  - [ ] Remove any `#[cfg(feature = "tcn_db")]` guards
  - [ ] `cargo build`
  - [ ] Commit: `cleanup: remove unused tcn_db feature flag`

- [ ] 0.4 Move claimer to account module
  - [ ] Create `src/account/mod.rs`
  - [ ] Move claimer logic from `src/strategy/claimer.rs` → `src/account/claimer.rs`
  - [ ] Update `lib.rs` to declare `pub mod account`
  - [ ] Update all `use crate::strategy::claimer` → `use crate::account::claimer`
  - [ ] Update CLI references in `main_dispatch.rs`
  - [ ] `cargo build && cargo test`
  - [ ] Commit: `refactor: move claimer from strategy to account module`

**Phase 0 checkpoint**: All tests pass, dead code removed, claimer relocated.

---

## Phase 1: Extract `ploy-core`
> Goal: Create shared types crate with zero runtime dependencies.
> Validation: `cargo build -p ploy-core && cargo test -p ploy-core`

- [ ] 1.1 Scaffold workspace
  - [ ] Convert root `Cargo.toml` to workspace format
  - [ ] Create `crates/ploy-core/Cargo.toml` with minimal deps (serde, rust_decimal, thiserror, chrono)
  - [ ] Create `crates/ploy-core/src/lib.rs`
  - [ ] Add `ploy-core` as dependency of main app
  - [ ] `cargo build` (workspace compiles)
  - [ ] Commit: `build: initialize Cargo workspace with ploy-core crate`

- [ ] 1.2 Extract error types
  - [ ] Copy `src/error.rs` → `crates/ploy-core/src/error.rs`
  - [ ] Update main app to `use ploy_core::error::{PloyError, Result}`
  - [ ] Remove duplicated error types from main app
  - [ ] `cargo build && cargo test`
  - [ ] Commit: `refactor: extract PloyError to ploy-core`

- [ ] 1.3 Extract domain types
  - [ ] Create `crates/ploy-core/src/domain/mod.rs`
  - [ ] Move core types: `Order`, `Position`, `Market`, `FeeModel`, `Decimal` re-exports
  - [ ] Source from: `src/domain/`, `src/strategy/fee_model.rs`, `src/strategy/trading_costs.rs`
  - [ ] Update imports across main app
  - [ ] `cargo build && cargo test`
  - [ ] Commit: `refactor: extract domain types to ploy-core`

- [ ] 1.4 Extract strategy traits
  - [ ] Create `crates/ploy-core/src/strategy/mod.rs`
  - [ ] Move: `Strategy` trait, `StrategyAction`, `StrategyConfig`, `DataFeed` trait, `MarketUpdate`
  - [ ] Source from: `src/strategy/traits.rs`, `src/strategy/feeds.rs` (trait only)
  - [ ] Update imports
  - [ ] `cargo build && cargo test`
  - [ ] Commit: `refactor: extract strategy traits to ploy-core`

- [ ] 1.5 Extract config types
  - [ ] Create `crates/ploy-core/src/config/mod.rs`
  - [ ] Move shared config structs: `DryRunConfig`, `DatabaseConfig`, `LoggingConfig`
  - [ ] Keep strategy-specific configs in main app
  - [ ] `cargo build && cargo test`
  - [ ] Commit: `refactor: extract shared config types to ploy-core`

**Phase 1 checkpoint**: `ploy-core` compiles independently, main app depends on it, all tests pass.

---

## Phase 2: Extract `ploy-polymarket`
> Goal: Isolate Polymarket execution layer into its own crate.
> Validation: `cargo build -p ploy-polymarket && cargo test -p ploy-polymarket`

- [ ] 2.1 Scaffold crate
  - [ ] Create `crates/ploy-polymarket/Cargo.toml`
  - [ ] Deps: `ploy-core`, tokio, reqwest, ethers, hmac, sha2, serde_json
  - [ ] Create `crates/ploy-polymarket/src/lib.rs`
  - [ ] Commit: `build: scaffold ploy-polymarket crate`

- [ ] 2.2 Move signing module
  - [ ] Move `src/signing/` → `crates/ploy-polymarket/src/signing/`
  - [ ] Includes: wallet.rs, auth.rs, hmac.rs, order.rs, nonce_manager.rs
  - [ ] Update imports in main app
  - [ ] `cargo build && cargo test`
  - [ ] Commit: `refactor: move signing module to ploy-polymarket`

- [ ] 2.3 Move Polymarket adapters
  - [ ] Move `src/adapters/polymarket_clob.rs` → `crates/ploy-polymarket/src/clob.rs`
  - [ ] Move `src/adapters/polymarket_ws.rs` → `crates/ploy-polymarket/src/ws.rs`
  - [ ] Move `src/adapters/polymarket_official.rs` → `crates/ploy-polymarket/src/official.rs`
  - [ ] Update imports
  - [ ] `cargo build && cargo test`
  - [ ] Commit: `refactor: move Polymarket adapters to ploy-polymarket`

- [ ] 2.4 Move market discovery
  - [ ] Extract market search/discovery from `src/services/discovery.rs`
  - [ ] Move Polymarket-specific types
  - [ ] `cargo build && cargo test`
  - [ ] Commit: `refactor: move market discovery to ploy-polymarket`

**Phase 2 checkpoint**: Polymarket execution fully isolated, main app uses `ploy_polymarket::*`.

---

## Phase 3: Extract `ploy-data`
> Goal: Isolate read-only data sources.
> Validation: `cargo build -p ploy-data && cargo test -p ploy-data`

- [ ] 3.1 Scaffold crate
  - [ ] Create `crates/ploy-data/Cargo.toml`
  - [ ] Deps: `ploy-core`, tokio, tokio-tungstenite, reqwest, serde
  - [ ] Commit: `build: scaffold ploy-data crate`

- [ ] 3.2 Move Binance data sources
  - [ ] Move `src/adapters/binance_ws.rs` → `crates/ploy-data/src/binance/ws.rs`
  - [ ] Move `src/adapters/binance_kline_ws.rs` → `crates/ploy-data/src/binance/kline.rs`
  - [ ] Move `src/collector/binance_klines.rs` → `crates/ploy-data/src/binance/klines.rs`
  - [ ] Move `src/collector/binance_depth.rs` → `crates/ploy-data/src/binance/depth.rs`
  - [ ] `cargo build && cargo test`
  - [ ] Commit: `refactor: move Binance data sources to ploy-data`

- [ ] 3.3 Move other data sources
  - [ ] Move Deribit IV: `src/adapters/chainlink_rtds.rs` → `crates/ploy-data/src/chainlink/`
  - [ ] Move Kalshi REST: `src/adapters/kalshi_rest.rs` → `crates/ploy-data/src/kalshi/`
  - [ ] Create DataSource trait in `crates/ploy-data/src/traits.rs`
  - [ ] `cargo build && cargo test`
  - [ ] Commit: `refactor: move remaining data sources to ploy-data`

**Phase 3 checkpoint**: All data sources isolated, main app uses `ploy_data::*`.

---

## Phase 4: Extract `ploy-risk`
> Goal: Unify all risk management into one crate.
> Validation: `cargo build -p ploy-risk && cargo test -p ploy-risk`

- [ ] 4.1 Scaffold and move risk modules
  - [ ] Create `crates/ploy-risk/Cargo.toml` (deps: `ploy-core`, rust_decimal)
  - [ ] Move `src/strategy/risk_mgmt/risk.rs` → `crates/ploy-risk/src/risk_manager.rs`
  - [ ] Move `src/strategy/risk_mgmt/slippage.rs` → `crates/ploy-risk/src/slippage.rs`
  - [ ] Move `src/strategy/risk_mgmt/validation.rs` → `crates/ploy-risk/src/validation.rs`
  - [ ] Move `src/coordination/circuit_breaker.rs` → `crates/ploy-risk/src/circuit_breaker.rs`
  - [ ] Remove embedded risk checks from `src/strategy/execution/engine.rs` (delegate to ploy-risk)
  - [ ] `cargo build && cargo test`
  - [ ] Commit: `refactor: unify risk management into ploy-risk crate`

**Phase 4 checkpoint**: Single source of truth for all risk logic.

---

## Phase 5: Extract `ploy-backtest`
> Goal: Unified backtest framework, isolated from live code.
> Validation: `cargo build -p ploy-backtest && cargo test -p ploy-backtest`

- [ ] 5.1 Scaffold and create unified engine
  - [ ] Create `crates/ploy-backtest/Cargo.toml` (deps: `ploy-core`, `ploy-data`, sqlx optional)
  - [ ] Design `BacktestEngine` trait: `fn signal(&self, snapshot: &MarketSnapshot) -> Option<Signal>`
  - [ ] Create shared replay loop, trade simulator, PnL calculator
  - [ ] Commit: `feat: create unified backtest framework in ploy-backtest`

- [ ] 5.2 Migrate backtest strategies
  - [ ] Extract signal logic from `src/strategy/directional_backtest.rs` → `crates/ploy-backtest/src/strategies/directional.rs`
  - [ ] Extract from `src/strategy/staggered_arb_backtest.rs` → `strategies/staggered_arb.rs`
  - [ ] Extract from `src/strategy/liquidity_vacuum_backtest.rs` → `strategies/liquidity_vacuum.rs`
  - [ ] Extract from `src/strategy/garch_probability_backtest.rs` → `strategies/garch_probability.rs`
  - [ ] Extract from `src/strategy/momentum_backtest.rs` → `strategies/momentum.rs`
  - [ ] `cargo build -p ploy-backtest && cargo test -p ploy-backtest`
  - [ ] Commit: `refactor: migrate all backtest strategies to ploy-backtest`

- [ ] 5.3 Move backtest infrastructure
  - [ ] Move `src/strategy/backtest.rs` → `crates/ploy-backtest/src/engine.rs` (merge with 5.1)
  - [ ] Move `src/strategy/backtest_feed.rs` → `crates/ploy-backtest/src/feed.rs`
  - [ ] Move `src/strategy/backtest_recorder.rs` → `crates/ploy-backtest/src/recorder.rs`
  - [ ] Move `src/strategy/backtest_report.rs` → `crates/ploy-backtest/src/report.rs`
  - [ ] Move `src/strategy/execution_sim.rs` → `crates/ploy-backtest/src/execution_sim.rs`
  - [ ] Remove old files from main app
  - [ ] `cargo build && cargo test`
  - [ ] Commit: `refactor: move backtest infrastructure to ploy-backtest`

- [ ] 5.4 Create backtest CLI
  - [ ] Create `crates/ploy-backtest/src/bin/backtest.rs`
  - [ ] Move backtest-related CLI commands from `src/cli/strategy.rs`
  - [ ] `cargo build -p ploy-backtest && cargo test`
  - [ ] Commit: `feat: add standalone backtest CLI binary`

**Phase 5 checkpoint**: All backtests run via `ploy-backtest`, zero backtest code in main app.

---

## Phase 6: Slim Down Main App
> Goal: Split god files, simplify strategy/mod.rs, unify deployment.
> Validation: Full `cargo build && cargo test` + manual dry-run test.

- [ ] 6.1 Split `coordinator/bootstrap.rs` (7.4K lines)
  - [ ] Extract: `bootstrap/config_validation.rs` — config preflight checks
  - [ ] Extract: `bootstrap/service_construction.rs` — DB pool, exchange clients
  - [ ] Extract: `bootstrap/agent_spawning.rs` — agent registration + tokio spawn
  - [ ] Extract: `bootstrap/health_wiring.rs` — health checks, metrics, watchdog
  - [ ] Keep `bootstrap/mod.rs` as orchestrator calling phases in order
  - [ ] `cargo build && cargo test`
  - [ ] Commit: `refactor: split bootstrap.rs into phase-based modules`

- [ ] 6.2 Split `coordinator/coordinator.rs` (6.5K lines)
  - [ ] Extract: `coordinator/handlers/risk.rs` — risk check handler
  - [ ] Extract: `coordinator/handlers/queue.rs` — order queue drain
  - [ ] Extract: `coordinator/handlers/command.rs` — command dispatch
  - [ ] Keep main select! loop in `coordinator.rs`
  - [ ] `cargo build && cargo test`
  - [ ] Commit: `refactor: extract coordinator handlers into separate modules`

- [ ] 6.3 Split `cli/strategy.rs` (6.3K lines)
  - [ ] Create `src/cli/strategy/` directory
  - [ ] Extract: `strategy/start.rs`, `strategy/stop.rs`, `strategy/status.rs`
  - [ ] Extract: `strategy/logs.rs`, `strategy/reload.rs`
  - [ ] Extract: `strategy/accuracy.rs`, `strategy/integrity.rs`
  - [ ] Keep `strategy/mod.rs` as router
  - [ ] `cargo build && cargo test`
  - [ ] Commit: `refactor: split cli/strategy.rs into per-command modules`

- [ ] 6.4 Split `strategy/momentum.rs` (3.8K lines)
  - [ ] Create `src/strategy/momentum/` directory
  - [ ] Extract: `momentum/config.rs`, `momentum/detector.rs`, `momentum/engine.rs`
  - [ ] Extract: `momentum/exit_manager.rs`, `momentum/position.rs`
  - [ ] Keep `momentum/mod.rs` with re-exports
  - [ ] `cargo build && cargo test`
  - [ ] Commit: `refactor: split momentum.rs into modular directory`

- [ ] 6.5 Slim down `strategy/mod.rs`
  - [ ] Remove all backtest re-exports (now in ploy-backtest)
  - [ ] Remove dead code re-exports
  - [ ] Group remaining exports by domain
  - [ ] Target: <30 pub use statements (down from 107)
  - [ ] `cargo build && cargo test`
  - [ ] Commit: `refactor: reduce strategy/mod.rs exports from 107 to <30`

- [ ] 6.6 Deprecate strategy CLI mode
  - [ ] Remove `ploy strategy start/stop` commands
  - [ ] All live strategies go through `ploy platform start`
  - [ ] Keep `ploy strategy status/logs` as read-only inspection
  - [ ] Update CLI help text
  - [ ] `cargo build && cargo test`
  - [ ] Commit: `refactor: deprecate individual strategy CLI, unify to platform mode`

**Phase 6 checkpoint**: No file >2K lines, strategy/mod.rs clean, single deployment model.

---

## Phase 7: Deployment Consolidation
> Goal: 2 service templates, base+overlay env pattern.

- [ ] 7.1 Create base env template
  - [ ] Create `deployment/env.base.example` (coordinator, DB, risk budgets, logging)
  - [ ] Create `deployment/env.crypto.example` (crypto-specific overrides)
  - [ ] Create `deployment/env.sports.example` (sports-specific overrides)
  - [ ] Commit: `deploy: create base+overlay env template pattern`

- [ ] 7.2 Consolidate service files
  - [ ] Create `deployment/ploy-platform.service` (template, parameterized)
  - [ ] Create `deployment/ploy-collector.service` (data collection)
  - [ ] Remove 10 old service files
  - [ ] Update deployment docs
  - [ ] Commit: `deploy: consolidate 12 service files into 2 templates`

- [ ] 7.3 Add config validation
  - [ ] Add `ploy platform validate` command — preflight config check
  - [ ] Validate: TOML syntax, DB reachable, required env vars present
  - [ ] `cargo build && cargo test`
  - [ ] Commit: `feat: add platform config validation command`

**Phase 7 checkpoint**: Deployment is simple — one service template, one base env, domain overlays.

---

## Phase 8: Final Verification
> Goal: Everything works end-to-end.

- [ ] 8.1 Full test suite
  - [ ] `cargo test --workspace` — all crates pass
  - [ ] `cargo build --release` — release build succeeds
  - [ ] `cargo clippy --workspace` — no warnings

- [ ] 8.2 Integration test
  - [ ] Dry-run staggered_arb via platform mode
  - [ ] Run a backtest via `ploy-backtest` binary
  - [ ] Verify TUI dashboard works

- [ ] 8.3 Documentation
  - [ ] Update root README with new workspace structure
  - [ ] Update CLAUDE.md with new module layout
  - [ ] Document deployment guide

- [ ] 8.4 Merge
  - [ ] PR to main with full diff review
  - [ ] Squash-merge or merge commit (preserve phase history)

---

## Execution Order & Dependencies

```
Phase 0 (cleanup)
    ↓
Phase 1 (ploy-core)        ← foundation, everything depends on this
    ↓
Phase 2 (ploy-polymarket)  ← can parallel with Phase 3
Phase 3 (ploy-data)        ← can parallel with Phase 2
    ↓
Phase 4 (ploy-risk)        ← depends on ploy-core
    ↓
Phase 5 (ploy-backtest)    ← depends on ploy-core + ploy-data
    ↓
Phase 6 (slim main app)    ← depends on all extractions done
    ↓
Phase 7 (deployment)       ← depends on Phase 6
    ↓
Phase 8 (verification)     ← final
```

## Risk Mitigation

- **Each phase ends with `cargo build && cargo test`** — no silent breakage
- **Phases 2 & 3 can run in parallel** (no shared files)
- **Phase 0 is low-risk** — only deletes confirmed dead code
- **Phase 1 is the riskiest** — touches every file's imports; do it carefully
- **Git tags at each phase checkpoint** for easy rollback

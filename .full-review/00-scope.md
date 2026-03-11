# Review Scope

## Target

Branch `hotfix/staggered-arb-release-20260306` vs `main` — 10 commits, ~317 source files changed.

The branch represents a major **coordinator refactor**: the monolithic `bootstrap.rs` (~7,000 lines) and `coordinator.rs` (~6,500 lines) have been decomposed into focused sub-modules. Key themes:

1. **Coordinator decomposition** — `src/coordinator/` split into: `admission/`, `capital/`, `journal/`, `position/`, `queue/`, `risk/`, `strategy_runtime/`, `governance.rs`
2. **Bootstrap decomposition** — `src/coordinator/bootstrap/` split into: `bootstrap_config/`, `coordinator_bootstrap.rs`, `crypto_runtime_support/`, `managed_crypto/`, `runtime_orchestration.rs`, `runtime_spawns.rs`, `strategy_deployments.rs`
3. **New strategies** — `src/strategy/pm_5m_directional.rs`, `src/strategy/pm_5m_directional_backtest.rs`, `src/strategy/staggered_arb_live/`
4. **Control plane** — new `src/control_plane/` module
5. **CLI routing** — foreground intents now routed through coordinator ingress
6. **Staggered arb live** — new live trading strategy with entry, leg2, lifecycle, order_updates, runtime_flow

## Files

### Core Coordinator (new sub-modules)
- `src/coordinator/admission.rs` + `admission/deployments.rs` + `admission/duplicate_guard.rs`
- `src/coordinator/capital.rs` + `capital/crypto/` + `capital/market/`
- `src/coordinator/journal.rs` + `journal/restore.rs`
- `src/coordinator/position.rs` + `position/transitions.rs`
- `src/coordinator/queue.rs`
- `src/coordinator/risk.rs` + `risk/checks.rs` + `risk/config.rs` + `risk/exposure.rs` + `risk/queries.rs` + `risk/stats.rs` + `risk/transitions.rs` + `risk/types.rs`
- `src/coordinator/strategy_runtime.rs` + `strategy_runtime/actions.rs` + `strategy_runtime/control.rs` + `strategy_runtime/observability.rs` + `strategy_runtime/order_store.rs` + `strategy_runtime/session.rs` + `strategy_runtime/setup.rs`
- `src/coordinator/governance.rs`
- `src/coordinator/coordinator/control_surface.rs` + `execution.rs` + `ingress.rs` + `order_updates.rs` + `recovery.rs` + `runtime_status.rs` + `tests.rs`

### Bootstrap (decomposed)
- `src/coordinator/bootstrap/bootstrap_config.rs` + `bootstrap_config/coordinator_env.rs` + `bootstrap_config/crypto_env.rs`
- `src/coordinator/bootstrap/coordinator_bootstrap.rs`
- `src/coordinator/bootstrap/crypto_runtime_support/` (market_data_runtime, market_discovery, preflight)
- `src/coordinator/bootstrap/managed_crypto/` (config, env)
- `src/coordinator/bootstrap/runtime_orchestration.rs` + `runtime_spawns.rs` + `strategy_deployments.rs`
- `src/coordinator/bootstrap/startup_context.rs` + `tests.rs`

### New Strategies
- `src/strategy/staggered_arb_live/` (entry, leg2, lifecycle, order_updates, runtime_flow, tests)
- `src/strategy/pm_5m_directional.rs`
- `src/strategy/pm_5m_directional_backtest.rs`
- `config/strategies/pm_5m_directional_default.toml`

### Control Plane
- `src/control_plane/` (new module)

### CLI / API
- `src/cli/strategy/runtime_ops/foreground.rs` + `foreground_submit.rs`
- `src/api/handlers/sidecar/` (grok_decision, ingress, read_side, write_side, types, tests)

### Modified Adapters
- `src/adapters/binance_ws.rs` + `binance_ws/runtime.rs`
- `src/adapters/polymarket_ws.rs` + `polymarket_ws/connection.rs` + `polymarket_ws/runtime_support.rs`

### Modified Strategies
- `src/strategy/adapters/momentum_adapter.rs` + `split_arb_adapter.rs` + `split_arb_adapter/runtime_support.rs`
- `src/strategy/claimer.rs` + `claimer/claim_flow.rs`
- `src/strategy/execution/engine/round_flow.rs` + `tests.rs`
- `src/strategy/execution/executor/execution_flow.rs`
- `src/strategy/gamma_scalping/strategy.rs` + `strategy/decision_flow.rs`
- `src/strategy/manager.rs` + `src/strategy/mod.rs`
- `src/strategy/momentum/entry_runtime.rs`
- `src/strategy/multi_outcome.rs`
- `src/strategy/nba_comeback/core/positioning.rs` + `strategy/opportunity_flow.rs`
- `src/strategy/pattern_memory/decision_runtime.rs` + `strategy.rs`
- `src/strategy/runtime_specs/deployment_matrix.rs` + `runtime_configs.rs` + `runtime_plans.rs`

## Flags

- Security Focus: no
- Performance Critical: yes (live trading system)
- Strict Mode: no
- Framework: Rust / Tokio / Axum / sqlx (PostgreSQL)

## Review Phases

1. Code Quality & Architecture
2. Security & Performance
3. Testing & Documentation
4. Best Practices & Standards
5. Consolidated Report

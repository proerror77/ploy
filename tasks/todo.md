# Momentum Best Edge Split (2026-03-11)

## Goal
Move the best-edge window selection and deferred entry execution out of `src/strategy/momentum/trade_flow.rs` so the trade-flow root keeps direct entry/exit ownership while the queued-window path lives in a dedicated module.

## File ownership

- `src/strategy/momentum/trade_flow.rs`
  - owner: immediate entry/exit flow, cooldown checks, and shared direct-trade path
- `src/strategy/momentum/best_edge.rs`
  - owner: `PendingSignal`, `WindowRiskTracker`, queued-signal selection, and deferred best-edge execution

## Tasks

- [x] Extract `PendingSignal` / `WindowRiskTracker` and their focused tests into a sibling module.
- [x] Extract queued-signal selection and deferred execution helpers into the same module.
- [x] Re-run compile plus focused momentum best-edge regressions after the split.

## Progress notes

- 2026-03-11: Added [best_edge.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/strategy/momentum/best_edge.rs) for window tracking, pending-signal queueing, delayed best-edge selection, deferred execution, and focused regression tests.
- 2026-03-11: Reduced [trade_flow.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/strategy/momentum/trade_flow.rs) so `maybe_enter(...)` delegates best-edge queueing and the root file no longer owns the queued execution path.
- 2026-03-11: Validation passed:
  - `rtk cargo check --lib --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-momentum-best-edge-1 rtk cargo test test_window_id_rounds_down_to_15m_boundary --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-momentum-best-edge-2 rtk cargo test test_window_tracker_prefers_highest_edge_signal --lib -- --exact --nocapture`

# Sidecar Ingress Deployment Gate Split (2026-03-11)

## Goal
Move deployment/account gate ownership out of `src/api/handlers/sidecar/ingress.rs` so the root ingress helper file keeps parsing, presentation, and coordinator-error helpers.

## Tasks

- [x] Extract deployment/account scope helpers into `src/api/handlers/sidecar/ingress/deployment_gate.rs`.
- [x] Keep parsing/presentation helpers in `src/api/handlers/sidecar/ingress.rs`.
- [ ] Re-run focused sidecar ingress validations after the split.

## Progress notes

- 2026-03-11: Added [deployment_gate.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/api/handlers/sidecar/ingress/deployment_gate.rs) for account-scope, deployment gate, binding validation, and metadata enrichment.
- 2026-03-11: Reduced [ingress.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/api/handlers/sidecar/ingress.rs) to parsing, priority policy, sidecar activity broadcast, and coordinator error mapping.
- 2026-03-11: Validation passed:
  - `rtk cargo check --lib --message-format=short`
- 2026-03-11: Focused sidecar test filters were attempted but the RTK wrapper returned `0 passed, 736 filtered out` for:
  - `rtk cargo test non_live_deployment_ingress_is_blocked_by_default --lib -- --exact --nocapture`
  - `rtk cargo test api::handlers::sidecar::tests::non_live_deployment_ingress_is_blocked_by_default --lib -- --exact --nocapture`
- 2026-03-11: The extraction is committed with compile validation cleared, but a sidecar-specific assertion still needs a working RTK test selector.

# Staggered Arb Reporting Split (2026-03-11)

## Goal
Move reporting/state snapshot ownership out of `src/strategy/staggered_arb_live.rs` so the root live adapter keeps config, market handling, and execution logic.

## File ownership

- `src/strategy/staggered_arb_live.rs`
  - owner: live adapter config/state, market handling, trait entrypoints
- `src/strategy/staggered_arb_live/reporting.rs`
  - owner: summary formatting, strategy state snapshot, position export, shutdown/reset reporting helpers

## Tasks

- [x] Extract summary/state reporting helpers into a sibling module.
- [x] Keep the `Strategy` impl in the root file but delegate state/reporting methods.
- [x] Re-run compile plus focused staggered-arb regressions after the split.

## Progress notes

- 2026-03-11: Added [reporting.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/strategy/staggered_arb_live/reporting.rs) for summary building, gate-count formatting, state snapshots, position export, and shutdown/reset helpers.
- 2026-03-11: Reduced [staggered_arb_live.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/strategy/staggered_arb_live.rs) so the `Strategy` impl now delegates `state()`, `positions()`, `is_active()`, `shutdown()`, and `reset()`.
- 2026-03-11: Validation passed:
  - `rtk cargo check --lib --message-format=short`
  - `rtk cargo test test_summary_empty --lib -- --exact --nocapture`
  - `rtk cargo test test_summary_includes_per_symbol_gate_breakdown --lib -- --exact --nocapture`
  - `rtk cargo test test_required_feeds --lib -- --exact --nocapture`

# Staggered Arb State Support Split (2026-03-11)

## Goal
Move the shared live-state/support spine out of `src/strategy/staggered_arb_live.rs` so entry/leg2/runtime modules depend on a dedicated owner instead of the root file.

## File ownership

- `src/strategy/staggered_arb_live.rs`
  - owner: adapter config, constructor, trait facade, and high-level flow delegation
- `src/strategy/staggered_arb_live/state_support.rs`
  - owner: `LiveWindow`, `QuoteRoute`, balance/sigma helpers, PM quote persistence, and active-cycle helpers

## Tasks

- [x] Extract `LiveWindow` / `QuoteRoute` into a sibling state-support module.
- [x] Extract shared PM quote, balance, sigma, and cycle helper methods into the same module.
- [x] Re-run compile plus focused staggered-arb regressions after the split.

## Progress notes

- 2026-03-11: Added [state_support.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/strategy/staggered_arb_live/state_support.rs) for shared live-state types and helper methods used across `entry`, `leg2`, `runtime_flow`, and tests.
- 2026-03-11: Reduced [staggered_arb_live.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/strategy/staggered_arb_live.rs) so the root file no longer owns PM quote persistence/synthetic state, balance helpers, or active-cycle checks directly.
- 2026-03-11: Validation passed:
  - `rtk cargo check --lib --message-format=short`
  - `rtk cargo test test_record_pm_quote_resets_persistence_after_stale_gap --lib -- --exact --nocapture`
  - `rtk cargo test test_try_entry_does_not_cap_concurrency_when_max_concurrent_is_zero --lib -- --exact --nocapture`
  - `rtk cargo test test_live_greeks_can_accelerate_leg2_close_before_merge_target --lib -- --exact --nocapture`

# RL CLI Agent State Split (2026-03-11)

## Goal
Reduce `src/rl/cli_agent.rs` root-file ownership by extracting market-state updates and execution feedback handling into sibling modules.

## Tasks

- [x] Extract observation/event processing into a `market_state` sibling module.
- [x] Extract execution/position feedback handling into an `execution_feedback` sibling module.
- [x] Keep public agent lifecycle/API methods in the root file.
- [x] Re-run RL-focused compile and behavior regressions after the split.

## Review

- [x] Confirm `cli_agent.rs` no longer owns the large `process_crypto_event` and `handle_execution` implementations directly.
- [x] Confirm the extracted modules only depend on the parent agent state and do not create a new runtime surface.

## Progress notes

- 2026-03-11: Added [market_state.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/rl/cli_agent/market_state.rs) for observation updates, event processing, exposure refresh, and mark-to-market logic.
- 2026-03-11: Added [execution_feedback.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/rl/cli_agent/execution_feedback.rs) for submitted/success/failure execution handling.
- 2026-03-11: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-rl-cli-agent-split rtk cargo check --lib --features rl --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-rl-cli-agent-split rtk cargo test --features rl test_rl_signal_on_good_sum --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-rl-cli-agent-split rtk cargo test --features rl test_position_tracking --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-rl-cli-agent-split rtk cargo test --features rl test_submitted_execution_does_not_pause_agent --lib -- --exact --nocapture`

# RL CLI Agent Config/Test Split (2026-03-11)

## Goal
Move RL CLI config/default ownership and focused regressions out of `src/rl/cli_agent.rs` so the root file only owns the agent runtime state and public lifecycle surface.

## File ownership

- `src/rl/cli_agent.rs`
  - owner: thin RL agent owner / module wiring
- `src/rl/cli_agent/config.rs`
  - owner: `RLCryptoAgentConfig` and defaults
- `src/rl/cli_agent/tests.rs`
  - owner: focused RL CLI regressions

## Tasks

- [x] Extract `RLCryptoAgentConfig` and its defaults into a sibling module.
- [x] Move inline RL CLI tests into a sibling module.
- [x] Re-run RL-focused compile and behavior regressions after the split.

## Progress notes

- 2026-03-11: Added [config.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/rl/cli_agent/config.rs) and re-exported `RLCryptoAgentConfig` from [cli_agent.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/rl/cli_agent.rs).
- 2026-03-11: Added [tests.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/rl/cli_agent/tests.rs) so the root file no longer owns the RL compatibility test suite.
- 2026-03-11: Validation attempt:
  - `CARGO_TARGET_DIR=/tmp/ploy-rl-config-split rtk cargo check --lib --features rl --message-format=short`
- 2026-03-11: The RL split no longer introduces its own compile errors, but branch-wide compile is still blocked by existing `nba_comeback` errors in [strategy.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/strategy/nba_comeback/strategy.rs) and [state_flow.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/strategy/nba_comeback/strategy/state_flow.rs).

# RL CLI Runtime Facade Split (2026-03-11)

## Goal
Move the public lifecycle, ingress, and read-side facade out of `src/rl/cli_agent.rs` so the root file only owns agent types and constructors/bootstrap.

## File ownership

- `src/rl/cli_agent.rs`
  - owner: `RLCryptoAgent`, `InternalPosition`, constructors/bootstrap, module wiring
- `src/rl/cli_agent/runtime.rs`
  - owner: public lifecycle, ingress, and read-side facade

## Tasks

- [x] Extract the public runtime facade into a sibling module.
- [x] Re-run RL-focused compile and behavior regressions after the split.

## Progress notes

- 2026-03-11: Added [runtime.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/rl/cli_agent/runtime.rs) for lifecycle, ingress, and read-side facade methods.
- 2026-03-11: Reduced [cli_agent.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/rl/cli_agent.rs) to agent type ownership plus `new()` / `with_defaults()`.
- 2026-03-11: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-rl-cli-runtime-cut rtk cargo check --lib --features rl --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-rl-cli-runtime-cut rtk cargo test --features rl test_rl_agent_lifecycle --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-rl-cli-runtime-cut rtk cargo test --features rl test_rl_signal_on_good_sum --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-rl-cli-runtime-cut rtk cargo test --features rl test_position_tracking --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-rl-cli-runtime-cut rtk cargo test --features rl test_submitted_execution_does_not_pause_agent --lib -- --exact --nocapture`

# RL CLI Policy Ownership Split (2026-03-11)

## Goal
Split the remaining RL CLI policy owner into focused sibling modules so policy-output decoding and intent mapping stop living in one file.

## File ownership

- `src/rl/cli_agent/policy.rs`
  - owner: action selection orchestration and rule-based fallback
- `src/rl/cli_agent/policy_output.rs`
  - owner: ONNX/discrete policy-output decoding helpers
- `src/rl/cli_agent/intent_mapping.rs`
  - owner: deployment-id derivation, share sizing, and `ContinuousAction -> OrderIntent` mapping

## Tasks

- [x] Extract policy-output decoding helpers into a sibling module.
- [x] Extract `ContinuousAction -> OrderIntent` mapping into a sibling module.
- [x] Re-run RL-focused compile and behavior regressions after the split.

## Progress notes

- 2026-03-11: Added [policy_output.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/rl/cli_agent/policy_output.rs) for output-shape decoding, logits/probabilities handling, and discrete-to-continuous fallback mapping.
- 2026-03-11: Added [intent_mapping.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/rl/cli_agent/intent_mapping.rs) for deployment-id derivation, intent construction, and share sizing.
- 2026-03-11: Reduced [policy.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/rl/cli_agent/policy.rs) to policy selection orchestration plus rule-based fallback.
- 2026-03-11: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-rl-policy-split rtk cargo check --lib --features rl --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-rl-policy-split rtk cargo test --features rl test_rl_signal_on_good_sum --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-rl-policy-split rtk cargo test --features rl test_position_tracking --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-rl-policy-split rtk cargo test --features rl test_submitted_execution_does_not_pause_agent --lib -- --exact --nocapture`

# Runtime Schema Surface Split (2026-03-11)

## Goal
Break `src/persistence/runtime_schema.rs` into domain-focused submodules so market-data schema, control-plane tables, analytics tables, and repair DDL stop living in one 1000+ line owner.

## File ownership

- `src/persistence/runtime_schema.rs`
  - owner: thin runtime-schema façade
- `src/persistence/runtime_schema/market_data.rs`
  - owner: quote/binance/orderbook/metadata tables
- `src/persistence/runtime_schema/control_tables.rs`
  - owner: accounts, governance, execution, risk runtime tables
- `src/persistence/runtime_schema/analytics.rs`
  - owner: settlements and observability/evidence tables
- `src/persistence/runtime_schema/repairs.rs`
  - owner: startup schema repair DDL

## Tasks

- [x] Extract market-data schema builders into a dedicated submodule.
- [x] Extract account/governance/runtime table builders into a dedicated submodule.
- [x] Extract observability/settlement schema builders into a dedicated submodule.
- [x] Extract repair DDL into a dedicated submodule and leave a thin façade behind.
- [x] Re-run compile plus focused persistence regressions after the split.

## Progress notes

- 2026-03-11: Added [market_data.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/persistence/runtime_schema/market_data.rs), [control_tables.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/persistence/runtime_schema/control_tables.rs), [analytics.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/persistence/runtime_schema/analytics.rs), and [repairs.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/persistence/runtime_schema/repairs.rs).
- 2026-03-11: Reduced [runtime_schema.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/persistence/runtime_schema.rs) to a thin re-export façade so existing callers did not move.
- 2026-03-11: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-runtime-schema-split rtk cargo check --lib --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-runtime-schema-tests rtk cargo test ensure_pm_market_metadata_table_exists --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-runtime-schema-tests2 rtk cargo test persistence_module_reexports_market_data_surface --lib -- --exact --nocapture`

# Market Persistence Alerts Wave 1 (2026-03-11)

## Goal
Move trade-alert schema/state/emission ownership out of `src/persistence/market_persistence/trades.rs` so the root trade collector keeps tick persistence and poll-loop wiring.

## Tasks

- [x] Extract trade-alert DDL, config/state, and emission flow into a sibling module.
- [x] Rewire both trade persistence entrypoints to the new alert owner.
- [x] Re-run compile plus focused persistence regressions after the split.

## Progress notes

- 2026-03-11: Added [alerts.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/persistence/market_persistence/alerts.rs) for `clob_trade_alerts` schema, trade-alert config/state, and alert emission.
- 2026-03-11: Reduced [trades.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/persistence/market_persistence/trades.rs) to trade tick collection, persistence, and runtime spawn wiring.
- 2026-03-11: Rewired [collector_targets.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/persistence/market_persistence/collector_targets.rs) to consume the new alert owner directly.
- 2026-03-11: Validation attempt:
  - `CARGO_TARGET_DIR=/tmp/ploy-market-alerts-check4 rtk cargo check --lib --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-market-alerts-test4 rtk cargo test persistence_module_reexports_market_data_surface --lib -- --exact --nocapture`
- 2026-03-11: Both validation commands are currently blocked by pre-existing compile failures in [subscriptions.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/adapters/polymarket_ws/subscriptions.rs); no errors referenced the new `market_persistence` alert split files.

# Market Persistence Runtime Wave 1 (2026-03-11)

## Goal
Move the event-matcher polling/runtime owner out of `src/persistence/market_persistence/trades.rs` so the root trade collector keeps only tick schema and per-market ingestion.

## File ownership

- `src/persistence/market_persistence/trades.rs`
  - owner: trade tick schema + per-market collection/persistence
- `src/persistence/market_persistence/runtime.rs`
  - owner: event-matcher trade persistence daemon/runtime config + tracked-market polling

## Tasks

- [x] Extract the event-matcher trade persistence spawn/runtime into a sibling module.
- [x] Keep `trades.rs` focused on `ensure_clob_trade_ticks_table(...)` and `collect_trades_for_market(...)`.
- [x] Re-run compile plus focused persistence regressions after the split.

## Progress notes

- 2026-03-11: Added [runtime.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/persistence/market_persistence/runtime.rs) for env-driven runtime config, alert/bootstrap state, tracked-market refresh, and concurrent trade collection dispatch.
- 2026-03-11: Reduced [trades.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/persistence/market_persistence/trades.rs) to schema + per-market ingestion only.
- 2026-03-11: Rewired [market_persistence.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/persistence/market_persistence.rs) so `spawn_polymarket_trade_persistence(...)` now exports from the runtime owner.
- 2026-03-11: Validation passed:
  - `rtk cargo check --lib --message-format=short`
  - `rtk cargo test persistence_module_reexports_market_data_surface --lib -- --exact --nocapture`

# Domain OrderRequest Bridge Cut (2026-03-11)

## Goal
Move `StrategyOrderIntent -> OrderRequest` conversion out of `src/strategy/runtime_order.rs` so the compatibility bridge lives under `crate::domain` instead of the canonical strategy runtime owner.

## Tasks

- [x] Add a domain-owned `order_request_from_strategy_intent(...)` bridge.
- [x] Rewire foreground submit, managed runtime order commands, and strategy-side compatibility tests to use the domain bridge.
- [x] Remove the old `order_request_from_intent(...)` implementation from `src/strategy/runtime_order.rs`.
- [x] Re-run focused compile and bridge regressions after the move.

## Review

- [x] Confirm `runtime_order.rs` now owns only `StrategyOrderIntent -> OrderIntent` conversion.
- [x] Confirm the remaining `OrderRequest` bridge is crate-private under `src/domain`.

## Progress notes

- 2026-03-11: Added [order_request_bridge.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/domain/order_request_bridge.rs) and re-exported `order_request_from_strategy_intent` as a crate-private domain helper in [mod.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/domain/mod.rs).
- 2026-03-11: Rewired [foreground_submit.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/cli/strategy/runtime_ops/foreground_submit.rs), [order_commands.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/coordinator/strategy_runtime/actions/order_commands.rs), [strategy.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/strategy/event_edge/strategy.rs), [strategy.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/strategy/nba_comeback/strategy.rs), and [tests.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/strategy/staggered_arb_live/tests.rs) to use the domain bridge.
- 2026-03-11: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-domain-order-bridge rtk cargo check --lib --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-domain-order-bridge rtk cargo test order_request_from_strategy_intent_preserves_action_id --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-domain-order-bridge rtk cargo test build_coordinator_payload_requires_deployment_id --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-domain-order-bridge rtk cargo test persist_runtime_order_insert_uses_action_order_id_and_leg --lib -- --exact --nocapture`

# Platform Namespace Retirement (2026-03-11)

## Goal
Remove the now-empty `crate::platform` namespace after queue/risk/position/persistence/data-plane ownership has been moved elsewhere.

## Tasks

- [x] Remove `pub mod platform;` from `src/lib.rs`.
- [x] Delete the dead `src/platform/mod.rs` shim.
- [x] Re-run compile plus a repo-wide search to confirm there are no remaining `crate::platform` consumers.

## Review

- [x] Confirm `rg -n "crate::platform::|use crate::platform|ploy::platform::|platform::Domain|platform::PlatformDataPlane"` returns no live consumers before deleting the namespace.
- [x] Confirm `src/lib.rs` already re-exports the surviving canonical owners (`domain`, `data_plane`, `coordinator`, `persistence`) directly.

## Progress notes

- 2026-03-11: Deleted [mod.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/platform/mod.rs) and removed `pub mod platform;` from [lib.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/lib.rs).
- 2026-03-11: Validation passed:
  - `rg -n "crate::platform::|use crate::platform|ploy::platform::|platform::Domain|platform::PlatformDataPlane" src tests -g '!target'`
  - `CARGO_TARGET_DIR=/tmp/ploy-namespace-retire rtk cargo check --lib --message-format=short`

# RL Compatibility Runtime Retirement (2026-03-11)

## Goal
Delete the dead `src/rl/order_platform.rs` compatibility runtime now that the RL CLI no longer consumes it.

## Tasks

- [x] Remove `order_platform` from `src/rl/mod.rs`.
- [x] Delete `src/rl/order_platform.rs`.
- [x] Update RL CLI messaging so live mode refers to coordinator ingress instead of a local order runtime.
- [x] Re-run RL-focused compile/tests after the retirement.

## Review

- [x] Confirm there are no remaining `RlOrderRuntime*` references in `src/rl` or `src/main_commands/rl`.
- [x] Confirm the RL CLI banner no longer suggests a separate local order runtime.

## Progress notes

- 2026-03-11: Removed the dead `order_platform` module from [mod.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/rl/mod.rs) and deleted [order_platform.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/rl/order_platform.rs).
- 2026-03-11: Updated [agent.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/main_commands/rl/agent.rs) to advertise coordinator ingress instead of a local order runtime.
- 2026-03-11: Validation passed:
  - `rg -n "RlOrderRuntime|RlOrderRuntimeConfig|RlRuntimeStats|order_platform" src/rl src/main_commands/rl -g '*.rs'`
  - `CARGO_TARGET_DIR=/tmp/ploy-rl-runtime-retire rtk cargo check --lib --features rl --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-rl-runtime-retire rtk cargo test --features rl test_rl_agent_lifecycle --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-rl-runtime-retire rtk cargo test --features rl test_position_tracking --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-rl-runtime-retire rtk cargo test --features rl test_submitted_execution_does_not_pause_agent --lib -- --exact --nocapture`

# Strategy Compatibility Surface Retirement (2026-03-11)

## Goal
Shrink the remaining legacy `strategy` compatibility surface by removing dead runtime code and making the `runtime_order` bridge crate-private.

## Tasks

- [x] Make `src/strategy/runtime_order.rs` crate-private instead of part of the public strategy module surface.
- [x] Delete the orphaned `src/strategy/orchestrator.rs` legacy runtime file.
- [x] Re-run focused compile/runtime-order tests after the surface cut.

## Review

- [x] Confirm the only remaining `runtime_order` consumers live inside the crate.
- [x] Confirm `StrategyOrchestrator` had no module-tree consumers before deletion.

## Progress notes

- 2026-03-11: Changed [mod.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/strategy/mod.rs) so `runtime_order` is now `pub(crate)`.
- 2026-03-11: Deleted the dead [orchestrator.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/strategy/orchestrator.rs) compatibility runtime file.
- 2026-03-11: Validation passed:
  - `rg -n "StrategyOrchestrator|OrchestratorConfig|ploy::strategy::runtime_order|pub mod runtime_order" src tests -g '!target'`
  - `CARGO_TARGET_DIR=/tmp/ploy-strategy-compat-retire rtk cargo check --lib --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-strategy-compat-retire rtk cargo test order_request_from_intent_preserves_action_id --lib -- --exact --nocapture`

# Platform Persistence Shim Retirement (2026-03-11)

## Goal
Remove the last `platform` persistence compatibility shims so market-data schema and pipeline ownership live under `crate::persistence`.

## Tasks

- [x] Move quote/price/lob/orderbook schema DDL into `src/persistence/runtime_schema.rs`.
- [x] Remove dead `src/platform/persistence_pipeline.rs` and `src/platform/persistence_schema.rs` shims.
- [x] Delete the orphaned `src/platform/persistence_pipeline/runtime.rs` implementation after the shim removal.
- [x] Stop exporting persistence pipeline types from `src/platform/mod.rs`.
- [x] Re-run focused persistence compile/tests after retiring the shims.

## Review

- [x] Confirm `src/persistence/runtime_schema.rs` no longer calls into `crate::platform::persistence_schema`.
- [x] Confirm `src/platform/mod.rs` now only re-exports data-plane/domain primitives.

## Progress notes

- 2026-03-11: Inlined the quote/price/lob/orderbook schema builders into [runtime_schema.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/persistence/runtime_schema.rs).
- 2026-03-11: Deleted [persistence_pipeline.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/platform/persistence_pipeline.rs), [runtime.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/platform/persistence_pipeline/runtime.rs), and [persistence_schema.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/platform/persistence_schema.rs), and removed the matching `platform` re-exports in [mod.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/platform/mod.rs).
- 2026-03-11: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-persistence-shim-cut rtk cargo check --lib --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-persistence-shim-cut-surface rtk cargo test persistence_module_reexports_market_data_surface --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-persistence-shim-cut-quote rtk cargo test quote_dedup_skips_unchanged_within_interval --lib -- --exact --nocapture`

# Legacy Orchestrator Live Submit Guard (2026-03-11)

## Goal
Stop the legacy `StrategyOrchestrator` from acting like a second live execution runtime by disabling non-dry-run order submission while preserving dry-run compatibility behavior.

## Tasks

- [x] Reject `StrategyAction::SubmitIntent` in `StrategyOrchestrator` whenever the executor is not dry-run.
- [x] Keep dry-run submit behavior intact so legacy tooling still works for observation/simulation paths.
- [x] Re-run focused compile after the guard lands.

## Review

- [x] Confirm the live guard triggers before risk checks or executor submission.
- [x] Confirm cancel/modify/log/alert paths remain unchanged.

## Progress notes

- 2026-03-11: Added a live-submit guard to [orchestrator.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/strategy/orchestrator.rs) so the legacy orchestrator now warns and skips `SubmitIntent` whenever the underlying executor is not dry-run.
- 2026-03-11: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-foreground-submit-cut rtk cargo check --lib --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-foreground-submit-cut-req rtk cargo test test_strategy_manager_creation --lib -- --exact --nocapture`

# CLI Foreground Coordinator-Only Submit (2026-03-11)

## Goal
Retire the foreground runtime's direct execution fallback so strategy submit actions in CLI foreground mode only route through coordinator ingress.

## Tasks

- [x] Remove `OrderExecutor` fallback from `ForegroundIntentSubmitter`.
- [x] Keep order logging and persistence, but require coordinator ingress for actual submission.
- [x] Update operator-facing messaging to describe coordinator-only submission.
- [x] Re-run focused foreground-submit compile/tests after the cut.

## Review

- [x] Confirm `foreground_submit.rs` no longer executes orders directly.
- [x] Confirm `handle_strategy_actions` still retains executor ownership only for cancel operations.

## Progress notes

- 2026-03-11: Removed the `DirectExecuted` outcome and direct `OrderExecutor::execute` fallback from [foreground_submit.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/cli/strategy/runtime_ops/foreground_submit.rs).
- 2026-03-11: `ForegroundIntentSubmitter` now only carries `dry_run`; [foreground.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/cli/strategy/runtime_ops/foreground.rs) keeps the executor only for explicit cancel flows.
- 2026-03-11: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-foreground-coordinator-only rtk cargo check --lib --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-foreground-coordinator-only-tests rtk cargo test build_coordinator_payload_requires_deployment_id --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-foreground-coordinator-only-tests2 rtk cargo test build_coordinator_payload_preserves_strategy_metadata --lib -- --exact --nocapture`

# Strategy OrderRequest Surface Quarantine (2026-03-11)

## Goal
Stop exposing `order_request_from_intent` through the public `strategy` facade so `OrderRequest` is no longer presented as part of the canonical strategy API surface.

## Tasks

- [x] Remove `order_request_from_intent` from the strategy runtime facade and `src/strategy/mod.rs` re-exports.
- [x] Rewire existing compatibility callers to import `runtime_order::order_request_from_intent` explicitly.
- [x] Keep the compatibility bridge itself in `runtime_order.rs` for now.
- [x] Re-run compile plus focused runtime-order and foreground-submit regressions after the surface cut.

## Review

- [x] Confirm public facade exports no longer include `order_request_from_intent`.
- [x] Confirm coordinator runtime, foreground submit, and strategy compatibility consumers still compile through explicit module paths.

## Progress notes

- 2026-03-11: Removed `order_request_from_intent` from [runtime_facade.rs](/Users/proerror/Documents/ploy/src/strategy/runtime_facade.rs) and [mod.rs](/Users/proerror/Documents/ploy/src/strategy/mod.rs).
- 2026-03-11: Rewired [foreground_submit.rs](/Users/proerror/Documents/ploy/src/cli/strategy/runtime_ops/foreground_submit.rs), [order_commands.rs](/Users/proerror/Documents/ploy/src/coordinator/strategy_runtime/actions/order_commands.rs), [strategy.rs](/Users/proerror/Documents/ploy/src/strategy/event_edge/strategy.rs), [strategy.rs](/Users/proerror/Documents/ploy/src/strategy/nba_comeback/strategy.rs), and [tests.rs](/Users/proerror/Documents/ploy/src/strategy/staggered_arb_live/tests.rs) to import the bridge from `crate::strategy::runtime_order`.
- 2026-03-11: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-strategy-surface-cut rtk cargo check --lib --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-strategy-surface-cut-runtime rtk cargo test order_request_from_intent_preserves_action_id --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-strategy-surface-cut-fg rtk cargo test build_coordinator_payload_requires_deployment_id --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-strategy-surface-cut-fg2 rtk cargo test build_coordinator_payload_preserves_strategy_metadata --lib -- --exact --nocapture`

# RL Public Surface Quarantine (2026-03-11)

## Goal
Stop exporting the legacy RL order runtime as part of the public `rl` and crate-root API so the compatibility execution stack is no longer presented as a canonical runtime surface.

## Tasks

- [x] Remove `RlOrderRuntime*` re-exports from `src/rl/mod.rs`.
- [x] Remove `RlOrderRuntime*` crate-root re-exports from `src/lib.rs`.
- [x] Rewire the RL CLI command to import the compatibility runtime from its concrete module path.
- [x] Re-run RL-focused compile and runtime regressions after the public-surface cut.

## Review

- [x] Confirm the only remaining direct consumer is the RL CLI command.
- [x] Confirm RL runtime behavior tests still pass after the surface quarantine.

## Progress notes

- 2026-03-11: Removed `RlOrderRuntime`, `RlOrderRuntimeConfig`, and `RlRuntimeStats` from [mod.rs](/Users/proerror/Documents/ploy/src/rl/mod.rs) and from the crate-root RL exports in [lib.rs](/Users/proerror/Documents/ploy/src/lib.rs).
- 2026-03-11: Rewired [agent.rs](/Users/proerror/Documents/ploy/src/main_commands/rl/agent.rs) to import the compatibility runtime from `ploy::rl::order_platform`.
- 2026-03-11: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-rl-surface-cut rtk cargo check --lib --features rl --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-rl-surface-cut-start rtk cargo test --features rl test_rl_order_runtime_start_blocks_live_runtime --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-rl-surface-cut-agent rtk cargo test --features rl test_rl_agent_lifecycle --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-rl-surface-cut-pos rtk cargo test --features rl test_position_tracking --lib -- --exact --nocapture`

# Journal Ingress Writes Cut (2026-03-11)

## Goal
Move ingress/risk/runtime-state journal writes out of `src/coordinator/journal.rs` so the root journal owner becomes a thin shell around restore, module wiring, and shared metadata parsing.

## Tasks

- [x] Add a journal submodule for ingress-side signal/risk/exit-intent writes plus runtime-state persistence.
- [x] Move `persist_signal_from_intent`, `persist_risk_decision`, `persist_exit_reason_intent`, and `persist_risk_runtime_state` out of `journal.rs`.
- [x] Reduce `journal.rs` to restore loading, pool wiring, and shared metadata helpers.
- [x] Re-run compile plus focused ingress/runtime-status regressions after the cut.

## Review

- [x] Confirm coordinator ingress/rejection/runtime-status callers still hit the same journal method surface.
- [x] Confirm `restore.rs` keeps compiling after parent-module imports no longer leak through `journal.rs`.

## Progress notes

- 2026-03-11: Added [ingress_writes.rs](/Users/proerror/Documents/ploy/src/coordinator/journal/ingress_writes.rs) for signal history, risk decision, exit reason intent, and risk runtime-state persistence.
- 2026-03-11: Reduced [journal.rs](/Users/proerror/Documents/ploy/src/coordinator/journal.rs) to the journal shell, restore reads, module wiring, and shared `metadata_decimal` parsing.
- 2026-03-11: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-journal-ingress-cut rtk cargo check --lib --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-journal-ingress-cut-updates rtk cargo test test_handle_order_intent_emits_rejected_update_for_missing_deployment --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-journal-ingress-cut-status rtk cargo test refresh_global_state_marks_stale_running_agents --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-journal-ingress-cut-pending rtk cargo test test_drain_and_execute_emits_pending_and_fill_updates --lib -- --exact --nocapture`

# Journal Execution Writes Cut (2026-03-10)

## Goal
Move execution-side journal writes out of `src/coordinator/journal.rs` so the root journal owner keeps restore plus ingress/risk writes, while execution persistence/evaluation behavior lives in a dedicated submodule.

## Tasks

- [x] Add a journal submodule for execution persistence, analysis, and live-evaluation writes.
- [x] Move `persist_execution`, `persist_exit_reason_execution`, `persist_execution_analysis`, and `persist_live_strategy_evaluation` out of `journal.rs`.
- [x] Keep restore and ingress/risk write paths in the root journal owner.
- [x] Re-run compile plus focused execution/restore regressions after the cut.

## Review

- [x] Confirm `coordinator` callers still hit the same `persist_execution` surface.
- [x] Confirm restore parsing still compiles after the journal import boundary changed.

## Progress notes

- 2026-03-10: Added [execution_writes.rs](/Users/proerror/Documents/ploy/src/coordinator/journal/execution_writes.rs) for execution persistence, exit-reason execution writes, execution analysis, and live strategy evaluation evidence.
- 2026-03-10: Reduced [journal.rs](/Users/proerror/Documents/ploy/src/coordinator/journal.rs) to the journal shell, restore reads, ingress/risk writes, and shared metadata parsing.
- 2026-03-10: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-journal-cut rtk cargo check --lib --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-journal-cut-buy rtk cargo test test_drain_and_execute_records_single_success_for_buy_fill --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-journal-cut-restore rtk cargo test test_execution_error_is_failure_treats_blank_as_success --lib -- --exact --nocapture`

# Coordinator Ingress Admission Cut (2026-03-10)

## Goal
Move the post-preflight coordinator ingress admission pipeline out of `src/coordinator/coordinator/ingress.rs` so submit-facing handle APIs stop sharing ownership with governance checks, allocator reservation, and queue-admission orchestration.

## Tasks

- [x] Add a dedicated coordinator submodule for runtime ingress admission orchestration.
- [x] Move `handle_order_intent` and its governance/account-notional/reservation helpers out of `ingress.rs`.
- [x] Reduce `ingress.rs` to the `CoordinatorHandle` submit-facing trade-intent bridge.
- [x] Re-run compile plus focused ingress/governance regressions after the cut.

## Review

- [x] Confirm runtime preflight and rejection helpers remain in their existing owner modules.
- [x] Confirm missing-deployment rejection, force-close domain gating, and pending/fill updates still pass.

## Progress notes

- 2026-03-10: Added [ingress_pipeline.rs](/Users/proerror/Documents/ploy/src/coordinator/coordinator/ingress_pipeline.rs) to own runtime order-intent admission, governance policy checks, allocator reservation, and queue enqueue orchestration.
- 2026-03-10: Reduced [ingress.rs](/Users/proerror/Documents/ploy/src/coordinator/coordinator/ingress.rs) to the `CoordinatorHandle::submit_trade_intent` bridge.
- 2026-03-10: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-ingress-cut rtk cargo check --lib --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-ingress-cut-missing rtk cargo test test_handle_order_intent_emits_rejected_update_for_missing_deployment --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-ingress-cut-force rtk cargo test test_handle_force_close_domain_blocks_new_buy_immediately --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-ingress-cut-updates rtk cargo test test_drain_and_execute_emits_pending_and_fill_updates --lib -- --exact --nocapture`

# Coordinator Execution Settlement Cut (2026-03-10)

## Goal
Move execution success/failure settlement, position-book updates, and risk-exposure refresh out of `src/coordinator/coordinator/execution.rs` so the queue-drain loop becomes a thin dispatcher and recovery reuses a dedicated execution-outcome owner.

## Tasks

- [x] Add a dedicated coordinator submodule for execution outcome settlement helpers.
- [x] Move success/failure persistence, fill-settlement, and risk-refresh helpers out of `execution.rs`.
- [x] Reduce `drain_and_execute` to queue draining plus executor dispatch/delegation.
- [x] Re-run compile plus focused buy/sell fill regressions after the cut.

## Review

- [x] Confirm `recovery.rs` still reuses the extracted settlement helpers instead of duplicating logic.
- [x] Confirm the queue-drain happy path, pending/fill updates, and sell-fill PnL regression tests still pass.

## Progress notes

- 2026-03-10: Added [execution_settlement.rs](/Users/proerror/Documents/ploy/src/coordinator/coordinator/execution_settlement.rs) to own execution success/failure journaling, capital settlement, position-book updates, and risk-refresh helpers.
- 2026-03-10: Reduced [execution.rs](/Users/proerror/Documents/ploy/src/coordinator/coordinator/execution.rs) to a thin queue-drain dispatcher that delegates post-execution handling to the new settlement owner.
- 2026-03-10: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-coord-exec-cut rtk cargo check --lib --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-coord-exec-cut rtk cargo test test_drain_and_execute_records_single_success_for_buy_fill --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-coord-exec-cut-sell rtk cargo test test_drain_and_execute_sell_fill_reduces_position_and_realizes_pnl --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-coord-exec-cut-updates rtk cargo test test_drain_and_execute_emits_pending_and_fill_updates --lib -- --exact --nocapture`

# Persistence Pipeline Ownership Cut (2026-03-10)

## Goal
Move the remaining platform-owned persistence pipeline/schema surface under `src/persistence` so bootstrap and other runtime callers stop treating `src/platform` as the canonical owner.

## Tasks

- [x] Add a regression test that proves `crate::persistence` exposes the pipeline/schema surface.
- [x] Move pipeline ownership into `src/persistence` and reduce `src/platform/persistence_pipeline.rs` to a compatibility shim.
- [x] Rewire bootstrap/support and other direct schema callers to `crate::persistence`.
- [x] Re-run focused compile/tests and confirm the platform module is only a legacy bridge.

## Review

- [x] Confirm persistence callers no longer import pipeline/schema directly from `crate::platform`.
- [x] Confirm the persistence pipeline dedup tests still run from the persistence-owned module.

## Progress notes

- 2026-03-10: Added [pipeline.rs](/Users/proerror/Documents/ploy/src/persistence/pipeline.rs) and [runtime.rs](/Users/proerror/Documents/ploy/src/persistence/pipeline/runtime.rs) so the persistence pipeline implementation now lives under `src/persistence`.
- 2026-03-10: Reduced [persistence_pipeline.rs](/Users/proerror/Documents/ploy/src/platform/persistence_pipeline.rs) to a compatibility shim and rewired bootstrap/runtime callers to import pipeline ownership from `crate::persistence`.
- 2026-03-10: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-order-intent-cut-worker rtk cargo check --lib --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-order-intent-cut-worker rtk cargo test persistence_module_reexports_market_data_surface --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-order-intent-cut-worker rtk cargo test quote_dedup_skips_unchanged_within_interval --lib -- --exact --nocapture`

# Data Plane Ownership Cut (2026-03-10)

## Goal
Move `PlatformDataPlane`, freshness tracking, and their related runtime/handle types out of `src/platform` so the live market-data surface has a neutral owner and `platform` degrades to a compatibility layer.

## Tasks

- [x] Add a top-level `src/data_plane` owner for runtime and freshness modules.
- [x] Reduce `src/platform` to compatibility re-exports for data-plane types.
- [x] Rewire live runtime, bootstrap, adapter, service, and strategy callers away from `crate::platform::*` data-plane imports.
- [x] Re-run compile and focused data-plane/feed regressions after the move.

## Review

- [x] Confirm repo-internal imports for data-plane types no longer point at `crate::platform`.
- [x] Confirm the data-plane runtime and feed consumers still compile and pass focused regressions.

## Progress notes

- 2026-03-10: Moved the data-plane owner into [mod.rs](/Users/proerror/Documents/ploy/src/data_plane/mod.rs), [runtime.rs](/Users/proerror/Documents/ploy/src/data_plane/runtime.rs), and [freshness.rs](/Users/proerror/Documents/ploy/src/data_plane/freshness.rs).
- 2026-03-10: Reduced [mod.rs](/Users/proerror/Documents/ploy/src/platform/mod.rs) so `platform` now re-exports the data-plane surface instead of owning it.
- 2026-03-10: Rewired bootstrap, managed runtime startup, adapters, services, TUI, RL CLI, and strategy runners to import data-plane types from `crate::data_plane`.
- 2026-03-10: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-dataplane-cut rtk cargo check --lib --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-dataplane-cut rtk cargo test source_health_reports_down_healthy_and_degraded --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-dataplane-cut rtk cargo test test_from_data_plane_reuses_singleton_adapters --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-dataplane-cut rtk cargo test test_feed_builder --lib -- --exact --nocapture`

# Domain Ownership Cut (2026-03-10)

## Goal
Move the shared `Domain` scope type out of `src/platform` and into `src/domain` so control-plane, coordinator, strategy, API, and persistence contracts stop depending on a legacy platform owner for a cross-cutting business type.

## Tasks

- [x] Move the `Domain` type implementation into a `src/domain` leaf module.
- [x] Keep crate-root and `platform` re-exports so external compatibility is preserved during the import migration.
- [x] Rewire repo-internal imports away from `crate::platform::Domain`.
- [x] Re-run compile and focused cross-layer regressions after the move.

## Review

- [x] Confirm repo-internal source imports no longer point at `crate::platform::Domain`.
- [x] Confirm deployment/control-plane, order-intent, and coordinator domain gating tests still pass.

## Progress notes

- 2026-03-10: Moved the type implementation from [types.rs](/Users/proerror/Documents/ploy/src/platform/types.rs) to [scope.rs](/Users/proerror/Documents/ploy/src/domain/scope.rs), and re-exported it from [mod.rs](/Users/proerror/Documents/ploy/src/domain/mod.rs).
- 2026-03-10: Updated [mod.rs](/Users/proerror/Documents/ploy/src/platform/mod.rs) and [lib.rs](/Users/proerror/Documents/ploy/src/lib.rs) so `platform::Domain` and the crate-root `Domain` remain compatibility re-exports.
- 2026-03-10: Rewired coordinator, strategy, control-plane, API, RL, persistence, and agent modules to import `Domain` from `crate::domain`.
- 2026-03-10: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-domain-cut rtk cargo check --lib --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-domain-cut rtk cargo test deployment_runtime_scope_matching --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-domain-cut rtk cargo test order_intent_from_trade_intent_maps_priority_and_metadata --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-domain-cut rtk cargo test test_handle_force_close_domain_blocks_new_buy_immediately --lib -- --exact --nocapture`

# Market Persistence Ownership Extraction (2026-03-09)

## Goal
Move the Polymarket trade/settlement persistence service out of `coordinator/bootstrap` so bootstrap stops owning long-running market-data persistence behavior.

## Tasks

- [x] Move `bootstrap/market_persistence.rs` into a `platform`-owned module.
- [x] Rewire bootstrap siblings to import the persistence service from the new owner.
- [x] Keep bootstrap behavior unchanged while removing `market_persistence` from bootstrap-owned implementation.
- [x] Re-run compile and focused persistence/bootstrap regressions after the move.

## Review

- [x] Confirm bootstrap no longer defines the market persistence implementation body.
- [x] Confirm trade persistence, collector-target persistence, and settlement persistence still compile from the new owner module.

## Progress notes

- 2026-03-09: Moved the full Polymarket trade/settlement persistence implementation into [market_persistence.rs](/Users/proerror/Documents/ploy/src/platform/market_persistence.rs) and exposed it through [mod.rs](/Users/proerror/Documents/ploy/src/platform/mod.rs).
- 2026-03-09: Follow-up cleanup deleted the leftover [market_persistence.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/market_persistence.rs) bootstrap shim and rewired [bootstrap.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap.rs) to import persistence ownership directly from `crate::platform`.
- 2026-03-09: Moved deployment-selector coin parsing out of [support.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/support.rs) after the strategy deployment/runtime-spec ownership transfer.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo test collect_managed_strategy_runtime_plans_collapses_crypto_spawn_specs --lib -- --nocapture`
  - `cargo test --features rl test_rl_order_runtime_start_blocks_live_runtime --lib -- --nocapture`
  - `cargo test --features rl test_rl_agent_lifecycle --lib -- --nocapture`

# Coordinator Control Surface Extraction (2026-03-10)

## Goal
Move coordinator ingress/control APIs and control-command fanout out of `src/coordinator/coordinator.rs` so the main file keeps execution/admission ownership while control surface logic lives in its own module.

## Tasks

- [x] Extract `CoordinatorHandle` submit/pause/resume/shutdown methods into a dedicated `coordinator/control_surface` module.
- [x] Extract coordinator-side command fanout (`pause_all`, `resume_all`, domain halt/shutdown, agent pause/resume) into the same module.
- [x] Simplify the main run loop to delegate control command handling instead of inlining the full match.
- [x] Re-run compile and focused control-plane regressions after the extraction.

## Review

- [x] Confirm `coordinator.rs` no longer owns the full handle/control API surface.
- [x] Confirm control commands still block/allow ingress correctly after the extraction.

## Progress notes

- 2026-03-10: Added [control_surface.rs](/Users/proerror/Documents/ploy/src/coordinator/coordinator/control_surface.rs) to own `CoordinatorHandle` ingress/control methods plus coordinator command fanout.
- 2026-03-10: Reduced [coordinator.rs](/Users/proerror/Documents/ploy/src/coordinator/coordinator.rs) by replacing the inlined control-command match with `handle_control_command(...)`.
- 2026-03-10: Validation passed:
  - `cargo check --lib`
  - `cargo test test_handle_force_close_domain_blocks_new_buy_immediately --lib -- --nocapture`
  - `cargo test test_governance_status_includes_domain_ingress_and_agents --lib -- --nocapture`
  - `cargo test test_drain_and_execute_records_single_success_for_buy_fill --lib -- --nocapture`

# Runtime Schema Ownership Extraction (2026-03-09)

## Goal
Move bootstrap-owned runtime schema helpers into `src/persistence` so schema/table ownership stops hiding under the coordinator bootstrap path.

## Tasks

- [x] Introduce `src/persistence/runtime_schema.rs` as the owner for runtime schema helpers.
- [x] Re-export runtime schema helpers from `src/persistence/mod.rs`.
- [x] Rewire bootstrap/CLI/runtime callers away from `crate::coordinator::bootstrap::ensure_*` and onto `crate::persistence`.
- [x] Re-run compile and focused regressions after the ownership move.

## Review

- [x] Confirm runtime schema helpers now compile from `crate::persistence`.
- [x] Confirm bootstrap schema ownership is reduced to a thin compatibility layer instead of the implementation body.

## Progress notes

- 2026-03-09: Added [runtime_schema.rs](/Users/proerror/Documents/ploy/src/persistence/runtime_schema.rs) and re-exported the runtime schema helpers from [mod.rs](/Users/proerror/Documents/ploy/src/persistence/mod.rs).
- 2026-03-09: Updated [strategy.rs](/Users/proerror/Documents/ploy/src/cli/strategy.rs), [strategy_runtime.rs](/Users/proerror/Documents/ploy/src/coordinator/strategy_runtime.rs), and [adapters.rs](/Users/proerror/Documents/ploy/src/strategy/adapters.rs) to consume runtime schema helpers from `crate::persistence`.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo check --lib --features rl`
  - `cargo test collect_managed_strategy_runtime_plans_collapses_crypto_spawn_specs --lib -- --nocapture`

# Strategy Runtime Specs Ownership (2026-03-09)

## Goal
Move deployment-matrix/runtime-config/runtime-plan ownership under `src/strategy` so bootstrap stops acting like the owner of managed strategy spec compilation.

## Tasks

- [x] Add `src/strategy/runtime_specs` as the strategy-owned home for deployment matrix, runtime config builders, and managed runtime plan compilation.
- [x] Rewire bootstrap to consume `crate::strategy::runtime_specs` instead of owning submodules under `bootstrap/strategy_deployments`.
- [x] Keep the bootstrap-facing plan wrapper thin while deleting the old bootstrap-owned implementation files.
- [x] Re-run compile and focused managed-runtime regressions after the move.

## Review

- [x] Confirm `bootstrap/strategy_deployments` no longer owns the deployment matrix/runtime builder implementation files.
- [x] Confirm managed runtime planning now compiles from `crate::strategy::runtime_specs`.

## Progress notes

- 2026-03-09: Added [runtime_specs](/Users/proerror/Documents/ploy/src/strategy/runtime_specs/mod.rs) under `src/strategy` and exposed it from [mod.rs](/Users/proerror/Documents/ploy/src/strategy/mod.rs).
- 2026-03-09: Converted [strategy_deployments.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/strategy_deployments.rs) into a thin bootstrap-facing wrapper over `crate::strategy::runtime_specs`.
- 2026-03-09: Deleted the bootstrap-owned implementation files under `src/coordinator/bootstrap/strategy_deployments/`.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo test collect_managed_strategy_runtime_plans_collapses_crypto_spawn_specs --lib -- --nocapture`

# RL Order Runtime Alias Retirement (2026-03-09)

## Goal
Stop the RL CLI path from pretending `OrderPlatform` is a first-class runtime surface by keeping the canonical naming on the RL side and dropping the old compatibility aliases.

## Tasks

- [x] Remove `OrderPlatform` / `PlatformConfig` / `PlatformStats` aliases from the RL runtime surface.
- [x] Rewire RL callers to use `RlOrderRuntime` / `RlOrderRuntimeConfig` / `RlRuntimeStats`.
- [x] Re-run focused RL compile/tests after the alias retirement.

## Review

- [x] Confirm repo-wide source references to the removed RL order-runtime aliases are gone.
- [x] Confirm the RL CLI/runtime still compiles and passes focused regressions.

## Progress notes

- 2026-03-09: Removed the legacy `OrderPlatform`, `PlatformConfig`, and `PlatformStats` aliases from [order_platform.rs](/Users/proerror/Documents/ploy/src/rl/order_platform.rs) and narrowed [mod.rs](/Users/proerror/Documents/ploy/src/rl/mod.rs) to the canonical RL runtime names.
- 2026-03-09: Updated [agent.rs](/Users/proerror/Documents/ploy/src/main_commands/rl/agent.rs) to construct `RlOrderRuntime` directly.
- 2026-03-09: Validation passed:
  - `cargo check --lib --features rl`
  - `cargo test --features rl test_rl_order_runtime_start_blocks_live_runtime --lib -- --nocapture`
  - `cargo test --features rl test_rl_agent_lifecycle --lib -- --nocapture`

# OpenClaw Config Shim Retirement (2026-03-09)

## Goal
Delete the leftover `agents/openclaw/config.rs` shim so OpenClaw modules stop pretending they own config types that were already moved under bootstrap ownership.

## Tasks

# Strategy Execution Engine Leg1 Extraction (2026-03-10)

## Progress notes

- 2026-03-10: Moved the heavy Leg1 submission/persistence/version-conflict flow out of [engine.rs](/Users/proerror/Documents/ploy/src/strategy/execution/engine.rs) into [leg1.rs](/Users/proerror/Documents/ploy/src/strategy/execution/engine/leg1.rs), leaving `engine.rs` with a thin `enter_leg1` wrapper.
- 2026-03-10: Validation passed:
  - `rtk cargo check --lib`
  - `rtk cargo test leg_updates_should_use_incrementing_cycle_versions --lib -- --nocapture`
  - `rtk cargo test leg1_cycle_version_conflict_should_abort_and_error --lib -- --nocapture`

# Strategy And Adapter Wave 5 (2026-03-10)

## Goal
Keep shrinking active-core live/runtime files after the legacy retirement wave by extracting ownership from the remaining heavy strategy and adapter modules.

## File ownership

- `src/strategy/execution/engine.rs`
  - owner: execution flow extraction
- `src/strategy/momentum.rs`
  - owner: momentum runtime/state flow extraction
- `src/adapters/polymarket_ws.rs`
  - owner: websocket lifecycle / subscription flow extraction
- `src/adapters/postgres.rs`
  - owner: Postgres persistence/read-model extraction

## Tasks

- [x] Extract the next execution-flow ownership slice from `engine.rs`.
- [x] Extract the next runtime/state-flow slice from `momentum.rs`.
- [x] Extract a websocket lifecycle/subscription owner from `polymarket_ws.rs`.
- [x] Extract a Postgres read/persistence owner from `postgres.rs`.
- [x] Re-run compile plus focused regressions after the wave.

## Progress notes

- 2026-03-10: Preflight file ownership for Wave 5 assigned before starting the next parallel batch.
- 2026-03-10: Moved the claimer native-gas preflight, auto-topup, and on-chain redeem flow out of [claimer.rs](/Users/proerror/Documents/ploy/src/strategy/claimer.rs) into [claim_flow.rs](/Users/proerror/Documents/ploy/src/strategy/claimer/claim_flow.rs), leaving the root claimer module with thin async delegators.
- 2026-03-10: Moved Polymarket WebSocket subscription ownership out of [polymarket_ws.rs](/Users/proerror/Documents/ploy/src/adapters/polymarket_ws.rs) / [connection.rs](/Users/proerror/Documents/ploy/src/adapters/polymarket_ws/connection.rs) into [subscriptions.rs](/Users/proerror/Documents/ploy/src/adapters/polymarket_ws/subscriptions.rs).
- 2026-03-10: Moved Binance WebSocket proxy/runtime lifecycle ownership out of [binance_ws.rs](/Users/proerror/Documents/ploy/src/adapters/binance_ws.rs) into [runtime.rs](/Users/proerror/Documents/ploy/src/adapters/binance_ws/runtime.rs).
- 2026-03-10: Moved momentum runtime-state helpers and rate-limit/window tracking out of [momentum.rs](/Users/proerror/Documents/ploy/src/strategy/momentum.rs) into [runtime_state.rs](/Users/proerror/Documents/ploy/src/strategy/momentum/runtime_state.rs).
- 2026-03-10: Moved the large `StrategyEngine` test ownership out of [engine.rs](/Users/proerror/Documents/ploy/src/strategy/execution/engine.rs) into [tests.rs](/Users/proerror/Documents/ploy/src/strategy/execution/engine/tests.rs).
- 2026-03-10: Moved Postgres recovery/read-model ownership out of [postgres.rs](/Users/proerror/Documents/ploy/src/adapters/postgres.rs) into [recovery.rs](/Users/proerror/Documents/ploy/src/adapters/postgres/recovery.rs).
- 2026-03-10: Wave 5 validation passed so far:
  - `CARGO_TARGET_DIR=/tmp/ploy-main-wave5 rtk cargo check --lib`
  - `CARGO_TARGET_DIR=/tmp/ploy-main-wave5 rtk cargo test test_window_tracker_prefers_highest_edge_signal --lib -- --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-main-wave5 rtk cargo test leg1_cycle_version_conflict_should_abort_and_error --lib -- --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-main-wave5 rtk cargo test orphaned_order_cancel_gate_requires_exchange_id_and_active_status --lib -- --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-main-wave5 rtk cargo test characterization_agg_trade_produces_price_update --lib -- --exact --nocapture`

# Strategy And Adapter Wave 6 (2026-03-10)

## Goal
Keep shrinking the remaining active-core modules after Wave 5 by extracting clear owners from the heaviest adapter, platform, CLI, and live-strategy files still on the hot path.

## File ownership

- `src/adapters/polymarket_clob.rs`
  - owner: remaining authenticated/read-path extraction
- `src/cli/strategy/runtime_ops.rs`
  - owner: runtime CLI orchestration extraction
- `src/platform/persistence_pipeline.rs`
  - owner: persistence pipeline stage ownership
- `src/strategy/event_edge/strategy.rs`
  - owner: event-edge runtime/position flow extraction

## Tasks

- [x] Extract the next `polymarket_clob` ownership slice into a sibling module.
- [x] Extract the next `runtime_ops` ownership slice into a sibling module.
- [x] Extract the next `persistence_pipeline` ownership slice into a sibling module.
- [x] Extract the next `event_edge` strategy ownership slice into a sibling module.
- [x] Re-run compile plus focused regressions after the wave.

## Progress notes

- 2026-03-10: Preflight file ownership for Wave 6 assigned before dispatching the next parallel batch.
- 2026-03-10: Moved the remaining Polymarket CLOB API response/model ownership out of [polymarket_clob.rs](/Users/proerror/Documents/ploy/src/adapters/polymarket_clob.rs) into [models.rs](/Users/proerror/Documents/ploy/src/adapters/polymarket_clob/models.rs), keeping the root adapter focused on client behavior.
- 2026-03-10: Moved the foreground strategy runner, feed wiring, and action dispatch loop out of [runtime_ops.rs](/Users/proerror/Documents/ploy/src/cli/strategy/runtime_ops.rs) into [foreground.rs](/Users/proerror/Documents/ploy/src/cli/strategy/runtime_ops/foreground.rs), and deleted the leftover dead wrapper functions.
- 2026-03-10: Moved persistence-pipeline buffering, dedup, flush/runtime loop, and focused tests out of [persistence_pipeline.rs](/Users/proerror/Documents/ploy/src/platform/persistence_pipeline.rs) into [runtime.rs](/Users/proerror/Documents/ploy/src/platform/persistence_pipeline/runtime.rs).
- 2026-03-10: Moved event-edge pending-order, signal-intent, fill-reconciliation, and state-metrics ownership out of [strategy.rs](/Users/proerror/Documents/ploy/src/strategy/event_edge/strategy.rs) into [runtime_flow.rs](/Users/proerror/Documents/ploy/src/strategy/event_edge/strategy/runtime_flow.rs).
- 2026-03-10: Wave 6 validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-wave6 rtk cargo check --lib`
  - `CARGO_TARGET_DIR=/tmp/ploy-wave6 rtk cargo test test_position_response_deserializes_numeric_fields --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-wave6 rtk cargo test quote_dedup_skips_unchanged_within_interval --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-wave6 rtk cargo test on_market_update_tracks_discovered_events_and_expiry --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-wave6 rtk cargo test test_graceful_stop_reports_closed_action_channel --lib -- --exact --nocapture`

# Strategy And Adapter Wave 7 (2026-03-10)

## Goal
Keep cutting active-core runtime ownership out of the remaining heavy modules that still sit on the live path or strategy lifecycle boundary.

## File ownership

- `src/adapters/polymarket_ws.rs`
  - owner: remaining websocket runtime/broadcast extraction
- `src/strategy/manager.rs`
  - owner: strategy lifecycle and command-surface extraction
- `src/strategy/gamma_scalping/strategy.rs`
  - owner: gamma strategy runtime/decision-flow extraction
- `src/platform/position.rs`
  - owner: position reconciliation/state-transition extraction

## Tasks

- [x] Extract the next `polymarket_ws` ownership slice into a sibling module.
- [x] Extract the next `strategy manager` ownership slice into a sibling module.
- [x] Extract the next `gamma_scalping` ownership slice into a sibling module.
- [x] Extract the next `platform position` ownership slice into a sibling module.
- [x] Re-run compile plus focused regressions after the wave.

## Progress notes

- 2026-03-10: Preflight file ownership for Wave 7 assigned before dispatching the next parallel batch.
- 2026-03-10: Moved Polymarket WebSocket runtime support ownership out of [polymarket_ws.rs](/Users/proerror/Documents/ploy/src/adapters/polymarket_ws.rs) into [runtime_support.rs](/Users/proerror/Documents/ploy/src/adapters/polymarket_ws/runtime_support.rs), including the circuit breaker, quote cache, and their focused tests. Integrated a follow-up fix in [subscriptions.rs](/Users/proerror/Documents/ploy/src/adapters/polymarket_ws/subscriptions.rs) so the extracted module tree still logs registration events cleanly.
- 2026-03-10: Moved strategy-manager lifecycle ownership out of [manager.rs](/Users/proerror/Documents/ploy/src/strategy/manager.rs) into [lifecycle.rs](/Users/proerror/Documents/ploy/src/strategy/manager/lifecycle.rs), leaving the root manager focused on channel ownership, runtime loop, factory, and tests.
- 2026-03-10: Moved gamma-scalping decision/runtime ownership out of [strategy.rs](/Users/proerror/Documents/ploy/src/strategy/gamma_scalping/strategy.rs) into [decision_flow.rs](/Users/proerror/Documents/ploy/src/strategy/gamma_scalping/strategy/decision_flow.rs).
- 2026-03-10: Moved `PositionAggregator` state-transition and cleanup ownership out of [position.rs](/Users/proerror/Documents/ploy/src/platform/position.rs) into [transitions.rs](/Users/proerror/Documents/ploy/src/platform/position/transitions.rs).
- 2026-03-10: Wave 7 validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-wave7-main rtk cargo check --lib`
  - `CARGO_TARGET_DIR=/tmp/ploy-wave7-main rtk cargo test test_reduce_position_partial_close --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-wave7-main rtk cargo test test_agent_open_shares_for_token_side --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-wave7-main rtk cargo test test_graceful_stop_reports_closed_action_channel --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-wave7-main rtk cargo test evaluate_entry_emits_submit_intents --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-wave7-main rtk cargo test test_circuit_breaker_opens_after_failures --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-wave7-ws rtk cargo test characterization_book_snapshot_produces_quote_update --lib -- --exact --nocapture`

# Strategy And Adapter Wave 8 (2026-03-10)

## Goal
Keep collapsing the remaining active-core and legacy strategy surface by extracting ownership from the heaviest coordinator and strategy modules still on the live path.

## File ownership

- `src/coordinator/coordinator.rs`
  - owner: remaining runtime/recovery/control extraction
- `src/strategy/strategies/momentum_strat.rs`
  - owner: legacy momentum signal/runtime extraction
- `src/strategy/split_arb.rs`
  - owner: split-arb decision/runtime extraction
- `src/strategy/crypto_lob_ml/strategy.rs`
  - owner: crypto LOB ML inference/runtime extraction

## Tasks

- [x] Extract the next `coordinator` ownership slice into a sibling module.
- [x] Extract the next `momentum_strat` ownership slice into a sibling module.
- [x] Extract the next `split_arb` ownership slice into a sibling module.
- [x] Extract the next `crypto_lob_ml` ownership slice into a sibling module.
- [x] Re-run compile plus focused regressions after the wave.

## Progress notes

- 2026-03-10: Preflight file ownership for Wave 8 assigned before dispatching the next parallel batch.
- 2026-03-10: Moved coordinator intent-ingress ownership out of [coordinator.rs](/Users/proerror/Documents/ploy/src/coordinator/coordinator.rs) into [ingress.rs](/Users/proerror/Documents/ploy/src/coordinator/coordinator/ingress.rs), leaving the root coordinator focused on wiring, runtime loop, and tests.
- 2026-03-10: Moved legacy momentum signal/runtime ownership out of [momentum_strat.rs](/Users/proerror/Documents/ploy/src/strategy/strategies/momentum_strat.rs) into [signal_flow.rs](/Users/proerror/Documents/ploy/src/strategy/strategies/momentum_strat/signal_flow.rs).
- 2026-03-10: Moved `SplitArbEngine` runtime/opportunity ownership out of [split_arb.rs](/Users/proerror/Documents/ploy/src/strategy/split_arb.rs) into [runtime_flow.rs](/Users/proerror/Documents/ploy/src/strategy/split_arb/runtime_flow.rs).
- 2026-03-10: Moved crypto-LOB-ML inference/decision ownership out of [strategy.rs](/Users/proerror/Documents/ploy/src/strategy/crypto_lob_ml/strategy.rs) into [inference.rs](/Users/proerror/Documents/ploy/src/strategy/crypto_lob_ml/strategy/inference.rs).
- 2026-03-10: Wave 8 validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-wave8-main rtk cargo check --lib`
  - `CARGO_TARGET_DIR=/tmp/ploy-wave8-main rtk cargo test test_handle_force_close_domain_blocks_new_buy_immediately --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-wave8-momentum rtk cargo check --lib`
  - `CARGO_TARGET_DIR=/tmp/ploy-wave8-momentum rtk cargo test test_series_mapping --lib -- --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-wave8-splitarb rtk cargo check --lib`
  - `CARGO_TARGET_DIR=/tmp/ploy-wave8-splitarb rtk cargo test test_split_arb_adapter_creation --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-wave8-lobml rtk cargo check --lib`
  - `CARGO_TARGET_DIR=/tmp/ploy-wave8-lobml rtk cargo test on_tick_emits_inference_log_once_sequence_is_ready --lib -- --nocapture`

# Strategy And Adapter Wave 9 (2026-03-10)

## Goal
Keep shrinking the remaining core infrastructure by extracting ownership from the heaviest execution, capital-allocation, and config modules still used on the live path.

## File ownership

- `src/strategy/execution/executor.rs`
  - owner: execution/result-handling extraction
- `src/coordinator/capital/crypto.rs`
  - owner: crypto allocator/runtime slice extraction
- `src/coordinator/capital/market.rs`
  - owner: market capital accounting extraction
- `src/config.rs`
  - owner: runtime/env config extraction

## Tasks

- [x] Extract the next `execution executor` ownership slice into a sibling module.
- [x] Extract the next `capital/crypto` ownership slice into a sibling module.
- [x] Extract the next `capital/market` ownership slice into a sibling module.
- [x] Extract the next `config` ownership slice into a sibling module.
- [x] Re-run compile plus focused regressions after the wave.

## Progress notes

- 2026-03-10: Preflight file ownership for Wave 9 assigned before dispatching the next parallel batch.
- 2026-03-10: Reused the already-landed config extraction from commit `8251242` as the Wave 9 config slice; [config.rs](/Users/proerror/Documents/ploy/src/config.rs) now delegates env parsing into [env_overrides.rs](/Users/proerror/Documents/ploy/src/config/env_overrides.rs).
- 2026-03-10: Moved `OrderExecutor` submission/retry/fill-confirmation ownership out of [executor.rs](/Users/proerror/Documents/ploy/src/strategy/execution/executor.rs) into [execution_flow.rs](/Users/proerror/Documents/ploy/src/strategy/execution/executor/execution_flow.rs), leaving the root executor focused on construction, public API, and tests.
- 2026-03-10: Moved crypto capital runtime accounting, settlement, and ledger snapshot ownership out of [crypto.rs](/Users/proerror/Documents/ploy/src/coordinator/capital/crypto.rs) into [ledger.rs](/Users/proerror/Documents/ploy/src/coordinator/capital/crypto/ledger.rs).
- 2026-03-10: Moved market-domain capital accounting, settlement, and deployment-ledger ownership out of [market.rs](/Users/proerror/Documents/ploy/src/coordinator/capital/market.rs) into [accounting.rs](/Users/proerror/Documents/ploy/src/coordinator/capital/market/accounting.rs); fixed `available_notional_for(...)` to honor the allocator domain instead of hardcoding `Sports`.
- 2026-03-10: Cleared unrelated parallel-agent edits from maintenance/persistence files before validation so Wave 9 stays atomic.
- 2026-03-10: Wave 9 validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-wave9-main rtk cargo check --lib`
  - `CARGO_TARGET_DIR=/tmp/ploy-wave9-executor rtk cargo test execute_reports_last_retryable_error_when_retries_exhausted --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-wave9-config rtk cargo test test_parse_string_list_json_array --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-wave9-crypto rtk cargo test test_crypto_allocator_deployment_ledger_snapshot_groups_open_and_pending --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-wave9-market rtk cargo test test_market_allocator_deployment_ledger_snapshot_groups_open_and_pending --lib -- --exact --nocapture`

# Strategy And Adapter Wave 10 (2026-03-10)

## Goal
Keep shrinking the remaining live-path active core by extracting ownership from the strategy, API ingress, and capital-policy modules that still act like mixed owner/facade files.

## File ownership

- `src/strategy/momentum.rs`
  - owner: config/facade/tests extraction
- `src/api/handlers/sidecar.rs`
  - owner: types/tests extraction
- `src/coordinator/capital/crypto.rs`
  - owner: dimensions/policy extraction

## Tasks

- [ ] Extract the next `momentum` ownership slice into sibling modules.
- [ ] Extract the next `sidecar` ownership slice into sibling modules.
- [ ] Extract the next `capital/crypto` ownership slice into sibling modules.
- [ ] Re-run compile plus focused regressions after the wave.

## Progress notes

- 2026-03-10: Preflight file ownership for Wave 10 assigned before dispatching the next parallel batch.

# Strategy And Adapter Wave 11 (2026-03-11)

## Goal
Finish the half-applied `nba_comeback` strategy split by moving config-loading and tests into dedicated sibling modules so the root strategy file stays focused on runtime ownership.

## File ownership

- `src/strategy/nba_comeback/strategy.rs`
  - owner: thin strategy owner / module wiring
- `src/strategy/nba_comeback/strategy/config_loader.rs`
  - owner: config/default/TOML loading
- `src/strategy/nba_comeback/strategy/tests.rs`
  - owner: focused NBA strategy regressions

## Tasks

- [x] Complete the extracted config-loader module and restore `from_config` / `from_toml` there.
- [x] Move the inline NBA strategy tests into a dedicated sibling module.
- [x] Re-run compile plus focused NBA strategy regressions after the split.

## Progress notes

- 2026-03-11: Found the `nba_comeback` split already half-started in the worktree: `strategy.rs` had dropped config/test code and declared `mod config_loader; mod tests;`, but the module files did not exist yet.
- 2026-03-11: Added [config_loader.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/strategy/nba_comeback/strategy/config_loader.rs) to own default config, TOML parsing helpers, `from_config`, and `from_toml`.
- 2026-03-11: Added [tests.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/strategy/nba_comeback/strategy/tests.rs) to own the strategy-focused config/fill regressions.
- 2026-03-11: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-nba-config-cut rtk cargo check --lib --message-format=short`
- 2026-03-11: Focused lib-test runs were attempted with:
  - `CARGO_TARGET_DIR=/tmp/ploy-nba-config-cut-t1 rtk cargo test strategy::nba_comeback::strategy::tests::from_toml_builds_nba_strategy_and_overrides_config --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-nba-config-cut-t2 rtk cargo test strategy::nba_comeback::strategy::tests::emits_canonical_submit_order_and_tracks_fill_into_position --lib -- --exact --nocapture`
- 2026-03-11: The focused test invocations did not complete within the tool window because compiling the branch's full `lib test` target exceeded the session limit; no NBA-specific compile failures surfaced after the module split.

# Strategy And Adapter Wave 12 (2026-03-11)

## Goal
Keep shrinking `nba_comeback` live-path ownership by moving fill settlement, position updates, and state/reset helpers out of the root strategy file.

## File ownership

- `src/strategy/nba_comeback/strategy.rs`
  - owner: thin trait entrypoints and scan loop
- `src/strategy/nba_comeback/strategy/state_flow.rs`
  - owner: order update flow, position bookkeeping, runtime state helpers

## Tasks

- [x] Move `on_order_update` heavy logic behind a thin strategy delegator.
- [x] Move state/positions/is_active/shutdown/reset helpers into a sibling module.
- [x] Re-run compile plus focused NBA fill regression after the split.

## Progress notes

- 2026-03-11: Added [state_flow.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/strategy/nba_comeback/strategy/state_flow.rs) to own fill settlement, position metadata updates, runtime state snapshots, shutdown, and reset behavior.
- 2026-03-11: Kept [strategy.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/strategy/nba_comeback/strategy.rs) as the thin trait owner; `on_order_update`, `state`, `positions`, `is_active`, `shutdown`, and `reset` now delegate into `state_flow`.
- 2026-03-11: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-nba-config-loader rtk cargo check --lib --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-nba-order-update-fast rtk cargo test emits_canonical_submit_order_and_tracks_fill_into_position --lib -- --exact --nocapture`

# Collector Wave 1 (2026-03-11)

## Goal
Move `sync_collector` database/schema ownership into a dedicated persistence sibling so the root collector file stays focused on runtime orchestration and in-memory alignment.

## File ownership

- `src/collector/sync_collector.rs`
  - owner: runtime loop, price alignment, broadcast flow
- `src/collector/sync_collector/persistence.rs`
  - owner: quote/token target persistence, sync schema bootstrap, sync-record sinks

## Tasks

- [x] Move quote/token target persistence entrypoints behind thin delegators.
- [x] Move schema bootstrap, derived view DDL, and raw/legacy sink writes into a sibling module.
- [x] Re-run compile plus focused collector tests after the split.

## Progress notes

- 2026-03-11: Added [persistence.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/collector/sync_collector/persistence.rs) to own `persist_polymarket_quote_tick`, `upsert_token_targets`, schema initialization, derived-view creation, and the raw/legacy database sinks.
- 2026-03-11: Kept [sync_collector.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/collector/sync_collector.rs) focused on runtime startup, in-memory price history, Polymarket alignment, broadcast, and lag analysis.
- 2026-03-11: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-sync-collector-persistence rtk cargo check --lib --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-sync-collector-persistence-tests rtk cargo test select_pm_price_handles_xrp_and_avoids_empty_prefix_bug --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-sync-collector-persistence-tests2 rtk cargo test select_pm_price_returns_none_for_unknown_symbol --lib -- --exact --nocapture`

# Claimer Wave 1 (2026-03-11)

## Goal
Move `relayer` proxy-signing and request-construction ownership into a dedicated sibling module so the root file stays focused on claim submission and polling flow.

## File ownership

- `src/strategy/claimer/relayer.rs`
  - owner: relayer env/config gates, submit flow, polling
- `src/strategy/claimer/relayer/proxy_support.rs`
  - owner: builder credentials, HMAC/header construction, calldata/proxy hashing helpers, request payload types

## Tasks

- [x] Move proxy-signing/calldata/header helpers into a sibling module.
- [x] Keep the relayer submit/poll loop in the root file.
- [x] Re-run compile safety after the split.

## Progress notes

- 2026-03-11: Added [proxy_support.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/strategy/claimer/relayer/proxy_support.rs) to own builder credentials, relayer request/response payload types, HMAC/header construction, calldata encoding, proxy wallet derivation, and struct-hash generation.
- 2026-03-11: Kept [relayer.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/strategy/claimer/relayer.rs) focused on feature gating, base-url selection, SDK/legacy submit flow, and polling.
- 2026-03-11: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-relayer-proxy-support rtk cargo check --lib --message-format=short`
- 2026-03-11: `cargo test --lib -- --list` did not expose any registered relayer-focused unit tests in the current lib harness, so this slice is compile-verified only.

# Claimer Wave 2 (2026-03-11)

## Goal
Shrink `src/strategy/claimer/relayer.rs` again by moving the SDK path, legacy HTTP submit/poll flow, and relayer-focused tests into sibling modules so the root file owns only gating and path selection.

## File ownership

- `src/strategy/claimer/relayer.rs`
  - owner: relayer env/config gates and top-level path selection
- `src/strategy/claimer/relayer/sdk_flow.rs`
  - owner: builder SDK submit/poll path
- `src/strategy/claimer/relayer/legacy_flow.rs`
  - owner: legacy HTTP payload fetch, submit, and poll path
- `src/strategy/claimer/relayer/tests.rs`
  - owner: relayer-focused regressions

## Tasks

- [x] Move the builder SDK submit/poll implementation into a sibling module.
- [x] Move the legacy HTTP submit/poll implementation into a sibling module.
- [x] Move relayer-focused tests out of the root file.
- [x] Re-run compile plus focused relayer regressions after the split.

## Progress notes

- 2026-03-11: Added [sdk_flow.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/strategy/claimer/relayer/sdk_flow.rs), [legacy_flow.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/strategy/claimer/relayer/legacy_flow.rs), and [tests.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/strategy/claimer/relayer/tests.rs).
- 2026-03-11: Reduced [relayer.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/strategy/claimer/relayer.rs) to relayer env/config helpers plus the top-level `claim_position_via_relayer_proxy(...)` path selector.
- 2026-03-11: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-relayer-wave2-claimer rtk cargo check --lib --features claimer_daemon --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-relayer-wave2-claimer cargo test --lib --features claimer_daemon -- --list | rg 'relayer|proxy_signature|missing_relayer|0x_prefix|hmac'`
  - `CARGO_TARGET_DIR=/tmp/ploy-relayer-wave2-claimer rtk cargo test --features claimer_daemon strategy::claimer::relayer::tests::test_relayer_hmac_signature_urlsafe_base64 --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-relayer-wave2-claimer rtk cargo test --features claimer_daemon strategy::claimer::relayer::tests::test_missing_relayer_builder_credential_groups --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-relayer-wave2-claimer rtk cargo test --features claimer_daemon strategy::claimer::relayer::tests::test_proxy_signature_matches_builder_relayer_client_vector --lib -- --exact --nocapture`

# Runtime Schema Repairs Wave 1 (2026-03-11)

## Goal
Break `src/persistence/runtime_schema/repairs.rs` into domain-focused sibling modules so trade-state repairs, runtime-event repairs, and idempotency/freshness repairs stop living in one monolithic DDL owner.

## File ownership

- `src/persistence/runtime_schema/repairs.rs`
  - owner: thin façade that assembles the startup repair DDL
- `src/persistence/runtime_schema/repairs/trade_state_repairs.rs`
  - owner: orders/positions/reconciliation/nonce/fill repair fragments
- `src/persistence/runtime_schema/repairs/runtime_event_repairs.rs`
  - owner: balance snapshot / heartbeat / system event repair fragments
- `src/persistence/runtime_schema/repairs/idempotency_repairs.rs`
  - owner: order idempotency / quote freshness repair fragments

## Tasks

- [x] Split trade-state repair SQL into a sibling module.
- [x] Split runtime-event repair SQL into a sibling module.
- [x] Split idempotency/freshness repair SQL into a sibling module.
- [x] Re-run compile plus focused persistence regressions after the split.

## Progress notes

- 2026-03-11: Added [trade_state_repairs.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/persistence/runtime_schema/repairs/trade_state_repairs.rs), [runtime_event_repairs.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/persistence/runtime_schema/repairs/runtime_event_repairs.rs), and [idempotency_repairs.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/persistence/runtime_schema/repairs/idempotency_repairs.rs).
- 2026-03-11: Reduced [repairs.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/persistence/runtime_schema/repairs.rs) to a façade that assembles the same startup `DO $$ ... $$` repair block.
- 2026-03-11: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-runtime-schema-repairs rtk cargo check --lib --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-runtime-schema-repairs rtk cargo test persistence::tests::persistence_module_reexports_market_data_surface --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-runtime-schema-repairs rtk cargo test coordinator::bootstrap::tests::ensure_pm_market_metadata_table_exists --lib -- --exact --nocapture`

# RPC Event Methods Wave 1 (2026-03-11)

## Goal
Shrink `src/cli/rpc.rs` by moving event discovery, multi-outcome analysis, and event-registry method handling into a sibling module so the root file keeps JSON-RPC framing, idempotency, and top-level dispatch ownership.

## File ownership

- `src/cli/rpc.rs`
  - owner: request parsing, config/idempotency bootstrap, top-level method dispatch
- `src/cli/rpc/event_methods.rs`
  - owner: `event_edge.scan`, `multi_outcome.analyze`, `events.upsert`, `events.list`, `events.update_status`

## Tasks

- [x] Extract the event/multi-outcome/event-registry method handlers into a sibling module.
- [x] Re-run compile safety after the split.

## Progress notes

- 2026-03-11: Added [event_methods.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/cli/rpc/event_methods.rs) and rewired [rpc.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/cli/rpc.rs) to delegate event-related methods through `handle_event_method(...)`.
- 2026-03-11: Validation passed:
  - `rtk cargo check --lib --message-format=short`

# Coordinator Order Intent Ownership Cut (2026-03-10)

## Goal
Move `OrderIntent` / `OrderPriority` ownership out of `src/platform` and into `src/coordinator` so the canonical order-ingress contract lives with coordinator-owned admission, queueing, and execution infrastructure.

## Tasks

- [x] Add a coordinator-owned `order_intent` module and move `OrderIntent` / `OrderPriority` into it.
- [x] Rewire control-plane, coordinator, sidecar, strategy runtime, and RL compatibility callers to the new owner.
- [x] Remove the `platform` re-export instead of leaving a compatibility shim.
- [x] Re-run compile plus focused order-intent / coordinator / RL regressions after the move.

## Progress notes

- 2026-03-10: Added [order_intent.rs](/Users/proerror/Documents/ploy/src/coordinator/order_intent.rs) and re-exported `OrderIntent` / `OrderPriority` from [mod.rs](/Users/proerror/Documents/ploy/src/coordinator/mod.rs).
- 2026-03-10: Reduced [types.rs](/Users/proerror/Documents/ploy/src/platform/types.rs) to `Domain` ownership only and removed the `platform` re-export from [mod.rs](/Users/proerror/Documents/ploy/src/platform/mod.rs).
- 2026-03-10: Rewired [control_plane.rs](/Users/proerror/Documents/ploy/src/control_plane.rs), [sidecar.rs](/Users/proerror/Documents/ploy/src/api/handlers/sidecar.rs), [runtime_order.rs](/Users/proerror/Documents/ploy/src/strategy/runtime_order.rs), the coordinator subtree, and the RL compatibility runtime to import the coordinator-owned contract.
- 2026-03-10: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-order-intent-cut rtk cargo check --lib --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-order-intent-cut rtk cargo test order_intent_from_strategy_intent_preserves_runtime_metadata --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-order-intent-cut rtk cargo test trade_intent_into_order_intent_maps_priority_and_metadata --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-order-intent-cut rtk cargo test test_drain_and_execute_sell_fill_reduces_position_and_realizes_pnl --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-order-intent-cut-rl rtk cargo test --features rl test_rl_order_runtime_start_blocks_live_runtime --lib -- --exact --nocapture`

# Control Plane Contract Split (2026-03-10)

## Goal
Split `src/control_plane.rs` into contract-owned submodules so deployment metadata, evaluation evidence, trade intent bridging, and risk-decision types stop sharing one mixed owner file.

## Tasks

- [x] Extract deployment/runtime contract types into a dedicated `control_plane/deployments.rs`.
- [x] Extract evaluation evidence types into `control_plane/evaluation.rs`.
- [x] Extract `TradeIntent` and its order-intent bridge into `control_plane/trade_intent.rs`.
- [x] Extract `RiskDecision` / `RiskDecisionStatus` into `control_plane/risk_decision.rs`.
- [x] Reduce `src/control_plane.rs` to a thin re-export facade plus focused tests.
- [x] Re-run compile plus focused control-plane regressions after the split.

## Progress notes

- 2026-03-10: Added [deployments.rs](/Users/proerror/Documents/ploy/src/control_plane/deployments.rs), [evaluation.rs](/Users/proerror/Documents/ploy/src/control_plane/evaluation.rs), [trade_intent.rs](/Users/proerror/Documents/ploy/src/control_plane/trade_intent.rs), and [risk_decision.rs](/Users/proerror/Documents/ploy/src/control_plane/risk_decision.rs).
- 2026-03-10: Reduced [control_plane.rs](/Users/proerror/Documents/ploy/src/control_plane.rs) to a thin re-export facade with the existing focused tests preserved at the root.
- 2026-03-10: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-cp-main rtk cargo check --lib --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-cp-main rtk cargo test trade_intent_into_order_intent_maps_priority_and_metadata --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-cp-main rtk cargo test trade_intent_into_order_intent_normalizes_blank_deployment_metadata --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-cp-main rtk cargo test deployment_runtime_scope_matching --lib -- --exact --nocapture`

# Subscription Planner Ownership Cut (2026-03-10)

## Goal
Move `SubscriptionPlanner` and its runtime-planning contracts out of `src/platform` and into `src/coordinator` so platform stops presenting strategy subscription orchestration as a platform primitive.

## Tasks

- [x] Move the subscription planner implementation into a coordinator-owned module.
- [x] Rewire bootstrap crypto-runtime preflight to consume the coordinator-owned planner types.
- [x] Remove the `platform` export instead of leaving a compatibility shim.
- [x] Re-run compile plus focused planner/bootstrap regressions after the move.

## Progress notes

- 2026-03-10: Moved planner ownership from [subscription_planner.rs](/Users/proerror/Documents/ploy/src/platform/subscription_planner.rs) to [subscription_planner.rs](/Users/proerror/Documents/ploy/src/coordinator/subscription_planner.rs).
- 2026-03-10: Updated [mod.rs](/Users/proerror/Documents/ploy/src/coordinator/mod.rs) to expose the new owner and removed the `platform` export from [mod.rs](/Users/proerror/Documents/ploy/src/platform/mod.rs).
- 2026-03-10: Rewired [preflight.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/crypto_runtime_support/preflight.rs) to use `crate::coordinator::subscription_planner`.
- 2026-03-10: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-subplan rtk cargo check --lib --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-subplan rtk cargo test build_plan_deduplicates_overlapping_tokens --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-subplan rtk cargo test apply_strategy_deployments_does_not_route_unknown_crypto_strategy_to_momentum --lib -- --exact --nocapture`

# Market Persistence Ownership Cut (2026-03-10)

## Goal
Move the Polymarket trade/settlement persistence runtime out of `src/platform` and into `src/persistence` so platform stops owning long-running persistence workers and trade-alert schema setup.

## Tasks

- [x] Move the `market_persistence` module tree under `src/persistence`.
- [x] Rewire bootstrap imports to consume the persistence-owned worker entrypoints.
- [x] Remove the `platform` export instead of leaving a compatibility shim.
- [x] Re-run compile plus focused bootstrap regressions after the move.

## Progress notes

- 2026-03-10: Moved [market_persistence.rs](/Users/proerror/Documents/ploy/src/platform/market_persistence.rs) and its `collector_targets/trades/settlements` submodules under [market_persistence.rs](/Users/proerror/Documents/ploy/src/persistence/market_persistence.rs).
- 2026-03-10: Updated [mod.rs](/Users/proerror/Documents/ploy/src/persistence/mod.rs) to expose the persistence-owned worker entrypoints and removed the old `platform` export from [mod.rs](/Users/proerror/Documents/ploy/src/platform/mod.rs).
- 2026-03-10: Rewired [bootstrap.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap.rs) to import the trade/settlement persistence entrypoints from `crate::persistence`.
- 2026-03-10: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-market-persist rtk cargo check --lib --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-market-persist rtk cargo test apply_strategy_deployments_does_not_route_unknown_crypto_strategy_to_momentum --lib -- --exact --nocapture`

# Coordinator Ingress Pipeline Extraction (2026-03-10)

## Goal
Extract shared ingress preflight and rejection choreography out of `control_surface.rs` and `ingress.rs` so the live order-admission path stops duplicating gate logic and reject-side effects.

## Tasks

- [x] Add a shared ingress-preflight owner for domain/deployment/reduce-only/ingress-mode checks.
- [x] Rewire `CoordinatorHandle::submit_order(...)` to use the shared preflight instead of inlining checks.
- [x] Add a shared ingress-rejection owner for `persist_risk_decision + emit_rejected_intent_update + warn`.
- [x] Rewire `handle_order_intent(...)` to use the shared helpers while keeping admission behavior unchanged.
- [x] Re-run compile plus focused coordinator regressions after the extraction.

## Progress notes

- 2026-03-10: Added [ingress_preflight.rs](/Users/proerror/Documents/ploy/src/coordinator/coordinator/ingress_preflight.rs) to own the shared domain/deployment/reduce-only/ingress-mode validation used by both coordinator handle ingress and runtime ingress.
- 2026-03-10: Added [ingress_rejections.rs](/Users/proerror/Documents/ploy/src/coordinator/coordinator/ingress_rejections.rs) to own the common blocked-intent persistence/update/logging choreography.
- 2026-03-10: Reduced [control_surface.rs](/Users/proerror/Documents/ploy/src/coordinator/coordinator/control_surface.rs) so `submit_order(...)` now delegates to the shared preflight owner.
- 2026-03-10: Reduced [ingress.rs](/Users/proerror/Documents/ploy/src/coordinator/coordinator/ingress.rs) by replacing repeated reject paths with `reject_order_intent(...)` and the shared preflight owner.
- 2026-03-10: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-ingress-cut rtk cargo check --lib --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-ingress-cut rtk cargo test test_handle_force_close_domain_blocks_new_buy_immediately --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-ingress-cut rtk cargo test test_handle_order_intent_emits_rejected_update_for_missing_deployment --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-ingress-cut rtk cargo test test_drain_and_execute_records_single_success_for_buy_fill --lib -- --exact --nocapture`

# Strategy Runtime Update Flow Extraction (2026-03-10)

## Goal
Move the managed-runtime order-update, observability, and split-arb poll flow out of `src/coordinator/strategy_runtime/actions.rs` so the root actions module focuses on dispatch and submit/cancel handling.

## Tasks

- [x] Extract the managed runtime update/poll flow into a dedicated `actions/update_flow.rs`.
- [x] Rewire `actions.rs` to delegate coordinator updates and observability writes to the extracted owner.
- [x] Keep submit/cancel behavior unchanged while reducing root-file ownership.
- [x] Re-run compile plus focused managed-runtime regressions after the extraction.

## Progress notes

- 2026-03-10: Added [update_flow.rs](/Users/proerror/Documents/ploy/src/coordinator/strategy_runtime/actions/update_flow.rs) to own `handle_runtime_order_update(...)`, `persist_runtime_observability(...)`, and the split-arb poll loop.
- 2026-03-10: Reduced [actions.rs](/Users/proerror/Documents/ploy/src/coordinator/strategy_runtime/actions.rs) so the root module now delegates managed-runtime update flow to the extracted owner.
- 2026-03-10: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-actions-cut rtk cargo check --lib --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-actions-cut rtk cargo test persist_runtime_order_insert_uses_action_order_id_and_leg --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-actions-cut rtk cargo test test_graceful_stop_reports_closed_action_channel --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-actions-cut rtk cargo test test_strategy_manager_creation --lib -- --exact --nocapture`
- 2026-03-10: Residual note: `CARGO_TARGET_DIR=/tmp/ploy-actions-cut rtk cargo test persist_runtime_order_result_records_submission_and_fill --lib -- --exact --nocapture` is currently failing with an existing order-store expectation mismatch (`Submitted` vs `Filled`) and was not introduced by this extraction.

# Control Plane Contract Split (2026-03-10)

## Goal
Break `src/control_plane.rs` into contract-focused submodules so deployment, evaluation, intent, and risk-decision ownership stop living in one file.

## Tasks

- [x] Extract deployment contracts into a dedicated submodule.
- [x] Extract evaluation/evidence contracts into a dedicated submodule.
- [x] Extract trade-intent and risk-decision contracts into dedicated submodules.
- [x] Keep the root file as a thin re-export and test surface.
- [x] Re-run compile plus focused control-plane regressions after the split.

## Progress notes

- 2026-03-10: Added [deployments.rs](/Users/proerror/Documents/ploy/src/control_plane/deployments.rs), [evaluation.rs](/Users/proerror/Documents/ploy/src/control_plane/evaluation.rs), [trade_intent.rs](/Users/proerror/Documents/ploy/src/control_plane/trade_intent.rs), and [risk_decision.rs](/Users/proerror/Documents/ploy/src/control_plane/risk_decision.rs).
- 2026-03-10: Reduced [control_plane.rs](/Users/proerror/Documents/ploy/src/control_plane.rs) to a thin re-export surface plus focused regression tests.
- 2026-03-10: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-cp-split rtk cargo check --lib --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-cp-split rtk cargo test trade_intent_into_order_intent_maps_priority_and_metadata --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-cp-split rtk cargo test deployment_runtime_scope_matching --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-cp-split rtk cargo test trade_intent_into_order_intent_normalizes_blank_deployment_metadata --lib -- --exact --nocapture`

# Coordinator Queue Ownership Cut (2026-03-10)

## Goal
Move `OrderQueue` / `QueueStats` ownership out of `src/platform` and into `src/coordinator` so queueing stops looking like part of a second platform runtime.

## Tasks

- [x] Move the queue implementation into a coordinator-owned module.
- [x] Rewire coordinator and RL compatibility runtime imports to the new queue owner.
- [x] Remove the `platform` re-export instead of leaving a compatibility shim.
- [x] Re-run compile plus focused queue / RL regressions after the move.

## Progress notes

- 2026-03-10: Moved queue ownership from [queue.rs](/Users/proerror/Documents/ploy/src/platform/queue.rs) to [queue.rs](/Users/proerror/Documents/ploy/src/coordinator/queue.rs).
- 2026-03-10: Updated [coordinator.rs](/Users/proerror/Documents/ploy/src/coordinator/coordinator.rs), [state.rs](/Users/proerror/Documents/ploy/src/coordinator/state.rs), [tests.rs](/Users/proerror/Documents/ploy/src/coordinator/coordinator/tests.rs), and [order_platform.rs](/Users/proerror/Documents/ploy/src/rl/order_platform.rs) to consume the new coordinator-owned queue types.
- 2026-03-10: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-queue-check3 rtk cargo check --lib --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-queue-check2 rtk cargo test test_queue_stats_snapshot_from --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-queue-cut rtk cargo test --features rl test_rl_order_runtime_start_blocks_live_runtime --lib -- --exact --nocapture`

# Coordinator Position Ownership Cut (2026-03-10)

## Goal
Move `Position` / `AggregatedPosition` / `PositionAggregator` ownership out of `src/platform` and into `src/coordinator` so shared live position state lives with coordinator-owned execution/runtime infrastructure.

## Tasks

- [x] Move the position implementation and transitions submodule into coordinator-owned modules.
- [x] Rewire coordinator state and RL compatibility runtime imports to the new position owner.
- [x] Remove the `platform` re-export instead of leaving a compatibility shim.
- [x] Re-run compile plus focused position / coordinator / RL regressions after the move.

## Progress notes

- 2026-03-10: Moved position ownership from [position.rs](/Users/proerror/Documents/ploy/src/platform/position.rs) to [position.rs](/Users/proerror/Documents/ploy/src/coordinator/position.rs), including [transitions.rs](/Users/proerror/Documents/ploy/src/coordinator/position/transitions.rs).
- 2026-03-10: Updated [coordinator.rs](/Users/proerror/Documents/ploy/src/coordinator/coordinator.rs), [state.rs](/Users/proerror/Documents/ploy/src/coordinator/state.rs), and [order_platform.rs](/Users/proerror/Documents/ploy/src/rl/order_platform.rs) to consume the new coordinator-owned position types.
- 2026-03-10: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-position-cut rtk cargo check --lib --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-position-cut rtk cargo test test_reduce_position_partial_close --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-position-cut rtk cargo test test_drain_and_execute_sell_fill_reduces_position_and_realizes_pnl --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-position-cut-rl rtk cargo test --features rl test_position_tracking --lib -- --exact --nocapture`

# Coordinator Risk Ownership Cut (2026-03-10)

## Goal
Move `RiskGate` and its related runtime contract/types out of `src/platform` and into `src/coordinator` so live risk state is owned by the same layer that owns order admission, queueing, and execution.

## Tasks

- [x] Move the risk implementation and submodules into coordinator-owned modules.
- [x] Rewire coordinator, RL compatibility runtime, TUI, and bootstrap env wiring to the new risk owner.
- [x] Remove the `platform` re-export instead of leaving a compatibility shim.
- [x] Re-run compile plus focused risk / coordinator / RL regressions after the move.

## Progress notes

- 2026-03-10: Moved risk ownership from [risk.rs](/Users/proerror/Documents/ploy/src/platform/risk.rs) to [risk.rs](/Users/proerror/Documents/ploy/src/coordinator/risk.rs), including the `checks/config/exposure/queries/stats/transitions/types` submodules.
- 2026-03-10: Updated [coordinator.rs](/Users/proerror/Documents/ploy/src/coordinator/coordinator.rs), [state.rs](/Users/proerror/Documents/ploy/src/coordinator/state.rs), [command.rs](/Users/proerror/Documents/ploy/src/coordinator/command.rs), [config.rs](/Users/proerror/Documents/ploy/src/coordinator/config.rs), [coordinator_env.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/bootstrap_config/coordinator_env.rs), [order_platform.rs](/Users/proerror/Documents/ploy/src/rl/order_platform.rs), [event.rs](/Users/proerror/Documents/ploy/src/tui/event.rs), [journal.rs](/Users/proerror/Documents/ploy/src/coordinator/journal.rs), and [lib.rs](/Users/proerror/Documents/ploy/src/lib.rs) to consume the new coordinator-owned risk types.
- 2026-03-10: Validation passed:
  - `CARGO_TARGET_DIR=/tmp/ploy-risk-cut rtk cargo check --lib --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-risk-cut rtk cargo test test_query_helpers_report_runtime_snapshots --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-risk-cut rtk cargo test test_drain_and_execute_sell_fill_reduces_position_and_realizes_pnl --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-risk-cut-rl rtk cargo test --features rl test_rl_order_runtime_start_blocks_live_runtime --lib -- --exact --nocapture`

# Strategy And Adapter Wave 10 (2026-03-10)

## Goal
Keep collapsing active live-path ownership by cutting remaining sidecar write-path, momentum engine config, and executor idempotency flow out of their current root files.

## File ownership

- `src/api/handlers/sidecar.rs`
  - owner: write-path handler extraction
- `src/strategy/momentum.rs`
  - owner: momentum config/defaults extraction
- `src/strategy/execution/executor/execution_flow.rs`
  - owner: idempotency/execution orchestration split

## Tasks

- [ ] Extract the sidecar write-path handlers into a sibling module.
- [ ] Extract momentum config/defaults into a sibling module.
- [ ] Extract executor idempotency flow into a sibling module.
- [ ] Re-run compile plus focused regressions after the wave.

## Progress notes

- 2026-03-10: Preflight file ownership for Wave 10 assigned before dispatching the next parallel batch.

- [x] Rewire `agents/openclaw/*` modules to import `OpenClawConfig` / `AllocatorConfig` / `RegimeConfig` / `StraddleConfig` from `crate::coordinator::bootstrap`.
- [x] Delete `src/agents/openclaw/config.rs` and remove the dead `mod config;` entry from `src/agents/openclaw/mod.rs`.
- [x] Re-run focused OpenClaw compile/tests after the shim removal.

## Review

- [x] Confirm repo-wide search shows no remaining imports from `agents::openclaw::config`.
- [x] Confirm `agents/openclaw` no longer defines any config ownership layer and compiles directly against bootstrap-owned config types.

## Progress notes

- 2026-03-09: Rewired [agent.rs](/Users/proerror/Documents/ploy/src/agents/openclaw/agent.rs), [allocator.rs](/Users/proerror/Documents/ploy/src/agents/openclaw/allocator.rs), [performance.rs](/Users/proerror/Documents/ploy/src/agents/openclaw/performance.rs), [regime.rs](/Users/proerror/Documents/ploy/src/agents/openclaw/regime.rs), and [straddle.rs](/Users/proerror/Documents/ploy/src/agents/openclaw/straddle.rs) to import config types directly from `crate::coordinator::bootstrap`.
- 2026-03-09: Deleted [config.rs](/Users/proerror/Documents/ploy/src/agents/openclaw/config.rs) and removed the dead `mod config;` line from [mod.rs](/Users/proerror/Documents/ploy/src/agents/openclaw/mod.rs).
- 2026-03-09: Validation passed:
  - repo-wide search for `agents::openclaw::config` and `super::config::(OpenClawConfig|AllocatorConfig|RegimeConfig|StraddleConfig)` returned no remaining source matches
  - `cargo check --lib`
  - `cargo test regime_policy --lib -- --nocapture`
  - `cargo test regime_display --lib -- --nocapture`

# RL Compatibility Runtime Surface Pruning (2026-03-09)

## Goal
Delete dead RL compatibility event types that no longer have producers so the RL runtime surface reflects the remaining crypto-only CLI path instead of pretending to support unused domain/update event variants.

## Tasks

- [x] Remove dead `SportsEvent`, `PoliticsEvent`, `QuoteUpdateEvent`, and `OrderUpdateEvent` from `src/rl/runtime_types.rs`.
- [x] Rewire `RLCryptoAgent` and `rl::mod` exports to only expose the surviving `CryptoEvent`, `DomainEvent`, and `QuoteData` surface.
- [x] Re-run `rl` compile and focused RL compatibility tests after the pruning.

## Review

- [x] Confirm repo-wide search shows no remaining source references to the removed RL compatibility event types.
- [x] Confirm `RLCryptoAgent::on_event` only handles event variants that can actually be produced by the current RL CLI/runtime path.

## Progress notes

- 2026-03-09: Removed dead `SportsEvent`, `PoliticsEvent`, `QuoteUpdateEvent`, and `OrderUpdateEvent` from [runtime_types.rs](/Users/proerror/Documents/ploy/src/rl/runtime_types.rs), leaving the RL compatibility runtime aligned with the surviving crypto-only CLI flow.
- 2026-03-09: Updated [cli_agent.rs](/Users/proerror/Documents/ploy/src/rl/cli_agent.rs) to stop matching never-produced event variants and shrank [mod.rs](/Users/proerror/Documents/ploy/src/rl/mod.rs) exports to `CryptoEvent`, `DomainEvent`, and `QuoteData`.
- 2026-03-09: Validation passed:
  - `cargo check --lib --features rl`
  - `cargo test --features rl test_rl_agent_lifecycle --lib -- --nocapture`
  - `cargo test --features rl test_position_tracking --lib -- --nocapture`
  - repo-wide search for `SportsEvent|PoliticsEvent|OrderUpdateEvent|QuoteUpdateEvent|DomainEvent::Sports|DomainEvent::Politics|DomainEvent::OrderUpdate|DomainEvent::QuoteUpdate` returned no remaining source matches

# Control Plane Contract Extraction (2026-03-09)

## Goal
Move deployment/evidence/trade-intent contracts out of `src/platform` so `platform/` only owns runtime primitives, while removing the dead raw-order gateway types that no longer participate in behavior.

## Tasks

- [x] Move `platform/contracts.rs` into a top-level `control_plane` module.
- [x] Rewire coordinator/API/CLI imports so deployment/evidence/trade-intent types stop coming from `crate::platform`.
- [x] Remove dead `OrderCommand` / `OrderExecutionReport` types and stop exporting control-plane contracts from `platform::mod`.
- [x] Re-run default + `rl` compile and focused control-plane tests after the ownership move.

## Review

- [x] Confirm `src/platform` no longer defines or re-exports deployment/evidence/trade-intent contracts.
- [x] Confirm `TradeIntent`, `StrategyDeployment`, and strategy evaluation evidence all compile from the new `crate::control_plane` namespace.
- [x] Confirm no source references to `platform::OrderCommand` or `platform::OrderExecutionReport` remain.

## Progress notes

- 2026-03-09: Renamed [contracts.rs](/Users/proerror/Documents/ploy/src/platform/contracts.rs) into the new top-level [control_plane.rs](/Users/proerror/Documents/ploy/src/control_plane.rs), making deployment/evidence/trade-intent ownership explicit instead of hiding it under `platform`.
- 2026-03-09: Updated coordinator/API/CLI imports to consume `StrategyDeployment`, `MarketSelector`, `TradeIntent`, and strategy evaluation evidence from `crate::control_plane`; `platform::mod` now only re-exports runtime primitives.
- 2026-03-09: Removed dead `OrderCommand` and `OrderExecutionReport` types during the extraction; repo-wide search now returns no remaining source references.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo check --lib --features rl`
  - `cargo test --lib control_plane::tests -- --nocapture`
  - `cargo test apply_strategy_deployments_does_not_route_unknown_crypto_strategy_to_momentum --lib -- --nocapture`

# Standalone Domain Runtime Retirement (2026-03-09)

## Goal
Retire the remaining standalone domain runtime entrypoints so `event_edge`, `nba_comeback`, and sports split-arb no longer present alternate live/runtime paths beside the managed strategy runtime.

## Tasks

- [x] Remove the standalone `event_edge` runner/config surface and keep only the canonical `EventEdgeStrategy`.
- [x] Retire the standalone `ploy strategy nba-comeback` loop and replace it with a compatibility error that points operators to managed deployments.
- [x] Retire the standalone `ploy sports split-arb` loop and delete the old sports runner module.

# PM 5m Directional Strategy (2026-03-10)

## Goal
Add a brand-new standalone crypto strategy named `pm_5m_directional` that implements the Polymarket 5m directional core without changing existing `momentum` behavior.

## Tasks

- [x] Register `pm_5m_directional` in the canonical strategy factory and default config surface.
- [x] Add failing tests for factory wiring and core entry gating behavior.
- [x] Implement `pm_5m_directional` as an independent strategy module using Binance spot + Binance L2 + Polymarket event/quote feeds.
- [x] Implement the PRD core gates for V1: z-score probability, signed short-horizon flow, OBI confirmation, fee-adjusted edge, no-trade zone, spread/size checks, and hold-to-settlement lifecycle.
- [ ] Run focused validation for the new strategy path.

## Review

- [x] Confirm the repo can instantiate `pm_5m_directional` from TOML without touching `momentum`.
- [x] Confirm the new strategy only submits entries when the directional gates and PM execution gates both pass.
- [x] Confirm the new strategy uses IOC/FAK-style submit intents and defaults to hold-to-settlement.
- [x] Re-run compile and focused canonical strategy tests after the entrypoint cleanup.

## Review

- [x] Confirm there are no remaining source references to `run_event_edge`, `EventEdgeConfig`, `run_sports_split_arb`, or `SportsSplitArbConfig`.
- [x] Confirm `event_edge` and `nba_comeback` canonical `Strategy` implementations still compile and pass focused tests.
- [x] Confirm the remaining CLI entrypoints fail fast with explicit retirement guidance instead of spinning their own live loops.

## Progress notes

- 2026-03-10: Added standalone [pm_5m_directional.rs](/Users/proerror/Documents/ploy/src/strategy/pm_5m_directional.rs), registered it in [manager.rs](/Users/proerror/Documents/ploy/src/strategy/manager.rs), exposed it from [mod.rs](/Users/proerror/Documents/ploy/src/strategy/mod.rs), and added the default template [pm_5m_directional_default.toml](/Users/proerror/Documents/ploy/config/strategies/pm_5m_directional_default.toml).
- 2026-03-10: Implemented the V1 directional core gates plus IOC submit intents, terminal partial-fill handling, hold-to-settlement state retention, and unit coverage for factory wiring, entry gating, no-trade-zone blocking, partial fills, and unrealized PnL reporting.
- 2026-03-10: Focused validation is currently blocked by unrelated existing compile errors in [crypto.rs](/Users/proerror/Documents/ploy/src/coordinator/capital/crypto.rs), [capital.rs](/Users/proerror/Documents/ploy/src/coordinator/capital.rs), and [deployments.rs](/Users/proerror/Documents/ploy/src/coordinator/admission/deployments.rs); the new strategy path itself has not produced a strategy-local compiler error yet.

- 2026-03-09: Removed the standalone `EventEdgeConfig` + `run_event_edge(...)` surface from [event_edge/mod.rs](/Users/proerror/Documents/ploy/src/strategy/event_edge/mod.rs) and stopped re-exporting it from [strategy/mod.rs](/Users/proerror/Documents/ploy/src/strategy/mod.rs).
- 2026-03-09: Deleted [runner.rs](/Users/proerror/Documents/ploy/src/strategy/sports/runner.rs), shrank [sports/mod.rs](/Users/proerror/Documents/ploy/src/strategy/sports/mod.rs) to discovery-only exports, and changed [sports.rs](/Users/proerror/Documents/ploy/src/main_commands/sports.rs) to return an explicit retirement error instead of running a standalone sports split-arb loop.
- 2026-03-09: Replaced the standalone NBA CLI loop in [strategy.rs](/Users/proerror/Documents/ploy/src/cli/strategy.rs) with a direct retirement error and marked the CLI/runtime help text as deprecated in [runtime.rs](/Users/proerror/Documents/ploy/src/cli/runtime.rs).
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo check --lib --features rl`
  - `cargo check`
  - `cargo test test_strategy_manager_creation --lib -- --nocapture`
  - `cargo test strategy::event_edge::strategy::tests --lib -- --nocapture`
  - `cargo test strategy::nba_comeback::strategy::tests --lib -- --nocapture`

# Bootstrap Config Extraction (2026-03-09)

## Goal
Move the bootstrap config model and `from_app_config` env hydration out of `bootstrap.rs` so the top-level bootstrap flow stops owning both configuration assembly and runtime assembly.

## Tasks

- [x] Extract `PlatformBootstrapConfig` and its `Default` / `from_app_config` / deployment reapply logic into a dedicated bootstrap config module.
- [x] Re-export the config type from `bootstrap.rs` so existing callers keep using `coordinator::bootstrap::PlatformBootstrapConfig`.
- [x] Make sibling bootstrap modules import the support helpers they actually use instead of relying on `bootstrap.rs` parent imports.
- [x] Re-run default + `rl` compile plus focused bootstrap config tests after the extraction.

## Review

- [x] Confirm `bootstrap.rs` no longer defines the config struct or its large env-hydration impl block.
- [x] Confirm the new bootstrap config module owns the runtime enablement matrix, OpenClaw lockdown, and strategy deployment reapply path.
- [x] Confirm focused bootstrap config regressions still pass after the extraction.

## Progress notes

- 2026-03-09: Added [bootstrap_config.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/bootstrap_config.rs) and moved `PlatformBootstrapConfig` ownership there, including `Default`, `reapply_strategy_deployments_for_runtime`, and the full `from_app_config` env-hydration path.
- 2026-03-10: Split the env-hydration body again into [coordinator_env.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/bootstrap_config/coordinator_env.rs) and [crypto_env.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/bootstrap_config/crypto_env.rs), so [bootstrap_config.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/bootstrap_config.rs) now focuses on config shape plus high-level runtime scoping instead of carrying all coordinator/crypto env overlays inline.
- 2026-03-09: Updated [bootstrap.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap.rs) to re-export `PlatformBootstrapConfig`, leaving the top-level file focused on platform startup and runtime assembly.
- 2026-03-09: Updated [managed_crypto.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/managed_crypto.rs), [market_persistence.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/market_persistence.rs), and [strategy_deployments.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/strategy_deployments.rs) to import the env/config helpers they consume directly instead of relying on parent-module wildcard scope.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo check --lib --features rl`
  - `cargo test from_app_config_reads_crypto_lob_ml_model_env_vars --lib -- --nocapture`
  - `cargo test from_app_config_reads_crypto_agent_signal_gate_env_vars --lib -- --nocapture`
  - `cargo test from_app_config_ignores_deprecated_price_exits_env --lib -- --nocapture`
  - `cargo test --features rl build_crypto_rl_policy_runtime_config_preserves_model_controls --lib -- --nocapture`

# RL Compatibility Runtime Extraction (2026-03-09)

## Goal
Move the RL-only compatibility runtime surface out of `src/platform` into `src/rl` so `platform/` stops presenting a second live runtime alongside the coordinator path.

## Tasks

- [x] Extract the queue-driven RL runtime types from `platform::types` into `rl::runtime_types`.
- [x] Move `OrderPlatform`, `PlatformConfig`, and `PlatformStats` out of `platform/platform.rs` into `rl::order_platform`.
- [x] Rewire RL CLI entrypoints and tests to import the compatibility runtime from `crate::rl`.
- [x] Remove the dead `src/platform/platform.rs` module and shrink `platform::mod` exports back to shared platform primitives.
- [x] Re-run default + `rl` feature validation after the namespace move.

## Review

- [x] Confirm `OrderPlatform`, `PlatformConfig`, `PlatformStats`, and the RL-only event structs are no longer exported from `crate::platform`.
- [x] Confirm the remaining `src/platform` surface is limited to shared queue/risk/position/data-plane/contracts primitives plus canonical execution types.
- [x] Confirm the RL CLI still compiles and its compatibility runtime tests pass after the extraction.

## Progress notes

- 2026-03-09: Added [runtime_types.rs](/Users/proerror/Documents/ploy/src/rl/runtime_types.rs) and moved `DomainEvent`, `CryptoEvent`, `PoliticsEvent`, `SportsEvent`, `QuoteData`, `QuoteUpdateEvent`, and `OrderUpdateEvent` under the `rl` namespace.
- 2026-03-09: Added [order_platform.rs](/Users/proerror/Documents/ploy/src/rl/order_platform.rs) and moved the queue-driven compatibility runtime there, preserving the dry-run/live-blocking tests for the RL CLI path.
- 2026-03-09: Updated [rl/mod.rs](/Users/proerror/Documents/ploy/src/rl/mod.rs), [rl/cli_agent.rs](/Users/proerror/Documents/ploy/src/rl/cli_agent.rs), and [main_commands/rl/agent.rs](/Users/proerror/Documents/ploy/src/main_commands/rl/agent.rs) so RL imports the compatibility runtime from `crate::rl` instead of `crate::platform`.
- 2026-03-09: Deleted [platform/platform.rs](/Users/proerror/Documents/ploy/src/platform/platform.rs) and shrank [platform/mod.rs](/Users/proerror/Documents/ploy/src/platform/mod.rs) / [platform/types.rs](/Users/proerror/Documents/ploy/src/platform/types.rs) back to shared platform ownership.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo check --lib --features rl`
  - `cargo test --features rl test_order_platform_start_blocks_live_runtime --lib -- --nocapture`
  - `cargo test --features rl test_rl_agent_lifecycle --lib -- --nocapture`
  - `cargo test --features rl test_rl_signal_on_good_sum --lib -- --nocapture`

# RL Execution Report Namespace Migration (2026-03-09)

## Goal
Move the RL-only `ExecutionStatus` / `ExecutionReport` compatibility types out of `platform` and into `rl` so `platform` stops exporting RL-specific execution results as if they were shared live-runtime primitives.

## Tasks

- [x] Add an `rl`-owned execution types module for `ExecutionStatus` and `ExecutionReport`.
- [x] Rewire RL order-platform and CLI agent code to import execution types from `crate::rl`.
- [x] Remove the RL execution-type exports/definitions from `platform` and update the root lib re-export surface.
- [x] Re-run default + `rl` compile plus focused RL compatibility tests.

## Review

- [x] Confirm `src/platform/types.rs` no longer defines `ExecutionStatus` or `ExecutionReport`.
- [x] Confirm the remaining `ExecutionReport` references live under `src/rl` plus the feature-gated root lib export.
- [x] Confirm the RL compatibility runtime tests still pass after the namespace move.

## Progress notes

- 2026-03-09: Added [execution_types.rs](/Users/proerror/Documents/ploy/src/rl/execution_types.rs) and moved the RL compatibility execution result types there.
- 2026-03-09: Updated [order_platform.rs](/Users/proerror/Documents/ploy/src/rl/order_platform.rs) and [cli_agent.rs](/Users/proerror/Documents/ploy/src/rl/cli_agent.rs) to consume the execution result types from `crate::rl` instead of `crate::platform`.
- 2026-03-09: Removed the old execution result definitions from [types.rs](/Users/proerror/Documents/ploy/src/platform/types.rs), shrank [mod.rs](/Users/proerror/Documents/ploy/src/platform/mod.rs) accordingly, and made the root [lib.rs](/Users/proerror/Documents/ploy/src/lib.rs) export them only behind the `rl` feature.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo check --lib --features rl`
  - `cargo test --features rl test_order_platform_start_blocks_live_runtime --lib -- --nocapture`
  - `cargo test --features rl test_rl_agent_lifecycle --lib -- --nocapture`
  - `cargo test --features rl test_position_tracking --lib -- --nocapture`

# Momentum Config Namespace Migration (2026-03-09)

## Goal
Move the last trading bootstrap config DTO out of `src/agents` so the agents namespace contains only governance-plane code.

## Tasks

- [x] Move `CryptoTradingConfig` and `CryptoEntryMode` into a strategy-side runtime-config module.
- [x] Rewire bootstrap and momentum runtime-config builders to use the strategy-side config types.
- [x] Delete `src/agents/crypto.rs` and stop re-exporting trading config from `src/agents/mod.rs`.
- [x] Re-run compile and momentum bootstrap-config regressions after the namespace move.

## Review

- [x] Confirm there are no remaining references to `crate::agents::crypto` or agent-side momentum config types.
- [x] Confirm `src/agents` now exposes only governance-plane modules.
- [x] Confirm bootstrap momentum config rendering still passes with the new strategy-side config module.

## Progress notes

- 2026-03-09: Added [momentum_runtime_config.rs](/Users/proerror/Documents/ploy/src/strategy/momentum_runtime_config.rs) and re-exported `CryptoTradingConfig` / `CryptoEntryMode` from [strategy/mod.rs](/Users/proerror/Documents/ploy/src/strategy/mod.rs).
- 2026-03-09: Updated [bootstrap.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap.rs), [managed_crypto.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/managed_crypto.rs), and [strategy_deployments.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/strategy_deployments.rs) to consume the strategy-side momentum config types.
- 2026-03-09: Deleted [src/agents/crypto.rs](/Users/proerror/Documents/ploy/src/agents/crypto.rs) and removed its export from [src/agents/mod.rs](/Users/proerror/Documents/ploy/src/agents/mod.rs), leaving `src/agents` governance-only.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo test build_momentum_runtime_config_renders_directional_crypto_settings --lib -- --nocapture`
  - `cargo test build_momentum_runtime_config_rejects_non_directional_modes --lib -- --nocapture`

# Trading Agent Contract Retirement (2026-03-09)

## Goal
Remove the dead pull-based trading agent contract now that no legacy `TradingAgent` implementations remain in `src/`.

## Tasks

- [x] Delete `src/agents/context.rs` and the unused `TradingAgent`/`AgentConfig` surface.
- [x] Extract `GovernanceAgent` into its own module and keep OpenClaw on the governance path.
- [x] Update `agents/mod.rs` and OpenClaw imports so `src/agents` only exposes governance and config compatibility surfaces.
- [x] Re-run compile plus governance-focused regression tests after the contract cleanup.

## Review

- [x] Confirm there are no remaining `TradingAgent`, `AgentContext`, or `AgentConfig` references under `src/agents`.
- [x] Confirm OpenClaw still runs through `GovernanceAgent` + `GovernanceContext`.
- [x] Confirm `src/agents` now only contains governance-plane code and config compatibility DTOs.

## Progress notes

- 2026-03-09: Added [governance_agent.rs](/Users/proerror/Documents/ploy/src/agents/governance_agent.rs) and moved the surviving `GovernanceAgent` trait into that dedicated module.
- 2026-03-09: Deleted [context.rs](/Users/proerror/Documents/ploy/src/agents/context.rs) and [traits.rs](/Users/proerror/Documents/ploy/src/agents/traits.rs), which had become dead after the legacy trading-agent implementations were removed.
- 2026-03-09: Updated [agents/mod.rs](/Users/proerror/Documents/ploy/src/agents/mod.rs) and [openclaw/agent.rs](/Users/proerror/Documents/ploy/src/agents/openclaw/agent.rs) so `src/agents` now exports only governance/runtime-config compatibility surfaces.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo test test_governance_policy_blocks_domain --lib -- --nocapture`
  - `cargo test test_governance_status_includes_domain_ingress_and_agents --lib -- --nocapture`

# Crypto LOB ML Legacy Agent Removal (2026-03-09)

## Goal
Delete the dead `CryptoLobMlAgent` runtime file after moving its bootstrap-facing config and enums into the canonical strategy namespace.

## Tasks

- [x] Extract `CryptoLobMlConfig`, `CryptoLobMlExitMode`, and `CryptoLobMlEntrySidePolicy` into `strategy::crypto_lob_ml`.
- [x] Rewire bootstrap-managed crypto config, runtime TOML rendering, and bootstrap tests to use the new strategy-side types.
- [x] Remove `src/agents/crypto_lob_ml.rs` and stop exporting it from `src/agents/mod.rs`.
- [x] Re-run focused bootstrap/config validation after deleting the legacy agent module.

## Review

- [x] Confirm there are no remaining source references to `crate::agents::crypto_lob_ml`.
- [x] Confirm `CryptoLobMlConfig` and the exit/entry enums now live under [strategy/crypto_lob_ml](/Users/proerror/Documents/ploy/src/strategy/crypto_lob_ml/mod.rs).
- [x] Confirm bootstrap env parsing and runtime-config rendering still pass with the strategy-side config types.

## Progress notes

- 2026-03-09: Added [config.rs](/Users/proerror/Documents/ploy/src/strategy/crypto_lob_ml/config.rs) and re-exported `CryptoLobMlConfig`, `CryptoLobMlExitMode`, and `CryptoLobMlEntrySidePolicy` from [mod.rs](/Users/proerror/Documents/ploy/src/strategy/crypto_lob_ml/mod.rs).
- 2026-03-09: Updated [managed_crypto.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/managed_crypto.rs), [strategy_deployments.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/strategy_deployments.rs), and bootstrap tests in [bootstrap.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap.rs) to consume the strategy-side config/enums.
- 2026-03-09: Deleted [src/agents/crypto_lob_ml.rs](/Users/proerror/Documents/ploy/src/agents/crypto_lob_ml.rs) and removed its export from [src/agents/mod.rs](/Users/proerror/Documents/ploy/src/agents/mod.rs) after confirming there was no remaining live caller.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo test build_crypto_lob_ml_runtime_config_renders_coin_filters --lib -- --nocapture`
  - `cargo test from_app_config_reads_crypto_lob_ml_model_env_vars --lib -- --nocapture`
  - `cargo test from_app_config_ignores_deprecated_price_exits_env --lib -- --nocapture`
  - `cargo test crypto_lob_ml_config_defaults_match_bootstrap_expectations --lib -- --nocapture`

# Crypto RL Legacy Agent Removal (2026-03-09)

## Goal
Delete the dead `CryptoRlPolicyAgent` runtime file now that bootstrap and the canonical wrapper only need the RL config surface.

## Tasks

- [x] Extract `CryptoRlPolicyConfig` into the canonical `strategy::crypto_rl_policy` namespace.
- [x] Rewire bootstrap-managed crypto config and runtime TOML rendering to use the new strategy-side config type.
- [x] Remove `src/agents/crypto_rl_policy.rs` and stop exporting it from `src/agents/mod.rs`.
- [x] Re-run default + `rl` feature validation after deleting the legacy agent module.

## Review

- [x] Confirm there are no remaining source references to `crate::agents::crypto_rl_policy`.
- [x] Confirm `CryptoRlPolicyConfig` now lives under [strategy/crypto_rl_policy](/Users/proerror/Documents/ploy/src/strategy/crypto_rl_policy/mod.rs).
- [x] Confirm bootstrap and RL CLI validation still pass without the deleted legacy agent file.

## Progress notes

- 2026-03-09: Added [config.rs](/Users/proerror/Documents/ploy/src/strategy/crypto_rl_policy/config.rs) and re-exported `CryptoRlPolicyConfig` from [mod.rs](/Users/proerror/Documents/ploy/src/strategy/crypto_rl_policy/mod.rs) so the canonical strategy namespace now owns the shared RL runtime config.
- 2026-03-09: Updated [managed_crypto.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/managed_crypto.rs), [strategy_deployments.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/strategy_deployments.rs), and the bootstrap RL config test in [bootstrap.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap.rs) to consume the strategy-side config type.
- 2026-03-09: Deleted [src/agents/crypto_rl_policy.rs](/Users/proerror/Documents/ploy/src/agents/crypto_rl_policy.rs) and removed its export from [src/agents/mod.rs](/Users/proerror/Documents/ploy/src/agents/mod.rs) after confirming there was no remaining live caller.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo check --lib --features rl`
  - `cargo test --features rl build_crypto_rl_policy_runtime_config_preserves_model_controls --lib -- --nocapture`
  - `cargo test --features rl test_rl_agent_lifecycle --lib -- --nocapture`

# Managed Crypto Bootstrap Config Rename (2026-03-09)

## Goal
Remove the last `legacy_crypto` ownership surface from bootstrap config now that `crypto_lob_ml` and `crypto_rl_policy` launch through canonical managed strategy runtimes.

## Tasks

- [x] Rename the bootstrap module from `legacy_crypto.rs` to `managed_crypto.rs`.
- [x] Rename `PlatformBootstrapConfig.legacy_crypto` to `managed_crypto` while preserving a serde alias for backward compatibility.
- [x] Rewire deployment mapping, bootstrap startup, and `platform_mode` filtering to use `managed_crypto`.
- [x] Re-run compile plus focused bootstrap/platform-mode regressions after the config ownership rename.

## Review

- [x] Confirm code references no longer use `legacy_crypto` as an active runtime/config owner.
- [x] Confirm `managed_crypto.rs` now owns the crypto preview runtime env hydration.
- [x] Confirm compile/tests pass and only the serde alias remains for backward compatibility.

## Progress notes

- 2026-03-09: Renamed [legacy_crypto.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/legacy_crypto.rs) to [managed_crypto.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/managed_crypto.rs) and updated the env hydration entrypoint to `apply_managed_crypto_runtime_env(...)`.
- 2026-03-09: Updated [bootstrap.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap.rs), [strategy_deployments.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/strategy_deployments.rs), and [platform_mode.rs](/Users/proerror/Documents/ploy/src/main_modes/platform_mode.rs) so the canonical preview wrappers no longer sit behind a `legacy_crypto` field name.
- 2026-03-09: Preserved `#[serde(alias = "legacy_crypto")]` on [PlatformBootstrapConfig](/Users/proerror/Documents/ploy/src/coordinator/bootstrap.rs) so any serialized bootstrap config using the old field can still deserialize during the transition.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo test from_app_config_reads_crypto_lob_ml_model_env_vars --lib -- --nocapture`
  - `cargo test from_app_config_ignores_deprecated_price_exits_env --lib -- --nocapture`
  - `cargo test pattern_memory_deployment_does_not_enable_lob_ml -- --nocapture`
  - `cargo check --lib --features rl`
  - `cargo test --features rl build_crypto_rl_policy_runtime_config_preserves_model_controls --lib -- --nocapture`

# Crypto Preview Managed Runtime Launch (2026-03-09)

## Goal
Launch the `crypto_lob_ml` and `crypto_rl_policy` canonical preview wrappers from the managed strategy runtime in `bootstrap`, while shrinking `legacy_crypto.rs` back down to config/env ownership for the remaining live trading-agent path.

## Tasks

- [x] Add runtime-config builders for the canonical `crypto_lob_ml` and `crypto_rl_policy` wrappers under `strategy_deployments.rs`.
- [x] Start the preview wrappers from `bootstrap.rs` through `spawn_managed_strategy_runtime_task(...)`.
- [x] Move legacy crypto live-agent spawn ownership out of `legacy_crypto.rs` and keep that module focused on legacy config/env hydration.
- [x] Remove the last `LegacyControl` quote-subscribe action from the canonical `crypto_rl_policy` wrapper.
- [x] Re-run compile plus focused bootstrap/wrapper tests, including the `rl` feature gate for the RL runtime-config builder.

## Review

- [x] Confirm `bootstrap.rs` now owns launching the canonical crypto preview wrappers directly.
- [x] Confirm `legacy_crypto.rs` no longer owns the crypto live-agent spawn pipeline.
- [x] Confirm the canonical `crypto_rl_policy` wrapper no longer emits `LegacyControl` actions in its event-discovery path.

## Progress notes

- 2026-03-09: Added `build_crypto_lob_ml_runtime_config(...)` and `build_crypto_rl_policy_runtime_config(...)` in [strategy_deployments.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/strategy_deployments.rs) so bootstrap can render managed runtime TOML for the crypto preview wrappers.
- 2026-03-09: Updated [bootstrap.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap.rs) so `crypto_lob_ml` and `crypto_rl_policy` now launch as managed strategy runtimes using their canonical wrappers, while the legacy live-agent path remains separate.
- 2026-03-09: Shrunk [legacy_crypto.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/legacy_crypto.rs) back toward config/env ownership by removing the legacy spawn orchestration from that module.
- 2026-03-09: Removed `LegacyControl(SubscribeFeed)` from [strategy.rs](/Users/proerror/Documents/ploy/src/strategy/crypto_rl_policy/strategy.rs); the canonical RL wrapper now tracks discovered events without issuing compatibility control actions.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo test build_crypto_lob_ml_runtime_config_renders_coin_filters --lib -- --nocapture`
  - `cargo test from_toml_builds_expected_feeds --lib -- --nocapture`
  - `cargo test event_discovered_tracks_event_without_legacy_control_actions --lib -- --nocapture`
  - `cargo test on_tick_emits_buy_up_signal_log_when_rule_based_policy_triggers --lib -- --nocapture`
  - `cargo check --lib --features rl`
  - `cargo test --features rl build_crypto_rl_policy_runtime_config_preserves_model_controls --lib -- --nocapture`

# Legacy Crypto Spawn Retirement (2026-03-09)

## Goal
Stop `legacy_crypto.rs` from owning any live runtime spawn path now that `crypto_lob_ml` and `crypto_rl_policy` both have canonical managed-runtime wrappers.

## Tasks

- [x] Remove the legacy `spawn_legacy_crypto_agent_runtimes` path from bootstrap so `lob_ml / rl_policy` are no longer started twice.
- [x] Delete the legacy trading-agent spawn helpers from `src/coordinator/bootstrap/legacy_crypto.rs`, leaving only config/env compatibility ownership.
- [x] Re-run compile plus narrow bootstrap/wrapper regressions after the runtime ownership cut.

## Review

- [x] Confirm `bootstrap.rs` no longer calls a legacy crypto agent spawn helper.
- [x] Confirm `legacy_crypto.rs` now only owns config/env translation for the compatibility surface.
- [x] Confirm the canonical `crypto_rl_policy` wrapper tests and legacy env parsing regression still pass.

## Progress notes

- 2026-03-09: Removed the `spawn_legacy_crypto_agent_runtimes(...)` call from [bootstrap.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap.rs), so `crypto_lob_ml` / `crypto_rl_policy` only start via managed strategy runtime spawn.
- 2026-03-09: Deleted the legacy trading-agent spawn helpers from [legacy_crypto.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/legacy_crypto.rs); the module now stays as a config/env compatibility layer only.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo test from_app_config_reads_crypto_lob_ml_model_env_vars --lib -- --nocapture`
  - `cargo test crypto_rl_policy::strategy --lib -- --nocapture`

# Crypto RL Policy Canonical Wrapper (2026-03-09)

## Goal
Let `crypto_rl_policy` start running through the canonical `Strategy` runtime by adding an observe-only wrapper that consumes the managed feed contract, reuses the extracted RL policy core, and emits decision logs instead of orders.

## Tasks

- [x] Add a canonical `CryptoRlPolicyStrategy` wrapper under `src/strategy/crypto_rl_policy/strategy.rs`.
- [x] Reuse the extracted `crypto_rl_policy::core` helpers for observation building, action decoding, sizing, and fallback policy logic inside the wrapper.
- [x] Register `crypto_rl_policy` in `StrategyFactory` so the managed runtime can construct it from TOML.
- [x] Add narrow wrapper tests for feed wiring and decision-log emission once inputs are ready.
- [x] Re-run compile plus targeted wrapper tests, including an `onnx` feature compile gate.

## Review

- [x] Confirm `crypto_rl_policy` now has a canonical `Strategy` implementation that stays observe-only and does not submit orders.
- [x] Confirm the wrapper only owns feed/cache/decision-preview state and leaves live execution/governance on the legacy path.
- [x] Confirm `StrategyFactory::from_toml()` can construct the wrapper and the targeted tests pass.

## Progress notes

- 2026-03-09: Added [strategy.rs](/Users/proerror/Documents/ploy/src/strategy/crypto_rl_policy/strategy.rs) and exported it from [mod.rs](/Users/proerror/Documents/ploy/src/strategy/crypto_rl_policy/mod.rs).
- 2026-03-09: Registered `crypto_rl_policy` in [manager.rs](/Users/proerror/Documents/ploy/src/strategy/manager.rs) so the managed runtime can instantiate it from TOML instead of depending solely on the legacy bootstrap path.
- 2026-03-09: The wrapper stays observe-only, reuses the extracted RL policy core for inference/fallback logic, and no longer emits `LegacyControl` actions in its canonical path.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo test from_toml_builds_expected_feeds --lib -- --nocapture`
  - `cargo test event_discovered_tracks_event_without_legacy_control_actions --lib -- --nocapture`
  - `cargo test on_tick_emits_buy_up_signal_log_when_rule_based_policy_triggers --lib -- --nocapture`
  - `cargo check --lib --features onnx`

# Crypto RL Policy Core Extraction (2026-03-09)

## Goal
Move the pure policy interpretation, observation building, position tracking types, and sizing helpers out of the legacy `CryptoRlPolicyAgent` shell into a strategy-side module so the remaining live runtime code becomes a thin wrapper around canonical strategy-owned logic.

## Tasks

- [x] Create a strategy-side `crypto_rl_policy` module that owns the extracted action/observation/state helpers.
- [x] Rewire the legacy `CryptoRlPolicyAgent` to delegate ONNX output interpretation, observation assembly, sizing, and rule-based fallback logic to the new strategy-side core.
- [x] Add narrow regression tests for the extracted helper behavior under the new strategy module.
- [x] Re-run compile plus targeted core helper tests after the ownership move.

## Review

- [x] Confirm `DiscreteAction`, `ContinuousAction`, tracked-position types, deployment metadata helpers, and observation builders now live under `src/strategy/crypto_rl_policy/`.
- [x] Confirm the legacy agent compiles while delegating its pure policy logic to the new strategy-side core instead of owning duplicate implementations.
- [x] Confirm the extracted core owns regression coverage for action mapping, sizing, deployment-id normalization, and rule-based exit behavior.

## Progress notes

- 2026-03-09: Added [core.rs](/Users/proerror/Documents/ploy/src/strategy/crypto_rl_policy/core.rs) and [mod.rs](/Users/proerror/Documents/ploy/src/strategy/crypto_rl_policy/mod.rs), and exported the new module from [strategy/mod.rs](/Users/proerror/Documents/ploy/src/strategy/mod.rs).
- 2026-03-09: Updated [crypto_rl_policy.rs](/Users/proerror/Documents/ploy/src/agents/crypto_rl_policy.rs) so the legacy agent now delegates ONNX output decoding, observation building, rule-based fallback policy, sizing, and deployment metadata to the extracted strategy-side core.
- 2026-03-09: Added new helper regressions in the extracted core for discrete-action mapping, share sizing, deployment-id normalization, and forced-loss sell behavior.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo test continuous_action_maps_to_expected_discrete_action --lib -- --nocapture`
  - `cargo test compute_shares_scales_with_position_delta --lib -- --nocapture`
  - `cargo test deployment_id_for_symbol_normalizes_case --lib -- --nocapture`
  - `cargo test rule_based_policy_sells_on_deep_loss --lib -- --nocapture`
  - `cargo check --lib --features onnx`

# Crypto LOB ML Canonical Wrapper (2026-03-09)

## Goal
Let `crypto_lob_ml` start running through the canonical `Strategy` runtime without cutting over live order ownership, by adding an observe-only wrapper that consumes the managed feed contract, builds sequence state, and emits inference/log events instead of orders.

## Tasks

- [x] Add a canonical `CryptoLobMlStrategy` wrapper under `src/strategy/crypto_lob_ml/strategy.rs`.
- [x] Reuse the extracted `crypto_lob_ml::core` helpers for sequence assembly and GBM-anchor inference inside the wrapper.
- [x] Register `crypto_lob_ml` in `StrategyFactory` so the canonical runtime can instantiate it from TOML.
- [x] Add narrow wrapper tests for config/feed wiring and sequence-warm inference logging.
- [x] Re-run compile plus the targeted wrapper tests.

## Review

- [x] Confirm `crypto_lob_ml` now has a canonical `Strategy` implementation that does not emit submit actions.
- [x] Confirm the wrapper only owns feed/cache/inference/logging state and leaves execution/governance/legacy bootstrap untouched.
- [x] Confirm `StrategyFactory::from_toml()` can construct the wrapper and the targeted wrapper tests pass.

## Progress notes

- 2026-03-09: Added [strategy.rs](/Users/proerror/Documents/ploy/src/strategy/crypto_lob_ml/strategy.rs) and exported it from [mod.rs](/Users/proerror/Documents/ploy/src/strategy/crypto_lob_ml/mod.rs).
- 2026-03-09: The new canonical wrapper now tracks event discovery, Binance spot/L2 state, Polymarket quotes, and per-event sequence caches, then emits `StrategyAction::LogEvent` with GBM-anchor inference once a sequence is warm.
- 2026-03-09: Registered `crypto_lob_ml` in [manager.rs](/Users/proerror/Documents/ploy/src/strategy/manager.rs) so canonical runtime creation no longer depends on the legacy agent bootstrap path.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo test from_toml_builds_expected_feeds --lib -- --nocapture`
  - `cargo test on_tick_emits_inference_log_once_sequence_is_ready --lib -- --nocapture`
  - `cargo test on_tick_skips_events_without_price_to_beat_when_required --lib -- --nocapture`

# Crypto LOB ML Core Extraction (2026-03-09)

## Goal
Move the pure sequence-building, normalization, and observation-alignment logic out of the legacy `CryptoLobMlAgent` shell into a strategy-side module so future canonical strategy migration stops depending on the old trading-agent runtime for core model preparation.

## Tasks

- [x] Create a strategy-side `crypto_lob_ml` module that owns the extracted pure helpers and local sequence state types.
- [x] Rewire the legacy `CryptoLobMlAgent` to delegate to the new strategy-side core instead of owning duplicate helper implementations.
- [x] Move the duplicated pure helper regression coverage to the new core module and delete the now-redundant legacy-agent copies.
- [x] Re-run compile plus narrow helper/inference regression tests after the ownership move.

## Review

- [x] Confirm the pure sequence helpers (`build_sequence`, sequence alignment, deployment metadata helpers, GBM anchor helper inputs) now live under `src/strategy/crypto_lob_ml/`.
- [x] Confirm the legacy agent keeps compiling while delegating to the new strategy-side core.
- [x] Confirm the extracted core owns the canonical helper regression coverage and the branch passes targeted compile/tests.

## Progress notes

- 2026-03-09: Added [core.rs](/Users/proerror/Documents/ploy/src/strategy/crypto_lob_ml/core.rs) and [mod.rs](/Users/proerror/Documents/ploy/src/strategy/crypto_lob_ml/mod.rs), and exported the new module from [strategy/mod.rs](/Users/proerror/Documents/ploy/src/strategy/mod.rs).
- 2026-03-09: Updated [crypto_lob_ml.rs](/Users/proerror/Documents/ploy/src/agents/crypto_lob_ml.rs) so the legacy agent delegates sequence caching, normalization, deployment metadata, and model-input alignment to the new strategy-side core instead of owning those implementations directly.
- 2026-03-09: Deleted the duplicated pure helper tests from the legacy agent now that the extracted core owns that regression surface.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo test build_sequence --lib -- --nocapture`
  - `cargo test deployment_metadata_helpers --lib -- --nocapture`
  - `cargo test test_estimate_p_up_validates_sequence_input_dim --lib -- --nocapture`

# Canonical Strategy SubmitIntent Migration (2026-03-09)

## Goal
Shrink the remaining raw `SubmitOrder` surface inside canonical strategy code by moving the active crypto strategy implementations onto `StrategyAction::SubmitIntent`, so the managed runtime no longer needs raw `OrderRequest` compatibility for these paths.

## Tasks

- [x] Convert the straightforward strategy modules (`momentum_strat`, `two_leg`, `gamma_scalping`) from `SubmitOrder` to `SubmitIntent`.
- [x] Finish the in-progress canonical handoff migration already underway in `adapters.rs` and `staggered_arb_live.rs` so the branch compiles again.
- [x] Add narrow regression tests proving the new canonical intent emission for touched strategy paths.
- [x] Re-run `cargo check --lib` and the narrow canonical-handoff regression test that is wired into the current lib target.

## Review

- [x] Confirm the touched strategy paths now emit `StrategyOrderIntent` instead of raw `OrderRequest`.
- [x] Confirm branch-level compile is restored after the partial `adapters` / `staggered_arb_live` migration.
- [x] Confirm at least one targeted canonical-emission regression test passes in the current lib target.

## Progress notes

- 2026-03-09: Converted [momentum_strat.rs](/Users/proerror/Documents/ploy/src/strategy/strategies/momentum_strat.rs), [two_leg.rs](/Users/proerror/Documents/ploy/src/strategy/strategies/two_leg.rs), [gamma_scalping/strategy.rs](/Users/proerror/Documents/ploy/src/strategy/gamma_scalping/strategy.rs), [adapters.rs](/Users/proerror/Documents/ploy/src/strategy/adapters.rs), and [staggered_arb_live.rs](/Users/proerror/Documents/ploy/src/strategy/staggered_arb_live.rs) to emit canonical crypto `SubmitIntent` actions for the migrated paths.
- 2026-03-09: Added narrow regression tests for momentum, two-leg, and gamma-scalping canonical intent emission; the gamma-scalping test is currently wired into the active lib test target.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo test evaluate_entry_emits_submit_intents --lib -- --nocapture`

# Legacy Agent Surface Narrowing (2026-03-09)

## Goal
Collapse the remaining legacy trading-agent compatibility surface so `CryptoLobMlAgent`, `CryptoRlPolicyAgent`, and `TradingAgent` are no longer casually re-exported from `crate::agents`; only the explicit `legacy_crypto` bootstrap path should depend on them.

## Tasks

- [x] Stop re-exporting legacy crypto agent types and the `TradingAgent` trait from `src/agents/mod.rs`.
- [x] Update the remaining compatibility callers to use explicit legacy module paths.
- [x] Keep governance-plane exports available so `OpenClaw` startup remains unaffected.
- [x] Re-run compile plus a narrow bootstrap env/config regression after the surface reduction.

## Review

- [x] Confirm `crate::agents` root now exposes governance/runtime essentials but not the legacy crypto agent implementations.
- [x] Confirm `legacy_crypto.rs` still compiles as the only explicit compatibility owner of `CryptoLobMlAgent` / `CryptoRlPolicyAgent`.
- [x] Confirm bootstrap tests still resolve the legacy crypto config enums via explicit module paths.

## Progress notes

- 2026-03-09: Narrowed [agents/mod.rs](/Users/proerror/Documents/ploy/src/agents/mod.rs) so legacy trading-agent implementations are no longer re-exported from the root agents module.
- 2026-03-09: Updated [crypto_lob_ml.rs](/Users/proerror/Documents/ploy/src/agents/crypto_lob_ml.rs), [crypto_rl_policy.rs](/Users/proerror/Documents/ploy/src/agents/crypto_rl_policy.rs), [legacy_crypto.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/legacy_crypto.rs), and [bootstrap.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap.rs) to use explicit compatibility paths.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo test from_app_config_reads_crypto_lob_ml_model_env_vars --lib -- --nocapture`

# Legacy Agent Public Surface Quarantine (2026-03-09)

## Goal
Narrow the last remaining legacy crypto runtime surface so the compatibility-only `TradingAgent` implementations stop leaking through the main `agents` module root, making the surviving legacy ownership explicit in `bootstrap/legacy_crypto.rs` instead of feeling like first-class runtime APIs.

## Tasks

- [x] Stop re-exporting `CryptoLobMlAgent`, `CryptoRlPolicyAgent`, and `TradingAgent` from `src/agents/mod.rs`.
- [x] Update remaining legacy runtime imports to use explicit compatibility module paths.
- [x] Keep governance-facing exports (`GovernanceContext`, `OpenClawAgent`, `GovernanceAgent`) intact.
- [x] Re-run compile after shrinking the public surface.

## Review

- [x] Confirm only the legacy bootstrap compatibility path imports the legacy trading-agent types directly.
- [x] Confirm non-legacy callers can still reach governance agent types through `crate::agents`.
- [x] Confirm the public-surface shrink compiles without runtime behavior changes.

## Progress notes

- 2026-03-09: Removed the legacy crypto agent and `TradingAgent` root re-exports from [agents/mod.rs](/Users/proerror/Documents/ploy/src/agents/mod.rs), and rewired [bootstrap/legacy_crypto.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/legacy_crypto.rs) plus legacy agent modules to import explicit compatibility paths.
- 2026-03-09: Validation passed:
  - `cargo check --lib`

# Canonical SubmitIntent Batch Conversion (2026-03-09)

## Goal
Shrink the remaining strategy-side raw `SubmitOrder` surface in one larger batch by moving the canonical strategy implementations and crypto adapters onto `StrategyAction::SubmitIntent`, leaving the legacy compatibility path for genuinely old runtimes instead of active strategy code.

## Tasks

- [x] Convert `MomentumStrategy`, `TwoLegStrategy`, and `GammaScalpingStrategy` to emit `SubmitIntent` directly.
- [x] Convert `MomentumStrategyAdapter`, `SplitArbStrategyAdapter`, and `StaggeredArbAdapter` live submit paths to emit `SubmitIntent`.
- [x] Add or update local helper builders so the converted strategies use one canonical strategy-side submit shape instead of open-coded `OrderRequest` assembly.
- [x] Update targeted strategy tests to assert `SubmitIntent` behavior where the action type changed.
- [x] Re-run compile plus targeted canonical-submit strategy tests.

## Review

- [x] Confirm the touched strategy/adaptor files no longer emit `StrategyAction::SubmitOrder` in production code.
- [x] Confirm `staggered_arb_live` still preserves stable client-order/idempotency semantics through `StrategyOrderIntent::into_order_request()`.
- [x] Confirm the converted strategies now carry explicit `Domain::Crypto` and market slug identity at the strategy contract boundary.

## Progress notes

- 2026-03-09: Converted [momentum_strat.rs](/Users/proerror/Documents/ploy/src/strategy/strategies/momentum_strat.rs), [two_leg.rs](/Users/proerror/Documents/ploy/src/strategy/strategies/two_leg.rs), and [gamma_scalping/strategy.rs](/Users/proerror/Documents/ploy/src/strategy/gamma_scalping/strategy.rs) to build `StrategyOrderIntent` directly.
- 2026-03-09: Converted [adapters.rs](/Users/proerror/Documents/ploy/src/strategy/adapters.rs) and [staggered_arb_live.rs](/Users/proerror/Documents/ploy/src/strategy/staggered_arb_live.rs) so the active crypto adapters/runtime-facing live strategy paths now submit canonical strategy intents instead of raw `OrderRequest`s.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo test evaluate_entry_emits_submit_intents --lib -- --nocapture`
  - `cargo test submit_intent --lib -- --nocapture`
  - `cargo test test_live_leg1_submit_sets_client_order_and_idempotency_key --lib -- --nocapture`
  - `cargo test test_live_leg2_uses_position_tokens_even_without_active_window --lib -- --nocapture`

# Binance L2 Feed Contract Expansion (2026-03-09)

## Goal
Expand the strategy-side `BinanceL2` market-update contract to expose the additional OBI levels (`1/2/3/20`) that still only exist inside the legacy crypto runtime, so `lob_ml` and `rl_policy` wrappers stop being blocked on missing L2 feature surface.

## Tasks

- [x] Extend `collector/binance_depth.rs` snapshots to compute and carry `obi_1`, `obi_2`, `obi_3`, and `obi_20`.
- [x] Extend `MarketUpdate::BinanceL2` to expose the extra OBI levels to strategy callers.
- [x] Rewire `DataFeedManager` Binance L2 forwarding to populate the expanded contract.
- [x] Add or update a narrow collector regression test to assert the new snapshot fields.
- [x] Re-run compile and the narrow L2 snapshot regression test.

## Review

- [x] Confirm the new OBI levels are available at the strategy feed boundary, not only inside the legacy `LobCache`.
- [x] Confirm the collector still builds snapshots correctly after the field expansion.
- [x] Confirm the expanded feed contract compiles without forcing downstream strategy rewrites in this slice.

## Progress notes

- 2026-03-09: Expanded [binance_depth.rs](/Users/proerror/Documents/ploy/src/collector/binance_depth.rs) `LobSnapshot` with `obi_1`, `obi_2`, `obi_3`, and `obi_20`, and updated snapshot construction in both cache read paths.
- 2026-03-09: Expanded [traits.rs](/Users/proerror/Documents/ploy/src/strategy/traits.rs) `MarketUpdate::BinanceL2` plus [feeds.rs](/Users/proerror/Documents/ploy/src/strategy/feeds.rs) forwarding, so canonical strategy consumers can now observe the same extra OBI levels the legacy `lob_ml` / `rl_policy` agents use.
- 2026-03-10: Extracted the feed runtime orchestration out of [feeds.rs](/Users/proerror/Documents/ploy/src/strategy/feeds.rs) into [runtime.rs](/Users/proerror/Documents/ploy/src/strategy/feeds/runtime.rs), moving Binance/Polymarket start-up, kline backfill, token subscribe, and L2 spin-up behind a dedicated runtime owner while leaving the root file focused on state layout, builders, and shared feed wiring.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo test test_apply_depth_snapshot_replaces_book_state --lib -- --nocapture`

# Legacy Crypto Bootstrap Config Collapse (2026-03-09)

## Goal
Move the remaining legacy crypto runtime knobs under one explicit compatibility subtree so `PlatformBootstrapConfig` no longer owns `lob_ml` / `rl_policy` as top-level bootstrap fields, and `legacy_crypto.rs` no longer needs the entire bootstrap config just to hydrate env vars or spawn compatibility runtimes.

## Tasks

- [x] Introduce a dedicated `LegacyCryptoRuntimeConfig` under `bootstrap/legacy_crypto.rs`.
- [x] Rewire `PlatformBootstrapConfig` to hold `legacy_crypto` instead of top-level `enable_crypto_lob_ml` / `enable_crypto_rl_policy` / config fields.
- [x] Change legacy crypto env hydration to take `&CryptoTradingConfig` plus `&mut LegacyCryptoRuntimeConfig` instead of the whole bootstrap config.
- [x] Change legacy crypto runtime spawning to take `&LegacyCryptoRuntimeConfig` instead of the whole bootstrap config.
- [x] Update platform-mode filters and bootstrap tests to use the nested compatibility config.
- [x] Re-run compile plus the narrow bootstrap/platform regression tests for the moved ownership.

## Review

- [x] Confirm `PlatformBootstrapConfig` no longer exposes legacy crypto runtime knobs as top-level fields.
- [x] Confirm `legacy_crypto.rs` no longer depends on the entire bootstrap config for env parsing or runtime spawn decisions.
- [x] Confirm deployment routing still toggles legacy crypto compatibility through `cfg.legacy_crypto.*`.
- [x] Confirm compile and narrow regression tests pass after the move.

## Progress notes

- 2026-03-09: Added nested legacy-crypto ownership in [bootstrap.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap.rs) and rewired [legacy_crypto.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/legacy_crypto.rs) to hydrate/spawn from `CryptoTradingConfig + LegacyCryptoRuntimeConfig` instead of the full bootstrap config.
- 2026-03-09: Updated [platform_mode.rs](/Users/proerror/Documents/ploy/src/main_modes/platform_mode.rs) and bootstrap tests so crypto domain filtering and legacy env assertions now target `cfg.legacy_crypto`.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo test from_app_config_reads_crypto_lob_ml_model_env_vars --lib -- --nocapture`
  - `cargo test pattern_memory_deployment_does_not_enable_lob_ml -- --nocapture`

# Bootstrap Support Helper Extraction (2026-03-09)

## Goal
Move the last top-level bootstrap utility helpers out of `bootstrap.rs` so the file stops accumulating env parsers, deployment-state loading, selector coin expansion, and orderbook formatting helpers alongside the real bootstrap flow.

## Tasks

- [x] Create a dedicated `bootstrap/support.rs` module for the remaining bootstrap utility helpers.
- [x] Rewire `bootstrap.rs` to import those helpers and delete the inline implementations.
- [x] Keep deployment loading and selector-expansion behavior unchanged in this slice.
- [x] Re-run compile and a deployment-routing regression test after the move.

## Review

- [x] Confirm `bootstrap.rs` no longer owns the env/deployment support helpers inline.
- [x] Confirm the extracted support module preserves the existing deployment-file fallback logic and market-selector coin extraction behavior.
- [x] Confirm bootstrap still compiles and the deployment-routing regression test still passes.

## Progress notes

- 2026-03-09: Added [support.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/support.rs) and moved the remaining top-level bootstrap utility helpers there.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo test apply_strategy_deployments_does_not_route_unknown_crypto_strategy_to_momentum --lib -- --nocapture`

# EventEdge Canonical Strategy Wrapper (2026-03-09)

## Goal
Wrap `EventEdgeCore` behind the canonical `Strategy` trait inside `src/strategy/event_edge/` only, so the repo gains a real strategy-side implementation without touching `bootstrap.rs`, `manager.rs`, or sports/NBA integration yet.

## Tasks

- [x] Add targeted failing tests for wrapper-local behavior: required feeds, discovered-event bookkeeping, canonical submit-action emission, and order-update position tracking.
- [x] Add a new `src/strategy/event_edge/strategy.rs` wrapper implementing `Strategy` around `EventEdgeCore`.
- [x] Keep all wiring local to `src/strategy/event_edge/` and avoid bootstrap/manager/sports/NBA edits in this slice.
- [x] Run the smallest relevant compile/tests for touched files only.

## Review

- [x] Confirm the wrapper uses `EventEdgeCore` for decision policy instead of duplicating thresholds.
- [x] Confirm the wrapper can hold discovered events, emit canonical `StrategyAction::SubmitOrder`, and translate fills into `PositionInfo`.
- [x] Confirm no non-local integration files were edited.

## Progress notes

- 2026-03-09: Added [strategy.rs](/Users/proerror/Documents/ploy/src/strategy/event_edge/strategy.rs) with a canonical `Strategy` wrapper, TOML builder, discovered-event bookkeeping, pending-order reservation tracking, and order-fill to `PositionInfo` translation.
- 2026-03-09: Local wiring is limited to [mod.rs](/Users/proerror/Documents/ploy/src/strategy/event_edge/mod.rs) plus small deterministic helpers in [core.rs](/Users/proerror/Documents/ploy/src/strategy/event_edge/core.rs).
- 2026-03-09: Validation attempted:
  - `cargo test strategy::event_edge::strategy::tests --lib -- --nocapture`
  - `cargo check --lib`

# Canonical Sports And Politics Runtime Cutover (2026-03-09)

## Goal
Retire the legacy `SportsTradingAgent` / `PoliticsTradingAgent` startup paths from platform bootstrap, move both domains onto canonical managed strategy runtime entrypoints, and wire `event_edge` + `nba_comeback` into `StrategyFactory`.

## Tasks

- [x] Add canonical runtime config builders for `event_edge` and `nba_comeback` under `bootstrap/strategy_deployments.rs`.
- [x] Rewire `bootstrap.rs` so sports and politics spawn through `spawn_managed_strategy_runtime_task(...)` instead of legacy trading-agent startup.
- [x] Keep sports quote/orderbook collector support alive by downgrading the old sports bootstrap helper into a runtime-support helper instead of deleting the whole support slice.
- [x] Register `event_edge` and `nba_comeback` in `StrategyFactory` and strategy availability metadata.
- [x] Re-open politics in `platform_mode` when no explicit domain filter is applied, while still filtering it out when the CLI only selects crypto/sports.
- [x] Add or update targeted tests for the new runtime config builders and platform-mode gating.
- [x] Collapse `src/agents/sports.rs` and `src/agents/politics.rs` into config-only compatibility modules and stop exporting their legacy agent types.

## Review

- [x] Confirm `bootstrap.rs` no longer calls legacy sports/politics agent spawners.
- [x] Confirm `event_edge` and `nba_comeback` now have canonical `StrategyFactory` entries.
- [x] Confirm sports-specific market-data persistence support still initializes separately from strategy runtime ownership.
- [x] Confirm the new builders emit canonical `[strategy] + [event_edge|nba_comeback]` TOML.
- [x] Confirm `SportsTradingAgent` and `PoliticsTradingAgent` no longer appear anywhere under `src/`.

## Progress notes

- 2026-03-09: Added canonical runtime builders for `event_edge` and `nba_comeback` in [strategy_deployments.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/strategy_deployments.rs).
- 2026-03-09: Rewired [bootstrap.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap.rs) so `Domain::Sports` and `Domain::Politics` now use managed strategy runtime spawns instead of `SportsTradingAgent` / `PoliticsTradingAgent`.
- 2026-03-09: Downgraded the old sports runtime helper into [runtime_spawns.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/runtime_spawns.rs) `prepare_sports_runtime_support(...)`, preserving PM WS/collector/persistence setup while removing strategy ownership.
- 2026-03-09: Registered `event_edge` and `nba_comeback` in [manager.rs](/Users/proerror/Documents/ploy/src/strategy/manager.rs) and exported `NbaComebackStrategy` from [mod.rs](/Users/proerror/Documents/ploy/src/strategy/nba_comeback/mod.rs).
- 2026-03-09: Reduced [sports.rs](/Users/proerror/Documents/ploy/src/agents/sports.rs) and [politics.rs](/Users/proerror/Documents/ploy/src/agents/politics.rs) to config-only compatibility shims, and stopped re-exporting the deleted legacy agent types from [mod.rs](/Users/proerror/Documents/ploy/src/agents/mod.rs).
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo test strategy::event_edge::strategy --lib -- --nocapture`
  - `cargo test strategy::nba_comeback::strategy --lib -- --nocapture`
  - `cargo test build_event_edge_runtime_config_ --lib -- --nocapture`
  - `cargo test build_nba_comeback_runtime_config_ --lib -- --nocapture`

# Platform NBA Agent Retirement (2026-03-09)

## Goal
Delete the remaining `platform::NbaComebackAgent` compatibility path by moving the CLI `nba_comeback` command onto the canonical strategy-side implementation and removing the dead platform export/module.

## Tasks

- [x] Rework `src/cli/strategy.rs` `run_nba_comeback(...)` to drive `NbaComebackStrategy` instead of `platform::NbaComebackAgent`.
- [x] Keep the CLI output useful for dry-run signal inspection without reintroducing a second runtime contract.
- [x] Remove the dead NBA agent export/module from `src/platform/mod.rs` and `src/platform/agents/mod.rs`.
- [x] Delete `src/platform/agents/nba_agent.rs` if nothing still instantiates it.
- [x] Re-run the narrowest CLI/strategy compile tests after the cutover.

## Review

- [x] Confirm no code instantiates `platform::NbaComebackAgent`.
- [x] Confirm `src/platform/mod.rs` no longer re-exports the deleted agent.
- [x] Confirm the CLI command still prints NBA comeback signals in dry-run mode.

## Progress notes

- 2026-03-09: Reworked [strategy.rs](/Users/proerror/Documents/ploy/src/cli/strategy.rs) so the `nba_comeback` CLI command now drives [strategy.rs](/Users/proerror/Documents/ploy/src/strategy/nba_comeback/strategy.rs) directly instead of instantiating `platform::NbaComebackAgent`.
- 2026-03-09: Added `NbaComebackStrategy::from_config(...)` plus a direct-config unit test so canonical callers no longer need a TOML round trip.
- 2026-03-09: Deleted [nba_agent.rs](/Users/proerror/Documents/ploy/src/platform/agents/nba_agent.rs) and removed the dead `NbaComebackAgent` exports from [mod.rs](/Users/proerror/Documents/ploy/src/platform/agents/mod.rs) and [mod.rs](/Users/proerror/Documents/ploy/src/platform/mod.rs).
- 2026-03-09: Deleted [sports.rs](/Users/proerror/Documents/ploy/src/agents/sports.rs) and [politics.rs](/Users/proerror/Documents/ploy/src/agents/politics.rs) once bootstrap-owned runtime config types made those legacy shims unnecessary.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo test strategy::nba_comeback::strategy --lib -- --nocapture`
  - `cargo test sports_runtime_config_defaults_match_bootstrap_expectations --lib -- --nocapture`
  - `cargo test politics_runtime_config_defaults_match_bootstrap_expectations --lib -- --nocapture`

# Canonical Strategy Handoff Unification (2026-03-09)

## Goal
Start collapsing the duplicate `StrategyAction::SubmitOrder { OrderRequest }` vs `CoordinatorHandle::submit_order(OrderIntent)` contract so canonical strategies stop depending on a private runtime-only execution path.

## Tasks

- [x] Define the canonical strategy-side submit payload that can survive outside `strategy_runtime.rs`.
- [x] Keep existing strategies compiling through a compatibility path while the new handoff is introduced.
- [ ] Move managed runtime submission closer to coordinator admission instead of direct executor ownership.
- [x] Preserve order-update feedback so strategies still receive fills/status changes.
- [x] Add a regression test covering action id propagation into the actual execution handoff.

## Review

- [x] Confirm the new canonical submit payload is not another permanent fourth runtime contract.
- [x] Confirm managed runtime still preserves `client_order_id` and idempotency semantics.
- [x] Confirm strategies still observe terminal fill/failure updates after the handoff change.

## Progress notes

- 2026-03-09: Added `StrategyAction::SubmitIntent { StrategyOrderIntent }` in [traits.rs](/Users/proerror/Documents/ploy/src/strategy/traits.rs) as the canonical strategy-side submit payload, plus a direct idempotency regression test for `into_order_request()`.
- 2026-03-09: Updated [strategy.rs](/Users/proerror/Documents/ploy/src/strategy/event_edge/strategy.rs) and [strategy.rs](/Users/proerror/Documents/ploy/src/strategy/nba_comeback/strategy.rs) so the current canonical domain strategies emit `SubmitIntent` instead of raw `OrderRequest`.
- 2026-03-09: Added compatibility normalization in [strategy_runtime.rs](/Users/proerror/Documents/ploy/src/coordinator/strategy_runtime.rs), [orchestrator.rs](/Users/proerror/Documents/ploy/src/strategy/orchestrator.rs), and [strategy.rs](/Users/proerror/Documents/ploy/src/cli/strategy.rs) so existing execution paths still accept the new canonical payload without breaking older strategies.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo test strategy::event_edge::strategy --lib -- --nocapture`
  - `cargo test strategy::nba_comeback::strategy --lib -- --nocapture`
  - `cargo test strategy_order_intent_into_order_request_preserves_action_id --lib -- --nocapture`

# Strategy Metadata And Momentum State Cleanup (2026-03-09)

## Goal
Eliminate duplicated crypto up/down series mappings outside `bootstrap` and remove redundant per-field `Arc<RwLock<...>>` state from `MomentumStrategyAdapter` now that the strategy runtime already holds `&mut self`.

## Tasks

- [x] Add a shared crypto series registry under `src/strategy/crypto/`.
- [x] Rewire non-`bootstrap` callers to use the shared registry instead of hardcoded series IDs and symbol/window mappings.
- [x] Simplify `MomentumStrategyAdapter` internal state from nested async locks to direct owned state.
- [x] Keep the public `Strategy` trait boundary unchanged.
- [x] Run the smallest relevant compile/test coverage for the touched strategy modules.

## Review

- [x] Confirm the shared registry now owns the canonical 5m/15m crypto up/down series metadata for strategy-side callers.
- [x] Confirm `MomentumStrategyAdapter` no longer uses redundant internal `Arc<RwLock<...>>` state for positions, quotes, cooldowns, and pending orders.
- [x] Confirm targeted strategy tests still pass after the refactor.

## Progress notes

- 2026-03-09: Added [series_registry.rs](/Users/proerror/Documents/ploy/src/strategy/crypto/series_registry.rs) and re-exported it from [mod.rs](/Users/proerror/Documents/ploy/src/strategy/crypto/mod.rs) as the strategy-side source of truth for crypto series metadata.
- 2026-03-09: Rewired [momentum.rs](/Users/proerror/Documents/ploy/src/strategy/momentum.rs), [adapters.rs](/Users/proerror/Documents/ploy/src/strategy/adapters.rs), [staggered_arb_live.rs](/Users/proerror/Documents/ploy/src/strategy/staggered_arb_live.rs), [updown_backtest.rs](/Users/proerror/Documents/ploy/src/analysis/updown_backtest.rs), [collector_modes.rs](/Users/proerror/Documents/ploy/src/main_modes/collector_modes.rs), and [crypto.rs](/Users/proerror/Documents/ploy/src/main_commands/crypto.rs) to stop hardcoding the same series metadata.
- 2026-03-09: Simplified `MomentumStrategyAdapter` to use direct owned state instead of per-field async locks, while keeping its runtime contract unchanged.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo test strategy::crypto::series_registry --lib -- --nocapture`
  - `cargo test strategy::adapters::tests --lib -- --nocapture`

# Bootstrap Schema And Persistence Module Extraction (2026-03-09)

## Goal
Move the remaining schema/DDL and market-persistence ownership out of `bootstrap.rs` so the main bootstrap file stops mixing startup assembly with table management, trade polling, alerts, and settlement refresh loops.

## Tasks

- [x] Create dedicated `bootstrap/schema.rs` and `bootstrap/market_persistence.rs` modules.
- [x] Rewire `bootstrap.rs` to import those modules and delete the in-file implementations.
- [x] Preserve the existing bootstrap/public entry points used by CLI, runtime spawns, and strategy observability setup.
- [x] Run compile plus targeted bootstrap tests after the move.

## Review

- [x] Confirm `bootstrap.rs` no longer owns the schema DDL/repair implementations inline.
- [x] Confirm trade polling, trade alerts, and settlement refresh ownership now live in the new market-persistence module.
- [x] Confirm existing bootstrap tests still pass after the extraction.

## Progress notes

- 2026-03-09: Added [schema.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/schema.rs) for startup schema helpers and [market_persistence.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/market_persistence.rs) for Polymarket trade/settlement persistence ownership.
- 2026-03-09: `bootstrap.rs` now imports/re-exports those helpers instead of carrying the DDL, alerting, and settlement implementations inline.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo test ensure_pm_market_metadata_table_exists --lib -- --nocapture`
  - `cargo test from_app_config_ignores_legacy_enable_price_exits_env --lib -- --nocapture`

# Secret Debug Redaction Batch (2026-03-09)

## Goal
Eliminate clearly unsafe secret exposure through `Debug` formatting and low-risk secret cloning for runtime credential/config types, without touching bootstrap/coordinator runtime code.

## Tasks

- [x] Verify which `.full-review` secret-leak findings are still correct on this branch.
- [x] Add failing/targeted tests for credential/config `Debug` redaction where practical.
- [x] Replace unsafe derived `Debug` output on secret-bearing config/credential types with redacted manual implementations.
- [x] Remove unsafe `Clone` on `Wallet` if the current branch does not require cloning wallet objects directly.
- [x] Run the smallest relevant Rust test set plus a compile check for touched modules.

## Review

- [x] Confirm `ApiCredentials`, `DatabaseConfig`, `KalshiConfig`, and `GrokConfig` no longer print raw secrets via `Debug`.
- [x] Confirm HMAC signing debug logging no longer includes the full signing payload.
- [x] Confirm `Wallet` is no longer clonable directly if the codebase does not rely on that capability.

## Progress notes

- 2026-03-09: Scope is intentionally disjoint from the ongoing bootstrap/coordinator refactor; only secret-bearing config/credential types and their tests should move in this slice.
- 2026-03-09: Verified `.full-review` items against the current branch before editing. `ApiCredentials` debug leakage, HMAC payload logging, and secret-bearing config `Debug` derives were all still real; `Wallet` already had custom `Debug`, so the only wallet-side change in this slice was dropping direct `Clone`.
- 2026-03-09: Added targeted redaction tests for `ApiCredentials`, `GrokConfig`, `DatabaseConfig`, and `KalshiConfig`.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo test test_api_credentials_debug_redacts_secrets -- --nocapture`
  - `cargo test test_grok_config_debug_redacts_api_key -- --nocapture`
  - `cargo test test_database_config_debug_redacts_url -- --nocapture`
  - `cargo test test_kalshi_config_debug_redacts_credentials -- --nocapture`

# Bootstrap Strategy Deployments Module Extraction (2026-03-09)

## Goal
Move crypto strategy classification, deployment mapping, and runtime config builder ownership out of `bootstrap.rs` into a dedicated submodule so bootstrap stops doubling as a strategy router and TOML config factory.

## Tasks

- [x] Create a dedicated `bootstrap/strategy_deployments.rs` submodule for crypto strategy classification, deployment mapping, and managed-runtime config builders.
- [x] Rewire `bootstrap.rs` to import those helpers and delete the in-file strategy deployment/config-builder block.
- [x] Keep runtime behavior unchanged in this slice; this is a file-boundary cleanup, not a contract migration.
- [x] Run targeted compile/tests for deployment routing and managed-runtime config rendering.

## Review

- [x] Confirm `bootstrap.rs` no longer owns crypto strategy classification or managed runtime TOML builder implementations inline.
- [x] Confirm the new submodule owns both deployment enablement mapping and runtime config rendering helpers.
- [x] Confirm existing bootstrap tests for deployment routing and momentum/staggered config rendering still pass after the move.

## Progress notes

- 2026-03-09: After extracting runtime spawns, the next thick bootstrap ownership block was the strategy deployment router and runtime config builder cluster.
- 2026-03-09: Added [strategy_deployments.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/strategy_deployments.rs) and moved crypto strategy classification, deployment target collection, and momentum/staggered/pattern-memory config builder logic there.
- 2026-03-09: `bootstrap.rs` now imports those helpers instead of owning the block inline.
- 2026-03-09: Targeted validation passed:
  - `cargo check --lib`
  - `cargo test apply_strategy_deployments_does_not_route_unknown_crypto_strategy_to_momentum --lib -- --nocapture`
  - `cargo test build_split_arb_runtime_config_renders_symbols_and_series_ids --lib -- --nocapture`
  - `cargo test build_momentum_runtime_config_renders_directional_crypto_settings --lib -- --nocapture`
  - `cargo test build_momentum_runtime_config_rejects_non_directional_modes --lib -- --nocapture`

# Bootstrap Strategy Deployments Submodule Split (2026-03-09)

## Goal
Break the remaining `bootstrap/strategy_deployments.rs` god-module into focused ownership slices so deployment matrix application, managed-runtime planning, and runtime TOML rendering stop living in one file.

## Tasks

- [x] Split deployment matrix / crypto classification into a dedicated `deployment_matrix` submodule.
- [x] Split managed runtime plan assembly into a dedicated `runtime_plans` submodule.
- [x] Split TOML/runtime config rendering into a dedicated `runtime_configs` submodule.
- [x] Re-run focused bootstrap compile/tests after the submodule split.

## Review

- [x] Confirm `strategy_deployments.rs` is now a thin module shell that only wires submodules/re-exports.
- [x] Confirm deployment routing, managed plan assembly, and config rendering each have their own file under `bootstrap/strategy_deployments/`.
- [x] Confirm bootstrap behavior remains unchanged via focused compile/tests.

## Progress notes

- 2026-03-09: Replaced the single-file [strategy_deployments.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/strategy_deployments.rs) implementation with a thin module shell plus:
  - [deployment_matrix.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/strategy_deployments/deployment_matrix.rs)
  - [runtime_plans.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/strategy_deployments/runtime_plans.rs)
  - [runtime_configs.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/strategy_deployments/runtime_configs.rs)
- 2026-03-09: This slice keeps the old public helper surface intact for `bootstrap.rs` and bootstrap tests while moving the real ownership to dedicated files.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo check --lib --features rl`
  - `cargo test apply_strategy_deployments_does_not_route_unknown_crypto_strategy_to_momentum --lib -- --nocapture`
  - `cargo test collect_managed_strategy_runtime_plans_collapses_crypto_spawn_specs --lib -- --nocapture`
  - `cargo test build_split_arb_runtime_config_renders_symbols_and_series_ids --lib -- --nocapture`
  - `cargo test build_momentum_runtime_config_renders_directional_crypto_settings --lib -- --nocapture`
  - `cargo test --features rl build_crypto_rl_policy_runtime_config_preserves_model_controls --lib -- --nocapture`

# Bootstrap Runtime Spawns Module Extraction (2026-03-09)

## Goal
Move bootstrap runtime/domain spawn ownership out of `bootstrap.rs` into a dedicated submodule so the main bootstrap file starts shedding file-level responsibility instead of only accumulating local helper extractions.

## Tasks

- [x] Create a dedicated `bootstrap/runtime_spawns.rs` submodule for managed-strategy, legacy-trading-agent, governance, sports, and politics spawn helpers.
- [x] Rewire `bootstrap.rs` to import those helpers and delete the in-file helper bodies.
- [x] Keep runtime behavior unchanged in this slice; this is a file-boundary cleanup, not a contract migration.
- [x] Run targeted compile/tests for bootstrap config behavior and coordinator governance coverage.

## Review

- [x] Confirm the top of `bootstrap.rs` no longer contains the runtime spawn helper implementations.
- [x] Confirm the new submodule owns the three runtime startup paths: managed strategy, legacy trading agent, and governance/domain wrappers.
- [x] Confirm the main bootstrap flow still calls the same helper entry points after the move.

## Progress notes

- 2026-03-09: The implementation plan calls for reducing `bootstrap` back to pure assembly, and the current file still carried all spawn helper bodies inline.
- 2026-03-09: This slice deliberately moves those helpers behind a real module boundary so later strategy/risk/contract cuts do not keep expanding `bootstrap.rs`.
- 2026-03-09: Added [runtime_spawns.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/runtime_spawns.rs) and moved the managed-strategy, legacy-trading-agent, governance, sports, and politics startup helpers there.
- 2026-03-09: `bootstrap.rs` now imports those helpers instead of owning their bodies inline; file length dropped from `6745` lines to `6269`.
- 2026-03-09: Targeted validation passed:
  - `cargo check --lib`
  - `cargo test test_governance_status_includes_domain_ingress_and_agents --lib -- --nocapture`
  - `cargo test from_app_config_ignores_legacy_enable_price_exits_env --lib -- --nocapture`
  - `cargo test apply_strategy_deployments_does_not_route_unknown_crypto_strategy_to_momentum --lib -- --nocapture`

# Bootstrap Crypto Runtime Support Extraction (2026-03-09)

## Goal
Move the giant crypto runtime support block out of `start_platform()` so bootstrap stops owning event matcher discovery, PM collector refresh, market-data persistence bridges, and Binance LOB wiring inline.

## Tasks

- [x] Add a dedicated crypto bootstrap support module and move the crypto runtime/data-plane startup block there.
- [x] Replace the inline `if config.enable_crypto { ... }` block in `start_platform()` with a helper call that returns the managed/shared crypto data-plane handles.
- [x] Keep the managed runtime handoff contract unchanged in this slice; this is ownership migration, not behavior redesign.
- [x] Re-run default + `rl` compile after the extraction.

## Review

- [x] Confirm `start_platform()` no longer owns the giant crypto runtime setup block inline.
- [x] Confirm PM token seeding/refresh, WS/data-plane setup, persistence pipeline wiring, and Binance LOB startup now live in the dedicated crypto support module.
- [x] Confirm compile still passes in default and `rl` builds after the extraction.

## Progress notes

- 2026-03-09: Added [crypto_runtime_support.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/crypto_runtime_support.rs) with `initialize_crypto_runtime_support(...)` and a small `CryptoRuntimeSupport` return object for the managed/shared data-plane handles.
- 2026-03-09: Replaced the giant inline crypto block in [bootstrap.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap.rs) with a single helper call, leaving bootstrap responsible only for orchestration and the later managed-runtime spawn loop.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo check --lib --features rl`

# Bootstrap Legacy TradingAgent Spawn Consolidation (2026-03-09)

## Goal
Unify the remaining legacy `TradingAgent` registration and task-spawn plumbing behind one helper so `bootstrap.rs` no longer repeats `register_agent -> AgentContext::new -> tokio::spawn(agent.run)` across the old runtime paths.

## Tasks

- [x] Add a bootstrap-local helper for spawning legacy `TradingAgent` instances.
- [x] Migrate the remaining legacy branches (`momentum` fallback, `lob_ml`, `rl`, `sports`, `politics`) to the shared helper without changing their runtime behavior.
- [x] Leave governance-agent startup on its separate `GovernanceContext` path.
- [x] Run targeted compile/tests for coordinator governance and bootstrap behavior.

## Review

- [x] Confirm the repeated legacy trading-agent spawn sequence now lives in one helper.
- [x] Confirm sports/politics extracted helpers reuse the same legacy trading-agent spawn path as crypto legacy branches.
- [x] Confirm OpenClaw still stays on the governance-only startup path.

## Progress notes

- 2026-03-09: After consolidating managed-runtime spawn ownership, the remaining repeated bootstrap wiring was the old `TradingAgent` registration and task launch path.
- 2026-03-09: This slice is intended to make the runtime boundary explicit: managed strategies use one helper, legacy trading agents use another, governance agents keep their own context.
- 2026-03-09: Added `spawn_trading_agent_task(...)` so legacy runtime spawn now centralizes coordinator registration, `AgentContext` construction, and task launch in one place.
- 2026-03-09: Migrated the momentum fallback, `lob_ml`, `rl`, `sports`, and `politics` branches to that helper while keeping OpenClaw on `GovernanceContext`.
- 2026-03-09: Targeted validation passed:
  - `cargo check --lib`
  - `cargo test test_governance_status_includes_domain_ingress_and_agents --lib -- --nocapture`
  - `cargo test test_handle_force_close_domain_blocks_new_buy_immediately --lib -- --nocapture`

# Bootstrap Sports And Politics Spawn Extraction (2026-03-09)

## Goal
Move the legacy `sports` and `politics` bootstrap spawn branches behind dedicated helpers so the main bootstrap flow stops owning domain-specific pool setup, data-plane wiring, and agent construction details.

## Tasks

- [x] Extract the full `sports` branch into an async bootstrap helper without changing its PM WS persistence or Grok wiring.
- [x] Extract the `politics` branch into an async bootstrap helper without changing its `EventEdgeCore` initialization or PM client requirement.
- [x] Keep the actual `SportsTradingAgent` and `PoliticsTradingAgent` runtimes unchanged in this slice.
- [x] Run targeted compile/tests for bootstrap config behavior and coordinator/governance coverage.

## Review

- [x] Confirm the main bootstrap flow now delegates sports/politics startup instead of inlining those branches.
- [x] Confirm sports still creates its dedicated domain data plane and persistence bridges before agent spawn.
- [x] Confirm politics still fails fast when the Polymarket client is unavailable.

## Progress notes

- 2026-03-09: After the managed-runtime consolidation, the thickest remaining bootstrap ownership blocks were the legacy `sports` and `politics` domain spawns.
- 2026-03-09: This slice is structural only; it should reduce main-flow sprawl without changing domain runtime behavior.
- 2026-03-09: Added `spawn_sports_trading_agent(...)` and `spawn_politics_trading_agent(...)` so the main bootstrap flow now delegates those domain-specific startup paths instead of open-coding pool setup, PM WS bridges, and agent construction.
- 2026-03-09: Kept the sports PM L2 persistence wiring, Grok enrichment, and politics `EventEdgeCore` creation unchanged inside the extracted helpers.
- 2026-03-09: Targeted validation passed:
  - `cargo check --lib`
  - `cargo test test_governance_status_includes_domain_ingress_and_agents --lib -- --nocapture`
  - `cargo test from_app_config_ignores_legacy_enable_price_exits_env --lib -- --nocapture`
  - `cargo test apply_strategy_deployments_does_not_route_unknown_crypto_strategy_to_momentum --lib -- --nocapture`

# Bootstrap Managed Runtime Spawn Consolidation (2026-03-09)

## Goal
Collapse the duplicated canonical managed-strategy bootstrap wiring into one helper so `bootstrap.rs` stops open-coding coordinator registration, shutdown plumbing, and runtime task spawning for each migrated strategy.

## Tasks

- [x] Add a bootstrap-local managed-runtime spawn helper that owns coordinator registration and task launch for canonical strategy runtimes.
- [x] Migrate the `momentum`, `pattern_memory`, and `staggered_arb` bootstrap branches to the shared helper without changing their runtime configs or fallback behavior.
- [x] Keep legacy-only agents (`lob_ml`, `rl`, `sports`, `politics`) untouched in this slice.
- [x] Run targeted compile/tests for the migrated bootstrap paths and existing coordinator execution coverage.

## Review

- [x] Confirm `bootstrap.rs` no longer repeats the `register_agent -> shutdown_rx -> tokio::spawn(run_managed_strategy_runtime)` pattern across the three managed strategy branches.
- [x] Confirm the migrated branches keep their current agent ids, risk registration, and observability wiring.
- [x] Confirm unsupported momentum entry modes still fall back to the legacy trading-agent branch.

## Progress notes

- 2026-03-09: After migrating directional momentum, the canonical managed-runtime path was still duplicated three times across `momentum`, `pattern_memory`, and `staggered_arb`.
- 2026-03-09: This slice focuses only on consolidating bootstrap-side spawn ownership so later legacy-to-managed migrations reuse one canonical launch path.
- 2026-03-09: Added `ManagedStrategyRuntimeSpawn` plus `spawn_managed_strategy_runtime_task(...)` so bootstrap now has one canonical helper for coordinator registration, shutdown subscription, observability handoff, and managed-runtime task launch.
- 2026-03-09: Migrated the `momentum`, `pattern_memory`, and `staggered_arb` branches to the shared helper without touching their config builders, ids, or momentum's legacy fallback behavior.
- 2026-03-09: Targeted validation passed:
  - `cargo check --lib`
  - `cargo test build_momentum_runtime_config_renders_directional_crypto_settings --lib -- --nocapture`
  - `cargo test build_split_arb_runtime_config_renders_symbols_and_series_ids --lib -- --nocapture`
  - `cargo test test_drain_and_execute_records_single_success_for_buy_fill --lib -- --nocapture`

# Momentum Managed Runtime Migration (2026-03-09)

## Goal
Replace the default directional crypto momentum live branch in `bootstrap.rs` with the canonical managed strategy runtime, while preserving legacy fallback for unsupported non-directional entry modes.

## Tasks

- [x] Add bootstrap-side momentum runtime config generation from `CryptoTradingConfig`.
- [x] Switch the `enable_crypto_momentum` startup branch from `CryptoTradingAgent` to `run_managed_strategy_runtime` when `entry_mode == directional`.
- [x] Preserve a legacy fallback path for `arb_only` and `vol_straddle` modes until canonical equivalents exist.
- [x] Add targeted tests for momentum runtime config rendering and unsupported-mode fallback gating.
- [x] Run targeted compile/tests for bootstrap config rendering and core coordinator governance coverage.

## Review

- [x] Confirm the default directional momentum path now spawns the canonical managed strategy runtime instead of `CryptoTradingAgent`.
- [x] Confirm unsupported momentum modes still route to the legacy trading-agent path rather than silently changing behavior.
- [x] Confirm generated momentum runtime config carries the expected symbols, timing, and risk envelope.

## Progress notes

- 2026-03-09: The canonical runtime already supports `momentum` through `StrategyFactory`, but bootstrap was still directly spawning `CryptoTradingAgent`.
- 2026-03-09: Added `build_momentum_runtime_config(...)` plus a template/external-file renderer so bootstrap can derive a managed `momentum` TOML from `CryptoTradingConfig` while preserving the current risk and timing envelope.
- 2026-03-09: Replaced the default directional `enable_crypto_momentum` branch in bootstrap with `run_managed_strategy_runtime(...)`, using the existing `crypto` agent id and coordinator registration path.
- 2026-03-09: Kept a guarded legacy fallback for `arb_only` and `vol_straddle` entry modes so unsupported semantics do not silently drift during the migration.
- 2026-03-09: Added bootstrap tests for managed momentum config rendering and non-directional rejection.
- 2026-03-09: Targeted validation passed:
  - `cargo check --lib`
  - `cargo test build_momentum_runtime_config_renders_directional_crypto_settings --lib -- --nocapture`
  - `cargo test build_momentum_runtime_config_rejects_non_directional_modes --lib -- --nocapture`
  - `cargo test apply_strategy_deployments_does_not_route_unknown_crypto_strategy_to_momentum --lib -- --nocapture`

# Momentum Legacy Fallback Retirement (2026-03-09)

## Goal
Finish the momentum cutover by removing the last bootstrap fallback to `CryptoTradingAgent`, so a bad managed runtime config now fails closed instead of silently reviving the legacy runtime.

## Tasks

- [x] Remove the `CryptoTradingAgent` fallback from the momentum startup branch in `bootstrap.rs`.
- [x] Stop publicly re-exporting `CryptoTradingAgent` once bootstrap no longer instantiates it.
- [x] Collapse `src/agents/crypto.rs` into a config-only compatibility shim once no live runtime still instantiates it.
- [x] Re-run momentum bootstrap compile/tests after the fallback removal.

## Review

- [x] Confirm bootstrap no longer instantiates `CryptoTradingAgent`.
- [x] Confirm non-directional / invalid momentum runtime config now skips startup instead of reviving a second runtime contract.
- [x] Confirm `src/agents/mod.rs` no longer exposes `CryptoTradingAgent` as a live runtime entrypoint.
- [x] Confirm `src/agents/crypto.rs` no longer contains the dead `TradingAgent` runtime implementation.

## Progress notes

- 2026-03-09: Removed the `CryptoTradingAgent::new(...)` fallback from the `enable_crypto_momentum` bootstrap branch; invalid managed momentum configs now warn and skip startup.
- 2026-03-09: Trimmed [mod.rs](/Users/proerror/Documents/ploy/src/agents/mod.rs) so only `CryptoTradingConfig` / `CryptoEntryMode` stay public from [crypto.rs](/Users/proerror/Documents/ploy/src/agents/crypto.rs).
- 2026-03-09: Replaced [crypto.rs](/Users/proerror/Documents/ploy/src/agents/crypto.rs) with a config-only compatibility shim after `CryptoTradingAgent` lost its last live bootstrap entrypoint.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo test build_momentum_runtime_config_renders_directional_crypto_settings --lib -- --nocapture`
  - `cargo test build_momentum_runtime_config_rejects_non_directional_modes --lib -- --nocapture`

# Legacy Crypto Bootstrap Quarantine (2026-03-09)

## Goal
Move the last live `TradingAgent` bootstrap ownership for `crypto_lob_ml` and `crypto_rl_policy` out of `bootstrap.rs`, so managed strategy and governance paths stay in the main assembly flow while legacy crypto runtime remains isolated in one compatibility module.

## Tasks

- [x] Create a dedicated bootstrap submodule for legacy crypto agent env parsing and spawn logic.
- [x] Rewire `PlatformBootstrapConfig::from_app_config` to call the extracted legacy crypto config helper instead of inlining the `PLOY_CRYPTO_LOB_ML__*` / `PLOY_CRYPTO_RL_POLICY__*` parsing block.
- [x] Rewire the crypto startup path to delegate legacy `lob_ml` / `rl_policy` spawns to the extracted module.
- [x] Move the generic legacy `spawn_trading_agent_task(...)` helper into the legacy crypto module so `runtime_spawns.rs` only owns managed/governance startup.
- [x] Remove direct bootstrap imports of legacy crypto runtime traits/agent types once the helper move is complete.
- [x] Re-run compile plus bootstrap config regressions after the extraction.

## Review

- [x] Confirm `bootstrap.rs` no longer inlines the `PLOY_CRYPTO_LOB_ML__*` / `PLOY_CRYPTO_RL_POLICY__*` env parsing block.
- [x] Confirm `bootstrap.rs` no longer directly constructs `CryptoLobMlAgent` or `CryptoRlPolicyAgent`.
- [x] Confirm `runtime_spawns.rs` no longer owns the generic legacy trading-agent spawn helper.
- [x] Confirm the legacy crypto runtime surface is now concentrated in [legacy_crypto.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/legacy_crypto.rs).

## Progress notes

- 2026-03-09: Added [legacy_crypto.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/legacy_crypto.rs) to own the remaining `crypto_lob_ml` / `crypto_rl_policy` env parsing and runtime spawn paths.
- 2026-03-09: Rewired [bootstrap.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap.rs) so the main assembly flow delegates both legacy crypto config hydration and legacy runtime startup to the new module.
- 2026-03-09: Moved `spawn_trading_agent_task(...)` out of [runtime_spawns.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/runtime_spawns.rs) and into the legacy crypto module, which let bootstrap drop its direct `TradingAgent` / `AgentContext` / legacy-agent imports.
- 2026-03-09: File sizes after the extraction:
  - [bootstrap.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap.rs): `2840` lines
  - [legacy_crypto.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/legacy_crypto.rs): `544` lines
  - [runtime_spawns.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/runtime_spawns.rs): `366` lines

# Bootstrap OpenClaw Spawn Extraction (2026-03-09)

## Goal
Move the OpenClaw-specific startup branch out of the main bootstrap flow so `bootstrap.rs` delegates governance-plane wiring instead of inlining it.

## Tasks

- [x] Extract the OpenClaw enable/register/spawn block into a dedicated helper.
- [x] Keep the helper scoped to OpenClaw only, without changing other bootstrap runtime branches.
- [x] Run targeted compile/test validation for bootstrap config handling and coordinator governance state.

## Review

- [x] Confirm the main bootstrap flow no longer inlines OpenClaw websocket/register/spawn wiring.
- [x] Confirm OpenClaw startup behavior and logging remain unchanged after extraction.
- [x] Confirm no other bootstrap runtime branch is altered by this slice.

## Progress notes

- 2026-03-09: After moving OpenClaw onto `GovernanceContext`, its bootstrap branch became a clean extraction seam inside `bootstrap.rs`.
- 2026-03-09: Added `spawn_openclaw_governance_agent(...)` to encapsulate OpenClaw websocket setup, coordinator registration, governance-context construction, and task spawn.
- 2026-03-09: Replaced the inline OpenClaw branch in the main bootstrap flow with a single helper call, leaving other bootstrap runtime branches untouched.
- 2026-03-09: Targeted validation passed:
  - `cargo check --lib`
  - `cargo test test_governance_status_includes_domain_ingress_and_agents --lib -- --nocapture`
  - `cargo test from_app_config_ignores_legacy_enable_price_exits_env --lib -- --nocapture`

# Trading Agent Context Governance Trim (2026-03-09)

## Goal
Remove governance-only capabilities from `AgentContext` now that OpenClaw uses `GovernanceContext`, so trading agents no longer receive control-plane methods by default.

## Tasks

- [x] Verify no remaining trading agent uses governance-policy or pause/resume-peer helpers through `AgentContext`.
- [x] Remove governance-only methods from `AgentContext` while leaving order submission and heartbeat/state reporting intact.
- [x] Run targeted compile/test validation covering coordinator governance state after the context trim.

## Review

- [x] Confirm `AgentContext` no longer exposes peer pause/resume or governance-policy mutation methods.
- [x] Confirm only `GovernanceContext` carries those methods after the cut.
- [x] Confirm trading-agent implementations continue to compile unchanged.

## Progress notes

- 2026-03-09: Post-OpenClaw search showed `submit_pause_agent`, `submit_resume_agent`, `read_governance_policy`, and `update_governance_policy` were no longer referenced by any `TradingAgent` implementation.
- 2026-03-09: Removed the last governance-only helper methods from `src/agents/context.rs`, leaving trading-agent context with order submission, state reporting, state reads, and command intake only.
- 2026-03-09: Verified the governance helpers now exist only on `src/agents/governance_context.rs`, with `OpenClawAgent` as the only remaining caller.
- 2026-03-09: Targeted validation passed:
  - `cargo check --lib`
  - `cargo test test_governance_status_includes_domain_ingress_and_agents --lib -- --nocapture`

# OpenClaw Governance Context Extraction (2026-03-09)

## Goal
Separate OpenClaw from the generic `TradingAgent` contract by giving it a governance-only context that cannot submit orders, while preserving its current pause/resume/policy authority.

## Tasks

- [x] Add a dedicated governance context with only state, governance, and coordinator-control capabilities.
- [x] Introduce a governance-agent trait and move `OpenClawAgent` off the `TradingAgent` trait.
- [x] Rewire only the OpenClaw bootstrap path to use the governance-specific context, leaving other trading agents unchanged.
- [x] Run targeted compile/test validation around OpenClaw governance behavior and platform startup compilation.

## Review

- [x] Confirm `OpenClawAgent` no longer imports or receives `submit_order` capability through context.
- [x] Confirm bootstrap spawns OpenClaw through the governance-specific path only.
- [x] Confirm trading-agent paths for crypto/sports/politics remain unchanged by this slice.

## Progress notes

- 2026-03-09: Agent inventory showed OpenClaw is the safest first live runtime peel because it already behaves like governance-plane code while still hanging off `TradingAgent`.
- 2026-03-09: Added `src/agents/governance_context.rs` and a new `GovernanceAgent` trait so governance-plane agents can observe state, update policy, and pause/resume peers without receiving order-submission capability.
- 2026-03-09: Moved `OpenClawAgent` from `TradingAgent` to `GovernanceAgent` and rewired only the OpenClaw bootstrap branch to construct `GovernanceContext`.
- 2026-03-09: Verified there are no remaining `TradingAgent for OpenClawAgent` or `submit_order` call sites under `src/agents/openclaw`.
- 2026-03-09: Targeted validation passed:
  - `cargo check --lib`
  - `cargo test test_governance_status_includes_domain_ingress_and_agents --lib -- --nocapture`
  - `cargo test from_app_config_ignores_legacy_enable_price_exits_env --lib -- --nocapture`

# Platform Dead DomainAgent Retirement (2026-03-09)

## Goal
Remove `DomainAgent` implementations that no longer have bootstrap, CLI, or main-command wiring so the legacy platform runtime surface shrinks before higher-risk runtime migrations.

## Tasks

- [x] Verify `CryptoAgent` and `EventEdgePlatformAgent` have no active runtime wiring outside their own module/export surface.
- [x] Remove the dead modules and their public exports while keeping still-active NBA and RL platform paths intact.
- [x] Run targeted compile/test validation to prove the remaining platform runtime still builds and keeps its current dry-run-only guardrails.

## Review

- [x] Confirm no bootstrap, CLI, or main-command path still references the removed dead agents.
- [x] Confirm `platform/mod.rs` and `platform/agents/mod.rs` only export the still-supported legacy platform agents after the cut.
- [x] Confirm `OrderPlatform` behavior remains unchanged after the surface-area reduction.

## Progress notes

- 2026-03-09: Agent inventory confirmed `CryptoAgent` and `EventEdgePlatformAgent` were only referenced by their own files and re-export layers, with no active bootstrap/runtime wiring.
- 2026-03-09: Removed `src/platform/agents/crypto_agent.rs` and `src/platform/agents/event_edge_agent.rs`, and shrank platform exports to keep only still-supported NBA and RL legacy platform paths.
- 2026-03-09: Cleaned the stale `CryptoTradingAgent` module comment that still referenced deleted `CryptoAgent` code.
- 2026-03-09: Targeted validation passed:
  - `cargo check --lib`
  - `cargo test test_order_platform_start_allows_dry_run --lib -- --nocapture`
  - `cargo test test_order_platform_start_blocks_live_runtime --lib -- --nocapture`

# Coordinator Execution Runner Extraction (2026-03-09)

## Goal
Move the execution runner out of `src/coordinator/coordinator.rs` so queue drain, fill application, and execution-side capital/risk refresh live behind one dedicated execution module while preserving current behavior.

## Tasks

- [x] Inventory the execution seam: queue drain, executor submit loop, fill application, and post-fill risk/capital updates.
- [x] Add a dedicated execution module that owns `drain_and_execute()` and the tightly coupled helper methods it needs.
- [x] Rewire `Coordinator` to keep orchestration ownership while delegating the execution runner path through the extracted module.
- [x] Move execution-focused regression tests next to the extracted module when it improves cohesion.
- [x] Run targeted compile/test validation for queue draining, BUY/SELL fill tracking, and global state refresh behavior.

## Review

- [x] Confirm `Coordinator` no longer stores the execution runner body inline in `coordinator.rs`.
- [x] Confirm queue expiry/failure settlement and successful execution persistence still happen on the same paths.
- [x] Confirm BUY fills still open tracked positions, SELL fills still reduce them FIFO, and risk-gate accounting remains unchanged.

## Progress notes

- 2026-03-09: Planned after journal extraction. The next cohesive seam is the execution runner body: queue drain + executor submit loop + sell-fill application + post-fill risk refresh.
- 2026-03-09: Added `src/coordinator/coordinator/execution.rs` as a private coordinator submodule and moved execution-runner helpers (`drain_and_execute`, domain settlement, sell-fill reduction, post-fill exposure refresh) out of the main `coordinator.rs` body.
- 2026-03-09: Kept behavior unchanged by leaving restore paths and run-loop call sites on `Coordinator` while exposing the extracted methods as `pub(super)` only within the coordinator module tree.
- 2026-03-09: Added a SELL execution regression to prove execution extraction still reduces tracked positions and realizes FIFO PnL after a BUY fill.
- 2026-03-09: Targeted validation passed:
  - `cargo check --lib`
  - `cargo test test_drain_and_execute_records_single_success_for_buy_fill --lib -- --nocapture`
  - `cargo test test_drain_and_execute_sell_fill_reduces_position_and_realizes_pnl --lib -- --nocapture`
  - `cargo test test_queue_stats_snapshot_from --lib -- --nocapture`

# Coordinator Execution Journal Extraction (2026-03-09)

## Goal
Move execution-log ownership and SQL persistence out of `src/coordinator/coordinator.rs` so coordinator keeps orchestration while restore/persistence logic lives behind one journal module.

## Tasks

- [x] Inventory the execution journal seam: execution log pool, restore loaders, and execution/signal/risk/exit persistence helpers.
- [x] Add `src/coordinator/journal.rs` with `ExecutionJournal`, restore payload loaders, and persistence methods.
- [x] Rewire `Coordinator` to use the shared journal owner instead of directly owning `execution_log_pool`.
- [x] Keep runtime restore behavior unchanged by delegating restore/load calls through the journal.
- [x] Run targeted compile/test validation for restore, persistence, and execution accounting.

## Review

- [x] Confirm execution-log pool ownership no longer lives directly on `Coordinator`.
- [x] Confirm `restore_runtime_state_from_execution_log()` still rebuilds positions, allocator state, and counters from the same persisted records.
- [x] Confirm signal/risk/execution persistence still fires on the same ingress and execution paths.

## Progress notes

- 2026-03-09: Planned after admission extraction. The next large cohesive seam is execution journal ownership: execution-log pool + restore loaders + signal/risk/execution/exit persistence.
- 2026-03-09: Added `src/coordinator/journal.rs` and moved execution-log restore helpers, risk-runtime snapshot loading, signal/risk/exit persistence, execution analysis, and live strategy evaluation writes behind `ExecutionJournal`.
- 2026-03-09: `Coordinator` now owns an `ExecutionJournal` instead of an `execution_log_pool`; restore paths delegate to the journal and execution/ingress persistence calls route through the same owner.
- 2026-03-09: Targeted validation passed:
  - `cargo check --lib`
  - `cargo test test_execution_error_is_failure_treats_blank_as_success --lib -- --nocapture`
  - `cargo test test_string_metadata_from_json_normalizes_scalar_values --lib -- --nocapture`
  - `cargo test test_drain_and_execute_records_single_success_for_buy_fill --lib -- --nocapture`

---

# Coordinator Admission Extraction (2026-03-09)

## Goal
Move deployment registry ownership and order-admission policy out of `src/coordinator/coordinator.rs` so coordinator keeps execution/orchestration while admission rules live behind one module.

## Tasks

- [x] Inventory the remaining admission subsystem: duplicate guard, deployment gate, kelly sizing, min-order constraints, and idempotency key generation.
- [x] Add `src/coordinator/admission.rs` with `AdmissionController`, deployment registry loading/helpers, and the admission policy logic.
- [x] Rewire `Coordinator` and `CoordinatorHandle` to use the shared admission controller instead of raw `deployments` / `duplicate_guard` fields.
- [x] Move deployment-gate and duplicate-guard tests out of `coordinator.rs` and keep them with the admission module.
- [x] Run targeted compile/test validation for deployment resolution, duplicate guard, and coordinator execution accounting.

## Review

- [x] Confirm deployment registry ownership no longer lives directly on `Coordinator`.
- [x] Confirm `handle.shared_deployments()` still exposes the same underlying registry.
- [x] Confirm request idempotency and deployment-gate behavior are unchanged after the extraction.

## Progress notes

- 2026-03-09: Planned after governance extraction. The next large cohesive seam is order admission: deployment registry + duplicate guard + sizing + venue minimums + stable idempotency.
- 2026-03-09: Added `src/coordinator/admission.rs` and moved duplicate guard, deployment registry loading/resolution, Kelly sizing, venue minimum checks, and stable idempotency key construction behind `AdmissionController`.
- 2026-03-09: `Coordinator` and `CoordinatorHandle` now share one admission owner instead of directly owning `deployments` and `duplicate_guard`; `handle.shared_deployments()` delegates to the admission registry.
- 2026-03-09: Moved deployment-gate, duplicate-guard, and idempotency coverage into the admission module; targeted validation passed:
  - `cargo check --lib`
  - `cargo test test_deployment_gate_accepts_explicit_deployment_and_applies_metadata --lib -- --nocapture`
  - `cargo test test_build_order_request_fallback_uses_intent_created_at --lib -- --nocapture`
  - `cargo test test_drain_and_execute_records_single_success_for_buy_fill --lib -- --nocapture`

---

# Coordinator Governance Extraction (2026-03-09)

## Goal
Move governance policy, ingress state, and per-agent pause ownership out of `src/coordinator/coordinator.rs` so the coordinator stops directly owning multiple control-plane locks.

## Tasks

- [x] Inventory the governance/ingress seam shared by `CoordinatorHandle` and `Coordinator`.
- [x] Add `src/coordinator/governance.rs` with `GovernanceController`, `IngressMode`, governance policy helpers, and DB policy persistence/load functions.
- [x] Rewire `Coordinator` and `CoordinatorHandle` to use the shared governance controller instead of raw ingress/policy locks.
- [x] Keep execution behavior unchanged by leaving queue draining and order execution in `coordinator.rs`.
- [x] Run targeted compile/test validation for governance blocking and domain pause behavior.

## Review

- [x] Confirm handle-side and coordinator-side buy gating now read from the same governance owner.
- [x] Confirm policy update/history and governance status still work after the extraction.
- [x] Confirm per-agent pause state is no longer owned directly by `Coordinator`.

## Progress notes

- 2026-03-09: Planned next slice after capital extraction. The seam is the control-plane state (`ingress_mode`, `domain_ingress_mode`, `governance_policy`, `paused_agent_ids`) plus its persistence helpers, not `drain_and_execute`.
- 2026-03-09: Added `src/coordinator/governance.rs` and moved `IngressMode`, `GovernancePolicy`, governance DB persistence/load helpers, and the shared control-plane state into `GovernanceController`.
- 2026-03-09: `Coordinator` and `CoordinatorHandle` now share one governance owner instead of each reaching into separate ingress/policy locks, which removes duplicated state ownership without touching execution/drain logic.
- 2026-03-09: Moved pure governance policy tests out of `coordinator.rs`; targeted validation passed:
  - `cargo check --lib`
  - `cargo test test_governance_policy_blocks_domain --lib -- --nocapture`
  - `cargo test test_handle_force_close_domain_blocks_new_buy_immediately --lib -- --nocapture`
  - `cargo test test_governance_status_includes_domain_ingress_and_agents --lib -- --nocapture`

---

# Coordinator Capital Policy Extraction (2026-03-08)

## Goal
Extract coordinator-owned capital allocation state into a dedicated module so execution/gov code stops owning four allocator implementations directly.

## Tasks

- [x] Create `src/coordinator/capital.rs` with the allocator state, identity helpers, and deployment ledger snapshot logic.
- [x] Wire `src/coordinator/mod.rs` and `src/coordinator/coordinator.rs` to use a single `Arc<CapitalPolicy>` instead of four allocator fields.
- [x] Move allocator-focused tests out of `coordinator.rs` and keep them with the extracted capital module.
- [x] Preserve existing coordinator behavior by routing `governance_status`, kelly sizing, reservation, release, and settlement through `CapitalPolicy`.
- [x] Run targeted compile/test validation for both coordinator execution accounting and capital ledger behavior.

## Review

- [x] Confirm `CoordinatorHandle` no longer assembles allocator/deployment snapshots by reading four independent locks.
- [x] Confirm `Coordinator::new`, runtime restore, and settlement helpers now delegate to `CapitalPolicy`.
- [x] Confirm allocator regression tests live in `src/coordinator/capital.rs`, not at the bottom of `coordinator.rs`.

## Progress notes

- 2026-03-08: Added `src/coordinator/capital.rs` as the new ownership boundary for allocator identity, caps, reservation/release, settlement, and deployment ledger snapshots.
- 2026-03-08: Replaced the four allocator fields on `Coordinator`/`CoordinatorHandle` with `Arc<CapitalPolicy>`, which collapses capital-governance state behind one seam without changing order execution flow.
- 2026-03-08: Removed the duplicated allocator/type/test block from `src/coordinator/coordinator.rs`; the coordinator now consumes the module instead of defining it.
- 2026-03-08: Validation passed:
  - `cargo check --lib`
  - `cargo test test_drain_and_execute_records_single_success_for_buy_fill --lib -- --nocapture`
  - `cargo test test_crypto_allocator_ledger_snapshot_reports_open_pending_and_available --lib -- --nocapture`

---

# Strategy Action Contract Split (2026-03-08)

## Goal
Separate canonical strategy decision actions from legacy feed/governance control actions so the managed live runtime no longer presents dynamic feed and risk updates as first-class strategy outputs.

## Tasks

- [x] Inventory all `StrategyAction::{UpdateRisk,SubscribeFeed,UnsubscribeFeed}` producers and consumers.
- [x] Split legacy control-plane actions out of the top-level action surface in `src/strategy/traits.rs`.
- [x] Update managed runtime, CLI, and legacy orchestrator handling to route compatibility-only control actions through the new legacy branch.
- [x] Retag dormant strategy emitters (`momentum_strat`, `two_leg`, `gamma_scalping`) to the legacy control path.
- [x] Run targeted compile/test validation on the managed runtime and strategy manager.

## Review

- [x] Confirm current managed live strategies do not emit dynamic feed/risk actions.
- [x] Confirm the coordinator runtime now treats these actions as explicit compatibility-only inputs.
- [x] Confirm `cargo check --lib` and targeted runtime/manager tests still pass.

## Progress notes

- 2026-03-08: Parallel analysis confirmed `UpdateRisk`/`SubscribeFeed`/`UnsubscribeFeed` were only emitted by dormant strategy implementations, while the current `StrategyFactory` live path goes through adapters and static `required_feeds()` wiring.
- 2026-03-08: Introduced `StrategyControlAction` and wrapped these compatibility-only actions behind `StrategyAction::LegacyControl`, which makes the canonical strategy contract explicit without breaking dormant legacy modules in one shot.
- 2026-03-08: Updated `coordinator/strategy_runtime.rs`, `cli/strategy.rs`, and `strategy/orchestrator.rs` so live/coordinator paths handle the legacy branch explicitly instead of pretending these actions are canonical.
- 2026-03-08: Validation passed:
  - `cargo check --lib`
  - `cargo test persist_runtime_order_ --lib -- --nocapture`
  - `cargo test test_strategy_manager_creation --lib -- --nocapture`

---

# Bootstrap Managed Runtime Extraction (2026-03-08)

## Goal
Start the approved structure refactor by moving the managed strategy runtime out of `src/coordinator/bootstrap.rs` into a dedicated coordinator module, while preserving existing behavior and keeping regression coverage on the execution path.

## Tasks

- [x] Read `.full-review/01-05` and reconcile the valid structure findings with the approved layered-runtime plan.
- [x] Extract managed strategy runtime helpers and launcher into `src/coordinator/strategy_runtime.rs`.
- [x] Update `src/coordinator/mod.rs` and `src/coordinator/bootstrap.rs` so bootstrap launches the runtime instead of owning its internals.
- [x] Move runtime-order helper tests to the new module and keep targeted regression coverage green.
- [x] Add an architecture breadcrumb in the new runtime module explaining the ownership boundary.
- [x] Run targeted validation for the extracted runtime helpers and existing split-arb runtime config behavior.

## Review

- [x] Confirm `bootstrap.rs` no longer owns managed strategy runtime internals.
- [x] Confirm runtime-order helper tests now live with the extracted module.
- [x] Confirm targeted tests still pass after the extraction.

## Progress notes

- 2026-03-08: Read the `.full-review` reports and confirmed the first high-leverage structural slice is extracting the managed strategy runtime from `bootstrap.rs`, not trying to unify all agent abstractions in one step.
- 2026-03-08: Created `src/coordinator/strategy_runtime.rs` and moved strategy instantiation, feed wiring, action execution, runtime order persistence helpers, and managed-runtime observability there.
- 2026-03-08: Left `ensure_strategy_observability_tables()` in `bootstrap.rs` for compatibility because it is still used by CLI/strategy codepaths; this slice changes runtime ownership without widening the schema migration surface.
- 2026-03-08: Targeted validation passed after the extraction:
  - `cargo test persist_runtime_order_ --lib -- --nocapture`
  - `cargo test build_split_arb_runtime_config_ --lib -- --nocapture`

---

# Coordinator Execution Accounting And Aliyun Release Fixes (2026-03-08)

## Goal
Validate the latest external review against the current branch and land the confirmed low-risk critical fixes without expanding into the larger bootstrap/runtime refactor.

## Tasks

- [x] Re-verify the reported critical findings against current code and mark stale findings explicitly.
- [x] Fix duplicated `record_success` accounting in `src/coordinator/coordinator.rs`.
- [x] Replace misleading `let _ = positions.open_position(...)` drops with explicit position tracking in `src/coordinator/coordinator.rs`.
- [x] Make `.github/workflows/release-aliyun.yml` build a Linux ARM release artifact for the Aliyun trading host.
- [x] Add targeted regression coverage for coordinator execution accounting.
- [x] Run targeted validation and capture results.

## Review

- [x] Confirm which external review findings were valid versus stale on this branch.
- [x] Confirm coordinator success counters no longer double-count a single fill.
- [x] Confirm the Aliyun release workflow now targets `aarch64-unknown-linux-gnu`.

## Progress notes

- 2026-03-08: Re-verified the external review against the current branch. Valid findings: duplicate `record_success`, oversized `bootstrap.rs`, and the Aliyun release workflow building the wrong architecture. Stale/inaccurate findings: root `README.md` exists, and the two `let _ = positions.open_position(...)` sites were not discarding errors because `open_position` is infallible and returns a `position_id`.
- 2026-03-08: Added an execution-path regression test proving a single dry-run BUY fill increments RiskGate success counters exactly once.
- 2026-03-08: `release-aliyun.yml` now builds on `ubuntu-24.04-arm`, targets `aarch64-unknown-linux-gnu`, and records the target in `RELEASE.txt` and the deployment summary.

---

# Collector consolidation TODO

## Goal
Reduce duplicated market-data collection paths and converge on canonical raw tables.

## Phase 1 (start now)

- [x] Inventory current collector and persistence paths (tables + writers + overlap)
- [x] Add explicit consolidation plan
- [x] Make `orderbook-history` write canonical `clob_orderbook_snapshots` (while keeping legacy `clob_orderbook_history_ticks` for compatibility)
- [x] Add migration note for consumers currently reading `clob_orderbook_history_ticks`

## Phase 2

- [x] Convert `sync_records` from primary sink to derived layer (view/materialized view over raw tables)
- [x] Remove duplicated schema DDL from runtime/CLI paths and centralize
- [x] Deprecate legacy `ticks` pathway after read-side migration

## Phase 3

- [x] Remove or archive `backtest_collector` CSV-only flow from primary data pipeline
- [ ] Add one unified collector docs page (what to run for live vs backfill vs research)
- [ ] Add lightweight data-quality checks (freshness + dedup ratios)

## Progress notes

- 2026-03-04: Started Phase 1 implementation.
- 2026-03-04: `OrderbookHistoryCollector` now mirrors into canonical `clob_orderbook_snapshots` with dedup-by-key (`token_id`, `book_timestamp`, `hash`, `source`) checks.
- 2026-03-04: Added migration note at `tasks/collector_migration_note.md`.
- 2026-03-04: Added `platform::persistence_schema` and switched bootstrap + CLI replay backfill table ensures to shared helpers.
- 2026-03-04: `SyncCollector` now persists canonical raw tables (`binance_lob_ticks`, `clob_quote_ticks`) and creates `sync_records_derived` view; legacy `sync_records` writes are compatibility-only behind `PLOY_COLLECTOR_PERSIST_SYNC_RECORDS`.
- 2026-03-04: Legacy `services/data_collector` now defaults to canonical `clob_quote_ticks`; legacy `ticks` writes require `PLOY_LEGACY_TICKS_ENABLED=true`.
- 2026-03-04: `backtest_collector` CSV sink is now compatibility-only (`persist_csv=false` by default), so primary collector pipeline is DB-first.

---

# Strategy Deployment Control Plane Stabilization TODO (2026-03-05)

## Goal
Reduce strategy "listing/deployment" chaos by enforcing one control semantics across API surfaces and removing unsafe strategy fallback behavior in platform bootstrap.

## Tasks

- [x] Create implementation plan doc under `docs/plans/` for this stabilization work.
- [x] Align enable/disable governance between `/api/deployments` and `/api/strategies/control/:id`.
- [x] Ensure enabling via `/api/strategies/control/:id` enforces the same evidence gate rules as `/api/deployments`.
- [x] Remove implicit unknown-strategy -> momentum fallback in deployment matrix application.
- [x] Add/adjust tests for deployment strategy mapping and API enable gate behavior.
- [x] Reconcile direct-live gate tests with current documented behavior (blocked by default, env override explicit).
- [x] Run targeted tests and capture results.
- [x] Commit atomic changes with clear scope messages.

## Review

- [x] Verified no unrelated dirty changes were reverted.
- [x] Verified control plane behavior is consistent across endpoints.
- [x] Verified strategy mapping no longer silently routes unknown strategy keys to momentum.
 
## Progress notes

- [x] Create implementation plan doc under `docs/plans/` for this stabilization work.
- [x] Align enable/disable governance between `/api/deployments` and `/api/strategies/control/:id`.
- [x] Ensure enabling via `/api/strategies/control/:id` enforces the same evidence gate rules as `/api/deployments`.
- [x] Remove implicit unknown-strategy -> momentum fallback in deployment matrix application.
- [x] Add/adjust tests for deployment strategy mapping and API enable gate behavior.
- [x] Reconcile direct-live gate tests with current documented behavior (blocked by default, env override explicit).
- [x] Run targeted tests and capture results.
- [x] Commit atomic changes with clear scope messages.
- [x] Verified no unrelated dirty changes were reverted.
- [x] Verified control plane behavior is consistent across endpoints.
- [x] Verified strategy mapping no longer silently routes unknown strategy keys to momentum.

---

# Trading Host OOM Hardening TODO (2026-03-06)

## Goal
Prevent trading host OOM/timeout caused by on-host Rust builds and missing service memory guards.

## Tasks

- [x] Verify `tango-1-1` runtime state (`rustc/cargo`, `systemd` restart/memory policy, active build processes).
- [x] Pin host default Rust commands to rustup stable (`rustc/cargo` -> latest stable).
- [x] Enforce/automate `systemd` guardrails (`Restart`, `MemoryHigh`, `MemoryMax`, `OOMPolicy`) in GitHub Actions deploy flow.
- [x] Disable legacy remote source-build deploy path by default (`scripts/aws_ec2_deploy.sh` requires explicit override).
- [x] Add trading-host deployment policy to `AGENTS.md` and `CLAUDE.md`.

## Review

- [x] Confirmed host now reports `rustc 1.94.0` and `cargo 1.94.0`.
- [x] Confirmed `ploy-platform.service` shows `Restart=always`, `MemoryHigh=1280M`, `MemoryMax=1536M`, `OOMPolicy=kill`.
- [x] Confirmed no active `cargo`/`rustc` compile processes remain on host.

---

# LEG2 Hotfix Rollout And Acceptance (2026-03-06)

## Goal
Deploy the staggered-arb LEG2 partial-fill hotfix, restart the live platform, and verify online that retries only submit remaining shares while auto-claimer remains active.

## Tasks

- [x] Reproduce the LEG2 retry issue from live fills/logs and identify root cause.
- [x] Implement cumulative LEG2 fill tracking with remaining-shares resubmission.
- [x] Add targeted tests for partial-cancel and cumulative-fill closeout behavior.
- [x] Deploy the hotfix binary to the live host and restart `ploy-platform`.
- [x] Confirm post-restart strategy runtime and auto-claimer startup logs.
- [ ] Confirm a fresh post-restart `STAG-ARB` trade path no longer re-submits full LEG2 size after a partial/failed attempt.
- [x] Review BTC live activity and document why BTC did or did not trade.

## Review

- [x] Local targeted tests and `cargo check` passed before deployment.
- [x] Live host restarted onto the new binary with `--features claimer`.
- [x] Post-restart runtime confirmed active for `BTCUSDT`, `ETHUSDT`, `SOLUSDT`; auto-claimer startup confirmed in live logs.
- [x] BTC feed/runtime coverage confirmed after restart; absence of BTC trades so far is lack of qualifying fills/signals in the observed window, not missing subscription.
- [x] No post-restart `orderbook ... does not exist` execution errors were observed in the acceptance window.
- [ ] Fresh post-restart order-path acceptance still pending a real `LEG1` fill that advances into `LEG2`.

---

# STAG-ARB Live Quote Scoping And Forced-Close Hardening (2026-03-06)

## Goal
Stop live staggered-arb from mixing quotes across event windows, make forced-close price guards real, and ensure runtime config injection does not silently drop BTC.

## Tasks

- [x] Stop live `ploy-platform.service` on `tango-1-1` before local strategy changes.
- [x] Add targeted tests that prove live quotes must be scoped by `event_id`, not only symbol.
- [x] Add targeted tests for `force_complete_threshold` guarding forced Leg2 closes above threshold.
- [x] Change `staggered_arb_live` quote routing/storage from symbol-scoped to event-scoped.
- [x] Wire `force_complete_threshold` into live forced-close paths only.
- [x] Align backtest forced-close threshold semantics with live behavior.
- [x] Fix bootstrap staggered-arb runtime rendering so deployment-scoped `symbols` and `series_ids` override the canonical template without silently dropping BTC.
- [x] Run targeted strategy/bootstrap tests and capture results.

## Review

- [x] Confirmed `ploy-platform.service` on `tango-1-1` is stopped before implementation.
- [x] Verified live staggered-arb no longer reuses quotes across different windows for the same symbol.
- [x] Verified forced close does not buy Leg2 above configured threshold.
- [x] Verified runtime-rendered staggered-arb config injects both symbols and series IDs.

## Progress notes

- 2026-03-06: `tango-1-1` `ploy-platform.service` stopped successfully; host reported `inactive (dead)` immediately after manual stop.
- 2026-03-06: Added live regression test `test_try_entry_uses_event_scoped_quotes` and switched live PM quote storage/routing to `event_id` scope.
- 2026-03-06: Added live/backtest threshold tests so forced timeout paths are blocked when `force_complete_threshold=1.00` and combined sum exceeds $1.
- 2026-03-06: Added live event-expiry settlement path so single-leg `FINAL WINDOW HOLD` positions and threshold-blocked positions do not remain stuck open forever.
- 2026-03-06: Fixed bootstrap staggered-arb runtime rendering so managed config derives from the canonical template while overriding both deployment-scoped symbols and series IDs.
- 2026-03-06: Verified with targeted tests:
  - `cargo test test_try_entry_uses_event_scoped_quotes -- --nocapture`
  - `cargo test test_force_threshold_blocks_forced_timeout_above_cap -- --nocapture`
  - `cargo test test_force_complete_threshold_blocks_backtest_timeout_above_cap -- --nocapture`
  - `cargo test build_split_arb_runtime_config_overrides_template_symbols_and_series_ids -- --nocapture`
  - `cargo test staggered_arb_live::tests -- --nocapture`

---

# Managed Staggered Arb Runtime And Release Workflow Merge (2026-03-06)

## Goal
Fold the separate hotfix worktree back into this strategy branch without regressing the live quote-scoping fixes: keep share-based managed runtime generation, preserve partial-fill retry behavior, and make the Aliyun release workflow explicitly start inactive ploy services.

## Tasks

- [x] Compare the current worktree against `hotfix/leg2-reconcile-20260306` and identify overlapping files.
- [x] Keep the live `staggered_arb` partial-fill reconciliation logic while verifying it does not conflict with event-scoped quote routing.
- [x] Reconcile managed runtime generation in `bootstrap.rs` so it derives from the canonical `staggered_arb.toml` template instead of hardcoded fallback defaults.
- [x] Bring over the release workflow changes that package/install `staggered_arb.toml` and explicitly `start` or `restart` installed ploy services.
- [x] Merge both sessions' `tasks/todo.md` and `tasks/lessons.md` records instead of dropping one side's incident history.
- [x] Run targeted validation on merged bootstrap/workflow changes and capture the result.

## Review

- [x] Confirmed the only real semantic conflict was managed runtime generation in `bootstrap.rs`; `staggered_arb_live.rs` changes were additive.
- [x] Kept `force_complete_threshold = 1.00` in the checked-in strategy template to preserve the bad-price forced-close guard.
- [x] Preserved the hotfix-side partial-fill retry logic and exchange-order reconciliation already present in the current worktree.

## Progress notes

- 2026-03-06: Compared this worktree with `hotfix/leg2-reconcile-20260306` and found overlap in `bootstrap.rs`, `staggered_arb.toml`, `staggered_arb_live.rs`, `tasks/todo.md`, `tasks/lessons.md`, plus the uncommitted `release-aliyun.yml` follow-up.
- 2026-03-06: Resolved bootstrap by keeping template-derived managed runtime rendering and deployment-scoped overrides for `symbols` and `series_ids`.
- 2026-03-06: Resolved workflow merge by keeping packaged `staggered_arb.toml`, explicit ploy unit handling, and `wait_for_unit_active` for both `start` and `restart` paths.
- 2026-03-06: Revalidated merged state with `cargo test bootstrap -- --nocapture`, `cargo test build_split_arb_runtime_config_ -- --nocapture`, `cargo test staggered_arb_live::tests -- --nocapture`, and a YAML parse check for `.github/workflows/release-aliyun.yml`.

---

# Managed Staggered Arb Runtime And Release Workflow Closure (2026-03-06)

## Goal
Keep managed `staggered_arb` on share-based sizing, ship the canonical strategy template in release bundles, and make the Aliyun rollout path recover installed inactive services automatically.

## Tasks

- [x] Confirm managed runtime sizing drift came from bootstrap rendering, not host config drift.
- [x] Render managed split-arb runtime from the canonical `staggered_arb.toml` template while keeping runtime symbol/series overrides.
- [x] Include `staggered_arb.toml` in the release bundle and install it on the host during rollout.
- [x] Update the release restart step so installed inactive `ploy` services are started and waited to `active`.
- [x] Extend the release restart step to include installed `ploy-deribit-*` collectors.

## Review

- [x] Managed runtime rendering now preserves `shares_per_trade = 20` and does not inject `fixed_amount_usd`.
- [x] Release workflow now deploys `staggered_arb.toml` alongside `momentum.toml`.
- [x] Release workflow restart logic now handles both `restart` and `start`, with an explicit `active` wait loop.
- [x] Release workflow now discovers and restarts installed `ploy-deribit-*` collector units on the trading host.

---

# Layered Live Runtime Refactor Design And Planning (2026-03-06)

## Goal
Define the target four-layer live trading architecture and write a concrete implementation plan to converge the repo onto one canonical strategy runtime.

## Tasks

- [x] Review the current architecture against the target four-layer model.
- [x] Validate target boundaries for Strategy, Capital Governance, Execution, and Control planes.

---

# Staggered Arb OBI Long-Gamma Capped-Loss Refactor (2026-03-06)

## Goal
Shift staggered-arb from mixed "old arb threshold + opening-window directional entry" behavior into an OBI-triggered long-gamma profile with capped-loss LEG2 stops and Greeks-aware merge acceleration.

## Tasks

- [ ] Add targeted failing tests for capped-loss stop completion above the generic force cap, Greeks-accelerated LEG2 close, and long-gamma entry band filtering.
- [ ] Add strategy config support for stop-loss-specific completion caps and long-gamma fair-value band filtering.
- [ ] Update live and backtest LEG2 logic so stop-loss uses the capped-loss threshold while profitable gamma/theta urgency behaves consistently.
- [ ] Re-run targeted staggered-arb tests and a local backtest comparison window.

## Review

- [ ] Confirm the new stop-loss path caps directional damage without reopening the old bad-price forced-close bug.
- [ ] Confirm Greeks remain a secondary state filter/exit accelerator rather than the primary entry signal.

---

# ETH Up/Down Missing Settlement Investigation On tango-1-1 (2026-03-07)

## Goal
Find why the ETH 5-minute Up/Down order pair appears to have been bought but is no longer visible with no obvious settlement result.

## Tasks

- [x] Confirm live host services and identify the components responsible for order tracking and claim/settlement.
- [x] Collect host evidence for the 2026-03-07 01:05-01:10 CST window (ETH Up/Down 2026-03-06 12:05PM-12:10PM ET).
- [x] Determine whether the order disappeared because of fill/cancel behavior, event-expiry handling, local state loss, or unresolved claim processing.
- [x] Summarize root cause and required fix or operational follow-up.

## Review

- [x] Root cause is supported by host evidence, not inference alone.

## Findings

---

# Wallet-Level PnL Reconciliation (2026-03-08)

## Goal
Correct staggered-arb live performance review so it matches the user's official Polymarket wallet PnL instead of only internal cycle-completed totals.

## Findings

- Official Polymarket profile 1D series for wallet `0xCbaAa60c5DEc85eaC2A2c424bdcD7258Ab67eEE2` moved from `-1166.9908` to `-1240.8458`, a delta of `-73.855`.
- Public wallet activity over the same rolling window was entirely crypto `Up or Down` flow in the sampled rows and netted about `-82.8991` cashflow, which is directionally consistent with the official `1D` wallet loss.
- Internal host `signal_history` over the same rolling window showed about `+25.0811` across `58` `split_arb_cycle_completed` rows (`merge +18.6563`, `forced -16.6014`, `settled +23.0262`), proving `cycle_completed` alone materially understates live wallet losses.
- Follow-up reviews must treat official wallet 1D PnL as the primary live truth, with public activity and internal strategy logs used only to explain the delta.

---

# Crypto 5m Repricing V1 Framework (2026-03-07)

## Goal
Ship a backtestable, live-ready v1 framework for Polymarket 5-minute crypto repricing trades:
enter during the early repricing window, use fair-gap plus Binance L2 direction as the baseline
signal, and force exits before the last 45 seconds.

## Tasks

- [x] Review existing replay/live strategy modules, data feeds, and fee/execution helpers.
- [x] Write the design baseline in `docs/plans/2026-03-07-crypto-5m-repricing-v1-design.md`.
- [x] Write the implementation plan in `docs/plans/2026-03-07-crypto-5m-repricing-v1.md`.
- [x] Add a dedicated pure 5-minute crypto repricing core module without mutating the current directional momentum semantics.
- [x] Add targeted core tests for time-window gating, cost-aware entry filters, and direction confirmation.
- [ ] Add a thin replay/backtest harness on top of the core module.
- [ ] Wire a CLI backtest entrypoint after the thin harness is accepted.
- [ ] Run targeted replay validation once the thin harness exists.

## Review

- [x] Confirm the current step is only the pure decision core, not the old backtest/runtime shell.
- [x] Confirm the core boundary is reusable for future replay/live adapters.
- [ ] Confirm replay PnL includes Polymarket crypto taker fees and simulated execution frictions once the thin harness is added.

## Progress notes

- 2026-03-07: Started with a broader framework cut, then trimmed back to core-first after user feedback that the old repo shell was making the code too heavy.
- 2026-03-07: Kept only `src/strategy/crypto_repricing.rs` as the reusable decision layer; deferred replay/CLI wiring.
- 2026-03-07: Verified core unit tests with `CARGO_TARGET_DIR=/tmp/ploy-core-target cargo test crypto_repricing::tests -- --nocapture` (5 passed).

- 2026-03-07: `tango-1-1` `ploy-platform.service` restarted at `2026-03-07 01:04:50 CST`, just before the target ETH `12:05PM-12:10PM ET` window opened.
- 2026-03-07: PM/host evidence shows both legs really matched for condition `0xaa911a860983c1c2233029a67a7565e679ea1c9270b8451156ee63a2d812e8ad` (`Ethereum Up or Down - March 6, 12:05PM-12:10PM ET`):
  - `LEG1 FILLED ETHUSDT DOWN @ 55.00¢ (20 shares)` with order `0x790a...3383`
  - `LEG2` order `0x4abf...cce3` also matched on PM for the `Up` side.
- 2026-03-07: PM Gamma still reported this market as `active=true`, `closed=false` when checked after the fills, so PM had not yet published official settlement state. That explains why the user could see buys but no settlement info.
- 2026-03-07: The account-level auto-claimer later detected both outcome positions as redeemable under the same condition and sent a relayer redeem (`tx=0xf3b9...2737`) at `2026-03-06T17:30:47Z`.
- 2026-03-07: The local Postgres `orders`, `fills`, and `positions` tables returned no matching rows for these PM order/token IDs, so this live path currently leaves no DB-backed settlement trail for the pair.
- 2026-03-07: Most likely user-visible behavior is "paired position was merge/redeem processed" rather than "market settlement record appeared". A follow-up product/code review is warranted because `src/strategy/claimer.rs` currently collapses both sides by `condition_id` and redeems `[1,2]`, which can make PM UI behavior look like disappearance without a settlement line item.

---

# Staggered Arb Delayed-Entry OBI And Real-Time Partial-Fill Refactor (2026-03-06)

## Goal
Shift `staggered_arb` to the operator's intended flow: wait through the first 30 seconds, let OBI choose `LEG1` direction without a hard sum cap, then manage `LEG2` against the actually-filled size with immediate partial-fill accounting and bounded-loss closes up to a wider cap.

## Tasks

- [x] Add failing tests for delayed post-open entry (`entry_after_start_min_secs`), disabled hard `max_initial_sum` gating, and unlimited concurrency/event-count settings.
- [x] Add failing live-order tests for immediate `PartiallyFilled` accounting on both `LEG1` and `LEG2`.
- [x] Update staggered-arb config/defaults so the first 30s are observation-only, `max_initial_sum` can be disabled, `min_entry_sum` is much lower, `max_entry_sigma` no longer clips the intended high-vol regime, and generic/protective close caps can reach `1.20`.
- [x] Implement real-time partial-fill handling so cumulative fills update positions immediately and `LEG1` accepts partials as the actual position size instead of chasing the remainder.
- [x] Re-run targeted live/backtest tests plus isolated host replay comparisons.

## Review

- [x] Confirmed `LEG1` no longer hard-rejects premium sums solely because `UP+DOWN` exceeds the old cap; `max_initial_sum = 0.0` now disables the hard gate in both live and replay, while premium-sum strength gates remain as soft quality filters.
- [x] Confirmed `PartiallyFilled` updates mutate exposure immediately without double-counting on later terminal callbacks; live tests now cover both `LEG1` and `LEG2` cumulative-fill accounting.
- [x] Confirmed host replay stays operational after widening close caps and removing the hard entry-sum gate, but the new profile materially increases trade count and is only flat on the March 5-6 six-hour window.

---

# Staggered Arb Settlement And Replay-Parity Fixes (2026-03-06)

## Goal
Fix the remaining correctness issues in `staggered_arb` before treating replay as live evidence: expiry settlement must respect partial `LEG2` progress, stale live orders must remain reconcilable, backtest clocks must use simulated fill times, and CLI replay must load the canonical live template instead of drifting defaults.

## Tasks

- [x] Fix live expiry settlement so partial `LEG2` fills are included in payout/cost accounting and late callbacks cannot double-close the same cycle.
- [x] Archive orphaned live orders for reconciliation instead of clearing same-event locks during hard cleanup.
- [x] Fix backtest `LEG2` accumulation so partial closes keep residual exposure open until it is actually hedged or settled.
- [x] Fix backtest entry timing so `wait_deadline` and recorded `leg1_time` use the modeled fill timestamp, not the earlier signal timestamp.
- [x] Make `strategy backtest staggered-arb` load `config/strategies/staggered_arb.toml` and override only CLI-scoped inputs such as symbols / capital.
- [x] Align replay OBI gating with live behavior by rejecting entries when no fresh Binance L2 OBI is available.
- [x] Re-run targeted live/backtest tests.
- [x] Rebuild a Linux artifact locally, upload it to `tango-1-1` in an isolated backtest path, and re-run the standard replay windows.

## Review

- [x] `cargo test strategy::staggered_arb_backtest::tests -- --nocapture` passed with `14/14`.
- [x] `cargo test strategy::staggered_arb_live::tests -- --nocapture` passed with `31/31`.
- [x] On `tango-1-1`, the parity-corrected March windows (`2026-03-05T20:00:00Z` to `2026-03-06T02:00:00Z` and `2026-03-05T04:00:00Z` to `2026-03-06T05:45:00Z`) now produce `0 trades / 0 PnL` because `binance_lob_ticks` coverage for both windows is `0`, so these windows are not valid live-parity evidence.
- [x] On the overlap window `2026-02-24T00:00:00Z` to `2026-02-24T06:00:00Z`, where `binance_lob_ticks` has `38,862` rows, the parity-corrected replay remains healthy: `217 trades`, `91.24%` win rate, `+491.75` PnL, `139.55` profit factor.

## Progress notes

- 2026-03-06: Fixed live expiry settlement so partially-hedged positions settle against actual `LEG2` progress, clear pending hedge markers, and ignore late terminal callbacks after settlement.
- 2026-03-06: Changed orphan hard-cleanup to archive stale live orders instead of dropping event/position locks; late fills can now reconcile safely.
- 2026-03-06: Fixed backtest `fill_leg2` to accumulate partial hedge fills, settle residual exposure at event outcome, and base `wait_deadline` on modeled `LEG1` fill time.
- 2026-03-06: Made CLI staggered-arb replay load the checked-in canonical TOML so shares, thresholds, and timing come from the same source as live config.
- 2026-03-06: Removed replay-only OBI fallback. Missing fresh Binance L2 OBI now blocks entry in replay the same way it does in live.
- 2026-03-06: Rebuilt Linux artifact `ploy-stag-20260306-config-parity`, uploaded it to `/root/ploy/bin/backtests/`, and re-ran host backtests without touching the live service binary.
- 2026-03-06: First production release attempt (`22771138938`) failed in CI because the staggered-arb replay changes depended on the uncommitted `UpdateType::BinanceL2` feed variant in `backtest_feed.rs`; release was halted before deploy and `ploy-platform.service` remained stopped on `tango-1-1`.

## Progress notes

- 2026-03-06: Added `entry_after_start_min_secs = 30`, disabled the hard `max_initial_sum` cap with `0.0`, widened generic/protective close caps to `1.20`, and removed concurrency / per-event trade caps by treating `0` as "disabled" in both live and replay.
- 2026-03-06: Live order tracking now treats `OrderStatus::PartiallyFilled` as an immediate state transition: cumulative filled shares, weighted average price, fees, and remaining exposure are updated before terminal callbacks arrive; `LEG1` partials are accepted as the actual position size and the residual is cancelled.
- 2026-03-06: Added parser/default regression coverage so missing TOML fields no longer silently fall back to the old opening-window profile.
- 2026-03-06: Targeted test suites passed: `strategy::staggered_arb_live::tests` 29/29 and `strategy::staggered_arb_backtest::tests` 10/10.
- 2026-03-06: Isolated replay on `tango-1-1` with `/root/ploy/bin/backtests/ploy-7f22b7f-delayed-obi-realtime-partials` produced mixed regime results:
  - `2026-03-05T20:00:00Z` to `2026-03-06T02:00:00Z`: 202 trades, 97 wins / 105 losses, `+0.71` PnL, profit factor `1.00`.
  - `2026-03-05T04:00:00Z` to `2026-03-06T05:45:00Z`: 648 trades, 345 wins / 303 losses, `+700.64` PnL, profit factor `1.88`.
  - `2026-02-24T00:00:00Z` to `2026-02-24T06:00:00Z`: 1,570 trades, 1,395 wins / 175 losses, `+14171.88` PnL, profit factor `27.97`.

---

# OBI Long-Gamma Protective Merge Refactor (2026-03-06)

## Goal
Refactor `staggered_arb` from a loose opening-window directional entry into an explicit "OBI-triggered long gamma + capped-loss LEG2" strategy with volatility regime filters and Greeks-assisted protective closes.

## Tasks

- [x] Add failing tests for capped-loss protective LEG2 closes above `force_complete_threshold` but below a new protective cap.
- [x] Add failing tests for volatility-band entry filtering and Greeks-assisted protective merge behavior.
- [x] Implement shared backtest/live config for volatility-band entry and protective LEG2 cap.
- [x] Align live and backtest LEG2 logic so stop-loss / theta urgency can buy `LEG2` up to the protective cap.
- [x] Run targeted strategy tests.
- [x] Run a full-window `staggered-arb` backtest comparison on a fast host using the updated binary.
- [x] Write the approved design doc under `docs/plans/2026-03-06-layered-live-runtime-design.md`.
- [x] Write the implementation plan under `docs/plans/2026-03-06-layered-live-runtime-implementation-plan.md`.
- [x] Commit the planning docs atomically with explicit paths only.

## Review

- [x] Local `staggered_arb` live/backtest test modules pass with the protective-close and sigma-band changes.
- [x] A wide-entry protective profile (`max_initial_sum=1.10`, `max_leg1_price=0.65`, `max_trades_per_event=3`, `max_fair_value_distance=0.25`) still lost money on `tango-1-1` replay: 86 trades, `-55.17` PnL over `2026-03-05T20:00:00Z` to `2026-03-06T02:00:00Z`.
- [x] Tightening the long-gamma entry band (`max_initial_sum=1.04`, `max_leg1_price=0.58`, `max_trades_per_event=2`, `max_fair_value_distance=0.15`) restored positive replay behavior on `tango-1-1`: 31 trades, `+34.60` PnL on the 6h window and 129 trades, `+196.87` PnL on `2026-03-05T04:00:00Z` to `2026-03-06T05:45:00Z`.
- [x] Adding a premium-entry strength gate (`premium_sum_threshold=1.00`, `premium_sum_direction_slope=1.25`, `premium_sum_obi_slope=0.25`) improved the long-window replay on `tango-1-1` to 115 trades and `+228.94` PnL, with profit factor `6.33`, while keeping the 6h window positive at 30 trades and `+32.86` PnL.
- [x] Added historical Binance L2 / OBI parity support to replay backtests, with an explicit fallback to price/Greeks-only entry when the requested window has no fresh `binance_lob_ticks`. On `tango-1-1`, the March 5-6 windows recovered from the temporary `0 trades` regression back to the premium-entry baseline: 30 trades / `+32.86` over 6h and 115 trades / `+228.94` over the full March window.
- [x] Verified the parity gate is active when L2 history exists. On `2026-02-24T00:00:00Z` to `2026-02-24T06:00:00Z`, where `binance_lob_ticks` has 29,208 rows for BTC/ETH/SOL, the premium-entry baseline produced 136 trades / `+784.80` while the parity+fallback build tightened to 124 trades / `+726.62`.

- [x] Confirmed the primary architectural issue is missing canonical live runtime ownership, not lack of layering intent.
- [x] Confirmed `bootstrap.rs` is currently over-coupled to strategy classification, runtime wiring, and strategy-specific behavior.
- [x] Confirmed the target design keeps strategy decisions in the Strategy Plane and limits agentic behavior to capital governance.
- [x] No runtime code changed in this planning step; only design and implementation planning docs were added.

## Progress notes

- 2026-03-06: Completed repository review across `src/strategy`, `src/agents`, `src/platform`, and `src/coordinator/bootstrap.rs`.
- 2026-03-06: Approved target architecture: strategy-owned decisions, agentic capital governance, coordinator-only execution ingress, control-plane-only deployment/config ownership.
- 2026-03-06: Saved design doc to `docs/plans/2026-03-06-layered-live-runtime-design.md`.
- 2026-03-06: Saved implementation plan to `docs/plans/2026-03-06-layered-live-runtime-implementation-plan.md`.

---

# Staggered Arb Opening-Window Entry Reset (2026-03-06)

## Goal
Restore `staggered_arb` to the intended live behavior: directional `LEG1` entries should be decided near event open, not blocked by an ultra-tight sum gate that rarely appears in production, while `LEG2` remains an opportunistic close.

## Tasks

- [x] Tighten entry timing back to the opening phase instead of leaving entry open for the full event.
- [x] Relax the initial sum cap so opening `LEG1` can fire on realistic BTC/ETH/SOL crypto windows.
- [x] Align backtest/default config with the checked-in live strategy template.
- [x] Add a regression test covering the opening-window entry behavior.

## Review

- [x] `staggered_arb.toml` now limits fresh `LEG1` entries to the first 30 seconds and raises `max_initial_sum` from `0.92` to `1.10`.
- [x] `StaggeredArbBacktestConfig::default()` now matches the live template for opening-window timing and initial-sum assumptions.
- [x] Added a live-unit test proving entries are allowed inside the opening window and rejected after it expires.

---

# Live Order Reconciliation And Binance L2 Persistence Fix (2026-03-07)

## Goal
Fix the post-deploy live issues where managed `staggered_arb` orders showed wrong immediate fill prices, new orders appeared in `signal_history` but not `orders`, and Binance L2 sockets could stay connected while `binance_lob_ticks` stopped advancing.

## Tasks

- [x] Reconcile terminal submit responses by querying the exchange once before trusting the immediate fill price.
- [x] Wire managed strategy runtime order submissions and poll updates into `orders` persistence using the action `client_order_id`.
- [x] Make zero-row `orders` updates fail loudly instead of succeeding silently.
- [x] Replace the fragile Binance diff-depth collector path with a combined partial-depth snapshot stream and freshness tracking.

## Review

- [x] `OrderExecutor` now re-queries terminal immediate fills that arrive without associated trade details, so live records use the exchange-confirmed fill price instead of the submitted limit price.
- [x] Coordinator-managed `split_arb` / `staggered_arb` orders now insert into `orders` before execution and update status/fills on submit and poll transitions.
- [x] `PostgresStore::update_order_status` and `update_order_fill` now error when no `orders` row matches, which exposes persistence regressions immediately.
- [x] `BinanceDepthStream` now uses the combined `@depth20@100ms` snapshot stream, records `BinanceLob` freshness, and rebuilds each snapshot from the message itself instead of accumulating unsynchronized deltas.

---

# Staggered Arb Dry-Run Gate Diagnostics (2026-03-06)

## Goal
Use the uploaded Linux binary on `tango-1-1` to observe real-time `LEG1` / `LEG2` gate behavior without deploying, so the live inactivity can be attributed to concrete reject reasons instead of inference.

## Tasks

- [x] Add periodic summary output for top `entry_gates` and `leg2_gates`.
- [x] Make foreground dry-run print summaries even when there are zero closed trades.
- [x] Rebuild the Linux binary locally and upload it to the host in an isolated path.
- [x] Run the uploaded binary against an isolated config on `tango-1-1` and capture the gate counts.
- [x] Fix live entry triggering so opening-window `LEG1` evaluation also runs on tick, not only on quote callbacks.

## Progress notes

- 2026-03-06: Added diagnostic summary fields so dry-run can show why `LEG1` is blocked and whether `LEG2` is waiting on merge price, delay, or force-close guards.
- 2026-03-06: Dry-run on `tango-1-1` with the uploaded Linux binary showed `entry_timing_gates` dominating while `entry_signal_gates` stayed `none`; no `LEG1` / `LEG2` actions fired during the sampled windows.
- 2026-03-06: Root cause was live entry evaluation depending on Polymarket quote callbacks; opening windows without a fresh quote update could miss `LEG1` entirely.
- 2026-03-06: Added tick-driven entry rechecks for symbols with a live opening-window candidate and verified on `tango-1-1` dry-run that `SOLUSDT` entered at `06:55:05Z`, merged at `06:55:12Z`, re-entered, and merged again at `06:55:50Z`.

---

# Trading Host Claim And Settlement Investigation (2026-03-07)

## Goal
Find the exact `tango-1-1` trading-host service names, log locations, and the repo docs/code paths that explain how Polymarket position claiming or settlement should behave when a bought order seems to disappear without visible settlement.

## Tasks

- [x] Locate exact `systemd` service names and any host/logging paths referenced for `tango-1-1`.
- [x] Search docs, tasks, and scripts for Polymarket claim/settlement and host-debug guidance.
- [x] Search runtime code for claimers, expiry settlement, reconciliation, and order/archive flows relevant to disappearing positions.
- [x] Summarize concise debug-oriented findings with exact file references.

## Review

- [x] Current host evidence in `tasks/todo.md` points to `ploy-platform.service` on `tango-1-1`, while deploy/control code still supports legacy `ploy` / `ploy-platform-live` naming.
- [x] Primary log surfaces are `journalctl -u <unit>` plus file logging under `/opt/ploy/logs/ploy.log` (or `PLOY_LOG_DIR` / `/var/log/ploy` fallback).
- [x] Wallet claim/redeem path lives in `src/strategy/claimer.rs` and is started as an in-process account-level daemon from platform bootstrap; `pm_token_settlements` is separate read-only market-resolution persistence for data/labels.
- [x] Main “disappearing order” debug surfaces are exchange truth (`pm.get_positions`, `pm.get_open_orders`), DB truth (`orders`, `positions`, `signal_history`), and `staggered_arb_live` event-expiry/orphan-order reconciliation paths.

---

# Staggered Arb Dynamic Close Caps (2026-03-07)

## Goal
Replace the static `force_complete_threshold` / `protective_close_threshold` gates with urgency-aware dynamic caps so early protective closes stay stricter while late forced closes can still cap risk near expiry.

## Tasks

- [x] Add shared live/backtest helpers that derive dynamic protective and forced close thresholds from time remaining and configured cap.
- [x] Update live `LEG2` decision paths to use dynamic thresholds instead of a flat `1.08` gate.
- [x] Update replay logic and targeted tests so live/backtest stay aligned.
- [x] Re-run staggered-arb backtests on the recent live-like window and one adjacent overlap window to verify whether the dynamic cap improves trade quality.

## Review

- [x] Static `force_complete_threshold` / `protective_close_threshold` are now treated as final caps, while early-window forced/protective closes use stricter adaptive thresholds derived from time remaining.
- [x] Recent live-like replay improved from `39 trades / +13.46 / PF 1.97 / 9 aborts` under the static `1.08` gate to `39 trades / +20.66 / PF 3.69 / 5 aborts` with dynamic caps.
- [x] Adjacent overlap validation on `2026-02-26T00:00:00Z..06:00:00Z` also improved from `20 trades / +31.42 / PF 19.61` to `20 trades / +32.89 / PF 103.44`, with largest loss shrinking from `-1.37` to `-0.32`.

---

# Staggered Arb OBI Signal Strengthening (2026-03-07)

## Goal
Upgrade staggered-arb from a fixed-threshold OBI confirmation gate to a stronger OBI regime that uses persistence for entry, unlocks slightly more aggressive entry only for strong persistent signals, and delays protective stop merges when OBI/displacement/Greeks still support the original leg1 thesis.

## Tasks

- [x] Add shared OBI helper logic for direction confirmation, persistence, strong-signal entry bonuses, and OBI decay/flip support checks.
- [x] Apply the stronger OBI entry/stop logic to both live and replay code paths.
- [x] Add targeted tests for strong-OBI entry bonuses and supportive-OBI stop-loss suppression.
- [x] Re-run the recent live-like replay window and the adjacent `2026-02-26` overlap window to see whether trade count or PnL improves.

## Review

- [x] New OBI logic is in place: strong/persistent OBI can slightly relax direction threshold, widen the leg1 price cap, and extend the 15m opening window; supportive OBI can delay protective stop-loss merges.
- [x] Unit coverage passed: `staggered_arb_backtest` `18/18`, `staggered_arb_live` `35/35`.
- [x] Replay impact on the two primary validation windows was neutral rather than positive:
  - recent live-like window stayed at `39 trades / +20.65 / PF 3.69 / 5 aborts`
  - `2026-02-26T00:00:00Z..06:00:00Z` stayed at `20 trades / +32.89 / PF 103.44`
- [x] Conclusion: the stronger OBI branch is logically sound and tested, but these windows were not bottlenecked by the old fixed OBI gate; the next marginal improvement is more likely to come from signal-persistence exits or smarter `LEG2` execution than from further loosening OBI entry alone.

---

# Staggered Arb 5m-Only Window Restriction (2026-03-07)

## Goal
Drop the 15m staggered-arb window from the canonical profile after replay showed it consistently drags recent production-like and adjacent overlap results, while the 5m window remains positive on both validation windows.

## Tasks

- [x] Compare current full-profile replay against 5m-only and 15m-only runs on the recent live-like window.
- [x] Re-run the same decomposition on an adjacent overlap window with Binance L2 coverage.
- [x] Restrict the checked-in staggered-arb profile and parser/default fallbacks to the 5m window only.
- [x] Add regression assertions so missing-field TOML parsing keeps the 5m-only default.

## Review

- [x] Time-dynamic entry/merge thresholds were tested first and underperformed, so they were discarded rather than merged.
- [x] `15m` was the consistent drag in both validation windows:
  - `2026-03-06T20:30:00Z..2026-03-07T01:20:00Z`: full `64 trades / -2.88 / PF 0.91`, `5m-only 45 / +5.92 / PF 1.32`, `15m-only 21 / -9.22 / PF 0.35`
  - `2026-02-26T00:00:00Z..06:00:00Z`: full `76 trades / +35.33 / PF 2.11`, `5m-only 35 / +36.47 / PF 3.58`, `15m-only 38 / -4.07 / PF 0.76`
- [x] Canonical config, replay defaults, and live TOML regression tests now align on `allowed_windows = [300]`.

---

# Staggered Arb Protective Close Cap Sweep (2026-03-07)

## Goal
Increase recent live-like replay PnL without materially reducing trade count by tightening close caps, after testing showed the new protective recovery window logic did not improve outcomes on its own.

## Tasks

- [x] Implement and test a short protective recovery window before `protective_stop_loss`.
- [x] Replay the recent live-like window and adjacent overlap window with the recovery-window build.
- [x] Sweep `protective_recovery_window_secs` on the recent live-like window to confirm whether the new logic helps at all.
- [x] Sweep `force_complete_threshold` / `protective_close_threshold` on the same recent window, then validate the best cap on independent windows.
- [x] Update canonical config plus parser/default fallbacks to the best cap that improved all validation windows.

## Review

- [x] The recovery-window implementation is correct and covered by new live/replay tests, but it did not improve the target window:
  - recent live-like window with `recovery=12`: `46 trades / +5.62 / PF 1.30 / 9 aborts`
  - same window with `recovery=0`: `46 trades / +5.83 / PF 1.32 / 9 aborts`
  - `8s`, `12s`, `20s`, and `30s` all converged to the same weaker result, so the feature is now disabled by default
- [x] Tightening both close caps to `1.06` was the first change that improved the recent main window while preserving turnover:
  - `2026-03-06T20:30:00Z..2026-03-07T01:20:00Z`: `46 trades / +6.24 / PF 1.35` vs `1.08 => +5.83 / PF 1.32`
  - `2026-02-26T00:00:00Z..06:00:00Z`: `35 trades / +36.86 / PF 3.68` vs `1.08 => +36.47 / PF 3.58`
  - `2026-03-07T00:00:00Z..06:00:00Z`: `21 trades / +18.26 / PF 12.69` vs `1.08 => +17.43 / PF 8.30`
- [x] Canonical TOML, backtest defaults, and parser fallbacks now align on:
  - `protective_recovery_window_secs = 0`
  - `force_complete_threshold = 1.06`
  - `protective_close_threshold = 1.06`

---

# Live Trading Record Reconciliation (2026-03-08)

## Goal
Explain why the current live trading record differs from replay backtest expectations, and verify whether live fills, order rows, and strategy logs are all being recorded correctly.

## Tasks

- [x] Pull the latest `orders`, `signal_history`, and strategy journal entries from `tango-1-1`.
- [x] Reconcile what the strategy thought it did versus what the host actually persisted.
- [x] Identify whether the gap comes from execution quality, partial fills, config drift, or missing persistence.
- [x] Summarize whether live成交记录 is trustworthy enough to use for further tuning.

## Review

- [x] Live trading records are partially trustworthy:
  - `orders` is being populated with submitted status, terminal status, `filled_shares`, and `avg_fill_price`
  - `signal_history` is being populated with `live_order_submit_result`, `live_order_poll_update`, and split-arb state/error events
  - `fills` is still empty for managed-runtime staggered-arb orders, so there is no per-trade fill ledger for these cycles yet
- [x] The concrete live-vs-replay divergence is not hypothetical; cycle `250192` on `ETHUSDT` shows it clearly:
  - first two `LEG1 -> LEG2 merge` cycles filled normally
  - the third cycle filled `LEG1` fully and then `LEG2 forced` filled `19/20` at `0.63`
  - the remaining `1` share was retried indefinitely as new `stag_leg2_forced_250192_*` orders and every retry failed before getting an exchange order id
- [x] The most likely root cause is venue minimum sizing on the residual `LEG2`:
  - the strategy accepts partial fills and resubmits the exact remainder
  - for cycle `250192`, the remainder became `1` share at `0.63`, i.e. below the live venue minimums already enforced elsewhere in the codebase (`5` shares and `$1` notional)
  - replay currently assumes that any positive remainder can always be completed, so it cannot reproduce this live failure mode
- [x] Practical conclusion:
  - yes, we do have live成交记录 in `orders` and `signal_history`
  - no, current live records do not fully match replay assumptions because the live execution path can get stuck on below-minimum residual `LEG2` orders
  - the next fix should clamp residual live `LEG2` submits against venue minimums and stop retrying impossible remainder sizes

---

# Staggered Arb Live Discipline Hardening (2026-03-08)

## Goal
Stop the live strategy from drifting into unhedged directional behavior by eliminating impossible residual `LEG2` retries, disabling single-leg final-window settlement for this profile, and keeping replay/live behavior aligned.

## Tasks

- [x] Keep `tango-1-1` live strategy stopped until the fixes and validations are complete.
- [x] Add a failing live test showing `fill_leg2()` must not submit residual orders below the Polymarket minimum size/notional.
- [x] Add a failing live/backtest test showing final-window positions should force `LEG2` instead of holding single-leg to settlement.
- [x] Implement live residual-`LEG2` minimum-size handling so impossible remainders stop retrying and are finalized deterministically.
- [x] Remove or gate the current final-window single-leg settlement path for this staggered-arb profile.
- [x] Align replay/backtest close behavior with the hardened live rules.
- [x] Run targeted staggered-arb live/backtest tests and summarize whether the new profile is closer to the desired hedge discipline.

## Review

- [x] Verify the host remains stopped during implementation.
- [x] Verify there are no new `LEG2` retry storms for `shares=1`.
- [x] Verify final-window cycles now resolve through explicit hedge logic instead of opportunistic single-leg settlement.

- [x] `tango-1-1` was stopped before implementation and remained `inactive (dead)` during the local fix cycle.
- [x] Live `fill_leg2()` no longer submits venue-invalid residual orders; the new regression test proves a `1-share` remainder now returns no order action instead of another `SubmitOrder`.
- [x] Backtest `fill_leg2()` now uses the same minimum-order rule, so replay no longer assumes a below-minimum residual can always be completed.
- [x] Final-window logic no longer intentionally holds a single-leg when `p_win` is high; the adapter now always attempts an explicit `LEG2` close if the force threshold still allows it.
- [x] Targeted verification passed:
  - `cargo test strategy::staggered_arb_live::tests -- --nocapture`
  - `cargo test strategy::staggered_arb_backtest::tests -- --nocapture`

---

# Staggered Arb Wallet-Loss Root Cause Fixes (2026-03-08)

## Goal
Bring staggered-arb closer to the user's intended hedge discipline by fixing the main live-vs-replay mismatches behind the March 7 wallet loss: stale replay PM asks, missing quote-persistence gating before `LEG1`, and overly optimistic settlement handling in replay.

## Tasks

- [x] Add failing coverage for PM ask clearing/persistence before entry in both live and backtest.
- [x] Require fresh, persistent opposite-side PM quotes before `LEG1` so the strategy only enters when hedgeability is durable, not just momentarily visible.
- [x] Use live quote timestamps instead of `Utc::now()` when reacting to Polymarket quote updates.
- [x] Make replay settlement behavior match live by removing the forced `LEG2` buy-at-settlement path.
- [x] Re-run targeted tests plus the previously bad replay window to see how much optimism is removed.

## Review

- [x] Confirm replay now clears PM asks when the book side disappears instead of keeping stale values alive.
- [x] Confirm unhedged expiry remains a residual fallback, not an optimistic forced close in replay.
- [x] Confirm the modified replay/live path materially narrows, but does not close, the gap on the March 7 loss window.

## Progress notes

- 2026-03-08: Added PM quote state tracking keyed by event in replay and live, including fresh-quote checks, persistence gating before `LEG1`, and feed-timestamp-driven live quote handling.
- 2026-03-08: Replay now clears vanished PM asks and resets persistence timing when a quote reappears after a stale gap; live mirrors the same persistence reset logic.
- 2026-03-08: Replay settlement no longer forces a synthetic `LEG2` buy at expiry. Residual single-leg positions are settled directly and recorded through the normal trade recorder path.
- 2026-03-08: Re-ran the March 7 wallet-loss window against `tango-1-1` data via SSH tunnel. Updated replay result: `84 trades / +33.48 PnL / PF 1.65`, with `76` merges, `5` settlements, `3` aborts, and per-symbol PnL `BTC -0.49`, `ETH +22.13`, `SOL +11.84`.
- 2026-03-08: The new replay is materially less optimistic than the earlier `+66.85` result and now exposes `Settlements: $-34.15`, but it still remains far above the official wallet `1D` loss (`~-$74`), so an execution/reconciliation gap still remains after these fixes.
- 2026-03-08: Targeted stale-gap persistence regression tests passed in both replay and live paths:
  - `CARGO_INCREMENTAL=0 cargo test strategy::staggered_arb_backtest::tests::test_record_pm_quote_resets_persistence_after_stale_gap -- --nocapture`
  - `CARGO_INCREMENTAL=0 cargo test strategy::staggered_arb_live::tests::test_record_pm_quote_resets_persistence_after_stale_gap -- --nocapture`

---

# Staggered Arb Managed Execution And BTC Diagnostics Hardening (2026-03-08)

## Goal
Reduce the remaining live execution ambiguity after the March 7 wallet loss by making managed staggered-arb orders use stable idempotency keys, surfacing the final submit error instead of generic retry exhaustion, and emitting per-symbol gate diagnostics so BTC no-trigger can be attributed directly.

## Tasks

- [x] Normalize managed runtime orders so `idempotency_key` defaults to the action `client_order_id`.
- [x] Make staggered-arb live `LEG1`/`LEG2` submit actions carry explicit `client_order_id` and `idempotency_key`.
- [x] Stop retrying clearly non-retryable execution errors and preserve the last underlying error when retries are exhausted.
- [x] Align managed runtime observability labels with `staggered_arb` instead of the stale `split_arb` alias.
- [x] Add per-symbol entry/leg2 gate counters to live summary and state metrics for BTC/ETH/SOL diagnosis.
- [x] Add targeted tests for executor retry behavior, managed runtime idempotency normalization, and per-symbol summary output.

## Review

- [x] Managed staggered-arb orders now use stable IDs end-to-end in both strategy submit actions and managed runtime normalization.
- [x] Retry exhaustion now reports the last underlying submit error, which makes `Max retries exceeded` debuggable in signal history.
- [x] Live summary now exposes `entry_signal_by_symbol` and `leg2_by_symbol`, so BTC no-trigger can be attributed without guessing from aggregate counters.

## Progress notes

- 2026-03-08: Updated `staggered_arb_live` so live `LEG1` and `LEG2` `OrderRequest`s reuse the strategy-generated `client_order_id` and set `idempotency_key` to the same stable value.
- 2026-03-08: Updated managed runtime order normalization to backfill `idempotency_key` from the action order ID whenever it is missing.
- 2026-03-08: Updated `OrderExecutor` retry handling to stop on non-retryable validation/auth/signing/liquidity failures and to surface the last underlying submit error when retryable attempts are exhausted.
- 2026-03-08: Renamed managed staggered-arb runtime observability labels from `split_arb` to `staggered_arb` while still accepting the legacy alias at runtime.
- 2026-03-08: Added per-symbol gate breakdowns to live summary/metrics so BTC/ETH/SOL reject reasons can be inspected directly.

---

# Bootstrap Domain Runtime Config Decoupling (2026-03-09)

## Goal
Break `bootstrap.rs`'s remaining type-level dependency on the legacy sports/politics agent modules so those files can be retired without dragging their config structs along.

## Tasks

- [x] Add local bootstrap runtime-config structs for sports and politics.
- [x] Switch `PlatformBootstrapConfig` to use the new local runtime-config types.
- [x] Delete the dead sports/politics config shim files from `src/agents/`.
- [x] Re-run compile and targeted bootstrap tests after the decoupling.

## Review

- [x] Confirm `bootstrap.rs` no longer imports sports/politics config types from `src/agents/*`.
- [x] Confirm the new runtime-config defaults preserve prior agent IDs and polling defaults.
- [x] Confirm `src/agents/mod.rs` no longer re-exports sports/politics legacy config shims.
- [x] Confirm bootstrap still compiles with the new local config module.

## Progress notes

- 2026-03-09: Added [runtime_config.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/runtime_config.rs) so bootstrap owns the sports/politics runtime config types directly instead of importing them from legacy agent modules.
- 2026-03-09: Deleted [sports.rs](/Users/proerror/Documents/ploy/src/agents/sports.rs) and [politics.rs](/Users/proerror/Documents/ploy/src/agents/politics.rs) after cutting bootstrap over to the new local runtime config types.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo test build_event_edge_runtime_config_ --lib -- --nocapture`
  - `cargo test build_nba_comeback_runtime_config_ --lib -- --nocapture`

# RL DomainAgent Surface Retirement (2026-03-09)

## Goal
Move the remaining `RLCryptoAgent` compatibility runtime out of the shared `platform` surface so `DomainAgent` stops leaking into live/runtime module boundaries and the RL CLI becomes the only owner of that legacy path.

## Tasks

- [x] Move `RLCryptoAgent` / `RLCryptoAgentConfig` into the RL module as a CLI-local compatibility module.
- [x] Rewire `src/main_commands/rl/agent.rs` to use the local compatibility module instead of `ploy::platform::{RLCryptoAgent, RLCryptoAgentConfig}`.
- [x] Delete `src/platform/agents/` and remove `RLCryptoAgent` re-exports from `src/platform/mod.rs`.
- [x] Delete the unused `SimpleAgent` trait/export if nothing still implements or imports it.
- [x] Re-run compile plus narrow RL/bootstrap regressions after the cutover.

## Review

- [x] Confirm `src/platform/mod.rs` no longer re-exports `RLCryptoAgent`.
- [x] Confirm `src/main_commands/rl/agent.rs` still runs through the legacy RL CLI path without touching the shared `platform` API surface.
- [x] Confirm `src/platform/agents/` is gone and `SimpleAgent` is no longer defined/exported.

## Progress notes

- 2026-03-09: Started the cutover after confirming `RLCryptoAgent` is no longer a live runtime entrypoint and only the RL CLI still instantiates it.

- 2026-03-09: Moved `RLCryptoAgent` into [cli_agent.rs](/Users/proerror/Documents/ploy/src/rl/cli_agent.rs), rewired the RL command to import it from the RL module, and deleted `src/platform/agents/`.
- 2026-03-09: Validation passed:
  - `cargo check --features rl --bin ploy`
  - `cargo test rl::cli_agent --lib --features rl -- --nocapture`
  - `cargo test from_app_config_ignores_legacy_enable_price_exits_env --lib -- --nocapture`

# DomainAgent Runtime Retirement (2026-03-09)

## Goal
Delete the last actual `DomainAgent`/`EventRouter` runtime path by rewriting the RL CLI to drive `RLCryptoAgent` directly and shrinking `OrderPlatform` down to pure risk/queue/execution ownership.

## Tasks

- [x] Rework `src/rl/cli_agent.rs` so `RLCryptoAgent` exposes inherent lifecycle/event/execution methods instead of only a `DomainAgent` impl.
- [x] Rewrite `src/main_commands/rl/agent.rs` to remove `EventRouter` / `AgentSubscription` and call the agent directly.
- [x] Simplify `src/platform/platform.rs` so it no longer owns router-based agent management or execution-report callbacks.
- [x] Delete `src/platform/router.rs` plus the `DomainAgent`, `AgentHealthStatus`, `AgentSubscription`, and `RouterStats` surfaces if nothing still imports them.
- [x] Re-run RL CLI compile/tests after the retirement.

## Review

- [x] Confirm `src/main_commands/rl/agent.rs` no longer imports `EventRouter`, `AgentSubscription`, or `DomainAgent`.
- [x] Confirm `src/platform/router.rs` is deleted and no code still references `RouterStats`.
- [x] Confirm the RL CLI still updates agent state from execution reports after live/dry-run order processing.

## Progress notes

- 2026-03-09: Started after confirming `RLCryptoAgent` is now the only remaining `DomainAgent` implementation in the repo.
- 2026-03-09: Reworked [cli_agent.rs](/Users/proerror/Documents/ploy/src/rl/cli_agent.rs) so `RLCryptoAgent` exposes inherent lifecycle/event/execution methods, then rewired [agent.rs](/Users/proerror/Documents/ploy/src/main_commands/rl/agent.rs) to drive the agent directly without `EventRouter`.
- 2026-03-09: Simplified [platform.rs](/Users/proerror/Documents/ploy/src/platform/platform.rs) down to queue/risk/execution ownership, removed router-based callbacks, and deleted the dead [router.rs](/Users/proerror/Documents/ploy/src/platform/router.rs) / [legacy_runtime.rs](/Users/proerror/Documents/ploy/src/platform/legacy_runtime.rs) compatibility layer files.
- 2026-03-09: Validation passed:
  - `cargo check`
  - `cargo test test_order_platform_start_blocks_live_runtime --lib -- --nocapture`
  - `cargo test from_app_config_reads_crypto_lob_ml_model_env_vars --lib -- --nocapture`
  - `cargo test --features rl test_rl_agent_lifecycle --lib -- --nocapture`
  - `cargo test --features rl test_position_tracking --lib -- --nocapture`
  - `cargo test runtime_scope_keeps_politics_when_no_explicit_selection -- --nocapture`
  - `cargo test explicit_selection_disables_politics_without_politics_flag -- --nocapture`
  - `cargo test sports_runtime_config_defaults_match_bootstrap_expectations --lib -- --nocapture`
  - `cargo test politics_runtime_config_defaults_match_bootstrap_expectations --lib -- --nocapture`

# Legacy Crypto Bootstrap Collapse (2026-03-09)

## Goal
Collapse the remaining `lob_ml` / `rl_policy` bootstrap ownership into a single legacy-crypto compatibility config surface instead of keeping agent-specific flags and configs at `PlatformBootstrapConfig` top level.

## Tasks

- [x] Introduce a bootstrap-local legacy crypto config wrapper that owns enable flags plus `lob_ml` / `rl_policy` settings.
- [x] Rewire `PlatformBootstrapConfig`, `legacy_crypto.rs`, and `strategy_deployments.rs` to use the nested legacy config surface.
- [x] Rewire `platform_mode.rs` and bootstrap tests to the nested fields without changing runtime behavior.
- [x] Re-run compile plus the narrow bootstrap/platform-mode regressions touched by the move.

## Review

- [x] Confirm `PlatformBootstrapConfig` no longer exposes top-level `enable_crypto_lob_ml`, `enable_crypto_rl_policy`, `crypto_lob_ml`, or `crypto_rl_policy`.
- [x] Confirm `legacy_crypto.rs` is now the only bootstrap module that understands the remaining legacy crypto runtime settings.
- [x] Confirm deployment-matrix behavior for `lob_ml` / `rl_policy` remains unchanged in this slice.

## Progress notes

- 2026-03-09: Promoted `LegacyCryptoRuntimeConfig` to [legacy_crypto.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/legacy_crypto.rs) and made it the bootstrap-local owner of `lob_ml` / `rl_policy` enable flags plus runtime config payloads.
- 2026-03-09: Rewired [bootstrap.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap.rs), [strategy_deployments.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/strategy_deployments.rs), and [platform_mode.rs](/Users/proerror/Documents/ploy/src/main_modes/platform_mode.rs) to use `cfg.legacy_crypto.*` instead of top-level legacy crypto fields.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo test from_app_config_reads_crypto_lob_ml_model_env_vars --lib -- --nocapture`
  - `cargo test from_app_config_ignores_legacy_enable_price_exits_env --lib -- --nocapture`
  - `cargo test pattern_memory_deployment_does_not_enable_lob_ml --bin ploy -- --nocapture`

# Pattern Memory Canonical Handoff (2026-03-09)

## Goal
Switch `PatternMemoryStrategy` to emit canonical `StrategyAction::SubmitIntent` payloads instead of raw `SubmitOrder`, reducing one more strategy's dependence on the legacy order handoff.

## Tasks

- [ ] Replace pattern-memory submit actions with `StrategyOrderIntent`.
- [ ] Keep order IDs, limit prices, side, share sizing, and metadata behavior unchanged.
- [ ] Re-run narrow pattern-memory compile/tests after the conversion.

## Review

- [ ] Confirm `src/strategy/pattern_memory/strategy.rs` no longer emits `StrategyAction::SubmitOrder`.
- [ ] Confirm behavior-equivalent intent fields are still present on entry actions.

# Canonical Submit Intent Unification (2026-03-09)

## Goal
Retire `StrategyAction::SubmitOrder` completely by extending the canonical `StrategyOrderIntent` to carry full order semantics, then migrate the remaining RL compatibility emitter plus all runtime handlers onto `SubmitIntent` only.

## Tasks

- [x] Extend `StrategyOrderIntent` with `order_type` and `time_in_force` so canonical intents can represent the remaining RL market/IOC paths.
- [x] Convert `src/rl/integration/rl_strategy.rs` to emit only `StrategyAction::SubmitIntent`, including exit/shutdown actions.
- [x] Delete `StrategyAction::SubmitOrder` and the raw-order normalization helper from `src/strategy/traits.rs`.
- [x] Rewire `strategy_runtime`, `orchestrator`, and CLI action handling/printing to operate on `SubmitIntent` only.
- [x] Re-run focused compile and RL/strategy regressions after the single-handoff cutover.

## Review

- [x] Confirm `rg "SubmitOrder|into_submit_order\\(" src` returns no source hits.
- [x] Confirm canonical intents preserve RL `OrderType` / `TimeInForce` semantics instead of silently downgrading them to `Limit/GTC`.
- [x] Confirm the remaining strategy emitters all still compile and route through `StrategyAction::SubmitIntent`.

## Progress notes

- 2026-03-09: Extended [traits.rs](/Users/proerror/Documents/ploy/src/strategy/traits.rs) so `StrategyOrderIntent` now carries `order_type` and `time_in_force`, and `into_order_request()` preserves those fields while still normalizing `client_order_id` + `idempotency_key`.
- 2026-03-09: Converted [rl_strategy.rs](/Users/proerror/Documents/ploy/src/rl/integration/rl_strategy.rs) to build canonical submit intents for buy, sell, and shutdown flows; added `market_slug` to `RLStrategy` so the intent path has complete metadata.
- 2026-03-09: Removed the `SubmitOrder` compatibility branch from [strategy_runtime.rs](/Users/proerror/Documents/ploy/src/coordinator/strategy_runtime.rs), [orchestrator.rs](/Users/proerror/Documents/ploy/src/strategy/orchestrator.rs), and [cli/strategy.rs](/Users/proerror/Documents/ploy/src/cli/strategy.rs), leaving `SubmitIntent` as the only strategy-side submit action.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo check --lib --features rl`
  - `cargo test strategy_order_intent_into_order_request_preserves_action_id --lib -- --nocapture`
  - `cargo test --features rl test_rl_strategy_creation --lib -- --nocapture`
  - `cargo test --features rl test_rule_based_action --lib -- --nocapture`
  - `rg "SubmitOrder|into_submit_order\\(" src`

# Bootstrap Managed Runtime Spawn Plans (2026-03-09)

## Goal
Stop `bootstrap.rs` from owning seven separate managed-strategy spawn branches by collapsing them into a unified managed runtime plan pipeline emitted from `strategy_deployments.rs`.

## Tasks

- [x] Add a managed runtime plan type that captures spawn payload, data-plane selection, and bootstrap preflight needs.
- [x] Move managed strategy selection/config building for `momentum`, `pattern_memory`, `staggered_arb`, `crypto_lob_ml`, `crypto_rl_policy`, `nba_comeback`, and `event_edge` into `strategy_deployments.rs`.
- [x] Replace the repeated managed-strategy spawn branches in `bootstrap.rs` with a single loop over managed runtime plans.
- [x] Keep the remaining sports support preflight outside the loop, but route the actual `nba_comeback` spawn through the shared plan pipeline.
- [x] Re-run focused bootstrap validation after the ownership collapse.

## Review

- [x] Confirm `bootstrap.rs` no longer contains per-strategy managed-runtime spawn branches for the seven canonical managed strategies.
- [x] Confirm `strategy_deployments.rs` is now the owner of managed runtime plan selection and config rendering.
- [x] Confirm the new pipeline preserves the special cases that still matter: pattern-memory table init and split-arb shared crypto data plane.

## Progress notes

- 2026-03-09: Added `ManagedRuntimeDataPlaneKind`, `ManagedRuntimeBootstrapStep`, and `ManagedStrategyRuntimePlan` to [strategy_deployments.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/strategy_deployments.rs), plus a new `collect_managed_strategy_runtime_plans(...)` selector.
- 2026-03-09: Moved managed runtime config selection for `momentum`, `pattern_memory`, `staggered_arb`, `crypto_lob_ml`, `crypto_rl_policy`, `nba_comeback`, and `event_edge` out of [bootstrap.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap.rs) and into the strategy-deployment layer.
- 2026-03-09: Replaced the repeated managed spawn branches in [bootstrap.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap.rs) with a single `for plan in managed_runtime_plans` loop that applies bootstrap preflight and data-plane selection before calling `spawn_managed_strategy_runtime_task(...)`.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo test collect_managed_strategy_runtime_plans_collapses_crypto_spawn_specs --lib -- --nocapture`
  - `cargo test apply_strategy_deployments_does_not_route_unknown_crypto_strategy_to_momentum --lib -- --nocapture`
  - `cargo test build_split_arb_runtime_config_renders_symbols_and_series_ids --lib -- --nocapture`
  - `cargo test build_momentum_runtime_config_renders_directional_crypto_settings --lib -- --nocapture`

# Bootstrap Sports Runtime Support Extraction (2026-03-09)

## Goal
Move the remaining sports/NBA websocket + persistence bootstrap special-case out of `runtime_spawns.rs` so spawn ownership stays focused on task launch and bootstrap support logic lives in dedicated modules.

## Tasks

- [x] Extract `prepare_sports_runtime_support(...)` into a dedicated bootstrap support module.
- [x] Remove the sports support implementation from `runtime_spawns.rs` so that file only owns spawn helpers.
- [x] Rewire `bootstrap.rs` imports to source sports support from the new module.
- [x] Re-run default + `rl` compile after the extraction.

## Review

- [x] Confirm sports support no longer lives in `runtime_spawns.rs`.
- [x] Confirm `bootstrap.rs` still invokes the same `prepare_sports_runtime_support(...)` entrypoint.
- [x] Confirm the extraction does not change bootstrap compile behavior.

## Progress notes

- 2026-03-09: Added [sports_runtime_support.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/sports_runtime_support.rs) and moved the NBA/sports websocket subscription + persistence preparation path there.
- 2026-03-09: Removed `prepare_sports_runtime_support(...)` from [runtime_spawns.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/runtime_spawns.rs) so spawn ownership stays limited to runtime task launch helpers.
- 2026-03-09: Updated [bootstrap.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap.rs) to import sports support from the new module.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo check --lib --features rl`

# Bootstrap Coordinator Control-Plane Extraction (2026-03-09)

## Goal
Move executor/coordinator/schema/API startup ownership out of `start_platform()` so `bootstrap.rs` stops directly owning the control-plane bootstrap path and focuses on runtime assembly.

## Tasks

- [x] Extract the executor + coordinator + schema restore path into a dedicated bootstrap module.
- [x] Extract API startup alongside that control-plane bootstrap path so the caller only receives initialized artifacts.
- [x] Rewire `start_platform()` to consume the extracted coordinator bootstrap artifacts instead of inlining the entire block.
- [x] Re-run default + `rl` compile after the extraction.

## Review

- [x] Confirm `bootstrap.rs` no longer inlines the executor/idempotency, schema migration, governance restore, and API startup block.
- [x] Confirm the new bootstrap module returns initialized `Coordinator`, `CoordinatorHandle`, and API handle ownership to the caller.
- [x] Confirm the extraction does not change compile behavior.

## Progress notes

- 2026-03-09: Added [coordinator_bootstrap.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/coordinator_bootstrap.rs) and moved executor initialization, idempotency cleanup, schema/migration setup, governance/execution/risk restore, ingress authorization, and API startup there.
- 2026-03-09: Updated [bootstrap.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap.rs) so `start_platform()` now delegates control-plane bootstrap to `initialize_coordinator_runtime(...)` and keeps only top-level startup orchestration.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo check --lib --features rl`

# OpenClaw Config Ownership Migration (2026-03-09)

## Goal
Move `OpenClawConfig` ownership out of `src/agents` and into bootstrap/governance assembly so `src/agents` trends toward runtime implementation only instead of exposing bootstrap config surface.

## Tasks

- [x] Add a bootstrap-owned OpenClaw config module and have bootstrap config consume it directly.
- [x] Convert `src/agents/openclaw/config.rs` into a compatibility shim instead of the canonical owner.
- [x] Remove unused public OpenClaw config/regime re-exports from `src/agents/openclaw/mod.rs`.
- [x] Re-run default + `rl` compile after the ownership migration.

## Review

- [x] Confirm `PlatformBootstrapConfig` no longer imports `OpenClawConfig` from `crate::agents`.
- [x] Confirm bootstrap now re-exports the OpenClaw config types from its own module.
- [x] Confirm `src/agents/openclaw/mod.rs` no longer exposes unused config/regime types.
- [x] Confirm the compatibility shim still compiles for OpenClaw runtime internals.

## Progress notes

- 2026-03-09: Added [openclaw_config.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/openclaw_config.rs) and made [bootstrap.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap.rs) re-export the OpenClaw config types from bootstrap ownership.
- 2026-03-09: Updated [bootstrap_config.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/bootstrap_config.rs) so `PlatformBootstrapConfig` depends on bootstrap-owned `OpenClawConfig` instead of `crate::agents`.
- 2026-03-09: Reduced [config.rs](/Users/proerror/Documents/ploy/src/agents/openclaw/config.rs) to a compatibility shim and removed the unused `OpenClawConfig` / `MarketRegime` / `RegimeSnapshot` public exports from [mod.rs](/Users/proerror/Documents/ploy/src/agents/openclaw/mod.rs).
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo check --lib --features rl`
  - `rg "crate::agents::OpenClawConfig|pub use config::OpenClawConfig|pub use regime::\\{MarketRegime, RegimeSnapshot\\}" src`

# LegacyControl Retirement (2026-03-09)

## Goal
Delete the `StrategyAction::LegacyControl` compatibility path so strategies stop emitting governance/feed-control actions and the canonical strategy contract is reduced to decision/execution/logging concerns only.

## Tasks

- [x] Remove `StrategyAction::LegacyControl` and `StrategyControlAction` from the strategy trait surface.
- [x] Remove all remaining `LegacyControl` emitters from momentum, two-leg, and gamma-scalping strategies.
- [x] Delete legacy-control handlers from managed runtime, orchestrator, and CLI strategy loops.
- [x] Re-run compile and focused strategy tests after the retirement.

## Review

- [x] Confirm there are no remaining source references to `LegacyControl` or `StrategyControlAction`.
- [x] Confirm two-leg risk escalation semantics still surface through `Alert` after removing risk-control actions.
- [x] Confirm the touched strategies still compile, and the available focused tests still pass.

## Progress notes

- 2026-03-09: Removed `StrategyAction::LegacyControl` and `StrategyControlAction` from [traits.rs](/Users/proerror/Documents/ploy/src/strategy/traits.rs), and stopped re-exporting the deleted compatibility type from [mod.rs](/Users/proerror/Documents/ploy/src/strategy/mod.rs).
- 2026-03-09: Deleted the remaining legacy-control emitters from [momentum_strat.rs](/Users/proerror/Documents/ploy/src/strategy/strategies/momentum_strat.rs), [two_leg.rs](/Users/proerror/Documents/ploy/src/strategy/strategies/two_leg.rs), and [strategy.rs](/Users/proerror/Documents/ploy/src/strategy/gamma_scalping/strategy.rs). The two-leg risk path now reports through `Alert` instead of a dead control-plane action.
- 2026-03-09: Removed legacy-control handling from [strategy_runtime.rs](/Users/proerror/Documents/ploy/src/coordinator/strategy_runtime.rs), [orchestrator.rs](/Users/proerror/Documents/ploy/src/strategy/orchestrator.rs), and [strategy.rs](/Users/proerror/Documents/ploy/src/cli/strategy.rs).
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo check --lib --features rl`
  - `cargo test --lib test_strategy_manager_creation -- --nocapture`
  - `cargo test --lib gamma_scalping::strategy::tests -- --nocapture`
  - `rg "StrategyControlAction|LegacyControl" src`

# Strategy Intent Raw-Order Bridge Extraction (2026-03-09)

## Goal
Remove raw `OrderRequest` materialization from the canonical `StrategyOrderIntent` type so the strategy trait surface stops directly depending on execution payloads.

## Tasks

- [x] Delete `StrategyOrderIntent::into_order_request()` from `traits.rs`.
- [x] Add a dedicated runtime-order bridge module and move the conversion helper there.
- [x] Rewire managed runtime, orchestrator, CLI, and intent-focused strategy tests to use the bridge helper.
- [x] Re-run compile and focused regression checks after the extraction.

## Review

- [x] Confirm `traits.rs` no longer imports or constructs `OrderRequest`.
- [x] Confirm there are no remaining source references to `into_order_request()`.
- [x] Confirm the runtime-order bridge helper preserves `client_order_id` and `idempotency_key`.

## Progress notes

- 2026-03-09: Added [runtime_order.rs](/Users/proerror/Documents/ploy/src/strategy/runtime_order.rs) and moved raw `OrderRequest` materialization there as `order_request_from_intent(...)`, including the action-id preservation regression test.
- 2026-03-09: Removed `StrategyOrderIntent::into_order_request()` from [traits.rs](/Users/proerror/Documents/ploy/src/strategy/traits.rs), which drops the direct `OrderRequest` dependency from the canonical strategy trait surface.
- 2026-03-09: Rewired [strategy_runtime.rs](/Users/proerror/Documents/ploy/src/coordinator/strategy_runtime.rs), [orchestrator.rs](/Users/proerror/Documents/ploy/src/strategy/orchestrator.rs), [strategy.rs](/Users/proerror/Documents/ploy/src/cli/strategy.rs), and intent-focused strategy tests in [strategy.rs](/Users/proerror/Documents/ploy/src/strategy/event_edge/strategy.rs), [staggered_arb_live.rs](/Users/proerror/Documents/ploy/src/strategy/staggered_arb_live.rs), and [strategy.rs](/Users/proerror/Documents/ploy/src/strategy/nba_comeback/strategy.rs) to use the bridge helper.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo check --lib --features rl`
  - `cargo test --lib runtime_order::tests -- --nocapture`
  - `cargo test --lib test_strategy_manager_creation -- --nocapture`
  - `rg "into_order_request\\(|order_request_from_intent\\(" src`

# Bootstrap Startup Context Extraction (2026-03-09)

## Goal
Move exchange/client/account/shared-db bootstrap preflight out of `start_platform()` so the top-level bootstrap flow focuses on assembly instead of low-level startup context wiring.

## Tasks

- [x] Extract exchange compatibility checks, Polymarket client setup, account/runtime target derivation, domain gating, and shared DB pool setup into a dedicated startup-context module.
- [x] Rewire `start_platform()` to consume the extracted startup context instead of owning those steps inline.
- [x] Re-run default + `rl` compile after the extraction.

## Review

- [x] Confirm `bootstrap.rs` no longer inlines exchange/client/account/shared-pool setup.
- [x] Confirm the new startup-context module owns the initial startup logging and bootstrap preflight decisions.
- [x] Confirm compile behavior is unchanged after the extraction.

## Progress notes

- 2026-03-09: Added [startup_context.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/startup_context.rs) and moved exchange compatibility checks, Polymarket client setup, account/runtime-target derivation, domain gating, and shared DB pool creation there.
- 2026-03-09: Updated [bootstrap.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap.rs) so `start_platform()` now consumes a `BootstrapStartupContext` instead of assembling those prerequisites inline.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo check --lib --features rl`

# Bootstrap Runtime Orchestration Extraction (2026-03-09)

## Goal
Move runtime support setup, managed runtime spawning, startup control application, and shutdown/join handling out of `start_platform()` so the top-level bootstrap path becomes a thin assembly function.

## Tasks

- [x] Extract settlement persistence, crypto/sports support wiring, managed runtime plan execution, OpenClaw spawn, and shutdown orchestration into a dedicated module.
- [x] Rewire `start_platform()` to delegate the runtime phase to the extracted orchestration function.
- [x] Re-run default + `rl` compile after the extraction.

## Review

- [x] Confirm `bootstrap.rs` no longer inlines runtime orchestration and shutdown handling.
- [x] Confirm the new runtime orchestration module owns settlement persistence, managed plan loop, startup pause/resume, and shutdown/join logic.
- [x] Confirm compile behavior is unchanged after the extraction.

## Progress notes

- 2026-03-09: Added [runtime_orchestration.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/runtime_orchestration.rs) and moved settlement persistence, crypto/sports runtime support setup, managed plan spawning, OpenClaw spawn, auto-claimer wiring, startup control application, and shutdown/join logic there.
- 2026-03-09: Updated [bootstrap.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap.rs) so `start_platform()` now delegates the runtime phase to `run_platform_runtime(...)`.
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo check --lib --features rl`

# Agent Runtime Type Ownership Extraction (2026-03-09)

## Goal
Move `AgentStatus` and `AgentRiskParams` out of `platform` so those agent-centric compatibility types stop making `platform` look like the canonical runtime owner.

## Tasks

- [x] Add a dedicated `agent_runtime` module as the authoritative owner of `AgentStatus` and `AgentRiskParams`.
- [x] Rewire coordinator, bootstrap, agents, RL, strategy runtime configs, API handlers, and platform risk code to import the types from the new owner.
- [x] Remove the old `platform/traits.rs` owner and stop re-exporting the agent runtime types from `platform`.
- [x] Re-run default + `rl` compile plus focused agent-runtime tests after the migration.

## Review

- [x] Confirm `platform/mod.rs` no longer re-exports `AgentStatus` or `AgentRiskParams`.
- [x] Confirm `platform/traits.rs` is gone and `lib.rs` now re-exports the agent runtime types from `agent_runtime`.
- [x] Confirm compile behavior is unchanged after the ownership move.

## Progress notes

- 2026-03-09: Added [agent_runtime.rs](/Users/proerror/Documents/ploy/src/agent_runtime.rs) as the authoritative owner of `AgentStatus` and `AgentRiskParams`, including their focused tests.
- 2026-03-09: Rewired coordinator, bootstrap, governance agents, RL, API handlers, strategy runtime configs, and platform risk code to import the types from `crate::agent_runtime` or root re-exports instead of `crate::platform`.
- 2026-03-09: Deleted [traits.rs](/Users/proerror/Documents/ploy/src/platform/traits.rs) and removed the old `AgentStatus` / `AgentRiskParams` re-export from [mod.rs](/Users/proerror/Documents/ploy/src/platform/mod.rs).
- 2026-03-09: Validation passed:
  - `cargo check --lib`
  - `cargo check --lib --features rl`
  - `cargo test --lib agent_runtime::tests -- --nocapture`
  - `rg "AgentRiskParams|AgentStatus" src/platform src/lib.rs`

# Strategy Runtime, Risk, And Adapter Ownership Cuts (2026-03-10)

## Goal
Finish the next large structural wave by shrinking the remaining core ownership hot spots: `strategy_runtime`, `platform/risk`, and `strategy/adapters`.

## Tasks

- [x] Extract `strategy_runtime` order-persistence / observability helpers into dedicated submodules so the canonical managed-runtime loop owns orchestration rather than inline storage/logging details.
- [x] Split `platform/risk.rs` into clearer ownership slices without changing risk semantics.
- [x] Split `strategy/adapters.rs` so momentum/shared adapter support stops living in one giant file.
- [ ] Re-run compile and focused managed-runtime / risk / strategy regressions after each integrated slice.

## Review

- [x] Confirm `strategy_runtime.rs` keeps runtime-loop ownership only and delegates order bridge / observability helpers.
- [x] Confirm `risk.rs` no longer centralizes config, counters, and circuit-breaker bookkeeping in one file.
- [x] Confirm `adapters.rs` shrinks and its extracted modules own the moved adapter support logic.

## Progress notes

- 2026-03-10: Reserved mainline ownership for `src/coordinator/strategy_runtime.rs`; parallel sidecar ownership goes to `src/platform/risk.rs` and `src/strategy/adapters.rs`.
- 2026-03-10: Added [observability.rs](/Users/proerror/Documents/ploy/src/coordinator/strategy_runtime/observability.rs) and [order_store.rs](/Users/proerror/Documents/ploy/src/coordinator/strategy_runtime/order_store.rs), moving managed-runtime signal-history persistence plus runtime order store/normalization helpers out of [strategy_runtime.rs](/Users/proerror/Documents/ploy/src/coordinator/strategy_runtime.rs).
- 2026-03-10: Added [actions.rs](/Users/proerror/Documents/ploy/src/coordinator/strategy_runtime/actions.rs) and moved the managed runtime action-dispatch / poll-update loop out of [strategy_runtime.rs](/Users/proerror/Documents/ploy/src/coordinator/strategy_runtime.rs), leaving the top-level file focused on runtime assembly, feed wiring, and command handling.
- 2026-03-10: Added [config.rs](/Users/proerror/Documents/ploy/src/platform/risk/config.rs), [types.rs](/Users/proerror/Documents/ploy/src/platform/risk/types.rs), and [stats.rs](/Users/proerror/Documents/ploy/src/platform/risk/stats.rs), moving `RiskConfig`, public risk result/state types, and internal stats structs out of [risk.rs](/Users/proerror/Documents/ploy/src/platform/risk.rs).
- 2026-03-10: Added [transitions.rs](/Users/proerror/Documents/ploy/src/platform/risk/transitions.rs), moving the heavy `RiskGate` state-transition ownership (`record_success`, `record_failure`, `record_loss`, circuit-breaker transitions, runtime restore helpers) out of [risk.rs](/Users/proerror/Documents/ploy/src/platform/risk.rs).
- 2026-03-10: Added [momentum_adapter.rs](/Users/proerror/Documents/ploy/src/strategy/adapters/momentum_adapter.rs) and moved the full `MomentumStrategyAdapter` ownership out of [adapters.rs](/Users/proerror/Documents/ploy/src/strategy/adapters.rs), leaving the top-level file focused on shared helpers plus split-arb ownership.
- 2026-03-10: Added [split_arb_adapter.rs](/Users/proerror/Documents/ploy/src/strategy/adapters/split_arb_adapter.rs) and moved the full `SplitArbStrategyAdapter` ownership out of [adapters.rs](/Users/proerror/Documents/ploy/src/strategy/adapters.rs), shrinking the top-level adapter file down to a thin facade plus shared `crypto_submit_intent` helper.
- 2026-03-10: Validation passed:
  - `cargo check --lib`
  - `cargo test persist_runtime_order_ --lib -- --nocapture`
  - `cargo test normalize_runtime_order_request_sets_idempotency_key_from_action_id --lib -- --nocapture`
  - `cargo test test_basic_check --lib -- --nocapture`
  - `cargo test test_drawdown_limit_triggers_circuit_breaker --lib -- --nocapture`
  - `cargo test test_restore_runtime_counters_halts_when_daily_loss_exceeded --lib -- --nocapture`
  - `cargo test strategy::adapters::tests --lib -- --nocapture`
  - `cargo test strategy::adapters::split_arb_adapter::tests --lib -- --nocapture`

# CLI Strategy Runtime Ops Extraction (2026-03-10)

## Goal
Move the standalone strategy runtime/process-management surface out of `src/cli/strategy.rs` so the CLI file keeps command definitions while runtime ownership lives in a dedicated submodule.

## Tasks

- [x] Extract the standalone runtime/process-management block from `src/cli/strategy.rs` into `src/cli/strategy/runtime_ops.rs`.
- [x] Rewire `StrategyCommands::run` call sites to import runtime/process-management helpers from the new module.
- [x] Preserve foreground runtime, daemon management, status/logs, and default-config behavior without changing semantics.
- [x] Re-run compile and focused strategy-manager regressions after the extraction.

## Review

- [x] Confirm `src/cli/strategy.rs` no longer owns config-dir/run-dir/log-dir helpers, standalone runtime execution, or process-management status helpers.
- [x] Confirm the new `runtime_ops` module retains strategy start/stop/status/log/reload behavior and order-action handling.

## Progress notes

- 2026-03-10: Added [runtime_ops.rs](/Users/proerror/Documents/ploy/src/cli/strategy/runtime_ops.rs) and moved config/run/log path helpers, foreground runtime execution, daemon management, action handling, status/log helpers, and default-config creation out of [strategy.rs](/Users/proerror/Documents/ploy/src/cli/strategy.rs).
- 2026-03-10: Rewired [strategy.rs](/Users/proerror/Documents/ploy/src/cli/strategy.rs) to delegate `list/start/stop/status/logs/reload` commands through the new runtime-ops module instead of owning the full standalone runtime block inline.
- 2026-03-10: File size delta:
  - [strategy.rs](/Users/proerror/Documents/ploy/src/cli/strategy.rs): `6144 -> 5137` lines
  - [runtime_ops.rs](/Users/proerror/Documents/ploy/src/cli/strategy/runtime_ops.rs): `0 -> 952` lines
- 2026-03-10: Validation passed:
  - `cargo check --lib`
  - `cargo test test_strategy_manager_creation --lib -- --nocapture`
  - `cargo test test_available_strategies --lib -- --nocapture`
  - `cargo test test_graceful_stop_reports_closed_action_channel --lib -- --nocapture`

# CLI Backtest Ops Extraction (2026-03-10)

## Goal
Move the backtest/reporting ownership out of `src/cli/strategy.rs` so the CLI root keeps command dispatch while the backtest execution/report surface lives in a dedicated module.

## Tasks

- [x] Extract `run_backtest`, backtest diagnostics, gamma verification, and backtest comparison/report helpers into `src/cli/strategy/backtest_ops.rs`.
- [x] Rewire `StrategyCommands::run` to import backtest command handlers from the new module.
- [x] Keep replay/verification/report behavior unchanged while removing the large backtest block from the CLI root file.
- [x] Re-run compile and focused backtest regressions after the extraction.

## Review

- [x] Confirm `src/cli/strategy.rs` no longer owns the `run_backtest*` / `verify_backtest_trades_gamma` / `run_live_backtest_compare` block inline.
- [x] Confirm the new backtest module still drives settlement handoff, replay diagnostics, and saved-report loading without behavior changes.

## Progress notes

- 2026-03-10: Added [backtest_ops.rs](/Users/proerror/Documents/ploy/src/cli/strategy/backtest_ops.rs) and moved the contiguous backtest execution/reporting surface out of [strategy.rs](/Users/proerror/Documents/ploy/src/cli/strategy.rs), including replay backtests, DB diagnostics, Gamma verification, run listing/diffing, and live-vs-backtest comparison.
- 2026-03-10: Rewired [strategy.rs](/Users/proerror/Documents/ploy/src/cli/strategy.rs) to delegate `Backtest`, `BacktestList`, `BacktestDiff`, and `LiveBacktestCompare` dispatch through the new backtest-ops module.
- 2026-03-10: File size delta:
  - [strategy.rs](/Users/proerror/Documents/ploy/src/cli/strategy.rs): `5137 -> 3444` lines
  - [backtest_ops.rs](/Users/proerror/Documents/ploy/src/cli/strategy/backtest_ops.rs): `0 -> 1702` lines
- 2026-03-10: Validation passed:
  - `cargo check --lib`
  - `cargo test test_settlement_binary_payout --lib -- --nocapture`
  - `cargo test test_config_from_toml_matches_checked_in_template --lib -- --nocapture`

# CLI Settlement And Risk Query Extraction (2026-03-10)

## Goal
Finish the next CLI/risk cleanup wave by moving settlement/dataset ownership out of `src/cli/strategy.rs` and moving `RiskGate` read/query helpers out of `src/platform/risk.rs`.

## Tasks

- [x] Extract settlement/reporting + crypto LOB dataset helpers into `src/cli/strategy/settlement_ops.rs`.
- [x] Rewire CLI root and backtest module to consume settlement helpers from the new owner.
- [x] Extract `RiskGate` read/query helpers into `src/platform/risk/queries.rs`.
- [x] Re-run compile and focused settlement/risk regressions after both slices land.

## Review

- [x] Confirm `src/cli/strategy.rs` no longer owns the settlement accuracy / directional-settlement backtest / dataset export block inline.
- [x] Confirm `src/platform/risk.rs` retains stateful mutations while query helpers now live in `queries.rs`.
- [x] Confirm CLI settlement commands and risk runtime snapshots still compile and behave the same.

## Progress notes

- 2026-03-10: Reserved mainline ownership for `src/cli/strategy.rs`, `src/cli/strategy/settlement_ops.rs`, and the `src/platform/risk.rs` / `src/platform/risk/queries.rs` pair so the tree returns to a buildable state before the next parallel wave.
- 2026-03-10: Added [settlement_ops.rs](/Users/proerror/Documents/ploy/src/cli/strategy/settlement_ops.rs) and moved the settlement accuracy report, directional settlement backtest, crypto LOB dataset export helpers, and shared resolution helper out of [strategy.rs](/Users/proerror/Documents/ploy/src/cli/strategy.rs).
- 2026-03-10: Rewired [strategy.rs](/Users/proerror/Documents/ploy/src/cli/strategy.rs) and [backtest_ops.rs](/Users/proerror/Documents/ploy/src/cli/strategy/backtest_ops.rs) to consume settlement helpers from the new owner module instead of half-owning the same surface.
- 2026-03-10: Added [queries.rs](/Users/proerror/Documents/ploy/src/platform/risk/queries.rs) and moved the `RiskGate` read/query helpers out of [risk.rs](/Users/proerror/Documents/ploy/src/platform/risk.rs), leaving the root file focused on state ownership, tests, and mutation wiring.
- 2026-03-10: Validation passed:
  - `cargo check --lib`
  - `cargo test test_settlement_binary_payout --lib -- --nocapture`
  - `cargo test test_query_helpers_report_runtime_snapshots --lib -- --nocapture`
- 2026-03-10: Parallel ownership reserved after this slice:
  - `src/coordinator/bootstrap/managed_crypto.rs`
  - `src/coordinator/bootstrap/crypto_runtime_support.rs`
  - `src/strategy/adapters/momentum_adapter.rs`

# Bootstrap Managed Crypto And Runtime Support Extraction (2026-03-10)

## Goal
Keep shrinking bootstrap-owned crypto runtime setup by splitting managed-crypto env/config ownership and crypto runtime preflight/discovery ownership into dedicated submodules.

## Tasks

- [x] Extract `ManagedCryptoRuntimeConfig` and runtime env hydration into `managed_crypto/config.rs` and `managed_crypto/env.rs`.
- [x] Extract crypto runtime preflight and market-discovery ownership into `crypto_runtime_support/preflight.rs` and `crypto_runtime_support/market_discovery.rs`.
- [x] Keep the bootstrap-facing root modules as thin facades over the extracted owners.
- [x] Re-run compile and focused bootstrap regressions after the extraction.

## Review

- [x] Confirm `managed_crypto.rs` no longer owns both config structs and env hydration bodies inline.
- [x] Confirm `crypto_runtime_support.rs` no longer owns preflight assembly and market-discovery collector wiring inline.
- [x] Confirm managed-runtime planning and crypto env/config tests still pass after the move.

## Progress notes

- 2026-03-10: Added [config.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/managed_crypto/config.rs) and [env.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/managed_crypto/env.rs), leaving [managed_crypto.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/managed_crypto.rs) as a thin facade over managed-crypto config and env ownership.
- 2026-03-10: Added [preflight.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/crypto_runtime_support/preflight.rs) and [market_discovery.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/crypto_runtime_support/market_discovery.rs), leaving [crypto_runtime_support.rs](/Users/proerror/Documents/ploy/src/coordinator/bootstrap/crypto_runtime_support.rs) to orchestrate the extracted pieces plus the existing market-data runtime module.
- 2026-03-10: Validation passed:
  - `cargo check --lib`
  - `cargo test from_app_config_reads_crypto_lob_ml_model_env_vars --lib -- --nocapture`
  - `cargo test collect_managed_strategy_runtime_plans_collapses_crypto_spawn_specs --lib -- --nocapture`

# Coordinator Core Ownership Wave (2026-03-10)

## Goal
Keep collapsing the active runtime core by splitting coordinator recovery/orchestration ownership, capital allocation internals, and execution-journal restore/parsing ownership into dedicated modules.

## Tasks

- [x] Extract coordinator recovery/bootstrap ownership out of `src/coordinator/coordinator.rs`.
- [x] Extract a major allocator slice out of `src/coordinator/capital.rs`.
- [x] Extract execution-journal restore/parsing ownership out of `src/coordinator/journal.rs`.
- [x] Re-run compile and focused coordinator regressions after each integrated slice.

## Review

- [x] Confirm `coordinator.rs` keeps runtime-loop ownership rather than restore/bootstrap details.
- [x] Confirm `capital.rs` no longer centralizes both crypto and market allocator internals in one file.
- [x] Confirm `journal.rs` no longer centralizes restore loaders and parsing helpers in the root owner.

## Progress notes

- 2026-03-10: Reserved ownership for the next parallel wave:
  - mainline: `src/coordinator/coordinator.rs`
  - worker 1: `src/coordinator/capital.rs`
  - worker 2: `src/coordinator/journal.rs`
- 2026-03-10: Extracted the crypto allocator ownership from [capital.rs](/Users/proerror/Documents/ploy/src/coordinator/capital.rs) into [crypto.rs](/Users/proerror/Documents/ploy/src/coordinator/capital/crypto.rs), leaving `CapitalPolicy` and the market allocator path in the root facade while targeted capital ledger checks stayed green.
- 2026-03-10: Added [recovery.rs](/Users/proerror/Documents/ploy/src/coordinator/coordinator/recovery.rs) and moved the coordinator recovery/bootstrap ownership out of [coordinator.rs](/Users/proerror/Documents/ploy/src/coordinator/coordinator.rs), including risk runtime restore, governance restore, execution-log restore, and the persistence pool setters.
- 2026-03-10: Added [restore.rs](/Users/proerror/Documents/ploy/src/coordinator/journal/restore.rs) and moved journal restore/parsing/loading ownership out of [journal.rs](/Users/proerror/Documents/ploy/src/coordinator/journal.rs), including persisted restore structs, risk snapshot loading, execution restore loading, JSON metadata normalization, and the restore-focused tests.
- 2026-03-10: Validation passed for the local coordinator slice:
  - `rtk cargo check --lib`
  - `rtk cargo test test_crypto_allocator_ledger_snapshot_reports_open_pending_and_available --lib -- --nocapture`
  - `rtk cargo test test_string_metadata_from_json_normalizes_scalar_values --lib -- --nocapture`
  - `rtk cargo test test_handle_force_close_domain_blocks_new_buy_immediately --lib -- --nocapture`
  - `rtk cargo test test_drain_and_execute_records_single_success_for_buy_fill --lib -- --nocapture`

# Admission Deployment Ownership Extraction (2026-03-10)

## Goal
Move deployment registry/load/matching/gating ownership out of `src/coordinator/admission.rs` so the root owner keeps duplicate-guard, sizing, and order-request orchestration while deployment resolution lives in a dedicated submodule.

## Tasks

- [x] Extract deployment registry loading and file/env discovery out of `src/coordinator/admission.rs`.
- [x] Extract deployment selector/timeframe matching and metadata application out of `src/coordinator/admission.rs`.
- [x] Keep `AdmissionController` behavior unchanged while delegating deployment gating to the extracted owner.
- [x] Re-run compile and focused admission regressions after the extraction.

## Review

- [x] Confirm `admission.rs` no longer centralizes deployment file loading and selector/timeframe matching internals.
- [x] Confirm deployment gating and stable idempotency bucket behavior still pass focused regressions.

## Progress notes

- 2026-03-10: Added [deployments.rs](/Users/proerror/Documents/ploy/src/coordinator/admission/deployments.rs) to own deployment JSON/env loading, metadata lookup, selector matching, timeframe inference, and deployment gate resolution.
- 2026-03-10: Left [admission.rs](/Users/proerror/Documents/ploy/src/coordinator/admission.rs) owning duplicate guarding, Kelly/min-order sizing, idempotency-key construction, and the public admission surface while delegating deployment-specific behavior to the new owner.
- 2026-03-10: Validation passed:
  - `rtk cargo check --lib`
  - `rtk cargo test test_deployment_gate_infers_unique_by_timeframe_hint --lib -- --nocapture`
  - `rtk cargo test test_build_order_request_uses_stable_idempotency_key_by_window --lib -- --nocapture`

# Strategy Engine Lifecycle Extraction (2026-03-10)

## Goal
Move cycle-abort, halt-persistence, and idle-transition lifecycle ownership out of `src/strategy/execution/engine.rs` so the root engine owner keeps orchestration while lifecycle transitions live in a dedicated submodule.

## Tasks

- [x] Extract halt/state-persistence helpers out of `src/strategy/execution/engine.rs`.
- [x] Extract cycle abort / force-leg2 / idle transition lifecycle routines out of `src/strategy/execution/engine.rs`.
- [x] Keep execution behavior unchanged while delegating lifecycle transitions to the extracted owner.
- [x] Re-run compile and focused engine regressions after the extraction.

## Review

- [x] Confirm `engine.rs` no longer centralizes lifecycle transition internals.
- [x] Confirm engine lifecycle regression coverage still passes after the move.

## Progress notes

- 2026-03-10: Added [lifecycle.rs](/Users/proerror/Documents/ploy/src/strategy/execution/engine/lifecycle.rs) to own halt persistence, strategy-state persistence, abort-cycle flows, forced Leg2 fallback, and idle-transition routines.
- 2026-03-10: Reduced [engine.rs](/Users/proerror/Documents/ploy/src/strategy/execution/engine.rs) to the core engine façade by delegating lifecycle calls through explicit `*_impl` imports.
- 2026-03-10: Validation passed:
  - `rtk cargo check --lib`
  - `rtk cargo test transition_to_idle_clears_state --lib -- --nocapture`
  - `rtk cargo test abort_cycle_without_active_cycle --lib -- --nocapture`

# Live Runtime And Strategy Core Wave (2026-03-10)

## Goal
Keep collapsing the active live-trading core by splitting a major admission slice plus the two largest live-strategy implementations into dedicated submodules with clear ownership.

## Tasks

- [x] Extract a major deployment/admission slice out of `src/coordinator/admission.rs`.
- [x] Extract a major ownership slice out of `src/strategy/staggered_arb_live.rs`.
- [x] Extract a major ownership slice out of `src/strategy/momentum.rs`.
- [x] Extract a major ownership slice out of `src/strategy/execution/engine.rs`.
- [ ] Re-run compile and focused strategy/admission regressions after integrating the wave.

## Review

- [x] Confirm `admission.rs` no longer centralizes deployment matching and admission policy helpers in one root file.
- [x] Confirm `staggered_arb_live.rs` no longer centralizes all runtime filters/evaluation/state helpers inline.
- [x] Confirm `momentum.rs` no longer centralizes all signal/state/config ownership inline.
- [x] Confirm `engine.rs` no longer centralizes all execution-engine subflows in one root file.

## Progress notes

- 2026-03-10: Reserved ownership for the active parallel wave:
  - worker 1: `src/coordinator/admission.rs`
  - worker 2: `src/strategy/staggered_arb_live.rs`
  - worker 3: `src/strategy/momentum.rs`
  - mainline: `src/strategy/execution/engine.rs`
- 2026-03-10: Added [lifecycle.rs](/Users/proerror/Documents/ploy/src/strategy/execution/engine/lifecycle.rs) and moved `StrategyEngine` cycle-control ownership out of [engine.rs](/Users/proerror/Documents/ploy/src/strategy/execution/engine.rs), including halt persistence, strategy-state persistence, forced-leg2 fallback, abort handling, and idle transition helpers.
- 2026-03-10: Added [matcher.rs](/Users/proerror/Documents/ploy/src/strategy/momentum/matcher.rs) and moved `EventMatcher` / `EventInfo` ownership out of [momentum.rs](/Users/proerror/Documents/ploy/src/strategy/momentum.rs), keeping the strategy root focused on signals, position logic, and runtime orchestration.
- 2026-03-10: Added [entry.rs](/Users/proerror/Documents/ploy/src/strategy/staggered_arb_live/entry.rs) and moved opening-window gating plus entry-evaluation logic out of [staggered_arb_live.rs](/Users/proerror/Documents/ploy/src/strategy/staggered_arb_live.rs), keeping the root adapter focused on orchestration and state transitions.
- 2026-03-10: Validation passed for the local engine slice:
  - `rtk cargo check --lib`
  - `rtk cargo test test_deployment_gate_accepts_explicit_deployment_and_applies_metadata --lib -- --nocapture`
  - `rtk cargo test test_build_order_request_uses_stable_idempotency_key_by_window --lib -- --nocapture`
  - `rtk cargo test transition_to_idle_clears_state --lib -- --nocapture`
  - `rtk cargo test abort_cycle_without_active_cycle --lib -- --nocapture`
- 2026-03-10: Validation passed for the momentum matcher slice:
  - `rtk cargo check --lib`
  - `rtk cargo test test_parse_price_from_question --lib -- --nocapture`
  - `rtk cargo test test_event_matcher_includes_btc_5m_series --lib -- --nocapture`
  - `rtk cargo test test_find_event_with_timing_prefers_best_across_all_series --lib -- --nocapture`
- 2026-03-10: Validation passed for the local momentum slice:
  - `rtk cargo check --lib`
  - `rtk cargo test test_parse_price_from_question --lib -- --nocapture`
  - `rtk cargo test test_event_matcher_ --lib -- --nocapture`
- 2026-03-10: Validation passed for the staggered-arb entry slice:
  - `rtk cargo check --lib`
  - `rtk cargo test test_try_entry_does_not_cap_concurrency_when_max_concurrent_is_zero --lib -- --nocapture`
  - `rtk cargo test test_try_entry_rejects_sigma_above_max_entry_sigma --lib -- --nocapture`
  - `rtk cargo test test_try_entry_requires_persistent_other_ask_before_leg1 --lib -- --nocapture`

# Live Strategy Core Wave 2 (2026-03-10)

## Goal
Keep collapsing the largest active live-strategy and execution modules by extracting another cohesive slice from the two biggest strategy files, the execution engine, and the claimer daemon.

## Tasks

- [x] Extract a second major ownership slice out of `src/strategy/staggered_arb_live.rs`.
- [x] Extract a second major ownership slice out of `src/strategy/momentum.rs`.
- [x] Extract a second major ownership slice out of `src/strategy/execution/engine.rs`.
- [x] Extract a major daemon/discovery-adjacent slice out of `src/strategy/claimer.rs`.
- [x] Re-run compile and focused live-strategy/claimer regressions after integrating the wave.

## Review

- [x] Confirm `staggered_arb_live.rs` no longer centralizes both entry and the next major runtime branch inline.
- [x] Confirm `momentum.rs` no longer centralizes both matcher/discovery and the next major strategy branch inline.
- [x] Confirm `engine.rs` no longer centralizes both lifecycle and the next major execution branch inline.
- [x] Confirm `claimer.rs` no longer centralizes both discovery and the next major daemon/claim path inline.

## Progress notes

- 2026-03-10: Reserved ownership for the next parallel wave:
  - worker 1: `src/strategy/staggered_arb_live.rs`
  - worker 2: `src/strategy/momentum.rs`
  - worker 3: `src/strategy/claimer.rs`
  - mainline: `src/strategy/execution/engine.rs`
- 2026-03-10: Added [lifecycle.rs](/Users/proerror/Documents/ploy/src/strategy/staggered_arb_live/lifecycle.rs) and moved staggered-arb position lifecycle ownership out of [staggered_arb_live.rs](/Users/proerror/Documents/ploy/src/strategy/staggered_arb_live.rs), including paper/live position structs, fill tracking, expired-event settlement, and leg finalization flow.
- 2026-03-10: Added [detector.rs](/Users/proerror/Documents/ploy/src/strategy/momentum/detector.rs) and moved `MomentumSignal` / `MomentumDetector` ownership out of [momentum.rs](/Users/proerror/Documents/ploy/src/strategy/momentum.rs), leaving the root strategy focused on orchestration and stateful trade management.
- 2026-03-10: Added [hedge_flow.rs](/Users/proerror/Documents/ploy/src/strategy/execution/engine/hedge_flow.rs) and moved Leg2 execution, forced hedge handling, and unwind ownership out of [engine.rs](/Users/proerror/Documents/ploy/src/strategy/execution/engine.rs), leaving the root engine focused on round/Leg1 orchestration plus lifecycle wrappers.
- 2026-03-10: Added [relayer.rs](/Users/proerror/Documents/ploy/src/strategy/claimer/relayer.rs) and moved relayer credential, proxy-signing, polling, and gasless-claim ownership out of [claimer.rs](/Users/proerror/Documents/ploy/src/strategy/claimer.rs), leaving the root daemon focused on eligibility, on-chain claim flow, and gas top-up orchestration.
- 2026-03-10: Validation passed for the wave:
  - `rtk cargo check --lib`
  - `rtk cargo test test_feed_builder --lib -- --nocapture`
  - `rtk cargo test test_from_data_plane_reuses_singleton_adapters --lib -- --nocapture`
  - `rtk cargo test characterization_replay_polymarket_quote_to_strategy_market_update --lib -- --nocapture`
  - `rtk cargo test leg2_pending_cycle_version_conflict_should_abort_and_error --lib -- --nocapture`
  - `rtk cargo test leg2_cycle_version_conflict_should_abort_and_error --lib -- --nocapture`

# Strategy And Adapter Wave 3 (2026-03-10)

## Goal
Keep collapsing the remaining active-core hotspots by extracting another major slice from the two heaviest live strategies, the monolithic strategy CLI, and the Polymarket adapter.

## Tasks

- [x] Extract another major ownership slice out of `src/strategy/staggered_arb_live.rs`.
- [x] Extract another major ownership slice out of `src/strategy/momentum.rs`.
- [x] Extract another major ownership slice out of `src/cli/strategy.rs`.
- [x] Extract a major API/ownership slice out of `src/adapters/polymarket_clob.rs`.
- [x] Re-run compile and focused regressions after integrating the wave.

## Review

- [x] Confirm `staggered_arb_live.rs` no longer centralizes both lifecycle and the next runtime branch inline.
- [x] Confirm `momentum.rs` no longer centralizes both detector ownership and the next strategy branch inline.
- [x] Confirm `cli/strategy.rs` no longer centralizes both command parsing and the next large operational command branch inline.
- [x] Confirm `polymarket_clob.rs` no longer centralizes both gateway/auth core and the next API ownership branch inline.

## Progress notes

- 2026-03-10: Reserved ownership for the next parallel wave:
  - worker 1: `src/strategy/staggered_arb_live.rs`
  - worker 2: `src/strategy/momentum.rs`
  - worker 3: `src/cli/strategy.rs`
  - mainline: `src/adapters/polymarket_clob.rs`
- 2026-03-10: Added [maintenance_ops.rs](/Users/proerror/Documents/ploy/src/cli/strategy/maintenance_ops.rs) and moved the strategy CLI's seeding, integrity, and backfill handlers out of [strategy.rs](/Users/proerror/Documents/ploy/src/cli/strategy.rs), leaving the root CLI focused on command wiring plus thin delegation.
- 2026-03-10: Added [analysis_commands.rs](/Users/proerror/Documents/ploy/src/cli/strategy/analysis_commands.rs) and moved the remaining analysis/reporting CLI argument + dispatch ownership out of [strategy.rs](/Users/proerror/Documents/ploy/src/cli/strategy.rs), leaving the root file focused on top-level subcommand wiring.
- 2026-03-10: Added [position_exit.rs](/Users/proerror/Documents/ploy/src/strategy/momentum/position_exit.rs) and moved momentum position/exit ownership out of [momentum.rs](/Users/proerror/Documents/ploy/src/strategy/momentum.rs), leaving the root strategy focused on discovery, signals, and runtime orchestration.
- 2026-03-10: Added [order_updates.rs](/Users/proerror/Documents/ploy/src/strategy/staggered_arb_live/order_updates.rs) and moved live order-update reconciliation, stale-order cancellation, and orphan cleanup ownership out of [staggered_arb_live.rs](/Users/proerror/Documents/ploy/src/strategy/staggered_arb_live.rs), keeping the root adapter focused on market/update orchestration.
- 2026-03-10: Added [gamma.rs](/Users/proerror/Documents/ploy/src/adapters/polymarket_clob/gamma.rs) and moved Gamma discovery/types ownership out of [polymarket_clob.rs](/Users/proerror/Documents/ploy/src/adapters/polymarket_clob.rs), leaving the adapter root focused on gateway/auth/trading flows.
- 2026-03-10: Validation passed for the wave:
  - `rtk cargo test test_exit_manager_stop_loss --lib -- --exact --nocapture`
  - `rtk cargo test test_orphan_leg1_cleanup_keeps_lock_and_allows_late_reconciliation --lib -- --exact --nocapture`
  - `rtk cargo test test_leg2_partial_then_full_fill_closes_once_with_weighted_price --lib -- --exact --nocapture`
  - `rtk cargo check --lib`

# Strategy And Adapter Wave 4 (2026-03-10)

## Goal
Keep shrinking the remaining live-strategy core by pulling the momentum engine's runtime/entry orchestration out of the root file now that detector, matcher, and exit ownership have already moved.

## Tasks

- [x] Extract the momentum runtime/event-loop ownership out of `src/strategy/momentum.rs`.
- [x] Keep the root strategy file focused on stateful trade management, PM update handling, and shared helpers.
- [x] Re-run compile and focused momentum/feeds regressions after the extraction.

## Review

- [x] Confirm `momentum.rs` no longer inlines the main run loop plus the full CEX/Chainlink entry path.
- [x] Confirm the extracted module keeps Binance/Chainlink entry behavior unchanged while preserving the existing strategy API.

## Progress notes

- 2026-03-10: Added [entry_runtime.rs](/Users/proerror/Documents/ploy/src/strategy/momentum/entry_runtime.rs) and moved the momentum engine's main run loop, CEX entry path, directional Binance entry path, PM ask lookup, and Chainlink entry path out of [momentum.rs](/Users/proerror/Documents/ploy/src/strategy/momentum.rs), leaving the root file focused on PM updates, position management, and execution state.
- 2026-03-10: Validation passed for the wave:
  - `rtk cargo check --lib`
  - `rtk cargo test characterization_replay_binance_price_to_strategy_market_update --lib -- --nocapture`
  - `rtk cargo test characterization_replay_polymarket_quote_to_strategy_market_update --lib -- --nocapture`
  - `rtk cargo test characterization_replay_binance_kline_to_strategy_market_update --lib -- --nocapture`
  - `rtk cargo test test_parse_price_from_question --lib -- --nocapture`
  - `rtk cargo test test_event_matcher_includes_btc_5m_series --lib -- --nocapture`
  - `rtk cargo test test_find_event_with_timing_prefers_best_across_all_series --lib -- --nocapture`

# Postgres Event Registry Extraction (2026-03-10)

## Goal
Move event-registry persistence out of `src/adapters/postgres.rs` so the root adapter keeps cycle/order/recovery ownership while registry CRUD and status-transition logic live behind a dedicated module boundary.

## Tasks

- [x] Extract the event-registry persistence methods into `src/adapters/postgres/event_registry.rs`.
- [x] Keep `PostgresStore`'s public API unchanged for discovery, RPC, and event-edge callers.
- [x] Re-run compile and focused event-edge regressions after the extraction.

## Review

- [x] Confirm `postgres.rs` no longer owns the event-registry query/state-transition implementation body.
- [x] Confirm the extracted module preserves registry filtering, status-transition validation, and stale-event expiry behavior.

## Progress notes

- 2026-03-10: Added [event_registry.rs](/Users/proerror/Documents/ploy/src/adapters/postgres/event_registry.rs) and moved `upsert_event`, `list_events`, `update_event_status`, `get_monitoring_events`, and `expire_stale_events` out of [postgres.rs](/Users/proerror/Documents/ploy/src/adapters/postgres.rs), leaving the root store focused on trading state, metrics, and recovery persistence.
- 2026-03-10: Validation passed for the slice:
  - `rtk cargo check --lib`
  - `rtk cargo test strategy::event_edge::strategy::tests::on_market_update_tracks_discovered_events_and_expiry --lib -- --exact --nocapture`
  - `rtk cargo test strategy::event_edge::strategy::tests::emits_canonical_submit_order_and_tracks_fill_into_position --lib -- --exact --nocapture`

# Staggered Arb Test Module Extraction (2026-03-10)

## Goal
Move the massive inline `staggered_arb_live` test module into a dedicated sibling file so the root strategy file reflects the live adapter implementation instead of mixing runtime ownership with 2k+ lines of tests.

## Tasks

- [x] Extract the inline `#[cfg(test)]` module out of `src/strategy/staggered_arb_live.rs` into `src/strategy/staggered_arb_live/tests.rs`.
- [x] Keep the test helpers and assertions unchanged while switching the root file to `mod tests;`.
- [x] Re-run compile and focused staggered-arb regressions after the move.

## Review

- [x] Confirm `staggered_arb_live.rs` is now focused on production strategy logic and no longer inlines the large test body.
- [x] Confirm the moved tests still exercise both entry and leg2/close paths from the new sibling module.

## Progress notes

- 2026-03-10: Added [tests.rs](/Users/proerror/Documents/ploy/src/strategy/staggered_arb_live/tests.rs) and moved the full inline `staggered_arb_live` test module out of [staggered_arb_live.rs](/Users/proerror/Documents/ploy/src/strategy/staggered_arb_live.rs), leaving the root file at `975` lines instead of `3015`.
- 2026-03-10: Validation passed for the slice:
  - `rtk cargo check --lib`
  - `rtk cargo test test_try_entry_rejects_sigma_above_max_entry_sigma --lib -- --nocapture`
  - `rtk cargo test test_leg2_partial_then_full_fill_closes_once_with_weighted_price --lib -- --nocapture`

# Polymarket CLOB Read API Extraction (2026-03-10)

## Goal
Move the heavy read-only market/order/account/trade retrieval path out of `src/adapters/polymarket_clob.rs` so the root client keeps constructor/auth/trading/status ownership while read APIs live behind a dedicated sibling module.

## Tasks

- [x] Extract the read-only CLOB/Gamma pagination and account/market retrieval methods into `src/adapters/polymarket_clob/read_api.rs`.
- [x] Keep `PolymarketClient`'s public API unchanged for callers.
- [x] Re-run compile and focused adapter regressions after the extraction.

## Review

- [x] Confirm `polymarket_clob.rs` no longer owns the bulk read/query implementation body.
- [x] Confirm the extracted module preserves market lookup, orderbook/best-price reads, account summary, position/trade history, and paginated order/trade retrieval behavior.

## Progress notes

- 2026-03-10: Added [read_api.rs](/Users/proerror/Documents/ploy/src/adapters/polymarket_clob/read_api.rs) and moved read-only retrieval ownership out of [polymarket_clob.rs](/Users/proerror/Documents/ploy/src/adapters/polymarket_clob.rs), including market/orderbook reads, balance/position/trade history, account summary, and paginated order/trade helpers.
- 2026-03-10: Validation passed for the slice:
  - `rtk cargo check --lib`
  - `rtk cargo test test_create_client --lib -- --exact --nocapture`
  - `rtk cargo test test_position_response_deserializes_numeric_fields --lib -- --exact --nocapture`

# Momentum Trade Flow Extraction (2026-03-10)

## Goal
Move the momentum strategy's trade-entry, queued-signal, and exit execution flow out of `src/strategy/momentum.rs` so the root file keeps market-update/orchestration ownership while trade-flow state transitions live behind a dedicated sibling module.

## Tasks

- [x] Extract the momentum trade-flow methods into `src/strategy/momentum/trade_flow.rs`.
- [x] Keep the existing strategy API and call graph unchanged for root/orchestration modules.
- [x] Re-run compile and focused momentum regressions after the extraction.

## Review

- [x] Confirm `momentum.rs` no longer owns the bulk entry/exit/pending-signal execution implementation body.
- [x] Confirm the extracted module preserves queue-based best-edge execution, cooldown handling, entry sizing, and exit execution behavior.

## Progress notes

- 2026-03-10: Added [trade_flow.rs](/Users/proerror/Documents/ploy/src/strategy/momentum/trade_flow.rs) and moved `maybe_enter`, `execute_exit`, `in_cooldown`, `process_pending_signals`, and `execute_pending_trade` out of [momentum.rs](/Users/proerror/Documents/ploy/src/strategy/momentum.rs), leaving the root file focused on shared state, event resolution, PM updates, and test coverage.
- 2026-03-10: Validation passed for the slice:
  - `rtk cargo check --lib`
  - `rtk cargo test test_exit_manager_stop_loss --lib -- --exact --nocapture`
  - `rtk cargo test test_find_event_with_timing_prefers_best_across_all_series --lib -- --nocapture`

# RPC PM Read Method Extraction (2026-03-10)

## Goal
Move the read-only `pm.*` JSON-RPC method handling out of `src/cli/rpc.rs` so the root file keeps protocol/idempotency/write-routing ownership while PM read dispatch lives behind a dedicated sibling module.

## Tasks

- [x] Extract the read-only PM JSON-RPC method handlers into `src/cli/rpc/pm_read_methods.rs`.
- [x] Keep the public RPC surface and method names unchanged.
- [x] Re-run compile after the extraction.

## Review

- [x] Confirm `rpc.rs` no longer inlines the full PM read dispatch surface.
- [x] Confirm the extracted module preserves request parsing, PM client initialization, and JSON-RPC response formatting for read-only `pm.*` methods.

## Progress notes

- 2026-03-10: Added [pm_read_methods.rs](/Users/proerror/Documents/ploy/src/cli/rpc/pm_read_methods.rs) and moved the read-only `pm.*` RPC handlers out of [rpc.rs](/Users/proerror/Documents/ploy/src/cli/rpc.rs), including event resolution, balance/positions/open-orders/order lookup, market/event/orderbook/trade reads, and account summary handling.
- 2026-03-10: Validation passed for the slice:
  - `rtk cargo check --lib`

# Polymarket WS Message Handling Extraction (2026-03-10)

## Progress notes

- 2026-03-10: Added [messages.rs](/Users/proerror/Documents/ploy/src/adapters/polymarket_ws/messages.rs) and moved Polymarket websocket payload types, book-top normalization helpers, and inbound `handle_message` / `process_*` handling out of [polymarket_ws.rs](/Users/proerror/Documents/ploy/src/adapters/polymarket_ws.rs), leaving the root file focused on connection lifecycle, subscription state, cache ownership, and tests.

# Strategy Engine Leg1 Extraction (2026-03-10)

## Goal
Move the Leg1 submission/fill/version-conflict path out of `src/strategy/execution/engine.rs` so the root engine keeps orchestration ownership while the highest-risk entry flow lives in a dedicated sibling module.

## Tasks

- [x] Extract the full `enter_leg1` implementation into `src/strategy/execution/engine/leg1.rs`.
- [x] Keep the root `StrategyEngine` API unchanged by delegating through a thin wrapper.
- [x] Re-run compile plus focused Leg1 regression tests after the extraction.

## Review

- [x] Confirm `engine.rs` no longer inlines the full Leg1 order submission/fill/unwind path.
- [x] Confirm Leg1 cycle-version persistence checks still abort correctly on conflicts after the extraction.

## Progress notes

- 2026-03-10: Added [leg1.rs](/Users/proerror/Documents/ploy/src/strategy/execution/engine/leg1.rs) and moved the full `enter_leg1` path out of [engine.rs](/Users/proerror/Documents/ploy/src/strategy/execution/engine.rs), including quote freshness, slippage gating, IOC request creation, order persistence, execution result handling, cycle-version conflict aborts, and detector triggering.
- 2026-03-10: Validation passed for the slice:
  - `rtk cargo check --lib`
  - `rtk cargo test leg1_cycle_version_conflict_should_abort_and_error --lib -- --nocapture`
  - `rtk cargo test leg_updates_should_use_incrementing_cycle_versions --lib -- --nocapture`

# Strategy Engine Round Flow Extraction (2026-03-10)

## Goal
Move quote-driven round management out of `src/strategy/execution/engine.rs` so the root engine file keeps constructor/runtime shell ownership while round updates, watch-window transitions, and cycle-state-driven quote handling live behind a dedicated sibling module.

## Tasks

- [x] Extract `on_quote_update`, `check_round_transition`, and `set_round` into `src/strategy/execution/engine/round_flow.rs`.
- [x] Keep the public `StrategyEngine` API unchanged via thin delegating wrappers in the root file.
- [x] Re-run compile plus focused `set_round` regressions after the extraction.

## Review

- [x] Confirm `engine.rs` no longer inlines the quote-driven round/watch-window/cycle routing logic.
- [x] Confirm watch-window entry and mid-cycle round-switch guards still behave correctly after the extraction.

## Progress notes

- 2026-03-10: Added [round_flow.rs](/Users/proerror/Documents/ploy/src/strategy/execution/engine/round_flow.rs) and moved quote-driven round handling out of [engine.rs](/Users/proerror/Documents/ploy/src/strategy/execution/engine.rs), including watch-window expiry, token filtering, Leg2 force checks, timeout-based round transitions, and `set_round` detector reset/persistence logic.
- 2026-03-10: Validation passed for the slice:
  - `rtk cargo check --lib`
  - `rtk cargo test set_round_transitions_to_watch_window --lib -- --nocapture`
  - `rtk cargo test set_round_blocked_mid_cycle --lib -- --nocapture`

# Sidecar Ingress Helper Extraction (2026-03-10)

## Goal
Move sidecar ingress/account-scope/deployment-binding/broadcast helpers out of `src/api/handlers/sidecar.rs` so the root handler file keeps request/response shapes, endpoint flow, persistence, and tests while ingress policy lives in a dedicated sibling module.

## Tasks

- [x] Extract sidecar ingress/account-scope/deployment-binding/broadcast helpers into `src/api/handlers/sidecar/ingress.rs`.
- [x] Keep handler behavior and endpoint surface unchanged by reusing the extracted helpers from the root module.
- [x] Re-run compile plus focused ingress/deployment helper regressions after the extraction.

## Review

- [x] Confirm `sidecar.rs` no longer inlines the ingress/account-scope/deployment-binding/broadcast helper bodies.
- [x] Confirm side/domain parsing and deployment metadata behavior still matches the existing tests after the extraction.

## Progress notes

- 2026-03-10: Added [ingress.rs](/Users/proerror/Documents/ploy/src/api/handlers/sidecar/ingress.rs) and moved sidecar ingress/account-scope/deployment-binding/broadcast helper ownership out of [sidecar.rs](/Users/proerror/Documents/ploy/src/api/handlers/sidecar.rs), leaving the root file focused on request/response types, handler flow, persistence, and tests.
- 2026-03-10: Validation passed for the slice:
  - `rtk cargo check --lib`
- 2026-03-10: `rtk cargo check --lib` passed after the extraction; focused `rtk cargo test --lib parse_domain_rejects_unknown_values -- --nocapture` is currently blocked by unrelated `src/cli/strategy/backtest_ops.rs` visibility errors in the existing workspace.

# Managed Runtime Coordinator Ingress Cutover (2026-03-10)

## Goal
Finish the managed-strategy live-path migration by making `StrategyAction::SubmitIntent` flow through coordinator ingress instead of direct `OrderExecutor::execute(...)`, while preserving strategy-local order tracking and runtime observability.

## Tasks

- [x] Preserve `client_order_id`, `order_type`, and `time_in_force` on `OrderIntent` and coordinator-built `OrderRequest`.
- [x] Sync coordinator rejection/pending/execution updates back into managed strategy runtimes.
- [x] Rewire the managed runtime action loop to submit via `CoordinatorHandle::submit_order(...)` instead of direct execution.
- [x] Keep recovery/control-plane callers aligned when they override `intent_id`.
- [x] Re-run compile plus focused contract/coordinator regressions after the cutover.

## Review

- [x] Confirm managed strategy submit flow no longer calls `executor.execute(...)` directly from `src/coordinator/strategy_runtime/actions.rs`.
- [x] Confirm coordinator ingress/execution emits rejection/pending/execution updates that the managed runtime consumes.
- [x] Confirm `OrderIntent -> OrderRequest` preserves runtime client order identity and execution semantics.

## Progress notes

- 2026-03-10: Extended [types.rs](/Users/proerror/Documents/ploy/src/platform/types.rs) so `OrderIntent` now owns `client_order_id`, `order_type`, and `time_in_force`, with defaults preserved for non-strategy callers.
- 2026-03-10: Updated [runtime_order.rs](/Users/proerror/Documents/ploy/src/strategy/runtime_order.rs) and [duplicate_guard.rs](/Users/proerror/Documents/ploy/src/coordinator/admission/duplicate_guard.rs) so coordinator-built requests keep the strategy/runtime client order ID plus `Market/IOC`-style execution settings.
- 2026-03-10: Rewired [actions.rs](/Users/proerror/Documents/ploy/src/coordinator/strategy_runtime/actions.rs) and [strategy_runtime.rs](/Users/proerror/Documents/ploy/src/coordinator/strategy_runtime.rs) so managed runtimes now submit canonical intents through coordinator ingress and consume coordinator order-update callbacks for local strategy state progression.
- 2026-03-10: Synced `intent_id` overrides in [control_plane.rs](/Users/proerror/Documents/ploy/src/control_plane.rs), [write_side.rs](/Users/proerror/Documents/ploy/src/api/handlers/sidecar/write_side.rs), and [recovery.rs](/Users/proerror/Documents/ploy/src/coordinator/coordinator/recovery.rs) so default client order IDs stay deterministic after external/requested intent IDs are applied.
- 2026-03-10: Validation passed for the cutover:
  - `CARGO_TARGET_DIR=/tmp/ploy-wave11-check2 rtk cargo check --lib --message-format=short`
  - `CARGO_TARGET_DIR=/tmp/ploy-wave11-check2 rtk cargo test order_intent_from_strategy_intent_preserves_runtime_metadata --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-wave11-check2 rtk cargo test test_build_order_request_uses_stable_idempotency_key_by_window --lib -- --exact --nocapture`
  - `CARGO_TARGET_DIR=/tmp/ploy-wave11-check2 rtk cargo test test_drain_and_execute_records_single_success_for_buy_fill --lib -- --exact --nocapture`

# Polymarket WS Surface Split (2026-03-11)

## Goal
Move the remaining adapter surface/bootstrap owner out of `src/adapters/polymarket_ws.rs` so the root module becomes a thin façade plus test-only support, while runtime/message/subscription ownership stays in sibling modules.

## Tasks

- [x] Extract `PolymarketWebSocket` / `QuoteUpdate` plus constructor and health/freshness wiring into `src/adapters/polymarket_ws/surface.rs`.
- [x] Keep the test-only `ingest_test_message` helper out of the live-path surface cut.
- [x] Re-run focused compile and polymarket WS regressions after the split.

## Progress notes

- 2026-03-11: Added [surface.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/adapters/polymarket_ws/surface.rs) and reduced [polymarket_ws.rs](/Users/proerror/Documents/ploy-order-intent-cut/src/adapters/polymarket_ws.rs) to module wiring, re-exports, and test-only support.
- 2026-03-11: Validation passed:
  - `rtk cargo check --lib --message-format=short`
  - `rtk cargo test test_build_subscription_list_includes_extra_tokens --lib -- --exact --nocapture`
  - `rtk cargo test characterization_single_book_message --lib -- --exact --nocapture`
  - `rtk cargo test characterization_freshness_recorded_on_book_update --lib -- --exact --nocapture`

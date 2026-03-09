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
- [ ] Confirm the CLI command still prints NBA comeback signals in dry-run mode.

## Progress notes

- 2026-03-09: Reworked [strategy.rs](/Users/proerror/Documents/ploy/src/cli/strategy.rs) so the `nba_comeback` CLI command now drives [strategy.rs](/Users/proerror/Documents/ploy/src/strategy/nba_comeback/strategy.rs) directly instead of instantiating `platform::NbaComebackAgent`.
- 2026-03-09: Added `NbaComebackStrategy::from_config(...)` plus a direct-config unit test so canonical callers no longer need a TOML round trip.
- 2026-03-09: Deleted [nba_agent.rs](/Users/proerror/Documents/ploy/src/platform/agents/nba_agent.rs) and removed the dead `NbaComebackAgent` exports from [mod.rs](/Users/proerror/Documents/ploy/src/platform/agents/mod.rs) and [mod.rs](/Users/proerror/Documents/ploy/src/platform/mod.rs).
- 2026-03-09: Validation passed:
  - `cargo check --bin ploy`
  - `cargo test strategy::nba_comeback::strategy --lib -- --nocapture`

# Canonical Strategy Handoff Unification (2026-03-09)

## Goal
Start collapsing the duplicate `StrategyAction::SubmitOrder { OrderRequest }` vs `CoordinatorHandle::submit_order(OrderIntent)` contract so canonical strategies stop depending on a private runtime-only execution path.

## Tasks

- [ ] Define the canonical strategy-side submit payload that can survive outside `strategy_runtime.rs`.
- [ ] Keep existing strategies compiling through a compatibility path while the new handoff is introduced.
- [ ] Move managed runtime submission closer to coordinator admission instead of direct executor ownership.
- [ ] Preserve order-update feedback so strategies still receive fills/status changes.
- [ ] Add a regression test covering action id propagation into the actual execution handoff.

## Review

- [ ] Confirm the new canonical submit payload is not another permanent fourth runtime contract.
- [ ] Confirm managed runtime still preserves `client_order_id` and idempotency semantics.
- [ ] Confirm strategies still observe terminal fill/failure updates after the handoff change.
- 2026-03-09: Both validations were blocked by unrelated in-flight errors outside this slice, currently in `src/coordinator/bootstrap.rs` and `src/strategy/nba_comeback/*`. The latest `cargo check --lib` output no longer reported `event_edge` compile errors after fixing the local wrapper issues.

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
  - `cargo test runtime_scope_keeps_politics_when_no_explicit_selection -- --nocapture`
  - `cargo test explicit_selection_disables_politics_without_politics_flag -- --nocapture`
  - `cargo test sports_runtime_config_defaults_match_bootstrap_expectations --lib -- --nocapture`
  - `cargo test politics_runtime_config_defaults_match_bootstrap_expectations --lib -- --nocapture`

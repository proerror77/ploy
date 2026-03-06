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
- [x] Write the approved design doc under `docs/plans/2026-03-06-layered-live-runtime-design.md`.
- [x] Write the implementation plan under `docs/plans/2026-03-06-layered-live-runtime-implementation-plan.md`.
- [x] Commit the planning docs atomically with explicit paths only.

## Review

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

# Layered Live Runtime Task 1: Canonical Ownership Freeze (2026-03-06)

## Goal
Make the canonical live runtime ownership explicit in source comments before any behavioral migration work starts.

## Tasks

- [x] Mark `src/strategy/traits.rs` as the only canonical live strategy contract.
- [x] Mark `src/agents/traits.rs` as a transitional compatibility surface.
- [x] Mark `src/platform/traits.rs` / `DomainAgent` as transitional compatibility only.
- [x] Add a short architecture note to `src/strategy/mod.rs` directing new live strategy work to the canonical path.
- [x] Record the immediate freeze rule in `docs/plans/2026-03-06-layered-live-runtime-design.md`.
- [x] Validate with `cargo check --lib`.
- [x] Commit the Task 1 boundary freeze atomically.

## Review

- [x] No runtime logic changed in this step.
- [x] The change is limited to ownership comments and migration guidance.
- [x] `cargo check --lib` passed; only pre-existing warnings remained in unrelated backtest files.

---

# Layered Live Runtime Task 2: Shrink Strategy Contract (2026-03-06)

## Goal
Remove governance/feed-lifecycle mutations from the canonical `StrategyAction` contract and keep live strategy outputs decision-only.

## Tasks

- [x] Remove `UpdateRisk`, `SubscribeFeed`, and `UnsubscribeFeed` from `src/strategy/traits.rs`.
- [x] Delete obsolete consumer branches in `src/strategy/orchestrator.rs`, `src/coordinator/bootstrap.rs`, and `src/cli/strategy.rs`.
- [x] Convert compiled strategy/runtime code to static feed ownership, including `src/strategy/gamma_scalping/strategy.rs`.
- [x] Synchronize legacy `src/strategy/strategies/*` implementations with the new contract so they no longer rely on runtime-owned actions.
- [x] Add focused regression tests for static feed declarations and event-handling behavior where compiled coverage exists.
- [x] Run focused validation and capture the results.
- [x] Commit the Task 2 contract shrink atomically.

## Review

- [x] Confirmed `DataFeedManager::start_for_feeds()` already owns Polymarket series discovery and token-quote subscription for the compiled runtime path.
- [x] Confirmed `gamma_scalping` now keeps `required_feeds()` static and no longer requests dynamic quote subscriptions on event discovery.
- [x] Confirmed canonical runtime consumers no longer accept strategy-originated governance mutations or feed rewiring requests.
- [x] Focused validation passed:
  - `cargo test strategy::manager --lib -- --nocapture`
  - `cargo test strategy::gamma_scalping::strategy --lib -- --nocapture`
- [x] Validation still shows only the same pre-existing warnings in unrelated backtest files (`liquidity_vacuum_backtest.rs`, `garch_probability_backtest.rs`).
- [x] `src/strategy/strategies/momentum_strat.rs` and `src/strategy/strategies/two_leg.rs` were synchronized to the new contract, but they are not currently reachable from `src/strategy/mod.rs`, so the runtime validation for this task remains centered on the compiled canonical path.

---

# Layered Live Runtime Task 3: Extract Managed Strategy Runtime (2026-03-06)

## Goal
Move the managed strategy runtime loop out of `src/coordinator/bootstrap.rs` into a dedicated coordinator module so bootstrap only assembles runtime inputs.

## Tasks

- [x] Create `src/coordinator/strategy_runtime.rs` and move the managed runtime loop there.
- [x] Introduce a generic `ManagedStrategyRuntimeConfig` value object for runtime startup inputs.
- [x] Point bootstrap at the extracted runtime module instead of keeping the main loop inline.
- [x] Keep shared observability/schema helpers available while avoiding a wider bootstrap churn in this slice.
- [x] Validate with `cargo test coordinator::bootstrap --lib -- --nocapture`.
- [x] Commit the Task 3 extraction atomically.

## Review

- [x] Managed strategy startup now routes through `src/coordinator/strategy_runtime.rs`.
- [x] `bootstrap.rs` no longer owns the runtime loop body; it only assembles runtime inputs and delegates.
- [x] Shared strategy observability/schema helpers remain in bootstrap for now because they are also used outside the managed runtime path.
- [x] Validation passed:
  - `cargo test coordinator::bootstrap --lib -- --nocapture`
- [x] The only remaining warnings are the same pre-existing unrelated ones in `liquidity_vacuum_backtest.rs` and `garch_probability_backtest.rs`.

---

# Layered Live Runtime Task 4: Governance-Only OpenClaw Context (2026-03-06)

## Goal
Split governance-only coordinator access from trading access so OpenClaw can manage policy and strategy state without retaining direct order-ingress capability.

## Tasks

- [x] Create `src/agents/governance_context.rs` with policy/state/command access only.
- [x] Remove governance-policy and pause/resume helpers from `src/agents/context.rs` so the trading context stays order-focused.
- [x] Re-export the narrowed governance context from `src/agents/mod.rs`.
- [x] Convert `src/agents/openclaw/agent.rs` to narrow `AgentContext` into `GovernanceContext` at runtime.
- [x] Update OpenClaw module docs to reflect its governance-only role.
- [x] Add focused regression tests for governance policy round-trips, command receipt, and agent pause/resume control commands.
- [x] Run focused validation and capture the results.
- [x] Commit the Task 4 context split atomically.

## Review

- [x] OpenClaw still uses the compatibility `TradingAgent` trait for bootstrap registration, but it now converts immediately into `GovernanceContext` and no longer has access to `submit_order`.
- [x] `AgentContext` is now the order-submitting compatibility surface only; governance pause/resume and policy mutation moved behind the narrowed context.
- [x] Focused validation passed:
  - `cargo test coordinator --lib -- --nocapture`
  - `cargo test governance_context --lib -- --nocapture`
- [x] Validation still shows only the same pre-existing unrelated warnings in `liquidity_vacuum_backtest.rs` and `garch_probability_backtest.rs`.

---

# Layered Live Runtime Task 5: Externalize Live Risk Ownership (2026-03-06)

## Goal
Stop treating runtime traits as the canonical owner of `AgentRiskParams` so live risk binding stays in bootstrap/coordinator/platform registration.

## Tasks

- [x] Remove `risk_params()` from `TradingAgent` and `DomainAgent`.
- [x] Update platform registration to require explicit externally supplied risk params instead of reading them from the runtime trait.
- [x] Delete now-obsolete `risk_params()` impls across pull-based and platform-agent compatibility runtimes.
- [x] Trim dead runtime-owned risk fields/imports that only existed to satisfy the old trait.
- [x] Add a focused risk-gate regression test proving externally bound agent/domain params still drive market allow-list enforcement.
- [x] Run focused validation and capture the results.
- [x] Commit the Task 5 risk-ownership move atomically.

## Review

- [x] `AgentRiskParams` remains the runtime risk type, but bootstrap/platform/coordinator registration is now the only canonical binding path.
- [x] Pull-based `TradingAgent` and transitional `DomainAgent` runtimes no longer claim risk ownership through their trait contracts.
- [x] `platform::risk` now documents external binding semantics and has a regression test covering externally registered market allow-lists.
- [x] Focused validation passed:
  - `cargo test platform::risk --lib -- --nocapture`
- [x] Validation still shows only the same pre-existing unrelated warnings in `liquidity_vacuum_backtest.rs` and `garch_probability_backtest.rs`.

---

# Layered Live Runtime Task 6: Move Momentum And Split-Arb To Canonical Runtime Startup (2026-03-06)

## Goal
Make managed crypto momentum and split-arb/staggered-arb deployments start through the canonical managed strategy runtime, with momentum runtime config projected from legacy live settings instead of template-only defaults.

## Tasks

- [x] Remove the bootstrap momentum live branch that spawned `CryptoTradingAgent`.
- [x] Route managed momentum startup through `run_managed_strategy_runtime(...)` using the coordinator registration path.
- [x] Keep `src/agents/crypto.rs` as a transitional compatibility surface only and mark that status in module docs.
- [x] Project legacy `CryptoTradingConfig` timing / sizing / edge / exit settings into the generated momentum TOML so canonical runtime startup does not silently fall back to unrelated template defaults.
- [x] Add focused bootstrap tests covering momentum deployment enablement, momentum config projection, and split-arb config rendering stability.
- [x] Fix the split-arb config tests to serialize access to `PLOY_STAGGERED_ARB_CONFIG` so bootstrap config tests no longer race each other.
- [x] Run focused validation and capture the results.
- [x] Commit the Task 6 runtime migration atomically.

## Review

- [x] Managed momentum bootstrap now registers with coordinator and launches via the canonical managed strategy runtime instead of the pull-based `CryptoTradingAgent` path.
- [x] `build_momentum_runtime_config(...)` now overrides template symbols and also projects legacy crypto live defaults such as timing window, cooldown, share sizing, directional mode, and exit thresholds.
- [x] Split-arb/staggered-arb remains on the canonical managed runtime path; this slice only stabilized its bootstrap config tests.
- [x] Focused validation passed:
  - `cargo test build_momentum_runtime_config_ --lib -- --nocapture`
  - `cargo test coordinator::bootstrap --lib -- --nocapture`
  - `cargo test strategy::adapters --lib -- --nocapture`
- [x] Validation still shows only the same pre-existing unrelated warnings in `liquidity_vacuum_backtest.rs` and `garch_probability_backtest.rs`.

---

# Layered Live Runtime Task 7A: Canonical EventEdge Strategy Bridge (2026-03-06)

## Goal
Move the politics/event-edge live path onto the canonical managed strategy runtime before tackling the larger NBA/sports migration surface.

## Tasks

- [x] Add `src/strategy/event_edge/strategy.rs` as a canonical `Strategy` wrapper around `EventEdgeCore`.
- [x] Register `event_edge` in `StrategyFactory` and expose it through the event-edge module.
- [x] Build bootstrap-generated TOML for `event_edge` from `EventEdgeAgentConfig`.
- [x] Replace the politics bootstrap branch that spawned `PoliticsTradingAgent` with managed strategy runtime startup.
- [x] Add focused tests for `event_edge` TOML parsing, decision normalization, and filled-order state updates.
- [x] Run focused validation and capture the results.
- [ ] Migrate `nba_comeback` / sports live runtime onto the same canonical path.
- [ ] Commit the full Task 7 migration once sports/NBA is moved as well.

## Review

- [x] `event_edge` can now run through the canonical `Strategy` contract with a tick-driven scan loop.
- [x] Politics bootstrap now assembles a managed-runtime config instead of spawning `PoliticsTradingAgent`.
- [x] This slice intentionally leaves NBA/sports migration for the next atomic cut because that path still owns ESPN/PolymarketSports/Grok observation logic not yet bridged into the strategy plane.
- [x] Focused validation passed:
  - `cargo test strategy::event_edge --lib -- --nocapture`
  - `cargo test coordinator::bootstrap --lib -- --nocapture`
  - `cargo test strategy::manager --lib -- --nocapture`

---

# Layered Live Runtime Task 7B: Canonical NBA Comeback Strategy Bridge (2026-03-06)

## Goal
Add the first canonical `nba_comeback` strategy wrapper so NBA logic can enter the `Strategy` plane before the larger sports bootstrap/runtime migration.

## Tasks

- [x] Add `src/strategy/nba_comeback/strategy.rs` as a canonical `Strategy` wrapper around `NbaComebackCore`.
- [x] Expose the wrapper through `src/strategy/nba_comeback/mod.rs`.
- [x] Register `nba_comeback` in `StrategyFactory` and list it in `available_strategies()`.
- [x] Keep sports bootstrap on the legacy agent path for now; do not mix the wrapper cut with the larger ESPN/PM sports runtime migration.
- [x] Add focused tests for TOML parsing, trailing-market resolution, and filled-order position tracking.
- [x] Run focused validation and capture the results.
- [ ] Switch sports live bootstrap from `SportsTradingAgent` to the canonical managed strategy runtime.

## Review

- [x] `nba_comeback` now has a canonical `Strategy`-plane wrapper that produces normalized `SubmitOrder` actions.
- [x] The wrapper currently covers deterministic ESPN + PolymarketSports entry logic and execution callbacks; it intentionally does not absorb the legacy sports agent's DB persistence, Grok, or collector responsibilities yet.
- [x] The sports bootstrap path remains unchanged in this slice to avoid mixing the bridge with a higher-risk live-runtime cut.
- [x] Focused validation passed:
  - `cargo test strategy::nba_comeback::strategy --lib -- --nocapture`
  - `cargo test strategy::manager --lib -- --nocapture`
  - `cargo check --lib`
- [x] Validation still shows only the same pre-existing unrelated warnings in `liquidity_vacuum_backtest.rs` and `garch_probability_backtest.rs`.

---

# Layered Live Runtime Task 7C: Managed Runtime Domain Identity Fix (2026-03-06)

## Goal
Remove the remaining `Domain::Crypto` hardcoding from the canonical managed strategy runtime so runtime health/reporting reflects the actual strategy plane domain.

## Tasks

- [x] Add explicit `domain: Domain` to `ManagedStrategyRuntimeConfig`.
- [x] Thread the domain through bootstrap's `run_managed_strategy_runtime(...)` wrapper.
- [x] Pass concrete domains from each current managed-runtime bootstrap call site (`momentum`, `pattern_memory`, `split_arb`, `event_edge`).
- [x] Use the configured domain in managed runtime health snapshots instead of hardcoding `Domain::Crypto`.
- [x] Run focused validation and capture the results.

## Review

- [x] Managed runtime identity is now explicit instead of inferred from a crypto-only default.
- [x] Execution behavior did not change; this slice only fixes runtime metadata/reporting.
- [x] Focused validation passed:
  - `cargo test coordinator::bootstrap --lib -- --nocapture`
  - `cargo check --lib`

---

# Layered Live Runtime Task 7D: NBA Runtime Config Builder (2026-03-06)

## Goal
Prepare the sports bootstrap migration by adding a canonical TOML projection helper for `nba_comeback` without switching the live sports runtime in the same cut.

## Tasks

- [x] Add `build_nba_comeback_runtime_config(...)` to `src/coordinator/bootstrap.rs`.
- [x] Project the legacy `NbaComebackConfig` fields into canonical runtime sections (`strategy`, `entry`, `timing`, `risk`, `scan`, `database`, `grok`, `performance`, `scaling`, `exit`).
- [x] Add a focused bootstrap test proving the generated TOML carries the expected strategy/runtime fields.
- [x] Keep the helper prep-only for now; do not switch the sports live bootstrap path in this slice.
- [x] Run focused validation and capture the results.

## Review

- [x] The helper now gives bootstrap a single canonical TOML projection for `nba_comeback`, instead of forcing the future sports migration to hand-roll config inside the larger runtime cut.
- [x] This slice intentionally does not change live startup behavior; `SportsTradingAgent` still owns the sports path until the next atomic migration.
- [x] Focused validation passed:
  - `cargo test coordinator::bootstrap --lib -- --nocapture`
  - `cargo check --lib`
- [x] Validation still shows only the same pre-existing unrelated warnings in `liquidity_vacuum_backtest.rs`, `garch_probability_backtest.rs`, and the existing unused import warning in `src/strategy/event_edge/strategy.rs`.

---

# Layered Live Runtime Task 7E: Sports Runtime Default Cutover (2026-03-06)

## Goal
Move the default `nba_comeback` sports live bootstrap onto the canonical managed strategy runtime while keeping the Grok-enabled path on the legacy sports agent until the canonical wrapper absorbs that behavior.

## Tasks

- [x] Add `build_nba_comeback_managed_runtime_spec(...)` to describe the canonical launch contract for sports/NBA.
- [x] Add focused bootstrap tests proving non-Grok NBA configs project into a canonical managed-runtime launch spec.
- [x] Add a focused bootstrap test proving Grok-enabled NBA configs still defer to the legacy sports agent path.
- [x] Switch the default sports bootstrap branch to spawn `run_managed_strategy_runtime(...)` for `nba_comeback`.
- [x] Keep `SportsTradingAgent` as an explicit fallback only for Grok-enabled deployments.
- [x] Run focused validation and capture the results.

## Review

- [x] Default `nba_comeback` live startup no longer hard-depends on `SportsTradingAgent`; bootstrap now assembles a canonical managed-runtime launch spec and spawns the strategy plane path directly.
- [x] The legacy sports agent still exists as a narrow compatibility path for `grok_enabled=true`, which avoids silently dropping Grok behavior before that logic is re-homed into the canonical strategy layer.
- [x] Focused validation passed:
  - `cargo test build_nba_comeback_managed_runtime_spec_ --lib -- --nocapture`
  - `cargo test coordinator::bootstrap --lib -- --nocapture`
  - `cargo check --lib`
- [x] Validation still shows only the same pre-existing unrelated warnings in `src/strategy/event_edge/strategy.rs`, `liquidity_vacuum_backtest.rs`, and `garch_probability_backtest.rs`.

---

# Layered Live Runtime Task 8A: Narrow Legacy Runtime Re-Exports (2026-03-06)

## Goal
Shrink the public entry surface for legacy live runtimes so compatibility agents are no longer exposed from the top-level crate APIs as if they were canonical extension points.

## Tasks

- [x] Stop re-exporting legacy pull-based trading agent types from `src/agents/mod.rs`.
- [x] Update the remaining legacy bootstrap fallback to import `SportsTradingAgent` through its explicit compatibility module path.
- [x] Stop re-exporting platform-agent types from `src/platform/mod.rs`.
- [x] Update the remaining CLI caller to use `crate::platform::agents::NbaComebackAgent` explicitly.
- [x] Clarify `src/platform/agents/mod.rs` and `src/main_modes/platform_mode.rs` comments so they describe transitional/runtime-filter semantics instead of presenting legacy agents as the steady state.
- [x] Run focused validation and capture the results.

## Review

- [x] The crate root no longer presents `CryptoTradingAgent`, `PoliticsTradingAgent`, `SportsTradingAgent`, `EventEdgePlatformAgent`, or `NbaComebackAgent` as first-class top-level APIs.
- [x] Remaining legacy uses now go through explicit compatibility paths, which makes later deletion/gating work in Task 8 materially smaller.
- [x] Focused validation passed:
  - `cargo test coordinator::bootstrap --lib -- --nocapture`
  - `cargo test --bin ploy runtime_scope_disables_politics_even_if_deployment_enables_it -- --nocapture`
  - `cargo check --lib`
- [x] Validation still shows only the same pre-existing unrelated warnings in `src/strategy/event_edge/strategy.rs`, `liquidity_vacuum_backtest.rs`, and `garch_probability_backtest.rs`.

---

# Layered Live Runtime Task 8B: Narrow Platform Trait Re-Exports (2026-03-06)

## Goal
Stop presenting the push-based compatibility trait path as part of the top-level `platform` API. Legacy runtime callers should import `DomainAgent` explicitly from `platform::traits`.

## Tasks

- [x] Change `src/platform/mod.rs` so `traits` is an explicit public compatibility module.
- [x] Stop re-exporting `DomainAgent` and `SimpleAgent` from the `platform` root.
- [x] Stop re-exporting `DomainAgent` from the crate root in `src/lib.rs`.
- [x] Update the remaining CLI compatibility path to import `DomainAgent` from `crate::platform::traits`.
- [x] Update compatibility platform-agent modules to import `DomainAgent` from `crate::platform::traits`.
- [x] Run focused validation and capture the results.

## Review

- [x] `crate::platform::DomainAgent` and `ploy::DomainAgent` are no longer available as convenience APIs; compatibility callers must now opt into `crate::platform::traits::DomainAgent`.
- [x] This slice does not change runtime behavior; it only narrows a misleading compatibility surface and makes later retirement of push-based runtime paths smaller.
- [x] Focused validation passed:
  - `cargo test main_modes::platform_mode --lib -- --nocapture`
  - `cargo check --bin ploy`
- [x] Validation still shows only the same pre-existing unrelated warnings in `src/strategy/event_edge/strategy.rs`, `liquidity_vacuum_backtest.rs`, and `garch_probability_backtest.rs`.

---

# Layered Live Runtime Task 8C: Narrow Agents Trait And Context Re-Exports (2026-03-06)

## Goal
Stop presenting pull-based compatibility traits and contexts as part of the `agents` root API. Legacy runtime code should opt into explicit `agents::traits`, `agents::context`, or `agents::governance_context` paths.

## Tasks

- [x] Stop re-exporting `AgentContext`, `GovernanceContext`, `TradingAgent`, and `GovernanceAgent` from `src/agents/mod.rs`.
- [x] Update the remaining pull-based compatibility agents to import `AgentContext` and `TradingAgent` from explicit submodules.
- [x] Update OpenClaw to import `GovernanceContext` from `agents::governance_context`.
- [x] Update bootstrap to import compatibility traits/contexts from explicit submodule paths.
- [x] Run focused validation and capture the results.

## Review

- [x] `crate::agents::TradingAgent`, `crate::agents::GovernanceAgent`, `crate::agents::AgentContext`, and `crate::agents::GovernanceContext` are no longer convenience APIs from the `agents` root.
- [x] This slice does not change runtime behavior; it only narrows another misleading compatibility surface so later removal of pull-based runtime paths is mechanically smaller.
- [x] Focused validation passed:
  - `cargo test coordinator::bootstrap --lib -- --nocapture`
  - `cargo check --bin ploy`
- [x] Validation still shows only the same pre-existing unrelated warnings in `src/strategy/event_edge/strategy.rs`, `liquidity_vacuum_backtest.rs`, and `garch_probability_backtest.rs`.

---

# Layered Live Runtime Task 8D: Narrow Platform Agent Module Re-Exports (2026-03-06)

## Goal
Stop presenting legacy platform-agent structs as convenience exports from `platform::agents`. Compatibility callers should name the explicit agent submodule they depend on.

## Tasks

- [x] Make the non-RL platform-agent submodules explicit public compatibility modules in `src/platform/agents/mod.rs`.
- [x] Stop re-exporting `CryptoAgent`, `EventEdgePlatformAgent`, and `NbaComebackAgent` from `platform::agents`.
- [x] Update the remaining CLI compatibility path to use `crate::platform::agents::nba_agent::NbaComebackAgent`.
- [x] Run focused validation and capture the results.

## Review

- [x] `crate::platform::agents::NbaComebackAgent` and the equivalent convenience paths for the other non-RL platform agents are no longer available; compatibility callers now opt into the explicit submodule path.
- [x] This slice does not change runtime behavior; it only removes another convenience export layer around legacy platform-agent types.
- [x] Focused validation passed:
  - `cargo check --bin ploy`
- [x] Validation still shows only the same pre-existing unrelated warnings in `src/strategy/event_edge/strategy.rs`, `liquidity_vacuum_backtest.rs`, and `garch_probability_backtest.rs`.

---

# Layered Live Runtime Task 8E: Narrow RL Platform Agent Re-Exports (2026-03-06)

## Goal
Remove the feature-gated RL agent convenience path from the `platform` root as well, so the RL CLI explicitly depends on the `platform::agents::rl_crypto_agent` compatibility module and `platform::traits::DomainAgent`.

## Tasks

- [x] Stop re-exporting `RLCryptoAgent` and `RLCryptoAgentConfig` from `src/platform/mod.rs`.
- [x] Make `src/platform/agents/rl_crypto_agent.rs` an explicit public compatibility module.
- [x] Update `src/main_commands/rl/agent.rs` to import `RLCryptoAgent`, `RLCryptoAgentConfig`, and `DomainAgent` from explicit compatibility paths.
- [x] Run focused validation and capture the results with `--features rl`.

## Review

- [x] `ploy::platform::RLCryptoAgent` and `ploy::platform::RLCryptoAgentConfig` are no longer convenience APIs from the platform root.
- [x] This slice does not change RL runtime behavior; it only removes the last root-level platform-agent convenience export and makes the RL CLI name the compatibility surfaces it depends on.
- [x] Focused validation passed:
  - `cargo check --features rl --bin ploy`
- [x] Validation still shows only the same pre-existing unrelated warnings in `src/strategy/event_edge/strategy.rs`, `liquidity_vacuum_backtest.rs`, and `garch_probability_backtest.rs`.

---

# Layered Live Runtime Task 8F: Mark TradingAgent Spawn Path As Compatibility-Only (2026-03-06)

## Goal
Make the remaining pull-based runtime path read as an explicit compatibility path in bootstrap, not a neutral peer to the canonical managed strategy runtime.

## Tasks

- [x] Rename `spawn_trading_agent_task(...)` to `spawn_compat_trading_agent_task(...)` in `src/coordinator/bootstrap.rs`.
- [x] Update the remaining sports/lob-ml/rl-policy compatibility branches to call the explicit compatibility helper name.
- [x] Run focused validation and capture the results.

## Review

- [x] `bootstrap.rs` now labels the remaining pull-based runtime path as compatibility-only at the callsite and helper level.
- [x] This slice does not change runtime behavior; it only makes the surviving legacy path harder to mistake for a canonical extension point.
- [x] Focused validation passed:
  - `cargo test coordinator::bootstrap --lib -- --nocapture`
  - `cargo check --bin ploy`
- [x] Validation still shows only the same pre-existing unrelated warnings in `src/strategy/event_edge/strategy.rs`, `liquidity_vacuum_backtest.rs`, and `garch_probability_backtest.rs`.

---

# Layered Live Runtime Task 8G: Narrow Compatibility Agent Struct Re-Exports (2026-03-06)

## Goal
Stop presenting the remaining pull-based compatibility agent structs as root-level `agents` exports. Bootstrap should depend on their explicit compatibility modules.

## Tasks

- [x] Stop re-exporting `CryptoLobMlAgent` and `CryptoRlPolicyAgent` from `src/agents/mod.rs`.
- [x] Update `src/coordinator/bootstrap.rs` to import those agent structs from `agents::crypto_lob_ml` and `agents::crypto_rl_policy`.
- [x] Keep the corresponding config types re-exported from `agents` for now.
- [x] Run focused validation and capture the results.

## Review

- [x] `crate::agents::CryptoLobMlAgent` and `crate::agents::CryptoRlPolicyAgent` are no longer convenience APIs from the `agents` root.
- [x] This slice does not change runtime behavior; it only narrows another compatibility export layer around the remaining pull-based crypto agent implementations.
- [x] Focused validation passed:
  - `cargo test coordinator::bootstrap --lib -- --nocapture`
  - `cargo check --bin ploy`
- [x] Validation still shows only the same pre-existing unrelated warnings in `src/strategy/event_edge/strategy.rs`, `liquidity_vacuum_backtest.rs`, and `garch_probability_backtest.rs`.

---

# Layered Live Runtime Task 9J: Extract Compatibility Crypto Agent Spawn Helpers (2026-03-06)

## Goal
Reduce bootstrap assembly noise for the two remaining crypto pull-based compatibility runtimes by moving their agent construction and spawn wiring into dedicated helpers.

## Tasks

- [x] Add `spawn_compat_crypto_lob_ml_agent(...)` in `src/coordinator/bootstrap.rs`.
- [x] Add `spawn_compat_crypto_rl_policy_agent(...)` in `src/coordinator/bootstrap.rs` behind `#[cfg(feature = "rl")]`.
- [x] Replace the inline `lob_ml` and `rl_policy` construction/spawn blocks in `start_platform()` with those helpers.
- [x] Keep all compatibility gating semantics unchanged in this slice.
- [x] Run focused validation and capture the results.

## Review

- [x] `start_platform()` no longer inlines the compatibility crypto agent construction and spawn logic for `lob_ml` and `rl_policy`; it now reads closer to “check prerequisites -> call helper”.
- [x] This slice does not retire those runtimes yet; it only pushes their remaining bootstrap assembly into dedicated compatibility helpers.
- [x] Focused validation passed:
  - `cargo test coordinator::bootstrap --lib -- --nocapture`
  - `cargo check --features rl --bin ploy`
- [x] Validation still shows only the same pre-existing unrelated warnings in `src/strategy/event_edge/strategy.rs`, `liquidity_vacuum_backtest.rs`, and `garch_probability_backtest.rs`.

---

# Layered Live Runtime Task 9K: Extract Compatibility Crypto Spawn Gating Helpers (2026-03-06)

## Goal
Finish reducing bootstrap noise for the remaining crypto pull-based compatibility paths by moving their prerequisite checks and skip warnings into dedicated helpers as well.

## Tasks

- [x] Add `maybe_spawn_compat_crypto_lob_ml_agent(...)` in `src/coordinator/bootstrap.rs`.
- [x] Add `maybe_spawn_compat_crypto_rl_policy_agent(...)` in `src/coordinator/bootstrap.rs` behind `#[cfg(feature = "rl")]`.
- [x] Replace the inline gating/warning logic in `start_platform()` with those helpers.
- [x] Keep all launch semantics and warning messages unchanged in this slice.
- [x] Run focused validation and capture the results.

## Review

- [x] The crypto branch in `start_platform()` now reads closer to canonical assembly: enablement checks, then one helper call per surviving compatibility path.
- [x] This slice does not retire `lob_ml` or `rl_policy`; it only moves their remaining prerequisite logic out of the main bootstrap flow.
- [x] Focused validation passed:
  - `cargo test coordinator::bootstrap --lib -- --nocapture`
  - `cargo check --features rl --bin ploy`
- [x] Validation still shows only the same pre-existing unrelated warnings in `src/strategy/event_edge/strategy.rs`, `liquidity_vacuum_backtest.rs`, and `garch_probability_backtest.rs`.

---

# Layered Live Runtime Task 8H: Remove Remaining Agents Root Re-Exports (2026-03-06)

## Goal
Finish shrinking `agents/mod.rs` down to a pure module namespace so bootstrap and other callers must name the explicit compatibility module they depend on.

## Tasks

- [x] Remove the remaining config/agent `pub use` entries from `src/agents/mod.rs`.
- [x] Update `src/coordinator/bootstrap.rs` to import crypto, sports, politics, and openclaw types from explicit submodules.
- [x] Keep runtime behavior unchanged in this slice.
- [x] Run focused validation and capture the results.

## Review

- [x] `crate::agents` no longer re-exports compatibility runtime types or configs; callers now opt into explicit modules such as `agents::crypto`, `agents::sports`, and `agents::openclaw`.
- [x] This slice does not change runtime behavior; it only removes the last root-level convenience exports from the compatibility agent namespace.
- [x] Focused validation passed:
  - `cargo test coordinator::bootstrap --lib -- --nocapture`
  - `cargo check --features rl --bin ploy`
- [x] Validation still shows only the same pre-existing unrelated warnings in `src/strategy/event_edge/strategy.rs`, `liquidity_vacuum_backtest.rs`, and `garch_probability_backtest.rs`.

---

# Layered Live Runtime Task 8I: Gate Compatibility Crypto Live Runtimes (2026-03-06)

## Goal
Retire the surviving pull-based crypto live runtimes from the default platform startup path without deleting their code yet. They should require an explicit env gate for temporary compatibility use.

## Tasks

- [x] Add a `compat_crypto_runtimes_enabled()` helper in `src/coordinator/bootstrap.rs`.
- [x] Gate `lob_ml` and `rl_policy` startup behind `PLOY_ENABLE_COMPAT_CRYPTO_RUNTIMES`.
- [x] Keep the compatibility code paths intact for explicit opt-in use.
- [x] Add focused tests proving the gate defaults off and honors explicit env overrides.
- [x] Run focused validation and capture the results.

## Review

- [x] `crypto_lob_ml` and `crypto_rl_policy` no longer start by default even if their enable flags are set; they now require `PLOY_ENABLE_COMPAT_CRYPTO_RUNTIMES=true`.
- [x] This slice keeps the compatibility runtimes available for temporary fallback, but removes them from the default live startup surface.
- [x] Focused validation passed:
  - `cargo test coordinator::bootstrap --lib -- --nocapture`
  - `cargo check --features rl --bin ploy`
- [x] Validation still shows only the same pre-existing unrelated warnings in `src/strategy/event_edge/strategy.rs`, `liquidity_vacuum_backtest.rs`, and `garch_probability_backtest.rs`.

---

# Layered Live Runtime Task 9A: Managed Runtime Spawn Helper (2026-03-06)

## Goal
Reduce `bootstrap.rs` special-case assembly by extracting the repeated managed-runtime registration and spawn boilerplate into one helper used by the canonical strategy paths.

## Tasks

- [x] Add a shared `spawn_managed_strategy_runtime_task(...)` helper in `src/coordinator/bootstrap.rs`.
- [x] Move the repeated register-agent + shutdown subscription + `tokio::spawn` pattern for `momentum`, `pattern_memory`, `split_arb`, `event_edge`, and canonical `nba_comeback` into that helper.
- [x] Keep strategy-specific TOML/config builders untouched in this slice; only remove duplicated startup assembly.
- [x] Preserve the Grok-enabled sports fallback path outside the helper.
- [x] Run focused validation and capture the results.

## Review

- [x] Canonical runtime startup is now assembled through one bootstrap helper instead of five near-identical inline branches.
- [x] This slice does not change strategy selection or execution semantics; it only reduces bootstrap duplication before larger Task 9 cleanup.
- [x] Focused validation passed:
  - `cargo test coordinator::bootstrap --lib -- --nocapture`
  - `cargo check --lib`
- [x] Validation still shows only the same pre-existing unrelated warnings in `src/strategy/event_edge/strategy.rs`, `liquidity_vacuum_backtest.rs`, and `garch_probability_backtest.rs`.

---

# Layered Live Runtime Task 9B: TradingAgent Spawn Helper (2026-03-06)

## Goal
Further reduce bootstrap churn by extracting the repeated registration and `AgentContext` spawn boilerplate for the remaining compatibility `TradingAgent` paths.

## Tasks

- [x] Add a shared `spawn_trading_agent_task(...)` helper in `src/coordinator/bootstrap.rs`.
- [x] Move the repeated compatibility-loop startup code for the Grok-enabled sports fallback, `crypto_lob_ml`, and `crypto_rl_policy` into that helper.
- [x] Keep OpenClaw untouched in this slice because it uses `GovernanceContext`, not `AgentContext`.
- [x] Run focused validation and capture the results.

## Review

- [x] Remaining compatibility TradingAgent paths now share one bootstrap launcher instead of repeating `register_agent -> AgentContext::new -> tokio::spawn`.
- [x] This slice does not retire those runtimes yet; it only shrinks bootstrap duplication ahead of deeper runtime removal work.
- [x] Focused validation passed:
  - `cargo test coordinator::bootstrap --lib -- --nocapture`
  - `cargo check --lib`
- [x] Validation still shows only the same pre-existing unrelated warnings in `src/strategy/event_edge/strategy.rs`, `liquidity_vacuum_backtest.rs`, and `garch_probability_backtest.rs`.

---

# Layered Live Runtime Task 9C: Sports Data Support Helper (2026-03-06)

## Goal
Pull the sports-specific market-data/persistence assembly out of `start_platform()` so the sports branch focuses on runtime selection instead of inlining collector-target, WS, and persistence bridge setup.

## Tasks

- [x] Add `load_sports_collector_targets(...)` and `start_sports_market_data_support(...)` helpers in `src/coordinator/bootstrap.rs`.
- [x] Move the sports collector-target query, PM WS seeding/refresh, and quote/orderbook persistence bridge setup into those helpers.
- [x] Leave runtime selection unchanged in this slice: canonical `nba_comeback` and Grok-enabled fallback still behave the same after support setup completes.
- [x] Run focused validation and capture the results.

## Review

- [x] The sports branch in `start_platform()` is materially shorter and now reads as: build runtime spec, start sports market-data support, then choose canonical runtime or fallback.
- [x] This slice does not change sports execution semantics; it only removes a large domain-specific assembly blob from the main bootstrap flow.
- [x] Focused validation passed:
  - `cargo test coordinator::bootstrap --lib -- --nocapture`
  - `cargo check --lib`
- [x] Validation still shows only the same pre-existing unrelated warnings in `src/strategy/event_edge/strategy.rs`, `liquidity_vacuum_backtest.rs`, and `garch_probability_backtest.rs`.

---

# Layered Live Runtime Task 9D: OpenClaw Spawn Unification (2026-03-06)

## Goal
Remove the last hand-written `TradingAgent` startup template from `bootstrap.rs` by routing OpenClaw through the shared compatibility-agent spawn helper.

## Tasks

- [x] Switch the OpenClaw bootstrap branch to use `spawn_trading_agent_task(...)`.
- [x] Keep OpenClaw behavior unchanged in this slice; it still converts `AgentContext` into `GovernanceContext` internally.
- [x] Run focused validation and capture the results.

## Review

- [x] `bootstrap.rs` no longer hand-writes any `TradingAgent` registration + `AgentContext` + `tokio::spawn` sequence.
- [x] OpenClaw remains a governance-plane special case semantically, but its startup assembly now matches the other compatibility runtimes.
- [x] Focused validation passed:
  - `cargo test coordinator::bootstrap --lib -- --nocapture`
  - `cargo check --lib`
- [x] Validation still shows only the same pre-existing unrelated warnings in `src/strategy/event_edge/strategy.rs`, `liquidity_vacuum_backtest.rs`, and `garch_probability_backtest.rs`.

---

# Layered Live Runtime Task 9E: Remove Bootstrap Runtime Wrapper (2026-03-06)

## Goal
Drop the extra `bootstrap.rs` wrapper around `run_managed_strategy_runtime_module(...)` so bootstrap constructs `ManagedStrategyRuntimeConfig` directly and stays one layer thinner.

## Tasks

- [x] Delete the local `run_managed_strategy_runtime(...)` wrapper from `src/coordinator/bootstrap.rs`.
- [x] Update `spawn_managed_strategy_runtime_task(...)` to call `run_managed_strategy_runtime_module(...)` with `ManagedStrategyRuntimeConfig` directly.
- [x] Remove any now-unused imports introduced by deleting the wrapper.
- [x] Run focused validation and capture the results.

## Review

- [x] `bootstrap.rs` no longer contains an unnecessary async pass-through around the strategy-runtime module.
- [x] This slice does not change runtime behavior; it only reduces one more layer of bootstrap indirection.
- [x] Focused validation passed:
  - `cargo test coordinator::bootstrap --lib -- --nocapture`
  - `cargo check --lib`
- [x] Validation still shows only the same pre-existing unrelated warnings in `src/strategy/event_edge/strategy.rs`, `liquidity_vacuum_backtest.rs`, and `garch_probability_backtest.rs`.

---

# Layered Live Runtime Task 9F: Spawn Canonical Runtime From Bootstrap Specs (2026-03-06)

## Goal
Stop having each canonical strategy branch manually unwrap `pm_client` and re-thread the same spawn arguments. Bootstrap should hand a `ManagedStrategyBootstrapSpec` to one helper and stay closer to pure assembly.

## Tasks

- [x] Add `spawn_managed_strategy_runtime_spec(...)` in `src/coordinator/bootstrap.rs`.
- [x] Route the managed `momentum`, `pattern_memory`, and `split_arb` crypto branches through the spec helper.
- [x] Route canonical `nba_comeback` and `event_edge` through the same spec helper.
- [x] Preserve all existing strategy selection and legacy fallback behavior in this slice.
- [x] Run focused validation and capture the results.

## Review

- [x] Canonical strategy startup now has one bootstrap entrypoint for “spec + risk params + wiring” instead of five near-identical inline spawn sequences.
- [x] This slice does not change which strategies are canonical or when legacy fallbacks still apply; it only reduces more bootstrap-specific assembly duplication.
- [x] Focused validation passed:
  - `cargo test coordinator::bootstrap --lib -- --nocapture`
  - `cargo check --lib`
- [x] Validation still shows only the same pre-existing unrelated warnings in `src/strategy/event_edge/strategy.rs`, `liquidity_vacuum_backtest.rs`, and `garch_probability_backtest.rs`.

---

# Layered Live Runtime Task 9G: Extract Sports Legacy Fallback Helper (2026-03-06)

## Goal
Move the `nba_comeback` legacy sports fallback assembly out of the main sports branch so `start_platform()` mostly selects between canonical runtime and compatibility fallback instead of inlining Grok and PM-observation setup.

## Tasks

- [x] Add `spawn_legacy_nba_comeback_agent(...)` in `src/coordinator/bootstrap.rs`.
- [x] Move the `SportsTradingAgent` fallback assembly, PM sports client wiring, and optional Grok attachment into that helper.
- [x] Keep fallback selection semantics unchanged: only the non-canonical `nba_comeback` path uses the helper.
- [x] Run focused validation and capture the results.

## Review

- [x] The sports branch in `start_platform()` now reads as: build runtime spec, start sports data support, then choose canonical runtime or a single fallback helper.
- [x] This slice does not change when the legacy sports agent still runs; it only removes another large domain-specific assembly block from bootstrap.
- [x] Focused validation passed:
  - `cargo test coordinator::bootstrap --lib -- --nocapture`
  - `cargo check --lib`
- [x] Validation still shows only the same pre-existing unrelated warnings in `src/strategy/event_edge/strategy.rs`, `liquidity_vacuum_backtest.rs`, and `garch_probability_backtest.rs`.

---

# Layered Live Runtime Task 9H: Promote Runtime Config Builders To Spec Builders (2026-03-06)

## Goal
Move one more layer of strategy-specific assembly out of `start_platform()` by promoting canonical runtime config builders into `ManagedStrategyBootstrapSpec` builders for the remaining crypto and politics managed paths.

## Tasks

- [x] Add managed-runtime spec builders for `momentum`, `pattern_memory`, `split_arb`, and `event_edge`.
- [x] Update the corresponding `start_platform()` branches to consume those builders instead of hand-assembling `ManagedStrategyBootstrapSpec` inline.
- [x] Add focused unit tests covering the new momentum/event-edge/split-arb spec builders.
- [x] Run focused validation and capture the results.

## Review

- [x] `start_platform()` no longer hand-writes canonical runtime specs for momentum, pattern-memory, split-arb, or event-edge; it now asks dedicated builders for those specs.
- [x] This slice does not change strategy routing semantics; it only pushes strategy-specific spec assembly out of the main bootstrap flow.
- [x] Focused validation passed:
  - `cargo test coordinator::bootstrap --lib -- --nocapture`
  - `cargo check --lib`
- [x] Bootstrap-focused unit coverage increased from 18 to 21 tests via direct canonical-launch assertions for new spec builders.
- [x] Validation still shows only the same pre-existing unrelated warnings in `src/strategy/event_edge/strategy.rs`, `liquidity_vacuum_backtest.rs`, and `garch_probability_backtest.rs`.

---

# Layered Live Runtime Task 9I: Extract OpenClaw Bootstrap Helper (2026-03-06)

## Goal
Move the remaining OpenClaw setup block out of `start_platform()` so bootstrap stops inlining dedicated regime-feed startup and compatibility-agent wiring for the governance path.

## Tasks

- [x] Add `spawn_openclaw_agent(...)` in `src/coordinator/bootstrap.rs`.
- [x] Move OpenClaw Binance WS creation, freshness wiring, zero-risk registration defaults, and agent spawn into that helper.
- [x] Keep OpenClaw behavior unchanged in this slice; it still uses the compatibility trading-agent path under the hood.
- [x] Run focused validation and capture the results.

## Review

- [x] The OpenClaw branch in `start_platform()` is now reduced to an enablement check plus one helper call.
- [x] This slice does not change OpenClaw governance semantics; it only removes another domain-specific startup blob from bootstrap.
- [x] Focused validation passed:
  - `cargo test coordinator::bootstrap --lib -- --nocapture`
  - `cargo check --lib`
- [x] Validation still shows only the same pre-existing unrelated warnings in `src/strategy/event_edge/strategy.rs`, `liquidity_vacuum_backtest.rs`, and `garch_probability_backtest.rs`.

---

# Layered Live Runtime Task 4A: Introduce GovernanceAgent Contract (2026-03-06)

## Goal
Stop forcing governance-only logic through the `TradingAgent` interface. OpenClaw should run through a formal governance contract and bootstrap should spawn it with governance-only coordinator access.

## Tasks

- [x] Add a `GovernanceAgent` trait in `src/agents/traits.rs`.
- [x] Switch `OpenClawAgent` from `TradingAgent` to `GovernanceAgent`.
- [x] Add `spawn_governance_agent_task(...)` in `src/coordinator/bootstrap.rs` and route OpenClaw through it.
- [x] Keep compatibility trading agents unchanged in this slice.
- [x] Run focused validation and capture the results.

## Review

- [x] OpenClaw no longer implements the pull-based trading-agent contract; it now runs behind a dedicated governance-agent trait with `GovernanceContext`.
- [x] `bootstrap.rs` now distinguishes compatibility trading-agent spawning from governance-agent spawning instead of forcing both through the same helper.
- [x] Focused validation passed:
  - `cargo test agents::governance_context --lib -- --nocapture`
  - `cargo test coordinator::bootstrap --lib -- --nocapture`
  - `cargo check --lib`
- [x] Validation still shows only the same pre-existing unrelated warnings in `src/strategy/event_edge/strategy.rs`, `liquidity_vacuum_backtest.rs`, and `garch_probability_backtest.rs`.

---

# Layered Live Runtime Task 4B: Construct GovernanceContext Directly (2026-03-06)

## Goal
Finish separating governance-only bootstrap wiring from trading-agent wiring by letting governance paths construct `GovernanceContext` directly instead of first creating a trading context and then narrowing it.

## Tasks

- [x] Add a direct `GovernanceContext::new(...)` constructor.
- [x] Update `spawn_governance_agent_task(...)` to construct `GovernanceContext` directly.
- [x] Update governance-context tests to use the direct constructor.
- [x] Keep the `From<AgentContext>` conversion available for transitional compatibility.
- [x] Run focused validation and capture the results.

## Review

- [x] Governance-only bootstrap wiring no longer instantiates a trading context just to discard order-ingress capability immediately after.
- [x] This slice does not change governance behavior; it only makes the context boundary explicit in code construction.
- [x] Focused validation passed:
  - `cargo test agents::governance_context --lib -- --nocapture`
  - `cargo test coordinator::bootstrap --lib -- --nocapture`
  - `cargo check --lib`
- [x] Validation still shows only the same pre-existing unrelated warnings in `src/strategy/event_edge/strategy.rs`, `liquidity_vacuum_backtest.rs`, and `garch_probability_backtest.rs`.

---

# Layered Live Runtime Task 5A: Add Governance-Only Risk Defaults (2026-03-06)

## Goal
Make governance-agent risk binding explicit instead of leaving zero-risk registration values as an inline bootstrap literal.

## Tasks

- [x] Add `AgentRiskParams::governance_only()` in `src/platform/traits.rs`.
- [x] Route OpenClaw bootstrap registration through that helper.
- [x] Add a focused unit test covering the governance-only preset.
- [x] Run focused validation and capture the results.

## Review

- [x] Governance-only coordinator registration now has an explicit policy preset instead of a bootstrap-local zeroed struct literal.
- [x] This slice does not change runtime behavior; it only makes governance risk binding explicit and reusable.
- [x] Focused validation passed:
  - `cargo test platform::traits --lib -- --nocapture`
  - `cargo test coordinator::bootstrap --lib -- --nocapture`
  - `cargo check --lib`
- [x] Validation still shows only the same pre-existing unrelated warnings in `src/strategy/event_edge/strategy.rs`, `liquidity_vacuum_backtest.rs`, and `garch_probability_backtest.rs`.

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

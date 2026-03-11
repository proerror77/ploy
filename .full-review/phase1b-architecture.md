# Phase 1b — Architecture & Design Review

**Branch:** `hotfix/staggered-arb-release-20260306` vs `main`
**Scope:** Coordinator decomposition, bootstrap decomposition, new strategies, control plane
**Date:** 2026-03-11

---

## Executive Summary

The decomposition is architecturally sound. The monolithic `bootstrap.rs` (~7,000 lines) and `coordinator.rs` (~6,500 lines) have been split into well-named, cohesive sub-modules with clear responsibilities. Module boundaries are clean and the dependency direction is correct (bootstrap → coordinator → capital/risk/position/queue). The main architectural concerns are: (1) the coordinator is accumulating too many direct field references making it a structural God Object even if the code is split across files, (2) the foreground execution path bypasses the coordinator entirely creating a dual-path risk model, and (3) the capital accounting layer has a subtle correctness gap in how it handles sell-side exposure.

---

## Critical

### A-C1 — Coordinator struct has 12 direct fields — structural God Object risk
**Files:** `src/coordinator/coordinator.rs`, `src/coordinator/coordinator/ingress.rs`
**Severity:** Critical (architectural)

The `Coordinator` struct holds direct `Arc<RwLock<_>>` references to: `admission`, `capital`, `governance`, `journal`, `order_queue`, `positions`, `risk`, `strategy_runtime`, `executor`, `state`, `shutdown_rx`, and `event_tx`. Every method in `ingress.rs`, `execution.rs`, `recovery.rs`, `control_surface.rs` accesses these fields directly. While the code is now split across files via `impl Coordinator` blocks in sub-modules, the struct itself is still a God Object — it owns everything and every sub-module can reach everything else through `self`.

The decomposition moved code into files but did not create true encapsulation. `ingress.rs` can directly access `self.capital`, `self.positions`, `self.risk`, `self.governance`, `self.journal`, and `self.order_queue` in a single function. This means any change to any sub-system's interface requires touching `ingress.rs`.

**Recommendation:** Introduce a `CoordinatorCore` struct that groups the shared state, and pass it as a parameter to sub-system methods rather than having everything on `self`. Alternatively, define a `CoordinatorIngress` trait that only exposes what ingress needs, breaking the direct field coupling.

---

### A-C2 — Foreground execution path bypasses all coordinator risk controls
**Files:** `src/cli/strategy/runtime_ops/foreground.rs`, `src/cli/strategy/runtime_ops/foreground_submit.rs`
**Severity:** Critical (correctness)

`foreground.rs` implements its own `handle_strategy_actions` loop that calls `OrderExecutor` directly. This path bypasses:
- `AdmissionController` (deployment gate, duplicate guard, Kelly sizing)
- `RiskGate` (exposure limits, circuit breaker, daily loss limit)
- `PositionAggregator` (position tracking)
- `CapitalAllocator` (per-deployment exposure tracking)
- `Journal` (risk decision audit trail)

The commit message says "cli: route foreground intents through coordinator ingress" but the implementation only routes managed runtime intents through the coordinator. Foreground runs (`ploy strategy run --foreground`) still use the direct executor path.

If foreground is intended as a dev/test mode only, this must be documented and enforced (e.g. `--dry-run` required for foreground). If foreground is used in production (e.g. for manual intervention), this is a correctness gap where live orders bypass all risk controls.

**Recommendation:** Either (a) route foreground through `CoordinatorHandle::submit_intent()` like managed runtime does, or (b) add a prominent `warn!` at startup and require `--dry-run` for foreground mode.

---

## High

### A-H1 — `pending_buy_notional_excluding_domains` hardcodes all known domains
**File:** `src/coordinator/coordinator/ingress.rs` lines 343–348
**Severity:** High

```rust
.pending_buy_notional_excluding_domains(&[
    Domain::Crypto,
    Domain::Sports,
    Domain::Politics,
    Domain::Economics,
])
```

This call excludes ALL known domains from the notional calculation, making the result always zero. The intent appears to be "calculate notional for the current intent's domain only" but the implementation excludes everything. If a new domain is added, it must be added to this exclusion list or the calculation silently changes behavior.

**Recommendation:** Replace with `pending_buy_notional_for_domain(intent.domain)` — a method that calculates notional for a specific domain rather than excluding all others.

---

### A-H2 — `MarketCapitalAllocator` tracks pending exposure but not confirmed fills
**File:** `src/coordinator/capital/market.rs`, `src/coordinator/capital/market/accounting.rs`
**Severity:** High

`MarketCapitalAllocator` tracks `pending_exposure` (intents in queue) and `open_exposure` (submitted orders). When an order fills, `record_fill` is called which removes from `open_exposure`. However, there is no tracking of realized positions — once an order fills, the capital allocator has no record of the position. This means:

1. A strategy can submit a BUY, it fills, and the capital allocator shows zero exposure for that market.
2. The strategy can immediately submit another BUY for the same market at full size.
3. The `PositionAggregator` tracks the position, but the `CapitalAllocator` does not cross-reference it.

The two systems (`PositionAggregator` and `CapitalAllocator`) track overlapping but inconsistent views of exposure.

**Recommendation:** After a fill, `record_fill` should add the filled notional to a `realized_exposure` bucket that is only released when a corresponding SELL fills. The risk check in `ingress.rs` should sum `pending + open + realized` exposure.

---

### A-H3 — Bootstrap initialization order is implicit and fragile
**File:** `src/coordinator/bootstrap/startup_context.rs`, `src/coordinator/bootstrap/coordinator_bootstrap.rs`, `src/coordinator/bootstrap/runtime_orchestration.rs`
**Severity:** High

`start_platform` calls three functions in sequence:
1. `initialize_startup_context` — creates exchange client, PM client, pool
2. `initialize_coordinator_runtime` — creates coordinator, executor, idempotency
3. `run_platform_runtime` — spawns agents, data planes, strategy runtimes

The ordering is correct but entirely implicit. If a future developer adds a new initialization step that depends on the coordinator being ready (e.g. registering a new agent type), they must know to add it in step 3, not step 2. There is no compile-time or runtime enforcement of this ordering.

**Recommendation:** Use a typestate pattern or builder pattern to make initialization order explicit:
```rust
let startup = StartupContext::new(config, app_config).await?;
let coordinator = startup.initialize_coordinator().await?;
let runtime = coordinator.initialize_runtime().await?;
runtime.run().await
```
Each step returns a typed handle that is required by the next step, making incorrect ordering a compile error.

---

### A-H4 — `GovernanceController` is not persisted on startup — governance state lost on restart
**File:** `src/coordinator/governance.rs`, `src/coordinator/bootstrap/coordinator_bootstrap.rs`
**Severity:** High

`GovernanceController::new()` initializes with default state (all domains running, no blocked domains). The `persist_governance_policy` method writes to the DB, and `load_governance_policy` reads from it. However, `initialize_coordinator_runtime` does not call `load_governance_policy` — it creates a fresh `GovernanceController` with defaults.

If an operator pauses a domain via the API, restarts the process, the domain is automatically unpaused. This is a correctness gap for operational governance.

**Recommendation:** In `initialize_coordinator_runtime`, after creating the `GovernanceController`, call `load_governance_policy` to restore persisted state. Add a startup log line indicating whether governance state was restored or initialized fresh.

---

### A-H5 — `strategy_runtime` module has two separate action dispatch paths
**File:** `src/coordinator/strategy_runtime/actions.rs`, `src/coordinator/strategy_runtime/session.rs`
**Severity:** High

`ManagedRuntimeSession` has a `handle_strategy_actions_runtime` loop that dispatches `StrategyAction` variants. Separately, `run_managed_strategy_runtime` in `strategy_runtime.rs` has its own action handling. The two paths handle overlapping action types (`SubmitIntent`, `CancelOrder`, `ModifyOrder`) with slightly different implementations. This creates a maintenance hazard where a fix to one path may not be applied to the other.

**Recommendation:** Consolidate into a single `ActionDispatcher` struct that both paths use. The session-level loop and the top-level runtime loop should call the same dispatcher.

---

## Medium

### A-M1 — `control_plane` module has no public API documentation
**File:** `src/control_plane.rs` (or `src/control_plane/mod.rs`)
**Severity:** Medium

The new `control_plane` module is referenced from `admission.rs` and `bootstrap.rs` but has no module-level documentation explaining its role in the architecture. Given that `StrategyDeployment` is a central concept (used in admission, capital, bootstrap), the control plane's relationship to the coordinator should be documented.

---

### A-M2 — `journal` module mixes persistence concerns with restore logic
**File:** `src/coordinator/journal.rs`, `src/coordinator/journal/restore.rs`
**Severity:** Medium

`journal.rs` handles write-path persistence (persisting risk decisions, order updates, governance policies). `journal/restore.rs` handles read-path recovery (loading fills, outcomes, governance state). These are distinct concerns — the write path is hot (called on every order) while the restore path is cold (called once at startup). Mixing them in the same module makes it harder to reason about the journal's performance characteristics.

**Recommendation:** Consider splitting into `journal/writer.rs` (hot path) and `journal/restore.rs` (cold path, already exists). The `Journal` struct should only expose write methods; restore functions should be free functions called directly from `recovery.rs`.

---

### A-M3 — `capital` module has two separate allocator types with overlapping responsibilities
**File:** `src/coordinator/capital/market.rs`, `src/coordinator/capital/crypto.rs`
**Severity:** Medium

`MarketCapitalAllocator` (for PM markets) and `CryptoCapitalAllocator` (for Binance) have similar structures but separate implementations. Both track pending/open exposure, both have `check_intent` methods, both have `record_fill` methods. The duplication means any fix to exposure tracking logic must be applied twice.

**Recommendation:** Extract a generic `ExposureBook<K>` trait or struct that both allocators use internally, with domain-specific logic only in the wrapper.

---

### A-M4 — `admission/deployments.rs` uses `pub(in crate::coordinator)` visibility inconsistently
**File:** `src/coordinator/admission/deployments.rs`
**Severity:** Medium (low impact)

Some functions use `pub(super)`, some use `pub(in crate::coordinator)`, and some are `pub(crate)`. The visibility levels are inconsistent and some are more permissive than needed. `buy_intent_missing_deployment_reason` is `pub(in crate::coordinator)` but is only called from `ingress.rs` — `pub(super)` would be sufficient.

---

## Low

### A-L1 — `bootstrap/support.rs` env helpers are duplicated in `strategy_runtime/actions.rs`
**File:** `src/coordinator/bootstrap/support.rs`, `src/coordinator/strategy_runtime/actions.rs`
**Severity:** Low

`env_u64`, `env_bool`, `env_decimal_opt` are defined in `bootstrap/support.rs` but a separate `env_u64` is defined in `strategy_runtime/actions.rs`. These should share a single implementation.

---

### A-L2 — `coordinator/state.rs` is nearly empty after decomposition
**File:** `src/coordinator/state.rs`
**Severity:** Low

`state.rs` now contains only `AgentSnapshot`, `GlobalState`, and `QueueStatsSnapshot`. `GlobalState` is a thin wrapper around `HashMap<String, AgentSnapshot>`. After the decomposition, most state has moved to dedicated modules (`position.rs`, `risk.rs`, `queue.rs`). Consider whether `state.rs` still needs to exist or whether its remaining types should move to `command.rs` (where `AgentSnapshot` is already re-exported alongside other command types).

---

## Architecture Strengths

1. **Clean module boundaries**: Each sub-module (`admission`, `capital`, `journal`, `position`, `queue`, `risk`, `strategy_runtime`, `governance`) has a single clear responsibility. The `pub(super)` / `pub(in crate::coordinator)` visibility annotations are used correctly to enforce encapsulation.

2. **Bootstrap decomposition is excellent**: `start_platform` is now 35 lines. Each initialization phase is a named function with a clear return type. The `StartupContext` struct cleanly groups the outputs of phase 1.

3. **Dependency direction is correct**: `bootstrap` → `coordinator` → `capital/risk/position/queue`. No upward dependencies. `strategy_runtime` correctly depends on `coordinator` via `CoordinatorHandle` (not direct struct access).

4. **`CoordinatorHandle` is the right abstraction**: Strategies interact with the coordinator only through `CoordinatorHandle` (a `Clone`-able channel-based handle). This prevents strategies from directly accessing coordinator internals and enables future coordinator replacement without changing strategy code.

5. **`AdmissionController` is well-designed**: The separation of `duplicate_guard`, `deployments`, and Kelly sizing into a dedicated admission layer is a good pattern. The admission controller is the right place for pre-queue validation.

---

## Summary

| Severity | Count |
|----------|-------|
| Critical | 2 |
| High     | 5 |
| Medium   | 4 |
| Low      | 2 |
| **Total**| **13** |

### Critical Issues for Phase 2 Context

- **A-C2** (foreground bypass): Security/risk implication — live orders can bypass all risk controls via foreground mode
- **A-H2** (capital allocator gap): Performance/correctness — exposure tracking is incomplete after fills, enabling over-exposure
- **A-H4** (governance not restored): Operational correctness — governance state lost on restart enables unintended trading after operator pause
- **A-C1** (coordinator God Object): Maintainability — 12-field struct makes lock ordering analysis difficult, increasing deadlock risk

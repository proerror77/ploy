# Phase 1a — Code Quality Review

**Branch:** `hotfix/staggered-arb-release-20260306` vs `main`
**Scope:** Coordinator decomposition, staggered-arb-live strategy, CLI routing
**Files examined:** 24 key files
**Date:** 2026-03-11

---

## Critical

### C1 — Nested write-lock acquisition in `close_position` (lock-ordering hazard)
**File:** `src/coordinator/position/transitions.rs` lines 52–72
**Confidence:** 90

`close_position` acquires a write lock on `self.positions`, then — while still holding it — acquires a write lock on `self.realized_pnl`. Both are `Arc<RwLock<_>>` fields on the same struct. The sibling method `reduce_position` correctly calls `drop(positions)` before acquiring `realized_pnl` (line 110). `close_position` does not. Any future code path that acquires `realized_pnl` first and then `positions` will deadlock. The pattern is already inconsistent within the same file.

**Fix:** Mirror `reduce_position`'s pattern — extract the needed data from the position, drop the positions lock, then update `realized_pnl`:
```rust
let removed = {
    let mut positions = self.positions.write().await;
    positions.remove(position_id)
};
if let Some(position) = removed {
    let pnl = (exit_price - position.entry_price) * Decimal::from(position.shares);
    *self.realized_pnl.write().await
        .entry(position.agent_id.clone())
        .or_insert(Decimal::ZERO) += pnl;
    Some(pnl)
} else {
    None
}
```

---

### C2 — `record_loss` does not reset `consecutive_failures` or increment daily order counters
**File:** `src/coordinator/risk/transitions.rs` lines 107–137
**Confidence:** 85

`record_loss` subtracts from PnL and may trigger the circuit breaker, but it never resets `consecutive_failures` and never increments `daily.order_count` or `daily.success_count`. A filled SELL order that realizes a loss is a successful execution — it should reset the failure streak and count toward the daily order total. As written, a sequence of: 3 failures → circuit breaker → reset → profitable BUY fills → SELL fills at a loss will leave `consecutive_failures` at 0 (correct), but the daily order count will be understated by the number of loss-realizing sells. More critically, if `record_loss` is called without a preceding `record_success`, the failure counter is not reset, so the system may stay in `Elevated` state indefinitely after a profitable trade that happened to realize a loss on exit.

**Fix:** Add `self.consecutive_failures.store(0, Ordering::SeqCst)` and increment `daily.order_count` / `daily.success_count` in `record_loss`, matching the pattern in `record_success`.

---

### C3 — `restore_runtime_state_from_execution_log` replays ALL historical fills with no time window
**File:** `src/coordinator/coordinator/recovery.rs` lines 77–231 and `src/coordinator/journal/restore.rs` lines 164–213
**Confidence:** 87

`load_execution_log_fills` fetches every row from `agent_order_executions` where `filled_shares > 0` with no date filter. On a long-running system this replays months of fills into `PositionAggregator`, creating phantom open positions for long-closed trades. The subsequent `apply_sell_fill_to_positions` calls will log "sell fill exceeded tracked position shares" for every historical sell that has no matching open position, and the capital allocator will be seeded with stale exposure data. The `outcomes_today` query correctly applies a `window_start`/`window_end` filter; `fills` does not.

**Fix:** Add a configurable lookback window to `load_execution_log_fills` (e.g. `WHERE executed_at >= $3` with a 7-day default), or filter to only fills where the position is still open. At minimum, add a `LIMIT` clause to prevent unbounded result sets.

---

## High

### H1 — `try_entry_for_window` is a 250-line function with ~25 early-return branches
**File:** `src/strategy/staggered_arb_live/entry.rs` lines 76–553
**Confidence:** 92

`try_entry_for_window` has approximately 25 distinct guard clauses, two major code paths (dry-run vs live), and inline share-sizing logic. Cyclomatic complexity exceeds 20. The dry-run and live paths share all entry-validation logic but diverge completely at line 453, meaning any change to entry criteria must be applied in two places. This function is the core trading decision point for the new live strategy and is the highest-risk function in the diff.

**Fix:** Extract entry validation into a pure function `validate_entry(adapter, symbol, ts, window, ...) -> Result<EntryParams, &'static str>` that returns either the computed entry parameters or a reject reason. The dry-run and live dispatch functions become thin wrappers that call the validator and act on the result. This also makes unit testing the validation logic trivial without needing a full adapter instance.

---

### H2 — `handle_order_intent` repeats a 4-step rejection pattern 9 times
**File:** `src/coordinator/coordinator/ingress.rs` lines 6–328
**Confidence:** 90

Every rejection path in `handle_order_intent` repeats:
1. `self.journal.persist_risk_decision(..., "BLOCKED", ...).await`
2. `self.emit_rejected_intent_update(..., OrderStatus::Rejected, ...).await`
3. `warn!(...)`
4. `return`

This pattern appears 9 times verbatim. Any change to the rejection flow (adding a metric, changing log level, adding an alert) must be applied in 9 places. This is a DRY violation in the hottest path of the coordinator.

**Fix:** Extract a `reject_intent(&self, intent: &OrderIntent, reason: String, label: &str)` async method on `Coordinator`. Each guard clause becomes: `return self.reject_intent(&intent, reason, "domain_blocked").await;`

---

### H3 — `pop_lowest_priority` rebuilds the entire heap on every full-queue enqueue
**File:** `src/coordinator/queue.rs` lines 128–149
**Confidence:** 85

When the queue is full and a higher-priority intent arrives, `pop_lowest_priority` calls `std::mem::take(&mut self.heap).into_vec()`, does a linear scan, then calls `BinaryHeap::from(items)` to rebuild — O(n) scan + O(n) heap construction per enqueue. For a queue of size 1000 with frequent high-priority arrivals this is O(n) per enqueue instead of O(log n).

**Fix:** Maintain a secondary `BinaryHeap` ordered in reverse (min-heap by priority) for O(log n) eviction, or use a `BTreeMap<(priority_u8, sequence), OrderIntent>` which supports O(log n) min/max removal natively.

---

### H4 — `spawn_split_arb_poll_task` spawns unbounded concurrent polling tasks per order
**File:** `src/coordinator/strategy_runtime/actions.rs` lines 564–677
**Confidence:** 88

`spawn_split_arb_poll_task` is called from `handle_runtime_order_update` every time a `Submitted` or `PartiallyFilled` update arrives. Each call spawns a new `tokio::spawn` task that polls for up to 600 seconds. If the same order receives multiple `Submitted` updates (WS reconnects, duplicate delivery), multiple polling tasks run concurrently for the same order, each sending duplicate `OrderUpdate` messages to the strategy manager. There is no deduplication or task handle tracking.

**Fix:** Track active poll tasks by `exchange_order_id` in a `HashMap<String, JoinHandle<()>>` (passed into the action handler). Before spawning, abort any existing task for the same order: `if let Some(h) = active_polls.remove(&id) { h.abort(); }`.

---

### H5 — `record_success` has a TOCTOU race on state transition from `Elevated` to `Normal`
**File:** `src/coordinator/risk/transitions.rs` lines 68–71
**Confidence:** 80

```rust
if *self.state.read().await == PlatformRiskState::Elevated {
    *self.state.write().await = PlatformRiskState::Normal;
```
The read lock is dropped before the write lock is acquired. Between the two acquisitions, another task could set the state to `Halted` (e.g. via `trigger_circuit_breaker`). The subsequent write then unconditionally overwrites `Halted` with `Normal`, clearing the circuit breaker silently.

**Fix:** Use a single write lock for the check-and-set:
```rust
let mut state = self.state.write().await;
if *state == PlatformRiskState::Elevated {
    *state = PlatformRiskState::Normal;
    info!("Risk state normalized after successful execution");
}
```

---

### H6 — `foreground.rs` silently falls back to dry-run on live authentication failure
**File:** `src/cli/strategy/runtime_ops/foreground.rs` lines 66–74
**Confidence:** 85

When `PolymarketClient::new_authenticated` fails in a non-dry-run context, the code prints a warning and falls back to a dry-run client. A misconfigured live deployment (wrong private key, expired API key) will silently run in observation mode rather than failing fast. The operator sees a yellow terminal warning but the process continues running for hours without placing orders.

**Fix:** In a live context, authentication failure should be a hard error:
```rust
Err(e) => {
    if !dry_run {
        return Err(anyhow::anyhow!("Live authentication failed: {}", e));
    }
    // dry-run fallback only when explicitly requested
    ...
}
```

---

### H7 — `deployment_gate_required()` reads an env var on every BUY intent
**File:** `src/coordinator/admission/deployments.rs` lines 22–32
**Confidence:** 82

`deployment_gate_required()` calls `std::env::var(...)` on every invocation. This function is called for every BUY intent in `enforce_live_buy_deployment_gate`. `std::env::var` acquires a global environment lock on some platforms and is not free.

**Fix:** Cache the result at startup using `std::sync::OnceLock<bool>`, or read it once in `AdmissionController::new` and store as a field.

---

### H8 — `apply_kelly_sizing` silently skips sizing when signal metadata is absent
**File:** `src/coordinator/admission.rs` lines 100–104
**Confidence:** 83

When neither `signal_fair_value` nor `signal_win_prob` is present in intent metadata, `apply_kelly_sizing` returns `None` (no rejection, no sizing). A strategy that accidentally uses the wrong metadata key (e.g. `signal_fair_val` instead of `signal_fair_value`) will have Kelly sizing silently disabled with no warning. In a live trading system this is a silent misconfiguration risk.

**Fix:** Add a `debug!` log when Kelly sizing is skipped due to missing metadata. Optionally add a config flag `kelly_require_signal_metadata: bool` that converts the silent skip into a block for strategies that are expected to always provide a fair value.

---

## Medium

### M1 — `position_counter` uses `RwLock<u64>` where `AtomicU64` suffices
**File:** `src/coordinator/position.rs` line 154, `transitions.rs` lines 17–21
**Confidence:** 85

`position_counter` is an `Arc<RwLock<u64>>` that is only ever incremented by one. This acquires a write lock on every `open_position` call. An `AtomicU64::fetch_add(1, Ordering::Relaxed)` is correct, cheaper, and simpler.

---

### M2 — Dead branch in `string_metadata_from_json`
**File:** `src/coordinator/journal/restore.rs` lines 143–151
**Confidence:** 88

```rust
if let Some(value) = value.as_str() {
    metadata.insert(key.clone(), value.to_string());
} else {
    metadata.insert(key.clone(), value.to_string()); // identical to the if branch
}
```
Both branches call `.to_string()` on the `serde_json::Value`. The `if let` is dead code. The intent was presumably to avoid JSON-quoting string values (e.g. `"foo"` instead of `foo`), but `serde_json::Value::to_string()` on a `Value::String` produces the quoted form. The correct fix is `value.as_str().map(str::to_string).unwrap_or_else(|| value.to_string())`.

---

### M3 — `GovernanceController::set_global_mode` unconditionally clears all domain modes
**File:** `src/coordinator/governance.rs` lines 216–219
**Confidence:** 82

Setting the global mode to any value clears all per-domain overrides. If an operator had set `Domain::Sports` to `Halted` and then sets the global mode to `Running`, the Sports halt is silently lost. The semantics are surprising and undocumented.

**Fix:** Only clear domain modes when transitioning to `Running`, or document the behavior explicitly and add a warning log when domain modes are cleared.

---

### M4 — `try_entry_for_window` clones `backtest_config` on every call
**File:** `src/strategy/staggered_arb_live/entry.rs` line 86
**Confidence:** 80

`let bc = adapter.config.backtest_config.clone()` clones the entire config struct on every entry attempt for every window for every symbol tick. The config is read-only within this function.

**Fix:** `let bc = &adapter.config.backtest_config;` — the borrow checker will enforce correctness.

---

### M5 — `#[allow(dead_code)]` on live trading position structs
**File:** `src/strategy/staggered_arb_live/lifecycle.rs` lines 44, 62
**Confidence:** 80

`PaperTrade` and `LiveOrderTrack` are annotated with `#[allow(dead_code)]`. Dead fields on position-tracking structs in a live trading system are a maintenance hazard — they may represent data intended for risk management or reporting that was never wired up.

**Fix:** Remove unused fields or wire them into the observability path. If reserved for future use, add a comment explaining the intent.

---

### M6 — `handle_strategy_actions_runtime` loop exits via `abort()` without draining in-flight actions
**File:** `src/coordinator/strategy_runtime/session.rs` lines 119–131 and `actions.rs` lines 56–171
**Confidence:** 82

`ManagedRuntimeSession::shutdown` calls `self.action_task.abort()` rather than waiting for the task to drain. An in-flight `SubmitIntent` that is mid-flight to the coordinator may be aborted without completing, potentially leaving the coordinator's duplicate-guard in an inconsistent state (the intent was registered in the guard but never enqueued).

**Fix:** Add a graceful shutdown signal (e.g. a `CancellationToken` or a dedicated `oneshot` channel) that causes the loop to drain remaining messages before exiting. Follow the abort with `let _ = self.action_task.await` to confirm termination.

---

### M7 — `load_execution_restore_data` always returns `Some(...)`, making the `None` branch dead
**File:** `src/coordinator/journal/restore.rs` lines 82–97
**Confidence:** 88

`load_execution_restore_data` unconditionally returns `Ok(Some(ExecutionRestoreData { ... }))`. The caller in `recovery.rs` line 88 checks `let Some(restore_data) = ... else { return Ok(()); }` — this branch can never be taken. The `Option` return type is misleading.

**Fix:** Return `Ok(None)` when both `fills` and `outcomes_today` are empty, or change the return type to `Result<ExecutionRestoreData>` and remove the `Option` wrapper.

---

## Low

### L1 — `governance_policy_blocked_domains_sorted` duplicates sort logic from `to_snapshot`
**File:** `src/coordinator/governance.rs` lines 107–113 and 290–298
**Confidence:** 80

The sort-and-collect logic for `blocked_domains` is duplicated between `GovernancePolicy::to_snapshot` and `governance_policy_blocked_domains_sorted`. The private function is only called from `persist_governance_policy`. One of them should call the other.

---

### L2 — `env_u64` helper is a module-private utility that should be in a shared location
**File:** `src/coordinator/strategy_runtime/actions.rs` lines 29–34
**Confidence:** 80

`env_u64` reads an env var as `u64` with a default. This pattern is duplicated across multiple modules. It should live in a shared `config::env` or `util::env` module.

---

## Architecture Observations (informational)

**Dual execution paths for foreground vs managed runtime:** `foreground.rs` implements its own `handle_strategy_actions` that uses `OrderExecutor` directly, bypassing the coordinator entirely. The scope note says "foreground intents now routed through coordinator ingress" but this is only true for the managed runtime path (`actions.rs`). Foreground runs do not benefit from the coordinator's risk gate, duplicate guard, Kelly sizing, or deployment gate. If this is intentional (foreground = dev/test mode), it should be documented. If not, it is a correctness gap.

**`RiskGate` has 9 separate `Arc<RwLock<_>>` fields:** The `check_order` hot path acquires at least 4 separate read locks sequentially. Consider consolidating related state (`daily_stats`, `drawdown_stats`, `consecutive_failures`) into a single `RwLock<RiskRuntimeState>` to reduce lock overhead and make atomic snapshots easier.

**`PositionAggregator` position IDs are not stable across restarts:** `open_position` generates IDs as `pos-{agent_id}-{counter}` where the counter resets to 0 on every restart. After a restart, newly opened positions may receive IDs that collide with IDs from the previous session if the counter wraps. This is unlikely in practice but worth noting for any future persistence of position IDs.

---

## Summary

| Severity | Count |
|----------|-------|
| Critical | 3 |
| High     | 8 |
| Medium   | 7 |
| Low      | 2 |
| **Total**| **20** |

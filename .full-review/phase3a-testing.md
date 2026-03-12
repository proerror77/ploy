# Phase 3a — Testing Strategy Review
**Branch**: `hotfix/staggered-arb-release-20260306`
**Date**: 2026-03-11
**Reviewer**: automated analysis (Phase 3a)

---

## Executive Summary

The coordinator decomposition introduced substantial new code across ~14 sub-modules. Test coverage is uneven: the admission, risk, position, queue, and governance modules have solid unit tests, but the critical concurrency hazards identified in Phase 1 are entirely untested. The foreground path has partial tests (payload construction only). The staggered_arb_live strategy has 43 tests covering config, lifecycle, and order-update scenarios, but no tests for the live coordinator integration path.

**Test file inventory:**
- `src/coordinator/bootstrap/tests.rs` — 14 tests (config rendering, env-var overrides)
- `src/coordinator/coordinator/tests.rs` — 9 tests (ingress, drain, governance)
- `src/strategy/execution/engine/tests.rs` — 14 tests (state machine, optimistic locking)
- `src/strategy/staggered_arb_live/tests.rs` — 43 tests (2034 lines)
- `src/strategy/momentum/tests.rs` — exists
- `src/tui/tests.rs` — exists
- `src/api/handlers/sidecar/tests.rs` — exists
- Inline `#[cfg(test)]` blocks in: `risk.rs` (13 tests), `position.rs` (6 tests), `queue.rs` (7 tests), `governance.rs` (4 tests), `admission/duplicate_guard.rs` (10 tests), `coordinator/bootstrap/strategy_deployments.rs`, `coordinator/bootstrap/runtime_config.rs`, `coordinator/bootstrap/openclaw_config.rs`, `coordinator/capital/market.rs`, `coordinator/capital/crypto.rs`, `coordinator/journal/restore.rs`, `coordinator/strategy_runtime/order_store.rs`, `cli/strategy/runtime_ops/foreground_submit.rs` (5 tests)
- Integration tests: `tests/architecture_gateway_only.rs`, `tests/legacy_live_gate.rs`, `tests/strategy_evaluations_and_deployment_gate.rs`

---

## 1. Critical Path Coverage

### 1.1 C2 — `record_loss` Does Not Reset `consecutive_failures`

**Status: Untested**

`record_loss` in `src/coordinator/risk/transitions.rs` (lines 107–137) updates `daily_stats.total_pnl` and `agent_stats.realized_pnl` but never touches `consecutive_failures` on the agent stats or the global `AtomicU32`. `record_success` does reset it (line 15: `self.consecutive_failures.store(0, Ordering::SeqCst)`; line 20: `stats.consecutive_failures = 0`).

This means a sequence of: failure → loss-realizing sell → failure will not reset the streak counter between the two failures, causing premature circuit-breaker trips. No test covers this path.

**Severity: Critical**

```rust
// Add to src/coordinator/risk.rs #[cfg(test)]
#[tokio::test]
async fn test_record_loss_does_not_reset_consecutive_failures() {
    let mut config = RiskConfig::default();
    config.max_consecutive_failures = 4;
    let gate = RiskGate::new(config);
    gate.register_agent("agent1", AgentRiskParams::default()).await;

    gate.record_failure("agent1", "order rejected").await;
    assert_eq!(gate.consecutive_failures(), 1);

    // A loss (realized from a sell fill) must not reset the failure streak.
    gate.record_loss("agent1", dec!(5)).await;
    assert_eq!(
        gate.consecutive_failures(), 1,
        "record_loss must not reset consecutive_failures"
    );

    gate.record_failure("agent1", "second failure").await;
    assert_eq!(gate.consecutive_failures(), 2);
    assert_eq!(gate.state().await, PlatformRiskState::Elevated);
}
```

---

### 1.2 H5 — TOCTOU Race on `Elevated → Normal` Transition

**Status: Untested**

`record_success` in `src/coordinator/risk/transitions.rs` (lines 68–71):

```rust
if *self.state.read().await == PlatformRiskState::Elevated {
    *self.state.write().await = PlatformRiskState::Normal;
    info!("Risk state normalized after successful execution");
}
```

This is a classic TOCTOU: the read-lock is dropped before the write-lock is acquired. Between those two lock acquisitions another task could have already transitioned the state to `Halted` (via `trigger_circuit_breaker`). The write then overwrites `Halted` with `Normal`, silently re-enabling trading.

No test exercises concurrent `record_success` + `trigger_circuit_breaker` racing on the same `RiskGate`.

**Severity: Critical**

```rust
#[tokio::test]
async fn test_record_success_does_not_overwrite_halted_state() {
    let mut config = RiskConfig::default();
    config.max_consecutive_failures = 1;
    config.circuit_breaker_auto_recover = false;
    let gate = Arc::new(RiskGate::new(config));
    gate.register_agent("agent1", AgentRiskParams::default()).await;

    // Force Elevated state.
    *gate.state.write().await = PlatformRiskState::Elevated;

    // Concurrently: record_success (which may normalize) vs trigger_circuit_breaker.
    let gate2 = gate.clone();
    tokio::join!(
        gate.record_success("agent1", dec!(1)),
        gate2.trigger_circuit_breaker("concurrent halt"),
    );

    // Halted must win — record_success must not overwrite it.
    assert_eq!(
        gate.state().await,
        PlatformRiskState::Halted,
        "Halted state must not be overwritten by concurrent record_success"
    );
}
```

---

### 1.3 C1 — Lock-Ordering Hazard in `close_position`

**Status: Untested**

`close_position` in `src/coordinator/position/transitions.rs` (lines 52–72) acquires `positions.write()` and then, while holding it, acquires `realized_pnl.write()`:

```rust
pub async fn close_position(&self, position_id: &str, exit_price: Decimal) -> Option<Decimal> {
    let mut positions = self.positions.write().await;          // lock 1
    if let Some(position) = positions.remove(position_id) {
        let pnl = ...;
        let mut realized = self.realized_pnl.write().await;    // lock 2 — held while lock 1 active
        *realized.entry(...).or_insert(Decimal::ZERO) += pnl;
        ...
    }
}
```

`reduce_position` (lines 76–116) correctly drops `positions` before acquiring `realized_pnl` (`drop(positions)` at line 110). The inconsistency means concurrent `close_position` + `reduce_position` on the same agent can deadlock if the Tokio runtime schedules them on different tasks.

No test exercises concurrent close + reduce on the same `PositionAggregator`.

**Severity: Critical**

```rust
#[tokio::test]
async fn test_concurrent_close_and_reduce_do_not_deadlock() {
    let agg = Arc::new(PositionAggregator::new());
    let pos_id = agg
        .open_position("agent1", Domain::Crypto, "btc-15m", "t1", Side::Up, 100, dec!(0.50))
        .await;

    let agg2 = agg.clone();
    let pos_id2 = pos_id.clone();

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        tokio::join!(
            agg.close_position(&pos_id, dec!(0.55)),
            agg2.reduce_position(&pos_id2, 50, dec!(0.52)),
        ),
    )
    .await;

    assert!(result.is_ok(), "concurrent close+reduce deadlocked");
}
```

---

### 1.4 A-C2 — Foreground Path Risk Bypass

**Status: Partially tested (payload construction only)**

`src/cli/strategy/runtime_ops/foreground_submit.rs` has 5 tests in its `#[cfg(test)]` block, but they only test `build_coordinator_payload` (JSON shape, deployment_id requirement, priority label mapping) and `external_priority_label`. They do not test:

- The `ForegroundIntentSubmitter::submit` path
- The fallback to `DirectExecuted` when no coordinator URL is reachable
- The `Skipped` path when no executor is configured
- Whether a live-mode submitter with no executor silently drops orders

The foreground path routes through the coordinator HTTP API (`submit_intent_via_coordinator`) when a `deployment_id` is present. When the coordinator is unreachable, it falls back to `DirectExecuted` via the `OrderExecutor` — which bypasses all coordinator risk controls. This fallback has zero tests.

**Severity: Critical**

```rust
// Add to foreground_submit.rs #[cfg(test)]
#[tokio::test]
async fn test_foreground_submitter_skipped_when_no_deployment_id_and_no_executor() {
    let submitter = ForegroundIntentSubmitter::new(false, None);
    let intent = sample_intent(HashMap::new()); // no deployment_id
    // Should produce Skipped outcome, not panic.
    handle_submit_intent("test-strategy", intent, &submitter, None).await;
    // Verify: no order was silently submitted.
}

#[tokio::test]
async fn test_foreground_submitter_live_mode_direct_executor_bypasses_risk_gate() {
    // This test documents the bypass — it should fail once the bypass is fixed.
    let executor = Arc::new(make_mock_executor());
    let submitter = ForegroundIntentSubmitter::new(false, Some(executor.clone()));
    let intent = sample_intent(HashMap::new()); // no deployment_id → direct path
    handle_submit_intent("test-strategy", intent, &submitter, None).await;
    // Assert: executor was called directly, no risk gate was consulted.
    assert!(executor.was_called(), "direct executor was invoked");
}
```

---

### 1.5 A-H1 — `pending_buy_notional_excluding_domains` Call Site

**Status: Function correct; call site semantics unverified**

The Phase 1 report flagged `pending_buy_notional_excluding_domains` as always returning zero. Reading the implementation at `src/coordinator/queue.rs:290–299`, the function is correct — it iterates `self.heap` and sums notionals for non-excluded domains.

The call site in `src/coordinator/coordinator/ingress.rs:339–351` passes all four known domains as the excluded list:

```rust
.pending_buy_notional_excluding_domains(&[
    Domain::Crypto,
    Domain::Sports,
    Domain::Politics,
    Domain::Economics,
])
```

This means `other_pending_buy_notional` is always zero for any intent from a known domain. The function is used to catch pending buys from *unknown* future domains — a reasonable design, but it means the governance notional cap (`current_account_notional`) never includes cross-domain pending buys from known domains. The existing test `test_pending_buy_notional_excluding_domains` passes `[Crypto, Sports]` and expects `10.00` from a Politics buy — it does not test the all-domains-excluded scenario.

**Severity: Medium (design question, not a confirmed bug)**

```rust
// Recommended test to document the all-domains-excluded behavior
#[test]
fn test_pending_buy_notional_all_known_domains_excluded_returns_zero() {
    let mut queue = OrderQueue::new(100);
    for domain in [Domain::Crypto, Domain::Sports, Domain::Politics, Domain::Economics] {
        let intent = OrderIntent::new("a", domain, "mkt", "tok", Side::Up, true, 10, dec!(0.50));
        queue.enqueue(intent).unwrap();
    }
    let notional = queue.pending_buy_notional_excluding_domains(&[
        Domain::Crypto, Domain::Sports, Domain::Politics, Domain::Economics,
    ]);
    assert_eq!(notional, Decimal::ZERO,
        "all known domains excluded — documents that cross-domain pending is not counted");
}
```

---

## 2. Strategy Test Coverage — `staggered_arb_live`

**Status: Good unit coverage; no coordinator integration tests**

`src/strategy/staggered_arb_live/tests.rs` (2034 lines, 43 test functions) covers:

- Config parsing and TOML defaults
- OBI bonus threshold adjustment
- Adapter creation and series mapping
- Leg1 submit (idempotency key, client order ID)
- Leg2 merge logic (fee-adjusted PnL gate, force threshold, protective stop)
- Required feeds
- Order update handling (lifecycle, order_updates, late fills after settlement)
- Tick-driven recheck (`on_tick`)
- Quote persistence gate
- Balance pause
- Event expiry settlement (single-leg, partial leg2, double-close prevention)
- Live leg2 without active window
- Residual below venue minimum

**Gaps:**

### 2.1 No test for the live coordinator submission path

The strategy emits `StrategyAction::SubmitIntent` which is consumed by `foreground.rs` → `foreground_submit.rs`. When running in managed mode (coordinator present), the intent is POSTed to the coordinator HTTP API. There is no integration test that verifies a staggered_arb intent flows through the coordinator risk gate.

**Severity: High**

```rust
#[tokio::test]
async fn test_staggered_arb_intent_routed_through_coordinator_risk_gate() {
    let (handle, coordinator) = make_test_handle();
    coordinator.risk_gate
        .register_agent_with_domain("staggered_arb", Domain::Crypto, AgentRiskParams::default())
        .await;

    let intent = OrderIntent::new(
        "staggered_arb", Domain::Crypto, "btc-up-5m", "token-up",
        Side::Up, true, 100, dec!(0.50),
    ).with_deployment_id("deploy.crypto.staggered_arb.5m");

    coordinator.handle_order_intent(intent).await;
    let (_, success_count, _) = coordinator.risk_gate.daily_stats().await;
    assert_eq!(success_count, 0, "intent should be queued, not yet executed");
    coordinator.drain_and_execute().await;
    let (_, success_count, _) = coordinator.risk_gate.daily_stats().await;
    assert_eq!(success_count, 1);
}
```

### 2.2 No test for leg2 timeout / force-close path in live mode

The live adapter has a `wait_deadline` per position and a `force_leg2_attempted` flag on `LiveOrderTrack`. No test verifies that a leg2 that times out triggers the force-close path and records the abort correctly in live (non-dry-run) mode.

**Severity: High**

### 2.3 No test for `on_order_update` with partial fill then cancel

The `on_order_update` handler processes `Filled`, `Cancelled`, and `PartiallyFilled` statuses. There is no test for the sequence: partial fill → cancel (which should record the partial fill and close the position with the partial shares).

**Severity: Medium**

---

## 3. Concurrency Test Coverage

**Status: Absent for all identified hazards**

None of the three concurrency issues (lock-ordering in `close_position`, TOCTOU in `record_success`, and the `record_loss` streak bug) have dedicated concurrency tests. All existing tests are sequential.

### 3.1 `OrderQueue` under concurrent enqueue/dequeue

`OrderQueue` is wrapped in `RwLock<OrderQueue>` at the coordinator level. There is no test that verifies priority ordering holds under rapid concurrent submissions from multiple agents.

**Severity: Medium**

```rust
#[tokio::test]
async fn test_queue_priority_ordering_under_concurrent_enqueue() {
    let queue = Arc::new(tokio::sync::RwLock::new(OrderQueue::new(200)));

    let mut handles = Vec::new();
    for i in 0..20u64 {
        let q = queue.clone();
        let priority = if i % 4 == 0 { OrderPriority::Critical } else { OrderPriority::Normal };
        handles.push(tokio::spawn(async move {
            let intent = make_intent_with_priority(priority);
            q.write().await.enqueue(intent).unwrap();
        }));
    }
    for h in handles { h.await.unwrap(); }

    let batch = queue.write().await.dequeue_batch(5);
    for item in &batch {
        assert_eq!(item.priority, OrderPriority::Critical,
            "Critical intents must drain first");
    }
}
```

---

## 4. Bootstrap Test Coverage

**Status: Config rendering only; no runtime behavior**

`src/coordinator/bootstrap/tests.rs` (14 tests) covers:
- `apply_strategy_deployments` domain routing (economics, crypto, unknown strategy, gamma_scalping alias, pm_5m_directional)
- `collect_managed_strategy_runtime_plans` plan generation
- `build_*_runtime_config` TOML rendering for all strategy types
- `from_app_config` env-var overrides for lob_ml and crypto agent signal gate
- `ensure_pm_market_metadata_table` (DB-gated, skips without `PLOY_TEST_DATABASE_URL`)

**Gaps:**

### 4.1 No test for `start_platform` bootstrap sequence

The `start_platform` function in `src/coordinator/bootstrap.rs` wires together coordinator, agents, API server, and journal. There is no test that exercises this wiring — only the individual config-building functions are tested.

**Severity: High**

### 4.2 No test for `PlatformBootstrapConfig::from_app_config` with conflicting env vars

The env-var override tests use `Mutex<()>` to serialize access, but they do not test the case where two conflicting env vars are set simultaneously (e.g., both `PLOY_CRYPTO_LOB_ML__EXIT_MODE` and the deprecated `PLOY_CRYPTO_LOB_ML__ENABLE_PRICE_EXITS`). The existing test `from_app_config_ignores_deprecated_price_exits_env` covers one direction but not the reverse.

**Severity: Low**

---

## 5. Recovery / Restore Test Coverage

**Status: Partially covered**

`src/coordinator/journal/restore.rs` has 4 unit tests covering domain/side parsing and metadata normalization.

`src/coordinator/risk.rs` has 2 restore tests:
- `test_restore_runtime_counters_restores_agent_and_failure_streaks`
- `test_restore_runtime_counters_halts_when_daily_loss_exceeded`

**Gaps:**

### 5.1 No end-to-end restore integration test

There is no test that exercises the full cold-start replay path: load fills from DB → replay into `PositionAggregator` → restore risk counters → verify the coordinator is in the correct state. The individual pieces are tested in isolation but the wiring in `coordinator_bootstrap.rs` is untested.

**Severity: High**

### 5.2 No test for restore with corrupt fills

The restore path skips rows with unknown domain, unknown side, zero shares, or non-positive fill price. There is no test that verifies a partially corrupt restore log results in a consistent (not panicking) position book.

**Severity: Medium**

```rust
#[test]
fn test_restore_skips_corrupt_fills_and_continues() {
    let fills = vec![
        make_fill("agent1", Domain::Crypto, true, 100, dec!(0.50)),      // valid
        make_fill("agent1", Domain::Crypto, true, 0, dec!(0.50)),        // zero shares — skip
        make_fill_raw("agent1", "unknown_domain", true, 10, dec!(0.50)), // bad domain — skip
        make_fill("agent1", Domain::Crypto, true, 10, dec!(-0.01)),      // bad price — skip
    ];
    let agg = PositionAggregator::new();
    replay_fills_into_aggregator(&fills, &agg);
    assert_eq!(agg.blocking_position_count(), 1);
}
```

---

## 6. Governance Test Coverage

**Status: Unit logic covered; persistence and restore not tested**

`src/coordinator/governance.rs` has 4 unit tests covering policy validation and block logic.

**Gaps:**

### 6.1 Governance state not restored on restart

`load_persisted_governance_policy` is called from `coordinator_bootstrap.rs` only when a DB pool is provided. If the pool is absent, the coordinator silently starts with the default config-derived policy, discarding any runtime changes made via the API. There is no test that verifies a policy update persisted to DB is correctly restored after a simulated restart.

**Severity: Critical**

```rust
#[tokio::test]
async fn test_governance_policy_survives_coordinator_restart() {
    let pool = test_pool().await;
    let coordinator = make_coordinator_with_pool(pool.clone()).await;

    coordinator.handle().update_governance_policy(GovernancePolicyUpdate {
        block_new_intents: true,
        blocked_domains: vec!["sports".to_string()],
        updated_by: "test".to_string(),
        ..Default::default()
    }).await.expect("policy update");

    // Simulate restart: new coordinator, same pool.
    let coordinator2 = make_coordinator_with_pool(pool.clone()).await;
    coordinator2.load_persisted_governance_policy().await.expect("restore");

    let snapshot = coordinator2.handle().governance_status().await;
    assert!(snapshot.policy.block_new_intents, "block_new_intents must survive restart");
    assert!(snapshot.policy.blocked_domains.contains(&"sports".to_string()));
}
```

### 6.2 No test for `set_global_mode` clearing domain modes

`set_global_mode` clears all domain-level overrides. No test verifies this side-effect.

**Severity: Low**

---

## 7. Test Quality Assessment

### Strengths

- `duplicate_guard.rs`: 10 tests, behavior-focused, cover scope variants, window expiry, critical bypass, sell exemption. High quality.
- `risk.rs`: 13 tests, cover all major state transitions. Correct use of `Ordering::SeqCst` in atomic operations.
- `position.rs`: 6 tests, cover open/close/reduce/aggregate/price-update/agent-stats. Isolated, fast.
- `queue.rs`: 7 tests, cover priority ordering, eviction, expiry, notional sum, sell-shares filter.
- `governance.rs`: 4 tests, cover policy validation and block logic. Pure unit tests, no DB.
- `execution/engine/tests.rs`: 14 tests, use a `RecordingStore` mock to verify optimistic-locking version sequences. Excellent pattern — tests behavior (version numbers passed to DB) not implementation.
- `staggered_arb_live/tests.rs`: 43 tests, comprehensive config and lifecycle coverage. Good use of `seed_persistent_pm_quotes` helper to avoid test setup duplication.
- `coordinator/tests.rs`: 9 tests including `test_drain_and_execute_sell_fill_reduces_position_and_realizes_pnl` which exercises the full buy→sell→position-reduce→pnl path.

### Weaknesses

- No concurrency tests anywhere in the coordinator.
- No integration tests that wire coordinator → risk → position → journal together end-to-end.
- Foreground path tests cover only JSON payload construction, not the submission lifecycle.
- Bootstrap tests test config rendering only — no runtime behavior.
- `coordinator/tests.rs` uses a dry-run `PolymarketClient` that always succeeds; no failure injection for testing error paths.
- `staggered_arb_live/tests.rs` has no tests for the live (non-dry-run) coordinator submission path.
- No property-based tests or fuzzing for the risk gate or admission controller.

---

## 8. Summary Table

| Finding | Severity | Status | Recommended Test |
|---|---|---|---|
| `record_loss` doesn't reset `consecutive_failures` (C2) | Critical | Untested | `test_record_loss_does_not_reset_consecutive_failures` |
| TOCTOU race: `record_success` overwrites `Halted` (H5) | Critical | Untested | Concurrent `record_success` + `trigger_circuit_breaker` |
| Lock-ordering deadlock in `close_position` (C1) | Critical | Untested | Concurrent close + reduce with timeout |
| Governance not restored on restart | Critical | Untested | DB-backed restart round-trip test |
| Foreground path risk bypass (A-C2) | Critical | Partial (payload only) | `ForegroundIntentSubmitter` lifecycle tests |
| Staggered arb → coordinator integration | High | Untested | Coordinator submission path test |
| Leg2 timeout / force-close in live mode | High | Untested | Lifecycle timeout test |
| Cold-start restore end-to-end | High | Untested | Full replay integration test |
| Bootstrap `start_platform` wiring | High | Untested | Platform bootstrap smoke test |
| Restore with corrupt fills | Medium | Untested | Partial-corrupt restore test |
| Queue concurrency under load | Medium | Untested | Concurrent enqueue priority test |
| `pending_buy_notional` all-domains-excluded | Medium | Undocumented | Document zero-return behavior |
| Partial fill → cancel lifecycle | Medium | Untested | `on_order_update` sequence test |
| `set_global_mode` clears domain overrides | Low | Untested | `GovernanceController` side-effect test |
| Deprecated env var conflict | Low | Partial | Reverse-direction env var test |

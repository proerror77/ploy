# Phase 3a: Test Coverage & Quality Analysis — Ploy Trading System

**Date**: 2026-03-08
**Scope**: Full Ploy trading system (Rust ~165K lines, 260+ source files)
**Focus**: Test coverage gaps, test quality, and testing strategy

---

## Executive Summary

The Ploy codebase has substantial test infrastructure — 153 files contain `#[cfg(test)]` modules, with ~796 individual test functions (651 `#[test]` + 145 `#[tokio::test]`). The coordinator alone has 53 tests. However, testing is heavily concentrated in pure computation modules (probability, fee models, calculations) while critical production paths — order execution, database persistence, and the bootstrap pipeline — have minimal or no test coverage. The CI pipeline runs all tests against a PostgreSQL service container, which is good, but there are no integration tests for the end-to-end order flow, no load/soak tests for memory growth, and no security-specific test assertions.

**Finding Distribution**: 3 Critical, 4 High, 5 Medium, 3 Low

---

## Critical Findings

### T-01: No integration test for order execution pipeline

**Severity**: Critical
**Impact**: The most important code path — intent submission → risk check → queue → execution → PnL recording — has zero end-to-end tests

The coordinator's `handle_order_intent` → `drain_queue` → `execute_order` pipeline is the core revenue path. While the coordinator has 53 unit tests (mostly for parsing, config validation, and state transitions), none test the full order lifecycle. The P-01 double `record_success` bug (Phase 2) would have been caught by an integration test asserting PnL accounting after a simulated trade.

**Recommendation**: Create an integration test that:
1. Constructs a coordinator with mock exchange client
2. Submits an order intent
3. Asserts risk gate checks pass
4. Verifies exactly one `record_success` call with correct PnL
5. Verifies position tracking updates

### T-02: No tests for position tracking error handling

**Severity**: Critical
**Impact**: The silently discarded `let _ = self.positions.open_position(...)` at coordinator.rs:2734,4229 (Phase 1 finding Q-01) has no test coverage

Position tracking is a financial safety mechanism. The `open_position` and `close_position` calls are fire-and-forget with `let _ =`, meaning errors are silently swallowed. No test verifies what happens when position tracking fails — whether the system continues trading with stale position data, whether risk limits are still enforced, or whether reconciliation catches the drift.

**Recommendation**: Add tests that:
1. Mock `PositionAggregator` to return errors
2. Verify the coordinator logs the error (not silently discards)
3. Verify risk gate still enforces position limits even with stale data

### T-03: No regression test for PnL accounting correctness

**Severity**: Critical
**Impact**: The double `record_success` bug (P-01) demonstrates that PnL accounting can silently corrupt without any test catching it

The `RiskGate` tracks daily PnL for circuit breaker decisions. If PnL is inflated 2x (as P-01 causes), the risk gate may allow trades that should be blocked, or block trades that should be allowed. No test asserts the invariant: "for a trade with realized PnL X, the risk gate's daily PnL counter increases by exactly X."

**Recommendation**: Add a unit test for the coordinator's PnL recording path that asserts:
- Profitable trade: `record_success` called once with exact PnL
- Losing trade: `record_loss` called once with abs(PnL), `record_success` called once with zero
- Net daily PnL matches sum of individual trades

---

## High Severity Findings

### T-04: PostgresStore (1,364 lines) has zero unit tests

**Severity**: High
**Location**: `src/adapters/postgres.rs`
**Impact**: All 50+ SQL queries are only validated at runtime; schema drift between bootstrap DDL and migrations is untested

The `PostgresStore` implements the `EngineStore` trait with 50+ methods for order persistence, cycle management, position tracking, and audit logging. None of these methods have unit tests. The CI integration tests exercise some paths indirectly, but there are no targeted tests for:
- SQL query correctness (especially complex queries with CTEs and window functions)
- Concurrent access patterns (two strategies writing simultaneously)
- Error handling for constraint violations

**Recommendation**: Add `sqlx::test` integration tests for critical store methods: `insert_order`, `update_order_status`, `get_active_positions`, `record_pnl`.

### T-05: Emergency stop mechanism has no concurrency test

**Severity**: High
**Location**: `src/coordination/emergency_stop.rs` (2 tests)
**Impact**: M-06 (Relaxed ordering) means the emergency stop may not propagate on ARM; no test verifies cross-thread visibility

The emergency stop has only 2 tests, both single-threaded. No test spawns multiple tokio tasks, triggers emergency stop from one, and verifies all others see it within a bounded time. On the production ARM server (tango-1-1), `Ordering::Relaxed` could cause unbounded delay.

**Recommendation**: Add a multi-threaded test:
```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn emergency_stop_visible_across_threads() {
    let stop = EmergencyStop::new();
    let stop_clone = stop.clone();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let b2 = barrier.clone();

    let reader = tokio::spawn(async move {
        b2.wait().await;
        // Must see stop within 100ms
        tokio::time::timeout(Duration::from_millis(100), async {
            while !stop_clone.is_stopped() {
                tokio::task::yield_now().await;
            }
        }).await.expect("stop not visible within 100ms");
    });

    barrier.wait().await;
    stop.trigger("test").await;
    reader.await.unwrap();
}
```

### T-06: No tests assert Debug output doesn't contain secrets

**Severity**: High
**Impact**: H-01 and H-02 (credential logging) could regress silently after fix

After fixing `ApiCredentials` and `DatabaseConfig` Debug impls to redact secrets, there should be regression tests asserting that `format!("{:?}", credentials)` does NOT contain the actual secret values.

**Recommendation**:
```rust
#[test]
fn api_credentials_debug_redacts_secret() {
    let creds = ApiCredentials {
        api_key: "ak_test_12345678".into(),
        secret: "super_secret_value".into(),
        passphrase: "my_passphrase".into(),
    };
    let debug = format!("{:?}", creds);
    assert!(!debug.contains("super_secret_value"));
    assert!(!debug.contains("my_passphrase"));
    assert!(debug.contains("REDACTED"));
}
```

### T-07: Staggered arb entry evaluation (470 lines) has no targeted unit tests

**Severity**: High
**Location**: `src/strategy/staggered_arb_live.rs` — 43 tests exist but focus on state management
**Impact**: The hot-path entry evaluation with 15+ filter gates is tested only indirectly

The `try_entry_for_window()` method is the most complex pure function in the codebase (470 lines, 15+ sequential checks). While staggered_arb_live.rs has 43 tests (the most of any strategy file), they primarily test order tracking, state transitions, and configuration parsing — not the entry evaluation logic itself. Each filter gate should have a targeted test proving it rejects when expected.

**Recommendation**: Extract `try_entry_for_window` into a pure function and add parameterized tests for each rejection reason.

---

## Medium Severity Findings

### T-08: No soak/long-running tests for memory growth (P-03)

**Severity**: Medium
**Impact**: Unbounded HashMap growth (P-03) will only manifest after hours/days of operation

The `archived_live_orders`, `entry_reject_counts`, and `cooldowns` HashMaps grow without bounds. No test simulates a long-running session (e.g., 10,000 simulated trades) and asserts memory stays within bounds.

### T-09: Integration tests use `unsafe` env manipulation

**Severity**: Medium
**Location**: `tests/legacy_live_gate.rs`, `tests/strategy_evaluations_and_deployment_gate.rs`
**Impact**: Tests using `unsafe { env::set_var() }` are unsound in multi-threaded test execution

Both integration test files use `unsafe { std::env::set_var() }` with a `Mutex` guard. Since Rust 1.66, `env::set_var` is documented as unsound in multi-threaded programs. While the Mutex prevents concurrent env modification within these tests, other tests running in parallel (via `cargo test`) may read env vars concurrently.

**Recommendation**: Use `temp_env` crate or refactor to pass config via function parameters instead of env vars.

### T-10: No test for WebSocket authentication flow

**Severity**: Medium
**Location**: `src/api/websocket.rs`
**Impact**: M-04 (WS token in query parameter) has no test verifying auth works or rejects invalid tokens

### T-11: Architecture gateway test is fragile (string scanning)

**Severity**: Medium
**Location**: `tests/architecture_gateway_only.rs`
**Impact**: The test scans source files for string patterns like `client.submit_order(` — easily bypassed by renaming

The architecture test that enforces "only executors can call submit_order" uses substring matching on source code. This is a good idea but fragile — a refactor renaming the method would silently bypass the guard.

### T-12: No property-based tests for financial calculations

**Severity**: Medium
**Impact**: Fee model, probability, and slippage calculations use `Decimal` arithmetic with edge cases

The `fee_model.rs` (9 tests), `probability.rs` (10 tests), and `slippage.rs` (10 tests) have good unit test coverage but all use hand-picked values. Property-based testing (via `proptest` or `quickcheck`) would catch edge cases like:
- Fees on zero-size orders
- Probability at exactly 0.0 or 1.0 boundaries
- Slippage with extreme price movements

---

## Low Severity Findings

### T-13: No frontend tests

**Severity**: Low
**Location**: `ploy-frontend/`
**Impact**: React dashboard has no unit or integration tests

The frontend has no test files, no test configuration, and no test scripts in `package.json`. Given that the frontend is a monitoring dashboard (not order entry), this is low severity.

### T-14: Backtest modules have limited assertion coverage

**Severity**: Low
**Impact**: Backtest results are logged but not programmatically asserted

### T-15: No test for circuit breaker state machine transitions

**Severity**: Low
**Location**: `src/coordination/circuit_breaker.rs` (6 tests)
**Impact**: Tests exist but don't cover all state transitions (Closed→Open→HalfOpen→Closed cycle)

---

## Test Coverage by Module

| Module | Files with tests | Test count | Critical paths tested? |
|--------|-----------------|------------|----------------------|
| coordinator/ | 2/3 | ~56 | Partial — unit tests only, no integration |
| strategy/ | 40+/60+ | ~350 | Good for calculations, weak for execution |
| adapters/ | 9/15 | ~65 | WebSocket mocking good, DB untested |
| coordination/ | 4/4 | ~16 | Basic, no concurrency tests |
| signing/ | 5/6 | ~10 | HMAC and order signing covered |
| platform/ | 8/12 | ~50 | Risk and position well-tested |
| persistence/ | 3/4 | ~5 | Minimal — DLQ and checkpoint only |
| api/ | 5/8 | ~16 | Sidecar endpoints tested, core routes not |
| rl/ | 8/10 | ~30 | Good coverage for RL-specific code |
| ai_clients/ | 8/10 | ~25 | Mock-based, good isolation |

## Positive Patterns

- **Architecture gateway test**: `tests/architecture_gateway_only.rs` enforces that only executor modules can call `submit_order` — a structural safety test
- **Legacy live gate tests**: `tests/legacy_live_gate.rs` verifies the safety gate blocks all known legacy commands
- **Strategy evaluation + deployment gate**: Integration test with real PostgreSQL verifying deployment lifecycle
- **CI with PostgreSQL service**: The test workflow provisions a real PostgreSQL 15 instance
- **Commit hygiene check**: CI rejects WIP/fixup commits on PRs
- **Feature-gated test compilation**: Tests behind `#[cfg(feature = "api")]` only run when the feature is enabled

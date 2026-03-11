# Phase 1: Code Quality & Architecture Review

**Branch:** `hotfix/staggered-arb-release-20260306` vs `main`
**Date:** 2026-03-11

---

## Code Quality Findings (from phase1a-quality.md)

### Critical (3)

| ID | File | Issue |
|----|------|-------|
| C1 | `coordinator/position/transitions.rs:52` | Nested write-lock acquisition in `close_position` — lock-ordering hazard, potential deadlock |
| C2 | `coordinator/risk/transitions.rs:107` | `record_loss` doesn't reset `consecutive_failures` or increment daily counters — risk state corruption |
| C3 | `coordinator/coordinator/recovery.rs:77` | Unbounded execution log replay on restart — replays ALL historical fills, creates phantom positions |

### High (8)

| ID | File | Issue |
|----|------|-------|
| H1 | `strategy/staggered_arb_live/entry.rs:76` | `try_entry_for_window` is 250 lines, cyclomatic complexity >20, duplicated dry-run/live paths |
| H2 | `coordinator/coordinator/ingress.rs` | 4-step rejection pattern repeated 9 times — DRY violation in hottest path |
| H3 | `coordinator/queue.rs:128` | `pop_lowest_priority` rebuilds entire heap O(n) on every full-queue enqueue |
| H4 | `coordinator/strategy_runtime/actions.rs:564` | `spawn_split_arb_poll_task` spawns unbounded concurrent polling tasks per order |
| H5 | `coordinator/risk/transitions.rs:68` | TOCTOU race on `Elevated→Normal` state transition — can silently clear circuit breaker |
| H6 | `cli/strategy/runtime_ops/foreground.rs:66` | Silent dry-run fallback on live auth failure — misconfigured live deployment runs silently |
| H7 | `coordinator/admission/deployments.rs:22` | `deployment_gate_required()` reads env var on every BUY intent — global lock overhead |
| H8 | `coordinator/admission.rs:100` | `apply_kelly_sizing` silently skips when signal metadata absent — silent misconfiguration |

### Medium (7) / Low (2)

See `.full-review/phase1a-quality.md` for full details.

---

## Architecture Findings (from phase1b-architecture.md)

### Critical (2)

| ID | File | Issue |
|----|------|-------|
| A-C1 | `coordinator/coordinator.rs` | Coordinator struct has 12 direct fields — structural God Object despite file decomposition |
| A-C2 | `cli/strategy/runtime_ops/foreground.rs` | Foreground execution path bypasses ALL coordinator risk controls (admission, risk gate, position tracking) |

### High (5)

| ID | File | Issue |
|----|------|-------|
| A-H1 | `coordinator/coordinator/ingress.rs:343` | `pending_buy_notional_excluding_domains` excludes ALL domains — result is always zero |
| A-H2 | `coordinator/capital/market.rs` | `MarketCapitalAllocator` doesn't track realized positions — exposure resets to zero after fill |
| A-H3 | `coordinator/bootstrap/startup_context.rs` | Bootstrap initialization order is implicit — no compile-time enforcement |
| A-H4 | `coordinator/governance.rs` | Governance state not restored on startup — operator pauses lost on restart |
| A-H5 | `coordinator/strategy_runtime/` | Two separate action dispatch paths with overlapping implementations |

### Medium (4) / Low (2)

See `.full-review/phase1b-architecture.md` for full details.

---

## Critical Issues for Phase 2 Context

The following findings should inform the security and performance review:

1. **Foreground bypass (A-C2)**: Live orders can bypass all risk controls via `ploy strategy run --foreground`. Security implication: no exposure limits, no circuit breaker, no deployment gate enforcement.

2. **Capital allocator gap (A-H2)**: After a fill, the capital allocator shows zero exposure for that market. A strategy can immediately re-enter at full size. Performance/correctness implication: actual exposure can be 2x the configured limit.

3. **Unbounded fill replay (C3)**: On restart, all historical fills are replayed into `PositionAggregator`. On a long-running system this is an unbounded DB query that can cause startup delays and incorrect initial state.

4. **TOCTOU race on circuit breaker (H5)**: A successful order can silently clear a `Halted` circuit breaker state. Security implication: the circuit breaker can be bypassed by a race condition.

5. **Governance not restored (A-H4)**: Operator-set domain pauses are lost on restart. Operational security implication: a paused domain automatically resumes after any process restart.

6. **Unbounded poll task spawning (H4)**: WS reconnects can spawn multiple concurrent polling tasks per order, sending duplicate order updates to the strategy manager. Performance implication: duplicate fills can be processed.

---

## Phase 1 Summary

| Category | Critical | High | Medium | Low | Total |
|----------|----------|------|--------|-----|-------|
| Code Quality | 3 | 8 | 7 | 2 | 20 |
| Architecture | 2 | 5 | 4 | 2 | 13 |
| **Combined** | **5** | **13** | **11** | **4** | **33** |

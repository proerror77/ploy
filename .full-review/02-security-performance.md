# Phase 2: Security & Performance Review

**Branch:** `hotfix/staggered-arb-release-20260306` vs `main`
**Date:** 2026-03-11

---

## Security Findings (from phase2a-security.md)

### Critical (1)

| ID | File | Issue |
|----|------|-------|
| F-01 | `foreground_submit.rs`, `foreground.rs` | Foreground fallback executes live orders without any risk controls when `deployment_id` absent in metadata — direct CLOB call bypasses admission, risk gate, governance, capital allocator, circuit breaker, journal |

### High (3)

| ID | File | Issue |
|----|------|-------|
| F-02 | `risk/transitions.rs:68–71` | TOCTOU race on `Elevated→Normal` circuit breaker transition — concurrent `trigger_circuit_breaker` can be silently overwritten (CWE-362) |
| F-03 | `coordinator/recovery.rs:55–72` | Governance state not restored on restart unless `governance_store_pool` is explicitly wired — operator pauses lost on OOM kill/deploy (CWE-665) |
| F-04 | `coordinator/recovery.rs:77–231`, `risk/exposure.rs:44–77` | Capital allocator exposure gap after fill — risk gate shows $0 exposure during recovery replay, enabling 2x exposure limit breach (CWE-841) |

### Medium (4)

| ID | File | Issue |
|----|------|-------|
| F-05 | `admission/deployments.rs:34–62` | Deployment JSON loaded from `PLOY_DEPLOYMENTS_FILE` env var with no path validation — attacker-controlled file can enable disabled deployments (CWE-73) |
| F-06 | `api/auth.rs:182–213` | Sidecar token accepted via `Authorization: Bearer` header — prevents WAF-level token-role separation; admin cookie accepts raw token (CWE-287) |
| F-07 | `coordinator/recovery.rs:77–94` | Fill replay loop runs synchronously at startup with no pagination — thousands of fills today block ingress channel for seconds/minutes (CWE-400) |
| F-08 | `sidecar/grok_decision.rs:59–96` | Grok prompt embeds unvalidated free-text fields from sidecar — prompt injection risk; `autonomous.rs` sanitization not applied here (CWE-77) |

### Low (3)

| ID | File | Issue |
|----|------|-------|
| F-09 | `admission/deployments.rs:81` | Deployment gate bypass for sell intents is architecturally correct but undocumented |
| F-10 | `api/auth.rs:87–91` | Admin token fingerprint uses unsalted SHA-256 — reversible via rainbow table (CWE-916) |
| F-11 | `ingress.rs`, `admission/deployments.rs:22–32` | `PLOY_DEPLOYMENT_GATE_REQUIRED=false` has no audit trail; checked at runtime on every request |

**Security Positives:** Parameterized SQL throughout (no injection risk), constant-time `ct_eq()` token comparison, secure cookie defaults (`HttpOnly`, `SameSite=Strict`), sidecar auth required by default, reduce-only sell guard, governance persistence with full audit trail, circuit breaker idempotency.

---

## Performance Findings (from phase2b-performance.md)

### Critical (2)

| ID | File | Issue |
|----|------|-------|
| P-C1 | `coordinator/coordinator.rs:237–274` | Serialized coordinator loop — `handle_order_intent` awaited inline, blocking all other intents. At 50ms DB latency, saturates at ~20 intents/sec |
| P-C2 | `coordinator/coordinator/ingress.rs` (18 call sites) | 14 sequential `persist_risk_decision` DB writes on the hot path for a single passing intent — observability writes blocking order processing |

### High (6)

| ID | File | Issue |
|----|------|-------|
| P-H1 | `coordinator/queue.rs:128–148` | `pop_lowest_priority` rebuilds entire heap O(n log n) on every full-queue eviction |
| P-H2 | `coordinator/queue.rs:205–229` | `cleanup_expired_intents` rebuilds heap O(n log n) on every drain tick (1s interval), even with zero expired items |
| P-H3 | `coordinator/strategy_runtime/actions.rs:337–355` | `spawn_split_arb_poll_task` spawns unbounded concurrent tasks per order — no dedup, no cancellation on WS reconnect |
| P-H4 | `coordinator/admission/deployments.rs:22–32` | `deployment_gate_required()` reads env var (global lock) on every BUY intent |
| P-H5 | `coordinator/coordinator/execution.rs:180–232` | `get_agent_positions` does full O(n) scan across all agents on every sell fill |
| P-H6 | `coordinator/coordinator/execution.rs:136`, `risk/exposure.rs:44–77` | `refresh_risk_exposure_for_agent` acquires 3 separate write locks + full position scan per fill |

### Medium (7)

| ID | File | Issue |
|----|------|-------|
| P-M1 | `coordinator/coordinator/ingress.rs:336–352` | `current_account_notional` acquires 6 locks + O(n) queue scan per BUY intent; result always zero due to A-H1 bug |
| P-M2 | `coordinator/coordinator/ingress.rs:72,107,121` | 4 separate governance lock acquisitions per intent — needs `governance_snapshot()` |
| P-M3 | `coordinator/risk/checks.rs:13–102` | `check_order` acquires 7 sequential read locks — needs `risk_snapshot()` |
| P-M4 | `strategy/staggered_arb_live/entry.rs:86` | `backtest_config.clone()` on every entry check — should be a reference |
| P-M5 | `strategy/staggered_arb_live/runtime_flow.rs:173–179` | Full `HashSet<String>` clone of all active window keys on every tick |
| P-M6 | `coordinator/journal.rs:66–175` | 4 sequential DB writes per fill — should be `tokio::join!` |
| P-M7 | `coordinator/capital/market/accounting.rs:294–321` | `market_cap_for` builds `HashSet` on every `reserve_buy` call |

### Low (5)

| ID | File | Issue |
|----|------|-------|
| P-L1 | `coordinator/position.rs:148–155` | Three separate `Arc<RwLock<_>>` fields where one combined struct would suffice |
| P-L2 | `coordinator/position.rs:154` | `position_counter` uses `Arc<RwLock<u64>>` instead of `AtomicU64` |
| P-L3 | `coordinator/coordinator.rs:91` | `stale_heartbeat_warn_at` uses `Arc<RwLock<HashMap>>` — `DashMap` would eliminate write lock |
| P-L4 | `coordinator/coordinator.rs:93` | `order_update_sinks` uses blocking `std::sync::RwLock` — should be `tokio::sync::RwLock` |
| P-L5 | `coordinator/coordinator.rs:69,119` | `authorized_agents` uses blocking `std::sync::RwLock` |

**Critical Path Summary:** A passing BUY intent requires ~17 sequential lock acquisitions and 2 DB writes before reaching the queue, all blocking the single coordinator `tokio::select!` loop. Estimated latency: 5–15ms under normal load, 50–200ms under DB pressure.

---

## Critical Issues for Phase 3 Context

1. **F-01 / A-C2 (foreground bypass)**: Live orders bypass all risk controls — needs tests documenting the bypass and verifying the fix.
2. **F-03 (governance not restored)**: Governance state lost on restart — needs integration test for restart round-trip.
3. **F-04 (capital allocator gap)**: Exposure underestimated during recovery — needs test for recovery-time intent submission.
4. **P-C2 (14 sequential DB writes)**: Journal writes block order processing — highest-impact performance fix before next live deployment.
5. **P-H3 (unbounded poll tasks)**: WS reconnects spawn duplicate polling tasks — affects staggered arb live correctness.

---

## Phase 2 Summary

| Category | Critical | High | Medium | Low | Total |
|----------|----------|------|--------|-----|-------|
| Security | 1 | 3 | 4 | 3 | 11 |
| Performance | 2 | 6 | 7 | 5 | 20 |
| **Combined** | **3** | **9** | **11** | **8** | **31** |

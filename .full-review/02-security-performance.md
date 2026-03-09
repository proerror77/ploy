# Phase 2: Security & Performance Review

**Date**: 2026-03-08
**Scope**: Full Ploy trading system

---

## Security Findings

### High (2)

1. **H-01: `ApiCredentials` derives `Debug` — secrets printed in logs/panics** (`signing/hmac.rs:12`)
   - CVSS 7.5, CWE-532. API key, HMAC secret, passphrase dumped via `{:?}` formatting
   - Fix: Custom Debug impl with `[REDACTED]` for secret fields

2. **H-02: HMAC debug log leaks full signing message** (`signing/hmac.rs:106-113`)
   - CVSS 7.1, CWE-532. Full signing payload (timestamp+method+path+body) logged at debug level
   - Fix: Remove `message` field from debug log

### Medium (7)

3. **M-01: `Wallet` derives `Clone` — private key signer copyable** (`signing/wallet.rs:14`)
4. **M-02: No API rate limiting** — brute-force/DoS on all endpoints including login
5. **M-03: Missing security headers** — no X-Content-Type-Options, X-Frame-Options, HSTS
6. **M-04: WebSocket auth token in query parameter** — logged, cached, visible in history
7. **M-05: `DatabaseConfig` derives `Debug`** — connection URL with credentials exposed
8. **M-06: Emergency stop `is_stopped` uses `Ordering::Relaxed`** — may miss stop signal on ARM
9. **M-07: Sidecar risk guard uses substring match** — bypassable with new tool names

### Low (6)

10. L-01: KalshiConfig stores API key without zeroization
11. L-02: GrokConfig stores API key as plain String with Debug derive
12. L-03: Prompt injection mitigation incomplete (500-char truncation only)
13. L-04: CORS allows localhost:5173 by default in production
14. L-05: stop-trading.yml writes SSH key to disk (cleanup on failure not guaranteed)
15. L-06: Deprecated `private_key_hex()` still in public API

### Positive

- All SQL parameterized — zero injection risk
- Constant-time token comparison with SHA-256 fingerprinting
- HttpOnly/SameSite=Strict/Secure cookies
- Autonomous agent direct order submission correctly disabled
- Dry-run defaults to true
- Dependencies are current versions with no known CVEs

---

## Performance Findings

### Critical (1)

1. **P-01: Double `record_success` call inflates PnL tracking** (`coordinator.rs:4268`)
   - Line 4268 unconditionally calls `record_success` after the if/else block that already handles both branches
   - Profitable trades: PnL inflated 2x. Losing trades: negative PnL passed to record_success
   - Fix: Remove line 4268 entirely

### High (3)

2. **P-02: Unnecessary `Arc<RwLock<>>` on MomentumStrategyAdapter** (`adapters.rs:52-86`)
   - Strategy trait takes `&mut self` — locks add pure overhead (~2-5μs per update)
3. **P-03: Unbounded HashMap growth in strategy state** (multiple files)
   - `archived_live_orders`, `entry_reject_counts`, `cooldowns` never pruned — OOM risk on 3.4GB server
4. **P-04: WebSocket UI polls database every 1 second** (`api/state.rs`)
   - 1s latency floor, constant DB load even when idle

### Medium (5)

5. P-05: Sequential RwLock acquisition in `handle_order_intent` (~10-20μs overhead)
6. P-06: PostgresStore uses string SQL without compile-time checks
7. P-07: No connection pool size tuning documentation
8. P-08: 470-line entry evaluation on hot path (staggered_arb_live.rs)
9. P-09: Circuit breaker uses RwLock for timestamp (could be AtomicI64)

### Low (4)

10. P-10: `to_f64().unwrap_or(0.0)` pattern on hot path (silent precision loss)
11. P-11: bootstrap.rs 7,761 lines impacts incremental compilation
12. P-12: No Prometheus metrics exposition
13. P-13: DashMap in QuoteCache never shrinks

### Positive

- DashMap for lock-free concurrent reads on price data
- Tokio select! loop well-structured in coordinator
- Broadcast channels for zero-copy market data fan-out
- Feature-gated heavy dependencies reduce production binary size

---

## Critical Issues for Phase 3 Context

### Testing implications:
- P-01 (double record_success) needs a regression test for PnL accounting
- M-06 (emergency stop ordering) needs a concurrency test on ARM
- H-01/H-02 (credential logging) need tests asserting Debug output doesn't contain secrets
- P-03 (unbounded growth) needs long-running soak tests

### Documentation implications:
- Security headers and rate limiting configuration should be documented
- Connection pool tuning guidance needed
- Emergency stop behavior and memory ordering guarantees should be documented
- API versioning strategy needs an ADR

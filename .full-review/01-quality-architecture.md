# Phase 1: Code Quality & Architecture Review

**Date**: 2026-03-08
**Scope**: Full Ploy trading system (~165K lines Rust, 260+ files, plus TS sidecar and React frontend)

---

## Code Quality Findings

### Critical (2)

1. **Silently discarded position-tracking errors** (`coordinator.rs:2734,4229`)
   - `let _ = self.positions.open_position(...)` discards Result, causing invisible exposure drift
   - Fix: Log error and optionally trigger position reconciliation

2. **`bootstrap.rs` god module** (7,761 lines)
   - Mixes DDL, env parsing, trade alerts, market resolution, account upsert, and bootstrap orchestration
   - Fix: Split into bootstrap.rs (~500 lines), schema.rs, env_helpers.rs, trade_alerts.rs

### High (8)

3. **Series ID magic numbers duplicated across 6+ files** — adding a symbol requires shotgun surgery
4. **Env parsing helpers reimplemented in 7+ files** with inconsistent signatures
5. **`staggered_arb_live.rs` 470-line entry method** with 15+ sequential filter gates
6. **`coordinator.rs` at 6,508 lines** — governance and execution logic need extraction
7. **45 `#[allow(dead_code)]` annotations** across 19 files indicate abandoned features
8. **Inline DDL in 9 files** instead of proper SQL migrations (shadow schema)
9. **`autonomous.rs` TODO** indicates autonomous trading bypasses RiskManager
10. **Three competing agent abstractions** (DomainAgent, TradingAgent, Strategy) with overlapping signatures

### Medium (12)

11. `MomentumConfig` has 30+ fields in flat struct — needs nested config groups
12. `fetch_orders_paginated`/`fetch_trades_paginated` near-identical pagination logic
13. `database_url_from_env()` pattern duplicated in 3+ files
14. `StaggeredArbAdapter` struct has 25+ fields — data clump smell
15. `MomentumStrategyAdapter` wraps everything in `Arc<RwLock<>>` unnecessarily (trait takes `&mut self`)
16. `on_market_update` deeply nested match arms (7 levels)
17. TODO/FIXME comments indicate unfinished work (autonomous risk, RL order execution)
18. `PriceCache` name collision between split_arb and adapters
19. `PloyError::Other(anyhow::Error)` catch-all bypasses typed error hierarchy
20. Type duplication — Position (5x), PositionStatus (3x), ArbStats (2x), RiskLevel (2x)
21. `parse_boolish` duplicated 7 times across API and adapter files
22. Strategy-specific config embedded in AppConfig instead of strategy-level configs

### Low (5)

23. Deprecated `private_key_hex()` still exists (returns empty string)
24. `config.rs` at 1,168 lines is a flat config monolith
25. `to_f64().unwrap_or(0.0)` pattern masks precision failures silently
26. `OrderError`/`RiskError` lose type information when converted to `PloyError`
27. `rand = "0.8"` — version 0.9 has been stable since early 2025

---

## Architecture Findings

### Critical (1)

1. **Bootstrap.rs god module** (7,761 lines) — composition root that has grown to contain business logic, DDL, persistence pipelines, and configuration parsing

### High (3)

2. **Three-layer agent abstraction** — DomainAgent (push), TradingAgent (pull), Strategy (event-driven) overlap in purpose; DomainAgent and TradingAgent share identical method signatures but aren't unified
3. **Type duplication across modules** — Position defined 5 times, PositionStatus 3 times, with alias re-exports as symptom treatment
4. **Shadow schema via bootstrap DDL** — CREATE TABLE statements in bootstrap.rs not tracked by sqlx migrate

### Medium (8)

5. Strategy module sprawl — 40+ flat submodules, 230+ re-exports from single mod.rs
6. Platform vs. Coordinator overlap in order execution orchestration
7. No API versioning — breaking changes require coordinated multi-service deploys
8. Inconsistent API error contracts — mix of ad-hoc JSON and typed responses
9. Circuit breaker logic duplicated in RiskGate and TradingCircuitBreaker
10. PostgresStore monolith — 1,364 lines, no compile-time SQL checks
11. Configuration consistency — strategy-specific fields in AppConfig, TOML float parsing gotcha
12. Strategies import concrete adapter types instead of abstractions

### Low (4)

13. WebSocket UI updates poll DB every 1s instead of event-driven emission
14. `parse_boolish` duplicated 7 times
15. No standard metrics exposition (Prometheus)
16. Module dependency direction — bootstrap imports from nearly every module

### Positive Patterns

- **EngineStore trait** — Clean DI for database access
- **Subscription Planner** — Functional delta computation for WS subscriptions
- **Data Plane + Freshness** — Shared market data with staleness monitoring
- **Safety gate** — Coordinator-only live trading enforcement
- **Event sourcing + DLQ** — Production-grade crash recovery with DB-level guards
- **Drift-safe migrations** — Schema detection before DDL
- **Feature-gated compilation** — Heavy deps behind feature flags
- **Governance policy** — Runtime-adjustable trading constraints with audit trail
- **Wallet security** — zeroize, constant-time auth comparison, explicit live order opt-in
- **Risk gate** — Multi-layer checks (agent, platform, domain, daily loss, drawdown, circuit breaker)
- **Optimistic locking** — Version numbers prevent concurrent order submissions

---

## Critical Issues for Phase 2 Context

### Security-relevant findings:
- `autonomous.rs` bypasses RiskManager — potential for uncontrolled order execution
- `PloyError::Other(anyhow::Error)` escape hatch could mask security-relevant errors
- Inline DDL creates schema drift risk — tables may exist in production but not in migration-created DBs
- `private_key_hex()` deprecated but still in public API

### Performance-relevant findings:
- `Arc<RwLock<>>` wrappers on MomentumStrategyAdapter fields add unnecessary lock contention
- WebSocket UI polls DB every 1s instead of event-driven
- PostgresStore uses string SQL without compile-time checks — runtime SQL errors possible
- `bootstrap.rs` 7,761 lines — compilation unit size may impact incremental build times
- `staggered_arb_live.rs` 470-line entry method — hot path with 15+ sequential checks

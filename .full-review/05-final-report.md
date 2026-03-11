# Comprehensive Code Review Report

**Branch:** `hotfix/staggered-arb-release-20260306` vs `main`
**Review Date:** 2026-03-11
**Phases Completed:** Code Quality, Architecture, Security, Performance, Testing, Documentation, Best Practices, CI/CD

---

## Review Target

Branch `hotfix/staggered-arb-release-20260306` — 10 commits, ~317 source files changed vs `main`.

The branch is a major **coordinator decomposition refactor**: monolithic `bootstrap.rs` (~7,000 lines) and `coordinator.rs` (~6,500 lines) split into focused sub-modules. It also introduces the `staggered_arb_live` live trading strategy, a new `pm_5m_directional` strategy, a new `control_plane` module, and routes foreground CLI intents through the coordinator ingress.

**Framework:** Rust / Tokio / Axum / sqlx (PostgreSQL) — live trading system.

---

## Executive Summary

The coordinator decomposition is architecturally sound and the file structure is significantly improved. However, the refactor has introduced or exposed several production-critical issues that must be resolved before this branch is deployed to a live trading host. The most urgent concerns are: a foreground execution path that bypasses all risk controls, a serialized coordinator loop that saturates at ~20 intents/sec under DB pressure, governance state that is silently lost on every restart, and a legacy CI workflow that may have written the Polymarket private key into a systemd unit file on the production host. None of the five critical code-quality findings from Phase 1 have been addressed in this branch.

---

## Findings by Priority

### P0 — Critical: Must Fix Before Deployment (9 unique findings)

| ID | Category | File | Issue |
|----|----------|------|-------|
| **R-01** | Security / Architecture | `cli/strategy/runtime_ops/foreground.rs`, `foreground_submit.rs` | **Foreground bypass**: `ForegroundIntentSubmitter::submit` executes live CLOB orders when `deployment_id` is absent in metadata, bypassing admission, risk gate, governance, capital allocator, circuit breaker, and journal. No tests cover the lifecycle or direct-executor fallback. |
| **R-02** | Performance | `coordinator/coordinator.rs:237–274` | **Serialized coordinator loop**: `handle_order_intent` is awaited inline in the single `tokio::select!` loop. At 50ms DB latency, throughput saturates at ~20 intents/sec. All queue drain ticks, state refresh ticks, and shutdown signals are blocked during processing. |
| **R-03** | Performance | `coordinator/coordinator/ingress.rs` (14 call sites) | **14 sequential DB writes per intent**: Every `persist_risk_decision` call on the hot path is awaited sequentially, blocking the coordinator loop for the duration of all DB round-trips. |
| **R-04** | Code Quality | `coordinator/position/transitions.rs:52–72` | **Lock-ordering deadlock**: `close_position` acquires `positions` write lock then `realized_pnl` write lock while the first is held. `reduce_position` acquires them in the opposite order. Concurrent close + reduce will deadlock. No test covers this. |
| **R-05** | Code Quality | `coordinator/risk/transitions.rs:107` | **Risk state corruption**: `record_loss` does not reset `consecutive_failures` or increment daily loss counters. After a loss-realizing sell between two failures, the circuit breaker counter is wrong. No test covers this path. |
| **R-06** | Security / Performance | `coordinator/risk/transitions.rs:68–71` | **TOCTOU circuit breaker bypass**: `record_success` reads `state`, releases the lock, then writes `Normal`. A concurrent `trigger_circuit_breaker` between the two lock acquisitions is silently overwritten, re-enabling trading after a circuit-breaker trip. |
| **R-07** | Security / Architecture | `coordinator/governance.rs`, `coordinator/recovery.rs:55–72` | **Governance state lost on restart**: Governance state is not restored unless `governance_store_pool` is explicitly wired. Operator-set domain pauses are silently lost on every OOM kill or deploy. With `Restart=always`, this means pauses never survive a crash. |
| **R-08** | CI/CD | `.github/workflows/deploy-prebuilt.yml` | **Private key on disk**: `deploy-prebuilt.yml` writes `POLYMARKET_PRIVATE_KEY` and `GROK_API_KEY` directly into `Environment=` lines in the systemd unit file. The key is readable by any root process and visible in `systemctl show`. Audit tango-1-1 immediately. |
| **R-09** | Best Practices | `coordinator/coordinator.rs:69,93` | **`std::sync::RwLock` in async context**: Blocks the Tokio worker thread when contended. Lock poisoning on panic silently drops order update registrations. Affects `authorized_agents` and `order_update_sinks`. |

---

### P1 — High: Fix Before Next Live Deployment (16 unique findings)

| ID | Category | File | Issue |
|----|----------|------|-------|
| **R-10** | Code Quality | `coordinator/coordinator/recovery.rs:77` | Unbounded fill replay on restart — replays ALL historical fills, creating phantom positions and blocking ingress for seconds/minutes on a long-running system. |
| **R-11** | Architecture | `coordinator/coordinator/ingress.rs:343` | `pending_buy_notional_excluding_domains` excludes ALL known domains — result is always zero. Capital notional limit is never enforced. |
| **R-12** | Architecture / Security | `coordinator/capital/market.rs`, `coordinator/recovery.rs:77–231` | Capital allocator exposure gap: after a fill, risk gate shows $0 exposure during recovery replay, enabling 2x exposure limit breach. |
| **R-13** | Performance | `coordinator/strategy_runtime/actions.rs:337–355` | Unbounded `spawn_split_arb_poll_task`: every order update spawns a new 600s polling task with no dedup or cancellation on WS reconnect. Duplicate fills can be processed. |
| **R-14** | Testing | `staggered_arb_live` | No integration test for staggered arb intent flowing through coordinator risk gate. |
| **R-15** | Testing | `staggered_arb_live` | No test for leg2 timeout / force-close path in live (non-dry-run) mode. |
| **R-16** | Testing | `coordinator/bootstrap.rs` | `start_platform` bootstrap wiring is untested — only config rendering is tested. |
| **R-17** | Testing | `coordinator/journal/restore.rs` | No end-to-end cold-start restore integration test (fill replay → position aggregator → risk counters). |
| **R-18** | Documentation | `coordinator/capital.rs`, `governance.rs`, `journal.rs` | Zero `//!` module-level docs on three non-trivial coordinator sub-modules. |
| **R-19** | Documentation | `src/control_plane/` | New module has no `//!` docs at all — role in four-layer architecture not explained. |
| **R-20** | Documentation | `CLAUDE.md` / `AGENTS.md` | Architecture decomposition not documented — no mention of coordinator sub-modules, canonical order path, or foreground vs managed runtime distinction. |
| **R-21** | CI/CD | `test.yml` | No `cargo audit` / `cargo deny` — zero dependency vulnerability scanning for a system handling private keys and financial transactions. |
| **R-22** | CI/CD | `deploy-tango21.yml`, `deploy.yml`, `release.yml` | Legacy workflows build Rust on-host (violating deployment policy) and target wrong architecture (x86_64 vs aarch64). |
| **R-23** | CI/CD | Secrets | No secrets rotation procedure documented. `deploy-prebuilt.yml` may have left private key in systemd unit on tango-1-1 (see R-08). |
| **R-24** | CI/CD | `stop-trading.yml`, `get-logs.yml` | `StrictHostKeyChecking=no` in 3 workflows — SSH MITM risk against the trading host. |
| **R-25** | CI/CD | `docs/runbooks/` | No runbooks for governance restore (R-07), foreground bypass warning (R-01), circuit breaker reset (R-06), or emergency stop. |

---

### P2 — Medium: Plan for Next Sprint (20 unique findings)

| ID | Category | File | Issue |
|----|----------|------|-------|
| **R-26** | Architecture | `coordinator/coordinator.rs` | Coordinator struct has 12 direct `Arc<RwLock<_>>` fields — structural God Object despite file decomposition. |
| **R-27** | Architecture | `coordinator/bootstrap/startup_context.rs` | Bootstrap initialization order is implicit — no compile-time enforcement (typestate pattern would prevent misconfiguration). |
| **R-28** | Performance | `coordinator/coordinator/ingress.rs:72,107,121` | 4 separate governance lock acquisitions per intent — needs `governance_snapshot()`. |
| **R-29** | Performance | `coordinator/risk/checks.rs:13–102` | `check_order` acquires 7 sequential read locks — needs `risk_snapshot()`. |
| **R-30** | Performance | `coordinator/coordinator/execution.rs:180–232` | `get_agent_positions` does full O(n) scan across all agents on every sell fill. |
| **R-31** | Performance | `coordinator/journal.rs:66–175` | 4 sequential DB writes per fill — should be `tokio::join!`. |
| **R-32** | Performance | `coordinator/capital/market/accounting.rs:294–321` | `market_cap_for` builds `HashSet` on every `reserve_buy` call. |
| **R-33** | Testing | `coordinator/coordinator/ingress.rs:339–351` | `pending_buy_notional_excluding_domains` all-domains-excluded behavior undocumented — always returns zero for known domains. |
| **R-34** | Testing | `coordinator/position.rs` | No concurrent enqueue/dequeue test for `OrderQueue` under load. |
| **R-35** | Testing | `staggered_arb_live` | No test for partial fill → cancel lifecycle sequence. |
| **R-36** | Testing | `coordinator/journal/restore.rs` | No test for restore with corrupt fills (zero shares, bad domain, negative price). |
| **R-37** | Documentation | `staggered_arb_live/*.rs` | Six sub-modules (entry, leg2, lifecycle, order_updates, runtime_flow) have no `//!` docs despite being 19–66KB each. |
| **R-38** | Documentation | `docs/strategies/staggered_arb_state_machine.md` | State machine doc covers paper path only — `LiveOrderTrack` lifecycle, coordinator integration, and `--foreground` vs managed runtime not documented. |
| **R-39** | Best Practices | `coordinator/journal.rs`, `governance.rs`, `observability.rs` | `sqlx::query` runtime strings instead of `sqlx::query!` compile-time macros — type mismatches only discovered at runtime. |
| **R-40** | Best Practices | `coordinator/strategy_runtime/order_store.rs` | `async_trait` macro used where native async traits suffice (Rust 1.75+) — adds `Box<dyn Future>` allocations. |
| **R-41** | CI/CD | `rollback.yml` | Targets `secrets.EC2_HOST` (AWS EC2) not Aliyun tango-1-1 — rollback during incident would target wrong host. |
| **R-42** | CI/CD | `release-aliyun.yml` | Post-deploy health check uses `|| true` — failed health does not abort deploy. |
| **R-43** | CI/CD | `release-aliyun.yml` | Migration applied via raw `psql` without tracking; not idempotent; no rollback migration. |
| **R-44** | CI/CD | Observability | No Prometheus metrics endpoint; no alerting on order failures or circuit breaker state changes. |
| **R-45** | CI/CD | Environments | No staging environment; tango-2-1 available but unused. `rollback.yml` and several workflows hardcode infrastructure values. |

---

### P3 — Low: Track in Backlog (14 unique findings)

| ID | Category | Issue |
|----|----------|-------|
| **R-46** | Code Quality | `staggered_arb_live/entry.rs:76` — `try_entry_for_window` is 250 lines, cyclomatic complexity >20 |
| **R-47** | Code Quality | `coordinator/coordinator/ingress.rs` — 4-step rejection pattern repeated 9 times (DRY violation) |
| **R-48** | Architecture | `coordinator/strategy_runtime/` — two separate action dispatch paths with overlapping implementations |
| **R-49** | Security | `admission/deployments.rs:34–62` — `PLOY_DEPLOYMENTS_FILE` env var accepts arbitrary path (CWE-73) |
| **R-50** | Security | `sidecar/grok_decision.rs:59–96` — Grok prompt embeds unvalidated free-text fields (CWE-77) |
| **R-51** | Security | `api/auth.rs:87–91` — admin token fingerprint uses unsalted SHA-256 (CWE-916) |
| **R-52** | Testing | `coordinator/governance.rs` — no test for `set_global_mode` clearing domain-level overrides |
| **R-53** | Testing | `coordinator/bootstrap/tests.rs` — deprecated env var conflict test covers only one direction |
| **R-54** | Documentation | `cli/strategy/runtime_ops/foreground.rs` — no `//!` block warning that foreground bypasses coordinator risk gate |
| **R-55** | Documentation | No `CHANGELOG.md` — breaking changes (bootstrap decomposition, new `control_plane`, `TradeIntent` type) not documented |
| **R-56** | Best Practices | `coordinator/risk/transitions.rs:15,76` — `Ordering::SeqCst` where `Relaxed` suffices (unnecessary memory fence) |
| **R-57** | Best Practices | `Cargo.toml:83` — `rand = "0.8"` two major versions behind (current: 0.9.x) |
| **R-58** | Best Practices | `Cargo.toml:60–61` — `ethers-core`/`ethers-signers` v2 deprecated; project already has `alloy` |
| **R-59** | CI/CD | `release-aliyun.yml` — no `StartLimitIntervalSec`/`StartLimitBurst`; crash loop not bounded |

---

## Findings by Category

| Category | Critical | High | Medium | Low | Total |
|----------|----------|------|--------|-----|-------|
| Code Quality | 3 | 3 | 2 | 0 | 8 |
| Architecture | 2 | 4 | 2 | 1 | 9 |
| Security | 2 | 2 | 3 | 1 | 8 |
| Performance | 2 | 4 | 5 | 0 | 11 |
| Testing | 5 | 4 | 3 | 2 | 14 |
| Documentation | 0 | 3 | 3 | 2 | 8 |
| Best Practices | 1 | 0 | 2 | 3 | 6 |
| CI/CD & DevOps | 1 | 5 | 7 | 2 | 15 |
| **Total** | **16** | **25** | **27** | **11** | **79** |

*(Raw phase totals: 123 findings across 4 phases; 79 unique after deduplication)*

---

## Recommended Action Plan

### Immediate (before any live deployment)

1. **Audit tango-1-1 for private key in systemd unit** (R-08) — run `grep -r POLYMARKET /etc/systemd/system/` on the host. Rotate the key if found. `[small]`

2. **Fix `rollback.yml` target host** (R-41) — change `secrets.EC2_HOST` to `secrets.ALIYUN_ECS_HOST` and update paths. `[small]`

3. **Remove `|| true` from post-deploy health check** (R-42) — a failed health check must abort the deploy. `[small]`

4. **Add `cargo audit` to `test.yml`** (R-21) as a blocking step. `[small]`

5. **Replace `StrictHostKeyChecking=no`** (R-24) with `known_hosts` verification in all 3 workflows. `[small]`

### Sprint 1 — Critical correctness and safety

6. **Fix foreground bypass** (R-01): Either route all foreground intents through the coordinator risk gate, or add a hard guard that refuses to execute live orders when `deployment_id` is absent. Add tests for the full lifecycle. `[medium]`

7. **Fix `record_loss` risk state corruption** (R-05): Reset `consecutive_failures` and increment daily counters in `record_loss`. Add test for loss-realizing sell between two failures. `[small]`

8. **Fix TOCTOU in `record_success`** (R-06): Acquire a single write lock and check-then-set atomically. Add concurrent test. `[small]`

9. **Fix `close_position` lock ordering** (R-04): Drop `positions` lock before acquiring `realized_pnl`. Add concurrent close + reduce test with timeout. `[small]`

10. **Fix governance restore on restart** (R-07): Ensure `governance_store_pool` is always wired in bootstrap; restore governance state during recovery. Add DB-backed restart round-trip test. `[medium]`

11. **Write governance restore runbook** (R-25): Document safe restart procedure including governance state backup/restore, foreground bypass warning, circuit breaker reset, and emergency stop. `[small]`

### Sprint 2 — Performance and throughput

12. **Fire-and-forget journal writes** (R-03): Replace 14 sequential `persist_risk_decision` awaits with `tokio::spawn` or a dedicated journaling channel. `[medium]`

13. **Decouple coordinator loop** (R-02): Move `handle_order_intent` off the main `select!` loop — spawn a task per intent or use a dedicated ingress worker. `[large]`

14. **Fix `pending_buy_notional` always-zero bug** (R-11): Remove the all-domains exclusion or fix the domain-matching logic. `[small]`

15. **Dedup poll task spawning** (R-13): Track `AbortHandle` per `exchange_order_id` in a `DashMap`; abort existing task before spawning. `[small]`

16. **Replace `std::sync::RwLock` with `tokio::sync::RwLock`** (R-09) for `authorized_agents` and `order_update_sinks`. `[small]`

17. **Bound fill replay on restart** (R-10): Add a `since` timestamp parameter to the fill query; only replay fills from the last N hours or since last checkpoint. `[medium]`

### Sprint 3 — Testing and documentation

18. **Add coordinator integration tests** (R-14, R-16, R-17): Wire coordinator → risk → position → journal end-to-end. Test staggered arb intent through risk gate. Test cold-start restore. `[large]`

19. **Add `//!` module docs** (R-18, R-19, R-20): Document `capital.rs`, `governance.rs`, `journal.rs`, `control_plane/`. Update `CLAUDE.md` with coordinator sub-module architecture and foreground vs managed runtime distinction. `[medium]`

20. **Migrate to `sqlx::query!` macros** (R-39): Start with journal and governance queries. `[medium]`

21. **Add `cargo deny` and staging environment** (R-21, R-45): Add `deny.toml` with license and advisory checks. Designate tango-2-1 as staging. `[medium]`

22. **Fix capital allocator exposure tracking** (R-12): Track pending reservations separately from realized positions; do not reset to zero after fill. `[large]`

---

## Review Metadata

- **Review date:** 2026-03-11
- **Branch:** `hotfix/staggered-arb-release-20260306` vs `main`
- **Phases completed:** Code Quality, Architecture, Security, Performance, Testing, Documentation, Best Practices, CI/CD
- **Flags:** Performance Critical
- **Raw findings:** 123 (across 4 phases, with cross-phase confirmation)
- **Unique findings:** 59 (after deduplication)
- **Output files:**
  - `.full-review/00-scope.md`
  - `.full-review/01-quality-architecture.md`
  - `.full-review/02-security-performance.md`
  - `.full-review/03-testing-documentation.md`
  - `.full-review/04-best-practices.md`
  - `.full-review/05-final-report.md`

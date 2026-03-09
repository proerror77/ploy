# Comprehensive Code Review Report

## Review Target

Full Ploy trading system codebase — a Polymarket prediction market trading bot.
- ~165K lines Rust (260+ source files), TypeScript sidecar, React frontend
- Frameworks: Tokio, Axum, sqlx, DashMap, alloy/ethers
- Deployed to Alibaba Cloud (tango-1-1, ARM) and AWS Tokyo (tango-2-1, x86)
- Trades real money on Polymarket CLOB

## Executive Summary

The Ploy trading system has strong architectural foundations — multi-layer risk management, event sourcing with DLQ, parameterized SQL throughout, and defense-in-depth safety gates. However, the review uncovered 5 critical issues that require immediate attention: a double PnL recording bug inflating risk gate accounting, silently discarded position-tracking errors, an x86 binary being deployed to an ARM production host, a 7,761-line god module, and missing root documentation. The system's ~796 tests provide good coverage for pure computation but leave the order execution pipeline — the core revenue path — completely untested end-to-end. The CI/CD pipeline has significant gaps: no staging environment, no dependency scanning, and feature flag mismatches between test and production builds.

## Findings by Priority

### Critical Issues (P0 — Must Fix Immediately)

| ID | Phase | Finding | Impact |
|----|-------|---------|--------|
| P-01 | Perf | Double `record_success` at coordinator.rs:4268 inflates PnL 2x | Risk gate accounting corrupted; may allow/block trades incorrectly |
| Q-01 | Quality | `let _ = self.positions.open_position(...)` silently discards errors | Invisible position drift; exposure tracking becomes stale |
| CICD-01 | CI/CD | x86 binary deployed to ARM host (release-aliyun.yml) | Binary cannot execute on primary production server |
| Q-02 | Quality | bootstrap.rs god module (7,761 lines) | Unmaintainable; mixes DDL, env parsing, alerts, orchestration |
| D-01 | Docs | No root-level README.md | No entry point for understanding the system |

### High Priority (P1 — Fix Before Next Release)

| ID | Phase | Finding |
|----|-------|---------|
| H-01 | Security | `ApiCredentials` derives `Debug` — secrets dumped to logs (CVSS 7.5) |
| H-02 | Security | HMAC debug log leaks full signing message (CVSS 7.1) |
| P-02 | Perf | Unnecessary `Arc<RwLock<>>` on MomentumStrategyAdapter (~2-5μs per update) |
| P-03 | Perf | Unbounded HashMap growth in strategy state — OOM risk on 3.4GB server |
| P-04 | Perf | WebSocket UI polls DB every 1s instead of event-driven |
| Q-03 | Quality | Series ID magic numbers duplicated across 6+ files |
| Q-04 | Quality | Env parsing helpers reimplemented in 7+ files |
| Q-05 | Quality | staggered_arb_live.rs 470-line entry method with 15+ filter gates |
| Q-06 | Quality | coordinator.rs at 6,508 lines needs extraction |
| Q-07 | Quality | 45 `#[allow(dead_code)]` annotations — abandoned features |
| Q-08 | Quality | Inline DDL in 9 files — shadow schema outside migrations |
| Q-09 | Quality | Three competing agent abstractions with overlapping signatures |
| Q-10 | Arch | Type duplication — Position (5x), PositionStatus (3x) |
| T-01 | Testing | No integration test for order execution pipeline |
| T-02 | Testing | No tests for position tracking error handling |
| T-03 | Testing | No regression test for PnL accounting correctness |
| T-04 | Testing | PostgresStore (1,364 lines) has zero unit tests |
| T-05 | Testing | Emergency stop has no concurrency test |
| T-06 | Testing | No tests assert Debug output doesn't contain secrets |
| T-07 | Testing | Staggered arb entry evaluation has no targeted tests |
| D-02 | Docs | No Architecture Decision Records (ADRs) |
| D-03 | Docs | API endpoints undocumented (20+ endpoints, no schemas) |
| D-04 | Docs | Inline documentation sparse (0.7% density) |
| D-05 | Docs | Configuration documentation incomplete |
| BP-02 | Best Prac | `std::sync::Mutex` in async `AutonomousAgent` — deadlock risk |
| BP-03 | Best Prac | No typed API error responses — `(StatusCode, String)` everywhere |
| BP-04 | Best Prac | Zero compile-time checked SQL (297 string queries, 0 macros) |
| CICD-02 | CI/CD | Feature flag mismatch between test and production builds |
| CICD-03 | CI/CD | No dependency vulnerability scanning |
| CICD-04 | CI/CD | No staging environment — direct to production |
| CICD-05 | CI/CD | Hardcoded secrets in workflow files |
| CICD-06 | CI/CD | SSH key written to disk on runners |
| CICD-11 | CI/CD | No blue-green or canary deployment |
| CICD-14 | CI/CD | No infrastructure as code |
| CICD-16 | CI/CD | Prometheus metrics exist but no scraper configured |
| CICD-19 | CI/CD | No runbooks or on-call procedures |
| CICD-20 | CI/CD | Emergency stop only covers AWS Docker, not primary Aliyun |

### Medium Priority (P2 — Plan for Next Sprint)

| ID | Phase | Finding |
|----|-------|---------|
| M-01 | Security | `Wallet` derives `Clone` — private key signer copyable |
| M-02 | Security | No API rate limiting on any endpoint |
| M-03 | Security | Missing security headers (X-Content-Type-Options, HSTS, etc.) |
| M-04 | Security | WebSocket auth token in query parameter |
| M-05 | Security | `DatabaseConfig` derives `Debug` — connection URL exposed |
| M-06 | Security | Emergency stop `is_stopped` uses `Ordering::Relaxed` |
| M-07 | Security | Sidecar risk guard uses bypassable substring match |
| P-05 | Perf | Sequential RwLock acquisition in handle_order_intent |
| P-06 | Perf | PostgresStore uses string SQL without compile-time checks |
| P-07 | Perf | No connection pool size tuning documentation |
| P-08 | Perf | 470-line entry evaluation on hot path |
| P-09 | Perf | Circuit breaker uses RwLock for timestamp (could be AtomicI64) |
| T-08 | Testing | No soak tests for memory growth |
| T-09 | Testing | Integration tests use `unsafe` env manipulation |
| T-10 | Testing | No test for WebSocket authentication flow |
| T-11 | Testing | Architecture gateway test uses fragile string scanning |
| T-12 | Testing | No property-based tests for financial calculations |
| D-06 | Docs | Module CLAUDE.md files serve AI context, not human docs |
| D-07 | Docs | Deployment documentation fragmented |
| D-08 | Docs | Strategy documentation scattered |
| D-09 | Docs | Migration documentation missing |
| BP-05–18 | Best Prac | 14 medium findings (env vars, error handling, async patterns, etc.) |
| CICD-07–10 | CI/CD | Deploy ordering, on-host builds, deprecated actions, no timeouts |
| CICD-12,15,17,18 | CI/CD | Rollback gaps, S3 leaks, no logging/notifications |
| CICD-21–23,25 | CI/CD | Config parity, service inconsistencies, version, workflow duplication |

### Low Priority (P3 — Track in Backlog)

| ID | Phase | Finding |
|----|-------|---------|
| L-01–06 | Security | KalshiConfig/GrokConfig key storage, prompt injection, CORS, SSH key, deprecated API |
| P-10–13 | Perf | to_f64 precision loss, bootstrap compile time, no Prometheus, DashMap shrink |
| T-13–15 | Testing | No frontend tests, backtest assertions, circuit breaker transitions |
| D-10–12 | Docs | No changelog, review findings untracked, sidecar undocumented |
| BP-19–30 | Best Prac | 12 low findings (dead code, glob exports, dependency freshness, etc.) |
| CICD-13,24 | CI/CD | No backup rotation, no changelog |

## Findings by Category

| Category | Critical | High | Medium | Low | Total |
|----------|----------|------|--------|-----|-------|
| Code Quality | 2 | 8 | 12 | 5 | 27 |
| Architecture | 1 | 3 | 8 | 4 | 16 |
| Security | 0 | 2 | 7 | 6 | 15 |
| Performance | 1 | 3 | 5 | 4 | 13 |
| Testing | 0 | 7 (3 crit-equiv) | 5 | 3 | 15 |
| Documentation | 0 | 5 (2 crit-equiv) | 4 | 3 | 12 |
| Best Practices | 0 | 4 | 14 | 12 | 30 |
| CI/CD & DevOps | 1 | 9 | 12 | 2 | 25 |
| **Total** | **5** | **41** | **67** | **39** | **153** |

## Recommended Action Plan

### Immediate (This Week)

1. **Remove duplicate `record_success`** at coordinator.rs:4268 — one-line fix, prevents PnL corruption (P-01) [small]
2. **Fix release-aliyun.yml architecture** — cross-compile for aarch64 or use ARM runner (CICD-01) [medium]
3. **Custom Debug for `ApiCredentials`** — redact secrets, add regression test (H-01, T-06) [small]
4. **Remove HMAC signing message from debug log** (H-02) [small]
5. **Fix emergency stop ordering** — change `Relaxed` to `Acquire` (M-06) [small]
6. **Log position tracking errors** — replace `let _ =` with proper error handling (Q-01) [small]

### This Sprint

7. **Add order execution integration test** — mock exchange, verify PnL accounting end-to-end (T-01, T-03) [medium]
8. **Remove `Arc<RwLock<>>` from MomentumStrategyAdapter** — trait takes `&mut self` (P-02) [small]
9. **Add HashMap pruning** to staggered arb and momentum strategies (P-03) [medium]
10. **Align CI feature flags** with production build features (CICD-02) [small]
11. **Add `cargo audit`** to test workflow (CICD-03) [small]
12. **Create root README.md** with architecture overview (D-01) [medium]
13. **Replace `std::sync::Mutex`** with `tokio::sync::Mutex` in AutonomousAgent (BP-02) [small]

### Next Sprint

14. **Event-driven WebSocket updates** instead of DB polling (P-04) [large]
15. **Add API rate limiting** via tower middleware (M-02) [medium]
16. **Add security headers** middleware (M-03) [small]
17. **Create typed `ApiError`** response type and auth extractors (BP-03) [medium]
18. **Add staging environment** or dry-run validation step (CICD-04) [large]
19. **Create emergency stop workflow for Aliyun** (CICD-20) [small]
20. **Write ADRs** for coordinator pattern, agent abstractions, bootstrap DDL (D-02) [medium]
21. **Deploy Prometheus + Grafana** to scrape existing metrics endpoint (CICD-16) [medium]

### Backlog

22. Split bootstrap.rs god module (Q-02) [large]
23. Unify agent abstractions (Q-09) [large]
24. Migrate critical SQL to `sqlx::query!` macros (BP-04) [large]
25. Consolidate deployment workflows (CICD-25) [medium]
26. Add infrastructure as code (CICD-14) [large]
27. Create runbooks for top 5 failure scenarios (CICD-19) [medium]
28. Profile and optimize entry evaluation hot path (P-08) [medium]
29. Add property-based tests for financial calculations (T-12) [medium]
30. Document all configuration options (D-05) [medium]

## Review Metadata

- Review date: 2026-03-08
- Phases completed: 1A (Quality), 1B (Architecture), 2A (Security), 2B (Performance), 3A (Testing), 3B (Documentation), 4A (Best Practices), 4B (CI/CD)
- Flags applied: framework=Rust/Tokio/Axum
- Total findings: 153
- Reviewers: claude-opus-4.6 (automated multi-agent review)



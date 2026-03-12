# Phase 4: Best Practices & Standards

**Branch:** `hotfix/staggered-arb-release-20260306` vs `main`
**Date:** 2026-03-11

---

## Framework & Language Findings (from phase4a-best-practices.md)

### Critical (1)

| ID | Area | Issue |
|----|------|-------|
| BP-C1 | `coordinator/coordinator.rs:69,93` | `std::sync::RwLock` used in async context — blocks Tokio worker thread; lock poisoning silently drops registrations |

### High (4)

| ID | Area | Issue |
|----|------|-------|
| BP-H1 | `coordinator/risk/transitions.rs:68–71` | TOCTOU read-then-write on `state` in `record_success` — can overwrite `Halted` with `Normal` (confirms F-02) |
| BP-H2 | `coordinator/coordinator/ingress.rs` (14 call sites) | Sequential `persist_risk_decision` DB writes block coordinator loop (confirms P-C2) |
| BP-H3 | `coordinator/position/transitions.rs:52–72` | Nested write locks in `close_position` — lock-ordering deadlock risk (confirms C1) |
| BP-H4 | `coordinator/strategy_runtime/actions.rs:588` | Unbounded poll-task spawning without dedup or handle tracking (confirms P-H3) |

### Medium (5)

| ID | Area | Issue |
|----|------|-------|
| BP-M1 | `coordinator/risk/transitions.rs:15,76,187,333` | `Ordering::SeqCst` used where `Relaxed` suffices — unnecessary full memory fence |
| BP-M2 | `coordinator/strategy_runtime/order_store.rs:1,9,27` | `async_trait` macro used where native async traits suffice (Rust 1.75+) |
| BP-M3 | `coordinator/queue.rs:129,211,235,248` | `BinaryHeap` O(n log n) rebuild for every filtered mutation |
| BP-M4 | `coordinator/journal.rs`, `governance.rs`, `observability.rs` | `sqlx::query` runtime strings instead of `sqlx::query!` compile-time macros |
| BP-M5 | `coordinator/queue.rs:128–149` | `swap_remove` + heap rebuild pattern is correct but non-obvious; needs comment |

### Low (4)

| ID | Area | Issue |
|----|------|-------|
| BP-L1 | `coordinator/risk/transitions.rs:353–356` | `Vec::drain(0..n)` O(n) shift for ring buffer; use `VecDeque::pop_front()` |
| BP-L2 | `Cargo.toml:83` | `rand = "0.8"` is two major versions behind (current: 0.9.x) |
| BP-L3 | `Cargo.toml:60–61` | `ethers-core`/`ethers-signers` v2 deprecated; project already has `alloy` |
| BP-L4 | `coordinator/strategy_runtime/actions.rs:173,274` | `#[allow(clippy::too_many_arguments)]` on 13-parameter function suppresses design signal |

**Dependency Hygiene:** `bincode = "=2.0.0-rc.3"` pinned to RC (burn compatibility workaround); `async-trait` superseded by native async traits in Rust 1.75+.

---

## CI/CD & DevOps Findings (from phase4b-cicd.md)

### Critical (1)

| ID | Area | Issue |
|----|------|-------|
| D-01 | `deploy-prebuilt.yml` | Writes `POLYMARKET_PRIVATE_KEY` and `GROK_API_KEY` directly into systemd unit `Environment=` lines on disk — readable by any root process, visible in `systemctl show` |

### High (5)

| ID | Area | Issue |
|----|------|-------|
| C-01 | `test.yml` | No `cargo audit` / `cargo deny` — zero dependency vulnerability scanning for a system handling private keys |
| D-02 | `deploy-tango21.yml`, `deploy.yml`, `release.yml` | Legacy workflows build Rust on-host, violating deployment policy; wrong arch (x86_64 vs aarch64) |
| S-01 | Secrets | No secrets rotation procedure; `deploy-prebuilt.yml` may have left private key in systemd unit on tango-1-1 |
| S-02 | `stop-trading.yml`, `get-logs.yml`, `deploy-prebuilt.yml` | `StrictHostKeyChecking=no` in 3 workflows — SSH MITM risk against trading host |
| O-03 | `docs/runbooks/` | No runbooks for governance restore, foreground bypass warning, circuit breaker reset, or emergency stop |

### Medium (10)

| ID | Area | Issue |
|----|------|-------|
| C-02 | `auto-review.yml` | Clippy is advisory-only (`exit 0`); does not block merge |
| C-03 | `test.yml` | `api` feature not tested in CI; production feature set (`api,claimer_daemon,pm_ctf`) untested |
| D-03 | `rollback.yml` | Targets `secrets.EC2_HOST` (AWS EC2) not Aliyun tango-1-1 — rollback during incident would target wrong host |
| D-04 | `release-aliyun.yml` | Post-deploy health check uses `|| true` — failed health does not abort deploy |
| D-05 | `release-aliyun.yml` | Migration applied via raw `psql` without tracking; not idempotent; no rollback migration |
| O-01 | Observability | No Prometheus metrics endpoint; no alerting on order failures or circuit breaker state |
| O-02 | Observability | No log aggregation; logs only accessible via SSH; log loss if host replaced |
| E-01 | Environments | No staging environment; tango-2-1 available but unused |
| E-02 | Environments | EC2 instance IDs and IPs hardcoded in workflow files |
| Sy-01 | Systemd | No `StartLimitIntervalSec`/`StartLimitBurst` — crash loop not bounded |

### Low (3)

| ID | Area | Issue |
|----|------|-------|
| Sy-02 | Systemd | No `ExecStartPre` config/DB validation before service start |
| Cf-01 | Config | Only 2 of 11 strategy configs deployed by CI; rest require manual placement (config drift risk) |
| C-04 | `auto-review.yml` | Advisory-only design not documented as deliberate choice |

**Positives:** `release-aliyun.yml` is well-designed — native ARM64 build, `--locked`, ELF verification, timestamped backup, systemd guardrails matching CLAUDE.md policy, `environment: production` gate, concurrency group.

---

## Phase 4 Summary

| Category | Critical | High | Medium | Low | Total |
|----------|----------|------|--------|-----|-------|
| Best Practices | 1 | 4 | 5 | 4 | 14 |
| CI/CD & DevOps | 1 | 5 | 10 | 3 | 19 |
| **Combined** | **2** | **9** | **15** | **7** | **33** |

# Phase 4: Best Practices & Standards

**Date**: 2026-03-08
**Scope**: Full Ploy trading system

---

## Framework & Language Findings

### High (4)

1. **BP-01: `ApiCredentials` derives `Debug` — secrets in logs** (`signing/hmac.rs:12`) — Duplicate of H-01; confirms the issue from a best-practices angle
2. **BP-02: `std::sync::Mutex` in async `AutonomousAgent`** (`ai_clients/autonomous.rs:125`) — Deadlock risk when lock held across `.await` points
3. **BP-03: No typed API error responses** — 30+ handlers return `(StatusCode, String)` instead of structured error JSON
4. **BP-04: Zero compile-time checked SQL queries** — 297 string `sqlx::query()` calls, 0 `sqlx::query!()` macros; all SQL validated only at runtime

### Medium (14)

5. BP-05: `parse_boolish` duplicated 7 times across API and adapter files
6. BP-06: 252 `std::env::var` calls scattered across 42 files — untestable, unsound in multi-threaded contexts
7. BP-07: `PloyError::Other(anyhow::Error)` catch-all bypasses typed error hierarchy
8. BP-08: `std::sync::RwLock` mixed with tokio locks in CoordinatorHandle
9. BP-09: ~60 spawned tasks with no JoinHandle tracking — panics silently swallowed
10. BP-10: No `CancellationToken` — shutdown via ad-hoc `Arc<RwLock<bool>>` flags
11. BP-11: 60+ `Arc<RwLock<>>` wrappings suggesting over-sharing
12. BP-12: Auth checks are manual function calls, not Axum middleware/extractors
13. BP-13: DDL in application code bypassing migration system (5+ files)
14. BP-14: Manual `Row::get()` instead of typed `FromRow` deserialization
15. BP-15: `async-trait` crate still used (40 occurrences) — native async traits available since Rust 1.75
16. BP-16: Vendored `polymarket-client-sdk` with no documented patch delta
17. BP-17: `bincode` pinned to release candidate `=2.0.0-rc.3`
18. BP-18: `panic = "abort"` in release profile — no Drop cleanup on panic in live trading

### Low (12)

19. BP-19: 45 `#[allow(dead_code)]` annotations across 19 files
20. BP-20: Glob re-exports in `api/handlers/mod.rs`
21. BP-21: `std::thread::sleep` in async code (nba_data_collector.rs)
22. BP-22: Flat router with 40+ routes, no nesting
23. BP-23: `AppState` with 15 fields
24. BP-24: `thiserror` v1 (v2 available)
25. BP-25: `rand` v0.8 (v0.9 available)
26. BP-26: Edition 2021 (2024 available)
27. BP-27: Both `futures` and `futures-util` declared
28. BP-28: No `[workspace.dependencies]` for shared versions
29. BP-29: No MSRV declared
30. BP-30: 17 feature flags with complex dependency chains

---

## CI/CD & DevOps Findings

### Critical (1)

1. **CICD-01: Architecture mismatch — x86 binary deployed to ARM host** (`release-aliyun.yml`) — Binary built on ubuntu-latest (x86_64) cannot execute on tango-1-1 (aarch64)

### High (9)

2. **CICD-02: Feature flag mismatch** — CI tests `--features rl` but production ships `claimer_daemon,api,pm_ctf`
3. **CICD-03: No security scanning** — No `cargo audit`, Dependabot, or dependency vulnerability checks
4. **CICD-04: No staging environment** — Every deploy goes straight to production
5. **CICD-05: Hardcoded secrets in workflow files** — DB credentials in `deploy-prebuilt.yml`
6. **CICD-06: SSH key written to disk** — 3 workflows write private key to runner filesystem
7. **CICD-11: No blue-green or canary deployment** — Stop-replace-start with downtime window
8. **CICD-14: No infrastructure as code** — No Terraform/Pulumi; servers set up imperatively via SSH
9. **CICD-16: Prometheus metrics exist but no scraper** — `/metrics` endpoint implemented but nothing reads it
10. **CICD-19: No runbooks** — No documented procedures for incident response
11. **CICD-20: Emergency stop only covers AWS Docker** — Primary Aliyun systemd deployment has no emergency stop workflow

### Medium (12)

12. CICD-07: Service stopped before binary uploaded — downtime on failed deploy
13. CICD-08: Rust built on production host (violates own policy)
14. CICD-09: Deprecated GitHub Actions (`actions-rs/toolchain@v1`)
15. CICD-10: No timeout on most deploy jobs
16. CICD-12: Rollback only covers AWS, not Aliyun
17. CICD-15: S3 bucket leak — new bucket per deployment run
18. CICD-17: No centralized logging
19. CICD-18: No deployment notifications
20. CICD-21: Config parity issues between environments
21. CICD-22: Systemd service file inconsistencies (memory limits, security hardening)
22. CICD-23: Version stuck at 0.1.0 in Cargo.toml
23. CICD-25: 7 overlapping deployment workflows with significant duplication

### Low (2)

24. CICD-13: No backup rotation
25. CICD-24: No CHANGELOG

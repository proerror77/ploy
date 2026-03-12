# Phase 4B — CI/CD & Operational Practices Review

**Branch**: `hotfix/staggered-arb-release-20260306` vs `main`
**Date**: 2026-03-11
**Reviewer**: DevOps Agent (Phase 4B)

---

## Executive Summary

The pipeline has a solid structural foundation: a proper build/test/deploy separation, GitHub environment protection on production jobs, systemd guardrails enforced at deploy time, and a rollback workflow. However, several gaps create meaningful operational risk for a live trading system: no security scanning, no post-deploy smoke test that blocks rollback, legacy workflows that build Rust on-host in violation of the deployment policy, secrets embedded in a systemd unit file, and no operational runbooks for the failure modes identified in prior phases (governance restore, foreground bypass, circuit breaker reset).

---

## 1. CI/CD Pipeline

### 1.1 Test Gate (`test.yml`)

**Severity: Medium**

The test job runs `cargo test --locked --features rl` against a live Postgres service container. This is good. However:

- `cargo clippy` and `cargo fmt` are run only in `auto-review.yml` (advisory, non-blocking) — they do **not** block merge. A clippy failure posts a PR comment but does not fail the required status check.
- There is no `cargo deny` or `cargo audit` step anywhere in the pipeline. For a system handling private keys and financial transactions, dependency vulnerability scanning is a baseline requirement.
- The test job does not run with `--all-features`, so the `api` feature (which enables the Axum server and all sidecar endpoints) is not tested in CI. The `rl` feature is tested but `api` is not.
- No integration tests or end-to-end tests exist in CI. All tests are unit-level.

**Recommendation**: Add `cargo deny check` as a required gate in `test.yml`. Add `cargo clippy -- -D warnings` as a blocking step (not advisory). Add a separate test run with `--features api,claimer_daemon,pm_ctf` to cover the production feature set.

### 1.2 Auto-Review (`auto-review.yml`)

**Severity: Low**

Clippy and fmt results are posted as PR comments but both steps use `exit 0` unconditionally — they never fail the workflow. This means a PR with 50 clippy warnings merges cleanly. The advisory-only design is intentional but should be documented as a deliberate choice, not a gap.

**Recommendation**: Either promote clippy to a blocking gate or document explicitly that it is advisory-only and why.

### 1.3 No Security Scanning

**Severity: High**

There is no `cargo audit`, `cargo deny`, Trivy, or any other dependency/vulnerability scan in any workflow. The codebase uses `ethers-core`, `alloy`, `reqwest`, `sqlx`, and `polymarket-client-sdk` — all high-value targets for supply-chain attacks. A compromised dependency could exfiltrate the Polymarket private key.

**Recommendation**: Add a weekly scheduled `cargo audit` job and a `cargo deny check advisories` step in `test.yml`. Pin third-party GitHub Actions to full commit SHAs (currently using `@v4`, `@v1.0.3`, etc. — mutable tags).

---

## 2. Release Process

### 2.1 Primary Path: `release-aliyun.yml`

**Severity: Low (well-designed)**

The Aliyun release workflow is the most mature in the repo:
- Builds on `ubuntu-24.04-arm` (native ARM64, matching tango-1-1's aarch64 target)
- Uses `--locked` to pin `Cargo.lock`
- Verifies ELF format and architecture before deploy
- Creates a timestamped backup (`ploy.bak.<ts>`) before overwriting
- Writes a `10-memory-restart.conf` drop-in enforcing `Restart=always`, `RestartSec=5`, `MemoryHigh=1280M`, `MemoryMax=1536M`, `OOMPolicy=kill`
- Waits for `systemctl is-active` with a 20-attempt / 2s-delay loop
- Requires `environment: production` (GitHub environment protection)
- Concurrency group prevents parallel deploys

One gap: the deploy step runs `rustup toolchain install stable` and creates symlinks on the trading host. This is correct for keeping rustup current but the `pkill -x cargo || true` / `pkill -x rustc || true` lines suggest the host has previously had on-host builds. The policy is correct; the defensive kill is a smell that the policy was not always enforced.

### 2.2 Legacy Workflows: `deploy.yml`, `release.yml`, `deploy-tango21.yml`, `deploy-prebuilt.yml`

**Severity: High**

These four workflows represent earlier generations of the deployment approach and contain serious issues:

- **`deploy-tango21.yml`**: Uploads raw source (`Cargo.toml`, `src/`, etc.) to S3 and runs `cargo build --release` on the EC2 host via SSM. This directly violates the "no build on trading host" policy. The EC2 instance ID (`i-01de34df55726073d`) and IP (`3.112.247.26`) are hardcoded in plaintext.

- **`deploy-prebuilt.yml`**: Generates a systemd unit file with secrets interpolated directly into `Environment=` lines:
  ```
  Environment="POLYMARKET_PRIVATE_KEY=${{ secrets.POLYMARKET_PRIVATE_KEY }}"
  Environment="GROK_API_KEY=${{ secrets.GROK_API_KEY }}"
  ```
  This writes the private key into `/etc/systemd/system/ploy-backend.service` on disk, readable by any process with root access and visible in `systemctl show`. This is a **critical secret exposure pattern**.

- **`deploy.yml`**: Builds x86_64 binary (wrong arch for tango-1-1 which is aarch64), no `--locked` flag, no feature flags matching production.

- **`release.yml`**: Builds x86_64 without production features (`claimer_daemon,api,pm_ctf`).

**Recommendation**: Delete or disable `deploy-tango21.yml`, `deploy-prebuilt.yml`, `deploy.yml`, and `release.yml`. They are superseded by `release-aliyun.yml` and create confusion about the canonical deploy path. At minimum, add a comment header marking them as deprecated and add a branch protection rule that only `release-aliyun.yml` can deploy to the `production` environment.

### 2.3 Rollback (`rollback.yml`)

**Severity: Medium**

The rollback workflow targets `secrets.EC2_HOST` (the old AWS EC2 host) and uses `/opt/ploy/` paths — inconsistent with the Aliyun deployment which uses `$DEPLOY_ROOT` (defaulting to `/root/ploy/`). A rollback triggered during an incident on tango-1-1 would silently target the wrong host.

**Recommendation**: Update `rollback.yml` to use `secrets.ALIYUN_ECS_HOST` and `/root/ploy/` paths, or parameterize the target host. Add a `dry_run` input that prints what would be done without executing.

---

## 3. Configuration Management

### 3.1 Strategy Configs in Git

**Severity: Low**

Strategy TOML files (`staggered_arb.toml`, `momentum.toml`, etc.) contain only trading parameters — no secrets. They are committed to git and deployed as part of the release bundle. This is appropriate.

The `release-aliyun.yml` bundle only includes `momentum.toml` and `staggered_arb.toml`. Other strategy configs (`gamma_scalping.toml`, `pattern_memory.toml`, `pm_5m_directional_default.toml`, etc.) are not deployed by CI — they must be manually placed on the host. This creates a config drift risk where the host has configs that differ from what is in git.

**Recommendation**: Either include all strategy configs in the release bundle, or document explicitly which configs are CI-managed vs. manually managed.

### 3.2 Secrets Management

**Severity: High**

The `.env` file on the host (`/root/ploy/.env`) is the primary secrets delivery mechanism for the Aliyun deployment. This is reasonable for a single-host setup. However:

- The `deploy-prebuilt.yml` workflow writes `POLYMARKET_PRIVATE_KEY` directly into a systemd unit file on disk (see §2.2). Even if this workflow is deprecated, it may have been used to provision existing hosts.
- There is no secrets rotation procedure documented anywhere.
- The `.gitignore` correctly excludes `.env` and `*.key` files.
- `production.example.toml` contains placeholder private key (`0x...`) — acceptable as an example, but the file is in git and could mislead operators into committing real credentials.

**Recommendation**: Audit tango-1-1 to confirm `POLYMARKET_PRIVATE_KEY` is not present in any systemd unit file. Establish a secrets rotation runbook. Consider using systemd `EnvironmentFile=` pointing to a 0600-owned file rather than inline `Environment=` directives.

---

## 4. Monitoring & Observability

### 4.1 Health Endpoint

**Severity: Low (present and functional)**

`GET /health` is implemented at `src/api/handlers/system.rs:51`. It checks DB connectivity (`SELECT 1`) and returns `{"status":"ok","db":"connected","uptime_secs":N}` or 503 when degraded. A `/healthz` liveness endpoint also exists in `src/services/health.rs`.

The deploy workflow checks `curl -f http://localhost:8080/health` after restart but uses `|| true` — a health check failure does not abort the deploy. This means a broken binary that starts but cannot connect to the DB will be considered a successful deployment.

**Recommendation**: Remove `|| true` from the post-deploy health check in `release-aliyun.yml`. A failed health check should trigger automatic rollback.

### 4.2 Logging

**Severity: Medium**

Logging uses `tracing` + `tracing-subscriber` + `tracing-appender`. Log files go to `/var/log/ploy/`. The `get-logs.yml` workflow provides a manual log retrieval mechanism via SSH.

There is no structured log aggregation (no CloudWatch, no Loki, no ELK). Logs are only accessible by SSHing to the host or running the `get-logs.yml` workflow. For a 24/7 trading system, this means:
- No alerting on error patterns (e.g., repeated order failures, circuit breaker trips)
- No cross-session log correlation
- Log loss if the host is replaced

**Recommendation**: At minimum, configure `journald` forwarding to a persistent log store, or add a lightweight log shipper (Vector, Promtail) to push to a managed service. Add alerting on `ERROR` log patterns for order submission failures and circuit breaker state changes.

### 4.3 Metrics

**Severity: Medium**

There are no Prometheus metrics, no OpenTelemetry instrumentation, and no metrics endpoint in any workflow or source file. The only operational visibility is logs and the `/health` endpoint. For a trading system, the absence of metrics means:
- No P&L dashboards
- No order fill rate tracking
- No latency percentiles for order submission
- No alerting on position size or daily loss limit approach

**Recommendation**: Add a `/metrics` endpoint (Prometheus format) exposing at minimum: orders submitted/filled/failed, active positions count, daily P&L, circuit breaker state, and WebSocket connection status.

---

## 5. Deployment Safety

### 5.1 No Blue-Green or Canary

**Severity: Medium**

All deployments are in-place: stop service → replace binary → start service. There is no blue-green or canary capability. For a trading system, this means:
- Downtime during every deploy (typically 5-10 seconds based on the `wait_for_unit_active` loop)
- No ability to validate a new binary against live traffic before full cutover
- A bad deploy that passes the health check but has a logic error will affect all live positions

**Recommendation**: For a single-host setup, a practical mitigation is a pre-deploy dry-run smoke test: start the new binary with `--dry-run` flag against the live DB, verify it reaches `READY` state, then perform the swap. Document this as the required pre-deploy step.

### 5.2 Deployment Gate

**Severity: Low (present)**

The `release-aliyun.yml` workflow uses `environment: production` which enables GitHub's environment protection rules (required reviewers, wait timers). This is the correct gate. The `deploy_production` boolean input also provides an explicit opt-in for manual dispatches.

### 5.3 Migration Safety

**Severity: Medium**

The deploy script runs a single hardcoded migration (`022_order_strategy_tracking.sql`) via `psql`. There is no migration framework (sqlx migrate, flyway, etc.) tracking which migrations have been applied. Running the same migration twice will either error or silently succeed depending on whether it uses `CREATE TABLE IF NOT EXISTS`. There is no rollback migration.

**Recommendation**: Use `sqlx migrate run` (already a dependency) as the migration step. This tracks applied migrations in a `_sqlx_migrations` table and is idempotent.

---

## 6. Operational Runbooks

### 6.1 No Runbooks for Known Failure Modes

**Severity: High**

The `scripts/` directory contains deployment and maintenance scripts but no runbooks for the operational failure modes identified in prior review phases:

- **A-H4 (governance not restored on restart)**: No documented procedure for re-applying governance pauses after a service restart. An operator responding to an incident would not know to manually re-pause domains.
- **F-01 (foreground bypass)**: No documented warning that `ploy strategy start --foreground` bypasses risk controls. An operator using this for debugging in production could inadvertently trade without risk gates.
- **Circuit breaker reset**: No documented procedure for resetting a tripped circuit breaker without restarting the service.
- **Emergency stop**: No documented procedure for triggering emergency stop via the API vs. `systemctl stop`.

The `ploy_maintenance.sh` script covers DB retention and log rotation — useful but not incident response.

**Recommendation**: Create `docs/runbooks/` with at minimum:
1. `restart.md` — safe restart procedure including governance state backup/restore
2. `emergency-stop.md` — API-based stop vs. systemd stop, when to use each
3. `circuit-breaker.md` — how to inspect state, reset, and validate
4. `governance-pause.md` — how to pause a domain and verify it survives restart (pending A-H4 fix)
5. `rollback.md` — step-by-step rollback using `rollback.yml` with the corrected host target

### 6.2 `ploy_maintenance.sh` is Unscheduled

**Severity: Low**

The maintenance script handles DB retention and journal vacuum but there is no cron job or systemd timer documented or deployed by CI. If it is not running, the 18GB database on tango-1-1 will continue growing.

**Recommendation**: Add a systemd timer unit for `ploy_maintenance.sh` to the release bundle and deploy it via `release-aliyun.yml`.

---

## 7. Environment Management

### 7.1 No Staging Environment

**Severity: Medium**

There is one production host (tango-1-1) and no staging environment. The test suite runs against an ephemeral Postgres container in CI, but there is no environment that mirrors production configuration for pre-release validation.

The `deploy-aws-jp.yml` workflow appears to have been used as an ad-hoc staging environment (it accepts trading parameters as inputs and deploys to a separate EC2 host), but it builds on-host and is not a proper staging pipeline.

**Recommendation**: Designate tango-2-1 (AWS Tokyo, currently running) as a staging environment. Add a `deploy-staging` job to `release-aliyun.yml` that deploys to tango-2-1 before the production deploy, with a manual approval gate between them.

### 7.2 Hardcoded Infrastructure Values

**Severity: Medium**

Several workflows hardcode infrastructure values that should be variables or secrets:
- `deploy-tango21.yml`: EC2 instance ID `i-01de34df55726073d` and IP `3.112.247.26` in plaintext
- `deploy-prebuilt.yml`: EC2 IP `13.231.209.90` in plaintext
- `stop-trading.yml` and `get-logs.yml`: Use `StrictHostKeyChecking=no` — disables SSH host key verification, enabling MITM attacks against the trading host

**Recommendation**: Move all host IPs and instance IDs to GitHub repository variables (not secrets, but not hardcoded). Replace `StrictHostKeyChecking=no` with `ssh_known_hosts` verification using the `appleboy/ssh-action` `known_hosts` parameter.

---

## 8. Systemd Service Configuration

### 8.1 Guardrails (Well-Implemented)

**Severity: Low (positive finding)**

The `release-aliyun.yml` deploy script writes a `10-memory-restart.conf` drop-in to every detected ploy service unit:
```
[Service]
Restart=always
RestartSec=5
MemoryHigh=1280M
MemoryMax=1536M
OOMPolicy=kill
```

This matches the CLAUDE.md deployment policy exactly. The deploy script also verifies these settings with `systemctl show` after restart. This is correct.

### 8.2 Missing `StartLimitIntervalSec` / `StartLimitBurst`

**Severity: Medium**

The drop-in sets `Restart=always` but does not set `StartLimitIntervalSec` or `StartLimitBurst`. If the binary crashes immediately on startup (e.g., DB unreachable, bad config), systemd will restart it indefinitely at 5-second intervals, consuming resources and flooding logs. The default systemd start limit (5 starts in 10 seconds) may or may not apply depending on the base unit definition.

**Recommendation**: Add `StartLimitIntervalSec=60` and `StartLimitBurst=5` to the drop-in. After 5 rapid failures, systemd will stop retrying and alert via `systemctl status`, giving an operator a clear signal.

### 8.3 No `ExecStartPre` Health Check

**Severity: Low**

There is no `ExecStartPre` step to verify DB connectivity or config validity before the main process starts. A misconfigured `.env` will cause the service to start, fail, and restart in a loop.

**Recommendation**: Add `ExecStartPre=/usr/bin/pg_isready -d $DATABASE_URL` or a lightweight `ploy config check` subcommand as `ExecStartPre`.

---

## 9. Prior Phase Findings — CI/CD Impact

| Finding | CI/CD Impact | Current Mitigation |
|---------|-------------|-------------------|
| A-H4: Governance not restored on restart | `Restart=always` means every OOM or crash loses governance state | None — no pre-restart state export, no post-restart restore hook |
| F-01: Foreground bypass | No CI gate prevents deploying code with this path | None — no integration test exercises the foreground path |
| P-C1: Coordinator serialization | No load test in CI to detect throughput regression | None |
| F-03: Governance pool not wired | Silent failure not caught by health endpoint | Health endpoint only checks DB, not governance pool wiring |

---

## Summary Table

| ID | Area | Severity | Finding |
|----|------|----------|---------|
| C-01 | CI Gates | High | No `cargo audit` / `cargo deny` — no dependency vulnerability scanning |
| C-02 | CI Gates | Medium | Clippy is advisory-only; does not block merge |
| C-03 | CI Gates | Medium | `api` feature not tested in CI; production feature set untested |
| D-01 | Deploy | Critical | `deploy-prebuilt.yml` writes `POLYMARKET_PRIVATE_KEY` into systemd unit file on disk |
| D-02 | Deploy | High | Legacy workflows (`deploy-tango21.yml`, `deploy.yml`, `release.yml`) build Rust on-host, violating deployment policy |
| D-03 | Deploy | Medium | `rollback.yml` targets wrong host (AWS EC2 vs. Aliyun tango-1-1) |
| D-04 | Deploy | Medium | Post-deploy health check uses `|| true` — failed health does not abort deploy |
| D-05 | Deploy | Medium | Migration applied via raw `psql` without tracking; not idempotent |
| S-01 | Secrets | High | No secrets rotation procedure; `deploy-prebuilt.yml` may have left private key in systemd unit |
| S-02 | Security | High | `StrictHostKeyChecking=no` in 3 workflows — SSH MITM risk |
| S-03 | Security | Medium | Third-party Actions pinned to mutable version tags, not commit SHAs |
| O-01 | Observability | Medium | No metrics endpoint; no alerting on order failures or circuit breaker state |
| O-02 | Observability | Medium | No log aggregation; logs only accessible via SSH |
| O-03 | Runbooks | High | No runbooks for governance restore, foreground bypass warning, circuit breaker reset, emergency stop |
| E-01 | Environments | Medium | No staging environment; tango-2-1 available but not used as staging |
| E-02 | Environments | Medium | Infrastructure values hardcoded in workflow files |
| Sy-01 | Systemd | Medium | No `StartLimitIntervalSec`/`StartLimitBurst` — crash loop not bounded |
| Sy-02 | Systemd | Low | No `ExecStartPre` config/DB validation before service start |
| Cf-01 | Config | Low | Only 2 of 11 strategy configs deployed by CI; rest require manual placement |

---

## Immediate Actions (Priority Order)

1. **Audit tango-1-1** for `POLYMARKET_PRIVATE_KEY` in `/etc/systemd/system/*.service` files. Rotate the key if found. (D-01)
2. **Fix `rollback.yml`** to target the correct host before the next incident requires it. (D-03)
3. **Remove `|| true`** from post-deploy health check in `release-aliyun.yml`. (D-04)
4. **Add `cargo audit`** to `test.yml` as a blocking step. (C-01)
5. **Write governance restore runbook** covering the A-H4 failure mode. (O-03)
6. **Replace `StrictHostKeyChecking=no`** with known_hosts verification. (S-02)

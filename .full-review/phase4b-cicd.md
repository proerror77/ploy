# Phase 4b: CI/CD Pipeline & Operational Review

**Date**: 2026-03-08
**Scope**: `.github/workflows/` (11 files), `deployment/` (13 service files, configs), `scripts/`, `Dockerfile`
**System**: Rust trading bot on Polymarket, deployed to Alibaba Cloud (tango-1-1, ARM) and AWS Tokyo (tango-2-1, x86)

---

## 1. CI/CD Pipeline

### CICD-01: Architecture Mismatch — x86 Binary Deployed to ARM Host
**Severity**: Critical
**Files**: `.github/workflows/release-aliyun.yml` (line 39, 109-112)

The `release-aliyun.yml` workflow builds on `ubuntu-latest` (x86_64) and verifies `ELF 64-bit LSB`, then deploys to tango-1-1 which is an ARM (aarch64) Alibaba Cloud instance. The binary will fail to execute or, if the host happens to have qemu-user installed, run with severe performance degradation.

The deploy script at line 331 checks `file ... | grep -q "ELF 64-bit LSB"` but does not verify the architecture field (x86-64 vs aarch64).

**Risk**: Binary cannot run on production host. Complete deployment failure.
**Recommendation**: Either cross-compile with `cross` or `cargo-zigbuild` targeting `aarch64-unknown-linux-gnu`, or use a self-hosted ARM runner. Add architecture verification: `grep -q "ARM aarch64"` or `grep -q "x86-64"` as appropriate per target.

### CICD-02: Feature Flag Inconsistency Across Workflows
**Severity**: High
**Files**: All build workflows

Different workflows build with different feature sets:
- `test.yml`: `--features rl`
- `release.yml`: `--features rl`
- `release-aliyun.yml`: `--features claimer_daemon,api,pm_ctf,tokio/io-std`
- `deploy-prebuilt.yml`: `--features rl`
- `deploy.yml`: `--features rl`
- `auto-review.yml` (clippy): `--features rl`
- `deploy-aws-jp.yml`: no features specified (default)
- `deploy-tango21.yml`: no features specified (default)

The test suite runs with `--features rl` but the Aliyun production release uses a completely different feature set (`claimer_daemon,api,pm_ctf`). Code paths exercised in CI may not match what ships.

**Risk**: Untested code paths in production. Bugs in `claimer_daemon`/`api`/`pm_ctf` features are never caught by CI.
**Recommendation**: Test with the same feature matrix that ships. Add a CI job that builds and tests with `PLOY_RELEASE_FEATURES`.

### CICD-03: No Security Scanning in Pipeline
**Severity**: High
**Files**: `.github/workflows/` (all)

No `cargo audit`, `cargo deny`, Trivy, Snyk, or any dependency vulnerability scanning. No Dependabot or Renovate configuration. A trading system handling real money with no supply-chain security checks.

**Risk**: Vulnerable dependencies ship to production undetected.
**Recommendation**: Add `cargo audit` to the test workflow. Add `.github/dependabot.yml` for automated dependency updates. Consider `cargo deny` for license and advisory checks.

### CICD-04: No Staging Environment
**Severity**: High
**Files**: All deploy workflows

Every deployment workflow targets `environment: production` directly. There is no staging, canary, or pre-production environment. The `deploy-prebuilt.yml` even hardcodes database credentials and creates the production database inline.

**Risk**: Every deployment is a direct-to-production push with no validation buffer. A bad release immediately affects live trading.
**Recommendation**: Add a staging environment (even a dry-run instance on the same host) that receives deployments first. Gate production deployment on staging health checks passing.

### CICD-05: Hardcoded Secrets in Workflow Files
**Severity**: High
**Files**: `.github/workflows/deploy-prebuilt.yml` (lines 153-165, 216-219)

The `deploy-prebuilt.yml` workflow contains:
- Hardcoded database credentials: `postgresql://ploy:ploy@localhost:5432/ploy` (lines 162, 216)
- Secrets baked into systemd unit files via `Environment=` directives (lines 217-219), which are visible to any process that can read `/etc/systemd/system/`
- Hardcoded `CREATE USER ploy WITH PASSWORD 'ploy'` (line 156)

**Risk**: Credentials visible in workflow logs, systemd unit files, and version control. Any user on the host can read the private key from the unit file.
**Recommendation**: Use `EnvironmentFile` pointing to a secrets file (as the proper service files in `deployment/` already do). Never embed secrets in systemd unit definitions.

### CICD-06: SSH Private Key Written to Disk
**Severity**: High
**Files**: `.github/workflows/deploy-aws-jp.yml` (lines 85-86, 129), `stop-trading.yml` (lines 25-26, 35), `get-logs.yml` (lines 25-26, 54)

Three workflows write `${{ secrets.AWS_EC2_PRIVATE_KEY }}` to `private_key.pem` on the runner filesystem. While they `rm` it afterward, if the workflow fails between write and cleanup, the key persists in the runner's filesystem and potentially in runner logs.

Additionally, all three use `StrictHostKeyChecking=no`, making them vulnerable to MITM attacks.

**Risk**: SSH key exposure on shared GitHub runners. MITM vulnerability.
**Recommendation**: Use `appleboy/ssh-action` (already used in other workflows) which handles key lifecycle internally. If raw SSH is needed, use `ssh-agent` with `webfactory/ssh-agent` action instead of writing keys to disk.

### CICD-07: deploy.yml Stops Service Before Binary Upload
**Severity**: Medium
**Files**: `.github/workflows/deploy.yml` (lines 102-128)

The deploy workflow stops the ploy service (line 112), then in a separate step uploads the binary (line 122). If the upload step fails, the service remains stopped with no automatic recovery. The `sleep 2` at line 120 is a race condition, not a synchronization mechanism.

**Risk**: Extended downtime during failed deployments. Trading halted with open positions.
**Recommendation**: Upload first, then atomically swap the binary and restart. The `release-aliyun.yml` workflow does this correctly — consolidate on that pattern.

### CICD-08: deploy-tango21.yml Builds Rust on Production Host
**Severity**: Medium
**Files**: `.github/workflows/deploy-tango21.yml` (lines 154-158)

The deployment script uploads source code to the EC2 instance and runs `cargo build --release` on the production host. This violates the project's own "Trading Host Deployment Policy" in CLAUDE.md which explicitly states: "do not build Rust source on-host."

**Risk**: Build process consumes all CPU/memory on a trading host, potentially causing OOM kills of running trading processes. Build artifacts consume disk space.
**Recommendation**: Remove this workflow or refactor to use pre-built binaries like `release-aliyun.yml`.

### CICD-09: Deprecated GitHub Actions
**Severity**: Medium
**Files**: `.github/workflows/deploy-prebuilt.yml` (line 22)

Uses `actions-rs/toolchain@v1` which has been deprecated and archived. Also uses `actions/cache@v3` (line 33) while other workflows use `v4`.

**Risk**: Deprecated actions may stop working without notice. Inconsistent cache behavior.
**Recommendation**: Replace with `dtolnay/rust-toolchain@stable` (already used elsewhere) and `actions/cache@v4`.

### CICD-10: No Timeout on Most Deploy Jobs
**Severity**: Medium
**Files**: `deploy.yml`, `deploy-prebuilt.yml`, `deploy-aws-jp.yml`, `deploy-tango21.yml`, `rollback.yml`

Only `test.yml` (30min), `auto-review.yml` (20min), `release-aliyun.yml` (40min), and `release.yml` (30min) have `timeout-minutes`. Deploy jobs have no timeout and could hang indefinitely (e.g., SSH connection stall).

**Risk**: Hung workflows consume runner minutes and block the concurrency group.
**Recommendation**: Add `timeout-minutes: 15` to all deploy jobs.

---

## 2. Deployment Strategy

### CICD-11: No Blue-Green or Canary Deployment
**Severity**: High

All deployments follow a stop-replace-start pattern. There is no blue-green, canary, or rolling deployment capability. For a system trading real money, this means:
- Downtime during every deployment (service stop -> binary copy -> service start)
- No traffic shifting or gradual rollout
- No automatic rollback on health check failure

**Risk**: Every deployment creates a window where the system cannot trade, potentially missing hedge exits or leaving positions unmanaged.
**Recommendation**: Implement at minimum a "deploy and verify before switching" pattern. The `release-aliyun.yml` already installs to a versioned directory — extend this to symlink-swap with health check verification before switching the active binary.

### CICD-12: Rollback Only Covers One Host
**Severity**: Medium
**Files**: `.github/workflows/rollback.yml`

The rollback workflow only targets `EC2_HOST` (AWS). There is no rollback workflow for the Aliyun ECS host (tango-1-1), which is the primary production target per CLAUDE.md.

**Risk**: Cannot quickly rollback the primary production deployment.
**Recommendation**: Add Aliyun rollback workflow, or parameterize the existing one to target either host.

### CICD-13: Backup Rotation Not Enforced
**Severity**: Low
**Files**: `.github/workflows/release-aliyun.yml` (lines 319-323), `release.yml` (lines 189-193)

Timestamped backups (`ploy.bak.YYYYMMDD_HHMMSS`) accumulate without cleanup. On a 197GB disk with a ~100MB binary, this is not immediately critical but will eventually consume space.

**Risk**: Disk space exhaustion over time.
**Recommendation**: Add backup rotation (keep last 5) in the deploy script or maintenance job.

---

## 3. Infrastructure as Code

### CICD-14: Infrastructure Not Codified
**Severity**: High

There is no Terraform, Pulumi, CloudFormation, or any IaC for:
- Alibaba Cloud ECS instances
- AWS EC2 instances
- Security groups / firewall rules
- S3 buckets (created ad-hoc per deployment in `deploy-prebuilt.yml` and `deploy-tango21.yml`)
- ECR repositories
- Database provisioning

Server setup is done imperatively via SSH in workflow scripts.

**Risk**: Infrastructure drift, unreproducible environments, no disaster recovery plan. If tango-1-1 dies, there is no automated way to recreate it.
**Recommendation**: At minimum, document the infrastructure setup. Ideally, codify with Terraform (supports both Alibaba Cloud and AWS).

### CICD-15: S3 Bucket Leak in deploy-prebuilt and deploy-tango21
**Severity**: Medium
**Files**: `.github/workflows/deploy-prebuilt.yml` (line 9), `deploy-tango21.yml` (line 16)

Both workflows create a new S3 bucket per run (`ploy-prebuilt-${{ github.run_number }}`, `ploy-deployment-${{ github.run_number }}`). The cleanup step in `deploy-tango21.yml` is commented out (line 255). These buckets accumulate indefinitely.

**Risk**: Unbounded S3 cost growth. Stale deployment artifacts with potential secrets.
**Recommendation**: Enable the cleanup step or use a single bucket with versioned prefixes.

---

## 4. Monitoring & Observability

### CICD-16: Prometheus Metrics Exist But No Scraper Configured
**Severity**: High
**Files**: `src/services/health.rs`, `src/services/metrics.rs`

The codebase has a well-implemented `/metrics` endpoint with Prometheus-format output (health status, PnL, order counts, WS status, per-symbol freshness). However, there is no evidence of:
- Prometheus server or Victoria Metrics deployed
- Grafana dashboards
- Alert rules (PnL threshold, consecutive failures, WS disconnect)
- Any scrape configuration

The metrics endpoint exists but nobody is reading it.

**Risk**: Operational blindness. The system could be losing money, disconnected, or in a failure loop with no alerting.
**Recommendation**: Deploy Prometheus + Grafana (or a managed alternative like Grafana Cloud free tier). Define alerts for: `ploy_daily_pnl_usd < -100`, `ploy_consecutive_failures > 3`, `ploy_websocket_connected == 0`, `ploy_up < 1`.

### CICD-17: No Centralized Logging
**Severity**: Medium

Logs go to journald on each host. There is no log aggregation (Loki, CloudWatch Logs, Elasticsearch). The `get-logs.yml` workflow SSHes into the host to tail logs — this is the only way to access them.

**Risk**: Logs lost on host failure. No cross-host correlation. No log-based alerting.
**Recommendation**: Ship journald logs to a centralized store. Loki + Promtail is lightweight and pairs with Grafana.

### CICD-18: No Deployment Notifications
**Severity**: Medium

No Slack, Discord, Feishu, or email notifications on deployment success/failure. The Feishu webhook is referenced in `deploy-aws-jp.yml` as a runtime env var for the trading bot, but not used for CI/CD notifications.

**Risk**: Failed deployments go unnoticed.
**Recommendation**: Add a notification step to deployment workflows using the existing Feishu webhook or GitHub's built-in notification system.

---

## 5. Incident Response

### CICD-19: No Runbooks or On-Call Procedures
**Severity**: High

No runbooks exist for:
- "Trading bot is losing money rapidly"
- "WebSocket disconnected for >5 minutes"
- "Database disk full"
- "Host unreachable"
- "Deployment failed mid-way"

The `stop-trading.yml` workflow exists as an emergency stop but only covers the Docker-based AWS deployment, not the primary Aliyun systemd deployment.

**Risk**: During an incident, the operator must figure out the correct response in real-time while money is at risk.
**Recommendation**: Create runbooks for the top 5 failure scenarios. Ensure `stop-trading.yml` covers all deployment targets.

### CICD-20: Emergency Stop Coverage Gap
**Severity**: High
**Files**: `.github/workflows/stop-trading.yml`

The emergency stop workflow only stops a Docker container named `ploy-trading` on the AWS host. The primary production deployment (tango-1-1, Aliyun, systemd) has no emergency stop workflow. An operator would need to SSH manually.

**Risk**: Cannot quickly halt trading on the primary production system via CI/CD.
**Recommendation**: Add an Aliyun emergency stop workflow that runs `systemctl stop ploy-platform-live` (or all ploy-* services) on tango-1-1.

---

## 6. Environment Management

### CICD-21: Config Parity Issues Between Environments
**Severity**: Medium
**Files**: `deployment/config/crypto_live.toml`, `deployment/config/platform_live.toml`, `deployment/production.toml`, `deployment/aws/config/production.toml`

Multiple production config files exist with subtle differences:
- `crypto_live.toml`: `json = false`, `sum_target = 1.0`
- `production.toml`: `json = true`, `sum_target = 0.95`
- `aws/config/production.toml`: `json = true`, `sum_target = 0.95`
- `platform_live.toml`: `json = false`, `sum_target = 1.0`, includes `nba_comeback` section

No config validation or schema enforcement exists. A typo in a TOML value (e.g., integer instead of float, as documented in MEMORY.md) causes silent runtime failure.

**Risk**: Behavioral differences between environments. Config errors cause runtime failures.
**Recommendation**: Add a config validation step to CI (e.g., `ploy config validate`). Reduce config duplication by using a base config with environment-specific overrides.

### CICD-22: Systemd Service File Inconsistencies
**Severity**: Medium
**Files**: `deployment/*.service`

Memory limits vary across service files without clear rationale:
- `ploy.service`: `MemoryMax=512M`
- `ploy@.service`: `MemoryMax=512M`
- `ploy-platform-live.service`: `MemoryMax=768M`
- `ploy-crypto-live.service`: `MemoryMax=768M`
- `ploy-strategy-split-arb-dryrun.service`: `MemoryMax=256M`

But the `release-aliyun.yml` deploy script overrides all of these with a systemd drop-in setting `MemoryMax=1536M` (line 364), which is 2-6x higher than the unit file values.

The `deployment/aws/ploy.service` has much stricter security hardening (`ProtectSystem=strict`, `MemoryDenyWriteExecute=true`, `ProtectKernelTunables=true`) than the other service files (`ProtectSystem=full`). This inconsistency means the AWS deployment is more hardened than Aliyun.

**Risk**: The drop-in override silently negates the carefully tuned memory limits. Security posture varies by host.
**Recommendation**: Align service files. Either remove the drop-in override or update the base service files to match. Standardize security hardening across all service files.

---

## 7. Release Process

### CICD-23: Version Stuck at 0.1.0
**Severity**: Medium
**Files**: `Cargo.toml` (line 4)

`Cargo.toml` has `version = "0.1.0"` despite the system being in production trading real money. Release tags (vX.Y.Z) are used in workflows but the binary itself always reports 0.1.0.

**Risk**: Cannot determine which version is running on a host from the binary itself.
**Recommendation**: Use `vergen` or a build script to embed the git tag/SHA into the binary. Bump `Cargo.toml` version as part of the release process.

### CICD-24: No CHANGELOG
**Severity**: Low

No CHANGELOG.md exists (confirmed by prior review phase3b). Release notes are auto-generated by GitHub (`generate_release_notes: true`) but there is no curated changelog.

**Risk**: Difficult to understand what changed between releases.
**Recommendation**: Adopt conventional commits and auto-generate changelogs, or maintain a manual CHANGELOG.md.

### CICD-25: Workflow Proliferation and Duplication
**Severity**: Medium
**Files**: All 11 workflow files

There are 7 deployment-related workflows with significant duplication:
1. `deploy.yml` — AWS EC2 (binary via SCP)
2. `deploy-prebuilt.yml` — AWS EC2 (binary via S3, builds frontend)
3. `deploy-aws-jp.yml` — AWS EC2 (Docker via ECR)
4. `deploy-tango21.yml` — AWS EC2 (source via S3, builds on host)
5. `release.yml` — GitHub Release + AWS EC2 deploy
6. `release-aliyun.yml` — GitHub Release + Aliyun ECS deploy
7. `rollback.yml` — AWS EC2 rollback

Each has its own build step, SSH pattern, and deployment logic. It is unclear which workflows are actively used vs. legacy.

**Risk**: Maintenance burden. Fixes applied to one workflow are not propagated to others. Confusion about which workflow to use.
**Recommendation**: Consolidate to 2-3 workflows: `test.yml`, `release.yml` (build + publish), `deploy.yml` (parameterized for target host). Archive or delete unused workflows.

---

## Summary Table

| ID | Severity | Category | Finding |
|----|----------|----------|---------|
| CICD-01 | Critical | Build | x86 binary deployed to ARM host |
| CICD-02 | High | Build | Feature flag mismatch between test and release |
| CICD-03 | High | Security | No dependency vulnerability scanning |
| CICD-04 | High | Deployment | No staging environment |
| CICD-05 | High | Security | Hardcoded secrets in workflow files |
| CICD-06 | High | Security | SSH key written to disk on runners |
| CICD-07 | Medium | Deployment | Service stopped before binary uploaded |
| CICD-08 | Medium | Policy | Rust built on production host |
| CICD-09 | Medium | Build | Deprecated GitHub Actions |
| CICD-10 | Medium | Reliability | No timeout on deploy jobs |
| CICD-11 | High | Deployment | No blue-green or canary deployment |
| CICD-12 | Medium | Deployment | Rollback only covers AWS, not Aliyun |
| CICD-13 | Low | Operations | No backup rotation |
| CICD-14 | High | IaC | No infrastructure as code |
| CICD-15 | Medium | Cost | S3 bucket leak per deployment |
| CICD-16 | High | Observability | Prometheus metrics exist but no scraper |
| CICD-17 | Medium | Observability | No centralized logging |
| CICD-18 | Medium | Observability | No deployment notifications |
| CICD-19 | High | Incident | No runbooks or on-call procedures |
| CICD-20 | High | Incident | Emergency stop only covers AWS Docker |
| CICD-21 | Medium | Config | Config parity issues between environments |
| CICD-22 | Medium | Config | Systemd service file inconsistencies |
| CICD-23 | Medium | Release | Version stuck at 0.1.0 |
| CICD-24 | Low | Release | No CHANGELOG |
| CICD-25 | Medium | Maintenance | 7 overlapping deployment workflows |

**Critical**: 1 | **High**: 9 | **Medium**: 12 | **Low**: 3

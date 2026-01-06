# DevOps Implementation Tickets

**GitHub Issues/Tickets for DevOps Improvements**
Generated: 2026-01-05

Use these as templates to create tickets in your project management system.

---

## PHASE 1: FOUNDATION (Week 1) - 3 hours

### Ticket P1-1: Fix CI Linting Strictness
**Priority**: CRITICAL
**Effort**: 30 minutes
**Owner**: DevOps Lead

**Description**:
Currently, Clippy warnings don't block CI merges. This allows code quality to degrade.

**Acceptance Criteria**:
- [ ] Remove `continue-on-error: true` from test.yml line 64
- [ ] Clippy failures now block deployment
- [ ] All existing warnings fixed
- [ ] Pipeline passes with strict linting

**Files to Change**:
- `.github/workflows/test.yml`

**Test**:
```bash
cargo clippy --all-targets --features rl -- -D warnings
```

---

### Ticket P1-2: Add Security Vulnerability Scanning
**Priority**: CRITICAL
**Effort**: 30 minutes
**Owner**: DevOps Lead

**Description**:
No automated checking for vulnerable dependencies. Need to add cargo-audit to CI.

**Acceptance Criteria**:
- [ ] `cargo audit` runs in test workflow
- [ ] Workflow fails if vulnerabilities found
- [ ] All current vulnerabilities resolved
- [ ] Runs on PR and push to main

**Files to Change**:
- `.github/workflows/test.yml` (add security job)

**Implementation**:
```bash
cargo install cargo-audit --locked
cargo audit --deny warnings
```

---

### Ticket P1-3: Create Code Formatting Configuration
**Priority**: HIGH
**Effort**: 30 minutes
**Owner**: Developer

**Description**:
Project has no `.rustfmt.toml`. Need to define formatting standards for consistency.

**Acceptance Criteria**:
- [ ] Create `.rustfmt.toml` with project standards
- [ ] All code reformatted to match
- [ ] Developers can run `cargo fmt` locally
- [ ] CI enforces formatting

**Files to Create**:
- `.rustfmt.toml`

**Configuration**:
```toml
edition = "2021"
max_width = 100
hard_tabs = false
tab_spaces = 4
newline_style = "Auto"
use_try_shorthand = true
format_strings = true
comment_width = 80
reorder_imports = true
reorder_modules = true
```

---

### Ticket P1-4: Create Clippy Configuration
**Priority**: HIGH
**Effort**: 30 minutes
**Owner**: Developer

**Description**:
No clippy.toml defines linting rules. Need to enforce consistent standards.

**Acceptance Criteria**:
- [ ] Create `clippy.toml` with project rules
- [ ] All clippy warnings resolved
- [ ] Linting is repeatable and documented
- [ ] CI passes with `-D warnings`

**Files to Create**:
- `clippy.toml`

---

### Ticket P1-5: Set Up Pre-commit Hooks
**Priority**: HIGH
**Effort**: 30 minutes
**Owner**: Developer

**Description**:
Developers should catch formatting/linting errors locally before pushing.

**Acceptance Criteria**:
- [ ] Create `.pre-commit-config.yaml`
- [ ] Hook runs formatting checks
- [ ] Hook runs clippy checks
- [ ] Hook prevents commits with secrets
- [ ] Developers can run `pre-commit install`

**Files to Create**:
- `.pre-commit-config.yaml`

**Setup**:
```bash
pip install pre-commit
pre-commit install
pre-commit run --all-files
```

---

### Ticket P1-6: Optimize Cargo Release Profile
**Priority**: MEDIUM
**Effort**: 30 minutes
**Owner**: Developer

**Description**:
Release profile could be optimized for build time vs. binary size/performance.

**Acceptance Criteria**:
- [ ] Add `[profile.staging]` for pre-production builds
- [ ] Staging builds faster than release
- [ ] Staging keeps debug symbols
- [ ] Release still optimized with LTO

**Files to Change**:
- `Cargo.toml`

---

## PHASE 2: SAFETY (Week 2) - 6 hours

### Ticket P2-1: Implement Automated Rollback
**Priority**: CRITICAL
**Effort**: 2 hours
**Owner**: DevOps Lead

**Description**:
Current rollback is manual, slow, and error-prone. Need automated rollback on deployment failure.

**Acceptance Criteria**:
- [ ] Create `scripts/rollback.sh` script
- [ ] Script verifies backup binary exists
- [ ] Script restarts service and health checks
- [ ] Can rollback in < 5 minutes
- [ ] Manual rollback still possible

**Files to Create**:
- `scripts/rollback.sh`

**Testing**:
```bash
./scripts/rollback.sh <ec2-host>
curl http://localhost:8080/health
```

---

### Ticket P2-2: Add Health Check Validation to Deployment
**Priority**: CRITICAL
**Effort**: 1 hour
**Owner**: DevOps Lead

**Description**:
Deployment doesn't verify health checks actually pass. Could deploy broken code.

**Acceptance Criteria**:
- [ ] Deployment waits for health check success
- [ ] Rollback triggered if health check fails
- [ ] Wait loop has timeout (5 minutes)
- [ ] Logs show all health check attempts

**Files to Change**:
- `.github/workflows/deploy.yml` - add health check job

---

### Ticket P2-3: Create Configuration Validation Script
**Priority**: HIGH
**Effort**: 1.5 hours
**Owner**: DevOps Lead

**Description**:
Deployments can fail silently due to missing/invalid configuration. Need validation.

**Acceptance Criteria**:
- [ ] Create `scripts/validate-config.sh`
- [ ] Check all required environment variables
- [ ] Validate TOML syntax if possible
- [ ] Test database connection
- [ ] Run before deployment

**Files to Create**:
- `scripts/validate-config.sh`

---

### Ticket P2-4: Create Health Check Script
**Priority**: HIGH
**Effort**: 1 hour
**Owner**: DevOps Lead

**Description**:
Health checking should be consistent and automated.

**Acceptance Criteria**:
- [ ] Create `scripts/health-check.sh`
- [ ] Check /health endpoint with retries
- [ ] Configurable timeout
- [ ] Exit 0 on success, 1 on failure

**Files to Create**:
- `scripts/health-check.sh`

---

### Ticket P2-5: Improve Systemd Service Configuration
**Priority**: MEDIUM
**Effort**: 1 hour
**Owner**: DevOps Lead

**Description**:
Current systemd service needs resource limits and better logging.

**Acceptance Criteria**:
- [ ] Add memory limit (1GB)
- [ ] Add CPU limit (80%)
- [ ] Configure proper restart policy
- [ ] Set up logging to journalctl
- [ ] Service restarts on failure

**Files to Create/Modify**:
- `deployment/ploy.service`

---

### Ticket P2-6: Improve GitHub Actions Deploy Workflow
**Priority**: HIGH
**Effort**: 2 hours
**Owner**: DevOps Lead

**Description**:
Deploy workflow needs validation step and better error handling.

**Acceptance Criteria**:
- [ ] Add validate job before deploy
- [ ] Run full test suite before deploy
- [ ] Validate configuration on EC2
- [ ] Automatic rollback on failure
- [ ] Health check blocks deployment

**Files to Change**:
- `.github/workflows/deploy.yml`

---

## PHASE 3: VISIBILITY (Week 3) - 4 hours

### Ticket P3-1: Add Prometheus Metrics
**Priority**: HIGH
**Effort**: 2 hours
**Owner**: Backend Developer

**Description**:
Need metrics to track order placement, success rates, and latency.

**Acceptance Criteria**:
- [ ] Create `src/services/metrics.rs`
- [ ] Track orders_placed_total
- [ ] Track orders_filled_total
- [ ] Track order_latency_ms
- [ ] Track api_errors_total
- [ ] Endpoint exposes metrics

**Files to Create**:
- `src/services/metrics.rs` (or update existing)

**Metrics**:
```
ploy_orders_placed_total
ploy_orders_filled_total
ploy_orders_failed_total
ploy_order_latency_ms
ploy_api_errors_total
```

---

### Ticket P3-2: Improve Health Check Endpoint
**Priority**: HIGH
**Effort**: 1 hour
**Owner**: Backend Developer

**Description**:
Current health endpoint is too simple. Need detailed status.

**Acceptance Criteria**:
- [ ] Return detailed health status (JSON)
- [ ] Check database connectivity
- [ ] Check API connectivity
- [ ] Check order queue health
- [ ] Return uptime and version
- [ ] HTTP 503 if unhealthy

**Response Format**:
```json
{
  "status": "healthy",
  "version": "0.1.0",
  "uptime_seconds": 3600,
  "checks": {
    "database": {"status": "up", "latency_ms": 5},
    "api_connection": {"status": "up", "latency_ms": 50},
    "order_queue": {"status": "up", "latency_ms": 0}
  }
}
```

---

### Ticket P3-3: Add Security Scanning Workflow
**Priority**: HIGH
**Effort**: 1 hour
**Owner**: DevOps Lead

**Description**:
Need scheduled security scanning beyond CI.

**Acceptance Criteria**:
- [ ] Create `.github/workflows/security.yml`
- [ ] Run cargo-audit daily
- [ ] Run gitleaks (secret detection)
- [ ] Check for outdated dependencies
- [ ] Generate SBOM
- [ ] Runs on schedule and on-demand

**Files to Create**:
- `.github/workflows/security.yml`

---

## PHASE 4: RESILIENCE (Weeks 4-5) - 6 hours

### Ticket P4-1: Add Database Migration Automation
**Priority**: HIGH
**Effort**: 2 hours
**Owner**: Backend Developer + DBA

**Description**:
Database migrations should be automated, versioned, and safe.

**Acceptance Criteria**:
- [ ] Create migrations/ directory with version control
- [ ] Automated migration on startup
- [ ] Rollback procedure documented
- [ ] Migration safety checks (backup before)
- [ ] Zero-downtime migration support

**Implementation**:
- Use sqlx migrations or Diesel
- Each change in separate file
- Reversible migrations

---

### Ticket P4-2: Implement Circuit Breaker for API Calls
**Priority**: HIGH
**Effort**: 2 hours
**Owner**: Backend Developer

**Description**:
API failures should trigger circuit breaker to prevent cascading failures.

**Acceptance Criteria**:
- [ ] Track API failure rate
- [ ] Open circuit on threshold exceeded
- [ ] Half-open state for recovery
- [ ] Exponential backoff on retry
- [ ] Metrics for circuit state

**Behavior**:
- Closed: Normal operation
- Open: Fail fast, don't call API
- Half-open: Try one request to recover

---

### Ticket P4-3: Set Up Multi-Instance Deployment
**Priority**: MEDIUM
**Effort**: 2 hours
**Owner**: DevOps Lead + Platform Engineer

**Description**:
Single EC2 instance is single point of failure. Need HA setup.

**Acceptance Criteria**:
- [ ] Deploy to 2+ EC2 instances
- [ ] Load balancer routes traffic
- [ ] Sticky sessions for trading state
- [ ] Shared database for state
- [ ] Can survive single instance failure

**Deployment**:
- AWS Classic Load Balancer
- Auto Scaling Group
- Shared PostgreSQL RDS
- Health check based routing

---

## PHASE 5: EXCELLENCE (Ongoing) - Variable

### Ticket P5-1: Infrastructure as Code (Terraform)
**Priority**: MEDIUM
**Effort**: 8 hours
**Owner**: Platform Engineer

**Description**:
Manually managing AWS resources. Should use Terraform.

**Deliverables**:
- [ ] `terraform/` directory
- [ ] EC2 configuration
- [ ] RDS database
- [ ] Load balancer
- [ ] Security groups
- [ ] IAM roles
- [ ] Terraform variables
- [ ] Deployment instructions

---

### Ticket P5-2: Kubernetes Deployment
**Priority**: MEDIUM
**Effort**: 12 hours
**Owner**: Platform Engineer

**Description**:
Kubernetes enables better scaling and resilience.

**Deliverables**:
- [ ] Kubernetes manifests
- [ ] Deployment strategy (rolling)
- [ ] Service configuration
- [ ] Persistent volume for data
- [ ] ConfigMap for configuration
- [ ] Secrets for credentials
- [ ] Health probes
- [ ] Resource limits

---

### Ticket P5-3: GitOps with ArgoCD
**Priority**: MEDIUM
**Effort**: 6 hours
**Owner**: Platform Engineer

**Description**:
Continuous deployment through git.

**Deliverables**:
- [ ] ArgoCD installation
- [ ] Application manifest
- [ ] Automatic sync on git push
- [ ] Manual approval option
- [ ] Rollback via git revert

---

### Ticket P5-4: Chaos Engineering Tests
**Priority**: LOW
**Effort**: 8 hours
**Owner**: Platform Engineer

**Description**:
Test system resilience to failures.

**Test Cases**:
- [ ] Stop one instance (should continue operating)
- [ ] Kill database connection (should fail gracefully)
- [ ] Saturate CPU (should stay responsive)
- [ ] Fill disk (should alert)
- [ ] Delay API responses (should timeout/retry)

---

### Ticket P5-5: Multi-Region Deployment
**Priority**: LOW
**Effort**: 16 hours
**Owner**: Platform Engineer

**Description**:
Geographic redundancy for disaster recovery.

**Regions**:
- [ ] US East (primary)
- [ ] EU West (secondary)
- [ ] Asia Pacific (optional)
- [ ] Global load balancing
- [ ] Database replication

---

## Cross-Cutting Concerns

### Documentation Tasks

**Ticket DOC-1: Create DEPLOYMENT.md**
**Priority**: HIGH
**Effort**: 1 hour
**Owner**: DevOps Lead

Deploy procedures, troubleshooting, rollback steps.

---

**Ticket DOC-2: Create RUNBOOKS.md**
**Priority**: HIGH
**Effort**: 1 hour
**Owner**: DevOps Lead

Daily checks, incident response, maintenance procedures.

---

**Ticket DOC-3: Create ARCHITECTURE.md**
**Priority**: MEDIUM
**Effort**: 1.5 hours
**Owner**: Platform Engineer

System design, deployment architecture, data flow.

---

## Implementation Schedule

```
Week 1: Phase 1 (Foundation)
  Mon-Tue: Tickets P1-1 to P1-3 (CI/security)
  Wed-Fri: Tickets P1-4 to P1-6 (Config & build)

Week 2: Phase 2 (Safety)
  Mon-Tue: Tickets P2-1 to P2-3 (Rollback & validation)
  Wed-Fri: Tickets P2-4 to P2-6 (Health & improve deploy)

Week 3: Phase 3 (Visibility)
  Mon-Wed: Tickets P3-1 to P3-3 (Metrics & security)
  Thu-Fri: Documentation (DOC tasks)

Week 4-5: Phase 4 (Resilience)
  Week 4: Tickets P4-1 to P4-2 (DB migrations & circuit breaker)
  Week 5: Ticket P4-3 (Multi-instance setup)

Ongoing: Phase 5 (Excellence)
  Tickets P5-1 to P5-5 (Infrastructure, Kubernetes, etc.)
```

---

## Dependency Map

```
P1-1 (Fix CI) ─┐
              ├─→ P2-6 (Improve deploy)
P1-2 (Security)─┘

P2-1 (Rollback) ─┐
                ├─→ P2-6 (Improve deploy)
P2-2 (Health check) ─┘

P3-1 (Metrics) ─┐
               ├─→ Monitoring dashboard (Phase 5)
P3-2 (Health) ──┘

P4-1 (DB migrations) ─┐
                     ├─→ P5-2 (Kubernetes)
P4-3 (Multi-instance)─┘

P5-2 (Kubernetes) ─┐
                  └─→ P5-3 (GitOps)
```

---

## Success Criteria

After completing all phases, you should be able to:

- [ ] Deploy safely with automated rollback
- [ ] Know within 1 minute if something is wrong
- [ ] Recover from failures automatically
- [ ] Track deployment success and trends
- [ ] Rotate secrets safely
- [ ] Run in multiple regions
- [ ] Scale automatically on demand
- [ ] Reproduce builds exactly
- [ ] Audit all changes via git

---

## Notes

- All tickets are independent within a phase
- Phase 1 should be completed before Phase 2
- Running tickets in parallel (within phase) is fine
- Estimate includes testing and documentation
- Adjust estimates based on team experience
- Create tickets in your project management system
- Link back to DEVOPS_ASSESSMENT.md for details

---

**Total Effort**: 19 hours (1-2 person weeks, 5 weeks timeline)
**Team Size**: 1-2 people
**Risk**: LOW (all changes are safe/backward compatible)
**Impact**: HIGH (prevents 80% of production issues)


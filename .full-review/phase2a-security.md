# Phase 2A Security Audit — ploy Trading System
**Branch**: `hotfix/staggered-arb-release-20260306` vs `main`
**Date**: 2026-03-11
**Auditor**: Security review agent (claude-sonnet-4-6)
**Scope**: Coordinator decomposition, staggered-arb-live strategy, control plane, CLI routing

---

## Executive Summary

The codebase demonstrates solid security fundamentals in several areas: parameterized SQL throughout (no injection risk), constant-time token comparison, secure-by-default cookie flags, and a well-layered coordinator ingress pipeline. However, **six findings warrant immediate attention** before production release, including one Critical and three High severity issues. The most dangerous is the foreground bypass path that allows live order execution with no coordinator risk controls when the coordinator URL is unreachable or unconfigured.

---

## Findings

---

### F-01 — CRITICAL: Foreground Fallback Executes Live Orders Without Any Risk Controls

**Severity**: Critical
**CWE**: CWE-284 (Improper Access Control)
**Files**:
- `src/cli/strategy/runtime_ops/foreground_submit.rs` lines 23–44, 270–315
- `src/cli/strategy/runtime_ops/foreground.rs` lines 33–85, 196–201

**Description**

`ForegroundIntentSubmitter::submit()` implements a two-stage fallback:

1. If `deployment_id` is present in metadata → route through coordinator HTTP ingress (all risk controls apply).
2. If `deployment_id` is absent → fall through to `self.executor.execute(order)` — a **direct CLOB call** with no admission check, no risk gate, no governance check, no capital allocator, no circuit breaker, no journal entry.

The fallback is triggered silently whenever `deployment_id_from_metadata()` returns `None`. Any strategy that does not embed `deployment_id` in its intent metadata (including the legacy `staggered_arb_live` path, which constructs intents via `crypto_submit_intent()` without guaranteed deployment metadata) will execute live orders through this path.

Additionally, if the coordinator HTTP endpoint is unreachable (network partition, coordinator not started, wrong `PLOY_RPC_COORDINATOR_INTENT_URL`), `submit_intent_via_coordinator` returns `Err(...)`, which propagates up and causes the order to be marked `Failed` — but the executor fallback is **not** attempted in that error case. However, the silent `None` path (missing deployment_id) does reach the executor directly.

**Attack / Failure Scenario**

An operator runs `ploy strategy start staggered_arb_live --foreground` on a live host. The strategy's `crypto_submit_intent()` helper does not inject `deployment_id` into the intent metadata. Every order silently bypasses the coordinator and executes directly against the CLOB. Exposure limits, circuit breaker, daily loss limit, governance pauses, and the audit journal are all bypassed. The system can accumulate unlimited live exposure.

**Remediation**

1. Remove the direct-executor fallback from `ForegroundIntentSubmitter::submit()` entirely. If coordinator routing fails, the order must be rejected, not silently executed.
2. Enforce that `deployment_id` is always present in strategy intents at the `StrategyOrderIntent` construction site; return an error at strategy startup if it is missing.
3. Add a startup assertion in `run_strategy_foreground` that verifies the coordinator URL is reachable before accepting live orders.

---

### F-02 — HIGH: TOCTOU Race on Elevated→Normal Circuit Breaker Transition

**Severity**: High
**CWE**: CWE-362 (Concurrent Execution Using Shared Resource with Improper Synchronization)
**File**: `src/coordinator/risk/transitions.rs` lines 68–71

**Description**

In `record_success()`, the state transition from `Elevated` to `Normal` is performed with two separate lock acquisitions:

```rust
if *self.state.read().await == PlatformRiskState::Elevated {
    *self.state.write().await = PlatformRiskState::Normal;
    info!("Risk state normalized after successful execution");
}
```

Between the `read()` guard drop and the `write()` guard acquisition, a concurrent call to `trigger_circuit_breaker()` can set the state to `Halted`. The subsequent `write()` then overwrites `Halted` with `Normal`, silently clearing the circuit breaker without logging a reset event, without clearing `halted_at`, and without resetting consecutive failure counters.

**Attack / Failure Scenario**

1. Platform accumulates failures → state transitions to `Elevated`.
2. One more failure triggers `trigger_circuit_breaker()` → state set to `Halted`.
3. Concurrently, a fill callback calls `record_success()`.
4. The read sees `Elevated` (stale), the write overwrites `Halted` → `Normal`.
5. Circuit breaker is silently cleared. Trading resumes despite the halt condition still being valid. `halted_at` remains set to the old timestamp, causing the next `try_auto_recover_circuit_breaker()` to behave incorrectly.

**Remediation**

Replace the read-then-write pattern with a single write-lock acquisition and an atomic compare-and-swap:

```rust
let mut state = self.state.write().await;
if *state == PlatformRiskState::Elevated {
    *state = PlatformRiskState::Normal;
    info!("Risk state normalized after successful execution");
}
```

This eliminates the window between the read and write.

---

### F-03 — HIGH: Governance State Not Restored on Restart Unless Pool Is Explicitly Wired

**Severity**: High
**CWE**: CWE-665 (Improper Initialization)
**File**: `src/coordinator/coordinator/recovery.rs` lines 55–72

**Description**

`load_persisted_governance_policy()` correctly loads governance state from the database, but it is guarded by:

```rust
let Some(pool) = self.governance_store_pool.as_ref() else {
    return Ok(());
};
```

`governance_store_pool` is set only via `set_governance_store_pool()`, which is a separate opt-in call. If the bootstrap path does not call this method (e.g., a new bootstrap variant, a test harness, or a misconfigured deployment), `GovernanceController::new()` initializes with `IngressMode::Running` and an empty `blocked_domains` set, silently discarding any operator-set domain pauses or global halts that were persisted before the restart.

The `GovernanceController::new()` constructor (governance.rs line 178–185) always starts with defaults regardless of what is in the database.

**Attack / Failure Scenario**

An operator pauses the Sports domain via the governance API due to a data quality incident. The process restarts (OOM kill, deploy). The governance pool is not wired in the new bootstrap path. Sports domain resumes trading immediately, placing orders against stale or incorrect market data.

**Remediation**

1. Make `governance_store_pool` a required field in `Coordinator` construction, not an optional post-construction setter. Fail fast at startup if the pool is absent and governance persistence is expected.
2. Alternatively, call `load_persisted_governance_policy()` unconditionally in the bootstrap sequence and log a warning (not silently skip) if the pool is absent.
3. Add an integration test that verifies governance state survives a coordinator restart.

---

### F-04 — HIGH: Capital Allocator Exposure Gap After Fill (Double-Entry Risk)

**Severity**: High
**CWE**: CWE-841 (Improper Enforcement of Behavioral Workflow)
**File**: `src/coordinator/coordinator/recovery.rs` lines 77–231 (recovery path); `src/coordinator/risk/exposure.rs` lines 44–77

**Description**

As described in the pre-audit context (A-H2): after a fill is processed, `MarketCapitalAllocator` shows zero exposure for the filled position because the allocator tracks *pending reservations*, not *open positions*. The reservation is released on fill, but the position tracker (`self.positions`) is updated separately. Between the reservation release and the next `refresh_risk_exposure_for_agent()` call, the risk gate's `current_agent_exposure` reads from `agent_stats[agent_id].exposure`, which is only updated by `update_agent_exposure()`.

In the recovery path (`restore_runtime_state_from_execution_log`), `settle_domain_success()` is called for each fill, but `refresh_risk_exposure_for_agent()` is called only once at the end of the loop (line 216), not after each fill. During the replay loop, the risk gate's exposure counters are stale, meaning a strategy that submits a new intent during recovery could pass exposure checks against an underestimated baseline.

**Attack / Failure Scenario**

Strategy has a $50 exposure limit. It holds a $45 open position. Process restarts. During the recovery replay loop, the risk gate shows $0 exposure (not yet refreshed). A new intent for $48 arrives during recovery. It passes the $50 exposure check. After recovery completes, actual exposure is $93 — nearly 2x the configured limit.

**Remediation**

1. Call `refresh_risk_exposure_for_agent()` after each fill is replayed in the recovery loop, not only at the end.
2. Consider holding the coordinator's ingress channel closed (rejecting new intents) until recovery is fully complete and all exposure counters are refreshed.

---

### F-05 — MEDIUM: Deployment JSON Loaded from Attacker-Controlled File Paths

**Severity**: Medium
**CWE**: CWE-73 (External Control of File Name or Path)
**File**: `src/coordinator/admission/deployments.rs` lines 34–62, 191–217

**Description**

`load_strategy_deployments()` resolves the deployment configuration from multiple sources in priority order:

1. `PLOY_STRATEGY_DEPLOYMENTS_JSON` env var (raw JSON string)
2. `PLOY_DEPLOYMENTS_JSON` env var
3. `PLOY_DEPLOYMENTS_FILE` env var (arbitrary file path)
4. Hardcoded candidate paths: `data/state/deployments.json`, `/opt/ploy/data/state/deployments.json`, `deployment/deployments.json`, `/opt/ploy/deployment/deployments.json`

The `PLOY_DEPLOYMENTS_FILE` env var accepts an arbitrary filesystem path with no validation. On a shared host or in a container with a writable volume, an attacker who can set environment variables or write to the candidate paths can inject a crafted `deployments.json` that:
- Enables a disabled deployment
- Changes `execution_mode` from `DryRunOnly` to `LiveOnly`
- Removes `account_ids` restrictions to allow any account
- Sets `lifecycle_stage` to `Live` to bypass lifecycle checks

The `parse_strategy_deployments()` function deserializes the JSON without any signature or integrity check.

**Remediation**

1. Restrict `PLOY_DEPLOYMENTS_FILE` to paths within a known safe prefix (e.g., `/opt/ploy/`).
2. Add a HMAC or checksum verification for deployment JSON loaded from disk, signed with a key only the operator controls.
3. Log the resolved deployment source (env var vs. file path vs. which candidate) at startup at `info` level so operators can audit which source was used.

---

### F-06 — MEDIUM: Sidecar Token Accepted via Standard Authorization Header (Token Confusion)

**Severity**: Medium
**CWE**: CWE-287 (Improper Authentication)
**File**: `src/api/auth.rs` lines 182–213

**Description**

`ensure_sidecar_authorized()` accepts the sidecar token from either `x-ploy-sidecar-token` (dedicated header) or the standard `Authorization: Bearer <token>` header. Similarly, `ensure_admin_authorized()` accepts the admin token from `x-ploy-admin-token` or `Authorization: Bearer <token>`.

`ensure_sidecar_or_admin_authorized()` (line 216–223) tries sidecar first, then admin. This means:
- A request carrying the admin token in `Authorization: Bearer` will first be tested as a sidecar token (ct_eq will fail), then tested as an admin token (will pass).
- A request carrying the sidecar token in `Authorization: Bearer` will pass sidecar auth.

The risk is that any endpoint that calls `ensure_sidecar_or_admin_authorized()` can be accessed with either token via the generic `Authorization` header, making it impossible to enforce token-role separation at the HTTP layer (e.g., via a WAF or API gateway that inspects the `Authorization` header). A sidecar token holder can reach admin-only endpoints if those endpoints use `ensure_sidecar_or_admin_authorized`.

Additionally, the admin cookie check (lines 167–173) accepts both the SHA-256 fingerprint of the token AND the raw token itself:
```rust
if cookie.as_deref().is_some_and(|v| ct_eq(v, &expected_fp) || ct_eq(v, &expected))
```
This means if the raw admin token is ever stored in a cookie (e.g., by a misconfigured client), it will be accepted. The raw token in a cookie is a higher-risk exposure than the fingerprint.

**Remediation**

1. Remove the `Authorization: Bearer` fallback from both `ensure_sidecar_authorized` and `ensure_admin_authorized`. Require dedicated headers (`x-ploy-sidecar-token`, `x-ploy-admin-token`) exclusively. This enables WAF-level enforcement.
2. Remove the `ct_eq(v, &expected)` branch from the cookie check. The cookie should only ever contain the fingerprint, never the raw token.

---

### F-07 — MEDIUM: Unbounded Fill Replay Blocks Startup (Pagination Missing)

**Severity**: Medium
**CWE**: CWE-400 (Uncontrolled Resource Consumption)
**File**: `src/coordinator/coordinator/recovery.rs` lines 77–94

**Description**

`restore_runtime_state_from_execution_log()` queries fills with a date window of `[today 00:00:00, today 24:00:00)`. This is correctly bounded to today. However, the recovery function calls `self.positions.clear()` and `self.capital_policy.reset_runtime_state()` before replaying fills (lines 102–103). If the fill query returns a large result set (e.g., a high-frequency strategy with thousands of fills today), the replay loop runs synchronously in the coordinator startup path, blocking the ingress channel for an extended period. There is no timeout or pagination on the fill query.

Additionally, the recovery loop calls `settle_domain_success()` for every fill, which acquires and releases multiple async locks per iteration. With thousands of fills, this can take seconds to minutes, during which the coordinator is not accepting new intents.

**Remediation**

1. Add a `LIMIT` clause or pagination to `load_execution_restore_data` to cap the number of fills replayed (e.g., last 10,000 fills today).
2. Consider running recovery in a background task and holding the ingress channel in a `Paused` state until recovery completes, rather than blocking startup.

---

### F-08 — MEDIUM: Grok Decision Prompt Contains Unvalidated External Strings

**Severity**: Medium
**CWE**: CWE-77 (Improper Neutralization of Special Elements used in a Command)
**File**: `src/api/handlers/sidecar/grok_decision.rs` lines 59–96, 239–282

**Description**

The `GrokDecisionRequest` struct accepts several free-text fields from the TypeScript sidecar that are passed directly into the Grok prompt via `build_unified_prompt()`:

- `momentum_narrative: Option<String>` — no length limit, no sanitization
- `research_summary: Option<String>` — no length limit, no sanitization
- `injury_updates[].details: Option<String>` — no length limit, no sanitization
- `clock: String` — no format validation

These strings are embedded into the LLM prompt. A compromised sidecar (or a MITM on the loopback interface between the TypeScript sidecar and the Rust backend) could inject prompt-manipulation content into these fields, potentially causing Grok to return a `"trade"` decision when it should return `"pass"`, or to return fabricated `fair_value` / `edge` values.

The `raw_response` from Grok is also persisted verbatim to the database (line 446), which could contain adversarial content if the LLM is manipulated.

**Remediation**

1. Enforce maximum length limits on all free-text fields in `GrokDecisionRequest` (e.g., 2000 chars for narrative fields, 100 chars for `clock`).
2. Strip or escape characters that are known prompt-injection vectors (e.g., sequences like `\n\nSystem:`, `\n\nHuman:`, `IGNORE PREVIOUS INSTRUCTIONS`) before embedding in the prompt.
3. Validate `clock` against a known format (e.g., `MM:SS` regex) and reject malformed values.
4. The existing memory notes confirm "Prompt injection sanitization" was applied to `autonomous.rs` — apply the same sanitization here.

---

### F-09 — LOW: Deployment Gate Bypass for Sell Intents Undocumented

**Severity**: Low
**CWE**: CWE-284 (Improper Access Control)
**File**: `src/coordinator/admission/deployments.rs` line 81; `src/coordinator/admission.rs` line 50

**Description**

The deployment gate check explicitly skips sell intents:
```rust
if !intent.is_buy || dry_run || !deployment_gate_required() {
    return Ok(());
}
```

This is architecturally correct for reduce-only exits (you must be able to close positions even if the deployment is disabled). However, it is not documented in the code, and the same bypass applies to `buy_intent_missing_deployment_reason()` (line 11–19 of deployments.rs). A sell intent with no `deployment_id` metadata passes all deployment gate checks silently.

If a strategy bug generates a spurious sell intent for a token the account does not hold, the reduce-only guard in `coordinator/ingress.rs` (lines 39–69) will catch it — but only if position tracking is accurate. If position tracking is stale (e.g., immediately after restart before recovery completes), a spurious sell could pass.

**Remediation**

1. Add a code comment at the sell bypass explaining the reduce-only rationale.
2. Ensure the reduce-only guard in `handle_order_intent` is always evaluated before the sell reaches the executor, even during recovery.

---

### F-10 — LOW: Admin Token Fingerprint Uses Unsalted SHA-256

**Severity**: Low
**CWE**: CWE-916 (Use of Password Hash With Insufficient Computational Effort)
**File**: `src/api/auth.rs` lines 87–91

**Description**

`admin_token_fingerprint()` computes `SHA-256(token)` with no salt. The fingerprint is stored in the session cookie. If an attacker obtains the cookie value, they can attempt to reverse the token via a rainbow table or brute-force attack (SHA-256 is fast; a short or low-entropy token could be reversed in seconds).

The comment in the code acknowledges that length leaks are acceptable for fixed-format bearer tokens, but does not address the unsalted hash risk.

**Remediation**

1. Use HMAC-SHA256 with a server-side secret as the cookie fingerprint: `HMAC-SHA256(server_secret, token)`. This makes the fingerprint unguessable even if the token is known.
2. Alternatively, use a random session ID stored server-side (mapping to the token), which is the standard session cookie pattern.

---

### F-11 — LOW: `PLOY_DEPLOYMENT_GATE_REQUIRED=false` Has No Audit Trail

**Severity**: Low
**CWE**: CWE-1188 (Insecure Default Initialization of Resource)
**Files**:
- `src/api/handlers/sidecar/ingress.rs` lines 110–120
- `src/coordinator/admission/deployments.rs` lines 22–32

**Description**

Setting `PLOY_DEPLOYMENT_GATE_REQUIRED=false` (or `0`, `no`, `off`) disables the deployment gate for all intents globally, including live buy intents. This env var is checked at runtime on every request (not cached), meaning it can be changed without restart. There is no audit log entry when the gate is disabled.

**Remediation**

1. Log a `warn!` at startup and at each request where the gate is disabled, so operators are alerted.
2. Consider requiring a separate `PLOY_DEPLOYMENT_GATE_OVERRIDE_TOKEN` to be set alongside the disable flag, to prevent accidental misconfiguration.

---

## Summary Table

| ID   | Severity | Title                                                        | File(s)                                      |
|------|----------|--------------------------------------------------------------|----------------------------------------------|
| F-01 | Critical | Foreground fallback executes live orders without risk controls | `foreground_submit.rs`, `foreground.rs`      |
| F-02 | High     | TOCTOU race on Elevated→Normal circuit breaker transition    | `risk/transitions.rs`                        |
| F-03 | High     | Governance state not restored unless pool explicitly wired   | `coordinator/recovery.rs`                    |
| F-04 | High     | Capital allocator exposure gap after fill (double-entry risk)| `coordinator/recovery.rs`, `risk/exposure.rs`|
| F-05 | Medium   | Deployment JSON loaded from attacker-controlled file paths   | `admission/deployments.rs`                   |
| F-06 | Medium   | Sidecar token accepted via Authorization header (confusion)  | `api/auth.rs`                                |
| F-07 | Medium   | Unbounded fill replay blocks startup (pagination missing)    | `coordinator/recovery.rs`                    |
| F-08 | Medium   | Grok prompt contains unvalidated external strings            | `sidecar/grok_decision.rs`                   |
| F-09 | Low      | Deployment gate bypass for sells undocumented                | `admission/deployments.rs`                   |
| F-10 | Low      | Admin token fingerprint uses unsalted SHA-256                | `api/auth.rs`                                |
| F-11 | Low      | `PLOY_DEPLOYMENT_GATE_REQUIRED=false` has no audit trail     | `ingress.rs`, `admission/deployments.rs`     |

---

## Positive Security Observations

The following patterns are well-implemented and should be preserved:

- **Parameterized SQL throughout**: All database queries use `sqlx` bound parameters. No SQL injection risk found.
- **Constant-time token comparison**: `ct_eq()` in `auth.rs` correctly uses XOR-fold to prevent timing side-channels.
- **Secure cookie defaults**: `HttpOnly`, `SameSite=Strict`, `Secure` (default true), configurable max-age with a minimum floor of 60 seconds.
- **Sidecar auth required by default**: `sidecar_auth_required()` defaults to `true` with a warning log when the env var is absent.
- **Reduce-only sell guard**: `sell_reduce_only_violation_reason()` prevents spurious sell intents from exceeding tracked open positions.
- **Governance persistence**: `persist_governance_policy()` uses a transaction with both upsert and history append, providing a complete audit trail of policy changes.
- **Circuit breaker idempotency**: `trigger_circuit_breaker()` checks for `Halted` state before writing, preventing redundant events.
- **Deployment gate ambiguity rejection**: Ambiguous deployment resolution (multiple candidates) is rejected rather than silently picking one.
- **External critical priority clamped**: `clamp_external_priority()` prevents external callers from escalating to `Critical` priority without explicit env var opt-in.

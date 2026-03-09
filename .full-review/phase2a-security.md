# Phase 2a: Security Audit — Ploy Trading System

**Date**: 2026-03-08
**Scope**: Full Ploy trading system (Rust ~165K lines, TypeScript sidecar, React frontend)
**Auditor**: Automated security review (claude-opus-4.6)
**Classification**: CONFIDENTIAL — contains vulnerability details for a live trading system

---

## Executive Summary

The Ploy trading system demonstrates strong security fundamentals: parameterized SQL queries throughout, constant-time token comparison, private key zeroization, and defense-in-depth auth layers. However, several findings require attention — particularly around sensitive data exposure in logs, missing API hardening, and the Wallet struct's `#[derive(Clone)]` which silently copies the private key signer across memory.

**Finding Distribution**: 2 High, 7 Medium, 6 Low, 3 Informational

---

## Critical Findings

*None identified.* The system's core security architecture is sound.

---

## High Severity Findings

### H-01: `ApiCredentials` derives `Debug` — secrets printed in logs/panics

**Severity**: High (CVSS 7.5)
**CWE**: CWE-532 (Insertion of Sensitive Information into Log File)
**Location**: `src/signing/hmac.rs:12`

```rust
#[derive(Debug, Clone)]
pub struct ApiCredentials {
    pub api_key: String,
    pub secret: String,
    pub passphrase: String,
}
```

The `Debug` derive on `ApiCredentials` means any `{:?}` formatting (panic messages, `tracing::debug!`, error chains) will dump the API key, HMAC secret, and passphrase to logs. On the production host (`tango-1-1`), journald captures all output.

**Attack scenario**: An attacker with read access to `/var/log/journal` or systemd logs obtains full Polymarket API credentials, enabling unauthorized trading on the account.

**Remediation**:
```rust
impl std::fmt::Debug for ApiCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiCredentials")
            .field("api_key", &format!("{}...", &self.api_key[..8.min(self.api_key.len())]))
            .field("secret", &"[REDACTED]")
            .field("passphrase", &"[REDACTED]")
            .finish()
    }
}
```

---

### H-02: HMAC debug log leaks full signing message including body content

**Severity**: High (CVSS 7.1)
**CWE**: CWE-532 (Insertion of Sensitive Information into Log File)
**Location**: `src/signing/hmac.rs:106-113`

```rust
tracing::debug!(
    "HMAC signing - timestamp: {}, method: {}, path: {}, message: '{}', address: {}",
    timestamp, method, path, message, self.address
);
```

The `message` variable contains the full HMAC signing payload: `{timestamp}{METHOD}{path}{body}`. For order submissions, the body includes order details. Combined with the timestamp and address, this provides enough information for an attacker to reconstruct valid HMAC signatures if they can observe the log output and know the signing algorithm.

**Remediation**: Remove or redact the `message` field from the debug log:
```rust
tracing::debug!(
    "HMAC signing - method: {}, path: {}, address: {}",
    method, path, self.address
);
```

---

## Medium Severity Findings

### M-01: `Wallet` derives `Clone` — private key signer copied across memory

**Severity**: Medium (CVSS 5.3)
**CWE**: CWE-316 (Cleartext Storage of Sensitive Information in Memory)
**Location**: `src/signing/wallet.rs:14`

```rust
#[derive(Clone)]
pub struct Wallet {
    inner: PrivateKeySigner,
    chain_id: u64,
}
```

The `Wallet` struct claims "the private key is only used during wallet creation and then immediately zeroized" (line 12-13), but `#[derive(Clone)]` on the struct means every `.clone()` call duplicates the `PrivateKeySigner` (which holds the private key in memory). The `PolymarketClient::clone()` at `polymarket_clob.rs:85` clones the wallet via `Arc<Wallet>`, which is safe (reference count), but the `Clone` derive itself is misleading and could lead to future misuse where `wallet.clone()` creates an untracked copy of the key material.

Additionally, `pub fn inner(&self) -> &PrivateKeySigner` (line 120) exposes the raw signer, which could be used to extract the private key, undermining the zeroization guarantee.

**Remediation**:
1. Remove `#[derive(Clone)]` from `Wallet` — force all sharing through `Arc<Wallet>`.
2. Remove or restrict `pub fn inner()` to `pub(crate)` and audit all call sites.

---

### M-02: No API rate limiting — brute-force and DoS exposure

**Severity**: Medium (CVSS 6.5)
**CWE**: CWE-307 (Improper Restriction of Excessive Authentication Attempts)
**Location**: `src/api/routes.rs` (entire router)

The Axum API server has no rate limiting middleware. All endpoints, including:
- `/api/auth/login` (admin token brute-force)
- `/api/sidecar/orders` (order flooding)
- `/api/system/halt` (repeated halt attempts)

...are unprotected against excessive requests. The admin token comparison is constant-time (good), but without rate limiting an attacker can attempt millions of tokens per second.

**Remediation**: Add `tower::limit::RateLimitLayer` or `tower_governor` middleware:
```rust
use tower::limit::RateLimitLayer;
use std::time::Duration;

// In create_router():
router.layer(RateLimitLayer::new(100, Duration::from_secs(60)))
```

For the login endpoint specifically, implement exponential backoff after N failed attempts.

---

### M-03: Missing security headers on API responses

**Severity**: Medium (CVSS 5.3)
**CWE**: CWE-693 (Protection Mechanism Failure)
**Location**: `src/api/routes.rs`

The API server does not set any security headers:
- No `X-Content-Type-Options: nosniff`
- No `X-Frame-Options: DENY`
- No `Content-Security-Policy`
- No `Strict-Transport-Security` (HSTS)
- No `Cache-Control: no-store` on sensitive endpoints

**Remediation**: Add a `tower_http::set_header::SetResponseHeaderLayer` or custom middleware:
```rust
use axum::http::header;
use tower_http::set_header::SetResponseHeaderLayer;

router
    .layer(SetResponseHeaderLayer::overriding(
        header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")))
    .layer(SetResponseHeaderLayer::overriding(
        header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY")))
```

---

### M-04: WebSocket auth token passed in query parameter — logged in access logs

**Severity**: Medium (CVSS 5.9)
**CWE**: CWE-598 (Use of GET Request Method With Sensitive Query Strings)
**Location**: `src/api/websocket.rs:22-36`

```rust
pub async fn websocket_handler(
    ws: WebSocketUpgrade,
    Query(auth): Query<WsAuth>,  // token in ?token=...
    ...
```

The admin token is passed as a URL query parameter (`/ws?token=SECRET`). Query parameters are:
- Logged in web server access logs
- Stored in browser history
- Visible in network monitoring tools
- Potentially cached by proxies

**Remediation**: Use the first WebSocket message for authentication instead of query params, or use a short-lived session token obtained via the `/api/auth/login` endpoint.

---

### M-05: `DatabaseConfig` derives `Debug` — connection URL with credentials exposed

**Severity**: Medium (CVSS 5.5)
**CWE**: CWE-532 (Insertion of Sensitive Information into Log File)
**Location**: `src/config.rs:673-680`

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseConfig {
    pub url: String,           // Contains postgres://user:password@host/db
    pub max_connections: u32,
}
```

The database URL typically contains credentials (`postgres://ploy:PASSWORD@localhost:5432/ploy`). The `Debug` derive means any debug formatting of `AppConfig` will dump the full connection string.

**Remediation**: Implement a custom `Debug` that redacts the URL:
```rust
impl std::fmt::Debug for DatabaseConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DatabaseConfig")
            .field("url", &"[REDACTED]")
            .field("max_connections", &self.max_connections)
            .finish()
    }
}
```

---

### M-06: Emergency stop `is_stopped` uses `Ordering::Relaxed` — may miss stop signal

**Severity**: Medium (CVSS 5.0)
**CWE**: CWE-362 (Concurrent Execution Using Shared Resource with Improper Synchronization)
**Location**: `src/coordination/emergency_stop.rs:133`

```rust
pub fn is_stopped(&self) -> bool {
    self.is_stopped.load(Ordering::Relaxed)
}
```

While `trigger()` uses `Ordering::SeqCst` to set the flag (line 152), the read side uses `Ordering::Relaxed`. On weakly-ordered architectures (ARM, which Alibaba Cloud may use), a thread could continue executing trades for an unbounded time after emergency stop is triggered, because `Relaxed` provides no cross-thread visibility guarantee.

For a financial safety mechanism, this is unacceptable.

**Remediation**: Change to `Ordering::Acquire`:
```rust
pub fn is_stopped(&self) -> bool {
    self.is_stopped.load(Ordering::Acquire)
}
```

---

### M-07: Sidecar risk guard only checks `submit_order` tool name — bypassable

**Severity**: Medium (CVSS 5.5)
**CWE**: CWE-863 (Incorrect Authorization)
**Location**: `ploy-sidecar/src/hooks/risk-guard.ts:32`

```typescript
if (!input.tool_name.includes("submit_order")) return {};
```

The risk guard uses `includes("submit_order")` which is a substring match. If a new tool is added with a different name that can submit orders (e.g., `batch_submit`, `emergency_order`, `submit_intent`), the guard will not intercept it. Additionally, the guard runs client-side in the sidecar process — the Rust backend is the true enforcement point.

The `MAX_PRICE = 0.20` hardcoded limit also means the sidecar cannot trade any market where the fair value is above $0.20, which may be overly restrictive for non-NBA strategies.

**Remediation**:
1. Use an allowlist approach: only permit known-safe tools, deny everything else.
2. Ensure the Rust backend (`/api/sidecar/orders`) independently validates all limits.

---

## Low Severity Findings

### L-01: `KalshiConfig` stores API key and secret in `Option<String>` without zeroization

**Severity**: Low (CVSS 3.7)
**CWE**: CWE-316 (Cleartext Storage of Sensitive Information in Memory)
**Location**: `src/config.rs:611-631`

Unlike the Polymarket private key (which uses `zeroize`), the Kalshi API key and secret are stored as plain `String` values in `KalshiConfig`. They persist in memory for the lifetime of the process and are never zeroized.

**Remediation**: Use `zeroize::Zeroizing<String>` for `api_key` and `api_secret`.

---

### L-02: `GrokConfig` stores API key as plain `String`

**Severity**: Low (CVSS 3.7)
**CWE**: CWE-316 (Cleartext Storage of Sensitive Information in Memory)
**Location**: `src/ai_clients/grok.rs:21`

```rust
pub struct GrokConfig {
    pub api_key: String,
    ...
}
```

The Grok API key is stored as a plain `String` and never zeroized. It also derives `Debug` (line 18) and `Clone` (line 18), meaning it can be printed and duplicated freely.

**Remediation**: Use `zeroize::Zeroizing<String>` and implement custom `Debug`.

---

### L-03: Prompt injection mitigation is incomplete — 500-char truncation may be insufficient

**Severity**: Low (CVSS 4.3)
**CWE**: CWE-77 (Improper Neutralization of Special Elements used in a Command)
**Location**: `src/ai_clients/autonomous.rs:303-309`

```rust
fn sanitize_for_prompt(input: &str) -> String {
    input
        .chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .take(500)
        .collect()
}
```

The sanitization strips control characters and truncates to 500 chars, but does not address:
- Markdown/formatting injection (e.g., `## SYSTEM: Ignore previous instructions`)
- Unicode homoglyphs that could confuse the LLM
- Nested prompt delimiters that could escape the context

However, the autonomous agent's direct order submission is disabled (lines 489-492, 530-532), routing through the coordinator instead. This significantly reduces the impact.

**Remediation**: Add delimiter escaping and consider a structured data format instead of string interpolation for LLM context.

---

### L-04: CORS allows `localhost:5173` by default in production

**Severity**: Low (CVSS 3.1)
**CWE**: CWE-942 (Permissive Cross-domain Policy with Untrusted Domains)
**Location**: `src/api/routes.rs:24-27`

```rust
if origins.is_empty() {
    origins.push(HeaderValue::from_static("http://localhost:5173"));
    origins.push(HeaderValue::from_static("http://127.0.0.1:5173"));
}
```

When `PLOY_API_CORS_ALLOWED_ORIGINS` is not set, the API defaults to allowing `localhost:5173`. On the production server, this means any process running on the same host on port 5173 can make authenticated cross-origin requests.

**Remediation**: In production, require explicit CORS origin configuration. If no origins are configured, deny all cross-origin requests.

---

### L-05: `stop-trading.yml` writes SSH private key to disk

**Severity**: Low (CVSS 3.3)
**CWE**: CWE-312 (Cleartext Storage of Sensitive Information)
**Location**: `.github/workflows/stop-trading.yml:25-26`

```yaml
echo "$PRIVATE_KEY" > private_key.pem
chmod 600 private_key.pem
```

The SSH private key is written to the runner's filesystem. While `rm private_key.pem` is called at the end, if the workflow fails between write and delete, the key persists on the runner. The `release-aliyun.yml` workflow avoids this by using `appleboy/ssh-action` which handles key material in memory.

**Remediation**: Migrate to `appleboy/ssh-action` like the other workflows, or use `trap` to ensure cleanup on failure.

---

### L-06: `Wallet::private_key_hex()` still in public API despite deprecation

**Severity**: Low (CVSS 2.7)
**CWE**: CWE-749 (Exposed Dangerous Method or Function)
**Location**: `src/signing/wallet.rs:89-98`

The method is deprecated and returns an empty string (good), but it remains `pub` and could confuse callers or be accidentally relied upon. No call sites exist currently.

**Remediation**: Remove the method entirely or change visibility to `pub(crate)`.

---

## Informational Findings

### I-01: SQL injection risk is well-mitigated

All SQL queries use parameterized `sqlx::query` with `$1`, `$2` bind parameters. No `format!`-based SQL construction was found. The `QueryBuilder` usage in `get_security_events` (system.rs:609) correctly uses `.push_bind()`. The `nonce_manager.rs` cleanup query uses `$2 || ' days'` which is safe because `$2` is bound as an integer.

**Status**: No action required.

---

### I-02: Authentication architecture is sound

- Constant-time comparison (`ct_eq`) for token validation
- SHA-256 fingerprinting for session cookies (not raw token)
- `HttpOnly; SameSite=Strict; Secure` cookie attributes (with env override for dev)
- Separate admin and sidecar token scopes
- Default-deny: auth required unless explicitly disabled via env var
- Sidecar endpoints require sidecar token; system endpoints require admin token

**Status**: No action required.

---

### I-03: Autonomous agent direct order submission is correctly disabled

The `AutonomousAgent::execute_action()` method returns `Err(PloyError::Validation("autonomous direct submit is disabled"))` for both `EnterPosition` and `ExitPosition` actions (lines 489-492, 530-532). All orders must route through the coordinator intent ingress. This is a strong defense-in-depth measure.

**Status**: No action required. The Phase 1 finding about "autonomous.rs bypasses RiskManager" is no longer accurate — direct submission is blocked.

---

## OWASP Top 10 Assessment

| Category | Status | Notes |
|----------|--------|-------|
| A01: Broken Access Control | PASS | Admin/sidecar token separation, deployment gate, domain scoping |
| A02: Cryptographic Failures | WARN | H-01, H-02 (credential exposure in logs); key material otherwise well-handled |
| A03: Injection | PASS | All SQL parameterized; prompt injection mitigated (L-03) |
| A04: Insecure Design | PASS | Defense-in-depth: coordinator gateway, deployment lifecycle gates, dry-run defaults |
| A05: Security Misconfiguration | WARN | M-03 (missing headers), L-04 (CORS defaults) |
| A06: Vulnerable Components | INFO | Dependencies are recent versions; no known CVEs in pinned versions |
| A07: Auth Failures | WARN | M-02 (no rate limiting on login), M-04 (WS token in query) |
| A08: Data Integrity Failures | PASS | EIP-712 signing, HMAC auth, deployment gate enforcement |
| A09: Logging Failures | WARN | H-01, H-02 (over-logging secrets); audit logging present for system events |
| A10: SSRF | PASS | No user-controlled URL fetching in the Rust backend |

---

## Dependency Assessment

### Rust (Cargo.toml)
- `sqlx 0.8.6`: Current, no known CVEs
- `reqwest 0.12`: Current
- `alloy 1.x`: Current
- `axum 0.8`: Current
- `tokio 1.x`: Current
- `hmac 0.12` / `sha2 0.10`: Current, well-audited crates
- `zeroize 1.8.2`: Current
- `polymarket-client-sdk 0.4`: Vendored (`vendor/polymarket-client-sdk`), should be periodically synced

### TypeScript (ploy-sidecar/package.json)
- `@anthropic-ai/claude-agent-sdk ^0.2.37`: Recent
- `zod ^4.0.0`: Recent
- Minimal dependency surface (good)

### React (ploy-frontend)
- No `dangerouslySetInnerHTML` usage found (good)
- Standard React/Vite stack

---

## Trading-Specific Security Assessment

| Control | Status | Details |
|---------|--------|---------|
| Order size limits | PASS | Autonomous agent: $50/trade, $200 total. Sidecar risk guard: $50 max |
| Position limits | PASS | `max_positions`, `max_positions_per_symbol` in RiskConfig |
| Emergency stop | WARN | M-06: Relaxed ordering on read side |
| Dry-run default | PASS | `dry_run` defaults to `true` in AppState |
| Direct live gate | PASS | `PLOY_ALLOW_DIRECT_LIVE` env required for legacy paths |
| Deployment lifecycle | PASS | `lifecycle_stage.allows_live_ingress()` gate on intent submission |
| Order expiration | PASS | 30-min default EIP-712 expiry, configurable via `PLOY_ORDER_EXPIRY_SECS` |
| Sidecar orders | PASS | `/api/sidecar/orders` only enabled when `PLOY_SIDECAR_ORDERS_LIVE_ENABLED=1` |

---

## Remediation Priority

| Priority | Finding | Effort |
|----------|---------|--------|
| 1 | H-01: Redact `ApiCredentials` Debug | 15 min |
| 2 | H-02: Remove HMAC signing message from debug log | 5 min |
| 3 | M-06: Fix emergency stop memory ordering | 5 min |
| 4 | M-02: Add API rate limiting | 1 hour |
| 5 | M-03: Add security headers | 30 min |
| 6 | M-04: Move WS auth out of query params | 2 hours |
| 7 | M-05: Redact `DatabaseConfig` Debug | 15 min |
| 8 | M-01: Remove Clone from Wallet, restrict inner() | 30 min |
| 9 | M-07: Harden sidecar risk guard | 30 min |
| 10 | L-01 through L-06 | 2 hours total |

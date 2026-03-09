# Phase 4a: Rust/Tokio/Axum Best Practices Review

**Date**: 2026-03-08
**Scope**: `src/` directory (~165K lines, 209 .rs files)
**Rust edition**: 2021 | **rustc**: 1.91.1 | **MSRV**: not declared

---

## 1. Rust Idioms

### 1.1 `ApiCredentials` derives `Debug` — secrets in logs

**Severity**: High (security)
**File**: `src/signing/hmac.rs:11-12`

```rust
#[derive(Debug, Clone)]
pub struct ApiCredentials {
    pub api_key: String,
    pub secret: String,
    pub passphrase: String,
}
```

**Problem**: `Debug` on a struct holding API key, secret, and passphrase means any
`tracing::debug!("{:?}", creds)` or panic backtrace will dump secrets to logs.

**Fix**: Remove `Debug` derive; implement `Debug` manually with redaction:
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

### 1.2 `parse_boolish` duplicated 7 times

**Severity**: Medium (DRY violation)
**Files**: `src/api/routes.rs:46`, `src/api/auth.rs:21`, `src/api/state.rs:121`,
`src/api/handlers/capabilities.rs:35`, `src/api/handlers/deployment_gate.rs:8`,
`src/api/handlers/sidecar.rs:261`, `src/adapters/polymarket_clob.rs:546`

**Problem**: Identical `parse_boolish(value: &str) -> bool` function copy-pasted
across 7 files. Any behavioral change requires 7 edits.

**Fix**: Extract to a shared utility module (e.g., `src/util.rs` or `src/config.rs`):
```rust
pub fn parse_boolish(value: &str) -> bool {
    matches!(value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "y" | "on")
}
```

### 1.3 Excessive `#[allow(dead_code)]` annotations

**Severity**: Low (code hygiene)
**Count**: 45 occurrences across 19 files

**Problem**: Widespread `#[allow(dead_code)]` suggests either premature code or
abandoned features. Dead code increases compile time and cognitive load.

**Fix**: Audit each annotation. Remove truly dead code; for intentionally-unused
fields (e.g., future use), add a doc comment explaining why.

### 1.4 `std::env::var` scattered across 252 call sites in 42 files

**Severity**: Medium (testability, configuration hygiene)
**Count**: 252 `std::env::var()` calls across 42 files

**Problem**: Environment variables read at arbitrary points throughout the codebase
make behavior hard to test and reason about. Functions like `l2_feed_enabled()`,
`sidecar_orders_live_enabled()`, and `database_url_from_env()` are scattered
across strategy, API, and adapter modules.

**Fix**: Centralize env-var reads into `AppConfig` or a dedicated `EnvConfig`
struct at startup. Pass config down via dependency injection. This makes the
system testable without `std::env::set_var` hacks (which are unsound in
multi-threaded contexts per Rust 1.66+).

### 1.5 `PloyError::Other(#[from] anyhow::Error)` catch-all variant

**Severity**: Medium (error handling)
**File**: `src/error.rs:114`

**Problem**: Having both `thiserror` (typed errors) and `anyhow` (erased errors)
in the same error enum defeats the purpose of typed errors. Any error can be
smuggled through `PloyError::Other`, making match arms unreliable.

**Fix**: Remove the `anyhow` dependency from the main error type. Convert
remaining `anyhow::Error` usage to specific `PloyError` variants. Keep `anyhow`
only in CLI/binary code where error types don't matter.

### 1.6 Glob re-exports in `api/handlers/mod.rs`

**Severity**: Low (namespace pollution)
**File**: `src/api/handlers/mod.rs`

```rust
pub use auth::*;
pub use capabilities::*;
pub use deployments::*;
// ... 9 more glob re-exports
```

**Problem**: Glob re-exports flatten the namespace, making it hard to trace where
a symbol comes from and risking name collisions as the handler count grows.

**Fix**: Use explicit re-exports for the handler functions actually used in
`routes.rs`, or qualify them at the call site.

## 2. Tokio Patterns

### 2.1 `std::sync::Mutex` used in async struct (`AutonomousAgent`)

**Severity**: High (potential deadlock)
**File**: `src/ai_clients/autonomous.rs:125`

```rust
last_grok_call: std::sync::Mutex<Option<Instant>>,
```

**Problem**: `std::sync::Mutex` in a struct used across `.await` points. If the
lock is held across an await, the thread is blocked and cannot service other
tasks. The `AutonomousAgent` is used in async contexts.

**Fix**: Replace with `tokio::sync::Mutex` since this is used in async code:
```rust
last_grok_call: tokio::sync::Mutex<Option<Instant>>,
```

### 2.2 `std::sync::RwLock` in `CoordinatorHandle` (mixed with tokio locks)

**Severity**: Medium (inconsistency, potential contention)
**File**: `src/coordinator/coordinator.rs:893`

```rust
authorized_agents: Arc<std::sync::RwLock<HashSet<String>>>,
```

**Problem**: All other fields in `CoordinatorHandle` use `tokio::sync::RwLock`,
but `authorized_agents` uses `std::sync::RwLock`. This is inconsistent and
could block the tokio runtime if contended.

**Fix**: Use `tokio::sync::RwLock` for consistency, or if the lock is only held
briefly without awaits, document why `std::sync` is intentional.

### 2.3 `std::thread::sleep` in async test code

**Severity**: Low (test quality)
**Files**: `src/platform/queue.rs:627`, `src/platform/persistence_pipeline.rs:886`,
`src/strategy/nba_comeback/nba_data_collector.rs:331`

**Problem**: `std::thread::sleep` blocks the current thread. In test code this is
mostly harmless, but in `nba_data_collector.rs:331` it's in non-test code.

**Fix**: Replace with `tokio::time::sleep` in async contexts. For sync tests,
`std::thread::sleep` is acceptable.

### 2.4 Spawned tasks with no JoinHandle tracking

**Severity**: Medium (observability, error swallowing)
**Count**: ~60 `tokio::spawn()` calls, many with handles discarded

**Problem**: Many `tokio::spawn` calls discard the `JoinHandle`, meaning panics
in spawned tasks are silently swallowed. In a trading system, a silently-dead
background task (e.g., quote persistence, order monitoring) can cause stale data
or missed exits.

**Fix**: Use `tokio::task::JoinSet` to track spawned tasks and propagate panics.
At minimum, add `.abort()` on shutdown and log panics:
```rust
let handle = tokio::spawn(async move { /* ... */ });
// In shutdown:
if let Err(e) = handle.await {
    if e.is_panic() { error!("task panicked: {e}"); }
}
```

### 2.5 No `CancellationToken` usage — shutdown via ad-hoc booleans

**Severity**: Medium (shutdown reliability)
**Pattern**: `Arc<RwLock<bool>>` for shutdown flags (e.g., `src/strategy/claimer.rs:616`,
`src/strategy/orchestrator.rs:74`)

**Problem**: Shutdown is signaled via `Arc<RwLock<bool>>` flags that must be
polled. This is less efficient and more error-prone than structured cancellation.

**Fix**: Use `tokio_util::sync::CancellationToken` (or `tokio::sync::watch`)
for cooperative cancellation. It integrates cleanly with `select!`:
```rust
tokio::select! {
    _ = token.cancelled() => break,
    msg = rx.recv() => { /* ... */ }
}
```

### 2.6 Excessive `Arc<RwLock<>>` wrapping — 60+ instances

**Severity**: Medium (complexity, contention)
**Count**: 60+ `Arc::new(RwLock::new(...))` in constructors

**Problem**: `MomentumStrategyAdapter` alone has 10 `Arc<RwLock<>>` fields.
`SplitArbEngine` has 6. This pattern suggests the struct is shared across tasks
when it may not need to be, or that interior mutability is being used as a
substitute for proper ownership design.

**Fix**: Audit whether each `Arc<RwLock<>>` is actually shared across tasks.
For single-owner fields, use plain ownership. For read-heavy/write-rare data,
consider `DashMap` (already a dependency) or `arc-swap`.

## 3. Axum Patterns

### 3.1 No typed error responses — `(StatusCode, String)` everywhere

**Severity**: High (API quality, maintainability)
**Count**: 30+ handler return types use `Result<Json<T>, (StatusCode, String)>`

**Problem**: Every handler returns `(StatusCode, String)` for errors. This means:
- No consistent error JSON shape for API consumers
- No centralized error logging
- Auth checks manually called in each handler instead of via middleware

**Fix**: Define an `ApiError` type implementing `IntoResponse`:
```rust
struct ApiError { status: StatusCode, code: &'static str, message: String }
impl IntoResponse for ApiError { /* JSON body */ }
```
Then use `Result<Json<T>, ApiError>` in handlers. Extract auth into an Axum
middleware/extractor:
```rust
struct AdminAuth; // extractor that calls ensure_admin_authorized
```

### 3.2 Auth checks are manual function calls, not middleware/extractors

**Severity**: Medium (security, DRY)
**Files**: Every handler in `src/api/handlers/` calls `ensure_admin_authorized(&headers)`
or `ensure_sidecar_authorized(&headers)` manually.

**Problem**: Easy to forget an auth check on a new endpoint. Auth logic is
duplicated across handlers.

**Fix**: Use Axum extractors:
```rust
struct AdminAuth;
#[async_trait]
impl<S> FromRequestParts<S> for AdminAuth { /* ... */ }
```
Then handlers that need admin auth simply include `_auth: AdminAuth` in their
parameter list.

### 3.3 Flat router with 40+ routes — no nesting

**Severity**: Low (organization)
**File**: `src/api/routes.rs`

**Problem**: All 40+ routes are registered in a single `create_router()` function.
As the API grows, this becomes unwieldy.

**Fix**: Use Axum's `Router::nest()` to group routes:
```rust
Router::new()
    .nest("/api/system", system_routes())
    .nest("/api/sidecar", sidecar_routes())
    .nest("/api/strategies", strategy_routes())
```

### 3.4 `AppState` is a large struct with 15 fields

**Severity**: Low (complexity)
**File**: `src/api/state.rs:19-60`

**Problem**: `AppState` carries 15 fields including DB pool, broadcast channels,
coordinator handle, Grok client, deployment maps, and file paths. This makes
it hard to test individual handlers.

**Fix**: Consider splitting into sub-states or using Axum's `FromRef` pattern
to extract only what each handler needs.

## 4. sqlx Patterns

### 4.1 Zero usage of compile-time checked queries (`sqlx::query!` / `sqlx::query_as!`)

**Severity**: High (correctness)
**Count**: 0 `sqlx::query!` or `sqlx::query_as!` calls; 297 `sqlx::query(` string calls

**Problem**: Every SQL query in the codebase uses runtime string queries
(`sqlx::query("SELECT ...")`). This means:
- No compile-time SQL validation
- No type checking of bind parameters
- Column name typos only caught at runtime
- Manual `.get("column")` calls on `Row` are error-prone

**Fix**: Migrate critical queries to `sqlx::query!` / `sqlx::query_as!` macros.
This requires `DATABASE_URL` at compile time (or `sqlx prepare` for offline mode).
Start with the most critical paths: order execution, position tracking, PnL.

### 4.2 DDL in application code (`CREATE TABLE IF NOT EXISTS`)

**Severity**: Medium (migration hygiene)
**Files**: `src/platform/persistence_schema.rs`, `src/collector/polymarket_orderbook_history.rs`,
`src/collector/token_targets.rs`, `src/collector/sync_collector.rs`,
`src/main_modes/deribit_iv_backfill_mode.rs`

**Problem**: Multiple modules execute `CREATE TABLE IF NOT EXISTS` at runtime,
bypassing the migration system (`./migrations/`). This creates:
- Schema drift between environments
- No rollback path
- Race conditions if multiple processes start simultaneously

**Fix**: Move all DDL into numbered migration files. Use `sqlx::migrate!()` as
the single schema management path.

### 4.3 Manual `Row::get()` instead of typed deserialization

**Severity**: Medium (type safety)
**File**: `src/adapters/postgres.rs` and throughout

```rust
let row = sqlx::query("SELECT id, slug, ...").fetch_one(&self.pool).await?;
Ok(row.get("id"))
```

**Problem**: `Row::get("column_name")` panics if the column doesn't exist or
the type doesn't match. Combined with string queries, this is doubly fragile.

**Fix**: Use `sqlx::query_as::<_, MyStruct>()` with `#[derive(sqlx::FromRow)]`
on domain types, or at minimum use `sqlx::query_scalar` for single-value queries.

## 5. Deprecated APIs and Modernization

### 5.1 `async-trait` crate still used (40 occurrences) — native async traits available

**Severity**: Medium (modernization)
**Count**: 40 `#[async_trait]` annotations across 33 files
**Rust version**: 1.91.1 (native async fn in traits stable since 1.75)

**Problem**: The `async-trait` crate was necessary before Rust 1.75. It adds
heap allocation (Box) for every async trait method call. With Rust 1.91.1,
native `async fn` in traits is available and avoids the boxing overhead.

**Caveat**: Native async traits don't support `dyn Trait` (object safety) without
`trait_variant` or manual boxing. Traits used as `dyn` (like `ExchangeClient`,
`Strategy`, `TradingAgent`) would need `#[trait_variant::make(SendStrategy: Send)]`
or keep `async-trait` selectively.

**Fix**: Audit each `#[async_trait]` usage:
- Traits only used with generics (not `dyn`): remove `async-trait`, use native syntax
- Traits used as `dyn`: keep `async-trait` or migrate to `trait_variant`

### 5.2 `thiserror` v1 — v2 available

**Severity**: Low (dependency freshness)
**File**: `Cargo.toml` — `thiserror = "1"`

**Problem**: `thiserror` v2 was released with improved diagnostics and
`#[error(transparent)]` improvements. v1 still works fine but is in maintenance mode.

**Fix**: Bump to `thiserror = "2"`. The migration is mostly mechanical.

### 5.3 `rand` v0.8 — v0.9 available

**Severity**: Low (dependency freshness)
**File**: `Cargo.toml` — `rand = "0.8"`

**Problem**: `rand` 0.9 was released with API improvements. 0.8 is still
maintained but won't receive new features.

**Fix**: Bump when convenient; API changes are minor.

### 5.4 Edition 2021 — Edition 2024 available

**Severity**: Low (modernization)
**File**: `Cargo.toml` — `edition = "2021"`

**Problem**: Rust edition 2024 is available (stable since Rust 1.85) with
improved `impl Trait` capture rules, `unsafe_op_in_unsafe_fn` lint, and
`gen` blocks.

**Fix**: Run `cargo fix --edition` and bump to `edition = "2024"` when ready.
Test thoroughly as some lint defaults change.

## 6. Dependency Management

### 6.1 Both `futures` and `futures-util` declared

**Severity**: Low (unnecessary dependency)
**File**: `Cargo.toml`

```toml
futures-util = "0.3"
futures = "0.3"
```

**Problem**: `futures` re-exports everything from `futures-util`. Having both
is redundant. Most code only needs `futures-util` (for `StreamExt`, `SinkExt`).

**Fix**: Remove `futures = "0.3"` and use `futures-util` directly. If
`futures::future::join_all` is needed, it's in `futures-util` too.

### 6.2 Vendored `polymarket-client-sdk` via `[patch.crates-io]`

**Severity**: Medium (maintenance burden)
**File**: `Cargo.toml`

```toml
[patch.crates-io]
polymarket-client-sdk = { path = "vendor/polymarket-client-sdk" }
```

**Problem**: Vendored dependency means upstream fixes and security patches
must be manually merged. The patch section also affects all workspace members.

**Fix**: If the vendor patches are upstreamable, submit PRs. If not, document
the delta in `vendor/polymarket-client-sdk/PATCHES.md` and set up a periodic
sync process.

### 6.3 No `[workspace.dependencies]` for shared versions

**Severity**: Low (workspace hygiene)
**File**: `Cargo.toml`

**Problem**: The workspace has 2 members (`.` and `tools/sdk_auth_check`) but
doesn't use `[workspace.dependencies]` to share dependency versions.

**Fix**: Move shared dependencies to `[workspace.dependencies]` and reference
them with `dep.workspace = true` in member crates.

### 6.4 `bincode` pinned to release candidate

**Severity**: Medium (stability risk)
**File**: `Cargo.toml` — `bincode = { version = "=2.0.0-rc.3" }`

**Problem**: Pinned to a release candidate. RC versions may have breaking
changes before final release, and the exact pin prevents security patches.

**Fix**: Monitor bincode 2.0 stable release. If burn compatibility allows,
upgrade when stable lands.

## 7. Build Configuration

### 7.1 No MSRV declared

**Severity**: Low (CI/reproducibility)
**File**: `Cargo.toml`

**Problem**: No `rust-version` field in `[package]`. Contributors may use
different Rust versions, leading to "works on my machine" issues.

**Fix**: Add `rust-version = "1.85"` (or whatever the actual minimum is) to
`Cargo.toml`. This enables `cargo` to warn when the toolchain is too old.

### 7.2 Feature flag proliferation (17 features)

**Severity**: Low (complexity)
**File**: `Cargo.toml` — 17 feature flags

**Problem**: 17 feature flags create a combinatorial testing surface. Some
features have dependency chains (`claimer` -> `claimer_cli` -> `claimer_daemon`)
that are non-obvious.

**Fix**: Document feature flags in a table. Consider consolidating related
features (e.g., `claimer` variants). Test the most common feature combinations
in CI.

### 7.3 Release profile uses `panic = "abort"` — no unwinding

**Severity**: Medium (operational)
**File**: `Cargo.toml`

```toml
[profile.release]
panic = "abort"
```

**Problem**: `panic = "abort"` means any panic immediately kills the process
with no cleanup. For a trading system, this means:
- No `Drop` destructors run (open orders may not be cancelled)
- No graceful shutdown on panic
- Harder to debug production crashes

**Fix**: Consider `panic = "unwind"` for release builds, or ensure all critical
cleanup is in signal handlers rather than `Drop`. At minimum, use
`std::panic::set_hook` to log panics before abort.

---

## Summary Table

| # | Finding | Severity | Category |
|---|---------|----------|----------|
| 1.1 | `ApiCredentials` derives `Debug` — secrets in logs | High | Security |
| 1.2 | `parse_boolish` duplicated 7 times | Medium | DRY |
| 1.3 | 45 `#[allow(dead_code)]` annotations | Low | Hygiene |
| 1.4 | 252 `std::env::var` calls scattered across 42 files | Medium | Testability |
| 1.5 | `PloyError::Other(anyhow::Error)` catch-all | Medium | Error handling |
| 1.6 | Glob re-exports in handlers | Low | Namespace |
| 2.1 | `std::sync::Mutex` in async `AutonomousAgent` | High | Deadlock risk |
| 2.2 | `std::sync::RwLock` mixed with tokio locks in Coordinator | Medium | Consistency |
| 2.3 | `std::thread::sleep` in async code | Low | Correctness |
| 2.4 | Spawned tasks with no JoinHandle tracking | Medium | Observability |
| 2.5 | No `CancellationToken` — shutdown via ad-hoc booleans | Medium | Reliability |
| 2.6 | 60+ `Arc<RwLock<>>` wrappings | Medium | Complexity |
| 3.1 | No typed error responses — `(StatusCode, String)` | High | API quality |
| 3.2 | Auth checks manual, not middleware/extractors | Medium | Security/DRY |
| 3.3 | Flat router with 40+ routes | Low | Organization |
| 3.4 | `AppState` with 15 fields | Low | Complexity |
| 4.1 | Zero compile-time checked SQL queries | High | Correctness |
| 4.2 | DDL in application code bypassing migrations | Medium | Schema hygiene |
| 4.3 | Manual `Row::get()` instead of typed deserialization | Medium | Type safety |
| 5.1 | `async-trait` still used (native available since 1.75) | Medium | Modernization |
| 5.2 | `thiserror` v1 (v2 available) | Low | Freshness |
| 5.3 | `rand` v0.8 (v0.9 available) | Low | Freshness |
| 5.4 | Edition 2021 (2024 available) | Low | Modernization |
| 6.1 | Both `futures` and `futures-util` declared | Low | Redundancy |
| 6.2 | Vendored `polymarket-client-sdk` | Medium | Maintenance |
| 6.3 | No `[workspace.dependencies]` | Low | Workspace hygiene |
| 6.4 | `bincode` pinned to RC | Medium | Stability |
| 7.1 | No MSRV declared | Low | Reproducibility |
| 7.2 | 17 feature flags, complex dependency chains | Low | Complexity |
| 7.3 | `panic = "abort"` in release — no cleanup on panic | Medium | Operational |

**Critical**: 0 | **High**: 4 | **Medium**: 14 | **Low**: 12

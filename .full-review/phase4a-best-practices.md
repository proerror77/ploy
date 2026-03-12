# Phase 4a — Rust/Tokio/Axum Best Practices Review

**Branch**: `hotfix/staggered-arb-release-20260306` vs `main`
**Date**: 2026-03-11
**Reviewer**: Claude Code (claude-sonnet-4-6)

---

## Summary

| Severity | Count |
|----------|-------|
| Critical | 1 |
| High     | 4 |
| Medium   | 5 |
| Low      | 4 |
| **Total**| **14** |

---

## Critical Findings

### C1 — `std::sync::RwLock` in async context (lock poisoning + blocking)

**File**: `src/coordinator/coordinator.rs:69,80,93,119,128`

**Current pattern**:
```rust
// coordinator.rs:69
authorized_agents: Arc<std::sync::RwLock<HashSet<String>>>,
// coordinator.rs:93
order_update_sinks: Arc<std::sync::RwLock<HashMap<String, mpsc::Sender<crate::strategy::OrderUpdate>>>>,
```

**Problem**: `std::sync::RwLock` is a blocking primitive. When held across an `.await` point it blocks the Tokio worker thread, starving other tasks. Even when not held across `.await`, acquiring a `std::sync::RwLock` inside an async context can block the thread if another holder is slow. Additionally, `std::sync::RwLock` is susceptible to lock poisoning — if a thread panics while holding the lock, all subsequent `read()`/`write()` calls return `Err`, which is why the code defensively uses `if let Ok(mut sinks) = self.order_update_sinks.write()` (order_updates.rs:15) and `if let Ok(mut authorized) = self.authorized_agents.write()` (coordinator.rs:203). Silently swallowing a poisoned-lock error means a registration or update is silently dropped.

**Recommended fix**: Replace both fields with `tokio::sync::RwLock`. The `emit_order_update` path already awaits the send (`tx.send(update).await`), so the guard must not be held across that point — `tokio::sync::RwLock` enforces this at compile time.

```rust
// coordinator.rs — change field types
authorized_agents: Arc<tokio::sync::RwLock<HashSet<String>>>,
order_update_sinks: Arc<tokio::sync::RwLock<HashMap<String, mpsc::Sender<crate::strategy::OrderUpdate>>>>,

// order_updates.rs — register_order_updates becomes async, or use try_write()
pub async fn register_order_updates(&mut self, agent_id: String) -> mpsc::Receiver<OrderUpdate> {
    let (tx, rx) = mpsc::channel(128);
    self.order_update_sinks.write().await.insert(agent_id, tx);
    rx
}

// emit_order_update — guard is dropped before .await
async fn emit_order_update(&self, agent_id: &str, update: OrderUpdate) {
    let tx = self.order_update_sinks.read().await.get(agent_id).cloned();
    let Some(tx) = tx else { return; };
    if tx.send(update).await.is_err() {
        warn!(agent_id, "strategy order update channel closed");
    }
}
```

This was flagged as P-L4/L5 in prior phases; it remains unresolved.

---

## High Findings

### H1 — TOCTOU read-then-write on `state` in `record_success`

**File**: `src/coordinator/risk/transitions.rs:68-71`

**Current pattern**:
```rust
if *self.state.read().await == PlatformRiskState::Elevated {
    *self.state.write().await = PlatformRiskState::Normal;
    info!("Risk state normalized after successful execution");
}
```

**Problem**: The read lock is released before the write lock is acquired. Between the two lock acquisitions another task could change `state` (e.g., `trigger_circuit_breaker` could set it to `Halted`). The write then unconditionally overwrites `Halted` with `Normal`, silently re-enabling trading after a circuit-breaker trip. This is the H5 TOCTOU finding from prior phases — it remains unresolved.

**Recommended fix**: Use a single write lock and check-then-set atomically:
```rust
{
    let mut state = self.state.write().await;
    if *state == PlatformRiskState::Elevated {
        *state = PlatformRiskState::Normal;
        info!("Risk state normalized after successful execution");
    }
}
```

---

### H2 — Sequential `persist_risk_decision` DB writes block the coordinator loop

**File**: `src/coordinator/coordinator/ingress.rs` (14 call sites, lines 15, 28, 60, 79, 96, 110, 123, 145, 163, 180, 196, 214, 235, 261, 286, 323)

**Current pattern**:
```rust
self.journal
    .persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
    .await;
```

**Problem**: Every call to `handle_order_intent` can make up to 14 sequential `.await` DB writes before returning. The coordinator's main `select!` loop is single-threaded — each `handle_order_intent` call holds the loop hostage for the duration of all these DB round-trips. Under load (e.g., burst of intents) this creates head-of-line blocking: queue drain ticks, state refresh ticks, and shutdown signals are all delayed. This is the P-C2 finding from prior phases — it remains unresolved.

**Recommended fix**: Fire-and-forget each journal write with `tokio::spawn`. The journal already handles errors internally (logs a warning), so there is no caller-visible error to propagate:
```rust
// Instead of:
self.journal.persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None).await;

// Use:
let journal = self.journal.clone();
let intent_snap = intent.clone();
let reason_snap = reason.clone();
tokio::spawn(async move {
    journal.persist_risk_decision(&intent_snap, "BLOCKED", Some(reason_snap), None).await;
});
```

Alternatively, introduce a dedicated journaling channel (bounded `mpsc`) and a background writer task, which provides back-pressure and avoids unbounded task spawning.

---

### H3 — Nested write locks in `close_position` (lock-ordering deadlock risk)

**File**: `src/coordinator/position/transitions.rs:52-72`

**Current pattern**:
```rust
pub async fn close_position(&self, position_id: &str, exit_price: Decimal) -> Option<Decimal> {
    let mut positions = self.positions.write().await;  // lock 1 acquired

    if let Some(position) = positions.remove(position_id) {
        let pnl = ...;
        let mut realized = self.realized_pnl.write().await;  // lock 2 acquired while lock 1 held
        *realized.entry(position.agent_id.clone()).or_insert(Decimal::ZERO) += pnl;
        ...
    }
}
```

**Problem**: `positions` write lock is held while acquiring `realized_pnl` write lock. If any other code path acquires these locks in the opposite order (realized_pnl first, then positions), a deadlock occurs. The `reduce_position` method (lines 86-116) correctly drops `positions` before acquiring `realized_pnl` — `close_position` should follow the same pattern. This is the C1 lock-ordering finding from prior phases — it remains unresolved in `close_position`.

**Recommended fix**:
```rust
pub async fn close_position(&self, position_id: &str, exit_price: Decimal) -> Option<Decimal> {
    let (agent_id, pnl) = {
        let mut positions = self.positions.write().await;
        let position = positions.remove(position_id)?;
        let pnl = (exit_price - position.entry_price) * Decimal::from(position.shares);
        (position.agent_id, pnl)
    }; // positions lock dropped here

    let mut realized = self.realized_pnl.write().await;
    *realized.entry(agent_id.clone()).or_insert(Decimal::ZERO) += pnl;
    ...
    Some(pnl)
}
```

---

### H4 — Unbounded poll-task spawning without dedup or handle tracking

**File**: `src/coordinator/strategy_runtime/actions.rs:588`

**Current pattern**:
```rust
tokio::spawn(async move {
    let started_at = std::time::Instant::now();
    while started_at.elapsed().as_millis() < poll_max_ms as u128 {
        tokio::time::sleep(Duration::from_millis(poll_interval_ms)).await;
        // polls order status for up to poll_max_ms (default 600s)
        ...
    }
});
```

**Problem**: Every `OrderUpdate` received by `handle_runtime_order_update` spawns a new long-lived polling task (up to 600 seconds). There is no deduplication — if the same order receives multiple updates before the first poll task finishes, multiple concurrent poll tasks run for the same order. There is no `JoinHandle` stored, so these tasks cannot be cancelled on strategy shutdown. Under a burst of order updates this creates an unbounded number of background tasks. This is the P-H3 finding from prior phases — it remains unresolved.

**Recommended fix**: Track poll tasks in a `DashMap<String, AbortHandle>` keyed by `exchange_order_id`. Before spawning, abort any existing task for the same order:
```rust
// In ManagedRuntimeSession or the actions module:
poll_tasks: Arc<DashMap<String, tokio::task::AbortHandle>>,

// Before spawning:
if let Some(old) = poll_tasks.remove(&exchange_order_id) {
    old.1.abort();
}
let handle = tokio::spawn(async move { ... });
poll_tasks.insert(exchange_order_id, handle.abort_handle());
```

---

## Medium Findings

### M1 — `Ordering::SeqCst` used where `Relaxed` or `AcqRel` suffices

**File**: `src/coordinator/risk/transitions.rs:15,76,187,333`; `src/coordinator/risk/queries.rs:76`; `src/coordinator/risk.rs:88`

**Current pattern**:
```rust
self.consecutive_failures.store(0, Ordering::SeqCst);
let global_failures = self.consecutive_failures.fetch_add(1, Ordering::SeqCst) + 1;
```

**Problem**: `Ordering::SeqCst` is the strongest (and most expensive) memory ordering. It imposes a total global order on all `SeqCst` operations across all threads, which requires a full memory fence on x86 and is significantly more expensive on ARM. The `consecutive_failures` counter is only ever read/written within `RiskGate` methods that are already serialized by the surrounding `tokio::sync::RwLock` guards on `agent_stats` and `daily_stats`. The atomic counter itself does not need cross-thread ordering guarantees beyond what the surrounding locks already provide.

**Recommended fix**: Use `Ordering::Relaxed` for the counter since the surrounding async locks provide the necessary happens-before relationships:
```rust
self.consecutive_failures.store(0, Ordering::Relaxed);
let global_failures = self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
```

If the counter is ever read without a surrounding lock (e.g., in a health-check path), `Ordering::Acquire`/`Release` is sufficient.

---

### M2 — `async_trait` macro used where native async traits suffice (Rust 1.75+)

**File**: `src/coordinator/strategy_runtime/order_store.rs:1,9,27`

**Current pattern**:
```rust
use async_trait::async_trait;

#[async_trait]
pub(crate) trait RuntimeOrderStore: Send + Sync {
    async fn insert_order(&self, order: &crate::domain::Order) -> Result<()>;
    ...
}
```

**Problem**: The `async_trait` proc-macro crate was necessary before Rust 1.75 (stable December 2023). Since Rust 1.75, `async fn` in traits is natively supported. The macro adds compile-time overhead, generates `Box<dyn Future>` allocations at runtime, and produces less readable error messages. The project uses `edition = "2021"` and targets stable Rust, so native async traits are available.

**Recommended fix**: Remove the `async_trait` dependency from this module and use native syntax:
```rust
// No use async_trait needed
pub(crate) trait RuntimeOrderStore: Send + Sync {
    async fn insert_order(&self, order: &crate::domain::Order) -> Result<()>;
    async fn update_order_status(...) -> Result<()>;
    async fn update_order_fill(...) -> Result<()>;
}

impl RuntimeOrderStore for PostgresStore {
    async fn insert_order(&self, order: &crate::domain::Order) -> Result<()> { ... }
    ...
}
```

Check whether `async_trait` is used elsewhere in the codebase before removing the Cargo.toml dependency.

---

### M3 — `BinaryHeap` O(n) rebuild for every filtered mutation

**File**: `src/coordinator/queue.rs:129,211,235,248`

**Current pattern**:
```rust
fn pop_lowest_priority(&mut self) -> Option<PrioritizedIntent> {
    let mut items: Vec<PrioritizedIntent> = std::mem::take(&mut self.heap).into_vec();
    // O(n) linear scan + swap_remove + O(n log n) rebuild
    ...
    self.heap = BinaryHeap::from(items);
    Some(dropped)
}

pub fn cleanup_expired_intents(&mut self) -> Vec<OrderIntent> {
    let items: Vec<_> = std::mem::take(&mut self.heap).into_vec();
    // O(n) scan, then O(n log n) rebuild
    for item in items { ... self.heap.push(item); }
    ...
}
```

**Problem**: `pop_lowest_priority`, `cleanup_expired_intents`, `remove_agent_orders`, and `remove_buy_orders` all drain the heap into a `Vec`, mutate it, and rebuild the heap. Each rebuild is O(n log n). These operations are called from the coordinator's hot path (drain tick, ingress). For a queue of 1024 items this is acceptable, but the pattern is fragile — adding a new filtered-removal operation requires the same O(n) rebuild pattern.

**Recommended fix**: For the current max queue size of 1024 this is not a performance emergency, but document the O(n log n) complexity with a comment. If the queue grows, consider a `BTreeMap<(priority, sequence), OrderIntent>` which supports O(log n) min/max removal and O(log n) filtered iteration without full rebuilds.

---

### M4 — `sqlx::query` runtime strings instead of `sqlx::query!` compile-time macros

**File**: `src/coordinator/journal.rs:94,211,276,336,392,463,561,704`; `src/coordinator/governance.rs:314,349`; `src/coordinator/strategy_runtime/observability.rs:80,129`

**Current pattern**:
```rust
let result = sqlx::query(
    r#"INSERT INTO risk_gate_decisions (...) VALUES ($1,$2,...)"#,
)
.bind(&self.account_id)
.bind(intent.intent_id)
...
.execute(pool)
.await;
```

**Problem**: `sqlx::query(...)` is a runtime query — the SQL is not validated against the database schema at compile time. Type mismatches between Rust types and PostgreSQL column types, wrong parameter counts, and invalid column names are only discovered at runtime. The `sqlx::query!` macro (or `sqlx::query_as!`) validates SQL at compile time when `DATABASE_URL` is set, catching these errors before deployment.

**Recommended fix**: Migrate journal and governance queries to `sqlx::query!` macros. This requires `DATABASE_URL` to be set during `cargo build` (already needed for the `api` feature). For queries that return rows, use `sqlx::query_as!` with typed structs. The `macros` feature is already enabled in `Cargo.toml` (`sqlx = { ..., features = [..., "macros", ...] }`).

Note: This is a significant migration effort. Prioritize the most critical paths (risk decisions, execution journal) first.

---

### M5 — `pop_lowest_priority` eviction uses `swap_remove` which invalidates heap invariant silently

**File**: `src/coordinator/queue.rs:128-149`

**Current pattern**:
```rust
fn pop_lowest_priority(&mut self) -> Option<PrioritizedIntent> {
    let mut items: Vec<PrioritizedIntent> = std::mem::take(&mut self.heap).into_vec();
    ...
    let dropped = items.swap_remove(lowest_idx);
    self.heap = BinaryHeap::from(items);  // O(n) heapify
    Some(dropped)
}
```

**Problem**: `swap_remove` is correct here because the heap is immediately rebuilt via `BinaryHeap::from(items)`. However, the pattern is subtle — a future maintainer might remove the `BinaryHeap::from` rebuild and use `self.heap.push` in a loop, silently breaking heap ordering. The intent is not obvious from the code.

**Recommended fix**: Add a comment explaining why `swap_remove` is safe here, and consider extracting the rebuild into a named helper:
```rust
// swap_remove is safe: we immediately rebuild the heap from the remaining items
let dropped = items.swap_remove(lowest_idx);
self.heap = BinaryHeap::from(items); // O(n) heapify — must follow swap_remove
```

---

## Low Findings

### L1 — `events.drain(0..drain)` is O(n) shift; use `VecDeque` or rotate

**File**: `src/coordinator/risk/transitions.rs:353-356`

**Current pattern**:
```rust
let mut events = self.circuit_events.write().await;
events.push(super::CircuitBreakerEvent { ... });
if events.len() > 100 {
    let drain = events.len() - 100;
    events.drain(0..drain);
}
```

**Problem**: `Vec::drain(0..n)` removes elements from the front, which requires shifting all remaining elements left — O(n). For a capped ring buffer of 100 events this is negligible, but `VecDeque` provides O(1) front removal.

**Recommended fix**: Change `circuit_events` to `VecDeque<CircuitBreakerEvent>` and use `pop_front()`:
```rust
while events.len() > 100 {
    events.pop_front();
}
```

---

### L2 — `rand = "0.8"` is two major versions behind current (0.9.x)

**File**: `Cargo.toml:83`

**Current pattern**:
```toml
rand = "0.8"
```

**Problem**: `rand` 0.8 was released in 2020. The current stable version is 0.9.x (released 2025). The 0.8 → 0.9 migration includes API improvements (`thread_rng()` replaced by `rng()`, `SliceRandom` trait changes). Staying on 0.8 means missing security patches and bug fixes. The codebase uses `rand::thread_rng()` and `rand::random::<T>()` which have direct equivalents in 0.9.

**Recommended fix**: Bump to `rand = "0.9"` and update call sites:
- `rand::thread_rng()` → `rand::rng()`
- `rand::random::<T>()` → `rand::random::<T>()` (unchanged in 0.9)
- `use rand::Rng` trait import remains the same

---

### L3 — `ethers-core`/`ethers-signers` v2 are deprecated in favor of `alloy`

**File**: `Cargo.toml:60-61`

**Current pattern**:
```toml
ethers-core = { version = "2", default-features = false, optional = true }
ethers-signers = { version = "2", default-features = false, optional = true }
```

**Problem**: The `ethers-rs` project is officially deprecated and unmaintained as of 2024. The maintainers have migrated to `alloy-rs`. The project already depends on `alloy = { version = "1", ... }` for signing. Keeping `ethers-core`/`ethers-signers` as optional dependencies (behind `claimer_daemon` feature) means maintaining two signing stacks.

**Recommended fix**: Migrate the `claimer_daemon` feature to use `alloy` signing primitives. This eliminates the `ethers-*` dependencies entirely. The `alloy` crate already provides `signer-local` and `sol-types` features which cover the claimer's needs.

---

### L4 — `#[allow(clippy::too_many_arguments)]` suppresses a legitimate design signal

**File**: `src/coordinator/strategy_runtime/actions.rs:173,274`

**Current pattern**:
```rust
#[allow(clippy::too_many_arguments)]
async fn handle_submit_intent(
    strategy_label: &str,
    agent_id: &str,
    strategy_id: &str,
    manager: &Arc<StrategyManager>,
    coordinator_handle: &CoordinatorHandle,
    runtime_order_store: Option<&Arc<dyn RuntimeOrderStore>>,
    paused: &AtomicBool,
    orders_submitted: &AtomicU64,
    observability_pool: Option<&PgPool>,
    observability_account_id: &str,
    split_arb_managed: bool,
    intent: crate::strategy::traits::StrategyOrderIntent,
) {
```

**Problem**: 13 parameters is a strong signal that the function is doing too much or that related parameters should be grouped into a context struct. The `#[allow]` suppresses the lint rather than addressing the underlying design issue. Functions with many parameters are harder to call correctly, harder to test, and harder to extend.

**Recommended fix**: Introduce a `RuntimeActionContext` struct grouping the stable parameters:
```rust
struct RuntimeActionContext<'a> {
    strategy_label: &'a str,
    agent_id: &'a str,
    strategy_id: &'a str,
    manager: &'a Arc<StrategyManager>,
    coordinator_handle: &'a CoordinatorHandle,
    runtime_order_store: Option<&'a Arc<dyn RuntimeOrderStore>>,
    paused: &'a AtomicBool,
    orders_submitted: &'a AtomicU64,
    observability_pool: Option<&'a PgPool>,
    observability_account_id: &'a str,
    split_arb_managed: bool,
}
```

This also makes `handle_strategy_actions_runtime` cleaner since the context can be constructed once and passed to all handlers.

---

## Dependency Hygiene Notes

From `Cargo.toml`:

| Dependency | Version | Note |
|---|---|---|
| `rand` | 0.8 | Two major versions behind; upgrade to 0.9 (L2) |
| `ethers-core` / `ethers-signers` | 2 | Deprecated; migrate to `alloy` (L3) |
| `async-trait` | 0.1 | Superseded by native async traits in Rust 1.75+ (M2) |
| `bincode` | =2.0.0-rc.3 | Pinned to a release candidate; track stable 2.0 release |
| `tokio-tungstenite` | 0.28 | Current; no action needed |
| `sqlx` | 0.8.6 | Current; no action needed |
| `axum` | 0.8 | Current; no action needed |
| `alloy` | 1 | Current; no action needed |

The `bincode = "=2.0.0-rc.3"` pin is a known workaround for `burn` compatibility. Track the `burn` 0.15 release which should lift this constraint.

---

## Prior Phase Findings Status

| ID | Finding | Status |
|----|---------|--------|
| C1 | `close_position` nested write locks | **Still present** (H3 above) |
| H5 | `record_success` TOCTOU read-then-write | **Still present** (H1 above) |
| P-C2 | Sequential `persist_risk_decision` DB writes | **Still present** (H2 above) |
| P-H3 | Unbounded poll task spawning | **Still present** (H4 above) |
| P-L4/L5 | `std::sync::RwLock` in async context | **Still present** (C1 above) |

All five prior-phase findings remain unresolved in this branch.

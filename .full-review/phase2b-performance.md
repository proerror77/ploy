# Phase 2b — Performance & Scalability Analysis

Branch: `hotfix/staggered-arb-release-20260306` vs `main`
Date: 2026-03-11
Scope: Coordinator decomposition, staggered arb live strategy, CLI foreground routing

---

## Executive Summary

The coordinator decomposition is architecturally sound but introduces a **serialized hot path** with 8–12 sequential async lock acquisitions and up to 4 synchronous DB writes before an order reaches the queue. The staggered arb live strategy is event-driven and well-structured, but inherits the pre-existing `spawn_split_arb_poll_task` unbounded-task problem (H4). The single-threaded `tokio::select!` coordinator loop means all of these costs are paid serially — there is no pipelining.

---

## Critical Issues

### C1 — Serialized Coordinator Loop: No Pipelining of Intent Processing

**Severity**: Critical
**Impact**: Every order intent blocks the entire coordinator loop. At 10 intents/sec with 50ms average DB latency per intent, the coordinator saturates at ~20 intents/sec throughput. During a market event burst (staggered arb fires on multiple symbols simultaneously), intents queue up in the 256-slot channel and experience compounding latency.

**Location**: `src/coordinator/coordinator.rs:237–274` (main `run()` loop)

The `tokio::select!` loop processes one branch at a time. `handle_order_intent` is called inline and awaits multiple DB writes before returning. This means:
- While one intent is being risk-checked and journaled, no other intents are dequeued from `order_rx`
- The drain tick cannot fire while an intent is being processed
- A single slow DB write (e.g., `persist_risk_decision`) stalls all subsequent intents

**Recommendation**: Spawn `handle_order_intent` as a task rather than awaiting it inline. The coordinator loop becomes a dispatcher; the intent processing pipeline runs concurrently. Requires making the shared state `Arc`-cloneable (already done) and ensuring the queue write is the only serialization point.

```rust
Some(intent) = self.order_rx.recv() => {
    let ctx = self.clone_processing_context();
    tokio::spawn(async move { ctx.handle_order_intent(intent).await; });
}
```

---

### C2 — 8+ Sequential DB Writes on Every Blocked Intent

**Severity**: Critical
**Impact**: Every blocked intent (domain check, deployment gate, governance, risk gate) triggers a synchronous `persist_risk_decision` DB write before returning. On a busy system with many rejected intents (e.g., during a governance pause), this creates a DB write storm that blocks the coordinator loop.

**Location**: `src/coordinator/coordinator/ingress.rs:15,28,60,79,96,110,123,145,163,180,196,214,235,261,286,323`

There are **18 call sites** to `persist_risk_decision` in `handle_order_intent` alone. Each is awaited inline. For a passed intent, the sequence is:
1. `persist_risk_decision` (domain block check)
2. `persist_risk_decision` (deployment identity check)
3. `persist_risk_decision` (sell reduce-only check)
4. `persist_risk_decision` (ingress mode check)
5. `persist_risk_decision` (domain mode check)
6. `persist_risk_decision` (agent pause check)
7. `persist_risk_decision` (governance policy check)
8. `persist_risk_decision` (deployment gate)
9. `persist_signal_from_intent` (signal history)
10. `persist_exit_reason_intent` (for sells)
11. `persist_risk_decision` (duplicate check)
12. `persist_risk_decision` (kelly sizing)
13. `persist_risk_decision` (min order constraints)
14. `persist_risk_decision` (PASSED — final)

That is up to **14 sequential DB round-trips** on the hot path for a single passing intent.

**Recommendation**: Fire-and-forget all journal writes using `tokio::spawn`. Journal writes are observability data — they must not block order processing. The journal already handles errors gracefully with `warn!` and no propagation.

```rust
// Instead of:
self.journal.persist_risk_decision(&intent, "BLOCKED", ...).await;
// Use:
let j = self.journal.clone();
let i = intent.clone();
tokio::spawn(async move { j.persist_risk_decision(&i, "BLOCKED", ...).await; });
```

This requires `ExecutionJournal` to be `Clone` (it holds `Option<PgPool>` which is already `Clone`).

---

## High Issues

### H1 — `pop_lowest_priority` Rebuilds Entire Heap O(n) (Pre-existing, Confirmed Present)

**Severity**: High
**Impact**: When the queue is full (1024 items) and a high-priority intent arrives, `pop_lowest_priority` drains the entire `BinaryHeap` into a `Vec`, scans it linearly, removes one item, then rebuilds the heap from the Vec — O(n log n). Under sustained load with a full queue, this fires on every high-priority intent.

**Location**: `src/coordinator/queue.rs:128–148`

```rust
fn pop_lowest_priority(&mut self) -> Option<PrioritizedIntent> {
    let mut items: Vec<PrioritizedIntent> = std::mem::take(&mut self.heap).into_vec();
    // ... linear scan ...
    self.heap = BinaryHeap::from(items); // O(n) heapify
    Some(dropped)
}
```

The `enqueue_with_eviction` path also does a preliminary O(n) scan via `.iter().map(...).max()` before calling `pop_lowest_priority`, making the total cost O(n) + O(n log n) per eviction.

**Recommendation**: Maintain a secondary min-heap (or a `BTreeMap<priority, VecDeque<sequence>>`) for O(log n) eviction. Alternatively, since the queue max is 1024 and eviction is rare, the current approach is acceptable if queue-full events are infrequent. Add a metric counter for eviction events to monitor frequency.

---

### H2 — `cleanup_expired_intents` Rebuilds Heap on Every Drain Tick

**Severity**: High
**Impact**: Called on every `drain_and_execute` tick (default 1000ms). Even with zero expired intents, it drains the entire heap into a Vec, iterates all items, then re-pushes each one individually — O(n log n) per tick.

**Location**: `src/coordinator/queue.rs:205–229`

```rust
pub fn cleanup_expired_intents(&mut self) -> Vec<OrderIntent> {
    let items: Vec<_> = std::mem::take(&mut self.heap).into_vec();
    for item in items {
        // ... check expiry, re-push non-expired ...
        self.heap.push(item); // O(log n) each
    }
    expired
}
```

With a queue of 1024 items and a 1-second drain interval, this is 1024 comparisons + 1024 heap insertions every second, even when nothing is expired.

**Recommendation**: Skip `cleanup_expired_intents` when the queue is empty. For non-empty queues, only run it every N ticks (e.g., every 10 seconds) since `dequeue()` already skips expired items lazily. The expired items will be naturally discarded on dequeue.

```rust
// In drain_and_execute:
let (expired, batch) = {
    let mut queue = self.order_queue.write().await;
    let expired = if queue.len() > 0 && should_run_cleanup {
        queue.cleanup_expired_intents()
    } else {
        vec![]
    };
    let batch = queue.dequeue_batch(self.config.batch_size);
    (expired, batch)
};
```

---

### H3 — `spawn_split_arb_poll_task` Spawns Unbounded Tasks (Pre-existing, Confirmed Present)

**Severity**: High
**Impact**: Every `Submitted` or `PartiallyFilled` coordinator update for a split-arb order spawns a new polling task. If the WS reconnects and re-delivers the same update, or if partial fills arrive in rapid succession, multiple tasks poll the same order concurrently. Each task holds a `PgPool` clone and runs for up to `PLOY_MANAGED_STRATEGY_ORDER_POLL_MAX_MS` (default 600 seconds = 10 minutes).

**Location**: `src/coordinator/strategy_runtime/actions.rs:337–355, 565–677`

```rust
if split_arb_managed
    && exchange_order_id.is_some()
    && matches!(update.status, OrderStatus::Submitted | OrderStatus::PartiallyFilled)
{
    spawn_split_arb_poll_task(...); // no dedup, no cancellation
}
```

With staggered arb now live, this is more likely to fire. A 5-symbol deployment with 2 legs each = 10 concurrent poll tasks minimum, each polling every 1.5 seconds = ~7 API calls/second just for polling.

**Recommendation**: Track active poll tasks in a `DashMap<String, AbortHandle>` keyed by `client_order_id`. Before spawning, abort any existing task for the same order. This bounds concurrent tasks to one per live order.

---

### H4 — `deployment_gate_required()` Reads Env Var on Every BUY Intent (Pre-existing, Confirmed Present)

**Severity**: High
**Impact**: `std::env::var()` acquires a global environment lock on every call. On Linux, this is a `pthread_mutex_lock` on the process environment. Called on every BUY intent in `enforce_live_buy_deployment_gate`.

**Location**: `src/coordinator/admission/deployments.rs:22–32`

```rust
pub(super) fn deployment_gate_required() -> bool {
    match std::env::var("PLOY_DEPLOYMENT_GATE_REQUIRED") { ... }
}
```

**Recommendation**: Cache the result at `AdmissionController` construction time as a `bool` field. The env var is set at startup and never changes at runtime.

---

### H5 — `PositionAggregator` Full Scan on Every Sell Fill

**Severity**: High
**Impact**: `apply_sell_fill_to_positions` in `execution.rs` calls `get_agent_positions()` which does a full scan of all positions (O(n) where n = total positions across all agents), then filters, sorts, and iterates. This is called on every sell fill.

**Location**: `src/coordinator/coordinator/execution.rs:180–232`, `src/coordinator/position.rs:181–189`

```rust
pub async fn get_agent_positions(&self, agent_id: &str) -> Vec<Position> {
    self.positions.read().await.values()
        .filter(|p| p.agent_id == agent_id)
        .cloned().collect()
}
```

With 50+ concurrent positions across multiple agents, this scans all positions for every sell.

**Recommendation**: Add a secondary index `HashMap<String, Vec<String>>` mapping `agent_id -> Vec<position_id>`. This makes `get_agent_positions` O(k) where k is the agent's position count.

---

### H6 — `refresh_risk_exposure_for_agent` Called After Every Fill

**Severity**: High
**Impact**: After every fill (buy or sell), `refresh_risk_exposure_for_agent` is called, which calls `positions.agent_stats()` (full O(n) scan) then `risk_gate.update_agent_exposure()` (3 separate `RwLock` write acquisitions: `agent_stats`, `total_exposure`, `domain_exposure`). This is 3 write locks + 1 full position scan per fill.

**Location**: `src/coordinator/coordinator/execution.rs:136`, `src/coordinator/risk/exposure.rs:44–77`

```rust
pub async fn update_agent_exposure(...) {
    let old_exposure = {
        let mut stats_map = self.agent_stats.write().await; // lock 1
        ...
    };
    let mut total = self.total_exposure.write().await; // lock 2
    ...
    self.apply_domain_exposure_change(domain, ...).await; // lock 3 (domain_exposure)
}
```

**Recommendation**: Batch the three writes into a single lock scope using a combined struct, or use `tokio::sync::Mutex` with a single lock covering all exposure state. Alternatively, update exposure incrementally at fill time rather than recomputing from scratch.

---

## Medium Issues

### M1 — `current_account_notional` Acquires 3 Locks + Full Queue Scan

**Severity**: Medium
**Impact**: Called on every BUY intent via `check_governance_policy`. Acquires `risk_gate.total_exposure` (read), `capital_policy.allocator_totals` (multiple reads), and `order_queue.read()` for `pending_buy_notional_excluding_domains` (O(n) queue scan). All three are sequential.

**Location**: `src/coordinator/coordinator/ingress.rs:336–352`

```rust
pub(super) async fn current_account_notional(&self) -> Decimal {
    let platform_exposure = self.risk_gate.total_exposure().await;       // lock 1
    let (allocator_open, allocator_pending) = self.capital_policy.allocator_totals().await; // locks 2-5
    let other_pending_buy_notional = self.order_queue.read().await       // lock 6
        .pending_buy_notional_excluding_domains(&[...]);
    ...
}
```

Note: `pending_buy_notional_excluding_domains` excludes ALL four domains (Crypto, Sports, Politics, Economics), so it always returns zero for the current domain set. This is the pre-existing A-H1 bug — the function is called but its result is always zero, making the 6-lock acquisition pointless.

**Recommendation**: Fix the A-H1 bug by removing the `pending_buy_notional_excluding_domains` call (or passing the correct exclusion list). Cache `current_account_notional` with a short TTL (100ms) to avoid recomputing on every intent.

---

### M2 — `GovernanceController` Acquires 4 Separate Locks Per Intent

**Severity**: Medium
**Impact**: `handle_order_intent` calls governance 3 times sequentially:
1. `ingress_modes()` — acquires `ingress_mode.read()` + `domain_ingress_mode.read()` (2 locks)
2. `is_agent_paused()` — acquires `paused_agent_ids.read()` (1 lock)
3. `current_policy()` via `check_governance_policy` — acquires `policy.read()` (1 lock)

That is 4 separate `RwLock` acquisitions for governance checks alone, all sequential.

**Location**: `src/coordinator/coordinator/ingress.rs:72,107,121`, `src/coordinator/governance.rs:187–245`

**Recommendation**: Add a `governance_snapshot()` method that acquires all four locks once and returns a `GovernanceSnapshot` struct. Replace the three separate calls with one.

---

### M3 — `check_order` in RiskGate Acquires 5+ Locks Sequentially

**Severity**: Medium
**Impact**: `risk_gate.check_order()` acquires:
1. `state.read()` (platform state)
2. `agent_params.read()` (agent params)
3. `agent_stats.read()` (agent exposure)
4. `domain_exposure.read()` (domain exposure)
5. `total_exposure.read()` (platform exposure)
6. `daily_stats.read()` (daily loss)
7. `drawdown_stats.read()` (drawdown)

All sequential. Called up to 3 times in the risk-gate adjustment loop.

**Location**: `src/coordinator/risk/checks.rs:13–102`

**Recommendation**: Snapshot all risk state into a single `RiskSnapshot` struct with one combined read pass. The risk check logic is pure computation once the snapshot is taken.

---

### M4 — `try_entry_for_window` Clones Entire Config on Every Call

**Severity**: Medium
**Impact**: `try_entry_for_window` clones `adapter.config.backtest_config` at the top of every call. `BacktestConfig` contains multiple `Vec` and `Decimal` fields. Called on every Polymarket quote update for every active window.

**Location**: `src/strategy/staggered_arb_live/entry.rs:86`

```rust
let bc = adapter.config.backtest_config.clone();
```

**Recommendation**: Take a reference instead: `let bc = &adapter.config.backtest_config;`. The config is not mutated in this function.

---

### M5 — `handle_tick` Clones All Active Window Keys Every Tick

**Severity**: Medium
**Impact**: `handle_tick` collects all active window symbol keys into a `HashSet<String>` (with clones) on every tick. Also iterates all positions to find `Leg1Filled` ones. Called at the strategy tick rate.

**Location**: `src/strategy/staggered_arb_live/runtime_flow.rs:173–179`

```rust
let mut symbols: HashSet<String> = self.active_windows.keys().cloned().collect();
symbols.extend(
    self.positions.iter()
        .filter(|p| p.state == PaperPositionState::Leg1Filled)
        .map(|p| p.symbol.clone()),
);
```

**Recommendation**: Maintain a `HashSet<String>` of symbols with active Leg1 positions as a field, updated incrementally on position state transitions. Avoid the full scan and clone on every tick.

---

### M6 — `persist_execution` Chains 4 Sequential DB Writes Per Fill

**Severity**: Medium
**Impact**: `persist_execution` in `journal.rs` calls 4 sequential DB writes per execution:
1. `INSERT INTO agent_order_executions` (line 94)
2. `persist_execution_analysis` → `INSERT INTO execution_analysis` (line 561)
3. `persist_live_strategy_evaluation` → `INSERT INTO strategy_evaluations` (line 704)
4. `persist_exit_reason_execution` (for sells) → `INSERT INTO exit_reasons` (line 463)

All four are awaited sequentially, blocking the execution loop for the duration of 4 DB round-trips.

**Location**: `src/coordinator/journal.rs:66–175`

**Recommendation**: Fire all four writes concurrently with `tokio::join!` or `tokio::spawn`. They are independent inserts with no ordering dependency.

---

### M7 — `market_cap_for` Builds `HashSet` on Every `reserve_buy` Call

**Severity**: Medium
**Impact**: When `auto_split_by_active_markets` is enabled, `market_cap_for` builds a `HashSet<String>` by iterating `open.by_market` and `pending.by_market` on every `reserve_buy` call. Called on every BUY intent.

**Location**: `src/coordinator/capital/market/accounting.rs:294–321`

```rust
fn market_cap_for(&self, market_key: &str) -> Decimal {
    let mut active_markets: HashSet<String> = self.open.by_market.iter()
        .filter(|(_, v)| **v > Decimal::ZERO)
        .map(|(k, _)| k.clone())
        .collect();
    // ...
}
```

**Recommendation**: Cache `active_market_count` as a field on `MarketExposureBook`, updated incrementally on `add` and `subtract_from_position_key`. This makes `market_cap_for` O(1).

---

## Low Issues

### L1 — `PositionAggregator` Uses Three Separate `Arc<RwLock<...>>` Fields

**Severity**: Low
**Impact**: `PositionAggregator` has three separate `Arc<RwLock<...>>` fields: `positions`, `realized_pnl`, `position_counter`. Operations like `agent_stats` acquire `positions.read()` and `realized_pnl.read()` separately, creating two lock acquisitions where one would suffice.

**Location**: `src/coordinator/position.rs:148–155`

**Recommendation**: Consolidate into a single `Arc<RwLock<PositionState>>` struct containing all three fields. Reduces lock overhead and simplifies reasoning about consistency.

---

### L2 — `position_counter` Uses `Arc<RwLock<u64>>` Instead of `AtomicU64`

**Severity**: Low
**Impact**: The position ID counter is a `Arc<RwLock<u64>>` requiring a write lock for every `open_position` call. An `AtomicU64` with `fetch_add(1, Ordering::Relaxed)` is lock-free and faster.

**Location**: `src/coordinator/position.rs:154`

---

### L3 — `stale_heartbeat_warn_at` Uses `Arc<RwLock<HashMap>>` for Infrequent Writes

**Severity**: Low
**Impact**: `stale_heartbeat_warn_at` is a `Arc<RwLock<HashMap<String, DateTime<Utc>>>>` that is written on every stale heartbeat warning. A `DashMap` would eliminate the write lock contention.

**Location**: `src/coordinator/coordinator.rs:91`

---

### L4 — `order_update_sinks` Uses `std::sync::RwLock` (Blocking)

**Severity**: Low
**Impact**: `order_update_sinks` uses `std::sync::RwLock` (blocking), not `tokio::sync::RwLock`. Any contention on this lock will block the async executor thread. It is read on every order update delivery.

**Location**: `src/coordinator/coordinator.rs:93`

**Recommendation**: Replace with `tokio::sync::RwLock` or `DashMap`.

---

### L5 — `authorized_agents` Uses `std::sync::RwLock` (Blocking)

**Severity**: Low
**Impact**: Same issue as L4. `authorized_agents` is a `Arc<std::sync::RwLock<HashSet<String>>>` checked on every external agent submission.

**Location**: `src/coordinator/coordinator.rs:69, 119`

---

## Order Processing Critical Path Analysis

For a passing BUY intent from submission to queue, the critical path is:

| Step | Operation | Locks Acquired | DB Writes |
|------|-----------|----------------|-----------|
| 1 | Domain allowlist check | 0 | 0 (fast path) |
| 2 | Deployment identity check | 0 | 0 (fast path) |
| 3 | Governance ingress modes | 2 (read) | 0 |
| 4 | Agent pause check | 1 (read) | 0 |
| 5 | Governance policy check | 1 (read) + 2 (read) for notional | 0 |
| 6 | Deployment gate | 1 (read) + env lock | 0 |
| 7 | `persist_signal_from_intent` | 0 | **1 DB write** |
| 8 | Duplicate intent check | 1 (write) | 0 |
| 9 | Kelly sizing | 1 (read) capital | 0 |
| 10 | Min order constraints | 0 | 0 |
| 11 | `risk_gate.check_order` | 7 (read) | 0 |
| 12 | `reserve_domain_capital` | 1 (write) | 0 |
| 13 | `persist_risk_decision` (PASSED) | 0 | **1 DB write** |
| 14 | `order_queue.write().enqueue` | 1 (write) | 0 |

**Total**: ~17 lock acquisitions, 2 DB writes, all sequential, all blocking the coordinator loop.

**Estimated latency** (tango-1-1, local Postgres): ~5–15ms per passing intent under normal load. Under DB pressure: 50–200ms, which at 1 intent/sec saturates the coordinator.

---

## Staggered Arb Live Strategy Assessment

The strategy itself (`runtime_flow.rs`, `entry.rs`) is event-driven and does not have tight polling loops or blocking operations. Key observations:

1. **No blocking I/O in the hot path**: `handle_market_update` and `try_entry_for_window` are pure computation. Good.
2. **Config clone on every entry check** (M4): Minor but fixable.
3. **`windows.clone()` in `try_entry`**: Clones the entire `Vec<LiveWindow>` on every quote update. With many active windows, this is unnecessary — a reference would suffice.
4. **`reconcile_stale_live_orders` on every tick**: Iterates all `live_orders` on every tick. With many concurrent orders, this is O(n) per tick. Acceptable for current scale.
5. **`settle_expired_event` double-iterates `active_windows`**: First collects expired windows (O(n)), then retains non-expired (O(n)). Could be combined into one pass.

The strategy correctly delegates order submission to the coordinator via `StrategyAction::SubmitIntent`, avoiding direct exchange calls. This is the right design.

---

## Recommendations by Priority

### Immediate (before next live deployment)

1. **C2**: Fire journal writes asynchronously — eliminates 14 sequential DB round-trips per intent
2. **H4**: Cache `deployment_gate_required()` at startup — eliminates env lock on every BUY
3. **M4**: Change `bc = adapter.config.backtest_config.clone()` to a reference

### Short-term (next sprint)

4. **C1**: Spawn `handle_order_intent` as a task — enables concurrent intent processing
5. **H2**: Skip `cleanup_expired_intents` when queue is empty or run less frequently
6. **M2**: Add `governance_snapshot()` to batch 4 governance lock acquisitions into 1
7. **M3**: Add `risk_snapshot()` to batch 7 risk lock acquisitions into 1
8. **M6**: Parallelize 4 journal DB writes with `tokio::join!`

### Medium-term

9. **H1**: Replace `pop_lowest_priority` with a proper min-heap or indexed structure
10. **H5**: Add agent-indexed secondary map to `PositionAggregator`
11. **H6**: Batch the 3 write locks in `update_agent_exposure` into one
12. **L2**: Replace `Arc<RwLock<u64>>` position counter with `AtomicU64`
13. **L4/L5**: Replace blocking `std::sync::RwLock` with `tokio::sync::RwLock` or `DashMap`

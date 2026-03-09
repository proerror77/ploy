# Phase 2b: Performance & Scalability Analysis — Ploy Trading System

**Date**: 2026-03-08
**Scope**: Full Ploy trading system (Rust ~165K lines)
**Focus**: Latency-critical order pipeline, database performance, concurrency, memory

---

## Executive Summary

The system is well-architected for single-instance performance with Tokio async throughout and DashMap for lock-free reads. However, several issues impact production reliability: a double `record_success` bug inflates PnL tracking, unnecessary `Arc<RwLock<>>` wrappers add lock contention on the momentum strategy hot path, unbounded HashMap growth in strategy state can cause memory pressure over multi-day runs, and the WebSocket UI polls the database every 1s instead of using event-driven updates.

**Finding Distribution**: 1 Critical, 3 High, 5 Medium, 4 Low

---

## Critical Findings

### P-01: Double `record_success` call inflates PnL tracking in RiskGate

**Severity**: Critical
**Location**: `src/coordinator/coordinator.rs:4254-4268`
**Impact**: Risk gate PnL accounting is corrupted — every successful trade with positive PnL is double-counted

```rust
// Lines 4256-4265: Correct branching logic
if realized_pnl < Decimal::ZERO {
    self.risk_gate.record_success(&agent_id, Decimal::ZERO).await;
    self.risk_gate.record_loss(&agent_id, realized_pnl.abs()).await;
} else {
    self.risk_gate.record_success(&agent_id, realized_pnl).await;
}

// Line 4268: DUPLICATE — always called regardless of branch above
self.risk_gate.record_success(&agent_id, realized_pnl).await;
```

The unconditional `record_success` at line 4268 runs after the if/else block, meaning:
- Profitable trades: `record_success` called twice with `realized_pnl` — PnL inflated 2x
- Losing trades: `record_success(0)` + `record_loss(abs)` + `record_success(realized_pnl)` — the negative PnL passed to `record_success` may corrupt the daily PnL counter

**Fix**: Remove line 4268 entirely — it's a leftover from a refactor.

---

## High Severity Findings

### P-02: Unnecessary `Arc<RwLock<>>` on MomentumStrategyAdapter fields

**Severity**: High
**Location**: `src/strategy/adapters.rs:52-86`
**Impact**: ~2-5μs lock overhead per market update on hot path; unnecessary contention

The `Strategy` trait takes `&mut self`, guaranteeing exclusive access. Yet every field is wrapped in `Arc<RwLock<>>`:
```rust
positions: Arc<RwLock<HashMap<String, MomentumPosition>>>,
cex_prices: Arc<RwLock<HashMap<String, CexPriceState>>>,
pm_quotes: Arc<RwLock<HashMap<String, PmQuoteState>>>,
```

Each `on_market_update` call acquires 3-5 read/write locks sequentially. Since `&mut self` already provides exclusivity, these locks add pure overhead.

**Fix**: Replace with plain `HashMap` fields. The `Arc<RwLock<>>` is only needed if the adapter is shared across threads, which the `StrategyManager` does not do.

### P-03: Unbounded HashMap growth in strategy state

**Severity**: High
**Location**: Multiple strategy files
**Impact**: Memory leak over multi-day runs; potential OOM on constrained server (3.4GB RAM)

Several HashMaps grow without bounds:
- `staggered_arb_live.rs`: `archived_live_orders: HashMap<String, LiveOrderTrack>` — every completed order is archived forever
- `staggered_arb_live.rs`: `entry_reject_counts`, `entry_reject_counts_by_symbol`, `leg2_skip_counts` — diagnostic counters never pruned
- `momentum.rs`: `cooldowns: HashMap<String, DateTime<Utc>>` — expired cooldowns never removed
- `polymarket_ws.rs`: `QuoteCache` has `MAX_CACHE_SIZE = 10_000` with TTL eviction (good), but the `DashMap` itself never shrinks

On tango-1-1 (3.4GB RAM + 4GB swap, systemd MemoryMax=1536M), unbounded growth will eventually trigger OOM kill.

**Fix**: Add periodic pruning (e.g., every 5 minutes, remove entries older than 1 hour):
```rust
fn prune_archived_orders(&mut self) {
    let cutoff = Utc::now() - Duration::hours(1);
    self.archived_live_orders.retain(|_, track| track.closed_at > cutoff);
}
```

### P-04: WebSocket UI polls database every 1 second

**Severity**: High
**Location**: `src/api/state.rs` (spawn_realtime_broadcast_loop)
**Impact**: 1s latency floor for UI updates; unnecessary DB load (1 query/sec per connected client)

The WebSocket broadcast loop polls PostgreSQL every second to detect new trades and position changes. This adds:
- 1s worst-case latency for UI updates
- Constant DB load even when no trades are happening
- Scales poorly with multiple WebSocket clients

**Fix**: Emit `WsMessage` directly from the coordinator when orders execute:
```rust
// In coordinator after successful execution:
if let Some(ws_tx) = &self.ws_broadcast_tx {
    let _ = ws_tx.send(WsMessage::Trade(trade_data));
}
```

---

## Medium Severity Findings

### P-05: Sequential lock acquisition in `handle_order_intent`

**Severity**: Medium
**Location**: `src/coordinator/coordinator.rs:900-950`
**Impact**: ~10-20μs per intent from sequential RwLock acquisitions

The intent handler acquires multiple RwLock reads sequentially:
1. `order_queue.read().await` — check pending shares
2. `ingress_mode.read().await` — check global mode
3. `domain_ingress_mode.read().await` — check domain mode

Each `.read().await` involves a lock acquisition. While RwLock reads are concurrent, the sequential pattern means the intent handler holds no locks while waiting for the next one, causing unnecessary round-trips.

**Fix**: Batch the reads or use `DashMap` for lock-free reads on hot-path state.

### P-06: PostgresStore uses string SQL without compile-time checks

**Severity**: Medium
**Location**: `src/adapters/postgres.rs` (1,364 lines)
**Impact**: Runtime SQL errors; no query plan optimization hints

All queries use `sqlx::query()` with string SQL. This means:
- SQL syntax errors are only caught at runtime
- No compile-time type checking of bind parameters
- No automatic query plan caching hints

**Fix**: Use `sqlx::query!()` macro for critical paths (order submission, cycle state transitions). This provides compile-time SQL validation and automatic type inference.

### P-07: No connection pool size tuning

**Severity**: Medium
**Location**: `src/config.rs` (DatabaseConfig)
**Impact**: Default pool size may be suboptimal for workload

The `DatabaseConfig` has `max_connections` but the default and actual pool configuration should be tuned for the workload:
- Too few connections: strategy evaluation blocks on DB writes
- Too many connections: PostgreSQL overhead from connection management

**Fix**: Set pool size based on workload: `max_connections = 2 * num_strategies + 5` as a starting point. Add connection pool metrics to monitor utilization.

### P-08: `staggered_arb_live.rs` entry evaluation is 470 lines of sequential checks

**Severity**: Medium
**Location**: `src/strategy/staggered_arb_live.rs:1200-1670`
**Impact**: Hot path with 15+ sequential filter gates; hard to profile and optimize

The `try_entry_for_window()` method runs 15+ sequential checks, each potentially doing HashMap lookups and Decimal arithmetic. The method is called on every price tick for every active window.

**Fix**: Extract into a pure function that returns early on first rejection. Profile to identify which checks are most frequently hit and reorder for fast rejection.

### P-09: Circuit breaker uses `RwLock` for `last_failure` timestamp

**Severity**: Medium
**Location**: `src/coordination/circuit_breaker.rs:223`
**Impact**: Write lock contention on failure recording path

```rust
*self.last_failure.write().await = Some(Utc::now());
```

The `last_failure` field uses `RwLock<Option<DateTime<Utc>>>` but could use `AtomicI64` (storing epoch millis) for lock-free updates, since `DateTime<Utc>` is just an i64 timestamp.

**Fix**: Use `AtomicI64` for the timestamp:
```rust
last_failure_epoch_ms: AtomicI64,
```

---

## Low Severity Findings

### P-10: `to_f64().unwrap_or(0.0)` pattern on hot path

**Severity**: Low
**Location**: `src/strategy/staggered_arb_live.rs` (12+ occurrences)
**Impact**: Silent precision loss; minor CPU overhead from repeated conversions

### P-11: Compilation unit size — bootstrap.rs at 7,761 lines

**Severity**: Low
**Impact**: Incremental compilation of coordinator module is slower than necessary

### P-12: No Prometheus metrics exposition

**Severity**: Low
**Impact**: Cannot monitor latency percentiles, queue depths, or cache hit rates in production

### P-13: `DashMap` in QuoteCache never shrinks

**Severity**: Low
**Location**: `src/adapters/polymarket_ws.rs:427`
**Impact**: Memory fragmentation over time; DashMap allocates shards that persist even after entries are evicted

---

## Positive Patterns

- **DashMap for QuoteCache**: Lock-free concurrent reads for price data — correct choice for high-frequency reads
- **Tokio select! loop**: The coordinator's main loop is well-structured with proper channel draining
- **Broadcast channels for data plane**: Market data distribution uses `tokio::sync::broadcast` — zero-copy fan-out
- **Connection pooling via sqlx**: PgPool handles connection lifecycle correctly
- **Feature-gated heavy deps**: `burn`, `duckdb`, `tract-onnx` behind feature flags — reduces binary size and compile time for production builds

---

## Recommended Priority Actions

1. **Immediate**: Remove duplicate `record_success` at coordinator.rs:4268 (P-01)
2. **This sprint**: Remove `Arc<RwLock<>>` from MomentumStrategyAdapter (P-02)
3. **This sprint**: Add HashMap pruning to staggered arb and momentum strategies (P-03)
4. **Next sprint**: Event-driven WebSocket updates instead of DB polling (P-04)
5. **Backlog**: Profile and optimize entry evaluation hot path (P-08)
6. **Backlog**: Add Prometheus metrics for latency monitoring (P-12)

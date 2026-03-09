# Phase 1B: Architectural Design & Structural Integrity Review

**Date**: 2026-03-08
**Scope**: Full Ploy trading system — 165,698 lines of Rust across 318 source files, plus TypeScript sidecar and React frontend
**Branch**: `hotfix/staggered-arb-release-20260306`

---

## Executive Summary

Ploy is a well-structured multi-strategy prediction market trading system with strong domain modeling and layered risk management. The architecture demonstrates thoughtful separation between strategy logic, execution, coordination, and data access. However, the system shows signs of organic growth: three competing agent abstractions coexist, type duplication is widespread, and the bootstrap/coordinator layer has accumulated excessive responsibility. The codebase would benefit from consolidating its agent model, extracting a shared utility crate, and decomposing the 7,761-line bootstrap module.

---

## 1. Component Boundaries

### 1.1 Three-Layer Agent Abstraction (Severity: High)

The system has three distinct agent interfaces that overlap in purpose:

1. **`DomainAgent`** (`src/platform/traits.rs`) — Push-based. The `EventRouter` calls `on_event()` and the agent returns `Vec<OrderIntent>`. Implemented by 4 agents in `src/platform/agents/`.

2. **`TradingAgent`** (`src/agents/traits.rs`) — Pull-based. The agent owns its `run()` loop and communicates via `AgentContext`. Implemented by 6 agents in `src/agents/`.

3. **`Strategy`** (`src/strategy/traits.rs`) — Event-driven. Receives `MarketUpdate`/`OrderUpdate` callbacks, returns `Vec<StrategyAction>`. Managed by `StrategyManager`. Implemented by 10 strategies.

**Impact**: New developers must understand which abstraction to use. The `DomainAgent` and `TradingAgent` traits share identical method signatures (`id()`, `name()`, `domain()`, `risk_params()`) but are not unified. The `Strategy` trait is the most mature (with `DataFeed` subscription, `StrategyAction` output, and `StrategyManager` lifecycle), but the coordinator only speaks `TradingAgent`.

**Recommendation**: Deprecate `DomainAgent` in favor of `TradingAgent` (the pull-based model is more flexible). Wrap `Strategy` implementations in a generic `StrategyAgent<S: Strategy>` adapter that implements `TradingAgent`, so all strategies can run under the coordinator. The `MomentumStrategyAdapter` and `SplitArbStrategyAdapter` in `src/strategy/adapters.rs` (2,972 lines) already do this manually — generalize the pattern.

### 1.2 Strategy Module Sprawl (Severity: Medium)

`src/strategy/mod.rs` is 271 lines of re-exports spanning 40+ submodules. The module mixes:
- Core abstractions (`traits`, `core/`, `execution/`)
- Live strategies (`momentum`, `split_arb`, `staggered_arb_live`)
- Backtesting engines (`backtest`, `staggered_arb_backtest`, `directional_backtest`, `garch_probability_backtest`, `liquidity_vacuum_backtest`, `momentum_backtest`)
- Supporting infrastructure (`feeds`, `registry`, `fee_model`, `trading_costs`, `reconciliation`)
- Domain-specific modules (`nba_comeback/`, `event_edge/`, `gamma_scalping/`, `pattern_memory/`)

**Impact**: The flat module structure makes it hard to understand which components are production strategies vs. research/backtest tools. The re-export surface is enormous — 230+ public symbols from a single module.

**Recommendation**: Reorganize into `strategy/live/`, `strategy/backtest/`, and `strategy/infra/` submodules. Reduce the re-export surface to only the types needed by external consumers (coordinator, API, CLI).

### 1.3 Platform vs. Coordinator Overlap (Severity: Medium)

Both `src/platform/` and `src/coordinator/` provide order execution orchestration:
- `OrderPlatform` (`src/platform/platform.rs`) owns `EventRouter`, `RiskGate`, `OrderQueue`, `PositionAggregator`, and `OrderExecutor`
- `Coordinator` (`src/coordinator/coordinator.rs`, 6,508 lines) also owns `RiskGate`, `OrderQueue`, `PositionAggregator`, and `OrderExecutor`

The `Coordinator` appears to be the newer, more complete implementation (it adds governance, deployment matrix, duplicate guards, domain ingress control). The `OrderPlatform` is the older push-based orchestrator.

**Impact**: Two parallel execution paths exist. Code changes to risk logic must be applied in both places.

**Recommendation**: Mark `OrderPlatform` as deprecated. Route all execution through the `Coordinator`. If the push-based `DomainAgent` model is still needed, wrap it as a `TradingAgent` that internally uses the `EventRouter`.

---

## 2. Dependency Management

### 2.1 Cargo.toml Health (Severity: Low)

The dependency set is well-curated:
- Feature flags properly gate optional heavy dependencies (`burn`, `duckdb`, `tract-onnx`, `ethers-*`, `claude-agent-sdk-rs`)
- Vendored `polymarket-client-sdk` via `[patch.crates-io]` — appropriate for a fast-moving SDK
- Release profile is sensible (`lto = "thin"`, `codegen-units = 4`, `panic = "abort"`)
- Dev profile optimizes third-party crate debug info (`debug = 0` for deps)

Minor concerns:
- `anyhow` and `thiserror` are both used. `PloyError::Other(#[from] anyhow::Error)` acts as an escape hatch that bypasses typed error handling. This is acceptable for a trading system where uptime matters more than error exhaustiveness, but it should be used sparingly.
- `rand = "0.8"` — version 0.9 has been stable since early 2025. Not urgent but worth updating.

### 2.2 Module Dependency Direction (Severity: Medium)

The dependency graph generally flows correctly: `domain` <- `platform` <- `coordinator` <- `agents`. However:

- `src/coordinator/bootstrap.rs` (7,761 lines) imports from nearly every module in the system: `adapters`, `agents`, `ai_clients`, `config`, `coordinator`, `domain`, `exchange`, `platform`, `signing`, `strategy`. This is the system's "god module" — it wires everything together.
- `src/strategy/` imports from `src/adapters/` directly (e.g., `SpotPrice`, `PolymarketClient`), bypassing the exchange abstraction layer. Strategies should depend on abstract data feeds, not concrete adapter types.
- `src/api/state.rs` imports `CoordinatorHandle` and `GrokClient` directly, creating a tight coupling between the API layer and specific implementation details.

**Recommendation**: Extract bootstrap wiring into a dedicated `src/wiring/` or `src/app/` module. Introduce a `DataProvider` trait in `src/platform/` that strategies depend on instead of importing adapter types directly.

### 2.3 No Circular Dependencies Detected (Severity: N/A)

The module graph is acyclic. Rust's module system enforces this at compile time. The `lib.rs` re-exports are clean and well-organized with feature gates.

---

## 3. API Design

### 3.1 REST API Structure (Severity: Low)

The API (`src/api/routes.rs`) is well-organized with clear resource grouping:
- `/api/auth/*` — Session management
- `/api/stats/*` — Analytics
- `/api/trades/*`, `/api/positions/*` — Trading data
- `/api/system/*` — Control plane
- `/api/strategies/*` — Strategy management
- `/api/deployments/*` — Deployment matrix
- `/api/governance/*` — Policy management
- `/api/sidecar/*` — Claude Agent SDK integration
- `/api/security/*` — Security events

Strengths:
- CORS is configurable via `PLOY_API_CORS_ALLOWED_ORIGINS`
- Auth uses constant-time comparison (`ct_eq`) to prevent timing attacks
- Sidecar live orders require explicit opt-in (`PLOY_SIDECAR_ORDERS_LIVE_ENABLED`)
- Capability discovery endpoint (`/api/capabilities`) for machine-readable surface

### 3.2 No API Versioning (Severity: Medium)

All endpoints are under `/api/` with no version prefix. The sidecar, frontend, and OpenClaw gateway all consume this API.

**Impact**: Breaking changes to response shapes will require coordinated deploys across Rust backend, TypeScript sidecar, and React frontend.

**Recommendation**: Add `/api/v1/` prefix. The current endpoints can be aliased to v1 with zero behavior change.

### 3.3 Inconsistent Error Contracts (Severity: Medium)

The RPC interface (`src/cli/rpc.rs`) uses JSON-RPC 2.0 with proper error codes. The REST API handlers return a mix of:
- `(StatusCode, Json<Value>)` — ad-hoc JSON responses
- `Result<Json<T>, StatusCode>` — typed responses without error bodies

There is no unified API error envelope. The sidecar handlers (`src/api/handlers/sidecar.rs`, 2,113 lines) define their own response types.

**Recommendation**: Define a standard `ApiError` type that serializes to `{ "error": { "code": "...", "message": "...", "details": ... } }` and implement `IntoResponse` for it. Use Axum's error handling middleware.

### 3.4 WebSocket Protocol (Severity: Low)

The WebSocket broadcast (`WsMessage` enum) supports `Trade`, `Position`, and `Market` message types. The `spawn_realtime_broadcast_loop` in `AppState` polls the database every 1 second to detect changes — this is a pragmatic approach but introduces up to 1s latency for UI updates.

**Recommendation**: For lower latency, emit `WsMessage` directly from the coordinator when orders execute, rather than polling the database.

---

## 4. Data Model

### 4.1 Migration Design (Severity: Low)

The 22 migrations show disciplined evolution:
- `001_init.sql` establishes core tables (rounds, ticks, cycles, orders, daily_metrics, strategy_state, dump_signals) with proper FK relationships, CHECK constraints, and indexes
- `007_performance_indexes.sql` uses drift-safe schema detection (`to_regclass`, `information_schema.columns`) — excellent defensive migration practice for mixed-version databases
- `019_data_integrity.sql` adds DLQ retry guards, status transition triggers, and a `check_data_integrity()` health check function
- Automatic `updated_at` trigger applied to all tables with that column

Strengths:
- Proper use of `ON CONFLICT` for upserts
- `ON DELETE CASCADE` / `SET NULL` used appropriately
- Partial indexes for active-state queries (e.g., `WHERE status IN ('pending', 'submitted')`)
- Singleton pattern for `strategy_state` (`CHECK (id = 1)`)

### 4.2 Schema Sprawl via Bootstrap DDL (Severity: High)

`src/coordinator/bootstrap.rs` contains `CREATE TABLE IF NOT EXISTS` statements for tables like `accounts` and `agent_order_executions` that are not in the migration files. This creates a shadow schema that:
- Is not tracked by `sqlx migrate`
- Cannot be rolled back
- May conflict with future migrations
- Is invisible to schema documentation tools

**Impact**: The database schema is split between `migrations/` (22 files) and runtime DDL in Rust code. A fresh database created by migrations alone will be missing tables that the coordinator expects.

**Recommendation**: Move all `CREATE TABLE` statements from bootstrap.rs into proper numbered migrations. The bootstrap should only run `sqlx::migrate!()` and then verify expected tables exist.

### 4.3 PostgresStore as Monolith (Severity: Medium)

`src/adapters/postgres.rs` (1,364 lines) is a single file containing all database access methods. It uses raw `sqlx::query()` with string SQL throughout — no compile-time checked queries (`sqlx::query!` macro).

**Impact**: SQL errors are only caught at runtime. Schema changes require manual verification of all query strings.

**Recommendation**: Consider using `sqlx::query!` for critical paths (order submission, cycle state transitions) where correctness is paramount. For the full store, consider splitting into domain-specific files: `postgres/rounds.rs`, `postgres/cycles.rs`, `postgres/orders.rs`, etc.

### 4.4 EngineStore Trait — Good DI Pattern (Severity: N/A — Positive)

`src/strategy/execution/engine_store.rs` defines a minimal `EngineStore` trait that decouples the strategy engine from `PostgresStore`. This enables unit testing with mock stores. The trait follows YAGNI — only methods the engine actually calls are included. This is a pattern worth replicating for other database consumers.

---

## 5. Design Patterns

### 5.1 Circuit Breaker — Dual Implementation (Severity: Medium)

Two circuit breaker implementations exist:
- `src/coordination/circuit_breaker.rs` — System-level `TradingCircuitBreaker` with `Closed/Open/HalfOpen` states, configurable thresholds, and auto-recovery
- `src/adapters/polymarket_ws.rs` — WebSocket-level `CircuitBreaker` for connection health

Additionally, `src/platform/risk.rs` has its own circuit breaker logic embedded in the `RiskGate` (consecutive failure counting, halted state, auto-recovery cooldown).

**Impact**: Three places implement failure-counting and state-transition logic. The `RiskGate` circuit breaker and the `TradingCircuitBreaker` could diverge in behavior.

**Recommendation**: The WebSocket circuit breaker is appropriately scoped to its adapter. However, the `RiskGate` should delegate to `TradingCircuitBreaker` rather than reimplementing the pattern.

### 5.2 Event Sourcing & DLQ — Well Implemented (Severity: N/A — Positive)

The persistence layer (`src/persistence/`) provides:
- `CheckpointService` — Periodic state snapshots with configurable intervals
- `DLQProcessor` — Dead letter queue with retry logic, max retries, and permanent failure marking
- `EventStore` — Append-only event log with metadata

The DLQ has database-level guards (migration 019): retry count constraints, status transition triggers preventing resolved->other transitions. This is production-grade reliability infrastructure.

### 5.3 Coordinator Pattern — Solid but Overloaded (Severity: High)

The `Coordinator` (6,508 lines) handles:
- Order intent ingestion and risk checking
- Queue draining and execution
- Agent state management and health checks
- Governance policy enforcement
- Deployment matrix management
- Duplicate order detection
- Domain ingress control (pause/resume per domain)
- Cross-agent position aggregation

The `CoordinatorHandle` (clone-friendly) provides a clean API for agents: `submit_order()`, `update_agent_state()`, `read_state()`.

**Impact**: The coordinator is the single point of failure and the hardest module to test. At 6,508 lines, it's difficult to reason about all state transitions.

**Recommendation**: Extract governance, deployment matrix, and duplicate detection into separate services that the coordinator delegates to. Target: coordinator under 2,000 lines focused on the core order flow.

### 5.4 Subscription Planner — Elegant (Severity: N/A — Positive)

`src/platform/subscription_planner.rs` computes deltas between current and desired WebSocket subscriptions, producing `PlanDelta` (subscribe/unsubscribe lists). This is a clean functional approach to managing shared data plane resources across multiple strategy consumers.

### 5.5 Data Plane Abstraction (Severity: N/A — Positive)

`src/platform/data_plane.rs` provides `PlatformDataPlane` with `CryptoDataPlaneHandle` and `BinanceDataPlaneHandle` — shared market data adapters with broadcast channels. The freshness tracking (`DataPlaneFreshness`) monitors data staleness per source. This is well-designed infrastructure for a multi-strategy system.

---

## 6. Architectural Consistency

### 6.1 Type Duplication Across Modules (Severity: High)

Multiple types are defined independently in different modules with the same name but different fields:

| Type | Locations | Notes |
|------|-----------|-------|
| `Position` | `domain/order.rs`, `platform/position.rs`, `strategy/momentum.rs`, `strategy/position_manager.rs`, `rl/environment/leadlag.rs` | 5 distinct definitions |
| `PositionStatus` | `strategy/split_arb.rs`, `strategy/core/position.rs`, `strategy/position_manager.rs` | 3 definitions |
| `ArbStats` | `strategy/split_arb.rs`, `strategy/core/position.rs` | 2 definitions |
| `RiskLevel` | `strategy/traits.rs`, `strategy/multi_outcome.rs` | 2 definitions |
| `AlertLevel` | `strategy/traits.rs`, `supervisor/alert_manager.rs` | 2 definitions |
| `CircuitBreaker` | `coordination/circuit_breaker.rs`, `adapters/polymarket_ws.rs` | 2 definitions (different scope) |

The `strategy/mod.rs` re-exports handle some of this with aliases (`CoreArbSide`, `CoreArbStats`, `CoreHedgedPosition`, `CorePositionStatus`, `PersistedPosition`, `PersistedPositionStatus`), but this is a symptom, not a solution.

**Impact**: Confusion about which `Position` type to use. Conversion boilerplate between equivalent types. Risk of using the wrong type in a given context.

**Recommendation**: Consolidate into canonical types in `src/domain/` or `src/platform/types.rs`. The `core/` module was started for this purpose but adoption is incomplete. The RL `Position` can remain separate (different domain), but the 4 trading-related `Position` types should merge.

### 6.2 `parse_boolish` Duplication (Severity: Low)

The function `parse_boolish` (parses "1"/"true"/"yes"/"on" to bool) is copy-pasted 7 times across:
- `src/api/auth.rs`
- `src/api/routes.rs`
- `src/api/state.rs`
- `src/api/handlers/capabilities.rs`
- `src/api/handlers/deployment_gate.rs`
- `src/api/handlers/sidecar.rs`
- `src/adapters/polymarket_clob.rs`

**Recommendation**: Move to a `src/util.rs` or `src/common.rs` module and import everywhere.

### 6.3 Strategy Trait Consistency (Severity: Low)

All 10 `Strategy` implementations follow the same pattern: implement `on_market_update`, `on_order_update`, `on_tick`, `state`, `positions`, `shutdown`. The trait is well-designed with clear contracts. The `StrategyManager` handles lifecycle uniformly.

The `TradingAgent` implementations are also consistent: each owns its `run()` loop, subscribes to data feeds, and submits orders via `AgentContext`.

The `DomainAgent` implementations are the least consistent — some have complex internal state machines while others are thin wrappers.

### 6.4 Configuration Consistency (Severity: Medium)

`src/config.rs` (1,168 lines) defines the monolithic `AppConfig` with nested configs for every subsystem. Each strategy also has its own config type (e.g., `MomentumConfig`, `SplitArbConfig`, `StaggeredArbBacktestConfig`, `GammaScalpingConfig`).

The config loading uses the `config` crate with TOML files + environment variable overrides. This is standard and works well. However:
- Strategy configs are loaded from separate TOML files (e.g., `config/strategies/staggered_arb.toml`) but the path resolution is ad-hoc
- The `AppConfig` includes strategy-specific fields (`event_edge_agent`, `nba_comeback`) that should be in strategy-level configs
- TOML float parsing gotcha (documented in MEMORY.md) — `85` vs `85.0` — is a recurring source of bugs

**Recommendation**: Move strategy-specific config out of `AppConfig` into a `StrategyDeployment`-level config system. The deployment matrix already exists — extend it to carry strategy parameters.

---

## 7. Cross-Cutting Concerns

### 7.1 Error Handling (Severity: Low)

`PloyError` (`src/error.rs`) is a well-structured thiserror enum with:
- Domain-specific variants (market data, order execution, state machine, risk management)
- Proper `#[from]` conversions for library errors (sqlx, reqwest, serde_json, io)
- Separate `OrderError` and `RiskError` enums with structured fields
- `From` impls that convert sub-errors into `PloyError` variants

The `PloyError::Other(#[from] anyhow::Error)` escape hatch is used sparingly. The error hierarchy is appropriate for a trading system.

### 7.2 Logging (Severity: Low)

Logging uses `tracing` consistently throughout:
- `init_logging()` sets up dual output: console + daily rotating file appender
- `init_logging_simple()` for CLI commands (WARN level only)
- `EnvFilter` supports runtime configuration via `RUST_LOG`
- `#[instrument]` annotations on key database methods
- Log directory writability is pre-checked to avoid panics

The file appender guard is leaked (`Box::leak`) — acceptable for a long-running process but worth documenting.

### 7.3 Metrics (Severity: Medium)

`src/services/metrics.rs` provides a `Metrics` service, but it's not integrated with a standard metrics backend (Prometheus, StatsD). The `PlatformStats` struct in `src/platform/platform.rs` tracks basic counters (intents processed, risk passed/blocked, executions success/failed).

**Recommendation**: Integrate with the `metrics` crate ecosystem for standard Prometheus exposition. The health server already exists on port 8080 — add a `/metrics` endpoint.

### 7.4 Input Validation (Severity: Low — Positive)

`src/validation.rs` provides explicit validation for external API data:
- `validate_price()` — ensures binary option prices are in [0, 1]
- `validate_shares()` — ensures non-zero, within max bounds

The `safety/direct_live.rs` module enforces a "coordinator-only live trading" gate — standalone strategies cannot submit live orders without going through the coordinator's risk checks. This is a strong safety pattern.

### 7.5 Security (Severity: Low — Positive)

- Private keys use `zeroize` for memory cleanup
- API tokens use constant-time comparison
- Sidecar auth is separate from admin auth
- Live order submission requires explicit opt-in flags
- Prompt injection sanitization in autonomous AI client

---

## 8. Bootstrap / Wiring Complexity

### 8.1 God Module: bootstrap.rs (Severity: Critical)

`src/coordinator/bootstrap.rs` at 7,761 lines is the largest file in the codebase. It contains:
- Database connection setup and table creation
- Account management
- WebSocket connection management
- Market data collection and persistence
- Strategy instantiation and configuration
- Agent creation and registration
- Data plane orchestration
- Quote persistence pipelines
- Binance price/LOB tick persistence
- CLOB orderbook snapshot persistence
- Platform start control and shutdown coordination

This file is the system's composition root, but it has grown far beyond that role. It contains business logic (quote persistence intervals, market refresh timers, collector configuration) that should live in dedicated modules.

**Impact**: Any change to platform startup requires modifying this file. It's the most likely source of merge conflicts in parallel development. Testing individual bootstrap phases is impossible without the full system context.

**Recommendation**: Decompose into:
- `bootstrap/db.rs` — Database setup and migration
- `bootstrap/data_plane.rs` — Market data adapter wiring
- `bootstrap/agents.rs` — Agent instantiation
- `bootstrap/persistence.rs` — Quote/tick persistence pipelines
- `bootstrap/mod.rs` — Orchestration of the above phases

Target: each file under 1,500 lines.

---

## 9. Findings Summary

| # | Finding | Severity | Category |
|---|---------|----------|----------|
| 1 | Three competing agent abstractions (DomainAgent, TradingAgent, Strategy) | High | Component Boundaries |
| 2 | Strategy module sprawl — 40+ flat submodules, 230+ re-exports | Medium | Component Boundaries |
| 3 | Platform vs. Coordinator overlap in order execution | Medium | Component Boundaries |
| 4 | Bootstrap.rs god module — 7,761 lines | Critical | Wiring Complexity |
| 5 | Type duplication — Position (5x), PositionStatus (3x), ArbStats (2x) | High | Consistency |
| 6 | Shadow schema — CREATE TABLE in bootstrap.rs outside migrations | High | Data Model |
| 7 | No API versioning | Medium | API Design |
| 8 | Inconsistent API error contracts | Medium | API Design |
| 9 | Circuit breaker logic duplicated in RiskGate and TradingCircuitBreaker | Medium | Design Patterns |
| 10 | PostgresStore monolith — 1,364 lines, no compile-time SQL checks | Medium | Data Model |
| 11 | `parse_boolish` duplicated 7 times | Low | Consistency |
| 12 | Strategy-specific config embedded in AppConfig | Medium | Configuration |
| 13 | No standard metrics exposition (Prometheus) | Medium | Cross-Cutting |
| 14 | WebSocket UI updates poll DB instead of event-driven | Low | API Design |
| 15 | Strategies import concrete adapter types instead of abstractions | Medium | Dependencies |

### Positive Patterns Worth Preserving

- **EngineStore trait** — Clean DI for database access in strategy engine
- **Subscription Planner** — Functional delta computation for WebSocket subscriptions
- **Data Plane + Freshness** — Shared market data with staleness monitoring
- **Safety gate** — Coordinator-only live trading enforcement
- **Event sourcing + DLQ** — Production-grade crash recovery
- **Drift-safe migrations** — Schema detection before DDL in migration 007
- **Feature-gated compilation** — Heavy optional deps (burn, duckdb, tract) behind feature flags
- **Governance policy** — Runtime-adjustable trading constraints with audit trail

---

## 10. Recommended Priority Actions

### Immediate (next sprint)
1. Move bootstrap DDL into numbered migrations (finding #6)
2. Extract `parse_boolish` to shared utility (finding #11)
3. Add `/api/v1/` prefix to REST routes (finding #7)

### Short-term (next 2-4 weeks)
4. Decompose `bootstrap.rs` into sub-modules (finding #4)
5. Consolidate `Position` types into canonical domain types (finding #5)
6. Define standard `ApiError` envelope for REST handlers (finding #8)
7. Delegate RiskGate circuit breaker to TradingCircuitBreaker (finding #9)

### Medium-term (next quarter)
8. Deprecate `DomainAgent`, unify on `TradingAgent` + `Strategy` (finding #1)
9. Reorganize strategy module into live/backtest/infra (finding #2)
10. Introduce `DataProvider` trait to decouple strategies from adapters (finding #15)
11. Add Prometheus metrics exposition (finding #13)
12. Deprecate `OrderPlatform` in favor of `Coordinator` (finding #3)

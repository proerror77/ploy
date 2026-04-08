# RTDS And Reference-Price Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add first-class Pyth `equity_prices` support and a unified reference-price capture foundation without regressing the existing Chainlink/Binance crypto path.

**Architecture:** Extend the vendored Polymarket RTDS client with typed `equity_prices` request/response support, then introduce a repo-owned reference-price registry and persistence path in `ploy-runner`. Keep the current crypto runtime behavior intact by treating the new foundation as an additive data plane in phase 1.

**Tech Stack:** Rust, vendored `polymarket-client-sdk`, Tokio streams, SQLx migrations, existing `ploy-runner` feed wiring, targeted `rtk cargo test/check`

---

### Task 1: Track the foundation lane and freeze the file map

**Files:**
- Modify: `tasks/todo.md`
- Modify: `docs/plans/2026-04-06-polymarket-expansion-master-plan.md`
- Create: `docs/plans/2026-04-06-rtds-reference-foundation-implementation-plan.md`

- [ ] Add a dedicated tracker section at the top of `tasks/todo.md` for the RTDS/reference-price lane.
- [ ] Link this plan from the master plan under the execution split.
- [ ] Verify the new plan file exists.

Run:

```bash
ls docs/plans | rg 'rtds-reference-foundation'
```

Expected: `2026-04-06-rtds-reference-foundation-implementation-plan.md`

### Task 2: Extend the vendored RTDS request/response model for `equity_prices`

**Files:**
- Modify: `vendor/polymarket-client-sdk/src/rtds/types/request.rs`
- Modify: `vendor/polymarket-client-sdk/src/rtds/types/response.rs`
- Modify: `vendor/polymarket-client-sdk/src/rtds/client.rs`
- Modify: `vendor/polymarket-client-sdk/src/rtds/mod.rs`

- [ ] Add a `Subscription::equity_prices(symbol: Option<String>, msg_type: &str)` constructor that serializes filters in the same JSON-string form the official RTDS docs require.
- [ ] Add typed response models for:
  - live updates
  - subscribe snapshot/backfill messages
  - carried-forward closed-session flags
- [ ] Add `RtdsMessage::as_equity_price_update()` and `RtdsMessage::as_equity_price_snapshot()` helpers.
- [ ] Add `Client::subscribe_equity_prices(symbol: Option<String>, include_snapshot: bool)` and `Client::unsubscribe_equity_prices(symbol: Option<String>)`.

Run:

```bash
CARGO_TARGET_DIR=/tmp/ploy-rtds-foundation rtk cargo test -p polymarket-client-sdk rtds -- --nocapture
```

Expected: the RTDS-focused vendored SDK tests pass.

### Task 3: Add example and regression coverage for Pyth-backed feeds

**Files:**
- Create: `vendor/polymarket-client-sdk/examples/rtds_equity_prices.rs`
- Modify: `vendor/polymarket-client-sdk/tests/websocket.rs`
- Modify: `vendor/polymarket-client-sdk/examples/rtds_crypto_prices.rs`

- [ ] Add a minimal example that subscribes to `AAPL` and `XAUUSD`, logs both snapshot and update payloads, and documents the lowercase payload semantics.
- [ ] Add regression tests that prove:
  - `equity_prices` filters serialize as escaped JSON strings
  - update payloads deserialize
  - snapshot payloads deserialize
  - unsubscribe requests target the correct topic/type pair
- [ ] Keep the existing crypto example aligned with the expanded API docs so the two feed families are parallel.

Run:

```bash
CARGO_TARGET_DIR=/tmp/ploy-rtds-foundation rtk cargo test -p polymarket-client-sdk websocket -- --nocapture
```

Expected: the websocket protocol tests pass with new `equity_prices` coverage.

### Task 4: Introduce a repo-owned reference-price registry in `ploy-runner`

**Files:**
- Create: `apps/ploy-runner/src/reference_prices.rs`
- Modify: `apps/ploy-runner/src/feeds.rs`
- Modify: `apps/ploy-runner/src/main.rs`
- Modify: `apps/ploy-runner/src/collector.rs`
- Modify: `apps/ploy-runner/Cargo.toml`

- [ ] Extract the current `ChainlinkPriceCache` ownership out of `feeds.rs` into a new `reference_prices.rs` module.
- [ ] Define one normalized cache key shape that can represent:
  - `btcusdt` Binance spot
  - `btc/usd` Chainlink
  - `aapl`, `xauusd`, `wti`, `eurusd` Pyth
- [ ] Add a `ReferencePriceRegistry` API that records:
  - normalized symbol
  - source
  - asset class
  - last value
  - last update timestamp
  - carried-forward flag when applicable
- [ ] Rewire the existing crypto spot and Chainlink tasks to publish into the new registry.
- [ ] Add a new Pyth RTDS task in `feeds.rs` that subscribes to configured non-crypto symbols and also writes into the same registry.
- [ ] Keep scanner/runtime callers on the existing crypto path for now; phase 1 is foundation, not a strategy-runtime semantic change.

Run:

```bash
CARGO_TARGET_DIR=/tmp/ploy-rtds-foundation rtk cargo check -p ploy-runner --all-targets
```

Expected: `ploy-runner` compiles with the new registry module and Pyth feed wiring.

### Task 5: Persist non-crypto reference ticks with explicit source metadata

**Files:**
- Create: `migrations/027_reference_price_ticks.sql`
- Modify: `apps/ploy-runner/src/feeds.rs`
- Modify: `apps/ploy-runner/src/main.rs`
- Modify: `crates/ploy-strategy-bundles/src/feed/database.rs`

- [ ] Add a migration for `reference_price_ticks` with columns for:
  - `symbol`
  - `source`
  - `asset_class`
  - `price`
  - `full_accuracy_value` nullable
  - `price_time`
  - `received_at`
  - `is_carried_forward`
- [ ] Persist Pyth/equity ticks into this table at live capture time.
- [ ] Add a small reader helper in `feed/database.rs` that can later be used by the backtest-expansion phase without wiring it into the default loader yet.
- [ ] Leave existing `binance_price_ticks` and crypto quote tables untouched.

Run:

```bash
CARGO_TARGET_DIR=/tmp/ploy-rtds-foundation rtk cargo check -p ploy-runner -p ploy-strategy-bundles --all-targets
```

Expected: both crates compile with the new migration-backed persistence path.

### Task 6: Verify the foundation slice end-to-end

**Files:**
- Modify: `tasks/todo.md`

- [ ] Run focused tests for the vendored SDK and `ploy-runner`.
- [ ] Run one smoke path that starts the runner with a tiny configured Pyth symbol set in dry-run mode or a fixture-backed mode.
- [ ] Record the exact commands and outcomes in `tasks/todo.md`.

Run:

```bash
CARGO_TARGET_DIR=/tmp/ploy-rtds-foundation rtk cargo test -p polymarket-client-sdk rtds -- --nocapture
CARGO_TARGET_DIR=/tmp/ploy-rtds-foundation rtk cargo test -p ploy-runner -- --nocapture
CARGO_TARGET_DIR=/tmp/ploy-rtds-foundation rtk cargo check -p ploy-runner -p ploy-strategy-bundles --all-targets
```

Expected: tests pass and the runner compiles cleanly for the new foundation slice.

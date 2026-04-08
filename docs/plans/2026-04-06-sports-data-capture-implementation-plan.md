# Sports Discovery, Data Capture, And Backtest-Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Polymarket sports live-state capture and persistence so sports markets can be discovered, recorded, and replayed for research/backtest use without enabling sports live execution yet.

**Architecture:** Add a dedicated sports feed client for the Polymarket Sports WebSocket, normalize live game-state messages into canonical internal events, persist them to a sports-state table, and make the events recordable in replay logs. This phase ends at discovery + data capture + backtest support, not live trading.

**Tech Stack:** Rust, WebSocket client code in `ploy-runner`, SQLx migrations, canonical `MarketUpdate` recording/replay, fixture-driven tests

---

### Task 1: Add a canonical sports-state event contract

**Files:**
- Modify: `crates/ploy-strategy-bundles/src/traits.rs`
- Modify: `crates/ploy-strategy-bundles/src/engine.rs`
- Modify: `crates/ploy-strategy-bundles/src/feed/recorded.rs`
- Modify: `crates/ploy-strategy-bundles/src/feed/historical.rs`
- Modify: `crates/ploy-strategy-bundles/src/lib.rs`

- [ ] Add a `MarketUpdate::SportsState` variant carrying:
  - `game_id`
  - `league`
  - `slug`
  - `home_team`
  - `away_team`
  - `status`
  - `period`
  - `score`
  - `elapsed`
  - `live`
  - `ended`
  - `finished_at`
  - event timestamp
- [ ] Make the runtime and recorded feed paths accept and preserve the new variant without affecting existing crypto strategy behavior.
- [ ] Add unit coverage proving `SportsState` survives record/replay round-trips.

Run:

```bash
CARGO_TARGET_DIR=/tmp/ploy-sports-data rtk cargo test -p ploy-strategy-bundles recorded -- --nocapture
```

Expected: the recorded-feed tests pass with the new sports event variant.

### Task 2: Add the live Polymarket Sports WebSocket client

**Files:**
- Create: `apps/ploy-runner/src/sports_feed.rs`
- Modify: `apps/ploy-runner/src/main.rs`
- Modify: `apps/ploy-runner/Cargo.toml`
- Modify: `apps/ploy-runner/src/discovery/sports.rs`

- [ ] Add a sports feed module that connects to `wss://sports-api.polymarket.com/ws`, handles `ping`/`pong`, and parses `sport_result` payloads.
- [ ] Normalize payloads into `MarketUpdate::SportsState`.
- [ ] Attach the feed to the runner only when sports market families are configured.
- [ ] Join sports-state messages to normalized sports market descriptors by slug and league.

Run:

```bash
CARGO_TARGET_DIR=/tmp/ploy-sports-data rtk cargo check -p ploy-runner --all-targets
```

Expected: `ploy-runner` compiles with the new sports feed module.

### Task 3: Persist sports state for historical replay

**Files:**
- Create: `migrations/029_sports_state_events.sql`
- Modify: `apps/ploy-runner/src/sports_feed.rs`
- Modify: `crates/ploy-strategy-bundles/src/feed/database.rs`

- [ ] Add a `sports_state_events` table storing the normalized sports game-state timeline.
- [ ] Persist each normalized sports-state message with explicit source and received timestamps.
- [ ] Add a historical loader helper in `feed/database.rs` that can read sports-state rows back into `MarketUpdate::SportsState`.
- [ ] Do not yet turn sports-state loading on by default for all backtests; the backtest-expansion phase will wire that policy.

Run:

```bash
CARGO_TARGET_DIR=/tmp/ploy-sports-data rtk cargo check -p ploy-runner -p ploy-strategy-bundles --all-targets
```

Expected: both crates compile with sports-state persistence support.

### Task 4: Add fixture-backed sports ingestion tests

**Files:**
- Create: `apps/ploy-runner/tests/sports_feed.rs`
- Create: `apps/ploy-runner/tests/fixtures/polymarket_sports_ws.jsonl`
- Modify: `crates/ploy-strategy-bundles/tests/backtest_integration.rs`

- [ ] Add fixture messages that cover:
  - scheduled
  - in-progress
  - break/halftime
  - final/ended
- [ ] Prove the sports feed parser converts them into canonical `SportsState` updates.
- [ ] Add one integration test that records a short mixed crypto+sports stream and replays it successfully.

Run:

```bash
CARGO_TARGET_DIR=/tmp/ploy-sports-data rtk cargo test -p ploy-runner sports_feed -- --nocapture
CARGO_TARGET_DIR=/tmp/ploy-sports-data rtk cargo test -p ploy-strategy-bundles backtest_integration -- --nocapture
```

Expected: parser and replay-parity tests pass.

### Task 5: Document the sports boundary explicitly

**Files:**
- Modify: `tasks/todo.md`
- Modify: `docs/plans/2026-04-06-polymarket-expansion-master-plan.md`
- Modify: `README.md`

- [ ] Record that this phase covers sports discovery, data capture, and backtest support only.
- [ ] Add a brief README note that sports live execution remains out of scope until lifecycle safeguards are added later.
- [ ] Capture the verification commands and results in `tasks/todo.md`.

Run:

```bash
rg -n "sports.*discovery|sports.*data capture|sports.*backtest support" README.md docs/plans tasks/todo.md
```

Expected: the scope boundary appears in the tracker and planning docs.

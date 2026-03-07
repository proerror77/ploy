# Crypto 5m Repricing V1 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build a backtestable, live-ready baseline framework for Polymarket 5-minute crypto repricing trades.

**Architecture:** Add a new dedicated strategy module instead of mutating the existing directional momentum engine. Reuse `HistoricalFeed`, `FeeModel`, and `ExecutionSimulator`, but model entries/exits around early repricing windows, fair-gap versus PM quotes, and Binance L2 confirmation.

**Tech Stack:** Rust, `chrono`, `rust_decimal`, existing `HistoricalFeed`, `FeeModel`, `ExecutionSimulator`, CLI wiring in `clap`.

---

### Task 1: Add the new strategy core types

**Files:**
- Create: `src/strategy/crypto_repricing.rs`
- Modify: `src/strategy/mod.rs`
- Test: `src/strategy/crypto_repricing.rs`

**Step 1: Write the failing test**

Add unit tests that construct minimal event state and assert:

- the trade window only opens between `240s` and `75s`
- the hard-flat threshold triggers at `45s`
- cost buffer logic blocks entries when fair-gap is too small

**Step 2: Run test to verify it fails**

Run: `cargo test crypto_repricing::tests -- --nocapture`
Expected: FAIL because the module does not exist yet.

**Step 3: Write minimal implementation**

Define:

- config for event windows, liquidity filters, fair-gap threshold, and direction threshold
- event state and feature snapshot structs
- shared helpers for realized-vol estimate, fair probability, direction score, and gap computation

**Step 4: Run test to verify it passes**

Run: `cargo test crypto_repricing::tests -- --nocapture`
Expected: PASS for core helper behavior.

**Step 5: Commit**

```bash
git add src/strategy/crypto_repricing.rs src/strategy/mod.rs
git commit -m "strategy: add crypto repricing core"
```

### Task 2: Build the backtest engine

**Files:**
- Create: `src/strategy/crypto_repricing_backtest.rs`
- Modify: `src/strategy/mod.rs`
- Test: `src/strategy/crypto_repricing_backtest.rs`

**Step 1: Write the failing test**

Add replay tests that feed synthetic `HistoricalFeed` updates and assert:

- no entry before `T-240s`
- entry happens inside the main window when gap and Binance L2 agree
- exit occurs by `T-45s` even without settlement

**Step 2: Run test to verify it fails**

Run: `cargo test crypto_repricing_backtest::tests -- --nocapture`
Expected: FAIL because the backtest engine does not exist yet.

**Step 3: Write minimal implementation**

Implement a replay engine that:

- tracks 5-minute event metadata from `EventState`
- updates Binance price/L2/PM quote state from `HistoricalFeed`
- computes fair-gap and direction filters
- executes entries/exits with `ExecutionSimulator`
- records trades and summary stats

**Step 4: Run test to verify it passes**

Run: `cargo test crypto_repricing_backtest::tests -- --nocapture`
Expected: PASS with deterministic replay behavior.

**Step 5: Commit**

```bash
git add src/strategy/crypto_repricing_backtest.rs src/strategy/mod.rs
git commit -m "strategy: add crypto repricing backtest engine"
```

### Task 3: Wire the CLI entrypoint

**Files:**
- Modify: `src/cli/strategy.rs`
- Modify: `src/strategy/mod.rs`
- Test: existing strategy CLI tests if present, otherwise module-level smoke coverage

**Step 1: Write the failing test**

Add or extend a CLI parsing test to prove a new backtest name resolves to the new engine.

**Step 2: Run test to verify it fails**

Run: `cargo test strategy:: -- --nocapture`
Expected: FAIL because the CLI does not know the new backtest name.

**Step 3: Write minimal implementation**

Wire a new backtest selector such as `crypto-repricing` / `crypto_repricing` and map it to the new engine/config.

**Step 4: Run test to verify it passes**

Run: `cargo test strategy:: -- --nocapture`
Expected: PASS and the new backtest path is selectable.

**Step 5: Commit**

```bash
git add src/cli/strategy.rs src/strategy/mod.rs
git commit -m "cli: wire crypto repricing backtest"
```

### Task 4: Validate with targeted replay

**Files:**
- Modify: `tasks/todo.md`

**Step 1: Run targeted tests**

Run:

```bash
cargo test crypto_repricing::tests -- --nocapture
cargo test crypto_repricing_backtest::tests -- --nocapture
```

Expected: PASS

**Step 2: Run compile check for CLI path**

Run:

```bash
cargo test strategy:: -- --nocapture
```

Expected: PASS or only unrelated filtered tests skipped.

**Step 3: Document results**

Update `tasks/todo.md` review checkboxes and note any live-readiness gaps left intentionally out of v1.

**Step 4: Commit**

```bash
git add tasks/todo.md
git commit -m "docs: record crypto repricing validation"
```

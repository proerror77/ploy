# Backtest And Replay Expansion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Expand the historical and replay backtest stack so it can consume the new reference-price and sports-state datasets while preserving existing crypto research behavior and keeping settlement data correctly backfillable.

**Architecture:** Extend the database loader in `ploy-strategy-bundles` to read the newly captured `reference_price_ticks`, normalized market catalog rows, and `sports_state_events`, then thread those updates through historical and replay modes. Preserve replay as the gold-standard parity path, keep historical source-ranking explicit instead of silently trusting every new table, and retain an official settlement completion/backfill path so missing outcomes can be repaired deterministically.

**Tech Stack:** Rust, SQLx query helpers, canonical `MarketUpdate` replay, `ploy-research`, `ploy-strategy-bundles` integration tests

---

### Task 1: Add historical loader support for new sources

**Files:**
- Modify: `crates/ploy-strategy-bundles/src/feed/database.rs`
- Modify: `crates/ploy-strategy-bundles/src/feed/mod.rs`
- Modify: `crates/ploy-strategy-bundles/src/feed/historical.rs`
- Modify: `crates/ploy-strategy-bundles/src/config.rs`

- [ ] Add loader helpers for:
  - `reference_price_ticks`
  - `sports_state_events`
  - normalized market catalog reads where needed for context
- [ ] Keep `pm_token_settlements` and any future settlement-completion tables as the source of truth for resolved outcomes; do not regress to inferred-only settlement.
- [ ] Add config flags that let a backtest opt into:
  - crypto-only
  - crypto + non-crypto reference data
  - crypto + sports state
- [ ] Preserve the existing trusted-source policy for PM quotes instead of loosening it in this phase.

Run:

```bash
CARGO_TARGET_DIR=/tmp/ploy-backtest-expansion rtk cargo check -p ploy-strategy-bundles --all-targets
```

Expected: the strategy-bundles crate compiles with the expanded loader path.

### Task 2: Make replay mode understand the expanded event stream

**Files:**
- Modify: `crates/ploy-strategy-bundles/src/feed/recorded.rs`
- Modify: `crates/ploy-strategy-bundles/src/engine.rs`
- Modify: `apps/ploy-runner/src/main.rs`

- [ ] Ensure recorded NDJSON logs include the new sports-state and any new metadata-bearing updates in stable sequence order.
- [ ] Keep replay mode deterministic when mixed feed families appear in one capture.
- [ ] Add one runner-level smoke path that can replay a mixed captured session without requiring live network access.

Run:

```bash
CARGO_TARGET_DIR=/tmp/ploy-backtest-expansion rtk cargo test -p ploy-strategy-bundles recorded_updates_replay_to_the_same_runtime_result -- --exact --nocapture
```

Expected: replay parity still passes after the event-stream expansion.

### Task 3: Add research-facing backtest coverage for sports and non-crypto reference data

**Files:**
- Modify: `crates/ploy-research/src/backtesting.rs`
- Modify: `crates/ploy-research/src/replay.rs`
- Create: `crates/ploy-strategy-bundles/tests/reference_price_backtest.rs`
- Create: `crates/ploy-strategy-bundles/tests/sports_backtest.rs`

- [ ] Add fixture-backed integration tests showing that:
  - non-crypto reference ticks load and remain time-ordered
  - sports-state updates load and replay
  - existing crypto scenarios still pass unchanged
- [ ] Add at least one regression test proving a run with initially missing settlement rows can be repaired once official settlement rows are backfilled.
- [ ] Keep the tests focused on feed correctness and runtime stability, not on inventing a sports strategy in this phase.
- [ ] Expose simple research helpers in `ploy-research` for replaying fills from the expanded feed world without changing its public contract unnecessarily.

Run:

```bash
CARGO_TARGET_DIR=/tmp/ploy-backtest-expansion rtk cargo test -p ploy-strategy-bundles -- --nocapture
CARGO_TARGET_DIR=/tmp/ploy-backtest-expansion rtk cargo test -p ploy-research -- --nocapture
```

Expected: both crates pass their focused backtest/replay coverage.

### Task 4: Add reference-data and sports examples for operator iteration

**Files:**
- Modify: `crates/ploy-strategy-bundles/examples/run_backtest.rs`
- Modify: `crates/ploy-strategy-bundles/examples/optimize_backtest.rs`
- Create: `config/strategies/reference-data.backtest.toml`
- Create: `config/strategies/sports-observation.backtest.toml`

- [ ] Add one example config that demonstrates loading non-crypto reference data in backtest mode.
- [ ] Add one example config that demonstrates sports-state-inclusive replay/backtest mode.
- [ ] Keep the examples observational unless a corresponding strategy actually consumes the extra inputs.

Run:

```bash
CARGO_TARGET_DIR=/tmp/ploy-backtest-expansion rtk cargo check -p ploy-strategy-bundles --examples
```

Expected: examples compile with the new config surface.

### Task 5: Record trust rules and verification results

**Files:**
- Modify: `tasks/todo.md`
- Modify: `docs/plans/2026-04-06-polymarket-expansion-master-plan.md`

- [ ] Document which historical sources are trusted by default after this phase.
- [ ] Record the verification matrix outcomes in `tasks/todo.md`.
- [ ] Leave explicit notes for the later hardening/trust-cutover phase instead of silently broadening trust.

Run:

```bash
rg -n "trusted|reference_price_ticks|sports_state_events" tasks/todo.md docs/plans/2026-04-06-polymarket-expansion-master-plan.md
```

Expected: the tracker and master plan mention the new source-ranking rules.

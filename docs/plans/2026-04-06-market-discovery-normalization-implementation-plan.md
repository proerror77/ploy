# Market Discovery And Metadata Normalization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the current crypto-only market scanning heuristics with a normalized market-discovery layer that can describe crypto and sports PM markets consistently.

**Architecture:** Introduce an explicit discovery subsystem in `ploy-runner` and a normalized market descriptor persisted in the database. Keep the current crypto scan behavior as the first adapter, then add sports-aware Gamma discovery using `/sports`, `/teams`, and `/sports/market-types`. Upgrade the sidecar tools to consume the same normalized shape instead of title-only event search.

**Tech Stack:** Rust, vendored Gamma client, TypeScript sidecar tools, SQLx migrations, existing runner scanner flow, focused `rtk cargo check` and `npm` build checks

---

### Task 1: Create the normalized market-catalog contract

**Files:**
- Create: `apps/ploy-runner/src/discovery/mod.rs`
- Create: `apps/ploy-runner/src/discovery/types.rs`
- Create: `migrations/028_pm_market_catalog.sql`
- Modify: `apps/ploy-runner/src/main.rs`
- Modify: `tasks/todo.md`

- [ ] Add a market descriptor type that records:
  - market family
  - event ID
  - market ID
  - slug
  - symbol/reference symbol
  - settlement source
  - league or asset family
  - start/end times
  - token IDs
  - market semantics (`updown`, `moneyline`, `yesno`, etc.)
- [ ] Add a `pm_market_catalog` table that stores the normalized descriptor alongside the raw Gamma payload.
- [ ] Wire the new discovery module into `main.rs` without deleting the legacy scanner entrypoint yet.

Run:

```bash
rg -n "pm_market_catalog|MarketFamily|MarketDescriptor" apps/ploy-runner/src/discovery migrations
```

Expected: the new descriptor and migration names exist in the planned locations.

### Task 2: Migrate the crypto window scanner onto the new discovery subsystem

**Files:**
- Create: `apps/ploy-runner/src/discovery/crypto.rs`
- Modify: `apps/ploy-runner/src/scanner.rs`
- Modify: `apps/ploy-runner/src/discovery/mod.rs`
- Modify: `apps/ploy-runner/src/feeds.rs`

- [ ] Move the current crypto discovery logic into `discovery/crypto.rs`.
- [ ] Replace question-string-only symbol inference with a normalization helper that emits an explicit reference symbol and settlement source.
- [ ] Persist discovered crypto markets into `pm_market_catalog` in addition to the existing compatibility metadata table.
- [ ] Keep emitting the existing `EventDiscovered` compatibility path so current crypto runtime behavior does not break during this phase.

Run:

```bash
CARGO_TARGET_DIR=/tmp/ploy-discovery rtk cargo check -p ploy-runner --all-targets
```

Expected: the runner compiles with the crypto adapter living under the new discovery subsystem.

### Task 3: Add sports-aware Gamma discovery

**Files:**
- Create: `apps/ploy-runner/src/discovery/sports.rs`
- Modify: `vendor/polymarket-client-sdk/src/gamma/client.rs`
- Modify: `vendor/polymarket-client-sdk/tests/gamma.rs`
- Modify: `apps/ploy-runner/src/discovery/types.rs`

- [ ] Add a sports discovery adapter that uses Gamma sports endpoints instead of title-only search.
- [ ] Normalize league/team metadata into the market descriptor so sports markets can be joined later to live sports state.
- [ ] Add focused vendored Gamma tests or runner-level tests that prove sports metadata requests remain stable.
- [ ] Persist sports descriptors to `pm_market_catalog` even before sports live-state ingestion exists.

Run:

```bash
CARGO_TARGET_DIR=/tmp/ploy-discovery rtk cargo test -p polymarket-client-sdk gamma -- --nocapture
CARGO_TARGET_DIR=/tmp/ploy-discovery rtk cargo check -p ploy-runner --all-targets
```

Expected: Gamma coverage passes and the runner still compiles.

### Task 4: Upgrade the sidecar Polymarket tools to consume normalized discovery

**Files:**
- Modify: `ploy-sidecar/src/tools/polymarket.ts`
- Modify: `ploy-sidecar/src/index.ts`
- Modify: `ploy-sidecar/src/schemas/output.ts`
- Modify: `ploy-sidecar/package.json`

- [ ] Replace the current `events?title=...` search-only tool with:
  - family-aware search
  - sports lookup by team/league/slug
  - snapshot enrichment that reports normalized market metadata
- [ ] Keep the existing simple search behavior available as a fallback mode for unstructured operator prompts.
- [ ] Update the sidecar output schema so downstream agent prompts can reference market family and settlement source explicitly.

Run:

```bash
cd ploy-sidecar && npm run build
```

Expected: the sidecar builds successfully with the new Polymarket tool contract.

### Task 5: Validate discovery parity and capture the new workflow

**Files:**
- Modify: `tasks/todo.md`
- Modify: `docs/plans/2026-04-06-polymarket-expansion-master-plan.md`

- [ ] Run focused checks for runner compile health and sidecar build health.
- [ ] Smoke one crypto query and one sports query through the new normalized discovery path.
- [ ] Record the verification commands and outcomes in `tasks/todo.md`.

Run:

```bash
CARGO_TARGET_DIR=/tmp/ploy-discovery rtk cargo check -p ploy-runner --all-targets
cd ploy-sidecar && npm run build
```

Expected: both build steps pass and the new discovery surfaces are documented in the tracker.

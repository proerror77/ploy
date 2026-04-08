# CLI And Operator Surface Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add market-discovery and feed-health inspection surfaces to `ployctl` and the sidecar so operators can understand the expanded PM data plane.

**Architecture:** Keep `ployctl` aligned with the control-plane model by adding new read-only modules for markets and feeds, backed by runtime snapshots or HTTP endpoints where available. In parallel, upgrade the sidecar Polymarket tool so agents can perform family-aware search and snapshots for crypto and sports without raw API spelunking.

**Tech Stack:** Rust CLI parsing, existing `ControlPlaneClient`, TypeScript sidecar tool code, snapshot-style tests, `npm` build checks

---

### Task 1: Add `ployctl` command parsing and renderer modules for markets and feeds

**Files:**
- Modify: `apps/ployctl/src/main.rs`
- Modify: `apps/ployctl/src/lib.rs`
- Create: `apps/ployctl/src/markets.rs`
- Create: `apps/ployctl/src/feeds.rs`

- [ ] Add `ployctl markets list`, `ployctl markets inspect <slug-or-id>`, and `ployctl feeds status` commands.
- [ ] Keep the parser style consistent with the existing manual enum/`match` approach in `main.rs`.
- [ ] Implement renderers that degrade cleanly when snapshots are absent instead of panicking.

Run:

```bash
CARGO_TARGET_DIR=/tmp/ploy-cli-expansion rtk cargo test -p ployctl -- --nocapture
```

Expected: `ployctl` parser and renderer tests pass with the new commands.

### Task 2: Extend the control-plane client with market/feed snapshot readers

**Files:**
- Modify: `apps/ployctl/src/client.rs`
- Modify: `apps/ployctl/src/system.rs`
- Modify: `apps/ployctl/src/trading.rs`
- Create: `apps/ployctl/tests/fixtures/markets.json`
- Create: `apps/ployctl/tests/fixtures/feeds.json`

- [ ] Add client helpers that can read market/feed snapshots either from HTTP endpoints or local runtime snapshot files.
- [ ] Reuse the existing error-shaping conventions from `system` and `deployments`.
- [ ] Add tests proving the client returns structured errors instead of empty success when the new snapshots are missing or malformed.

Run:

```bash
CARGO_TARGET_DIR=/tmp/ploy-cli-expansion rtk cargo test -p ployctl client -- --nocapture
```

Expected: client snapshot tests pass for the new market/feed readers.

### Task 3: Upgrade the sidecar Polymarket tools for family-aware lookup

**Files:**
- Modify: `ploy-sidecar/src/tools/polymarket.ts`
- Modify: `ploy-sidecar/src/index.ts`
- Modify: `ploy-sidecar/src/schemas/output.ts`
- Modify: `ploy-sidecar/README.md`

- [ ] Add tool arguments for family, league, team, and normalized symbol filters where relevant.
- [ ] Make `market_snapshot` return discovery metadata, feed/source metadata, and sports linkage when available.
- [ ] Update the sidecar prompt wiring so operator guidance points at the richer tool surface.

Run:

```bash
cd ploy-sidecar && npm run build
```

Expected: the sidecar builds with the upgraded tool surface.

### Task 4: Add docs and snapshot-style CLI coverage

**Files:**
- Modify: `README.md`
- Modify: `docs/plans/2026-04-06-polymarket-expansion-master-plan.md`
- Modify: `tasks/todo.md`

- [ ] Document the new `ployctl` commands and what they are intended to answer.
- [ ] Add tracker notes showing which snapshots or endpoints the new commands depend on.
- [ ] Record build/test verification for both `ployctl` and `ploy-sidecar`.

Run:

```bash
rg -n "ployctl markets|ployctl feeds status|family-aware" README.md docs/plans tasks/todo.md
```

Expected: the new operator surface is documented in the expected files.

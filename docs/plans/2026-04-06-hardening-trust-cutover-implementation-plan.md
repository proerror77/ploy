# Hardening And Trust-Cutover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add source-health metrics, trust policies, and runbook updates so the expanded PM data plane can be operated safely after the earlier implementation phases land.

**Architecture:** Build on the earlier phases rather than mixing hardening into the initial feature slices. Add explicit source-ranking policy, freshness metrics, and operator runbooks that define when new feeds and market families are considered trusted for historical research and operational use.

**Tech Stack:** Rust metrics/snapshot code, docs/runbooks, `ployctl` status surfaces, existing tracker workflow

---

### Task 1: Add explicit trust-policy configuration and documentation

**Files:**
- Modify: `config/default.toml`
- Create: `docs/runbooks/polymarket-data-trust-policy.md`
- Modify: `docs/plans/2026-04-06-polymarket-expansion-master-plan.md`
- Modify: `tasks/todo.md`

- [ ] Add config documentation for:
  - trusted quote sources
  - trusted reference sources
  - sports-state trust windows
  - carried-forward price handling
- [ ] Write a runbook that defines how a newly added source graduates from observed to trusted.
- [ ] Link the trust-policy runbook from the master plan and tracker.

Run:

```bash
rg -n "trust" config/default.toml docs/runbooks/polymarket-data-trust-policy.md tasks/todo.md
```

Expected: trust-policy terms appear in the config and runbook files.

### Task 2: Add health/freshness projections for new feed families

**Files:**
- Modify: `apps/ployctl/src/system.rs`
- Modify: `apps/ployctl/src/feeds.rs`
- Modify: `apps/ployctl/src/client.rs`
- Modify: `tasks/todo.md`

- [ ] Add feed-health views covering:
  - Chainlink crypto
  - Binance crypto
  - Pyth non-crypto
  - Sports WebSocket
- [ ] Surface freshness, carried-forward state, and stale-source warnings in the operator output.
- [ ] Keep the output concise enough for terminal use.

Run:

```bash
CARGO_TARGET_DIR=/tmp/ploy-hardening rtk cargo test -p ployctl -- --nocapture
```

Expected: `ployctl` tests still pass after the health projection expansion.

### Task 3: Add deployment and backtest runbooks for the expanded data plane

**Files:**
- Modify: `README.md`
- Create: `docs/runbooks/polymarket-reference-feeds.md`
- Create: `docs/runbooks/polymarket-sports-capture.md`
- Create: `docs/runbooks/polymarket-backtest-data-sources.md`

- [ ] Document how to enable and observe:
  - Pyth reference capture
  - sports-state capture
  - backtest source selection
- [ ] Document the explicit boundary that sports execution remains out of scope until later safeguards land.
- [ ] Link all runbooks from the README and the master plan.

Run:

```bash
rg -n "reference feeds|sports capture|backtest data sources" README.md docs/runbooks docs/plans/2026-04-06-polymarket-expansion-master-plan.md
```

Expected: the new runbooks are linked from README and the master plan.

### Task 4: Capture the final verification matrix and rollout notes

**Files:**
- Modify: `tasks/todo.md`

- [ ] Record the exact validation commands that must pass before trusting the expanded surface in everyday use.
- [ ] Add a short rollout checklist covering:
  - schema migration order
  - capture dry-run checks
  - replay/backtest parity checks
  - operator CLI checks
- [ ] Leave unresolved production enablement questions as explicit follow-ups rather than silent assumptions.

Run:

```bash
/opt/homebrew/bin/rtk read tasks/todo.md
```

Expected: the tracker contains the rollout notes and verification matrix at the top.

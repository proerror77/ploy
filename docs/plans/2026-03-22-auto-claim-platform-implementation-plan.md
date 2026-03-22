# Auto-Claim Platform Hardening Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add default-on account-level Polymarket auto-claim to the workspace platform and harden the operator surfaces around claim health, alerts, and auth.

**Architecture:** Extend the new workspace platform rather than reviving the legacy single-binary claimer path. `ploy-connectivity` will own claim primitives, `ploy-platform` will own account claim state, `ployd` will own the claim loop and health propagation, and operator clients will consume shared control-plane contracts.

**Tech Stack:** Rust workspace crates, `ployd`, `ployctl`, `ploytui`, shared Serde contracts, existing audit/rate-limit/health patterns, focused `rtk cargo test` validation.

---

### Task 1: Add claim contracts and account claim state

**Files:**
- Modify: `crates/ploy-operator-contracts/src/lib.rs`
- Create: `crates/ploy-operator-contracts/src/claims.rs`
- Modify: `crates/ploy-platform/src/lib.rs`
- Modify: `crates/ploy-platform/src/accounts.rs`
- Test: `crates/ploy-operator-contracts/src/claims.rs`
- Test: `crates/ploy-platform/src/accounts.rs`

**Step 1: Write the failing tests**

Add tests that assert stable wire keys and account claim status defaults.

**Step 2: Run tests to verify they fail**

Run: `rtk cargo test -p ploy-operator-contracts -p ploy-platform`
Expected: FAIL because claim contracts/state do not exist yet.

**Step 3: Write minimal implementation**

Add:

- `AccountClaimStatus`
- `RedeemablePositionSnapshot`
- `ClaimExecutionRecord`
- claim status collection helpers in `ploy-platform::accounts`

**Step 4: Run tests to verify they pass**

Run: `rtk cargo test -p ploy-operator-contracts -p ploy-platform`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/ploy-operator-contracts/src/lib.rs crates/ploy-operator-contracts/src/claims.rs crates/ploy-platform/src/lib.rs crates/ploy-platform/src/accounts.rs
git commit -m "contracts: add account claim state"
```

### Task 2: Add Polymarket redeem primitives to connectivity

**Files:**
- Modify: `crates/ploy-connectivity/src/lib.rs`
- Test: `crates/ploy-connectivity/src/lib.rs`

**Step 1: Write the failing tests**

Add tests for:

- redeem gateway request validation
- successful redeem response mapping
- retry-safe error mapping

**Step 2: Run tests to verify they fail**

Run: `rtk cargo test -p ploy-connectivity`
Expected: FAIL because no redeem primitive exists yet.

**Step 3: Write minimal implementation**

Add account-level redeem request/response primitives and a gateway method that
wraps the vendored Polymarket SDK redeem path.

**Step 4: Run tests to verify they pass**

Run: `rtk cargo test -p ploy-connectivity`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/ploy-connectivity/src/lib.rs
git commit -m "connectivity: add polymarket redeem gateway"
```

### Task 3: Add account auto-claim loop to ployd

**Files:**
- Modify: `apps/ployd/src/runtime.rs`
- Modify: `apps/ployd/src/config.rs`
- Test: `apps/ployd/src/runtime.rs`

**Step 1: Write the failing tests**

Add tests for:

- live accounts default to enabled auto-claim
- detected redeemables trigger immediate claim
- failures degrade account/system status and schedule retry

**Step 2: Run tests to verify they fail**

Run: `rtk cargo test -p ployd`
Expected: FAIL because claim loop state is missing.

**Step 3: Write minimal implementation**

Add:

- per-account claim loop state
- scan/claim scheduling
- backoff/retry state
- system/account degraded/recovering transitions

**Step 4: Run tests to verify they pass**

Run: `rtk cargo test -p ployd`
Expected: PASS

**Step 5: Commit**

```bash
git add apps/ployd/src/runtime.rs apps/ployd/src/config.rs
git commit -m "runtime: add account auto-claim loop"
```

### Task 4: Add claim API and CLI surface

**Files:**
- Modify: `apps/ployd/src/http.rs`
- Modify: `apps/ployctl/src/client.rs`
- Modify: `apps/ployctl/src/main.rs`
- Create: `apps/ployctl/src/claims.rs`
- Test: `apps/ployd/src/http.rs`
- Test: `apps/ployctl/src/main.rs`

**Step 1: Write the failing tests**

Add tests for:

- `GET /api/accounts/claims`
- `GET /api/accounts/:id/claims`
- `POST /api/accounts/:id/claims/run|rescan|pause|resume`
- CLI parsing/rendering for `ployctl claims ...`

**Step 2: Run tests to verify they fail**

Run: `rtk cargo test -p ployd -p ployctl`
Expected: FAIL because claim routes/commands do not exist.

**Step 3: Write minimal implementation**

Expose claim read/write API and matching `ployctl` commands.

**Step 4: Run tests to verify they pass**

Run: `rtk cargo test -p ployd -p ployctl`
Expected: PASS

**Step 5: Commit**

```bash
git add apps/ployd/src/http.rs apps/ployctl/src/client.rs apps/ployctl/src/main.rs apps/ployctl/src/claims.rs
git commit -m "control-plane: add claim operator surface"
```

### Task 5: Add TUI and frontend claim visibility

**Files:**
- Modify: `apps/ploytui/src/lib.rs`
- Modify: `apps/ploytui/src/main.rs`
- Modify: `ploy-frontend/src/services/api.ts`
- Modify: `ploy-frontend/src/types/index.ts`
- Modify: `ploy-frontend/src/pages/SystemControl.tsx`
- Modify: `ploy-frontend/src/pages/StrategyMonitor.tsx`
- Test: `apps/ploytui`
- Test: `ploy-frontend` build/lint

**Step 1: Write the failing tests**

Add focused TUI unit tests if needed; for frontend rely on type/build failures.

**Step 2: Run test/build to verify it fails**

Run:
- `rtk cargo test -p ploytui`
- `cd ploy-frontend && npm run build`

Expected: FAIL because claim data is not rendered yet.

**Step 3: Write minimal implementation**

Add claim status panel/sections and claim actions in operator clients.

**Step 4: Run tests/build to verify it passes**

Run:
- `rtk cargo test -p ploytui`
- `cd ploy-frontend && npm run build && npm run lint`

Expected: PASS

**Step 5: Commit**

```bash
git add apps/ploytui/src/lib.rs apps/ploytui/src/main.rs ploy-frontend/src/services/api.ts ploy-frontend/src/types/index.ts ploy-frontend/src/pages/SystemControl.tsx ploy-frontend/src/pages/StrategyMonitor.tsx
git commit -m "operator: surface account claim status"
```

### Task 6: Add claim metrics, audit, and auth scopes

**Files:**
- Modify: `crates/ploy-operator-contracts/src/system.rs`
- Modify: `apps/ployd/src/http.rs`
- Modify: `apps/ployd/src/events.rs`
- Modify: `apps/ployd/src/runtime.rs`
- Modify: `docs/runbooks/platform-startup.md`
- Modify: `docs/runbooks/platform-deploy.md`
- Test: `apps/ployd/src/http.rs`
- Test: `apps/ployd/src/runtime.rs`

**Step 1: Write the failing tests**

Add tests for:

- claim metrics included in system status/events
- claim actions rejected for readonly auth
- audit log records claim actions

**Step 2: Run tests to verify they fail**

Run: `rtk cargo test -p ployd -p ploy-operator-contracts`
Expected: FAIL because metrics/scope wiring is incomplete.

**Step 3: Write minimal implementation**

Add claim metrics to system snapshots, emit claim events, write audit records,
and enforce claim actions under write/admin auth only.

**Step 4: Run tests to verify they pass**

Run: `rtk cargo test -p ployd -p ploy-operator-contracts`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/ploy-operator-contracts/src/system.rs apps/ployd/src/http.rs apps/ployd/src/events.rs apps/ployd/src/runtime.rs docs/runbooks/platform-startup.md docs/runbooks/platform-deploy.md
git commit -m "platform: harden claim health and auth"
```

### Task 7: Retire legacy claimer path and stale docs

**Files:**
- Modify or delete legacy claimer entrypoints under `src/` if still present on the branch
- Modify: relevant docs/runbooks that still reference old claimer paths
- Test: `tests/workspace_runtime_retirement.rs` or equivalent retirement guards

**Step 1: Write the failing test**

Add or extend retirement guard so the old claimer path cannot quietly become the
default again.

**Step 2: Run test to verify it fails**

Run: `rtk cargo test -p ploy --test workspace_runtime_retirement`
Expected: FAIL if stale root runtime claim paths are still canonical.

**Step 3: Write minimal implementation**

Archive or retire stale legacy claim entrypoints and update docs so `ployd` is
the only canonical claim path.

**Step 4: Run test to verify it passes**

Run: `rtk cargo test -p ploy --test workspace_runtime_retirement`
Expected: PASS

**Step 5: Commit**

```bash
git add src docs tests
git commit -m "repo: retire legacy claimer entrypoints"
```

### Task 8: Run integration validation and ship branch

**Files:**
- Modify: `tasks/todo.md`

**Step 1: Run focused validation**

Run:

```bash
rtk cargo test -p ploy-connectivity -p ploy-platform -p ploy-operator-contracts -p ployd -p ployctl -p ploytui
rtk cargo test -p ploy --test platform_smoke --test platform_release_workflow --test workflow_security --test workspace_runtime_retirement
cd ploy-frontend && npm run build && npm run lint
```

Expected: PASS

**Step 2: Update tracked progress**

Record completed work and validation in `tasks/todo.md`.

**Step 3: Push branch**

```bash
rtk git push origin HEAD
```

**Step 4: Commit if docs/tracker changed**

```bash
git add tasks/todo.md
git commit -m "docs: record auto-claim hardening validation"
```

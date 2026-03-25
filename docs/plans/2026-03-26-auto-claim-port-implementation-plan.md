# Auto-Claim Mainline Port Implementation Plan

Date: 2026-03-26
Branch: `session/auto-claim-port`

## Scope

Port the still-unmerged auto-claim functionality from `session/auto-claim-hardening` to latest `main`, while leaving already-merged observability, runtime-mode, and documentation work untouched.

## Work phases

### Phase 1: Contract and platform state

Files:

- `crates/ploy-operator-contracts/src/claims.rs`
- `crates/ploy-operator-contracts/src/lib.rs`
- `crates/ploy-platform/src/accounts.rs`
- `crates/ploy-platform/src/control_plane.rs`
- `crates/ploy-platform/src/lib.rs`

Steps:

1. Add the claims wire contract module and re-export it.
2. Replace the `AccountSnapshot`-only account state with the branch-proven claim registry.
3. Update platform tests to cover account claim state transitions.

Verification:

- `rtk cargo test -p ploy-operator-contracts -p ploy-platform`

### Phase 2: Connectivity seam

Files:

- `crates/ploy-connectivity/src/lib.rs`

Steps:

1. Port the relay-backed claim gateway types and default implementation.
2. Keep the surface minimal: discovery plus claim execution only.
3. Avoid pulling in unrelated branch-only changes already present on main.

Verification:

- `rtk cargo test -p ploy-connectivity`

### Phase 3: Daemon runtime and HTTP surface

Files:

- `apps/ployd/src/config.rs`
- `apps/ployd/src/runtime.rs`
- `apps/ployd/src/http.rs`

Steps:

1. Add claim snapshot file support to config and runtime snapshot writes.
2. Sync live accounts from deployment registry into the account claim registry.
3. Port manual and automatic claim loops.
4. Expose claim read/write endpoints on the control plane.
5. Persist account claim detail to `account-claims.json`.

Verification:

- `rtk cargo test -p ployd`

### Phase 4: CLI operator surface

Files:

- `apps/ployctl/src/lib.rs`
- `apps/ployctl/src/client.rs`
- `apps/ployctl/src/main.rs`
- `apps/ployctl/src/claims.rs`

Steps:

1. Add claim client methods.
2. Add `ployctl claims` parsing and rendering.
3. Use snapshot fallback for read paths, matching existing control-plane behavior.

Verification:

- `rtk cargo test -p ployctl`

### Phase 5: Integration smoke

Files:

- `tests/platform_smoke.rs`

Steps:

1. Extend smoke coverage only where necessary to cover claim snapshot persistence or endpoint reachability.
2. Keep the smoke test lightweight and aligned with current mainline contracts.

Verification:

- `rtk cargo test --test platform_smoke`

## Stop conditions

Stop and re-evaluate if:

- the port requires reintroducing already-merged observability code
- the claim gateway cannot be isolated from unrelated branch changes
- current mainline auth scope assumptions conflict with claim write endpoints

## Done when

- `ployd` persists account claim state and exposes claim endpoints
- `ployctl claims ...` works
- relay-backed claim discovery and execution are present
- focused Rust tests pass
- no already-merged branch noise is reintroduced

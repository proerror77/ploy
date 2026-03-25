# Auto-Claim Mainline Port Design

Date: 2026-03-26
Branch: `session/auto-claim-port`
Base: `origin/main`

## Context

`origin/main` already absorbed the branch work for:

- platform metrics and alerts
- stale-source degradation
- finer auth scopes
- runtime-mode surfacing
- live deployment dry-run drill docs

The remaining unmerged product value from `session/auto-claim-hardening` is narrower:

- relay-backed account auto-claim for live accounts
- claims operator surface in `ployd` and `ployctl`

This port must land on the current workspace/control-plane runtime without re-importing already-merged observability or documentation deltas.

## Goal

Add account-level, relay-backed Polymarket auto-claim to the current mainline runtime, with:

- default-on auto-claim for live accounts
- account claim state persisted under `run/platform`
- `ployctl claims ...` operator commands
- HTTP control-plane endpoints for claim read/write actions
- snapshot-backed fallback reads, matching the rest of the control plane

## Non-goals

This port does not include:

- external alert delivery
- frontend/TUI claim dashboards unless already required by tests
- full old-branch documentation rewrites
- legacy claimer installer retirement
- changes to the old monolith `src/` runtime

## Desired behavior

### Runtime model

- Claims are tracked at the `account_id` level, not deployment level.
- A live account gets an auto-claim loop by default.
- Paper accounts do not claim and should report as not supported or paused.
- The claim loop scans redeemable positions and immediately executes claims.
- Claim failures degrade the account loop and schedule a retry with backoff.
- Claim failures do not kill the daemon.

### Control-plane surface

Read endpoints:

- `GET /api/accounts/claims`
- `GET /api/accounts/:id/claims`

Write endpoints:

- `POST /api/accounts/:id/claims/run`
- `POST /api/accounts/:id/claims/rescan`
- `POST /api/accounts/:id/claims/pause`
- `POST /api/accounts/:id/claims/resume`

CLI surface:

- `ployctl claims list`
- `ployctl claims inspect <account-id>`
- `ployctl claims run <account-id>`
- `ployctl claims rescan <account-id>`
- `ployctl claims pause <account-id>`
- `ployctl claims resume <account-id>`

### Persistence

Claim state persists to `run/platform/account-claims.json`.

The persisted detail contains:

- account claim status
- redeemable positions
- claim history

This mirrors the existing snapshot-backed control-plane pattern and allows read fallback when HTTP is unavailable.

## Data model

### Operator contracts

Add a new `claims` contract module with:

- `ClaimLoopState`
- `ClaimPositionState`
- `ClaimExecutionOutcome`
- `AccountClaimActionState`
- `AccountClaimStatus`
- `RedeemablePositionSnapshot`
- `ClaimExecutionRecord`
- `AccountClaimActionResponse`
- `AccountClaimDetailResponse`

### Platform state

Extend `ploy-platform` account ownership from the current `AccountSnapshot` stub to an `AccountClaimRegistry` that owns:

- live account enrollment
- status updates
- redeemable position snapshots
- claim execution history

## Connectivity model

The redeem backend is relay-first.

`ploy-connectivity` should provide:

- redeemable-position discovery for an account
- relay-backed claim execution

This reuses the auto-claim branch seam instead of the old direct-CTF claimer path.

## Integration points

### `ployd`

`PloyDaemon` gains:

- persisted claim state file support
- account claim sync from live deployments
- manual claim actions
- automatic periodic claim loops

### `ployctl`

`ControlPlaneClient` gains:

- claim reads over HTTP with snapshot fallback
- claim action POSTs

`main.rs` gains the `claims` command family.

## Auth model

Use the current mainline auth scopes.

- read endpoints require read/operator/admin according to current control-plane policy
- write claim actions require operator/admin, not sidecar-readonly

No browser session or frontend auth changes are required for this minimal port.

## Verification strategy

Port work should be driven by failing tests first:

1. contract serialization tests
2. `ployctl` parsing and client tests
3. `ployd` HTTP/runtime tests for claim endpoints and persistence
4. smoke coverage proving the control-plane still boots and snapshots cleanly

## Out-of-scope follow-up

After this port lands, the next worthwhile follow-up is:

- claim visualization in TUI/frontend
- retirement of the legacy claimer installer and remaining old docs
- claim-specific alerts wired into the already-merged observability system

# Auto-Claim Platform Design

## Goal

Add automatic Polymarket claim/redeem support to the new platform mainline so
resolved live account positions are claimed by default, exposed through the
control plane, and tracked with platform-grade health, audit, and operator
visibility.

## Scope

This design covers four connected areas:

- account-level auto-claim in `ployd`
- operator-visible claim read/write surfaces
- observability and degraded-state propagation for claim loops
- cleanup of stale legacy claimer entrypoints and docs so only the workspace
  platform path remains canonical

This design does not add new trading strategies, new research features, or a
separate settlement daemon outside `ployd`.

## Constraints

- Auto-claim is enabled by default.
- Auto-claim applies to live accounts, not paper deployments.
- Detected redeemable positions should be claimed immediately.
- Claim failures must not stop trading or crash `ployd`.
- Claim must be account-scoped, not deployment-scoped.
- The default platform entrypoints remain `ployd`, `ployctl`, and `ploytui`.

## Architecture

Auto-claim becomes an account-level background capability owned by the platform,
not by a strategy. `ploy-connectivity` owns the Polymarket redeem primitive,
`ploy-platform` owns account claim state, and `ployd` owns the scan/claim loop,
health projection, and operator-facing orchestration.

The system should preserve one control-plane truth:

- which accounts have redeemable positions
- whether claim execution is healthy
- what was claimed, when, and with what outcome

## Runtime Model

### Account Claim Loop

`ployd` runs a claim loop for every live account that has auto-claim enabled.

Each loop performs:

1. scan for redeemable positions on the account
2. publish/update the redeemable snapshot
3. immediately submit redeem transactions for newly redeemable positions
4. persist claim history and audit records
5. project account/system health

The loop must use retry with backoff on failure, but it must not block or crash
the trading runtime.

### Health Semantics

Claim health becomes part of the platform health model:

- `running`: recent scans and claims succeed
- `degraded`: consecutive failures or claim loop stalls
- `recovering`: retry succeeds after degraded

This health must be visible on both the account claim status and the system
status.

## Data Model

### Account Claim Status

Per account:

- `account_id`
- `enabled`
- `status`
- `last_scan_at`
- `last_claim_at`
- `last_error`
- `consecutive_failures`
- `next_retry_at`
- `pending_redeemable_count`
- `pending_redeemable_notional`

### Redeemable Position Snapshot

Per redeemable claim opportunity:

- `account_id`
- `condition_id`
- `market_id`
- `token_ids`
- `redeemable_size`
- `estimated_payout`
- `detected_at`
- `claim_state`

### Claim Execution Record

Per submitted claim:

- `claim_id`
- `account_id`
- `condition_id`
- `submitted_at`
- `tx_hash`
- `amount_claimed`
- `outcome`
- `error_message`

## Control-Plane Surface

### Read APIs

- `GET /api/system/status`
  - add aggregate claim health and claim counters
- `GET /api/accounts/claims`
  - account claim overview
- `GET /api/accounts/:id/claims`
  - detailed redeemable positions and recent claim history

### Write APIs

- `POST /api/accounts/:id/claims/run`
  - one-shot manual claim
- `POST /api/accounts/:id/claims/rescan`
  - force rescan
- `POST /api/accounts/:id/claims/pause`
  - pause auto-claim
- `POST /api/accounts/:id/claims/resume`
  - resume auto-claim

### Operator Clients

`ployctl`, `ploytui`, and the frontend should expose the same semantics:

- claim status
- recent claim history
- degraded claim accounts
- pause/resume/run/rescan controls

## Observability

The platform should emit claim-specific telemetry and operator events:

- claim scans
- redeemable detections
- submitted claims
- successful claims
- failed claims
- claim loop degraded/recovering transitions

Minimum metrics and summaries:

- pending redeemable count
- pending redeemable notional
- claim success count
- claim failure count
- last claim latency
- last successful claim time

Minimum alerts:

- claim loop stalled
- repeated claim failures
- redeemable notional stuck without claim progress

## Auth Model

Claim actions are privileged operator actions.

- readonly users/clients can inspect claim state
- write/admin users can run/rescan/pause/resume claims
- sidecar readonly token must not be able to trigger claims

## Legacy Cleanup

The old claimer world should stop being a default entrypoint.

Keep only reusable claim/redeem primitives that are needed by the new platform.
Any old CLI mode, old strategy-owned claimer path, or stale runbook that
suggests claim is outside `ployd` should be retired or archived.

## Implementation Order

1. Add account claim models and wire contracts.
2. Add redeem primitives to `ploy-connectivity`.
3. Add claim loop state and scheduling to `ployd`.
4. Add claim APIs and `ployctl` support.
5. Add TUI/frontend claim visibility.
6. Add metrics/audit/health transitions for claim loops.
7. Retire legacy claimer entrypoints and stale docs.

## Success Criteria

- Resolved live account positions are claimed automatically by default.
- Claim state is visible via API, CLI, and TUI.
- Claim failures degrade health without stopping trading.
- Claim actions are protected by write/admin auth.
- No old claimer entrypoint remains as the canonical platform path.

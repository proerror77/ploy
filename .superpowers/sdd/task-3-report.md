# Task 3 RED/GREEN Report

## Outcome

Live workers now submit intents through the authenticated `ControlPlaneClient`; `ployd` is the only component that calls the venue gateway and applies account-level risk. The daemon persists a pending order before the venue call. A lost response remains `unknown`, pauses/degrades the deployment, persists the ambiguity, and an idempotent replay cannot call the venue again, including after snapshot restore.

Live restore is deployment-scoped and consumes the daemon's complete trading-state contract. The removed worker SQL restore no longer applies a global `LIMIT 500` or maintains a second live venue ledger. Empty/missing restore state fails live startup. Position reconstruction uses all canonical fills, including fills belonging to terminal orders.

Recorder methods now return `Result`. A live signal persistence failure stops before submission; dry-run logs and continues.

## RED

- `worker_live_submit_uses_control_plane_client`: failed to compile because `ControlPlaneClient::submit_intent` did not exist.
- `live_recorder_failure_stops_submission`: failed to compile because `Recorder` writes returned `()` and could not report failure.
- Existing transport regression expected a transport error to become a terminal rejection/error rather than durable `unknown`.
- Existing live restore queried worker-owned SQL state, only active orders, and `LIMIT 500`.

## GREEN

- `rtk cargo test -p ploy-connectivity -p ploy-control-client -p ploy-strategy-bundles`: passed (235 passed, 1 ignored).
- `rtk cargo test -p ploy-strategy-runtime --features full --lib`: passed (7 passed).
- `rtk cargo test -p ploy-daemon-host`: passed.
- `rtk cargo check --locked --workspace`: passed (0 errors; 11 pre-existing warnings).
- Focused regressions passed: authenticated worker submit, pending-before-side-effect, unknown/no-retry after restore, scoped restore, restore failure, filled-order position reconstruction, recorder fail-closed, and shared-account cap.

## Notes

- No migration was added: the canonical daemon trading snapshot already represents pending/unknown order state and all fills.
- No local PostgreSQL or live/remote action was used.

## Review RED/GREEN Amendment

### RED

- The canonical worker executor returned no fills and reported reconciliation as unattempted; daemon fills could not update the worker position ledger.
- Durable idempotency metadata was absent from trading snapshots and lookup was deployment-local, so the same account could submit the same key through another deployment or after restart.
- `OrderState` had no ambiguity state; transport loss was encoded as `pending` plus `last_error`.
- Acknowledgement persistence remained outside the daemon submit boundary; a final snapshot failure could leave the venue side effect acknowledged while the deployment remained runnable.
- Restore trusted persisted positions instead of rebuilding them from the complete fill ledger and rejecting mismatches.
- Live recorder gaps remained: missing `DATABASE_URL` selected `NullRecorder`, live order/fill rows duplicated canonical daemon ownership, reconciliation/retry errors were discarded, and flush errors only warned.
- Exact stale regression `daemon_surfaces_live_gateway_transport_failure_as_error_without_finalizing_order` expected the obsolete terminal-error/pending behavior.

### GREEN

- `rtk cargo test -p ploy-connectivity -p ploy-control-client -p ploy-strategy-bundles`: passed, 236 passed and 1 ignored.
- `rtk cargo test -p ploy-strategy-runtime --features full --lib`: passed, 8 passed.
- `rtk cargo test -p ploy-daemon-host`: passed.
- `rtk cargo test -p ploy-platform-runtime -p ploy-trading`: passed.
- `rtk cargo test -p ploy-daemon-host daemon_surfaces_live_gateway_transport_failure_as_error_without_finalizing_order`: passed.
- `rtk cargo check --locked --workspace`: passed with 0 errors and 11 pre-existing warnings.
- `rtk git diff --check`: passed.
- Focused regressions passed for incremental canonical fill polling/deduplication, strategy position updates, account-scoped replay across deployments and restart, explicit durable `unknown`, acknowledgement persistence failure degradation, fill-rebuilt position mismatch rejection, live recorder fatal behavior, and live runtime DB requirements.

## Re-review 2 Evidence

- RED: daemon reconciliation excluded `unknown` orders even when a venue order ID proved a venue side effect; the worker continued after live polling errors; cross-deployment replay and unsupported response states could enter the generic acknowledged branch.
- GREEN: `unknown` plus venue ID is reconciled, while `unknown` without venue ID returns `Noop` and remains untracked.
- GREEN: live reconciliation errors panic before another update/submission; non-live behavior remains warning-and-continue.
- GREEN: replay responses owned by another deployment and states `pending`, misspelled, or unsupported return explicit fail-closed `Unknown`, create no canonical-to-local order mapping, and never use the canonical order ID as a venue ID.
- `rtk cargo test -p ploy-platform-runtime -p ploy-trading`: passed.
- `rtk cargo test -p ploy-strategy-bundles -p ploy-strategy-runtime --features full --lib`: passed.
- `rtk cargo test -p ploy-daemon-host`: passed; transport ambiguity regression is now named `daemon_records_live_gateway_transport_ambiguity_as_unknown`.
- `rtk cargo check --locked --workspace`: passed with 0 errors and the same 11 pre-existing warnings.
- `rtk git diff --check`: passed.

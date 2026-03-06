# Collector consolidation TODO

## Goal
Reduce duplicated market-data collection paths and converge on canonical raw tables.

## Phase 1 (start now)

- [x] Inventory current collector and persistence paths (tables + writers + overlap)
- [x] Add explicit consolidation plan
- [x] Make `orderbook-history` write canonical `clob_orderbook_snapshots` (while keeping legacy `clob_orderbook_history_ticks` for compatibility)
- [x] Add migration note for consumers currently reading `clob_orderbook_history_ticks`

## Phase 2

- [x] Convert `sync_records` from primary sink to derived layer (view/materialized view over raw tables)
- [x] Remove duplicated schema DDL from runtime/CLI paths and centralize
- [x] Deprecate legacy `ticks` pathway after read-side migration

## Phase 3

- [x] Remove or archive `backtest_collector` CSV-only flow from primary data pipeline
- [ ] Add one unified collector docs page (what to run for live vs backfill vs research)
- [ ] Add lightweight data-quality checks (freshness + dedup ratios)

## Progress notes

- 2026-03-04: Started Phase 1 implementation.
- 2026-03-04: `OrderbookHistoryCollector` now mirrors into canonical `clob_orderbook_snapshots` with dedup-by-key (`token_id`, `book_timestamp`, `hash`, `source`) checks.
- 2026-03-04: Added migration note at `tasks/collector_migration_note.md`.
- 2026-03-04: Added `platform::persistence_schema` and switched bootstrap + CLI replay backfill table ensures to shared helpers.
- 2026-03-04: `SyncCollector` now persists canonical raw tables (`binance_lob_ticks`, `clob_quote_ticks`) and creates `sync_records_derived` view; legacy `sync_records` writes are compatibility-only behind `PLOY_COLLECTOR_PERSIST_SYNC_RECORDS`.
- 2026-03-04: Legacy `services/data_collector` now defaults to canonical `clob_quote_ticks`; legacy `ticks` writes require `PLOY_LEGACY_TICKS_ENABLED=true`.
- 2026-03-04: `backtest_collector` CSV sink is now compatibility-only (`persist_csv=false` by default), so primary collector pipeline is DB-first.

---

# Strategy Deployment Control Plane Stabilization TODO (2026-03-05)

## Goal
Reduce strategy "listing/deployment" chaos by enforcing one control semantics across API surfaces and removing unsafe strategy fallback behavior in platform bootstrap.

## Tasks

- [x] Create implementation plan doc under `docs/plans/` for this stabilization work.
- [x] Align enable/disable governance between `/api/deployments` and `/api/strategies/control/:id`.
- [x] Ensure enabling via `/api/strategies/control/:id` enforces the same evidence gate rules as `/api/deployments`.
- [x] Remove implicit unknown-strategy -> momentum fallback in deployment matrix application.
- [x] Add/adjust tests for deployment strategy mapping and API enable gate behavior.
- [x] Reconcile direct-live gate tests with current documented behavior (blocked by default, env override explicit).
- [x] Run targeted tests and capture results.
- [x] Commit atomic changes with clear scope messages.

## Review

- [x] Verified no unrelated dirty changes were reverted.
- [x] Verified control plane behavior is consistent across endpoints.
- [x] Verified strategy mapping no longer silently routes unknown strategy keys to momentum.
 
## Progress notes

- [x] Create implementation plan doc under `docs/plans/` for this stabilization work.
- [x] Align enable/disable governance between `/api/deployments` and `/api/strategies/control/:id`.
- [x] Ensure enabling via `/api/strategies/control/:id` enforces the same evidence gate rules as `/api/deployments`.
- [x] Remove implicit unknown-strategy -> momentum fallback in deployment matrix application.
- [x] Add/adjust tests for deployment strategy mapping and API enable gate behavior.
- [x] Reconcile direct-live gate tests with current documented behavior (blocked by default, env override explicit).
- [x] Run targeted tests and capture results.
- [x] Commit atomic changes with clear scope messages.
- [x] Verified no unrelated dirty changes were reverted.
- [x] Verified control plane behavior is consistent across endpoints.
- [x] Verified strategy mapping no longer silently routes unknown strategy keys to momentum.

---

# Trading Host OOM Hardening TODO (2026-03-06)

## Goal
Prevent trading host OOM/timeout caused by on-host Rust builds and missing service memory guards.

## Tasks

- [x] Verify `tango-1-1` runtime state (`rustc/cargo`, `systemd` restart/memory policy, active build processes).
- [x] Pin host default Rust commands to rustup stable (`rustc/cargo` -> latest stable).
- [x] Enforce/automate `systemd` guardrails (`Restart`, `MemoryHigh`, `MemoryMax`, `OOMPolicy`) in GitHub Actions deploy flow.
- [x] Disable legacy remote source-build deploy path by default (`scripts/aws_ec2_deploy.sh` requires explicit override).
- [x] Add trading-host deployment policy to `AGENTS.md` and `CLAUDE.md`.

## Review

- [x] Confirmed host now reports `rustc 1.94.0` and `cargo 1.94.0`.
- [x] Confirmed `ploy-platform.service` shows `Restart=always`, `MemoryHigh=1280M`, `MemoryMax=1536M`, `OOMPolicy=kill`.
- [x] Confirmed no active `cargo`/`rustc` compile processes remain on host.

---

# LEG2 Hotfix Rollout And Acceptance (2026-03-06)

## Goal
Deploy the staggered-arb LEG2 partial-fill hotfix, restart the live platform, and verify online that retries only submit remaining shares while auto-claimer remains active.

## Tasks

- [x] Reproduce the LEG2 retry issue from live fills/logs and identify root cause.
- [x] Implement cumulative LEG2 fill tracking with remaining-shares resubmission.
- [x] Add targeted tests for partial-cancel and cumulative-fill closeout behavior.
- [x] Deploy the hotfix binary to the live host and restart `ploy-platform`.
- [x] Confirm post-restart strategy runtime and auto-claimer startup logs.
- [ ] Confirm a fresh post-restart `STAG-ARB` trade path no longer re-submits full LEG2 size after a partial/failed attempt.
- [x] Review BTC live activity and document why BTC did or did not trade.

## Review

- [x] Local targeted tests and `cargo check` passed before deployment.
- [x] Live host restarted onto the new binary with `--features claimer`.
- [x] Post-restart runtime confirmed active for `BTCUSDT`, `ETHUSDT`, `SOLUSDT`; auto-claimer startup confirmed in live logs.
- [x] BTC feed/runtime coverage confirmed after restart; absence of BTC trades so far is lack of qualifying fills/signals in the observed window, not missing subscription.
- [x] No post-restart `orderbook ... does not exist` execution errors were observed in the acceptance window.
- [ ] Fresh post-restart order-path acceptance still pending a real `LEG1` fill that advances into `LEG2`.

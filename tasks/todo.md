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

# Managed Staggered Arb Hotfix (2026-03-06)

## Goal
Close the live hotfix by making managed `staggered_arb` honor share-based sizing again and by fixing the formal Aliyun release workflow so future deploys complete cleanly.

## Tasks

- [x] Reproduce and confirm live mismatch between requested `20 shares` sizing and observed `fixed_amount_usd=$1 -> 5 shares` behavior.
- [x] Add a regression test that fails when managed runtime config injects `fixed_amount_usd` or drops `shares_per_trade = 20`.
- [x] Change managed split-arb runtime config generation to derive from the canonical `staggered_arb.toml` template and only inject runtime fields (`symbols`, `series_ids`).
- [x] Simplify the Aliyun deploy restart script to explicit ploy unit handling and ship `staggered_arb.toml` in the release bundle.
- [x] Re-run targeted Rust tests, `cargo check`, and shell syntax validation for the remote deploy script.
- [x] Commit the strategy fix and CI fix as separate atomic commits.

## Review

- [x] Confirmed current production binary was already on `v0.1.1-hotfix.20260306.2`, but managed runtime still overrode sizing.
- [x] Confirmed post-restart live logs no longer showed the old `LEG2 Failed -> retry full size` storm.
- [x] Confirmed the remaining issue was config drift in managed bootstrap, not stale host config.
- [x] Validated `.github/workflows/release-aliyun.yml` remote script with `bash -n` after simplification.

## Progress notes

- 2026-03-06: Added a bootstrap regression test asserting managed runtime config keeps `shares_per_trade = 20` and omits `fixed_amount_usd`.
- 2026-03-06: Switched managed `staggered_arb` runtime rendering to start from the canonical strategy template instead of duplicating risk defaults in `bootstrap.rs`.
- 2026-03-06: Updated the Aliyun release bundle/install path to carry `staggered_arb.toml` and replaced the brittle dynamic restart loop with explicit `ploy` unit handling.

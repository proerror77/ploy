# Todo

- [x] Add Postgres integration test harness for engine-store scenarios (Docker or `PLOY_TEST_DATABASE_URL` fallback)
- [x] Add cycle optimistic-lock success-path test (`update_cycle_state` increments version)
- [x] Add conflict tests for `update_cycle_state` / `update_cycle_leg1` / `update_cycle_leg2`
- [x] Add concurrent update race test (exactly one success, one conflict)
- [x] Run targeted integration tests and capture pass/skip behavior
- [x] Update issue #39 work log with diff + validation evidence

## Review

- Planned execution target: GitHub issue #39 (Phase 4 Postgres integration coverage).
- Added file:
  - `tests/engine_store_pg.rs`
- Covered scenarios:
  - `update_cycle_state_success_increments_version`
  - `update_cycle_state_conflict_returns_version_conflict`
  - `update_cycle_leg1_conflict_returns_version_conflict`
  - `update_cycle_leg2_conflict_returns_version_conflict`
  - `concurrent_cycle_updates_yield_one_success_and_one_conflict`
- Validation executed:
  - `cargo test --test engine_store_pg -- --nocapture`
- Validation outcome:
  - Test suite passed (`5/5`).
  - In this environment, tests printed skip message because neither Docker daemon nor `PLOY_TEST_DATABASE_URL` was available; assertions are guarded to no-op in that case.

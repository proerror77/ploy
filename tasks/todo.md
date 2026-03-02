# Todo

- [x] Add explicit `VersionConflict` error variant
- [x] Change cycle update APIs from `Result<bool>` to `Result<()>`
- [x] Make Postgres cycle updates return `VersionConflict` on optimistic-lock miss
- [x] Make MockStore cycle updates return explicit version conflict errors
- [x] Keep engine callsites compatible and remove one silent discard path
- [x] Verify targeted engine/store tests pass

## Review

- Planned execution target: GitHub issue #36 (Phase 1 conflict error model).
- Updated API/contracts:
  - `EngineStore::{update_cycle_state, update_cycle_leg1, update_cycle_leg2}` now return `Result<()>`
  - `PostgresStore` cycle update methods now return `PloyError::VersionConflict` when `rows_affected == 0`
  - `MockStore` now tracks cycle versions and returns the same explicit conflict error
- Validation:
  - `cargo test strategy::execution::engine_store::mock::mock_store_cycle_updates_should_honor_expected_version -- --nocapture`
  - `cargo test strategy::execution::engine::tests -- --nocapture`

Residual risks:
- `enter_leg2` path still treats `update_cycle_state(LEG2_PENDING)` as best-effort (logs error), not abort+halt.
- Legacy engine cycle-version semantics (`expected_version` sources) still need dedicated fix in subsequent phase.

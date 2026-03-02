# Todo

- [x] Introduce `strategy::risk` as the canonical risk subdomain facade
- [x] Introduce `strategy::impls` as strategy-implementation export surface
- [x] Shrink `strategy/mod.rs` root re-export surface (remove flat implementation exports)
- [x] Migrate in-tree imports away from removed root exports/module aliases
- [x] Build and run targeted tests to validate no behavior change
- [x] Update issue #37 work log with concrete diff/test evidence

## Review

- Planned execution target: GitHub issue #37 (Phase 2 export-surface convergence).
- Delivered architecture changes:
  - Added `src/strategy/risk.rs` facade over legacy `risk_mgmt/*`.
  - Added `src/strategy/impls.rs` as implementation export surface.
  - Rewrote `src/strategy/mod.rs` to keep only core contract re-exports and expose subdomains by module.
  - Migrated in-tree imports from flat `strategy::*` / alias modules to explicit subdomains (`strategy::execution::*`, `strategy::risk::*`, `strategy::impls::*`, module-local paths).
- Validation commands (executed):
  - `cargo build`
  - `cargo test strategy::execution::engine::tests -- --nocapture`
  - `cargo test coordinator::bootstrap::tests -- --nocapture`
- Validation outcome:
  - Build passed.
  - Target tests passed (`12/12`, `7/7`).
  - Pre-existing warning remains: unused variable `results` in `src/strategy/directional_backtest.rs:1594`.

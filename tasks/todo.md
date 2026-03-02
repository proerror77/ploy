# Todo

- [x] Rewire `strategy::risk_mgmt::RiskManager` to delegate runtime state/circuit logic to `platform::RiskGate`
- [x] Keep `RiskManager` public API stable for engine/services (`state`, `can_trade`, `daily_stats`, `halt_reason`, etc.)
- [x] Fix signed/unsigned time conversion edge case in `check_leg1_entry` and `must_force_leg2`
- [x] Run targeted tests (`strategy::risk_mgmt::risk::tests`, `strategy::execution::engine::tests`)
- [x] Run `cargo build` for regression check
- [x] Update issue #38 work log with this migration slice

## Review

- Planned execution target: GitHub issue #38 (Phase 3 risk unification).
- Delivered in this slice:
  - `RiskManager` is now an adapter over shared `platform::RiskGate` runtime.
  - Legacy `RiskManager` API remains stable for existing engine/service call sites.
  - Legacy risk-state mapping (`PlatformRiskState` -> `RiskState`) is explicit in adapter.
  - Halt reason compatibility preserved via adapter-side cache + gate event sync.
  - `i64 -> u64` time conversion pitfall removed in `check_leg1_entry`/`must_force_leg2`.
- Validation executed:
  - `cargo test strategy::risk_mgmt::risk::tests -- --nocapture` (5/5)
  - `cargo test strategy::execution::engine::tests -- --nocapture` (12/12)
  - `cargo build` (pass)
- Notes:
  - Existing unrelated warning may still appear in some test invocations: `src/strategy/directional_backtest.rs:1594` (`unused variable: results`).

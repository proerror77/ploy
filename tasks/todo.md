# Todo

- [x] Fix strategy action pipeline to persist orders into `orders` table with `strategy_id`
- [x] Ensure strategy action `client_order_id` is synchronized into submitted `OrderRequest`
- [x] Build-check locally with required features
- [x] Build Linux release binary with required features
- [x] Record review notes and residual risks

## Review

- Implemented order persistence for `strategy start --foreground` path in `src/cli/strategy.rs`.
- Added optional DB bootstrap (`DATABASE_URL`) and graceful fallback if DB unavailable.
- Added status lifecycle persistence: `Pending` insert -> `Submitted` update -> `Filled/other` update.
- Fixed `StrategyAction.client_order_id` mismatch by forcing request `client_order_id` to action ID before execution.
- Verified compile: `cargo check --features "claimer_daemon,api,pm_ctf"`.
- Linux release build:
  - `cargo build --release --target x86_64-unknown-linux-gnu ...` failed on missing `x86_64-linux-gnu-gcc`.
  - `cargo zigbuild --release --target x86_64-unknown-linux-gnu --features "claimer_daemon,api,pm_ctf"` succeeded.
  - Verified artifact is Linux ELF via `file target/x86_64-unknown-linux-gnu/release/ploy`.

Residual risks:
- `strategy` path currently records `leg=1` with `cycle_id=NULL` (acceptable for generic strategy runner but not round/cycle semantics).
- No automated integration test yet for dry-run order persistence; server-side dry-run validation still required.

# Tick-Preserving Optimize Verification Matrix

Date: 2026-04-23

This runbook defines the bounded checks for PM5D `optimize_backtest` recovery.
It is intentionally verification-only: do not use it to justify LOB or
aggTrade downsampling, and do not run heavy Parquet replay on a local Mac.

## Invariants

- Binance LOB cadence stays at collector/replay fidelity. Do not add a sample,
  bucket, or stride layer to `binance_lob_ticks`.
- Binance aggTrade replay stays tick-level. Do not replace it with candles or
  per-second aggregates.
- PM quote replay remains full resolution for trusted quote sources.
- `--max-updates` is smoke-only evidence. Any run using it is non-canonical.
- The Apr 15-22 six-symbol window is blocked until preflight, two-symbol smoke,
  narrow six-symbol smoke, and host-health checks have all passed.

## Cheap Local Checks

These are safe on the developer machine because they only inspect files.

```bash
./scripts/check_optimize_verification_gates.sh
```

Expected result before the recovery gates land: `FAIL`, with missing controls
listed. Expected result before any full-window rerun: `PASS`.

For source edits in other lanes, use only narrow checks unless the operator has
explicitly accepted local Rust/DuckDB/Parquet load:

```bash
cargo fmt --check --package ploy-strategy-bundles
```

If Rust source changed and local build cost is acceptable, this is the maximum
local compile check for this lane:

```bash
cargo check -p ploy-strategy-bundles \
  --features ploy-strategy-bundles/parquet-feed \
  --example optimize_backtest
```

Do not run `cargo test -p ploy-strategy-bundles --features parquet-feed --lib`
locally for this recovery unless the operator explicitly accepts the local
DuckDB/Parquet compile cost.

## Required CI/Self-Hosted Sequence

Run these on `ploy-ci-1` or another isolated runner, not on the local Mac.
The commands assume Stage 1 has added workflow inputs for run mode, preflight
limits, large-window override, DuckDB memory/temp controls, and optional
`max_updates`.

### 1. Preflight Only: Full Apr 15-22 Six Symbols

This measures the dangerous window without replaying or optimizing.

```bash
gh workflow run optimize.yml \
  -f git_ref=<branch-or-sha> \
  -f run_mode=preflight \
  -f train_start=2026-04-15 \
  -f train_end=2026-04-19 \
  -f val_start=2026-04-20 \
  -f val_end=2026-04-22 \
  -f symbols=BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT,DOGEUSDT,BNBUSDT \
  -f trials=0 \
  -f allow_large_window=false \
  -f max_preflight_rows=<approved-row-limit> \
  -f max_preflight_bytes=<approved-byte-limit> \
  -f duckdb_memory_limit=4GB \
  -f duckdb_temp_dir=/tmp/ploy-duckdb-optimize
```

Pass evidence:

- source-level row/byte counts for spot, aggTrade, LOB, PM quotes, and events;
- command exits before replay when thresholds are exceeded;
- runner remains online and SSH-able after the job.

### 2. Bounded Optimize Smoke: Two Symbols, One Day, Five Trials

This proves the optimizer can complete a small faithful replay.

```bash
gh workflow run optimize.yml \
  -f git_ref=<branch-or-sha> \
  -f run_mode=optimize \
  -f train_start=2026-04-15 \
  -f train_end=2026-04-15 \
  -f val_start=2026-04-16 \
  -f val_end=2026-04-16 \
  -f symbols=BTCUSDT,ETHUSDT \
  -f trials=5 \
  -f allow_large_window=false \
  -f max_updates= \
  -f duckdb_memory_limit=4GB \
  -f duckdb_temp_dir=/tmp/ploy-duckdb-optimize
```

Pass evidence:

- nonzero updates processed for train and validation;
- validation completes;
- output labels any random-search branch honestly;
- host-health check shows no lingering `Runner.Worker`, `optimize_backtest`,
  `duckdb`, `cargo`, or `rustc` processes.

### 3. Narrow Six-Symbol Smoke: Timestamp Window, One To Three Trials

This proves six-symbol ordering and source mix without the full multi-day load.

```bash
gh workflow run optimize.yml \
  -f git_ref=<branch-or-sha> \
  -f run_mode=optimize \
  -f train_start_ts=2026-04-15T00:00:00Z \
  -f train_end_ts=2026-04-15T01:00:00Z \
  -f val_start_ts=2026-04-15T01:00:00Z \
  -f val_end_ts=2026-04-15T02:00:00Z \
  -f symbols=BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT,DOGEUSDT,BNBUSDT \
  -f trials=3 \
  -f allow_large_window=false \
  -f max_updates= \
  -f duckdb_memory_limit=4GB \
  -f duckdb_temp_dir=/tmp/ploy-duckdb-optimize
```

Pass evidence:

- all six symbols appear in the manifest and replay summary;
- aggTrade and LOB counts are nonzero where data exists;
- replay completes or fails legibly without host loss;
- runner returns to online and idle.

## Full Window Gate

Do not run the original Apr 15-22 six-symbol optimization unless all of these
are true:

- `./scripts/check_optimize_verification_gates.sh` passes on the branch;
- full-window preflight has passed or failed before replay with clear sizing
  output;
- two-symbol smoke has passed;
- narrow six-symbol smoke has passed;
- post-run host-health evidence is attached for every prior job;
- the command uses an explicit `allow_large_window=true` override.

The full command must not be a workflow default. It must be an intentional,
operator-approved dispatch after the gates above.

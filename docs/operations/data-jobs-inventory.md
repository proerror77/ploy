# Data Jobs Inventory

Date: 2026-05-24

This inventory classifies repo-local data scripts, jobs, and workflow surfaces so
Phase 6 cleanup can retire duplicates without guessing. It does not change live
collection behavior.

## Canonical Runtime Surfaces

| Surface | Status | Owner | Notes |
| --- | --- | --- | --- |
| `crates/ploy-market-data` | canonical | Rust data plane | Owns live feed/scanner/discovery code used by runner/runtime surfaces. |
| `/opt/ploy/bin/ploy-runner collect-quotes` / `ploy-runner-host::ops` | canonical ops surface | runner ops | Full/ops build only; lean replay/backtest binaries intentionally do not expose it. |
| `/opt/ploy/bin/ploy-runner collect-pm-trades` / `ploy-runner-host::ops` | canonical ops surface | runner ops | Polls Polymarket Data API trade prints into `clob_trade_ticks`; full/ops build only. |
| `ploy-feed-loaders` | canonical historical DB loader | research/backtest adapters | Owns SQLx historical `MarketUpdate` loading outside strategy-bundles. |
| `scripts/export_parquet.sh` | canonical export helper | data/export host | Keep as the explicit Parquet export entrypoint until replaced by Rust datactl. |
| `.github/workflows/backtest.yml` | canonical CI backtest lane | CI/backtest host | Should remain separated from trade-host deploy assumptions. |
| `.github/workflows/optimize.yml` | canonical optimization lane | CI/backtest host | Same data-source assumptions as backtest lane. |
| `.github/workflows/deploy-trade.yml` | canonical trade-host deploy lane | trade host | Should own trading artifacts only. |

## Compatibility Or Transitional Live Collection

| Surface | Status | Owner | Replacement / Direction |
| --- | --- | --- | --- |
| `scripts/binance_price_collector.py` | compatibility collector | data host | Keep until equivalent Rust collector path is proven and deployed. |
| `scripts/binance_aggtrade_collector.py` | compatibility collector | data host | Deployed by `deploy-tango-1-1.yml`; retire only after Rust feed/persistence replacement. |
| `scripts/binance_lob_collector.py` | compatibility collector | data host | Critical L2 source today; do not delete without live replacement and freshness evidence. |
| `scripts/polymarket_quote_collector.py` | compatibility collector | data host | Prefer `collect-quotes`/market-data long term. |
| `.github/workflows/deploy-tango-1-1.yml` | transitional combined data/runtime deploy | tango-1-1 | Still bundles data collectors plus research artifacts; split after host-role cleanup. |

## One-Shot Backfill / Repair Jobs

| Surface | Status | Owner | Notes |
| --- | --- | --- | --- |
| `scripts/backfill_pm_midpoints.py` | one-shot backfill | data repair | Keep with migration/runbook context. |
| `scripts/backfill_quotes.py` | one-shot backfill | data repair | Candidate for archive after quote history repair is complete. |
| `scripts/backfill_quotes*.sh` | one-shot backfill variants | data repair | Consolidate or archive duplicates. |
| `scripts/backfill_settlements.py` | one-shot backfill | data repair | Keep until settlement repair path is Rust-owned. |
| `scripts/backfill_price_to_beat.sql` | one-shot SQL repair | DB repair | Keep as migration/reference until encoded in migration. |
| `scripts/fix_binance_lob_partitions.sql` | one-shot/operational repair | DB repair | Still deployed by `deploy-tango-1-1.yml`. |
| `scripts/fix_deribit_partitions.sql` | one-shot/operational repair | DB repair | Keep until Deribit path is retired or fully owned. |
| `scripts/verify_price_to_beat.sh` | verification helper | data repair | Keep while price-to-beat backfills are active. |
| `scripts/db_retention.sql` | maintenance SQL | DB maintenance | Keep as explicit maintenance helper. |

## Diagnostics / Reports

| Surface | Status | Owner | Notes |
| --- | --- | --- | --- |
| `scripts/check_db.sql` | diagnostic SQL | ops | Keep as SQL snippet/reference. |
| `scripts/check_polymarket_api_usage.sh` | diagnostic | ops | Keep; no Rust replacement needed. |
| `scripts/report_drawdown.py` | report helper | research | Keep until research reporting is consolidated. |
| `scripts/refresh_research_valid_windows.sh` | research maintenance | research | Keep; tied to factor research materialized view. |
| `scripts/run_factor_research.sh` | manual direct-DB debug runner | research | Break-glass only; requires `PLOY_ALLOW_DIRECT_FACTOR_RESEARCH=manual-direct-factor-research`. It is not the canonical research chain and must not be used for promotion evidence; prefer retained snapshots, hosted artifact workflows, durable trace, and Research Manager plans. |
| `scripts/run_factor_research_matrix.sh` | manual direct-DB debug batch runner | research | Calls `run_factor_research.sh` and inherits the same explicit ACK requirement and non-canonical status. |

## Archive Candidates

| Surface | Status | Reason |
| --- | --- | --- |
| `scripts/train_crypto_*_onnx_from_db.py` | research prototype | Keep only while ML lane is active; otherwise archive under research prototypes. `scripts/train_crypto_lob_mlp_onnx_from_db.py` has been archived in favor of `scripts/train_crypto_lob_tcn_onnx_from_db.py`. |

## Platform Release / Host Install

| Surface | Status | Owner | Notes |
| --- | --- | --- | --- |
| `scripts/install-platform-service.sh` | canonical platform release installer | release-platform | Active `release-platform.yml` bundle/install/execute path; now also owns host-support maintenance/watchdog unit installation; guarded by `tests/platform_release_workflow.rs`; do not archive. |

## Archived Legacy Research Scripts

| Surface | Archived Path | Reason |
| --- | --- | --- |
| `scripts/simulate_backtest.py` | `scripts/archive/research-debug/simulate_backtest.py` | Legacy standalone simulator; canonical evidence is produced by Rust/backtest and hosted snapshot-backed workflows. |
| `scripts/train_crypto_lob_mlp_onnx_from_db.py` | `scripts/archive/research-debug/train_crypto_lob_mlp_onnx_from_db.py` | Self-deprecated MLP trainer; active LOB ML docs point to the TCN training entry. |
| `scripts/copycat_dry_run.py` | `scripts/archive/research-debug/copycat_dry_run.py` | Public-profile copy-trading prototype; future copy-trading work should use canonical `MarketUpdate`/runtime path and runtime evidence. |
| `scripts/reverse_engineered_strategy_dry_run.py` | `scripts/archive/research-debug/reverse_engineered_strategy_dry_run.py` | Public-profile reverse-engineering prototype; not a canonical research, replay, or dry-run handoff path. |
| `scripts/check_db.rs` | `scripts/archive/legacy-research-tools/check_db.rs` | Duplicate DB diagnostic; canonical ops path is `/opt/ploy/bin/ploy-runner check-db`. |
| `scripts/check_db_data.rs` | `scripts/archive/legacy-research-tools/check_db_data.rs` | Duplicate DB diagnostic; canonical ops path is `/opt/ploy/bin/ploy-runner check-db`. |
| `scripts/collect_data.py` | `scripts/archive/legacy-research-tools/collect_data.py` | Generic Binance kline CSV collector; research now starts from retained snapshot artifacts. |
| `scripts/collect_klines.sh` | `scripts/archive/legacy-research-tools/collect_klines.sh` | Generic Binance kline CSV collector; research now starts from retained snapshot artifacts. |
| `scripts/discover_new_markets.py` | `scripts/archive/legacy-research-tools/discover_new_markets.py` | Local DB market discovery prototype; canonical discovery is Rust market-data / deployed collector flow. |
| `scripts/discover_pm_updown_markets.py` | `scripts/archive/legacy-research-tools/discover_pm_updown_markets.py` | CLI-based discovery prototype; canonical discovery is Rust market-data / deployed collector flow. |
| `scripts/install-service.sh` | `scripts/archive/legacy-root-runtime/install-service.sh` | Legacy host-support installer; maintenance/watchdog unit ownership moved to `install-platform-service.sh` and `release-platform.yml`. |
| `scripts/deploy_7_symbols.sh` | `scripts/archive/legacy-root-runtime/deploy_7_symbols.sh` | Manual host binary copy/restart script; deployment must use CI-built artifacts and workflows. |
| `scripts/deploy-ploy-runner.sh` | `scripts/archive/legacy-root-runtime/deploy-ploy-runner.sh` | Manual host binary/config/systemd mutation script; deployment must use CI-built artifacts and workflows. |

## Guardrails

- Do not delete collector scripts that are still copied or restarted by deploy workflows.
- Do not move Rust builds onto live trading hosts; deploy CI-built artifacts only.
- Prefer current Rust control-plane/runtime surfaces for new operational docs.
- Mark Python scripts as compatibility or one-shot unless they are the canonical source for a live data stream.
- Before retirement, verify runbook references with `rg` and confirm replacement freshness or parity evidence.
- Treat direct-DB factor research scripts as operator break-glass tools only.
  Artifact-backed research starts with `research-snapshot.yml`, routes through
  hosted factor review/walk-forward workflows, persists Research OS trace, and
  lets `research-trace-plan.yml` produce the next bounded research action.
- Treat `--allow-direct-db-debug` and `--allow-legacy-snapshot-build` as
  audited manual exceptions. They may reproduce old evidence or debug a data
  incident, but they must not feed promotion, handoff issues, config PRs, or
  Research Manager "ready" decisions.

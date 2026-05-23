# Data Jobs Inventory

Date: 2026-04-22

This inventory classifies repo-local data scripts, jobs, and workflow surfaces so
Phase 6 cleanup can retire duplicates without guessing. It does not change live
collection behavior.

## Canonical Runtime Surfaces

| Surface | Status | Owner | Notes |
| --- | --- | --- | --- |
| `crates/ploy-market-data` | canonical | Rust data plane | Owns live feed/scanner/discovery code used by runner/runtime surfaces. |
| `new-ploy-runner collect-quotes` / `ploy-runner-host::ops` | canonical ops surface | runner ops | Full/ops build only; lean replay/backtest binaries intentionally do not expose it. |
| `new-ploy-runner collect-pm-trades` / `ploy-runner-host::ops` | canonical ops surface | runner ops | Polls Polymarket Data API trade prints into `clob_trade_ticks`; full/ops build only. |
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
| `scripts/check_db.rs` | compatibility diagnostic | ops | Prefer runner `check-db`; archive after parity. |
| `scripts/check_db_data.rs` | compatibility diagnostic | ops | Prefer runner `check-db`; archive after parity. |
| `scripts/check_db.sql` | diagnostic SQL | ops | Keep as SQL snippet/reference. |
| `scripts/check_polymarket_api_usage.sh` | diagnostic | ops | Keep; no Rust replacement needed. |
| `scripts/report_drawdown.py` | report helper | research | Keep until research reporting is consolidated. |
| `scripts/refresh_research_valid_windows.sh` | research maintenance | research | Keep; tied to factor research materialized view. |
| `scripts/run_factor_research.sh` | manual direct-DB debug runner | research | Break-glass only; requires `PLOY_ALLOW_DIRECT_FACTOR_RESEARCH=manual-direct-factor-research`. Prefer retained artifacts and hosted workflows. |
| `scripts/run_factor_research_matrix.sh` | manual direct-DB debug batch runner | research | Calls `run_factor_research.sh` and inherits the same explicit ACK requirement. |

## Archive Candidates

| Surface | Status | Reason |
| --- | --- | --- |
| `scripts/collect_data.py` | archive candidate | Generic historical kline collector overlaps with current DB/export paths. |
| `scripts/collect_klines.sh` | archive candidate | Same overlap as `collect_data.py`. |
| `scripts/simulate_backtest.py` | archive candidate | Legacy Python simulation should not be primary backtest path. |
| `scripts/reverse_engineered_strategy_dry_run.py` | archive candidate | Research prototype, not canonical runtime. |
| `scripts/copycat_dry_run.py` | archive candidate | Research prototype; future copy-trading work should use canonical `MarketUpdate`/runtime path. |
| `scripts/discover_new_markets.py` | archive candidate | Discovery belongs in Rust market-data/scanner once parity is proven. |
| `scripts/discover_pm_updown_markets.py` | archive candidate | Same as above. |
| `scripts/deploy_7_symbols.sh` | archive candidate | Deployment should use manifests/workflows. |
| `scripts/deploy-ploy-runner.sh` | archive candidate | Prefer CI-built artifacts and deploy workflows. |
| `scripts/install-service.sh` / `scripts/install-platform-service.sh` | archive candidate | Keep only if runbooks still reference them; otherwise workflow/systemd install owns service deployment. |
| `scripts/train_crypto_*_onnx_from_db.py` | research prototype | Keep only while ML lane is active; otherwise archive under research prototypes. |

## Guardrails

- Do not delete collector scripts that are still copied or restarted by deploy workflows.
- Do not move Rust builds onto live trading hosts; deploy CI-built artifacts only.
- Prefer current Rust control-plane/runtime surfaces for new operational docs.
- Mark Python scripts as compatibility or one-shot unless they are the canonical source for a live data stream.
- Before retirement, verify runbook references with `rg` and confirm replacement freshness or parity evidence.

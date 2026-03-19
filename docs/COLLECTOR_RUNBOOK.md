# Collector And Backfill Runbook

This page answers one practical question:

`live`, `backfill`, and `research` each use different commands. Run the right one for the job.

## Quick Routing

| Goal | Command family | Use when | Do not use when |
| --- | --- | --- | --- |
| Continuous live/raw capture | `ploy collect` | You want fresh Binance + Polymarket-aligned data flowing into Postgres | You need historical replay or large backfills |
| Historical Polymarket L2 backfill | `ploy orderbook-history` | You need historical CLOB snapshots for specific token IDs | You want the main live sync collector |
| Historical Deribit IV baseline | `ploy deribit-iv-backfill` | You need `deribit_iv_ticks` for IV-aware research/backtests | You only need live PM/Binance data |
| Backtest/research dataset prep | `ploy strategy backfill-*` | You are preparing tables for replay, settlement, or kline-based analysis | You want a 24/7 collector process |

## 1. Live Collection: `ploy collect`

Use this when you want the main synchronized collector.

It is the right command for:

- live lag-analysis capture
- ongoing Binance + Polymarket raw data ingestion
- building a fresh `sync_records` stream for later research

Example:

```bash
ploy collect \
  --symbols BTCUSDT,ETHUSDT,SOLUSDT \
  --markets bitcoin-up-or-down-march-7-12pm,ethereum-up-or-down-march-7-12pm \
  --duration 0
```

Notes:

- `--duration 0` means run until stopped.
- This path writes to database tables; the main collector path is not the CSV sink.
- This mode auto-discovers crypto-series Polymarket tokens and bridges live PM prices into the collector.

Use `ploy collect` when the question is:

- "What should keep running in the background to accumulate fresh raw data?"

Do not use it when the question is:

- "I need the last 7 days of PM L2 snapshots."
- "I need Deribit IV history."
- "I need replay tables for an offline backtest."

## 2. Historical PM L2 Backfill: `ploy orderbook-history`

Use this when you need historical Polymarket CLOB orderbook snapshots for one or more token IDs.

Example:

```bash
ploy orderbook-history \
  --asset-ids 12345,67890 \
  --lookback-secs 3600 \
  --levels 20 \
  --sample-ms 1000 \
  --resume-from-db
```

Typical use cases:

- rebuild missing L2 history for specific assets
- backfill PM orderbook snapshots for research
- resume from the DB high-water mark without restarting from scratch

Key flags:

- `--asset-ids`: required token IDs
- `--resume-from-db`: continue from stored high-water mark
- `--lookback-secs`: rolling window if `--start-ms` is omitted
- `--sample-ms 0`: persist every returned snapshot

Use `ploy orderbook-history` when the question is:

- "I know the PM token IDs and want historical depth."

## 3. Deribit IV Baseline: `ploy deribit-iv-backfill`

Use this when you want historical Deribit volatility-index bars in Postgres.

Example:

```bash
ploy deribit-iv-backfill \
  --currencies BTC,ETH \
  --lookback-days 30 \
  --resolution-secs 60
```

Dry-run example:

```bash
ploy deribit-iv-backfill \
  --currencies BTC \
  --start 2026-02-01T00:00:00Z \
  --end 2026-02-07T00:00:00Z \
  --dry-run
```

This is the right path for:

- IV-aware research
- volatility baseline tables
- filling `deribit_iv_ticks`

Use `ploy deribit-iv-backfill` when the question is:

- "I need Deribit IV bars in the database."

## 4. Research And Backtest Prep: `ploy strategy backfill-*`

These commands are for offline prep, not live collection.

### 4.1 Binance Klines

```bash
ploy strategy backfill-klines \
  --symbols BTCUSDT,ETHUSDT,SOLUSDT \
  --from 2026-02-20T00:00:00Z \
  --to 2026-02-28T00:00:00Z \
  --interval 1m
```

Use this when you need historical Binance bars for backtests.

### 4.2 PM Replay Tables

```bash
ploy strategy backfill-pm-replay-tables \
  --from 2026-02-20T00:00:00Z \
  --to 2026-02-28T00:00:00Z \
  --symbols BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT
```

Use this when you want to materialize replay-friendly PM tables from `sync_records`.

### 4.3 PM Token Settlements

```bash
ploy strategy backfill-pm-token-settlements \
  --from 2026-02-20T00:00:00Z \
  --to 2026-02-28T00:00:00Z \
  --symbols BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT
```

Use this when you want official Polymarket settlement state in `pm_token_settlements`.

## 5. Recommended Workflows

### Live Capture Workflow

1. Run `ploy collect` continuously.
2. Periodically inspect downstream freshness/lag.
3. Use the collected raw tables later for research transforms.

### PM L2 Research Workflow

1. Identify token IDs to study.
2. Run `ploy orderbook-history --resume-from-db`.
3. Join those snapshots with settlement or replay tables later.

### Offline Backtest Prep Workflow

1. Backfill Binance klines with `ploy strategy backfill-klines`.
2. Backfill PM replay tables with `ploy strategy backfill-pm-replay-tables`.
3. Backfill settlements with `ploy strategy backfill-pm-token-settlements`.
4. Optionally backfill Deribit IV with `ploy deribit-iv-backfill`.

## 6. Lightweight Data-Quality Check

Use this when you want a quick health read on collector tables without starting a collector:

```bash
ploy collect --check-only
```

Tune the window and stale threshold if needed:

```bash
ploy collect --check-only \
  --lookback-minutes 30 \
  --freshness-warn-secs 90
```

The report prints, for each core collector relation:

- latest timestamp
- age in seconds
- recent row count
- duplicate row count
- duplicate ratio over the selected lookback window

Current coverage includes:

- `clob_quote_ticks`
- `binance_lob_ticks`
- `clob_orderbook_snapshots`
- `sync_records_derived`

Use it when the question is:

- "Is the collector fresh?"
- "Am I writing lots of duplicate snapshots?"
- "Did the raw pipeline stop moving?"

## 7. Sanity Checks

After dataset prep, useful follow-up commands are:

```bash
ploy strategy accuracy --lookback-hours 168 --limit 50
ploy strategy backtest-list --limit 20
```

If you are unsure which path to use, use this rule:

- Need fresh raw streaming data: `ploy collect`
- Need historical PM L2 for known assets: `ploy orderbook-history`
- Need Deribit IV history: `ploy deribit-iv-backfill`
- Need offline backtest tables: `ploy strategy backfill-*`

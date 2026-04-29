# Parquet Backtest Pipeline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace PostgreSQL network transfer in backtest with local Parquet files — daily export on Tango-1-1, rsync to ploy-ci-1, DuckDB reads locally — reducing data load time from ~13 minutes to under 60 seconds.

**Architecture:** A cron job on Tango-1-1 exports each table to date-partitioned Parquet files using DuckDB's `postgres_scanner`. The backtest workflow rsyncs the needed date range to ploy-ci-1 before running. `run_backtest` gains a `--data-dir` flag; when set, a new `parquet.rs` feed loader uses DuckDB to read Parquet instead of PostgreSQL. The PostgreSQL path remains as fallback.

**Tech Stack:** DuckDB (CLI on Tango-1-1 + Rust `duckdb` crate on ploy-ci-1), Apache Parquet, rsync over SSH, Rust, existing `MarketUpdate` / `HistoricalLoadOptions` types.

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `scripts/export_parquet.sh` | Create | Daily export: DuckDB reads PG via postgres_scanner, writes Parquet |
| `/etc/cron.d/parquet-export` | Create (on Tango-1-1) | Runs export_parquet.sh at 01:00 daily |
| `crates/ploy-strategy-bundles/src/feed/parquet.rs` | Create | DuckDB-backed feed loader, same interface as `database.rs` |
| `crates/ploy-strategy-bundles/src/feed/mod.rs` | Modify | Export `parquet` module |
| `crates/ploy-strategy-bundles/examples/run_backtest.rs` | Modify | Add `--data-dir` flag, dispatch to parquet loader |
| `crates/ploy-strategy-bundles/Cargo.toml` | Modify | Add `duckdb` dependency under `parquet-feed` feature |
| `.github/workflows/backtest.yml` | Modify | Add rsync step before Run backtest; add `data_dir` input |

---

## Phase 1 — Export Script on Tango-1-1

### Task 1: Write and deploy Parquet export script

**Files:**
- Create: `/opt/ploy/scripts/export_parquet.sh` (on Tango-1-1 via SSH)

- [ ] **Step 1: Install DuckDB CLI on Tango-1-1**

```bash
ssh tango-1-1 "
wget -q https://github.com/duckdb/duckdb/releases/download/v1.2.1/duckdb_cli-linux-amd64.zip \
  -O /tmp/duckdb.zip && \
unzip -o /tmp/duckdb.zip -d /usr/local/bin && \
chmod +x /usr/local/bin/duckdb && \
duckdb --version
"
```
Expected: `v1.2.1`

- [ ] **Step 2: Create export script**

```bash
ssh tango-1-1 "cat > /opt/ploy/scripts/export_parquet.sh << 'SCRIPT'
#!/bin/bash
set -e
EXPORT_DATE=\${1:-\$(date -d 'yesterday' +%Y-%m-%d)}
OUT_DIR=/opt/ploy/data/parquet
DB_URL=\"postgresql://postgres:postgres@localhost:5432/ploy\"

mkdir -p \$OUT_DIR/{binance_price_ticks,clob_quote_ticks,binance_lob_ticks,binance_agg_trade_ticks,pm_market_metadata,pm_token_settlements}

duckdb -c \"
INSTALL postgres_scanner; LOAD postgres_scanner;
ATTACH '\$DB_URL' AS pg (TYPE POSTGRES, READ_ONLY);

COPY (SELECT * FROM pg.binance_price_ticks
      WHERE trade_time >= '\$EXPORT_DATE'::date
        AND trade_time <  '\$EXPORT_DATE'::date + INTERVAL '1 day')
TO '\$OUT_DIR/binance_price_ticks/\$EXPORT_DATE.parquet' (FORMAT PARQUET, COMPRESSION ZSTD);

COPY (SELECT * FROM pg.clob_quote_ticks
      WHERE received_at >= '\$EXPORT_DATE'::date
        AND received_at <  '\$EXPORT_DATE'::date + INTERVAL '1 day')
TO '\$OUT_DIR/clob_quote_ticks/\$EXPORT_DATE.parquet' (FORMAT PARQUET, COMPRESSION ZSTD);

COPY (SELECT * FROM pg.binance_lob_ticks
      WHERE event_time >= '\$EXPORT_DATE'::date
        AND event_time <  '\$EXPORT_DATE'::date + INTERVAL '1 day')
TO '\$OUT_DIR/binance_lob_ticks/\$EXPORT_DATE.parquet' (FORMAT PARQUET, COMPRESSION ZSTD);

COPY (SELECT * FROM pg.binance_agg_trade_ticks
      WHERE trade_time >= '\$EXPORT_DATE'::date
        AND trade_time <  '\$EXPORT_DATE'::date + INTERVAL '1 day')
TO '\$OUT_DIR/binance_agg_trade_ticks/\$EXPORT_DATE.parquet' (FORMAT PARQUET, COMPRESSION ZSTD);

COPY (SELECT * FROM pg.pm_market_metadata
      WHERE start_time >= '\$EXPORT_DATE'::date
        AND start_time <  '\$EXPORT_DATE'::date + INTERVAL '1 day')
TO '\$OUT_DIR/pm_market_metadata/\$EXPORT_DATE.parquet' (FORMAT PARQUET, COMPRESSION ZSTD);

COPY (SELECT * FROM pg.pm_token_settlements
      WHERE created_at >= '\$EXPORT_DATE'::date
        AND created_at <  '\$EXPORT_DATE'::date + INTERVAL '1 day')
TO '\$OUT_DIR/pm_token_settlements/\$EXPORT_DATE.parquet' (FORMAT PARQUET, COMPRESSION ZSTD);
\"
echo \"Export complete: \$EXPORT_DATE\"
SCRIPT
chmod +x /opt/ploy/scripts/export_parquet.sh
"
```

- [ ] **Step 3: Test export for one day**

```bash
ssh tango-1-1 "/opt/ploy/scripts/export_parquet.sh 2026-04-17"
```
Expected: `Export complete: 2026-04-17`

- [ ] **Step 4: Verify Parquet files exist and are reasonable size**

```bash
ssh tango-1-1 "ls -lh /opt/ploy/data/parquet/*/2026-04-17.parquet"
```
Expected: files exist, `binance_price_ticks` ~50-200MB, `clob_quote_ticks` ~50-150MB, `binance_lob_ticks` ~200-500MB.

- [ ] **Step 5: Set up daily cron**

```bash
ssh tango-1-1 "echo '0 1 * * * root /opt/ploy/scripts/export_parquet.sh >> /var/log/parquet_export.log 2>&1' > /etc/cron.d/parquet-export"
```

- [ ] **Step 6: Backfill existing 14 days**

```bash
ssh tango-1-1 "for d in \$(seq 0 13 | xargs -I{} date -d '{} days ago' +%Y-%m-%d); do /opt/ploy/scripts/export_parquet.sh \$d; done"
```

- [ ] **Step 7: Commit export script to repo**

```bash
cp /tmp/export_parquet.sh scripts/export_parquet.sh  # copy local version
git add scripts/export_parquet.sh
git commit -m "feat(data): add daily Parquet export script for backtest pipeline"
```

---

## Phase 2 — Rust Parquet Feed Loader

### Task 2: Add duckdb dependency

**Files:**
- Modify: `crates/ploy-strategy-bundles/Cargo.toml`

- [ ] **Step 1: Add duckdb crate under feature flag**

In `crates/ploy-strategy-bundles/Cargo.toml`, add:

```toml
[features]
default = []
parquet-feed = ["duckdb"]

[dependencies]
# existing deps...
duckdb = { version = "1.1", features = ["bundled"], optional = true }
```

- [ ] **Step 2: Verify it compiles**

```bash
cargo check -p ploy-strategy-bundles --features parquet-feed
```
Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add crates/ploy-strategy-bundles/Cargo.toml
git commit -m "feat(deps): add duckdb optional dependency for parquet-feed feature"
```

---

### Task 3: Implement parquet.rs feed loader

**Files:**
- Create: `crates/ploy-strategy-bundles/src/feed/parquet.rs`
- Modify: `crates/ploy-strategy-bundles/src/feed/mod.rs`

- [ ] **Step 1: Write failing test**

In `crates/ploy-strategy-bundles/src/feed/parquet.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_load_returns_empty_for_missing_dir() {
        let result = load_from_parquet(
            "/nonexistent/path",
            &["BTCUSDT".to_string()],
            Utc::now() - chrono::Duration::days(1),
            Utc::now(),
            &Default::default(),
        );
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p ploy-strategy-bundles --features parquet-feed feed::parquet
```
Expected: compile error (module not yet implemented).

- [ ] **Step 3: Implement parquet.rs**

```rust
//! DuckDB-backed Parquet feed loader for backtesting.
//! Reads date-partitioned Parquet files exported by export_parquet.sh.

#[cfg(feature = "parquet-feed")]
use duckdb::Connection;
use chrono::{DateTime, Utc, NaiveDate};
use std::path::Path;

use crate::feed::database::HistoricalLoadOptions;
use crate::traits::MarketUpdate;

/// Load historical market updates from local Parquet files.
///
/// `data_dir` must contain subdirectories:
///   binance_price_ticks/, clob_quote_ticks/, binance_lob_ticks/,
///   binance_agg_trade_ticks/, pm_market_metadata/, pm_token_settlements/
///
/// Each subdirectory contains files named YYYY-MM-DD.parquet.
pub fn load_from_parquet(
    data_dir: &str,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    options: &HistoricalLoadOptions,
) -> Result<Vec<MarketUpdate>, Box<dyn std::error::Error>> {
    if !Path::new(data_dir).exists() {
        return Ok(vec![]);
    }

    #[cfg(not(feature = "parquet-feed"))]
    {
        return Err("parquet-feed feature not enabled".into());
    }

    #[cfg(feature = "parquet-feed")]
    {
        let conn = Connection::open_in_memory()?;
        let symbols_sql = symbols
            .iter()
            .map(|s| format!("'{}'", s))
            .collect::<Vec<_>>()
            .join(", ");

        // Build glob patterns for date range
        let dates = date_range(from.date_naive(), to.date_naive());
        let mut updates: Vec<MarketUpdate> = Vec::new();

        // Load spot prices (with 30-min warmup)
        let warmup_from = from - chrono::Duration::minutes(30);
        let warmup_dates = date_range(warmup_from.date_naive(), to.date_naive());
        let price_files = parquet_files(data_dir, "binance_price_ticks", &warmup_dates);
        if !price_files.is_empty() {
            let files_sql = files_to_sql(&price_files);
            let mut stmt = conn.prepare(&format!(
                "SELECT symbol, trade_time, price FROM read_parquet([{files_sql}])
                 WHERE symbol IN ({symbols_sql})
                   AND trade_time >= '{warmup_from}'
                   AND trade_time <= '{to}'
                 ORDER BY trade_time"
            ))?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, DateTime<Utc>>(1)?,
                    row.get::<_, rust_decimal::Decimal>(2)?,
                ))
            })?;
            for row in rows {
                let (symbol, ts, price) = row?;
                updates.push(MarketUpdate::SpotPrice { symbol, price, ts });
            }
        }

        // Load LOB ticks
        let lob_files = parquet_files(data_dir, "binance_lob_ticks", &dates);
        if !lob_files.is_empty() {
            let files_sql = files_to_sql(&lob_files);
            let sample = options.lob_sample_secs as i64;
            let mut stmt = conn.prepare(&format!(
                "SELECT DISTINCT ON (symbol, epoch(event_time) // {sample})
                        symbol, event_time, best_bid, best_ask, bid_qty, ask_qty, obi
                 FROM read_parquet([{files_sql}])
                 WHERE symbol IN ({symbols_sql})
                   AND event_time >= '{from}'
                   AND event_time <= '{to}'
                 ORDER BY symbol, epoch(event_time) // {sample}, event_time DESC"
            ))?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, DateTime<Utc>>(1)?,
                    row.get::<_, f64>(2)?,
                    row.get::<_, f64>(3)?,
                    row.get::<_, f64>(4)?,
                    row.get::<_, f64>(5)?,
                    row.get::<_, f64>(6).unwrap_or(0.0),
                ))
            })?;
            for row in rows {
                let (symbol, ts, best_bid, best_ask, bid_qty, ask_qty, obi) = row?;
                updates.push(MarketUpdate::L2Depth {
                    symbol, ts,
                    best_bid: rust_decimal::Decimal::try_from(best_bid).unwrap_or_default(),
                    best_ask: rust_decimal::Decimal::try_from(best_ask).unwrap_or_default(),
                    bid_qty: rust_decimal::Decimal::try_from(bid_qty).unwrap_or_default(),
                    ask_qty: rust_decimal::Decimal::try_from(ask_qty).unwrap_or_default(),
                    obi,
                });
            }
        }

        // Load PM market metadata (EventDiscovered + EventExpired)
        let meta_files = parquet_files(data_dir, "pm_market_metadata", &dates);
        if !meta_files.is_empty() {
            let files_sql = files_to_sql(&meta_files);
            let mut stmt = conn.prepare(&format!(
                "SELECT market_slug, symbol, up_token_id, down_token_id,
                        start_time, end_time, price_to_beat
                 FROM read_parquet([{files_sql}])
                 WHERE symbol IN ({symbols_sql})
                   AND start_time >= '{from}'
                   AND start_time <= '{to}'"
            ))?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, DateTime<Utc>>(4)?,
                    row.get::<_, DateTime<Utc>>(5)?,
                    row.get::<_, Option<rust_decimal::Decimal>>(6)?,
                ))
            })?;
            for row in rows {
                let (event_id, symbol, up_token, down_token, start, end, ptb) = row?;
                let window_secs = (end - start).num_seconds() as u64;
                updates.push(MarketUpdate::EventDiscovered {
                    event_id: event_id.clone(), symbol: symbol.clone(),
                    up_token: up_token.clone(), down_token: down_token.clone(),
                    end_time: end, window_secs, price_to_beat: ptb,
                    resolved_up_won: None,
                });
                updates.push(MarketUpdate::EventExpired {
                    event_id, symbol, up_token, down_token,
                    end_time: end, resolved_up_won: None,
                });
            }
        }

        // Load CLOB quotes
        let quote_files = parquet_files(data_dir, "clob_quote_ticks", &dates);
        if !quote_files.is_empty() {
            let files_sql = files_to_sql(&quote_files);
            let mut stmt = conn.prepare(&format!(
                "SELECT DISTINCT ON (date_trunc('second', received_at), token_id)
                        token_id, received_at, best_bid, best_ask, bid_size, ask_size
                 FROM read_parquet([{files_sql}])
                 WHERE received_at >= '{from}'
                   AND received_at <= '{to}'
                 ORDER BY date_trunc('second', received_at), token_id, received_at DESC"
            ))?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, DateTime<Utc>>(1)?,
                    row.get::<_, Option<rust_decimal::Decimal>>(2)?,
                    row.get::<_, Option<rust_decimal::Decimal>>(3)?,
                    row.get::<_, Option<rust_decimal::Decimal>>(4)?,
                    row.get::<_, Option<rust_decimal::Decimal>>(5)?,
                ))
            })?;
            for row in rows {
                let (token_id, ts, bid, ask, bid_sz, ask_sz) = row?;
                if let (Some(best_bid), Some(best_ask)) = (bid, ask) {
                    updates.push(MarketUpdate::Quote {
                        token_id, ts, best_bid, best_ask,
                        bid_size: bid_sz.unwrap_or_default(),
                        ask_size: ask_sz.unwrap_or_default(),
                    });
                }
            }
        }

        updates.sort_by_key(|u| u.timestamp());
        Ok(updates)
    }
}

fn date_range(from: NaiveDate, to: NaiveDate) -> Vec<NaiveDate> {
    let mut dates = vec![];
    let mut d = from;
    while d <= to {
        dates.push(d);
        d = d.succ_opt().unwrap_or(d);
    }
    dates
}

fn parquet_files(data_dir: &str, table: &str, dates: &[NaiveDate]) -> Vec<String> {
    dates
        .iter()
        .map(|d| format!("{}/{}/{}.parquet", data_dir, table, d))
        .filter(|p| Path::new(p).exists())
        .collect()
}

fn files_to_sql(files: &[String]) -> String {
    files.iter().map(|f| format!("'{f}'")).collect::<Vec<_>>().join(", ")
}
```

- [ ] **Step 4: Export parquet module in mod.rs**

In `crates/ploy-strategy-bundles/src/feed/mod.rs`, add:

```rust
#[cfg(feature = "parquet-feed")]
pub mod parquet;
```

- [ ] **Step 5: Run test**

```bash
cargo test -p ploy-strategy-bundles --features parquet-feed feed::parquet
```
Expected: PASS (empty result for nonexistent dir).

- [ ] **Step 6: Compile check**

```bash
cargo check -p ploy-strategy-bundles --features parquet-feed --example run_backtest
```

- [ ] **Step 7: Commit**

```bash
git add crates/ploy-strategy-bundles/src/feed/parquet.rs \
        crates/ploy-strategy-bundles/src/feed/mod.rs
git commit -m "feat(feed): add DuckDB Parquet feed loader"
```

---

## Phase 3 — Wire into run_backtest

### Task 4: Add --data-dir flag to run_backtest

**Files:**
- Modify: `crates/ploy-strategy-bundles/examples/run_backtest.rs`

- [ ] **Step 1: Add --data-dir parsing**

Find the `flag_value` calls block and add:

```rust
let data_dir = flag_value(&args, "--data-dir");
```

- [ ] **Step 2: Dispatch to parquet loader when --data-dir is set**

Find the block that calls `load_from_database_with_options` and wrap it:

```rust
let data = if let Some(ref dir) = data_dir {
    #[cfg(feature = "parquet-feed")]
    {
        eprintln!("Loading Parquet data from: {dir}");
        ploy_strategy_bundles::feed::parquet::load_from_parquet(
            dir,
            &symbols,
            from,
            to,
            &backtest_options,
        )?
    }
    #[cfg(not(feature = "parquet-feed"))]
    {
        panic!("--data-dir requires parquet-feed feature");
    }
} else {
    // existing PostgreSQL path
    load_from_database_with_options(&pool, &symbols, from, to, &backtest_options).await?
};
```

- [ ] **Step 3: Update build in backtest.yml to enable feature**

In `.github/workflows/backtest.yml`, change the build command:

```yaml
- name: Build run_backtest
  run: |
    . /home/runner/.cargo/env
    cargo build --release --locked \
      -p ploy-strategy-bundles \
      --features ploy-strategy-bundles/parquet-feed \
      --example run_backtest
```

- [ ] **Step 4: Compile check**

```bash
cargo check -p ploy-strategy-bundles --features parquet-feed --example run_backtest
```

- [ ] **Step 5: Commit**

```bash
git add crates/ploy-strategy-bundles/examples/run_backtest.rs \
        .github/workflows/backtest.yml
git commit -m "feat(backtest): add --data-dir flag for Parquet-backed backtest"
```

---

## Phase 4 — Workflow: rsync + run with Parquet

### Task 5: Update backtest.yml to rsync Parquet files

**Files:**
- Modify: `.github/workflows/backtest.yml`

- [ ] **Step 1: Add SSH key secret and rsync step**

Add `data_dir` input to workflow_dispatch:

```yaml
      data_dir:
        description: "Local Parquet data dir (leave empty to use DB)"
        required: false
        default: "/tmp/ploy-parquet"
```

Add rsync step before "Run backtest":

```yaml
      - name: Sync Parquet data from Tango-1-1
        if: ${{ github.event.inputs.data_dir != '' }}
        env:
          SSH_KEY: ${{ secrets.TANGO_SSH_KEY }}
        run: |
          mkdir -p ~/.ssh
          echo "$SSH_KEY" > ~/.ssh/tango_key
          chmod 600 ~/.ssh/tango_key
          mkdir -p ${{ github.event.inputs.data_dir }}
          rsync -az --progress \
            -e "ssh -i ~/.ssh/tango_key -o StrictHostKeyChecking=no" \
            root@172.16.0.204:/opt/ploy/data/parquet/ \
            ${{ github.event.inputs.data_dir }}/
```

Update "Run backtest" step to pass `--data-dir` when set:

```yaml
      - name: Run backtest
        env:
          CONFIG: ${{ github.event.inputs.config }}
          START_DATE: ${{ github.event.inputs.start_date }}
          END_DATE: ${{ github.event.inputs.end_date }}
          DATA_DIR: ${{ github.event.inputs.data_dir }}
        run: |
          EXTRA_ARGS=""
          if [ -n "$DATA_DIR" ]; then
            EXTRA_ARGS="--data-dir $DATA_DIR"
          fi
          ./target/release/examples/run_backtest \
            --config "${CONFIG}" \
            --db-url "postgresql://postgres:postgres@172.16.0.204:5432/ploy" \
            --start-date "${START_DATE}" \
            --end-date "${END_DATE}" \
            $EXTRA_ARGS
```

- [ ] **Step 2: Add TANGO_SSH_KEY to GitHub secrets**

In GitHub repo → Settings → Secrets → Actions, add `TANGO_SSH_KEY` with the private key that has access to Tango-1-1.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/backtest.yml
git commit -m "ci(backtest): add rsync Parquet sync step and --data-dir support"
```

---

## Phase 5 — Validation

### Task 6: End-to-end test

- [ ] **Step 1: Verify Parquet files exist on Tango-1-1**

```bash
ssh tango-1-1 "ls -lh /opt/ploy/data/parquet/binance_price_ticks/ | tail -5"
```
Expected: `.parquet` files for recent dates.

- [ ] **Step 2: Trigger backtest with data_dir set**

```bash
gh workflow run backtest.yml \
  --repo proerror77/ploy \
  --ref feat/ack-migration-prep \
  --field git_ref=feat/ack-migration-prep \
  --field start_date=2026-04-11 \
  --field end_date=2026-04-17 \
  --field config=config/strategies/02-pm5d-threelayer.unified.toml \
  --field data_dir=/tmp/ploy-parquet
```

- [ ] **Step 3: Measure data load time**

In the job log, look for:
```
Loading Parquet data from: /tmp/ploy-parquet
Loaded XXXXXX market updates
```
Expected: load time < 60 seconds (vs 13 minutes with PostgreSQL).

- [ ] **Step 4: Verify results match PostgreSQL baseline**

Compare trade count and P&L with the Apr 11-17 PostgreSQL run:
- Trades: ~244
- Net P&L: ~+$119

Acceptable variance: ±5% (due to LOB downsampling differences).

- [ ] **Step 5: Push branch and open PR**

```bash
git push origin feat/ack-migration-prep
gh pr create --title "feat: Parquet backtest pipeline — 13min → <60s data load" \
  --body "Replaces PostgreSQL network transfer with local Parquet files. Daily export cron on Tango-1-1, rsync to ploy-ci-1, DuckDB reads locally."
```

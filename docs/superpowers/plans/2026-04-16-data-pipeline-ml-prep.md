# Data Pipeline ML Prep Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix two data pipeline gaps blocking ML training: (1) add a real-time Binance aggTrades WebSocket collector that persists to `binance_agg_trade_ticks`, and (2) add Parquet export to `ploy-research` so `FactorObservation` datasets can be consumed by ML training scripts.

**Architecture:** Task 1 adds `spawn_agg_trade_ws_feed()` to `crates/ploy-market-data/src/feeds.rs` — it opens a direct Binance WebSocket stream per symbol and persists each aggTrade with `ON CONFLICT DO NOTHING`. Task 2 adds the `parquet` feature to polars in the workspace `Cargo.toml` and a new `export_observations_parquet()` function to `crates/ploy-research/src/factors.rs`. Task 3 wires a `--export-parquet <path>` flag into the existing `factor_research.rs` example.

**Tech Stack:** Rust, `tokio-tungstenite 0.28` (already in `ploy-market-data`), `polars 0.46` with `parquet` feature, `sqlx`, `serde_json`

---

## File Map

| File | Change |
|------|--------|
| `crates/ploy-market-data/src/feeds.rs` | Add `spawn_agg_trade_ws_feed()` |
| `Cargo.toml` (workspace) | Add `parquet` to polars features |
| `crates/ploy-research/src/factors.rs` | Add `export_observations_parquet()` + add `spot_move_since_pm_quote` to `observations_to_frame` |
| `crates/ploy-research/examples/factor_research.rs` | Add `--export-parquet <path>` CLI flag |

---

## Phase 1 — Real-Time aggTrades WebSocket Collector

### Task 1: `spawn_agg_trade_ws_feed()` in feeds.rs

**Files:**
- Modify: `crates/ploy-market-data/src/feeds.rs`

The existing `spawn_db_aggtrade_feed()` only polls the database every 2 seconds. This task adds a real WebSocket collector that connects directly to Binance and persists each trade.

Binance aggTrade stream URL: `wss://stream.binance.com:9443/ws/<SYMBOL_LOWER>@aggTrade`

Binance aggTrade message format:
```json
{
  "e": "aggTrade",
  "E": 1672515782136,
  "s": "BTCUSDT",
  "a": 5933014,
  "p": "0.001",
  "q": "100",
  "f": 100,
  "l": 105,
  "T": 1672515782136,
  "m": true
}
```

- [ ] **Step 1: Write the failing test**

Add this test at the bottom of `crates/ploy-market-data/src/feeds.rs` inside the existing `#[cfg(test)] mod tests` block:

```rust
#[test]
fn parse_agg_trade_message_extracts_fields() {
    let msg = serde_json::json!({
        "e": "aggTrade",
        "s": "BTCUSDT",
        "a": 12345_i64,
        "p": "50000.00",
        "q": "0.01",
        "f": 100_i64,
        "l": 105_i64,
        "T": 1672515782136_i64,
        "m": true
    });
    let parsed = parse_agg_trade_msg(&msg).unwrap();
    assert_eq!(parsed.symbol, "BTCUSDT");
    assert_eq!(parsed.agg_trade_id, 12345);
    assert!((parsed.price.to_f64().unwrap() - 50000.0).abs() < 0.01);
    assert!(parsed.is_buyer_maker);
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo test -p ploy-market-data parse_agg_trade_message 2>&1 | tail -5
```
Expected: compile error — `parse_agg_trade_msg` not defined.

- [ ] **Step 3: Add the `AggTradeMsg` struct and `parse_agg_trade_msg` function**

Add after the existing imports in `crates/ploy-market-data/src/feeds.rs`:

```rust
#[derive(Debug)]
struct AggTradeMsg {
    symbol: String,
    agg_trade_id: i64,
    first_trade_id: i64,
    last_trade_id: i64,
    price: rust_decimal::Decimal,
    quantity: rust_decimal::Decimal,
    trade_time: DateTime<Utc>,
    event_time: DateTime<Utc>,
    is_buyer_maker: bool,
}

fn parse_agg_trade_msg(v: &Value) -> Option<AggTradeMsg> {
    use chrono::TimeZone;
    let symbol = v["s"].as_str()?.to_string();
    let agg_trade_id = v["a"].as_i64()?;
    let first_trade_id = v["f"].as_i64().unwrap_or(0);
    let last_trade_id = v["l"].as_i64().unwrap_or(0);
    let price_str = v["p"].as_str()?;
    let qty_str = v["q"].as_str()?;
    let trade_time_ms = v["T"].as_i64()?;
    let event_time_ms = v["E"].as_i64().unwrap_or(trade_time_ms);
    let is_buyer_maker = v["m"].as_bool().unwrap_or(false);

    let price = price_str.parse::<rust_decimal::Decimal>().ok()?;
    let quantity = qty_str.parse::<rust_decimal::Decimal>().ok()?;
    let trade_time = Utc.timestamp_millis_opt(trade_time_ms).single()?;
    let event_time = Utc.timestamp_millis_opt(event_time_ms).single()?;

    Some(AggTradeMsg {
        symbol,
        agg_trade_id,
        first_trade_id,
        last_trade_id,
        price,
        quantity,
        trade_time,
        event_time,
        is_buyer_maker,
    })
}
```

- [ ] **Step 4: Run to verify the test passes**

```bash
cargo test -p ploy-market-data parse_agg_trade_message 2>&1 | tail -5
```
Expected: 1 test passes.

- [ ] **Step 5: Add `spawn_agg_trade_ws_feed()` function**

Add after `spawn_db_aggtrade_feed()` in `crates/ploy-market-data/src/feeds.rs`:

```rust
/// Spawn a task that subscribes to Binance aggTrade WebSocket streams
/// and persists each trade to `binance_agg_trade_ticks`.
///
/// Connects to `wss://stream.binance.com:9443/ws/<symbol>@aggTrade` for each symbol.
/// Uses ON CONFLICT DO NOTHING for idempotent inserts.
/// Reconnects automatically on disconnect with 5-second backoff.
pub fn spawn_agg_trade_ws_feed(
    symbols: Vec<String>,
    pool: PgPool,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        use futures::SinkExt;
        use futures::StreamExt;
        use tokio_tungstenite::connect_async;
        use tokio_tungstenite::tungstenite::Message;

        let symbols_lower: Vec<String> = symbols.iter().map(|s| s.to_lowercase()).collect();
        // Build combined stream URL: /stream?streams=btcusdt@aggTrade/ethusdt@aggTrade
        let streams = symbols_lower
            .iter()
            .map(|s| format!("{}@aggTrade", s))
            .collect::<Vec<_>>()
            .join("/");
        let url = format!("wss://stream.binance.com:9443/stream?streams={}", streams);

        let mut trade_count = 0u64;

        loop {
            info!(url = %url, "Connecting to Binance aggTrade WebSocket");
            let ws_stream = match connect_async(&url).await {
                Ok((ws, _)) => ws,
                Err(e) => {
                    error!(error = %e, "aggTrade WS connect failed, retrying in 5s");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    continue;
                }
            };

            let (mut _write, mut read) = ws_stream.split();

            while let Some(msg) = read.next().await {
                let text = match msg {
                    Ok(Message::Text(t)) => t,
                    Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => continue,
                    Ok(Message::Close(_)) | Err(_) => break,
                    _ => continue,
                };

                let v: Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                // Combined stream wraps payload in {"stream":"...","data":{...}}
                let data = v.get("data").unwrap_or(&v);

                let trade = match parse_agg_trade_msg(data) {
                    Some(t) => t,
                    None => continue,
                };

                let result = sqlx::query(
                    r#"
                    INSERT INTO binance_agg_trade_ticks
                        (symbol, agg_trade_id, first_trade_id, last_trade_id,
                         price, quantity, trade_time, event_time, is_buyer_maker, source)
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'binance_agg_trade_ws')
                    ON CONFLICT (symbol, agg_trade_id) DO NOTHING
                    "#,
                )
                .bind(&trade.symbol)
                .bind(trade.agg_trade_id)
                .bind(trade.first_trade_id)
                .bind(trade.last_trade_id)
                .bind(trade.price)
                .bind(trade.quantity)
                .bind(trade.trade_time)
                .bind(trade.event_time)
                .bind(trade.is_buyer_maker)
                .execute(&pool)
                .await;

                if let Err(e) = result {
                    warn!(error = %e, "Failed to persist aggTrade");
                    continue;
                }

                trade_count += 1;
                if trade_count % 500 == 0 {
                    info!(trades = trade_count, "aggTrade WS feed persisted trades");
                }
            }

            warn!("aggTrade WS disconnected, reconnecting in 5s");
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    })
}
```

- [ ] **Step 6: Verify build**

```bash
cargo build -p ploy-market-data 2>&1 | tail -10
```
Expected: compiles without error.

- [ ] **Step 7: Commit**

```bash
git add crates/ploy-market-data/src/feeds.rs
git commit -m "feat(market-data): add spawn_agg_trade_ws_feed for real-time Binance aggTrade collection"
```

---

## Phase 2 — Parquet Training Data Export

### Task 2: Add `parquet` feature to polars and `export_observations_parquet()`

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/ploy-research/src/factors.rs`

The existing `observations_to_frame()` builds a Polars DataFrame but doesn't include `spot_move_since_pm_quote` and has no export path. This task fixes both.

- [ ] **Step 1: Add `parquet` feature to workspace polars**

In `Cargo.toml` (workspace root), find the polars line and update it:

```toml
# Before:
polars = { version = "0.46", default-features = false, features = ["lazy", "fmt", "strings", "dtype-datetime", "dtype-date"] }

# After:
polars = { version = "0.46", default-features = false, features = ["lazy", "fmt", "strings", "dtype-datetime", "dtype-date", "parquet"] }
```

- [ ] **Step 2: Verify build**

```bash
cargo build -p ploy-research 2>&1 | tail -5
```
Expected: compiles without error.

- [ ] **Step 3: Write the failing test**

Add inside the existing `#[cfg(test)] mod tests` block in `crates/ploy-research/src/factors.rs`:

```rust
#[test]
fn export_observations_parquet_writes_file() {
    use chrono::Utc;
    use std::path::Path;

    let obs = vec![FactorObservation {
        event_id: "e1".into(),
        symbol: "BTCUSDT".into(),
        tick_ts: Utc::now(),
        time_remaining_secs: 120,
        signed_distance_to_beat: 0.1,
        abs_distance_to_beat: 0.1,
        drift_10s: 0.001,
        drift_30s: 0.002,
        flip_age_secs: 5.0,
        post_flip_drift: 0.0,
        sigma_horizon: 0.02,
        fair_prob_up: 0.55,
        fair_prob_up_clean: 0.54,
        prob_disagreement: 0.01,
        implied_sigma_horizon: 0.021,
        vol_gap: 0.001,
        distance_over_sigma: 0.5,
        model_prob_up: 0.56,
        model_edge_up: 0.06,
        reward_risk_up: 1.5,
        reward_risk_down: 0.8,
        obi: 0.1,
        spread_bps: 5.0,
        microprice_offset_bps: 1.0,
        bid_depth_near: 100.0,
        ask_depth_near: 90.0,
        depth_ratio: 1.1,
        depth_imbalance: 0.05,
        depth_far_ratio: 0.9,
        depth_acceleration: 0.0,
        obi_10: 0.08,
        pm_up_bid: 0.55,
        pm_up_ask: 0.56,
        pm_up_bid_size: 500.0,
        pm_up_ask_size: 400.0,
        pm_down_bid: 0.43,
        pm_down_ask: 0.44,
        pm_down_bid_size: 300.0,
        pm_down_ask_size: 350.0,
        pm_lag_secs: 0.5,
        settlement_up: 1.0,
        future_up_ask_change_30s: Some(0.02),
        future_up_ask_change_60s: None,
        cum_obi_delta_5m: 0.01,
        cum_depth_delta_5m: 5.0,
        cum_mprice_drift_5m: 0.003,
        cum_trade_imbalance_5m: 0.02,
        spot_move_since_pm_quote: 0.001,
    }];

    let tmp = std::env::temp_dir().join("test_obs_export.parquet");
    export_observations_parquet(&obs, &tmp).expect("export should succeed");
    assert!(tmp.exists(), "parquet file should be created");
    std::fs::remove_file(&tmp).ok();
}
```

- [ ] **Step 4: Run to verify it fails**

```bash
cargo test -p ploy-research export_observations_parquet_writes_file 2>&1 | tail -5
```
Expected: compile error — `export_observations_parquet` not defined.

- [ ] **Step 5: Fix `observations_to_frame` to include `spot_move_since_pm_quote`**

In `crates/ploy-research/src/factors.rs`, find `observations_to_frame` (around line 1261). The function currently ends before `spot_move_since_pm_quote`. Add the missing column before the closing `]`:

Find this line near the end of the `df![]` macro:
```rust
        "future_up_ask_change_60s" => rows.iter().map(|row| row.future_up_ask_change_60s.unwrap_or(f64::NAN)).collect::<Vec<_>>(),
    ]
```

Replace with:
```rust
        "future_up_ask_change_60s" => rows.iter().map(|row| row.future_up_ask_change_60s.unwrap_or(f64::NAN)).collect::<Vec<_>>(),
        "spot_move_since_pm_quote" => rows.iter().map(|row| row.spot_move_since_pm_quote).collect::<Vec<_>>(),
    ]
```

- [ ] **Step 6: Add `export_observations_parquet()` function**

Add after `observations_to_frame()` in `crates/ploy-research/src/factors.rs`:

```rust
/// Export a slice of `FactorObservation` to a Parquet file at `path`.
///
/// Creates or overwrites the file. The schema matches `observations_to_frame`.
/// Returns an error if the DataFrame cannot be built or the file cannot be written.
pub fn export_observations_parquet(
    rows: &[FactorObservation],
    path: &std::path::Path,
) -> PolarsResult<()> {
    use polars::io::parquet::ParquetWriter;
    use std::fs::File;

    let mut df = observations_to_frame(rows)?;
    let file = File::create(path).map_err(|e| {
        polars::error::PolarsError::IO {
            error: std::sync::Arc::new(e),
            msg: None,
        }
    })?;
    ParquetWriter::new(file).finish(&mut df)?;
    Ok(())
}
```

- [ ] **Step 7: Run to verify the test passes**

```bash
cargo test -p ploy-research export_observations_parquet_writes_file 2>&1 | tail -5
```
Expected: 1 test passes.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml crates/ploy-research/src/factors.rs
git commit -m "feat(research): add Parquet export for FactorObservation training datasets"
```

---

## Phase 3 — Wire Export into factor_research Example

### Task 3: Add `--export-parquet <path>` to factor_research.rs

**Files:**
- Modify: `crates/ploy-research/examples/factor_research.rs`

The `factor_research` binary already computes `FactorObservation` vectors. This task adds a CLI flag to trigger Parquet export after computation.

- [ ] **Step 1: Find the CLI args struct in factor_research.rs**

```bash
grep -n "struct.*Args\|clap\|#\[derive.*Parser\|export" crates/ploy-research/examples/factor_research.rs | head -20
```

- [ ] **Step 2: Add `export_parquet` field to the Args struct**

Find the `Args` struct (it uses `#[derive(Parser)]`). Add the new field:

```rust
/// If set, export all FactorObservations to a Parquet file at this path.
#[arg(long, value_name = "PATH")]
export_parquet: Option<std::path::PathBuf>,
```

- [ ] **Step 3: Add export call after observations are built**

Find the section in `main()` where `build_factor_observations_with_lob` or `build_factor_observations` is called and observations are collected. After the observations vector is populated, add:

```rust
if let Some(ref parquet_path) = args.export_parquet {
    tracing::info!(path = %parquet_path.display(), observations = all_observations.len(), "Exporting observations to Parquet");
    ploy_research::export_observations_parquet(&all_observations, parquet_path)
        .expect("Parquet export failed");
    tracing::info!(path = %parquet_path.display(), "Parquet export complete");
}
```

Note: `all_observations` is the name of the collected observations vector — check the actual variable name in the file and use that.

- [ ] **Step 4: Re-export `export_observations_parquet` from `ploy_research::lib`**

In `crates/ploy-research/src/lib.rs`, add to the existing `pub use factors::` line:

```rust
pub use factors::export_observations_parquet;
```

- [ ] **Step 5: Verify build**

```bash
cargo build -p ploy-research --example factor_research 2>&1 | tail -10
```
Expected: compiles without error.

- [ ] **Step 6: Commit**

```bash
git add crates/ploy-research/examples/factor_research.rs crates/ploy-research/src/lib.rs
git commit -m "feat(research): add --export-parquet flag to factor_research example"
```

---

## Self-Review Checklist

- [x] **Spec coverage:** aggTrades real-time WS collector (Task 1) + Parquet export (Task 2) + CLI flag (Task 3) — all gaps covered
- [x] **Placeholder scan:** No TBD/TODO. Task 3 Step 3 notes to check the actual variable name — this is intentional since the file is 2476 lines and the variable name must be verified at runtime
- [x] **Type consistency:** `AggTradeMsg` defined in Task 1 Step 3, used in Task 1 Step 5. `export_observations_parquet` defined in Task 2 Step 6, re-exported in Task 3 Step 4, tested in Task 2 Step 3
- [x] **`spot_move_since_pm_quote`:** Added to `observations_to_frame` in Task 2 Step 5 — was missing from the existing function
- [x] **ON CONFLICT:** Uses `(symbol, agg_trade_id)` unique constraint from migration 035 — correct
- [x] **Combined stream URL:** `wss://stream.binance.com:9443/stream?streams=btcusdt@aggTrade/ethusdt@aggTrade` — Binance combined stream format, handles multiple symbols in one connection

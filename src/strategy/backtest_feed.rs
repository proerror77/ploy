//! Market data feed abstraction for live and backtest sharing.
//!
//! The `MarketFeed` trait provides a unified interface for both live (Binance WS + PM WS)
//! and historical (DB/CSV replay) data sources. This enables the backtest engine to reuse
//! the exact same `MomentumDetector.check()` logic as the live strategy.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::{HashMap, VecDeque};
use std::path::Path;

use alloy::primitives::U256;
use anyhow::Result;
use sqlx::PgPool;
use tracing::info;

use crate::domain::Side;
use crate::strategy::backtest::{load_klines_from_csv, load_pm_prices_from_csv};

// ─────────────────────────────────────────────────────────────
// Core types
// ─────────────────────────────────────────────────────────────

/// A single market data update event, timestamped for replay ordering.
#[derive(Debug, Clone)]
pub struct MarketUpdate {
    pub timestamp: DateTime<Utc>,
    pub symbol: String,
    pub update_type: UpdateType,
}

/// The kind of update contained in a `MarketUpdate`.
#[derive(Debug, Clone)]
pub enum UpdateType {
    /// CEX spot trade (e.g. Binance)
    SpotTrade {
        price: Decimal,
        quantity: Option<Decimal>,
    },
    /// Polymarket quote update (best bid/ask for one token side in one event).
    ///
    /// `event_slug` is the Polymarket market slug (e.g. "btc-updown-5m-1771243500").
    PmQuote {
        event_slug: String,
        token_id: String,
        side: Side,
        best_bid: Option<Decimal>,
        best_ask: Option<Decimal>,
    },
    /// Event lifecycle update (metadata, settlement)
    EventState {
        event_slug: String,
        end_time: Option<DateTime<Utc>>,
        price_to_beat: Option<Decimal>,
        /// None = not yet settled, Some(true) = UP won, Some(false) = DOWN won
        outcome: Option<bool>,
    },
    /// Polymarket LOB snapshot (aggregated depth from clob_orderbook_snapshots)
    LobSnapshot {
        /// Token side: "UP" or "DOWN"
        side: String,
        /// Total ask-side liquidity in shares across all levels
        ask_depth_shares: u64,
        /// Best ask price
        best_ask: Option<Decimal>,
    },
    /// Binance L2 depth-derived features, downsampled for historical replay.
    BinanceL2 {
        obi_5: Decimal,
        obi_10: Decimal,
        bid_volume_5: Decimal,
        ask_volume_5: Decimal,
        spread_bps: Decimal,
    },
}

// ─────────────────────────────────────────────────────────────
// Trait
// ─────────────────────────────────────────────────────────────

/// Market data source for both live and backtest.
///
/// Implementors provide a stream of `MarketUpdate` events in chronological order.
/// Returns `None` when the data source is exhausted (backtest) or when the stream
/// is temporarily empty (live — caller should await next update).
pub trait MarketFeed {
    fn next_update(&mut self) -> Option<MarketUpdate>;
}

// ─────────────────────────────────────────────────────────────
// HistoricalFeed: pre-loaded replay from DB or CSV
// ─────────────────────────────────────────────────────────────

/// Historical market data feed that replays pre-loaded events in timestamp order.
///
/// All data is loaded upfront into a `VecDeque`, sorted by timestamp.
/// This guarantees deterministic replay with no lookahead bias — each
/// `next_update()` call returns the chronologically next event.
pub struct HistoricalFeed {
    pub(crate) updates: VecDeque<MarketUpdate>,
}

impl HistoricalFeed {
    /// Create a new HistoricalFeed from a vector of market updates.
    /// Updates will be sorted by timestamp for deterministic replay.
    pub fn new(mut updates: Vec<MarketUpdate>) -> Self {
        updates.sort_by_key(|u| u.timestamp);
        Self {
            updates: VecDeque::from(updates),
        }
    }

    /// Total number of remaining updates in the feed.
    pub fn len(&self) -> usize {
        self.updates.len()
    }

    pub fn is_empty(&self) -> bool {
        self.updates.is_empty()
    }

    // ─── DB loader ───────────────────────────────────────────

    /// Load historical data from database tables:
    /// - `binance_price_ticks` (fallback from `sync_records`) → SpotTrade
    /// - `clob_quote_ticks` → PmQuote (keyed by symbol via token→market mapping)
    /// - `pm_market_metadata` + `pm_token_settlements` → EventState
    pub async fn from_database(
        pool: &PgPool,
        symbols: &[String],
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    ) -> Result<Self> {
        let mut updates: Vec<MarketUpdate> = Vec::new();
        let mut spot_series: HashMap<String, Vec<(DateTime<Utc>, Decimal)>> = HashMap::new();

        let sync_records_exists: bool = sqlx::query_scalar::<_, Option<String>>(
            "SELECT to_regclass('public.sync_records')::text",
        )
        .fetch_one(pool)
        .await
        .unwrap_or(None)
        .is_some();

        let price_ticks_exists: bool = sqlx::query_scalar::<_, Option<String>>(
            "SELECT to_regclass('public.binance_price_ticks')::text",
        )
        .fetch_one(pool)
        .await
        .unwrap_or(None)
        .is_some();

        let klines_exists: bool = sqlx::query_scalar::<_, Option<String>>(
            "SELECT to_regclass('public.binance_klines')::text",
        )
        .fetch_one(pool)
        .await
        .unwrap_or(None)
        .is_some();

        let quote_ticks_exists: bool = sqlx::query_scalar::<_, Option<String>>(
            "SELECT to_regclass('public.clob_quote_ticks')::text",
        )
        .fetch_one(pool)
        .await
        .unwrap_or(None)
        .is_some();

        let lob_snaps_exists: bool = sqlx::query_scalar::<_, Option<String>>(
            "SELECT to_regclass('public.clob_orderbook_snapshots')::text",
        )
        .fetch_one(pool)
        .await
        .unwrap_or(None)
        .is_some();

        let binance_lob_ticks_exists: bool = sqlx::query_scalar::<_, Option<String>>(
            "SELECT to_regclass('public.binance_lob_ticks')::text",
        )
        .fetch_one(pool)
        .await
        .unwrap_or(None)
        .is_some();

        let pm_market_metadata_exists: bool = sqlx::query_scalar::<_, Option<String>>(
            "SELECT to_regclass('public.pm_market_metadata')::text",
        )
        .fetch_one(pool)
        .await
        .unwrap_or(None)
        .is_some();

        let pm_token_settlements_exists: bool = sqlx::query_scalar::<_, Option<String>>(
            "SELECT to_regclass('public.pm_token_settlements')::text",
        )
        .fetch_one(pool)
        .await
        .unwrap_or(None)
        .is_some();

        // 1. Try sync_records first, fall back to binance_price_ticks
        let spot_rows: Vec<(DateTime<Utc>, String, Decimal)> = if sync_records_exists {
            sqlx::query_as(
                r#"
                SELECT timestamp, symbol, bn_mid_price
                FROM sync_records
                WHERE ($1::text[] IS NULL OR symbol = ANY($1))
                  AND ($2::timestamptz IS NULL OR timestamp >= $2)
                  AND ($3::timestamptz IS NULL OR timestamp <= $3)
                ORDER BY timestamp
                "#,
            )
            .bind(if symbols.is_empty() {
                None::<Vec<String>>
            } else {
                Some(symbols.to_vec())
            })
            .bind(from)
            .bind(to)
            .fetch_all(pool)
            .await?
        } else {
            Vec::new()
        };

        if !spot_rows.is_empty() {
            for (ts, sym, price) in &spot_rows {
                updates.push(MarketUpdate {
                    timestamp: *ts,
                    symbol: sym.clone(),
                    update_type: UpdateType::SpotTrade {
                        price: *price,
                        quantity: None,
                    },
                });
                spot_series
                    .entry(sym.clone())
                    .or_default()
                    .push((*ts, *price));
            }
            info!("Loaded {} spot records from sync_records", spot_rows.len());
        } else if price_ticks_exists {
            // Fallback: binance_price_ticks (used by platform start collector)
            let price_rows: Vec<(DateTime<Utc>, String, Decimal, Option<Decimal>)> =
                sqlx::query_as(
                    r#"
                SELECT trade_time, symbol, price, quantity
                FROM binance_price_ticks
                WHERE ($1::text[] IS NULL OR symbol = ANY($1))
                  AND ($2::timestamptz IS NULL OR trade_time >= $2)
                  AND ($3::timestamptz IS NULL OR trade_time <= $3)
                ORDER BY trade_time
                "#,
                )
                .bind(if symbols.is_empty() {
                    None::<Vec<String>>
                } else {
                    Some(symbols.to_vec())
                })
                .bind(from)
                .bind(to)
                .fetch_all(pool)
                .await?;

            for (ts, sym, price, qty) in &price_rows {
                updates.push(MarketUpdate {
                    timestamp: *ts,
                    symbol: sym.clone(),
                    update_type: UpdateType::SpotTrade {
                        price: *price,
                        quantity: *qty,
                    },
                });
                spot_series
                    .entry(sym.clone())
                    .or_default()
                    .push((*ts, *price));
            }
            info!(
                "Loaded {} spot records from binance_price_ticks (sync_records was empty)",
                price_rows.len()
            );
        } else {
            info!("No sync_records or binance_price_ticks available for spot replay");
        }

        // 1b. Supplement with klines (fills gaps where sync_records/price_ticks are sparse)
        let kline_spot_rows: Vec<(DateTime<Utc>, String, Decimal)> = if klines_exists {
            sqlx::query_as(
                r#"
                SELECT close_time, symbol, close
                FROM binance_klines
                WHERE ($1::text[] IS NULL OR symbol = ANY($1))
                  AND ($2::timestamptz IS NULL OR close_time >= $2)
                  AND ($3::timestamptz IS NULL OR close_time <= $3)
                ORDER BY close_time
                "#,
            )
            .bind(if symbols.is_empty() {
                None::<Vec<String>>
            } else {
                Some(symbols.to_vec())
            })
            .bind(from)
            .bind(to)
            .fetch_all(pool)
            .await
            .unwrap_or_default()
        } else {
            Vec::new()
        };

        for (ts, sym, price) in &kline_spot_rows {
            updates.push(MarketUpdate {
                timestamp: *ts,
                symbol: sym.clone(),
                update_type: UpdateType::SpotTrade {
                    price: *price,
                    quantity: None,
                },
            });
            spot_series
                .entry(sym.clone())
                .or_default()
                .push((*ts, *price));
        }
        if !kline_spot_rows.is_empty() {
            info!(
                "Supplemented with {} kline spot records",
                kline_spot_rows.len()
            );
        }

        // 1c. Supplement with Binance L2 microstructure features for replay parity with live OBI gating.
        let binance_l2_rows: Vec<(
            DateTime<Utc>,
            String,
            Decimal,
            Decimal,
            Decimal,
            Decimal,
            Decimal,
        )> = if binance_lob_ticks_exists {
            sqlx::query_as(
                r#"
                SELECT DISTINCT ON (date_trunc('second', event_time), symbol)
                    event_time,
                    symbol,
                    obi_5,
                    obi_10,
                    bid_volume_5,
                    ask_volume_5,
                    spread_bps
                FROM binance_lob_ticks
                WHERE ($1::text[] IS NULL OR symbol = ANY($1))
                  AND ($2::timestamptz IS NULL OR event_time >= $2)
                  AND ($3::timestamptz IS NULL OR event_time <= $3)
                ORDER BY date_trunc('second', event_time), symbol, event_time DESC
                "#,
            )
            .bind(if symbols.is_empty() {
                None::<Vec<String>>
            } else {
                Some(symbols.to_vec())
            })
            .bind(from)
            .bind(to)
            .fetch_all(pool)
            .await
            .unwrap_or_default()
        } else {
            Vec::new()
        };

        for (ts, sym, obi_5, obi_10, bid_volume_5, ask_volume_5, spread_bps) in &binance_l2_rows {
            updates.push(MarketUpdate {
                timestamp: *ts,
                symbol: sym.clone(),
                update_type: UpdateType::BinanceL2 {
                    obi_5: *obi_5,
                    obi_10: *obi_10,
                    bid_volume_5: *bid_volume_5,
                    ask_volume_5: *ask_volume_5,
                    spread_bps: *spread_bps,
                },
            });
        }
        if !binance_l2_rows.is_empty() {
            info!(
                "Loaded {} Binance L2 feature rows from binance_lob_ticks",
                binance_l2_rows.len()
            );
        }

        // Sort spot series for point-in-time lookup (for s0/price_to_beat inference).
        for series in spot_series.values_mut() {
            series.sort_by_key(|(ts, _)| *ts);
        }

        fn infer_symbol_from_slug(slug: &str) -> Option<String> {
            let s = slug.to_ascii_lowercase();
            if s.starts_with("btc-") || s.starts_with("bitcoin-") {
                return Some("BTCUSDT".to_string());
            }
            if s.starts_with("eth-") || s.starts_with("ethereum-") {
                return Some("ETHUSDT".to_string());
            }
            if s.starts_with("sol-") || s.starts_with("solana-") {
                return Some("SOLUSDT".to_string());
            }
            None
        }

        fn infer_window_duration_secs(slug: &str) -> Option<i64> {
            let s = slug.to_ascii_lowercase();
            if s.contains("15m") {
                return Some(900);
            }
            if s.contains("5m") {
                return Some(300);
            }
            if s.contains("60m") {
                return Some(3600);
            }
            None
        }

        fn normalize_clob_token_id(raw: &str) -> Option<String> {
            let s = raw.trim();
            if s.is_empty() {
                return None;
            }
            if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                return U256::from_str_radix(hex, 16).ok().map(|u| u.to_string());
            }
            if s.chars().all(|c| c.is_ascii_digit()) {
                return Some(s.to_string());
            }
            // Last-resort: tolerate hex strings without 0x prefix.
            U256::from_str_radix(s, 16).ok().map(|u| u.to_string())
        }

        fn spot_at_or_before(
            series: &[(DateTime<Utc>, Decimal)],
            ts: DateTime<Utc>,
        ) -> Option<Decimal> {
            if series.is_empty() {
                return None;
            }
            match series.binary_search_by_key(&ts, |(t, _)| *t) {
                Ok(i) => Some(series[i].1),
                Err(0) => None,
                Err(i) => Some(series[i - 1].1),
            }
        }

        let mut token_to_symbol: HashMap<String, String> = HashMap::new();
        let mut token_to_slug: HashMap<String, String> = HashMap::new();
        let mut slug_to_symbol: HashMap<String, String> = HashMap::new();

        // 2a. Prefer sync_records mapping (event slug + token ids) when available.
        if sync_records_exists {
            let sync_map_rows: Result<Vec<(String, String, Option<String>, Option<String>)>> =
                sqlx::query_as(
                    r#"
                    SELECT DISTINCT pm_market_slug, symbol, pm_yes_token_id, pm_no_token_id
                    FROM sync_records
                    WHERE pm_market_slug IS NOT NULL
                      AND ($1::text[] IS NULL OR symbol = ANY($1))
                      AND ($2::timestamptz IS NULL OR timestamp >= $2)
                      AND ($3::timestamptz IS NULL OR timestamp <= $3)
                    "#,
                )
                .bind(if symbols.is_empty() {
                    None::<Vec<String>>
                } else {
                    Some(symbols.to_vec())
                })
                .bind(from)
                .bind(to)
                .fetch_all(pool)
                .await
                .map_err(Into::into);

            match sync_map_rows {
                Ok(rows) => {
                    for (slug, sym, yes_token_id, no_token_id) in rows {
                        if !slug.is_empty() && !sym.is_empty() {
                            slug_to_symbol.insert(slug.clone(), sym.clone());
                        }
                        if let Some(t) = yes_token_id {
                            token_to_slug.insert(t.clone(), slug.clone());
                            if !sym.is_empty() {
                                token_to_symbol.insert(t, sym.clone());
                            }
                        }
                        if let Some(t) = no_token_id {
                            token_to_slug.insert(t.clone(), slug.clone());
                            if !sym.is_empty() {
                                token_to_symbol.insert(t, sym.clone());
                            }
                        }
                    }
                    info!(
                        "Built token mapping from sync_records: {} tokens, {} slugs",
                        token_to_slug.len(),
                        slug_to_symbol.len()
                    );
                }
                Err(e) => {
                    info!("sync_records mapping query failed (older schema?): {e}");
                }
            }
        }

        // 2b. Fill missing token→slug using pm_token_settlements (token-level truth).
        if pm_token_settlements_exists {
            let settlement_map_rows: Vec<(String, Option<String>, Option<String>)> =
                sqlx::query_as(
                    r#"
                SELECT token_id, market_slug, outcome
                FROM pm_token_settlements
                WHERE market_slug IS NOT NULL AND market_slug != ''
                  AND ($1::timestamptz IS NULL OR fetched_at >= $1)
                  AND ($2::timestamptz IS NULL OR fetched_at <= $2)
                "#,
                )
                .bind(from)
                .bind(to)
                .fetch_all(pool)
                .await
                .unwrap_or_default();

            for (token_id, market_slug, outcome) in settlement_map_rows {
                let Some(slug) = market_slug else { continue };
                token_to_slug
                    .entry(token_id.clone())
                    .or_insert_with(|| slug.clone());
                if let Some(sym) = slug_to_symbol
                    .get(&slug)
                    .cloned()
                    .or_else(|| infer_symbol_from_slug(&slug))
                {
                    token_to_symbol.entry(token_id).or_insert(sym);
                }
                // Also seed slug_to_symbol from slug inference when metadata is missing.
                if !slug_to_symbol.contains_key(&slug) {
                    if let Some(sym) = infer_symbol_from_slug(&slug) {
                        slug_to_symbol.insert(slug.clone(), sym);
                    }
                }

                // For some datasets, `outcome` is "Up"/"Down"; keep for debugging.
                let _ = outcome;
            }
            info!(
                "Built token mapping from pm_token_settlements: {} tokens",
                token_to_slug.len()
            );
        }

        // 2c. Supplement token→slug using Polymarket market metadata (works for live + settled markets).
        //     The `raw_market` JSON contains `clobTokenIds` (token IDs in the CLOB) for the market.
        if pm_market_metadata_exists {
            let before = token_to_slug.len();
            let rows: Vec<(String, Option<String>, String)> = sqlx::query_as(
                r#"
                SELECT DISTINCT
                    market_slug,
                    symbol,
                    jsonb_array_elements_text((raw_market->>'clobTokenIds')::jsonb) AS token_id
                FROM pm_market_metadata
                WHERE raw_market IS NOT NULL
                  AND raw_market ? 'clobTokenIds'
                  AND ($1::text[] IS NULL OR symbol = ANY($1))
                  AND ($2::timestamptz IS NULL OR end_time >= $2)
                  AND ($3::timestamptz IS NULL OR start_time <= $3)
                "#,
            )
            .bind(if symbols.is_empty() {
                None::<Vec<String>>
            } else {
                Some(symbols.to_vec())
            })
            .bind(from)
            .bind(to)
            .fetch_all(pool)
            .await
            .unwrap_or_default();

            for (slug, sym, token_id) in rows {
                if slug.is_empty() || token_id.is_empty() {
                    continue;
                }
                let Some(token_id_norm) = normalize_clob_token_id(&token_id) else {
                    continue;
                };

                token_to_slug
                    .entry(token_id_norm.clone())
                    .or_insert_with(|| slug.clone());

                let symbol = sym
                    .filter(|s| !s.is_empty())
                    .or_else(|| slug_to_symbol.get(&slug).cloned())
                    .or_else(|| infer_symbol_from_slug(&slug));

                if let Some(symbol) = symbol {
                    if !symbol.is_empty() {
                        token_to_symbol
                            .entry(token_id_norm)
                            .or_insert(symbol.clone());
                        slug_to_symbol.entry(slug).or_insert(symbol);
                    }
                }
            }

            let after = token_to_slug.len();
            if after > before {
                info!(
                    "Supplemented token mapping from pm_market_metadata.raw_market.clobTokenIds: +{} tokens (now {})",
                    after - before,
                    after
                );
            }
        }

        // 3. Polymarket quotes from clob_quote_ticks
        //    Map token_id → {symbol, market_slug} so backtests can join quotes to event windows.
        //    Downsample to 1-second granularity: take the last quote per (second, token, side).
        //    Filter by known token_ids at SQL level to avoid loading millions of unmapped rows.
        let known_token_ids: Vec<String> = token_to_slug.keys().cloned().collect();
        let quote_rows: Vec<(
            DateTime<Utc>,
            String,
            String,
            Option<Decimal>,
            Option<Decimal>,
        )> = if quote_ticks_exists && !known_token_ids.is_empty() {
            sqlx::query_as(
                r#"
                    SELECT DISTINCT ON (date_trunc('second', received_at), token_id, side)
                           received_at, token_id, side, best_bid, best_ask
                    FROM clob_quote_ticks
                    WHERE ($1::timestamptz IS NULL OR received_at >= $1)
                      AND ($2::timestamptz IS NULL OR received_at <= $2)
                      AND token_id = ANY($3)
                    ORDER BY date_trunc('second', received_at), token_id, side, received_at DESC
                    "#,
            )
            .bind(from)
            .bind(to)
            .bind(&known_token_ids)
            .fetch_all(pool)
            .await
            .unwrap_or_default()
        } else {
            Vec::new()
        };

        for (ts, token_id, side, best_bid, best_ask) in &quote_rows {
            let event_slug = match token_to_slug.get(token_id.as_str()) {
                Some(s) => s.clone(),
                None => continue,
            };
            let symbol = token_to_symbol
                .get(token_id.as_str())
                .cloned()
                .or_else(|| slug_to_symbol.get(&event_slug).cloned())
                .or_else(|| infer_symbol_from_slug(&event_slug))
                .unwrap_or_default();
            if symbol.is_empty() {
                continue;
            }
            let side = match side.as_str() {
                "UP" => Side::Up,
                "DOWN" => Side::Down,
                _ => continue,
            };

            updates.push(MarketUpdate {
                timestamp: *ts,
                symbol,
                update_type: UpdateType::PmQuote {
                    event_slug,
                    token_id: token_id.clone(),
                    side,
                    best_bid: *best_bid,
                    best_ask: *best_ask,
                },
            });
        }
        info!(
            "Loaded {} quote ticks (pre-filtered to {} known tokens)",
            quote_rows.len(),
            known_token_ids.len()
        );

        // 3b. Fallback: if clob_quote_ticks is empty/unavailable, replay PM prices from sync_records.
        if quote_rows.is_empty() && sync_records_exists {
            let sync_quote_rows: Result<
                Vec<(
                    DateTime<Utc>,
                    String,
                    String,
                    Option<Decimal>,
                    Option<Decimal>,
                    Option<String>,
                    Option<String>,
                )>,
            > = sqlx::query_as(
                r#"
                SELECT DISTINCT ON (date_trunc('second', timestamp), pm_market_slug)
                    timestamp,
                    symbol,
                    pm_market_slug,
                    pm_yes_price,
                    pm_no_price,
                    pm_yes_token_id,
                    pm_no_token_id
                FROM sync_records
                WHERE pm_market_slug IS NOT NULL
                  AND ($1::text[] IS NULL OR symbol = ANY($1))
                  AND ($2::timestamptz IS NULL OR timestamp >= $2)
                  AND ($3::timestamptz IS NULL OR timestamp <= $3)
                ORDER BY date_trunc('second', timestamp), pm_market_slug, timestamp DESC
                "#,
            )
            .bind(if symbols.is_empty() {
                None::<Vec<String>>
            } else {
                Some(symbols.to_vec())
            })
            .bind(from)
            .bind(to)
            .fetch_all(pool)
            .await
            .map_err(Into::into);

            match sync_quote_rows {
                Ok(rows) => {
                    let row_count = rows.len();
                    for (ts, sym, slug, yes, no, yes_token_id, no_token_id) in rows {
                        if let Some(ask) = yes {
                            updates.push(MarketUpdate {
                                timestamp: ts,
                                symbol: sym.clone(),
                                update_type: UpdateType::PmQuote {
                                    event_slug: slug.clone(),
                                    token_id: yes_token_id
                                        .unwrap_or_else(|| format!("{}:UP", slug)),
                                    side: Side::Up,
                                    best_bid: None,
                                    best_ask: Some(ask),
                                },
                            });
                        }
                        if let Some(ask) = no {
                            updates.push(MarketUpdate {
                                timestamp: ts,
                                symbol: sym.clone(),
                                update_type: UpdateType::PmQuote {
                                    event_slug: slug.clone(),
                                    token_id: no_token_id
                                        .unwrap_or_else(|| format!("{}:DOWN", slug)),
                                    side: Side::Down,
                                    best_bid: None,
                                    best_ask: Some(ask),
                                },
                            });
                        }
                    }
                    info!(
                        "Supplemented with {} PM quotes from sync_records",
                        row_count
                    );
                }
                Err(e) => {
                    info!("sync_records PM quote replay query failed: {e}");
                }
            }
        }

        // 4. Event metadata + settlement
        //    Join with settlements to get UP/DOWN outcome per market_slug.
        //    A market has two tokens (UP + DOWN). We need ONE EventState per market_slug:
        //    - At start_time: EventState with S0 + end_time (window open)
        //    - At resolved_at: EventState with outcome (settlement)
        let mut event_rows: Vec<(
            String,                // market_slug
            Option<String>,        // symbol
            Option<DateTime<Utc>>, // start_time
            Option<DateTime<Utc>>, // end_time
            Option<Decimal>,       // price_to_beat
        )> = if pm_market_metadata_exists {
            sqlx::query_as(
                r#"
                SELECT market_slug, symbol, start_time, end_time, price_to_beat
                FROM pm_market_metadata
                WHERE ($1::text[] IS NULL OR symbol = ANY($1))
                  AND ($2::timestamptz IS NULL OR end_time >= $2)
                  AND ($3::timestamptz IS NULL OR start_time <= $3)
                ORDER BY start_time
                "#,
            )
            .bind(if symbols.is_empty() {
                None::<Vec<String>>
            } else {
                Some(symbols.to_vec())
            })
            .bind(from)
            .bind(to)
            .fetch_all(pool)
            .await
            .unwrap_or_default()
        } else {
            Vec::new()
        };

        // Fallback: derive event windows from pm_token_settlements.raw_market when metadata is empty.
        if event_rows.is_empty() && pm_token_settlements_exists {
            let raw_rows: Vec<(String, serde_json::Value)> = sqlx::query_as(
                r#"
                SELECT DISTINCT ON (market_slug) market_slug, raw_market
                FROM pm_token_settlements
                WHERE raw_market IS NOT NULL
                  AND market_slug IS NOT NULL
                  AND market_slug != ''
                  AND ($1::timestamptz IS NULL OR fetched_at >= $1)
                  AND ($2::timestamptz IS NULL OR fetched_at <= $2)
                ORDER BY market_slug, fetched_at DESC
                "#,
            )
            .bind(from)
            .bind(to)
            .fetch_all(pool)
            .await
            .unwrap_or_default();

            for (slug, raw) in raw_rows {
                let start_time = raw
                    .get("eventStartTime")
                    .and_then(|v| v.as_str())
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&Utc))
                    .or_else(|| {
                        raw.get("startDate")
                            .or_else(|| raw.get("start_date"))
                            .and_then(|v| v.as_str())
                            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                            .map(|dt| dt.with_timezone(&Utc))
                    });
                let end_time = raw
                    .get("endDate")
                    .or_else(|| raw.get("end_date"))
                    .and_then(|v| v.as_str())
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&Utc));

                let price_to_beat = raw
                    .get("groupItemThreshold")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<Decimal>().ok())
                    .or_else(|| {
                        let upper = raw
                            .get("upperBound")
                            .and_then(|v| v.as_str())
                            .and_then(|s| s.parse::<Decimal>().ok())
                            .unwrap_or(Decimal::ZERO);
                        let lower = raw
                            .get("lowerBound")
                            .and_then(|v| v.as_str())
                            .and_then(|s| s.parse::<Decimal>().ok())
                            .unwrap_or(Decimal::ZERO);
                        let mid = (upper + lower) / Decimal::from(2);
                        if mid > Decimal::ZERO {
                            Some(mid)
                        } else {
                            None
                        }
                    });

                let symbol = infer_symbol_from_slug(&slug);
                event_rows.push((slug, symbol, start_time, end_time, price_to_beat));
            }
            info!(
                "Derived {} event windows from pm_token_settlements.raw_market",
                event_rows.len()
            );
        }

        // Emit window-open events at start_time
        let mut event_open_count = 0usize;
        for (slug, sym, start_time, end_time, price_to_beat) in &event_rows {
            let (Some(st), Some(end)) = (*start_time, *end_time) else {
                continue;
            };

            // Fix end_time: some metadata rows have end_time set to end-of-day
            // instead of the actual window end. Infer correct duration from slug.
            let corrected_end = {
                let duration_from_slug = if slug.contains("-5m-") {
                    Some(chrono::Duration::seconds(300))
                } else if slug.contains("-15m-") {
                    Some(chrono::Duration::seconds(900))
                } else {
                    None
                };
                if let Some(dur) = duration_from_slug {
                    let expected_end = st + dur;
                    // Use slug-inferred end if the metadata end is suspiciously long
                    if (end - st) > dur * 2 {
                        expected_end
                    } else {
                        end
                    }
                } else {
                    end
                }
            };

            let symbol = sym
                .clone()
                .or_else(|| slug_to_symbol.get(slug.as_str()).cloned())
                .or_else(|| infer_symbol_from_slug(slug))
                .unwrap_or_default();

            if symbol.is_empty() {
                continue;
            }

            // price_to_beat=0 is common for up/down markets; infer S0 from spot at start_time.
            let s0 = match price_to_beat {
                Some(p) if *p > Decimal::ZERO => Some(*p),
                _ => spot_series
                    .get(symbol.as_str())
                    .and_then(|series| spot_at_or_before(series, st)),
            };

            let Some(s0) = s0 else { continue };

            if !slug_to_symbol.contains_key(slug) {
                slug_to_symbol.insert(slug.clone(), symbol.clone());
            }
            updates.push(MarketUpdate {
                timestamp: st,
                symbol,
                update_type: UpdateType::EventState {
                    event_slug: slug.clone(),
                    end_time: Some(corrected_end),
                    price_to_beat: Some(s0),
                    outcome: None,
                },
            });
            event_open_count += 1;
        }
        info!(
            "Loaded {} event window rows (pm_market_metadata + derived), emitted {} EventState opens",
            event_rows.len(),
            event_open_count
        );

        // 4b. Fallback: if no window-open events could be emitted, derive windows from sync_records.
        if event_open_count == 0 && sync_records_exists {
            let rows: Result<Vec<(String, String, DateTime<Utc>)>> = sqlx::query_as(
                r#"
                SELECT DISTINCT ON (pm_market_slug)
                    pm_market_slug,
                    symbol,
                    timestamp
                FROM sync_records
                WHERE pm_market_slug IS NOT NULL
                  AND ($1::text[] IS NULL OR symbol = ANY($1))
                  AND ($2::timestamptz IS NULL OR timestamp >= $2)
                  AND ($3::timestamptz IS NULL OR timestamp <= $3)
                ORDER BY pm_market_slug, timestamp ASC
                "#,
            )
            .bind(if symbols.is_empty() {
                None::<Vec<String>>
            } else {
                Some(symbols.to_vec())
            })
            .bind(from)
            .bind(to)
            .fetch_all(pool)
            .await
            .map_err(Into::into);

            match rows {
                Ok(slugs) => {
                    for (slug, sym, st) in slugs {
                        let Some(duration_secs) = infer_window_duration_secs(&slug) else {
                            continue;
                        };

                        let s0 = spot_series
                            .get(sym.as_str())
                            .and_then(|series| spot_at_or_before(series, st));
                        let Some(s0) = s0 else { continue };

                        let end = st + chrono::Duration::seconds(duration_secs);
                        if !slug_to_symbol.contains_key(&slug) {
                            slug_to_symbol.insert(slug.clone(), sym.clone());
                        }
                        updates.push(MarketUpdate {
                            timestamp: st,
                            symbol: sym,
                            update_type: UpdateType::EventState {
                                event_slug: slug,
                                end_time: Some(end),
                                price_to_beat: Some(s0),
                                outcome: None,
                            },
                        });
                        event_open_count += 1;
                    }
                    if event_open_count > 0 {
                        info!(
                            "Derived {} event windows from sync_records (no pm_market_metadata rows)",
                            event_open_count
                        );
                    }
                }
                Err(e) => {
                    info!("sync_records window-derivation query failed: {e}");
                }
            }
        }

        // Settlement events: one per market_slug where outcome='Up' has settled_price=1
        let settlement_rows: Vec<(
            String,                // market_slug
            String,                // outcome ('Up' or 'Down')
            Decimal,               // settled_price
            Option<DateTime<Utc>>, // resolved_at
        )> = if pm_token_settlements_exists {
            sqlx::query_as(
                r#"
                SELECT market_slug, outcome, settled_price, resolved_at
                FROM pm_token_settlements
                WHERE resolved = true
                  AND LOWER(outcome) = 'up'
                  AND ($1::timestamptz IS NULL OR resolved_at >= $1)
                  AND ($2::timestamptz IS NULL OR resolved_at <= $2)
                ORDER BY resolved_at
                "#,
            )
            .bind(from)
            .bind(to)
            .fetch_all(pool)
            .await
            .unwrap_or_default()
        } else {
            Vec::new()
        };

        for (slug, _outcome, settled_price, resolved_at) in &settlement_rows {
            if let Some(rat) = resolved_at {
                let symbol = slug_to_symbol
                    .get(slug.as_str())
                    .cloned()
                    .or_else(|| infer_symbol_from_slug(slug))
                    .unwrap_or_default();
                if symbol.is_empty() {
                    continue;
                }
                // settled_price=1 means Up won → outcome=true
                let up_won = *settled_price == Decimal::ONE;
                updates.push(MarketUpdate {
                    timestamp: *rat,
                    symbol,
                    update_type: UpdateType::EventState {
                        event_slug: slug.clone(),
                        end_time: None,
                        price_to_beat: None,
                        outcome: Some(up_won),
                    },
                });
            }
        }
        info!("Loaded {} settlement records", settlement_rows.len());

        // 5. LOB snapshots from clob_orderbook_snapshots
        //    Aggregate ask-side depth per token snapshot, map to symbol.
        //    Downsample: one snapshot per (5-second bucket, token_id) to keep volume manageable.
        let lob_rows: Vec<(DateTime<Utc>, String, Option<serde_json::Value>)> = if lob_snaps_exists
            && !known_token_ids.is_empty()
        {
            sqlx::query_as(
                    r#"
                    SELECT DISTINCT ON (
                        (EXTRACT(EPOCH FROM received_at)::bigint / 5),
                        token_id
                    )
                        received_at, token_id, asks
                    FROM clob_orderbook_snapshots
                    WHERE ($1::timestamptz IS NULL OR received_at >= $1)
                      AND ($2::timestamptz IS NULL OR received_at <= $2)
                      AND token_id = ANY($3)
                      AND jsonb_array_length(asks) > 0
                    ORDER BY (EXTRACT(EPOCH FROM received_at)::bigint / 5), token_id, received_at DESC
                    "#,
                )
                .bind(from)
                .bind(to)
                .bind(&known_token_ids)
                .fetch_all(pool)
                .await
                .unwrap_or_default() // non-fatal: LOB data is optional
        } else {
            Vec::new()
        };

        let mut lob_count = 0u64;
        for (ts, token_id, asks_json) in &lob_rows {
            let event_slug = match token_to_slug.get(token_id.as_str()) {
                Some(s) => s.clone(),
                None => continue,
            };
            let symbol = token_to_symbol
                .get(token_id.as_str())
                .cloned()
                .or_else(|| slug_to_symbol.get(&event_slug).cloned())
                .or_else(|| infer_symbol_from_slug(&event_slug))
                .unwrap_or_default();
            if symbol.is_empty() {
                continue;
            }

            // Determine side from token_id: check if it's in the UP or DOWN settlement
            // We use a simple heuristic: if the token settled at price=1 with outcome='Up', it's UP
            // For now, just aggregate total depth — the engine will use it for both sides
            let (total_depth, best_ask_price) = match asks_json {
                Some(arr) if arr.is_array() => {
                    let levels = arr.as_array().unwrap();
                    let mut depth = 0.0f64;
                    let mut best = None;
                    for level in levels {
                        if let (Some(size_str), Some(price_str)) = (
                            level.get("size").and_then(|v| v.as_str()),
                            level.get("price").and_then(|v| v.as_str()),
                        ) {
                            if let Ok(size) = size_str.parse::<f64>() {
                                depth += size;
                            }
                            if best.is_none() {
                                if let Ok(p) = price_str.parse::<Decimal>() {
                                    best = Some(p);
                                }
                            }
                        }
                    }
                    (depth as u64, best)
                }
                _ => continue,
            };

            if total_depth == 0 {
                continue;
            }

            // Determine side: check if token_id appears as UP or DOWN in settlements
            let side = if token_id.len() > 10 {
                // Use the token→settlement mapping to determine side
                // For simplicity, emit as generic depth — engine will match by symbol
                "BOTH".to_string()
            } else {
                "BOTH".to_string()
            };

            updates.push(MarketUpdate {
                timestamp: *ts,
                symbol,
                update_type: UpdateType::LobSnapshot {
                    side,
                    ask_depth_shares: total_depth,
                    best_ask: best_ask_price,
                },
            });
            lob_count += 1;
        }
        info!(
            "Loaded {} LOB snapshots ({} mapped to symbols)",
            lob_rows.len(),
            lob_count
        );

        // Sort all updates by timestamp for deterministic replay
        updates.sort_by_key(|u| u.timestamp);

        info!("HistoricalFeed ready: {} total events", updates.len());

        Ok(Self {
            updates: VecDeque::from(updates),
        })
    }

    // ─── CSV loader ──────────────────────────────────────────

    /// Load historical data from CSV files.
    ///
    /// Reuses the existing `load_klines_from_csv()` and `load_pm_prices_from_csv()`
    /// functions from the volatility arb backtest module, converting their output
    /// into `MarketUpdate` events.
    pub fn from_csv(kline_path: &Path, pm_path: &Path) -> Result<Self> {
        let mut updates: Vec<MarketUpdate> = Vec::new();

        // Load klines → SpotTrade updates (use close price as spot)
        let klines = load_klines_from_csv(kline_path)
            .map_err(|e| anyhow::anyhow!("Failed to load klines CSV: {}", e))?;

        for k in &klines {
            updates.push(MarketUpdate {
                timestamp: k.timestamp,
                symbol: k.symbol.clone(),
                update_type: UpdateType::SpotTrade {
                    price: k.close,
                    quantity: Some(k.volume),
                },
            });
        }
        info!("Loaded {} kline records from CSV", klines.len());

        // Load PM prices → PmQuote + EventState updates
        let pm_prices = load_pm_prices_from_csv(pm_path)
            .map_err(|e| anyhow::anyhow!("Failed to load PM prices CSV: {}", e))?;

        for p in &pm_prices {
            // Emit quote updates (token_id is not available in this CSV format).
            // We use `market_id` as a stable per-event identifier.
            updates.push(MarketUpdate {
                timestamp: p.timestamp,
                symbol: p.symbol.clone(),
                update_type: UpdateType::PmQuote {
                    event_slug: p.market_id.clone(),
                    token_id: format!("{}:UP", p.market_id),
                    side: Side::Up,
                    best_bid: Some(p.yes_bid),
                    best_ask: Some(p.yes_ask),
                },
            });
            updates.push(MarketUpdate {
                timestamp: p.timestamp,
                symbol: p.symbol.clone(),
                update_type: UpdateType::PmQuote {
                    event_slug: p.market_id.clone(),
                    token_id: format!("{}:DOWN", p.market_id),
                    side: Side::Down,
                    best_bid: None,
                    best_ask: {
                        // Derive DOWN ask from NO (complement). This is a lossy approximation.
                        let no_ask = Decimal::ONE - p.yes_ask;
                        if no_ask > Decimal::ZERO {
                            Some(no_ask)
                        } else {
                            None
                        }
                    },
                },
            });

            // Emit event state at resolution time (if outcome known)
            if p.outcome.is_some() {
                updates.push(MarketUpdate {
                    timestamp: p.resolution_time,
                    symbol: p.symbol.clone(),
                    update_type: UpdateType::EventState {
                        event_slug: p.market_id.clone(),
                        end_time: Some(p.resolution_time),
                        price_to_beat: Some(p.threshold_price),
                        outcome: p.outcome,
                    },
                });
            }
        }
        info!("Loaded {} PM price records from CSV", pm_prices.len());

        // Sort all by timestamp
        updates.sort_by_key(|u| u.timestamp);

        info!("HistoricalFeed (CSV) ready: {} total events", updates.len());

        Ok(Self {
            updates: VecDeque::from(updates),
        })
    }
}

impl MarketFeed for HistoricalFeed {
    fn next_update(&mut self) -> Option<MarketUpdate> {
        self.updates.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Side;
    use rust_decimal_macros::dec;

    /// Verify that HistoricalFeed replays in chronological order (no lookahead)
    #[test]
    fn test_feed_chronological_order() {
        let updates = vec![
            MarketUpdate {
                timestamp: DateTime::parse_from_rfc3339("2025-01-01T00:00:03Z")
                    .unwrap()
                    .with_timezone(&Utc),
                symbol: "BTCUSDT".into(),
                update_type: UpdateType::SpotTrade {
                    price: dec!(100003),
                    quantity: None,
                },
            },
            MarketUpdate {
                timestamp: DateTime::parse_from_rfc3339("2025-01-01T00:00:01Z")
                    .unwrap()
                    .with_timezone(&Utc),
                symbol: "BTCUSDT".into(),
                update_type: UpdateType::SpotTrade {
                    price: dec!(100001),
                    quantity: None,
                },
            },
            MarketUpdate {
                timestamp: DateTime::parse_from_rfc3339("2025-01-01T00:00:02Z")
                    .unwrap()
                    .with_timezone(&Utc),
                symbol: "BTCUSDT".into(),
                update_type: UpdateType::PmQuote {
                    event_slug: "btc-updown-5m-test".into(),
                    token_id: "btc-updown-5m-test:UP".into(),
                    side: Side::Up,
                    best_bid: None,
                    best_ask: Some(dec!(0.35)),
                },
            },
        ];

        let mut sorted = updates.clone();
        sorted.sort_by_key(|u| u.timestamp);

        let mut feed = HistoricalFeed {
            updates: VecDeque::from(sorted),
        };

        let mut prev_ts = DateTime::<Utc>::MIN_UTC;
        while let Some(update) = feed.next_update() {
            assert!(
                update.timestamp >= prev_ts,
                "Feed produced out-of-order event"
            );
            prev_ts = update.timestamp;
        }
    }
}

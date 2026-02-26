//! Market data feed abstraction for live and backtest sharing.
//!
//! The `MarketFeed` trait provides a unified interface for both live (Binance WS + PM WS)
//! and historical (DB/CSV replay) data sources. This enables the backtest engine to reuse
//! the exact same `MomentumDetector.check()` logic as the live strategy.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::VecDeque;
use std::path::Path;

use anyhow::Result;
use sqlx::PgPool;
use tracing::info;

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
    /// Polymarket quote update (best asks for UP/DOWN tokens)
    PmQuote {
        up_ask: Option<Decimal>,
        down_ask: Option<Decimal>,
    },
    /// Event lifecycle update (metadata, settlement)
    EventState {
        event_slug: String,
        end_time: Option<DateTime<Utc>>,
        price_to_beat: Option<Decimal>,
        /// None = not yet settled, Some(true) = UP won, Some(false) = DOWN won
        outcome: Option<bool>,
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
    /// Total number of remaining updates in the feed.
    pub fn len(&self) -> usize {
        self.updates.len()
    }

    pub fn is_empty(&self) -> bool {
        self.updates.is_empty()
    }

    // ─── DB loader ───────────────────────────────────────────

    /// Load historical data from database tables:
    /// - `sync_records` → SpotTrade (Binance spot)
    /// - `clob_quote_ticks` → PmQuote (Polymarket quotes)
    /// - `pm_market_metadata` + `pm_token_settlements` → EventState
    pub async fn from_database(
        pool: &PgPool,
        symbols: &[String],
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    ) -> Result<Self> {
        let mut updates: Vec<MarketUpdate> = Vec::new();

        // 1. Binance spot trades from sync_records
        let spot_rows: Vec<(DateTime<Utc>, String, Decimal)> = sqlx::query_as(
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
        .await?;

        for (ts, sym, price) in &spot_rows {
            updates.push(MarketUpdate {
                timestamp: *ts,
                symbol: sym.clone(),
                update_type: UpdateType::SpotTrade {
                    price: *price,
                    quantity: None,
                },
            });
        }
        info!("Loaded {} spot records from sync_records", spot_rows.len());

        // 2. Polymarket quotes from clob_quote_ticks
        //    We need to pair UP/DOWN quotes by (token_id, received_at).
        //    The simplest approach: load all and group by timestamp.
        let quote_rows: Vec<(DateTime<Utc>, String, String, Option<Decimal>)> = sqlx::query_as(
            r#"
            SELECT received_at, token_id, side,
                   best_ask
            FROM clob_quote_ticks
            WHERE ($1::timestamptz IS NULL OR received_at >= $1)
              AND ($2::timestamptz IS NULL OR received_at <= $2)
            ORDER BY received_at
            "#,
        )
        .bind(from)
        .bind(to)
        .fetch_all(pool)
        .await?;

        // Group consecutive UP/DOWN rows by token_id.
        // For the backtest feed, we emit one PmQuote per row, letting the
        // engine accumulate the latest ask prices as in live.
        for (ts, token_id, side, best_ask) in &quote_rows {
            let (up_ask, down_ask) = if side == "UP" {
                (*best_ask, None)
            } else {
                (None, *best_ask)
            };
            updates.push(MarketUpdate {
                timestamp: *ts,
                symbol: token_id.clone(),
                update_type: UpdateType::PmQuote { up_ask, down_ask },
            });
        }
        info!(
            "Loaded {} quote ticks from clob_quote_ticks",
            quote_rows.len()
        );

        // 3. Event metadata + settlement from pm_market_metadata + pm_token_settlements
        let event_rows: Vec<(
            String,           // market_slug
            Option<String>,   // symbol
            Option<DateTime<Utc>>, // end_time
            Option<Decimal>,  // price_to_beat
            Option<bool>,     // resolved (from settlements)
            Option<DateTime<Utc>>, // resolved_at
        )> = sqlx::query_as(
            r#"
            SELECT m.market_slug,
                   m.symbol,
                   m.end_time,
                   m.price_to_beat,
                   s.resolved,
                   s.resolved_at
            FROM pm_market_metadata m
            LEFT JOIN pm_token_settlements s ON s.market_slug = m.market_slug
            WHERE ($1::timestamptz IS NULL OR m.end_time >= $1)
              AND ($2::timestamptz IS NULL OR m.end_time <= $2)
            ORDER BY COALESCE(s.resolved_at, m.end_time)
            "#,
        )
        .bind(from)
        .bind(to)
        .fetch_all(pool)
        .await?;

        for (slug, sym, end_time, price_to_beat, resolved, resolved_at) in &event_rows {
            let ts = resolved_at.unwrap_or_else(|| end_time.unwrap_or(Utc::now()));
            let outcome = if *resolved == Some(true) {
                // Check settled price to determine UP/DOWN outcome
                // For now, use simple boolean
                Some(true)
            } else {
                None
            };

            updates.push(MarketUpdate {
                timestamp: ts,
                symbol: sym.clone().unwrap_or_default(),
                update_type: UpdateType::EventState {
                    event_slug: slug.clone(),
                    end_time: *end_time,
                    price_to_beat: *price_to_beat,
                    outcome,
                },
            });
        }
        info!("Loaded {} event records", event_rows.len());

        // Sort all updates by timestamp for deterministic replay
        updates.sort_by_key(|u| u.timestamp);

        info!(
            "HistoricalFeed ready: {} total events",
            updates.len()
        );

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
            // Emit quote update
            updates.push(MarketUpdate {
                timestamp: p.timestamp,
                symbol: p.symbol.clone(),
                update_type: UpdateType::PmQuote {
                    up_ask: Some(p.yes_ask),
                    down_ask: {
                        // Derive DOWN ask from NO price (complement)
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
                    up_ask: Some(dec!(0.35)),
                    down_ask: Some(dec!(0.70)),
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

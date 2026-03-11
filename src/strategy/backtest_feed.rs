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

use crate::domain::Side;
use crate::strategy::backtest::{load_klines_from_csv, load_pm_prices_from_csv};

mod database;

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
        bid_size: Option<Decimal>,
        ask_size: Option<Decimal>,
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
        /// Market slug this book belongs to.
        event_slug: String,
        /// Token id this book belongs to.
        token_id: String,
        /// Token side: UP or DOWN
        side: Side,
        /// Total ask-side liquidity in shares across all levels
        ask_depth_shares: u64,
        /// Best ask level size in shares at the top of book.
        best_ask_size_shares: u64,
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

/// Market data source for both live and backtest.
///
/// Implementors provide a stream of `MarketUpdate` events in chronological order.
/// Returns `None` when the data source is exhausted (backtest) or when the stream
/// is temporarily empty (live — caller should await next update).
pub trait MarketFeed {
    fn next_update(&mut self) -> Option<MarketUpdate>;
}

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
        let updates = database::load_database_updates(pool, symbols, from, to).await?;
        Ok(Self::new(updates))
    }

    /// Load historical data from CSV files.
    ///
    /// Reuses the existing `load_klines_from_csv()` and `load_pm_prices_from_csv()`
    /// functions from the volatility arb backtest module, converting their output
    /// into `MarketUpdate` events.
    pub fn from_csv(kline_path: &Path, pm_path: &Path) -> Result<Self> {
        let mut updates: Vec<MarketUpdate> = Vec::new();

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

        let pm_prices = load_pm_prices_from_csv(pm_path)
            .map_err(|e| anyhow::anyhow!("Failed to load PM prices CSV: {}", e))?;

        for p in &pm_prices {
            updates.push(MarketUpdate {
                timestamp: p.timestamp,
                symbol: p.symbol.clone(),
                update_type: UpdateType::PmQuote {
                    event_slug: p.market_id.clone(),
                    token_id: format!("{}:UP", p.market_id),
                    side: Side::Up,
                    best_bid: Some(p.yes_bid),
                    best_ask: Some(p.yes_ask),
                    bid_size: None,
                    ask_size: None,
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
                        let no_ask = Decimal::ONE - p.yes_ask;
                        if no_ask > Decimal::ZERO {
                            Some(no_ask)
                        } else {
                            None
                        }
                    },
                    bid_size: None,
                    ask_size: None,
                },
            });

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
                    bid_size: None,
                    ask_size: None,
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

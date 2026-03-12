//! Market data feed abstraction for live and backtest sharing.
//!
//! The `MarketFeed` trait provides a unified interface for both live (Binance WS + PM WS)
//! and historical (DB/CSV replay) data sources. This enables the backtest engine to reuse
//! the exact same `MomentumDetector.check()` logic as the live strategy.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet, VecDeque};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayEventWindow {
    pub market_slug: String,
    pub symbol: String,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplayEventWindowFilterStats {
    pub total_updates_before: usize,
    pub total_updates_after: usize,
    pub dropped_updates: usize,
    pub effective_from: Option<DateTime<Utc>>,
    pub effective_to: Option<DateTime<Utc>>,
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

    /// Retain only updates that belong to the supplied event windows.
    ///
    /// Event-scoped PM updates are filtered by market slug, while shared market-state updates
    /// (spot, Binance L2, LOB) are filtered by `(symbol, timestamp)` membership in the union of
    /// kept event windows. This lets PM replay stay segmented by event quality without mutating
    /// the raw collected data model.
    pub fn retain_pm_event_windows(
        &mut self,
        windows: &[ReplayEventWindow],
    ) -> ReplayEventWindowFilterStats {
        let total_updates_before = self.updates.len();
        if windows.is_empty() {
            self.updates.clear();
            return ReplayEventWindowFilterStats {
                total_updates_before,
                total_updates_after: 0,
                dropped_updates: total_updates_before,
                effective_from: None,
                effective_to: None,
            };
        }

        let kept_slugs: HashSet<&str> = windows
            .iter()
            .map(|window| window.market_slug.as_str())
            .collect();
        let mut windows_by_symbol: HashMap<&str, Vec<(DateTime<Utc>, DateTime<Utc>)>> =
            HashMap::new();
        for window in windows {
            windows_by_symbol
                .entry(window.symbol.as_str())
                .or_default()
                .push((window.start_time, window.end_time));
        }

        let mut retained = VecDeque::with_capacity(total_updates_before);
        for update in self.updates.drain(..) {
            let keep = match &update.update_type {
                UpdateType::PmQuote { event_slug, .. }
                | UpdateType::EventState { event_slug, .. } => {
                    kept_slugs.contains(event_slug.as_str())
                }
                UpdateType::SpotTrade { .. }
                | UpdateType::BinanceL2 { .. }
                | UpdateType::LobSnapshot { .. } => windows_by_symbol
                    .get(update.symbol.as_str())
                    .map(|ranges| {
                        ranges.iter().any(|(start_time, end_time)| {
                            update.timestamp >= *start_time && update.timestamp <= *end_time
                        })
                    })
                    .unwrap_or(false),
            };

            if keep {
                retained.push_back(update);
            }
        }

        let total_updates_after = retained.len();
        let effective_from = retained.front().map(|update| update.timestamp);
        let effective_to = retained.back().map(|update| update.timestamp);
        self.updates = retained;

        ReplayEventWindowFilterStats {
            total_updates_before,
            total_updates_after,
            dropped_updates: total_updates_before.saturating_sub(total_updates_after),
            effective_from,
            effective_to,
        }
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

    #[test]
    fn retain_pm_event_windows_keeps_only_selected_segments() {
        let start_a = DateTime::parse_from_rfc3339("2025-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let end_a = DateTime::parse_from_rfc3339("2025-01-01T00:05:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let start_b = DateTime::parse_from_rfc3339("2025-01-01T00:10:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let end_b = DateTime::parse_from_rfc3339("2025-01-01T00:15:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let mut feed = HistoricalFeed::new(vec![
            MarketUpdate {
                timestamp: start_a,
                symbol: "BTCUSDT".into(),
                update_type: UpdateType::SpotTrade {
                    price: dec!(100000),
                    quantity: None,
                },
            },
            MarketUpdate {
                timestamp: start_a + chrono::Duration::seconds(10),
                symbol: "BTCUSDT".into(),
                update_type: UpdateType::PmQuote {
                    event_slug: "btc-keep".into(),
                    token_id: "tok-keep-up".into(),
                    side: Side::Up,
                    best_bid: Some(dec!(0.40)),
                    best_ask: Some(dec!(0.41)),
                },
            },
            MarketUpdate {
                timestamp: start_b,
                symbol: "BTCUSDT".into(),
                update_type: UpdateType::SpotTrade {
                    price: dec!(100100),
                    quantity: None,
                },
            },
            MarketUpdate {
                timestamp: start_b + chrono::Duration::seconds(10),
                symbol: "BTCUSDT".into(),
                update_type: UpdateType::PmQuote {
                    event_slug: "btc-drop".into(),
                    token_id: "tok-drop-up".into(),
                    side: Side::Up,
                    best_bid: Some(dec!(0.52)),
                    best_ask: Some(dec!(0.53)),
                },
            },
            MarketUpdate {
                timestamp: end_b + chrono::Duration::seconds(45),
                symbol: "BTCUSDT".into(),
                update_type: UpdateType::EventState {
                    event_slug: "btc-drop".into(),
                    end_time: None,
                    price_to_beat: None,
                    outcome: Some(true),
                },
            },
            MarketUpdate {
                timestamp: end_a + chrono::Duration::seconds(45),
                symbol: "BTCUSDT".into(),
                update_type: UpdateType::EventState {
                    event_slug: "btc-keep".into(),
                    end_time: None,
                    price_to_beat: None,
                    outcome: Some(true),
                },
            },
        ]);

        let stats = feed.retain_pm_event_windows(&[ReplayEventWindow {
            market_slug: "btc-keep".into(),
            symbol: "BTCUSDT".into(),
            start_time: start_a,
            end_time: end_a,
        }]);

        assert_eq!(stats.total_updates_before, 6);
        assert_eq!(stats.total_updates_after, 3);
        assert_eq!(stats.dropped_updates, 3);
        assert_eq!(stats.effective_from, Some(start_a));
        assert_eq!(
            stats.effective_to,
            Some(end_a + chrono::Duration::seconds(45))
        );

        let kept: Vec<String> = feed
            .updates
            .iter()
            .map(|update| match &update.update_type {
                UpdateType::PmQuote { event_slug, .. }
                | UpdateType::EventState { event_slug, .. } => event_slug.clone(),
                UpdateType::SpotTrade { .. } => "spot".to_string(),
                UpdateType::BinanceL2 { .. } => "l2".to_string(),
                UpdateType::LobSnapshot { .. } => "lob".to_string(),
            })
            .collect();
        assert_eq!(kept, vec!["spot", "btc-keep", "btc-keep"]);
    }
}

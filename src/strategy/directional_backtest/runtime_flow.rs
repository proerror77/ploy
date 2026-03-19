use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use crate::adapters::SpotPrice;
use crate::domain::Side;
use crate::strategy::backtest::BacktestResults;
use crate::strategy::backtest_feed::{MarketFeed, UpdateType};
use crate::strategy::momentum::Direction;

use super::{ActiveWindowInfo, DirectionalBacktestEngine};

impl DirectionalBacktestEngine {
    /// Consume the feed and return aggregate results.
    pub fn run<F: MarketFeed>(&mut self, feed: &mut F) -> BacktestResults {
        while let Some(update) = feed.next_update() {
            self.track_data_range(update.timestamp);
            self.prune_expired_events(update.timestamp);

            match &update.update_type {
                UpdateType::SpotTrade { price, quantity } => {
                    self.handle_spot_trade(&update.symbol, *price, *quantity, update.timestamp);
                }
                UpdateType::PmQuote {
                    event_slug,
                    side,
                    best_ask,
                    ..
                } => {
                    self.handle_pm_quote(
                        &update.symbol,
                        event_slug,
                        *side,
                        *best_ask,
                        update.timestamp,
                    );
                }
                UpdateType::EventState {
                    event_slug,
                    end_time,
                    price_to_beat,
                    outcome,
                } => {
                    self.handle_event_state(
                        &update.symbol,
                        event_slug,
                        *end_time,
                        *price_to_beat,
                        *outcome,
                        update.timestamp,
                    );
                }
                UpdateType::LobSnapshot { .. } => {}
                UpdateType::BinanceL2 { .. } => {}
            }
        }

        self.close_remaining_positions();
        let _ = self.recorder.flush();
        self.build_results()
    }

    fn track_data_range(&mut self, ts: DateTime<Utc>) {
        if self.data_range_start.is_none() {
            self.data_range_start = Some(ts);
        }
        self.data_range_end = Some(ts);
    }

    fn prune_expired_events(&mut self, ts: DateTime<Utc>) {
        for events in self.active_events.values_mut() {
            events.retain(|event| event.end_time > ts);
        }
    }

    fn handle_event_state(
        &mut self,
        symbol: &str,
        event_slug: &str,
        end_time: Option<DateTime<Utc>>,
        price_to_beat: Option<Decimal>,
        outcome: Option<bool>,
        ts: DateTime<Utc>,
    ) {
        if let Some(won) = outcome {
            self.resolve_positions(symbol, event_slug, won, ts);
            if let Some(events) = self.active_events.get_mut(symbol) {
                events.retain(|event| event.event_slug != event_slug);
            }
            self.pm_asks_by_event.remove(event_slug);
            return;
        }

        if let (Some(end_time), Some(s0)) = (end_time, price_to_beat) {
            let events = self.active_events.entry(symbol.to_string()).or_default();
            if !events.iter().any(|event| event.event_slug == event_slug) {
                events.push(ActiveWindowInfo {
                    event_slug: event_slug.to_string(),
                    s0,
                    end_time,
                });
            }
        }
    }

    fn handle_spot_trade(
        &mut self,
        symbol: &str,
        price: Decimal,
        quantity: Option<Decimal>,
        ts: DateTime<Utc>,
    ) {
        self.spot_prices
            .entry(symbol.to_string())
            .and_modify(|spot| spot.update(price, quantity, ts))
            .or_insert_with(|| SpotPrice::new(price, quantity, ts));
    }

    fn handle_pm_quote(
        &mut self,
        symbol: &str,
        event_slug: &str,
        quote_side: Side,
        best_ask: Option<Decimal>,
        ts: DateTime<Utc>,
    ) {
        let entry = self
            .pm_asks_by_event
            .entry(event_slug.to_string())
            .or_insert((None, None));
        match quote_side {
            Side::Up => {
                if best_ask.is_some() {
                    entry.0 = best_ask;
                }
            }
            Side::Down => {
                if best_ask.is_some() {
                    entry.1 = best_ask;
                }
            }
        }

        for position in &mut self.positions {
            if position.symbol == symbol && position.event_slug == event_slug {
                match position.direction {
                    Direction::Up if quote_side == Side::Up => {
                        if let Some(ask) = best_ask {
                            position.latest_pm_price = ask;
                        }
                    }
                    Direction::Down if quote_side == Side::Down => {
                        if let Some(ask) = best_ask {
                            position.latest_pm_price = ask;
                        }
                    }
                    _ => {}
                }
            }
        }

        let should_run_logic = match self.last_logic_ts.get(symbol) {
            Some(last) => (ts - *last).num_seconds() >= 1,
            None => true,
        };
        if !should_run_logic {
            return;
        }
        self.last_logic_ts.insert(symbol.to_string(), ts);

        self.try_directional_entry(symbol, ts);
        self.check_exits(ts);
        self.record_equity(ts);
    }

    fn record_equity(&mut self, ts: DateTime<Utc>) {
        if self.equity > self.peak_equity {
            self.peak_equity = self.equity;
        }

        let drawdown = if self.peak_equity > Decimal::ZERO {
            (self.peak_equity - self.equity) / self.peak_equity
        } else {
            Decimal::ZERO
        };
        if drawdown > self.max_drawdown {
            self.max_drawdown = drawdown;
        }

        let should_record = self
            .equity_curve
            .last()
            .map(|(last_ts, _)| (ts - *last_ts).num_seconds() >= 1)
            .unwrap_or(true);
        if should_record {
            self.equity_curve.push((ts, self.equity));
        }
    }
}

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use tracing::{debug, trace};

use super::*;

impl StaggeredArbBacktestEngine {
    /// Consume the feed and return aggregate results.
    pub fn run<F: MarketFeed>(&mut self, feed: &mut F) -> BacktestResults {
        while let Some(update) = feed.next_update() {
            if self.data_range_start.is_none() {
                self.data_range_start = Some(update.timestamp);
            }
            self.data_range_end = Some(update.timestamp);

            for events in self.active_events.values_mut() {
                events.retain(|e| e.end_time > update.timestamp);
            }

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
                    if let Some(won) = outcome {
                        self.resolve_positions(&update.symbol, event_slug, *won, update.timestamp);
                        if let Some(events) = self.active_events.get_mut(&update.symbol) {
                            events.retain(|e| e.event_slug != *event_slug);
                        }
                        self.pm_asks_by_event.remove(event_slug);
                        self.pm_quote_state_by_event.remove(event_slug);
                    }

                    if outcome.is_none() {
                        if let (Some(end), Some(s0)) = (end_time, price_to_beat) {
                            let duration_secs = (*end - update.timestamp).num_seconds();
                            let allowed = if self.config.allowed_window_durations.is_empty() {
                                true
                            } else {
                                let tol = self.config.window_duration_tolerance as i64;
                                self.config
                                    .allowed_window_durations
                                    .iter()
                                    .any(|&d| (duration_secs - d as i64).abs() <= tol)
                            };
                            if !allowed {
                                trace!(
                                    "Skipping event {} with duration {}s (not in allowed list)",
                                    event_slug,
                                    duration_secs
                                );
                            } else {
                                let events =
                                    self.active_events.entry(update.symbol.clone()).or_default();
                                if !events.iter().any(|e| e.event_slug == *event_slug) {
                                    events.push(ActiveWindowInfo {
                                        event_slug: event_slug.clone(),
                                        s0: *s0,
                                        end_time: *end,
                                        window_duration_secs: duration_secs,
                                    });
                                }
                            }
                        }
                    }
                }
                UpdateType::LobSnapshot {
                    ask_depth_shares, ..
                } => {
                    self.lob_depth
                        .insert(update.symbol.clone(), *ask_depth_shares);
                }
                UpdateType::BinanceL2 { obi_5, .. } => {
                    if let Some(prev) = self.binance_l2_obi_5.insert(update.symbol.clone(), *obi_5)
                    {
                        self.binance_l2_obi_prev_5
                            .insert(update.symbol.clone(), prev);
                    }
                    self.binance_l2_obi_ts
                        .insert(update.symbol.clone(), update.timestamp);
                }
            }
        }

        self.close_remaining_positions();

        let total_events: usize = self.active_events.values().map(|v| v.len()).sum();
        let total_quotes = self.pm_asks_by_event.len();
        let total_spots = self.spot_prices.len();
        debug!(
            "Engine summary: {} active events, {} quote slugs, {} spot symbols, {} positions, {} closed trades",
            total_events,
            total_quotes,
            total_spots,
            self.positions.len(),
            self.closed_trades.len()
        );

        let _ = self.recorder.flush();
        self.build_results()
    }

    pub(super) fn handle_spot_trade(
        &mut self,
        symbol: &str,
        price: Decimal,
        quantity: Option<Decimal>,
        ts: DateTime<Utc>,
    ) {
        self.spot_prices
            .entry(symbol.to_string())
            .and_modify(|sp| sp.update(price, quantity, ts))
            .or_insert_with(|| SpotPrice::new(price, quantity, ts));
    }

    pub(super) fn handle_pm_quote(
        &mut self,
        symbol: &str,
        event_slug: &str,
        quote_side: Side,
        best_ask: Option<Decimal>,
        ts: DateTime<Utc>,
    ) {
        self.record_pm_quote(event_slug, quote_side, best_ask, ts);
        self.check_leg2_opportunities(symbol, ts);
        self.try_entry(symbol, ts);
        self.record_equity(ts);
    }

    pub(crate) fn market_depth(&self, symbol: &str) -> u64 {
        self.lob_depth.get(symbol).copied().unwrap_or(500)
    }

    pub(crate) fn record_pm_quote(
        &mut self,
        event_slug: &str,
        quote_side: Side,
        best_ask: Option<Decimal>,
        ts: DateTime<Utc>,
    ) {
        let state = self
            .pm_quote_state_by_event
            .entry(event_slug.to_string())
            .or_default();
        let side_state = state.side_mut(quote_side);
        if self.config.pm_quote_max_stale_secs > 0 {
            if let Some(last_seen_at) = side_state.last_seen_at {
                if (ts - last_seen_at).num_seconds() > self.config.pm_quote_max_stale_secs as i64 {
                    side_state.clear();
                }
            }
        }
        state.update(quote_side, best_ask, None, ts);
        self.pm_asks_by_event
            .insert(event_slug.to_string(), state.asks());
    }

    pub(crate) fn event_quote_state(
        &self,
        event_slug: &str,
        up_ask: Option<Decimal>,
        down_ask: Option<Decimal>,
        ts: DateTime<Utc>,
    ) -> PmEventQuoteState {
        self.pm_quote_state_by_event
            .get(event_slug)
            .copied()
            .unwrap_or_else(|| PmEventQuoteState::synthetic(up_ask, down_ask, ts))
    }

    pub(super) fn record_equity(&mut self, ts: DateTime<Utc>) {
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

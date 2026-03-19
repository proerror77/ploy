use super::CryptoRlPolicyStrategy;
use crate::adapters::SpotPrice;
use crate::collector::LobSnapshot;
use crate::strategy::traits::{
    MarketUpdate, StrategyAction, StrategyEvent, StrategyEventType, StrategyStateInfo,
};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;

impl CryptoRlPolicyStrategy {
    pub(super) fn apply_market_update(&mut self, update: &MarketUpdate) {
        match update {
            MarketUpdate::BinancePrice {
                symbol,
                price,
                timestamp,
            } => match self.spot_prices.get_mut(symbol) {
                Some(existing) => existing.update(*price, None, *timestamp),
                None => {
                    self.spot_prices
                        .insert(symbol.clone(), SpotPrice::new(*price, None, *timestamp));
                }
            },
            MarketUpdate::BinanceL2 {
                symbol,
                obi_1,
                obi_2,
                obi_3,
                obi_5,
                obi_10,
                obi_20,
                bid_volume_5,
                ask_volume_5,
                spread_bps,
                timestamp,
            } => {
                self.l2_by_symbol.insert(
                    symbol.clone(),
                    LobSnapshot {
                        timestamp: *timestamp,
                        symbol: symbol.clone(),
                        best_bid: Decimal::ZERO,
                        best_ask: Decimal::ZERO,
                        mid_price: Decimal::ZERO,
                        spread_bps: *spread_bps,
                        obi_1: *obi_1,
                        obi_2: *obi_2,
                        obi_3: *obi_3,
                        obi_5: *obi_5,
                        obi_10: *obi_10,
                        obi_20: *obi_20,
                        bid_volume_5: *bid_volume_5,
                        ask_volume_5: *ask_volume_5,
                        update_id: 0,
                    },
                );
            }
            MarketUpdate::PolymarketQuote {
                token_id, quote, ..
            } => {
                self.quotes.insert(token_id.clone(), *quote);
            }
            MarketUpdate::EventDiscovered { .. } => self.track_event(update),
            MarketUpdate::EventExpired { event_id } => {
                self.active_events.remove(event_id);
                self.last_logged_at.remove(event_id);
            }
            MarketUpdate::BinanceKline { .. } => {}
        }
    }

    pub(super) fn run_tick(
        &mut self,
        now: DateTime<Utc>,
    ) -> crate::error::Result<Vec<StrategyAction>> {
        if !self.enabled {
            return Ok(Vec::new());
        }

        let mut actions = Vec::new();
        let event_ids: Vec<String> = self.active_events.keys().cloned().collect();
        for event_id in event_ids {
            let Some(event) = self.active_events.get(&event_id).cloned() else {
                continue;
            };

            match self.evaluate_event(now, &event) {
                Ok(Some(signal)) => {
                    self.last_signal = Some(signal.clone());
                    self.last_reason = Some(format!(
                        "{} {} ready",
                        signal.symbol,
                        Self::action_label(signal.action)
                    ));
                    self.last_error = None;

                    if self.should_emit_signal_log(&event_id, now) {
                        actions.push(StrategyAction::LogEvent {
                            event: self.signal_event(&event, &signal),
                        });
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    self.last_error = Some(err.to_string());
                    actions.push(StrategyAction::LogEvent {
                        event: StrategyEvent::new(
                            StrategyEventType::Error,
                            format!("crypto_rl_policy evaluation failed for {}", event.event_id),
                        )
                        .with_data("event_id", &event.event_id)
                        .with_data("symbol", &event.symbol)
                        .with_data("error", err.to_string()),
                    });
                }
            }
        }

        Ok(actions)
    }

    pub(super) fn state_info(&self) -> StrategyStateInfo {
        let mut metrics = HashMap::new();
        metrics.insert("symbols".to_string(), self.symbols.join(","));
        metrics.insert(
            "active_events".to_string(),
            self.active_events.len().to_string(),
        );
        metrics.insert("quote_count".to_string(), self.quotes.len().to_string());
        metrics.insert(
            "l2_symbols".to_string(),
            self.l2_by_symbol.len().to_string(),
        );
        if let Some(reason) = &self.last_reason {
            metrics.insert("last_reason".to_string(), reason.clone());
        }
        if let Some(error) = &self.last_error {
            metrics.insert("last_error".to_string(), error.clone());
        }
        if let Some(signal) = &self.last_signal {
            metrics.insert("last_event_id".to_string(), signal.event_id.clone());
            metrics.insert("last_symbol".to_string(), signal.symbol.clone());
            metrics.insert(
                "last_action".to_string(),
                Self::action_label(signal.action).to_string(),
            );
            metrics.insert(
                "last_policy_source".to_string(),
                signal.policy_source.clone(),
            );
            metrics.insert(
                "last_desired_shares".to_string(),
                signal.desired_shares.to_string(),
            );
            metrics.insert(
                "last_remaining_secs".to_string(),
                signal.remaining_secs.to_string(),
            );
            metrics.insert("last_at".to_string(), signal.at.to_rfc3339());
        }

        StrategyStateInfo {
            strategy_id: self.id.clone(),
            phase: "observe_only".to_string(),
            enabled: self.enabled,
            active: !self.active_events.is_empty(),
            position_count: 0,
            pending_order_count: 0,
            total_exposure: Decimal::ZERO,
            unrealized_pnl: Decimal::ZERO,
            realized_pnl_today: Decimal::ZERO,
            last_update: Utc::now(),
            metrics,
        }
    }

    pub(super) fn reset_runtime_state(&mut self) {
        self.spot_prices.clear();
        self.l2_by_symbol.clear();
        self.quotes.clear();
        self.active_events.clear();
        self.last_signal = None;
        self.last_reason = None;
        self.last_error = None;
        self.last_logged_at.clear();
    }
}

use std::collections::VecDeque;

use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use tracing::{info, warn};

use crate::domain::{OrderStatus, Side};
use crate::error::Result;
use crate::strategy::traits::{
    MarketUpdate, OrderUpdate, StrategyAction, StrategyEvent, StrategyEventType,
};

use super::super::rebalancer::Straddle;
use super::{EventContext, GammaScalpingStrategy, PendingOrder};

impl GammaScalpingStrategy {
    pub(super) async fn handle_market_update(
        &mut self,
        update: &MarketUpdate,
    ) -> Result<Vec<StrategyAction>> {
        if !self.active {
            return Ok(vec![]);
        }

        match update {
            MarketUpdate::EventDiscovered {
                event_id,
                series_id,
                up_token,
                down_token,
                end_time,
                price_to_beat,
                ..
            } => {
                let symbol = self
                    .symbol_for_event(series_id)
                    .unwrap_or("BTCUSDT")
                    .to_string();

                let ctx = EventContext {
                    event_id: event_id.clone(),
                    series_id: series_id.clone(),
                    symbol,
                    up_token: up_token.clone(),
                    down_token: down_token.clone(),
                    end_time: *end_time,
                    price_to_beat: *price_to_beat,
                };

                self.active_events.insert(event_id.clone(), ctx);
                Ok(vec![])
            }

            MarketUpdate::EventExpired { event_id } => {
                self.active_events.remove(event_id);
                if let Some(straddle) = self.straddles.remove(event_id) {
                    self.realized_pnl += straddle.realized_pnl;
                    info!(
                        event_id,
                        pnl = %straddle.realized_pnl,
                        rebalances = straddle.rebalance_count,
                        "Straddle expired"
                    );
                }
                Ok(vec![])
            }

            MarketUpdate::BinanceKline { symbol, kline, .. } => {
                if kline.is_closed {
                    let closes = self.kline_history.entry(symbol.clone()).or_insert_with(|| {
                        VecDeque::with_capacity(self.config.vol_lookback_periods + 1)
                    });

                    closes.push_back(kline.close.to_f64().unwrap_or(0.0));
                    if closes.len() > self.config.vol_lookback_periods {
                        closes.pop_front();
                    }
                }
                Ok(vec![])
            }

            MarketUpdate::BinancePrice { symbol, price, .. } => {
                self.spot_prices
                    .insert(symbol.clone(), price.to_f64().unwrap_or(0.0));
                Ok(vec![])
            }

            MarketUpdate::BinanceL2 { .. } => Ok(vec![]),

            MarketUpdate::PolymarketQuote {
                token_id, quote, ..
            } => {
                self.quote_cache.insert(token_id.clone(), quote.clone());
                Ok(vec![])
            }
        }
    }

    pub(super) async fn handle_order_update(
        &mut self,
        update: &OrderUpdate,
    ) -> Result<Vec<StrategyAction>> {
        let pending = match self
            .pending_orders
            .remove(update.client_order_id.as_deref().unwrap_or(""))
        {
            Some(p) => p,
            None => return Ok(vec![]),
        };

        match update.status {
            OrderStatus::Filled => self.handle_filled_order(update, pending),

            OrderStatus::Cancelled | OrderStatus::Rejected | OrderStatus::Failed => {
                warn!(
                    order_id = %update.order_id,
                    status = ?update.status,
                    error = ?update.error,
                    "Gamma scalp order failed"
                );
                Ok(vec![])
            }

            _ => {
                if let Some(coid) = &update.client_order_id {
                    self.pending_orders.insert(coid.clone(), pending);
                }
                Ok(vec![])
            }
        }
    }

    fn handle_filled_order(
        &mut self,
        update: &OrderUpdate,
        pending: PendingOrder,
    ) -> Result<Vec<StrategyAction>> {
        self.trade_count += 1;

        if pending.is_entry {
            self.fill_entry_order(update, &pending);
        } else {
            self.fill_rebalance_or_exit(update, &pending);
        }

        Ok(vec![StrategyAction::LogEvent {
            event: StrategyEvent::new(StrategyEventType::OrderFilled, "Order filled")
                .with_data("event_id", &pending.event_id)
                .with_data("shares", update.filled_qty.to_string()),
        }])
    }

    fn fill_entry_order(&mut self, update: &OrderUpdate, pending: &PendingOrder) {
        let straddle = self
            .straddles
            .entry(pending.event_id.clone())
            .or_insert_with(|| {
                let ctx = self.active_events.get(&pending.event_id);
                Straddle {
                    event_id: pending.event_id.clone(),
                    symbol: ctx.map(|c| c.symbol.clone()).unwrap_or_default(),
                    up_token_id: ctx.map(|c| c.up_token.clone()).unwrap_or_default(),
                    down_token_id: ctx.map(|c| c.down_token.clone()).unwrap_or_default(),
                    up_shares: 0,
                    down_shares: 0,
                    up_entry_price: Decimal::ZERO,
                    down_entry_price: Decimal::ZERO,
                    entry_time: Utc::now(),
                    expiry_time: ctx.map(|c| c.end_time).unwrap_or_else(Utc::now),
                    last_rebalance: Utc::now(),
                    rebalance_count: 0,
                    realized_pnl: Decimal::ZERO,
                    cost_basis: Decimal::ZERO,
                }
            });

        let fill_price = update.avg_fill_price.unwrap_or(pending.price);
        let cost = fill_price * Decimal::from(update.filled_qty);

        if pending.side == Side::Up {
            straddle.up_shares += update.filled_qty;
            straddle.up_entry_price = fill_price;
        } else {
            straddle.down_shares += update.filled_qty;
            straddle.down_entry_price = fill_price;
        }
        straddle.cost_basis += cost;

        info!(
            event_id = %pending.event_id,
            side = ?pending.side,
            shares = update.filled_qty,
            price = %fill_price,
            "Straddle leg filled"
        );
    }

    fn fill_rebalance_or_exit(&mut self, update: &OrderUpdate, pending: &PendingOrder) {
        if let Some(straddle) = self.straddles.get_mut(&pending.event_id) {
            let fill_price = update.avg_fill_price.unwrap_or(pending.price);
            let pnl_delta = (fill_price - pending.price) * Decimal::from(update.filled_qty);

            if pending.token_id == straddle.up_token_id {
                if fill_price > pending.price {
                    straddle.up_shares = straddle.up_shares.saturating_sub(update.filled_qty);
                } else {
                    straddle.up_shares += update.filled_qty;
                }
            } else if fill_price > pending.price {
                straddle.down_shares = straddle.down_shares.saturating_sub(update.filled_qty);
            } else {
                straddle.down_shares += update.filled_qty;
            }

            straddle.realized_pnl += pnl_delta;
            straddle.rebalance_count += 1;
            straddle.last_rebalance = Utc::now();

            if straddle.up_shares == 0 && straddle.down_shares == 0 {
                let closed = self.straddles.remove(&pending.event_id);
                if let Some(s) = closed {
                    self.realized_pnl += s.realized_pnl;
                    self.last_cooldown = Some(Utc::now());
                    if s.realized_pnl < Decimal::ZERO {
                        self.daily_loss += s.realized_pnl.abs();
                    }
                }
            }
        }
    }

    pub(super) async fn handle_tick(&mut self, now: DateTime<Utc>) -> Result<Vec<StrategyAction>> {
        if !self.active {
            return Ok(vec![]);
        }

        let mut actions = Vec::new();

        let event_ids: Vec<String> = self.straddles.keys().cloned().collect();
        for event_id in &event_ids {
            if let Some(straddle) = self.straddles.get(event_id) {
                if let Some(mut rebal_actions) = self.check_rebalance(straddle, now) {
                    actions.append(&mut rebal_actions);
                }
            }
        }

        let candidates: Vec<EventContext> = self.active_events.values().cloned().collect();
        for ctx in &candidates {
            if let Some(mut entry_actions) = self.evaluate_entry(ctx, now) {
                actions.append(&mut entry_actions);
            }
        }

        Ok(actions)
    }

    pub(super) async fn shutdown_actions(&mut self) -> Result<Vec<StrategyAction>> {
        self.active = false;
        let mut actions = Vec::new();

        for (_, pending) in self.pending_orders.drain() {
            actions.push(StrategyAction::CancelOrder {
                order_id: pending.client_order_id,
            });
        }

        for (_, straddle) in &self.straddles {
            let exit = self.rebalancer.compute_exit(straddle);
            let mut exit_actions = self.actions_from_rebalance(straddle, exit);
            actions.append(&mut exit_actions);
        }

        info!(
            straddles = self.straddles.len(),
            realized_pnl = %self.realized_pnl,
            "Gamma scalping strategy shutting down"
        );

        Ok(actions)
    }

    pub(super) fn reset_runtime(&mut self) {
        self.straddles.clear();
        self.pending_orders.clear();
        self.kline_history.clear();
        self.quote_cache.clear();
        self.active_events.clear();
        self.spot_prices.clear();
        self.realized_pnl = Decimal::ZERO;
        self.daily_loss = Decimal::ZERO;
        self.trade_count = 0;
        self.last_cooldown = None;
        self.active = true;
    }
}

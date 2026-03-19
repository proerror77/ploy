use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use tracing::{debug, info};
use uuid::Uuid;

use crate::domain::Domain;
use crate::domain::{OrderType, Side, TimeInForce};
use crate::strategy::traits::{
    StrategyAction, StrategyEvent, StrategyEventType, StrategyOrderIntent,
};
use crate::strategy::volatility_arb::calculate_implied_volatility;

use super::super::greeks::{binary_greeks, realized_vol_from_closes};
use super::super::rebalancer::{RebalanceAction, Straddle};
use super::{EventContext, GammaScalpingStrategy};

impl GammaScalpingStrategy {
    /// Map a Polymarket series/event to a Binance symbol.
    pub(super) fn symbol_for_event(&self, series_id: &str) -> Option<&str> {
        for sym in &self.config.symbols {
            let prefix = sym.replace("USDT", "").to_lowercase();
            if series_id.to_lowercase().contains(&prefix) {
                return Some(sym.as_str());
            }
        }
        None
    }

    /// Check if we should enter a new straddle on this event.
    pub(super) fn evaluate_entry(
        &self,
        ctx: &EventContext,
        now: DateTime<Utc>,
    ) -> Option<Vec<StrategyAction>> {
        if self.straddles.contains_key(&ctx.event_id) {
            return None;
        }

        if self.straddles.len() >= self.config.max_positions {
            return None;
        }

        if self.daily_loss >= self.config.max_daily_loss_usd {
            return None;
        }

        if let Some(last) = self.last_cooldown {
            if (now - last).num_seconds() < self.config.cooldown_secs as i64 {
                return None;
            }
        }

        let remaining = (ctx.end_time - now).num_seconds().max(0) as u64;
        if remaining < self.config.min_time_remaining_secs
            || remaining > self.config.max_time_remaining_secs
        {
            return None;
        }

        let spot = self.spot_prices.get(&ctx.symbol)?;
        let up_quote = self.quote_cache.get(&ctx.up_token)?;
        let down_quote = self.quote_cache.get(&ctx.down_token)?;

        let up_ask = up_quote.best_ask?.to_f64()?;
        let down_ask = down_quote.best_ask?.to_f64()?;

        let straddle_cost = up_ask + down_ask;
        if straddle_cost >= 1.0 {
            return None;
        }

        let strike = ctx.price_to_beat.and_then(|p| p.to_f64()).unwrap_or(*spot);
        let time_frac = remaining as f64 / 900.0;
        let buffer = (*spot - strike) / strike;
        let implied_vol = calculate_implied_volatility(up_ask, buffer, time_frac)?;

        let closes = self.kline_history.get(&ctx.symbol)?;
        let closes_vec: Vec<f64> = closes.iter().copied().collect();
        let realized_vol = realized_vol_from_closes(&closes_vec, 900.0)?;

        let vol_edge = if implied_vol > 1e-12 {
            (realized_vol - implied_vol) / implied_vol
        } else {
            0.0
        };

        if vol_edge < self.config.min_vol_edge_pct {
            debug!(
                event_id = %ctx.event_id,
                vol_edge = %format!("{:.1}%", vol_edge * 100.0),
                realized = %format!("{:.4}", realized_vol),
                implied = %format!("{:.4}", implied_vol),
                "Vol edge insufficient for gamma scalp"
            );
            return None;
        }

        let max_usd = self.config.max_position_usd.to_f64().unwrap_or(10.0);
        let shares = ((max_usd / straddle_cost) * self.config.kelly_fraction)
            .floor()
            .max(1.0) as u64;

        let up_price = Decimal::from_f64_retain(up_ask)?;
        let down_price = Decimal::from_f64_retain(down_ask)?;

        info!(
            event_id = %ctx.event_id,
            symbol = %ctx.symbol,
            vol_edge = %format!("{:.1}%", vol_edge * 100.0),
            shares,
            up_ask = %up_price,
            down_ask = %down_price,
            "Opening gamma scalp straddle"
        );

        let up_order_id = format!("gs-entry-up-{}", Uuid::new_v4());
        let down_order_id = format!("gs-entry-dn-{}", Uuid::new_v4());

        let mut actions = vec![
            StrategyAction::SubmitIntent {
                intent: self.submit_intent(
                    up_order_id.clone(),
                    ctx.event_id.clone(),
                    ctx.up_token.clone(),
                    Side::Up,
                    true,
                    shares,
                    up_price,
                    1,
                ),
            },
            StrategyAction::SubmitIntent {
                intent: self.submit_intent(
                    down_order_id.clone(),
                    ctx.event_id.clone(),
                    ctx.down_token.clone(),
                    Side::Down,
                    true,
                    shares,
                    down_price,
                    1,
                ),
            },
            StrategyAction::LogEvent {
                event: StrategyEvent::new(
                    StrategyEventType::EntryTriggered,
                    format!(
                        "Gamma scalp entry: {} vol_edge={:.1}%",
                        ctx.symbol,
                        vol_edge * 100.0
                    ),
                )
                .with_data("event_id", &ctx.event_id)
                .with_data("vol_edge", format!("{:.4}", vol_edge))
                .with_data("realized_vol", format!("{:.6}", realized_vol))
                .with_data("implied_vol", format!("{:.6}", implied_vol))
                .with_data("shares", shares.to_string()),
            },
        ];

        if self.config.dry_run {
            actions.retain(|a| matches!(a, StrategyAction::LogEvent { .. }));
        }

        Some(actions)
    }

    /// Handle a rebalance or exit for an existing straddle.
    pub(super) fn check_rebalance(
        &self,
        straddle: &Straddle,
        now: DateTime<Utc>,
    ) -> Option<Vec<StrategyAction>> {
        let spot = self.spot_prices.get(&straddle.symbol)?;
        let strike = *spot;

        let remaining = straddle.time_remaining_secs(now);
        let window = straddle.window_secs();

        let greeks = binary_greeks(*spot, strike, 0.01, remaining, window)?;

        if self.rebalancer.should_exit(straddle, now) {
            let exit = self.rebalancer.compute_exit(straddle);
            return Some(self.actions_from_rebalance(straddle, exit));
        }

        if self.rebalancer.should_rebalance(straddle, &greeks, now) {
            if let Some(action) = self.rebalancer.compute_rebalance(straddle, &greeks) {
                return Some(self.actions_from_rebalance(straddle, action));
            }
        }

        None
    }

    /// Convert a RebalanceAction into StrategyActions (orders).
    pub(super) fn actions_from_rebalance(
        &self,
        straddle: &Straddle,
        action: RebalanceAction,
    ) -> Vec<StrategyAction> {
        let mut actions = Vec::new();

        match action {
            RebalanceAction::Rebalance {
                ref sell_token_id,
                sell_shares,
                ref buy_token_id,
                buy_shares,
            } => {
                let (sell_side, buy_side) = if *sell_token_id == straddle.up_token_id {
                    (Side::Up, Side::Down)
                } else {
                    (Side::Down, Side::Up)
                };

                if let Some(quote) = self.quote_cache.get(sell_token_id) {
                    if let Some(bid) = quote.best_bid {
                        let client_order_id = format!("gs-rebal-sell-{}", Uuid::new_v4());
                        actions.push(StrategyAction::SubmitIntent {
                            intent: self.submit_intent(
                                client_order_id,
                                straddle.event_id.clone(),
                                sell_token_id.clone(),
                                sell_side,
                                false,
                                sell_shares,
                                bid,
                                2,
                            ),
                        });
                    }
                }

                if let Some(quote) = self.quote_cache.get(buy_token_id) {
                    if let Some(ask) = quote.best_ask {
                        let client_order_id = format!("gs-rebal-buy-{}", Uuid::new_v4());
                        actions.push(StrategyAction::SubmitIntent {
                            intent: self.submit_intent(
                                client_order_id,
                                straddle.event_id.clone(),
                                buy_token_id.clone(),
                                buy_side,
                                true,
                                buy_shares,
                                ask,
                                2,
                            ),
                        });
                    }
                }

                actions.push(StrategyAction::LogEvent {
                    event: StrategyEvent::new(
                        StrategyEventType::Custom("Rebalance".to_string()),
                        format!(
                            "Gamma rebalance: sell {}×{} buy {}×{}",
                            sell_shares, sell_token_id, buy_shares, buy_token_id
                        ),
                    )
                    .with_data("event_id", &straddle.event_id),
                });
            }
            RebalanceAction::Exit {
                sell_up_shares,
                sell_down_shares,
            } => {
                if sell_up_shares > 0 {
                    if let Some(quote) = self.quote_cache.get(&straddle.up_token_id) {
                        if let Some(bid) = quote.best_bid {
                            let client_order_id = format!("gs-exit-up-{}", Uuid::new_v4());
                            actions.push(StrategyAction::SubmitIntent {
                                intent: self.submit_intent(
                                    client_order_id,
                                    straddle.event_id.clone(),
                                    straddle.up_token_id.clone(),
                                    Side::Up,
                                    false,
                                    sell_up_shares,
                                    bid,
                                    3,
                                ),
                            });
                        }
                    }
                }

                if sell_down_shares > 0 {
                    if let Some(quote) = self.quote_cache.get(&straddle.down_token_id) {
                        if let Some(bid) = quote.best_bid {
                            let client_order_id = format!("gs-exit-dn-{}", Uuid::new_v4());
                            actions.push(StrategyAction::SubmitIntent {
                                intent: self.submit_intent(
                                    client_order_id,
                                    straddle.event_id.clone(),
                                    straddle.down_token_id.clone(),
                                    Side::Down,
                                    false,
                                    sell_down_shares,
                                    bid,
                                    3,
                                ),
                            });
                        }
                    }
                }

                actions.push(StrategyAction::LogEvent {
                    event: StrategyEvent::new(
                        StrategyEventType::ExitTriggered,
                        format!("Gamma scalp exit: {}", straddle.event_id),
                    )
                    .with_data("event_id", &straddle.event_id)
                    .with_data("realized_pnl", straddle.realized_pnl.to_string())
                    .with_data("rebalance_count", straddle.rebalance_count.to_string()),
                });
            }
        }

        if self.config.dry_run {
            actions.retain(|a| matches!(a, StrategyAction::LogEvent { .. }));
        }

        actions
    }

    pub(super) fn submit_intent(
        &self,
        client_order_id: String,
        market_slug: String,
        token_id: String,
        side: Side,
        is_buy: bool,
        shares: u64,
        limit_price: Decimal,
        priority: u8,
    ) -> StrategyOrderIntent {
        StrategyOrderIntent {
            client_order_id,
            domain: Domain::Crypto,
            market_slug,
            token_id,
            side,
            is_buy,
            shares,
            limit_price,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            priority,
            metadata: HashMap::new(),
        }
    }
}

//! Gamma scalping strategy for Polymarket crypto binary options.
//!
//! Profits from realized volatility exceeding implied volatility by maintaining
//! delta-neutral straddle positions and rebalancing as the underlying moves.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use std::collections::{HashMap, VecDeque};
use tracing::{debug, info, warn};

use uuid::Uuid;

use crate::domain::{OrderRequest, OrderStatus, Quote, Side};
use crate::error::Result;
use crate::strategy::fee_model::FeeModel;
use crate::strategy::traits::{
    DataFeed, MarketUpdate, OrderUpdate, PositionInfo, Strategy, StrategyAction, StrategyEvent,
    StrategyEventType, StrategyStateInfo,
};
use crate::strategy::volatility_arb::calculate_implied_volatility;

use super::config::GammaScalpingConfig;
use super::greeks::{binary_greeks, realized_vol_from_closes};
use super::rebalancer::{RebalanceAction, Rebalancer, Straddle};

mod decision_flow;
mod runtime_support;
mod state_view;

/// Metadata for a tracked event (discovered from Polymarket).
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct EventContext {
    event_id: String,
    series_id: String,
    symbol: String,
    up_token: String,
    down_token: String,
    end_time: DateTime<Utc>,
    price_to_beat: Option<Decimal>,
}

/// Tracks a pending order so we can match fills back to straddles.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct PendingOrder {
    client_order_id: String,
    event_id: String,
    token_id: String,
    side: Side,
    is_entry: bool,
    shares: u64,
    price: Decimal,
}

/// Gamma scalping strategy.
#[allow(dead_code)]
pub struct GammaScalpingStrategy {
    config: GammaScalpingConfig,
    /// Active straddle positions keyed by event_id
    straddles: HashMap<String, Straddle>,
    /// Pending orders keyed by client_order_id
    pending_orders: HashMap<String, PendingOrder>,
    /// Kline close prices for realized vol calculation, keyed by symbol
    kline_history: HashMap<String, VecDeque<f64>>,
    /// Latest quotes keyed by token_id
    quote_cache: HashMap<String, Quote>,
    /// Discovered events keyed by event_id
    active_events: HashMap<String, EventContext>,
    /// Latest spot prices keyed by symbol
    spot_prices: HashMap<String, f64>,
    rebalancer: Rebalancer,
    fee_model: FeeModel,
    realized_pnl: Decimal,
    daily_loss: Decimal,
    trade_count: u32,
    last_cooldown: Option<DateTime<Utc>>,
    active: bool,
}

impl GammaScalpingStrategy {
    pub fn new(config: GammaScalpingConfig) -> Self {
        let rebalancer = Rebalancer::new(&config);
        Self {
            config,
            straddles: HashMap::new(),
            pending_orders: HashMap::new(),
            kline_history: HashMap::new(),
            quote_cache: HashMap::new(),
            active_events: HashMap::new(),
            spot_prices: HashMap::new(),
            rebalancer,
            fee_model: FeeModel::crypto(),
            realized_pnl: Decimal::ZERO,
            daily_loss: Decimal::ZERO,
            trade_count: 0,
            last_cooldown: None,
            active: true,
        }
    }

    /// Map a Polymarket series/event to a Binance symbol.
    fn id(&self) -> &str {
        &self.config.id
    }

    fn name(&self) -> &str {
        "gamma_scalping"
    }

    fn description(&self) -> &str {
        "Gamma scalping on Polymarket crypto binary options"
    }

    fn required_feeds(&self) -> Vec<DataFeed> {
        let mut feeds = vec![
            DataFeed::BinanceKlines {
                symbols: self.config.symbols.clone(),
                intervals: vec![self.config.kline_interval.clone()],
                closed_only: true,
            },
            DataFeed::BinanceSpot {
                symbols: self.config.symbols.clone(),
            },
            DataFeed::Tick { interval_ms: 5000 },
        ];

        if !self.config.series_ids.is_empty() {
            feeds.push(DataFeed::PolymarketEvents {
                series_ids: self.config.series_ids.clone(),
            });
        }

        feeds
    }

    async fn on_market_update(&mut self, update: &MarketUpdate) -> Result<Vec<StrategyAction>> {
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

    async fn on_order_update(&mut self, update: &OrderUpdate) -> Result<Vec<StrategyAction>> {
        let pending = match self
            .pending_orders
            .remove(update.client_order_id.as_deref().unwrap_or(""))
        {
            Some(p) => p,
            None => return Ok(vec![]),
        };

        match update.status {
            OrderStatus::Filled => {
                self.trade_count += 1;

                if pending.is_entry {
                    // Update or create straddle
                    let straddle = self
                        .straddles
                        .entry(pending.event_id.clone())
                        .or_insert_with(|| {
                            let ctx = self.active_events.get(&pending.event_id);
                            Straddle {
                                event_id: pending.event_id.clone(),
                                symbol: ctx.map(|c| c.symbol.clone()).unwrap_or_default(),
                                up_token_id: ctx.map(|c| c.up_token.clone()).unwrap_or_default(),
                                down_token_id: ctx
                                    .map(|c| c.down_token.clone())
                                    .unwrap_or_default(),
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
                } else {
                    // Rebalance or exit fill — update share counts
                    if let Some(straddle) = self.straddles.get_mut(&pending.event_id) {
                        let fill_price = update.avg_fill_price.unwrap_or(pending.price);
                        let pnl_delta =
                            (fill_price - pending.price) * Decimal::from(update.filled_qty);

                        if pending.token_id == straddle.up_token_id {
                            // Selling UP or buying UP
                            if fill_price > pending.price {
                                straddle.up_shares =
                                    straddle.up_shares.saturating_sub(update.filled_qty);
                            } else {
                                straddle.up_shares += update.filled_qty;
                            }
                        } else {
                            if fill_price > pending.price {
                                straddle.down_shares =
                                    straddle.down_shares.saturating_sub(update.filled_qty);
                            } else {
                                straddle.down_shares += update.filled_qty;
                            }
                        }

                        straddle.realized_pnl += pnl_delta;
                        straddle.rebalance_count += 1;
                        straddle.last_rebalance = Utc::now();

                        // If both legs are zero, straddle is closed
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

                Ok(vec![StrategyAction::LogEvent {
                    event: StrategyEvent::new(StrategyEventType::OrderFilled, "Order filled")
                        .with_data("event_id", &pending.event_id)
                        .with_data("shares", update.filled_qty.to_string()),
                }])
            }

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
                // Re-insert pending order for partial fills etc.
                if let Some(coid) = &update.client_order_id {
                    self.pending_orders.insert(coid.clone(), pending);
                }
                Ok(vec![])
            }
        }
    }

    async fn on_tick(&mut self, now: DateTime<Utc>) -> Result<Vec<StrategyAction>> {
        if !self.active {
            return Ok(vec![]);
        }

        let mut actions = Vec::new();

        // 1. Check existing straddles for rebalance/exit
        let event_ids: Vec<String> = self.straddles.keys().cloned().collect();
        for event_id in &event_ids {
            if let Some(straddle) = self.straddles.get(event_id) {
                if let Some(mut rebal_actions) = self.check_rebalance(straddle, now) {
                    actions.append(&mut rebal_actions);
                }
            }
        }

        // 2. Evaluate new entries
        let candidates: Vec<EventContext> = self.active_events.values().cloned().collect();
        for ctx in &candidates {
            if let Some(mut entry_actions) = self.evaluate_entry(ctx, now) {
                actions.append(&mut entry_actions);
            }
        }

        Ok(actions)
    }

    fn state(&self) -> StrategyStateInfo {
        let total_exposure: Decimal = self.straddles.values().map(|s| s.cost_basis).sum();

        let unrealized: Decimal = self
            .straddles
            .values()
            .map(|s| {
                let up_val = self
                    .quote_cache
                    .get(&s.up_token_id)
                    .and_then(|q| q.best_bid)
                    .unwrap_or(s.up_entry_price)
                    * Decimal::from(s.up_shares);
                let down_val = self
                    .quote_cache
                    .get(&s.down_token_id)
                    .and_then(|q| q.best_bid)
                    .unwrap_or(s.down_entry_price)
                    * Decimal::from(s.down_shares);
                up_val + down_val - s.cost_basis + s.realized_pnl
            })
            .sum();

        let mut metrics = HashMap::new();
        metrics.insert("straddles".to_string(), self.straddles.len().to_string());
        metrics.insert("trade_count".to_string(), self.trade_count.to_string());
        metrics.insert("daily_loss".to_string(), self.daily_loss.to_string());
        metrics.insert("dry_run".to_string(), self.config.dry_run.to_string());

        StrategyStateInfo {
            strategy_id: self.config.id.clone(),
            phase: if self.straddles.is_empty() {
                "scanning".to_string()
            } else {
                "active".to_string()
            },
            enabled: self.config.enabled,
            active: self.active,
            position_count: self.straddles.len(),
            pending_order_count: self.pending_orders.len(),
            total_exposure,
            unrealized_pnl: unrealized,
            realized_pnl_today: self.realized_pnl,
            last_update: Utc::now(),
            metrics,
        }
    }

    fn positions(&self) -> Vec<PositionInfo> {
        self.straddles
            .values()
            .flat_map(|s| {
                let mut positions = Vec::new();
                if s.up_shares > 0 {
                    let mut p = PositionInfo::new(
                        s.up_token_id.clone(),
                        Side::Up,
                        s.up_shares,
                        s.up_entry_price,
                        self.config.id.clone(),
                    );
                    if let Some(q) = self.quote_cache.get(&s.up_token_id) {
                        if let Some(bid) = q.best_bid {
                            p.update_price(bid);
                        }
                    }
                    p.metadata
                        .insert("event_id".to_string(), s.event_id.clone());
                    p.metadata
                        .insert("leg".to_string(), "straddle_up".to_string());
                    positions.push(p);
                }
                if s.down_shares > 0 {
                    let mut p = PositionInfo::new(
                        s.down_token_id.clone(),
                        Side::Down,
                        s.down_shares,
                        s.down_entry_price,
                        self.config.id.clone(),
                    );
                    if let Some(q) = self.quote_cache.get(&s.down_token_id) {
                        if let Some(bid) = q.best_bid {
                            p.update_price(bid);
                        }
                    }
                    p.metadata
                        .insert("event_id".to_string(), s.event_id.clone());
                    p.metadata
                        .insert("leg".to_string(), "straddle_down".to_string());
                    positions.push(p);
                }
                positions
            })
            .collect()
    }

    fn is_active(&self) -> bool {
        self.active && self.config.enabled
    }

    async fn shutdown(&mut self) -> Result<Vec<StrategyAction>> {
        self.active = false;
        let mut actions = Vec::new();

        // Cancel all pending orders
        for (_, pending) in self.pending_orders.drain() {
            actions.push(StrategyAction::CancelOrder {
                order_id: pending.client_order_id,
            });
        }

        // Exit all straddles
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

    fn reset(&mut self) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn required_feeds_are_static_and_series_driven() {
        let mut config = GammaScalpingConfig::default();
        config.series_ids = vec!["10684".to_string()];
        let mut strategy = GammaScalpingStrategy::new(config);

        let feeds_before = strategy.required_feeds();
        assert!(feeds_before.iter().any(|feed| matches!(
            feed,
            DataFeed::PolymarketEvents { series_ids } if series_ids == &vec!["10684".to_string()]
        )));
        assert!(!feeds_before
            .iter()
            .any(|feed| matches!(feed, DataFeed::PolymarketQuotes { .. })));

        strategy.active_events.insert(
            "evt-1".to_string(),
            EventContext {
                event_id: "evt-1".to_string(),
                series_id: "10684".to_string(),
                symbol: "BTCUSDT".to_string(),
                up_token: "up-token".to_string(),
                down_token: "down-token".to_string(),
                end_time: Utc::now() + Duration::minutes(5),
                price_to_beat: None,
            },
        );

        let feeds_after = strategy.required_feeds();
        assert_eq!(feeds_before, feeds_after, "required feeds must stay static");
    }

    #[tokio::test]
    async fn event_discovery_tracks_event_without_dynamic_quote_subscription_action() {
        let mut strategy = GammaScalpingStrategy::new(GammaScalpingConfig::default());
        let actions = strategy
            .on_market_update(&MarketUpdate::EventDiscovered {
                event_id: "evt-1".to_string(),
                series_id: "btc-daily".to_string(),
                up_token: "up-token".to_string(),
                down_token: "down-token".to_string(),
                end_time: Utc::now() + Duration::minutes(5),
                price_to_beat: None,
                title: None,
                condition_id: None,
            })
            .await
            .expect("event discovery should succeed");

        assert!(actions.is_empty(), "feed lifecycle is runtime-owned");
        let event = strategy
            .active_events
            .get("evt-1")
            .expect("event should be tracked");
        assert_eq!(event.up_token, "up-token");
        assert_eq!(event.down_token, "down-token");
    }

    #[test]
    fn evaluate_entry_emits_submit_intents() {
        use rust_decimal_macros::dec;
        let mut config = GammaScalpingConfig::default();
        config.dry_run = false;
        config.vol_lookback_periods = 5;
        let mut strategy = GammaScalpingStrategy::new(config);
        strategy.spot_prices.insert("BTCUSDT".to_string(), 100.0);
        strategy.kline_history.insert(
            "BTCUSDT".to_string(),
            VecDeque::from(vec![100.0, 110.0, 90.0, 120.0, 80.0, 130.0]),
        );
        strategy.quote_cache.insert(
            "token-up".to_string(),
            Quote {
                side: Side::Up,
                best_bid: Some(dec!(0.30)),
                best_ask: Some(dec!(0.31)),
                bid_size: Some(dec!(100)),
                ask_size: Some(dec!(100)),
                timestamp: Utc::now(),
            },
        );
        strategy.quote_cache.insert(
            "token-down".to_string(),
            Quote {
                side: Side::Down,
                best_bid: Some(dec!(0.28)),
                best_ask: Some(dec!(0.29)),
                bid_size: Some(dec!(100)),
                ask_size: Some(dec!(100)),
                timestamp: Utc::now(),
            },
        );

        let ctx = EventContext {
            event_id: "event-1".to_string(),
            series_id: "btc-series".to_string(),
            symbol: "BTCUSDT".to_string(),
            up_token: "token-up".to_string(),
            down_token: "token-down".to_string(),
            end_time: Utc::now() + chrono::Duration::seconds(600),
            price_to_beat: Some(dec!(100)),
        };

        let actions = strategy
            .evaluate_entry(&ctx, Utc::now())
            .expect("entry actions");

        assert!(matches!(
            actions.first(),
            Some(StrategyAction::SubmitIntent { .. })
        ));
        assert!(matches!(
            actions.get(1),
            Some(StrategyAction::SubmitIntent { .. })
        ));
    }
}

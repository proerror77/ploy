//! Gamma scalping strategy for Polymarket crypto binary options.
//!
//! Profits from realized volatility exceeding implied volatility by maintaining
//! delta-neutral straddle positions and rebalancing as the underlying moves.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use std::collections::{HashMap, VecDeque};
use tracing::{info, warn};

use crate::domain::{OrderStatus, Quote, Side};
use crate::error::Result;
use crate::strategy::fee_model::FeeModel;
use crate::strategy::traits::{
    DataFeed, MarketUpdate, OrderUpdate, PositionInfo, Strategy, StrategyAction, StrategyEvent,
    StrategyEventType, StrategyStateInfo,
};

use super::config::GammaScalpingConfig;
use super::rebalancer::{Rebalancer, Straddle};

mod decision_flow;

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
    fn symbol_for_event(&self, series_id: &str) -> Option<&str> {
        // Match series_id patterns to symbols
        for sym in &self.config.symbols {
            let prefix = sym.replace("USDT", "").to_lowercase();
            if series_id.to_lowercase().contains(&prefix) {
                return Some(sym.as_str());
            }
        }
        // Fallback: check active events
        None
    }

    /// Check if we should enter a new straddle on this event.
    fn evaluate_entry(
        &self,
        ctx: &EventContext,
        now: DateTime<Utc>,
    ) -> Option<Vec<StrategyAction>> {
        // Already have a straddle on this event?
        if self.straddles.contains_key(&ctx.event_id) {
            return None;
        }

        // At max positions?
        if self.straddles.len() >= self.config.max_positions {
            return None;
        }

        // Daily loss limit?
        if self.daily_loss >= self.config.max_daily_loss_usd {
            return None;
        }

        // Cooldown check
        if let Some(last) = self.last_cooldown {
            if (now - last).num_seconds() < self.config.cooldown_secs as i64 {
                return None;
            }
        }

        // Time window check
        let remaining = (ctx.end_time - now).num_seconds().max(0) as u64;
        if remaining < self.config.min_time_remaining_secs
            || remaining > self.config.max_time_remaining_secs
        {
            return None;
        }

        // Need spot price and quotes
        let spot = self.spot_prices.get(&ctx.symbol)?;
        let up_quote = self.quote_cache.get(&ctx.up_token)?;
        let down_quote = self.quote_cache.get(&ctx.down_token)?;

        let up_ask = up_quote.best_ask?.to_f64()?;
        let down_ask = down_quote.best_ask?.to_f64()?;

        // Straddle cost: both asks should sum to roughly $1 for a fair market
        let straddle_cost = up_ask + down_ask;
        if straddle_cost >= 1.0 {
            // No edge — straddle costs more than max payout
            return None;
        }

        // Compute implied vol from the UP token price
        let strike = ctx.price_to_beat.and_then(|p| p.to_f64()).unwrap_or(*spot);
        let time_frac = remaining as f64 / 900.0;
        let buffer = (*spot - strike) / strike;
        let implied_vol = calculate_implied_volatility(up_ask, buffer, time_frac)?;

        // Compute realized vol from kline history
        let closes = self.kline_history.get(&ctx.symbol)?;
        let closes_vec: Vec<f64> = closes.iter().copied().collect();
        let realized_vol = realized_vol_from_closes(&closes_vec, 900.0)?;

        // Vol edge: realized > implied
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

        // Position sizing: max_position_usd / straddle_cost
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

        let up_order = OrderRequest::buy_limit(ctx.up_token.clone(), Side::Up, shares, up_price);
        let down_order =
            OrderRequest::buy_limit(ctx.down_token.clone(), Side::Down, shares, down_price);

        let mut actions = vec![
            StrategyAction::SubmitOrder {
                client_order_id: up_order_id.clone(),
                purpose: crate::strategy::OrderPurpose::Entry,
                order: up_order,
                priority: 1,
            },
            StrategyAction::SubmitOrder {
                client_order_id: down_order_id.clone(),
                purpose: crate::strategy::OrderPurpose::Entry,
                order: down_order,
                priority: 1,
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
    fn check_rebalance(
        &self,
        straddle: &Straddle,
        now: DateTime<Utc>,
    ) -> Option<Vec<StrategyAction>> {
        let spot = self.spot_prices.get(&straddle.symbol)?;
        let strike = *spot; // ATM approximation for short-dated binaries

        let remaining = straddle.time_remaining_secs(now);
        let window = straddle.window_secs();

        let greeks = binary_greeks(*spot, strike, 0.01, remaining, window)?;

        // Check exit first
        if self.rebalancer.should_exit(straddle, now) {
            let exit = self.rebalancer.compute_exit(straddle);
            return Some(self.actions_from_rebalance(straddle, exit));
        }

        // Check rebalance
        if self.rebalancer.should_rebalance(straddle, &greeks, now) {
            if let Some(action) = self.rebalancer.compute_rebalance(straddle, &greeks) {
                return Some(self.actions_from_rebalance(straddle, action));
            }
        }

        None
    }

    /// Convert a RebalanceAction into StrategyActions (orders).
    fn actions_from_rebalance(
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
                // Determine sides
                let (sell_side, buy_side) = if *sell_token_id == straddle.up_token_id {
                    (Side::Up, Side::Down)
                } else {
                    (Side::Down, Side::Up)
                };

                // Sell order — use best bid as limit
                if let Some(quote) = self.quote_cache.get(sell_token_id) {
                    if let Some(bid) = quote.best_bid {
                        let sell_order = OrderRequest::sell_limit(
                            sell_token_id.clone(),
                            sell_side,
                            sell_shares,
                            bid,
                        );
                        actions.push(StrategyAction::SubmitOrder {
                            client_order_id: format!("gs-rebal-sell-{}", Uuid::new_v4()),
                            purpose: crate::strategy::OrderPurpose::Hedge,
                            order: sell_order,
                            priority: 2,
                        });
                    }
                }

                // Buy order — use best ask as limit
                if let Some(quote) = self.quote_cache.get(buy_token_id) {
                    if let Some(ask) = quote.best_ask {
                        let buy_order = OrderRequest::buy_limit(
                            buy_token_id.clone(),
                            buy_side,
                            buy_shares,
                            ask,
                        );
                        actions.push(StrategyAction::SubmitOrder {
                            client_order_id: format!("gs-rebal-buy-{}", Uuid::new_v4()),
                            purpose: crate::strategy::OrderPurpose::Hedge,
                            order: buy_order,
                            priority: 2,
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
                            let order = OrderRequest::sell_limit(
                                straddle.up_token_id.clone(),
                                Side::Up,
                                sell_up_shares,
                                bid,
                            );
                            actions.push(StrategyAction::SubmitOrder {
                                client_order_id: format!("gs-exit-up-{}", Uuid::new_v4()),
                                purpose: crate::strategy::OrderPurpose::Exit,
                                order,
                                priority: 3,
                            });
                        }
                    }
                }
                if sell_down_shares > 0 {
                    if let Some(quote) = self.quote_cache.get(&straddle.down_token_id) {
                        if let Some(bid) = quote.best_bid {
                            let order = OrderRequest::sell_limit(
                                straddle.down_token_id.clone(),
                                Side::Down,
                                sell_down_shares,
                                bid,
                            );
                            actions.push(StrategyAction::SubmitOrder {
                                client_order_id: format!("gs-exit-dn-{}", Uuid::new_v4()),
                                purpose: crate::strategy::OrderPurpose::Exit,
                                order,
                                priority: 3,
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
}

#[async_trait]
impl Strategy for GammaScalpingStrategy {
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

//! Momentum Strategy
//!
//! Implements CEX-to-DEX momentum arbitrage:
//! 1. Monitor Binance for BTC/ETH/SOL price movements
//! 2. When spot price moves, Polymarket odds lag behind
//! 3. Enter the side that should win before odds adjust
//! 4. Exit via take-profit, stop-loss, trailing stop, or time-based

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, error, info, warn};

use crate::domain::{OrderRequest, OrderStatus, Quote, Side};
use crate::error::Result;

use crate::strategy::detectors::{MomentumDetector, MomentumDetectorConfig, MomentumSignal, TrendDirection};
use crate::strategy::traits::{
    AlertLevel, DataFeed, MarketUpdate, OrderUpdate, PositionInfo, RiskLevel, Strategy,
    StrategyAction, StrategyEvent, StrategyEventType, StrategyStateInfo,
};

/// Momentum strategy configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MomentumConfig {
    /// Strategy ID
    pub id: String,
    /// Is strategy enabled
    pub enabled: bool,
    /// Minimum CEX price move to trigger (e.g., 0.003 = 0.3%)
    pub min_move_pct: Decimal,
    /// Maximum Polymarket odds for entry (e.g., 0.40 = 40¢)
    pub max_entry_price: Decimal,
    /// Minimum estimated edge to enter (e.g., 0.03 = 3%)
    pub min_edge: Decimal,
    /// Shares per trade
    pub shares_per_trade: u64,
    /// Maximum concurrent positions
    pub max_positions: usize,
    /// Cooldown between trades on same symbol (seconds)
    pub cooldown_secs: u64,
    /// Maximum trades per day (0 = unlimited)
    pub max_daily_trades: u32,
    /// Symbols to track (e.g., BTCUSDT, ETHUSDT, SOLUSDT)
    pub symbols: Vec<String>,
    /// Take profit percentage
    pub take_profit_pct: Decimal,
    /// Stop loss percentage
    pub stop_loss_pct: Decimal,
    /// Trailing stop percentage
    pub trailing_stop_pct: Decimal,
    /// Exit before resolution (seconds)
    pub exit_before_resolution_secs: u64,
    /// Momentum detector config
    pub detector_config: MomentumDetectorConfig,
    /// Dry run mode
    pub dry_run: bool,
}

impl Default for MomentumConfig {
    fn default() -> Self {
        Self {
            id: "momentum".to_string(),
            enabled: true,
            // === AGGRESSIVE ENTRY (CRYINGLITTLEBABY style) ===
            min_move_pct: dec!(0.003),      // 0.3% minimum move (was 0.5%)
            max_entry_price: dec!(0.40),    // Max 40¢ entry (was 55¢)
            min_edge: dec!(0.03),           // 3% minimum edge (was 5%)
            shares_per_trade: 100,
            // === ANTI-OVERTRADING CONTROLS ===
            max_positions: 3,               // Max 3 concurrent (was 5)
            cooldown_secs: 60,              // 60s between same symbol (was 30)
            max_daily_trades: 20,           // Max 20 trades/day
            symbols: vec!["BTCUSDT".into(), "ETHUSDT".into(), "SOLUSDT".into()],
            take_profit_pct: dec!(0.20),
            stop_loss_pct: dec!(0.15),
            trailing_stop_pct: dec!(0.10),
            exit_before_resolution_secs: 30,
            detector_config: MomentumDetectorConfig::default(),
            dry_run: true,
        }
    }
}

/// Symbol to series mapping
#[derive(Debug, Clone)]
pub struct SeriesMapping {
    pub symbol: String,
    pub series_ids: Vec<String>,
}

impl SeriesMapping {
    /// Get standard mappings
    pub fn standard_mappings() -> Vec<SeriesMapping> {
        vec![
            SeriesMapping {
                symbol: "BTCUSDT".into(),
                series_ids: vec!["41".into()], // btc-up-or-down-daily
            },
            SeriesMapping {
                symbol: "ETHUSDT".into(),
                series_ids: vec!["10191".into(), "10117".into(), "10332".into()],
            },
            SeriesMapping {
                symbol: "SOLUSDT".into(),
                series_ids: vec!["10423".into(), "10333".into()],
            },
        ]
    }
}

/// Active position
#[derive(Debug, Clone)]
struct ActivePosition {
    token_id: String,
    symbol: String,
    side: Side,
    entry_price: Decimal,
    shares: u64,
    entry_time: DateTime<Utc>,
    highest_price: Decimal,
    event_end_time: DateTime<Utc>,
    client_order_id: String,
}

impl ActivePosition {
    fn pnl_pct(&self, current_price: Decimal) -> Decimal {
        if self.entry_price.is_zero() {
            return Decimal::ZERO;
        }
        (current_price - self.entry_price) / self.entry_price
    }

    fn update_high(&mut self, price: Decimal) {
        if price > self.highest_price {
            self.highest_price = price;
        }
    }

    fn time_remaining(&self) -> i64 {
        (self.event_end_time - Utc::now()).num_seconds().max(0)
    }
}

/// Exit reason
#[derive(Debug, Clone)]
pub enum ExitReason {
    TakeProfit,
    StopLoss,
    TrailingStop,
    TimeExit,
    Manual,
}

impl std::fmt::Display for ExitReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExitReason::TakeProfit => write!(f, "TakeProfit"),
            ExitReason::StopLoss => write!(f, "StopLoss"),
            ExitReason::TrailingStop => write!(f, "TrailingStop"),
            ExitReason::TimeExit => write!(f, "TimeExit"),
            ExitReason::Manual => write!(f, "Manual"),
        }
    }
}

/// Pending order tracking
#[derive(Debug, Clone)]
struct PendingOrder {
    client_order_id: String,
    symbol: String,
    side: Side,
    is_entry: bool,
    signal: Option<EntrySignal>,
}

/// Entry signal data
#[derive(Debug, Clone)]
struct EntrySignal {
    symbol: String,
    side: Side,
    cex_move_pct: Decimal,
    pm_price: Decimal,
    edge: Decimal,
    event_end_time: DateTime<Utc>,
    token_id: String,
}

/// Momentum strategy
pub struct MomentumStrategy {
    config: MomentumConfig,
    detector: MomentumDetector,
    positions: HashMap<String, ActivePosition>,
    pending_orders: HashMap<String, PendingOrder>,
    last_trade_time: HashMap<String, DateTime<Utc>>,
    last_binance_prices: HashMap<String, (Decimal, DateTime<Utc>)>,
    price_history: HashMap<String, Vec<(DateTime<Utc>, Decimal)>>,
    active_events: HashMap<String, EventContext>,
    realized_pnl: Decimal,
}

/// Event context for trading
#[derive(Debug, Clone)]
struct EventContext {
    event_id: String,
    symbol: String,
    up_token_id: String,
    down_token_id: String,
    end_time: DateTime<Utc>,
}

impl MomentumStrategy {
    /// Create a new momentum strategy
    pub fn new(config: MomentumConfig) -> Self {
        let detector = MomentumDetector::new(config.detector_config.clone());

        Self {
            config,
            detector,
            positions: HashMap::new(),
            pending_orders: HashMap::new(),
            last_trade_time: HashMap::new(),
            last_binance_prices: HashMap::new(),
            price_history: HashMap::new(),
            active_events: HashMap::new(),
            realized_pnl: Decimal::ZERO,
        }
    }

    /// Check if symbol is in cooldown
    fn in_cooldown(&self, symbol: &str) -> bool {
        if let Some(last_time) = self.last_trade_time.get(symbol) {
            let elapsed = Utc::now() - *last_time;
            return elapsed.num_seconds() < self.config.cooldown_secs as i64;
        }
        false
    }

    /// Calculate momentum from price history
    fn calculate_momentum(&self, symbol: &str) -> Option<Decimal> {
        let history = self.price_history.get(symbol)?;
        if history.len() < 2 {
            return None;
        }

        let now = Utc::now();
        let lookback = chrono::Duration::seconds(self.config.detector_config.long_window_secs);
        let cutoff = now - lookback;

        // Get old price
        let old_price = history
            .iter()
            .rev()
            .find(|(ts, _)| *ts < cutoff)
            .or_else(|| history.first())?
            .1;

        // Get current price
        let current_price = history.last()?.1;

        if old_price.is_zero() {
            return None;
        }

        Some((current_price - old_price) / old_price)
    }

    /// Estimate fair value based on momentum
    fn estimate_fair_value(&self, momentum: Decimal) -> Decimal {
        let base_prob = dec!(0.50);
        let momentum_factor = momentum.abs() * dec!(10);
        (base_prob + momentum_factor).min(dec!(0.90))
    }

    /// Check exit conditions for a position
    fn check_exit(&self, pos: &ActivePosition, current_bid: Decimal) -> Option<ExitReason> {
        let pnl_pct = pos.pnl_pct(current_bid);

        // Take profit
        if pnl_pct >= self.config.take_profit_pct {
            return Some(ExitReason::TakeProfit);
        }

        // Stop loss
        if pnl_pct <= -self.config.stop_loss_pct {
            return Some(ExitReason::StopLoss);
        }

        // Trailing stop
        if pos.highest_price > pos.entry_price && current_bid < pos.highest_price {
            let drop = (pos.highest_price - current_bid) / pos.highest_price;
            if drop >= self.config.trailing_stop_pct {
                return Some(ExitReason::TrailingStop);
            }
        }

        // Time exit
        if pos.time_remaining() < self.config.exit_before_resolution_secs as i64 {
            return Some(ExitReason::TimeExit);
        }

        None
    }

    /// Process Binance price update
    fn on_binance_price(&mut self, symbol: &str, price: Decimal, timestamp: DateTime<Utc>) -> Vec<StrategyAction> {
        let mut actions = Vec::new();

        if !self.config.symbols.contains(&symbol.to_string()) {
            return actions;
        }

        // Update price history
        let history = self.price_history.entry(symbol.to_string()).or_default();
        history.push((timestamp, price));

        // Keep only recent history
        let cutoff = timestamp - chrono::Duration::seconds(300);
        history.retain(|(ts, _)| *ts > cutoff);

        self.last_binance_prices.insert(symbol.to_string(), (price, timestamp));

        // Check for momentum signal
        if let Some(momentum) = self.calculate_momentum(symbol) {
            if momentum.abs() >= self.config.min_move_pct {
                // Find matching event
                if let Some(event) = self.find_event_for_symbol(symbol) {
                    let side = if momentum > Decimal::ZERO { Side::Up } else { Side::Down };

                    // Check entry conditions
                    if self.can_enter(symbol, side, &event) {
                        let fair_value = self.estimate_fair_value(momentum);

                        let signal = EntrySignal {
                            symbol: symbol.to_string(),
                            side,
                            cex_move_pct: momentum,
                            pm_price: dec!(0.50), // Will be updated from PM quote
                            edge: fair_value - dec!(0.50),
                            event_end_time: event.end_time,
                            token_id: match side {
                                Side::Up => event.up_token_id.clone(),
                                Side::Down => event.down_token_id.clone(),
                            },
                        };

                        // Request PM quotes to confirm entry
                        actions.push(StrategyAction::LogEvent {
                            event: StrategyEvent::new(
                                StrategyEventType::SignalDetected,
                                format!("Momentum signal: {} {:?} ({:.2}%)", symbol, side, momentum * dec!(100)),
                            ),
                        });
                    }
                }
            }
        }

        actions
    }

    /// Find event for a symbol
    fn find_event_for_symbol(&self, symbol: &str) -> Option<&EventContext> {
        self.active_events.values().find(|e| e.symbol == symbol)
    }

    /// Check if we can enter a position
    fn can_enter(&self, symbol: &str, _side: Side, _event: &EventContext) -> bool {
        // Check position limit
        if self.positions.len() >= self.config.max_positions {
            return false;
        }

        // Check if already have position in this symbol
        if self.positions.values().any(|p| p.symbol == symbol) {
            return false;
        }

        // Check cooldown
        if self.in_cooldown(symbol) {
            return false;
        }

        true
    }

    /// Create entry order
    fn create_entry_order(&mut self, signal: EntrySignal) -> Vec<StrategyAction> {
        let mut actions = Vec::new();

        // Check if PM price is attractive
        if signal.pm_price > self.config.max_entry_price {
            debug!(
                "PM price {:.2}¢ > max {:.2}¢, skipping",
                signal.pm_price * dec!(100),
                self.config.max_entry_price * dec!(100)
            );
            return actions;
        }

        // Check edge
        if signal.edge < self.config.min_edge {
            debug!(
                "Edge {:.2}% < min {:.2}%, skipping",
                signal.edge * dec!(100),
                self.config.min_edge * dec!(100)
            );
            return actions;
        }

        let client_order_id = format!("{}-entry-{}", self.config.id, Utc::now().timestamp_millis());

        let order = OrderRequest::buy_limit(
            signal.token_id.clone(),
            signal.side,
            self.config.shares_per_trade,
            signal.pm_price,
        );

        self.pending_orders.insert(
            client_order_id.clone(),
            PendingOrder {
                client_order_id: client_order_id.clone(),
                symbol: signal.symbol.clone(),
                side: signal.side,
                is_entry: true,
                signal: Some(signal.clone()),
            },
        );

        info!(
            "ENTRY: {} {:?} @ {:.2}¢ (CEX: {:.2}%, edge: {:.2}%)",
            signal.symbol,
            signal.side,
            signal.pm_price * dec!(100),
            signal.cex_move_pct * dec!(100),
            signal.edge * dec!(100)
        );

        actions.push(StrategyAction::SubmitOrder {
            client_order_id,
            order,
            priority: 5,
        });

        actions
    }

    /// Create exit order
    fn create_exit_order(&mut self, symbol: &str, price: Decimal, reason: ExitReason) -> Vec<StrategyAction> {
        let mut actions = Vec::new();

        let pos = match self.positions.get(symbol) {
            Some(p) => p.clone(),
            None => return actions,
        };

        let pnl_pct = pos.pnl_pct(price);

        info!(
            "EXIT: {} {:?} @ {:.2}¢ - {} (P&L: {:.2}%)",
            symbol,
            pos.side,
            price * dec!(100),
            reason,
            pnl_pct * dec!(100)
        );

        let client_order_id = format!("{}-exit-{}", self.config.id, Utc::now().timestamp_millis());

        let order = OrderRequest::sell_limit(
            pos.token_id.clone(),
            pos.side,
            pos.shares,
            price,
        );

        self.pending_orders.insert(
            client_order_id.clone(),
            PendingOrder {
                client_order_id: client_order_id.clone(),
                symbol: symbol.to_string(),
                side: pos.side,
                is_entry: false,
                signal: None,
            },
        );

        actions.push(StrategyAction::SubmitOrder {
            client_order_id,
            order,
            priority: 10,
        });

        actions
    }
}

#[async_trait]
impl Strategy for MomentumStrategy {
    fn id(&self) -> &str {
        &self.config.id
    }

    fn name(&self) -> &str {
        "Momentum Strategy"
    }

    fn description(&self) -> &str {
        "CEX-to-DEX momentum arbitrage on prediction markets"
    }

    fn required_feeds(&self) -> Vec<DataFeed> {
        let mut feeds = vec![
            DataFeed::BinanceSpot {
                symbols: self.config.symbols.clone(),
            },
            DataFeed::Tick { interval_ms: 1000 },
        ];

        // Add Polymarket event feeds for each symbol's series
        for mapping in SeriesMapping::standard_mappings() {
            if self.config.symbols.contains(&mapping.symbol) {
                feeds.push(DataFeed::PolymarketEvents {
                    series_ids: mapping.series_ids,
                });
            }
        }

        feeds
    }

    async fn on_market_update(&mut self, update: &MarketUpdate) -> Result<Vec<StrategyAction>> {
        let mut actions = Vec::new();

        match update {
            MarketUpdate::BinancePrice {
                symbol,
                price,
                timestamp,
            } => {
                actions.extend(self.on_binance_price(symbol, *price, *timestamp));
            }
            MarketUpdate::PolymarketQuote {
                token_id,
                side,
                quote,
                ..
            } => {
                // Update position high water mark and check exit - collect info first to avoid borrow conflicts
                let exit_info: Option<(String, Decimal, ExitReason)> = {
                    if let Some(pos) = self.positions.get_mut(token_id) {
                        if let Some(bid) = quote.best_bid {
                            pos.update_high(bid);
                            self.check_exit(pos, bid).map(|reason| (pos.symbol.clone(), bid, reason))
                        } else {
                            None
                        }
                    } else {
                        // Try finding by token_id in case key differs
                        let mut found = None;
                        for pos in self.positions.values_mut() {
                            if pos.token_id == *token_id {
                                if let Some(bid) = quote.best_bid {
                                    pos.update_high(bid);
                                    if let Some(reason) = self.check_exit(pos, bid) {
                                        found = Some((pos.symbol.clone(), bid, reason));
                                    }
                                }
                                break;
                            }
                        }
                        found
                    }
                };

                // Create exit order outside the borrow
                if let Some((symbol, price, reason)) = exit_info {
                    actions.extend(self.create_exit_order(&symbol, price, reason));
                }

                // Check for entry confirmation - collect signals first
                let signals_to_process: Vec<EntrySignal> = self.pending_orders
                    .values()
                    .filter_map(|pending| {
                        if pending.is_entry {
                            if let Some(signal) = &pending.signal {
                                if signal.token_id == *token_id {
                                    if let Some(ask) = quote.best_ask {
                                        let mut updated_signal = signal.clone();
                                        updated_signal.pm_price = ask;
                                        updated_signal.edge = self.estimate_fair_value(signal.cex_move_pct) - ask;
                                        return Some(updated_signal);
                                    }
                                }
                            }
                        }
                        None
                    })
                    .collect();

                // Create entry orders outside the borrow
                for signal in signals_to_process {
                    actions.extend(self.create_entry_order(signal));
                }
            }
            MarketUpdate::EventDiscovered {
                event_id,
                series_id,
                up_token,
                down_token,
                end_time,
            } => {
                // Find which symbol this series belongs to
                for mapping in SeriesMapping::standard_mappings() {
                    if mapping.series_ids.contains(series_id) {
                        let event = EventContext {
                            event_id: event_id.clone(),
                            symbol: mapping.symbol.clone(),
                            up_token_id: up_token.clone(),
                            down_token_id: down_token.clone(),
                            end_time: *end_time,
                        };

                        self.active_events.insert(event_id.clone(), event);

                        // Subscribe to token quotes
                        actions.push(StrategyAction::SubscribeFeed {
                            feed: DataFeed::PolymarketQuotes {
                                tokens: vec![up_token.clone(), down_token.clone()],
                            },
                        });

                        info!("Discovered event for {}: {}", mapping.symbol, event_id);
                        break;
                    }
                }
            }
            MarketUpdate::EventExpired { event_id } => {
                self.active_events.remove(event_id);
            }
            _ => {}
        }

        Ok(actions)
    }

    async fn on_order_update(&mut self, update: &OrderUpdate) -> Result<Vec<StrategyAction>> {
        let mut actions = Vec::new();

        let pending = self.pending_orders
            .iter()
            .find(|(_, p)| {
                p.client_order_id == update.client_order_id.as_deref().unwrap_or("")
            })
            .map(|(k, v)| (k.clone(), v.clone()));

        let Some((client_id, pending)) = pending else {
            return Ok(actions);
        };

        match update.status {
            OrderStatus::Filled => {
                let fill_price = update.avg_fill_price.unwrap_or(Decimal::ZERO);

                if pending.is_entry {
                    // Entry filled
                    if let Some(signal) = &pending.signal {
                        let position = ActivePosition {
                            token_id: signal.token_id.clone(),
                            symbol: pending.symbol.clone(),
                            side: pending.side,
                            entry_price: fill_price,
                            shares: update.filled_qty,
                            entry_time: Utc::now(),
                            highest_price: fill_price,
                            event_end_time: signal.event_end_time,
                            client_order_id: client_id.clone(),
                        };

                        self.positions.insert(pending.symbol.clone(), position);
                        self.last_trade_time.insert(pending.symbol.clone(), Utc::now());

                        info!(
                            "Entry filled: {} {:?} {} shares @ {:.2}¢",
                            pending.symbol, pending.side, update.filled_qty, fill_price * dec!(100)
                        );

                        actions.push(StrategyAction::LogEvent {
                            event: StrategyEvent::new(StrategyEventType::OrderFilled, "Entry filled")
                                .with_data("symbol", pending.symbol.clone())
                                .with_data("price", fill_price.to_string()),
                        });
                    }
                } else {
                    // Exit filled
                    if let Some(pos) = self.positions.remove(&pending.symbol) {
                        let pnl = (fill_price - pos.entry_price) * Decimal::from(pos.shares);
                        self.realized_pnl += pnl;

                        info!(
                            "Exit filled: {} {} shares @ {:.2}¢ (P&L: ${:.2})",
                            pending.symbol, update.filled_qty, fill_price * dec!(100), pnl
                        );

                        actions.push(StrategyAction::LogEvent {
                            event: StrategyEvent::new(StrategyEventType::ExitTriggered, "Exit filled")
                                .with_data("symbol", pending.symbol.clone())
                                .with_data("pnl", pnl.to_string()),
                        });
                    }
                }

                self.pending_orders.remove(&client_id);
            }
            OrderStatus::Cancelled | OrderStatus::Rejected | OrderStatus::Expired => {
                if !pending.is_entry {
                    // Exit failed - critical
                    error!("Exit order failed for {}: {:?}", pending.symbol, update.status);
                    actions.push(StrategyAction::Alert {
                        level: AlertLevel::Critical,
                        message: format!("Exit order failed for {}", pending.symbol),
                    });
                }
                self.pending_orders.remove(&client_id);
            }
            _ => {}
        }

        Ok(actions)
    }

    async fn on_tick(&mut self, _now: DateTime<Utc>) -> Result<Vec<StrategyAction>> {
        let mut actions = Vec::new();

        // Check for time-based exits
        let positions_to_exit: Vec<(String, Decimal)> = self.positions
            .iter()
            .filter_map(|(symbol, pos)| {
                if pos.time_remaining() < self.config.exit_before_resolution_secs as i64 {
                    Some((symbol.clone(), pos.highest_price))
                } else {
                    None
                }
            })
            .collect();

        for (symbol, price) in positions_to_exit {
            actions.extend(self.create_exit_order(&symbol, price, ExitReason::TimeExit));
        }

        Ok(actions)
    }

    fn state(&self) -> StrategyStateInfo {
        let exposure = self.positions
            .values()
            .map(|p| p.entry_price * Decimal::from(p.shares))
            .sum();

        let unrealized_pnl = self.positions
            .values()
            .map(|p| {
                let current = p.highest_price;
                (current - p.entry_price) * Decimal::from(p.shares)
            })
            .sum();

        let mut metrics = HashMap::new();
        metrics.insert("active_events".to_string(), self.active_events.len().to_string());
        metrics.insert("symbols".to_string(), self.config.symbols.join(","));

        StrategyStateInfo {
            strategy_id: self.config.id.clone(),
            phase: if self.positions.is_empty() { "waiting" } else { "in_position" }.to_string(),
            enabled: self.config.enabled,
            active: !self.positions.is_empty() || !self.pending_orders.is_empty(),
            position_count: self.positions.len(),
            pending_order_count: self.pending_orders.len(),
            total_exposure: exposure,
            unrealized_pnl,
            realized_pnl_today: self.realized_pnl,
            last_update: Utc::now(),
            metrics,
        }
    }

    fn positions(&self) -> Vec<PositionInfo> {
        self.positions.values().map(|p| {
            let mut info = PositionInfo::new(
                p.token_id.clone(),
                p.side,
                p.shares,
                p.entry_price,
                self.config.id.clone(),
            );
            info.current_price = Some(p.highest_price);
            info.unrealized_pnl = (p.highest_price - p.entry_price) * Decimal::from(p.shares);
            info.metadata.insert("symbol".to_string(), p.symbol.clone());
            info
        }).collect()
    }

    fn is_active(&self) -> bool {
        !self.positions.is_empty() || !self.pending_orders.is_empty()
    }

    async fn shutdown(&mut self) -> Result<Vec<StrategyAction>> {
        let mut actions = Vec::new();

        // Cancel all pending orders
        for (client_id, _) in &self.pending_orders {
            actions.push(StrategyAction::CancelOrder {
                order_id: client_id.clone(),
            });
        }

        // Collect position info first to avoid borrow conflict
        let positions_to_exit: Vec<(String, Decimal)> = self.positions
            .iter()
            .map(|(symbol, pos)| (symbol.clone(), pos.highest_price))
            .collect();

        // Exit all positions at market
        for (symbol, price) in positions_to_exit {
            actions.extend(self.create_exit_order(&symbol, price, ExitReason::Manual));
        }

        Ok(actions)
    }

    fn reset(&mut self) {
        self.positions.clear();
        self.pending_orders.clear();
        self.last_trade_time.clear();
        self.last_binance_prices.clear();
        self.price_history.clear();
        self.active_events.clear();
        self.realized_pnl = Decimal::ZERO;
        self.detector.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = MomentumConfig::default();
        assert_eq!(config.shares_per_trade, 100);
        assert_eq!(config.max_positions, 5);
    }

    #[test]
    fn test_series_mapping() {
        let mappings = SeriesMapping::standard_mappings();
        assert_eq!(mappings.len(), 3);

        let btc = mappings.iter().find(|m| m.symbol == "BTCUSDT").unwrap();
        assert!(btc.series_ids.contains(&"41".to_string()));
    }
}

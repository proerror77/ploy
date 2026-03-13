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

use crate::domain::{OrderStatus, OrderType, Quote, Side, TimeInForce};
use crate::error::Result;
use crate::domain::Domain;

use crate::strategy::detectors::{
    MomentumDetector, MomentumDetectorConfig, MomentumSignal, TrendDirection,
};
use crate::strategy::traits::{
    AlertLevel, DataFeed, MarketUpdate, OrderUpdate, PositionInfo, Strategy, StrategyAction,
    StrategyEvent, StrategyEventType, StrategyOrderIntent, StrategyStateInfo,
};

mod signal_flow;
mod lifecycle;
mod market_flow;

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
            min_move_pct: dec!(0.003),   // 0.3% minimum move (was 0.5%)
            max_entry_price: dec!(0.40), // Max 40¢ entry (was 55¢)
            min_edge: dec!(0.03),        // 3% minimum edge (was 5%)
            shares_per_trade: 100,
            // === ANTI-OVERTRADING CONTROLS ===
            max_positions: 3,     // Max 3 concurrent (was 5)
            cooldown_secs: 60,    // 60s between same symbol (was 30)
            max_daily_trades: 20, // Max 20 trades/day
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
        Ok(self.handle_market_update(update))
    }

    async fn on_order_update(&mut self, update: &OrderUpdate) -> Result<Vec<StrategyAction>> {
        Ok(self.handle_order_update(update))
    }

    async fn on_tick(&mut self, _now: DateTime<Utc>) -> Result<Vec<StrategyAction>> {
        Ok(self.handle_tick())
    }

    fn state(&self) -> StrategyStateInfo {
        self.build_state()
    }

    fn positions(&self) -> Vec<PositionInfo> {
        self.build_positions()
    }

    fn is_active(&self) -> bool {
        self.runtime_is_active()
    }

    async fn shutdown(&mut self) -> Result<Vec<StrategyAction>> {
        Ok(self.shutdown_actions())
    }

    fn reset(&mut self) {
        self.reset_runtime();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = MomentumConfig::default();
        assert_eq!(config.shares_per_trade, 100);
        assert_eq!(config.max_positions, 3);
    }

    #[test]
    fn test_series_mapping() {
        let mappings = SeriesMapping::standard_mappings();
        assert_eq!(mappings.len(), 3);

        let btc = mappings.iter().find(|m| m.symbol == "BTCUSDT").unwrap();
        assert!(btc.series_ids.contains(&"41".to_string()));
    }

    #[test]
    fn create_entry_order_emits_submit_intent() {
        let mut strategy = MomentumStrategy::new(MomentumConfig::default());
        strategy.active_events.insert(
            "event-1".to_string(),
            EventContext {
                event_id: "event-1".to_string(),
                symbol: "BTCUSDT".to_string(),
                up_token_id: "token-up".to_string(),
                down_token_id: "token-down".to_string(),
                end_time: Utc::now(),
            },
        );

        let actions = strategy.create_entry_order(EntrySignal {
            symbol: "BTCUSDT".to_string(),
            side: Side::Up,
            cex_move_pct: dec!(0.01),
            pm_price: dec!(0.40),
            edge: dec!(0.05),
            event_end_time: Utc::now(),
            token_id: "token-up".to_string(),
        });

        match actions.first() {
            Some(StrategyAction::SubmitIntent { intent }) => {
                assert_eq!(intent.domain, Domain::Crypto);
                assert_eq!(intent.market_slug, "event-1");
                assert_eq!(intent.token_id, "token-up");
                assert!(intent.is_buy);
                assert_eq!(intent.shares, 100);
            }
            other => panic!("expected submit intent, got {other:?}"),
        }
    }

    #[test]
    fn handle_market_update_registers_and_expires_events() {
        let mut strategy = MomentumStrategy::new(MomentumConfig::default());
        let end_time = Utc::now();

        let actions = strategy.handle_market_update(&MarketUpdate::EventDiscovered {
            event_id: "event-1".to_string(),
            series_id: "41".to_string(),
            up_token: "token-up".to_string(),
            down_token: "token-down".to_string(),
            end_time,
            price_to_beat: None,
            title: None,
            condition_id: None,
        });

        assert!(actions.is_empty());
        assert!(strategy.active_events.contains_key("event-1"));

        let actions = strategy.handle_market_update(&MarketUpdate::EventExpired {
            event_id: "event-1".to_string(),
        });

        assert!(actions.is_empty());
        assert!(!strategy.active_events.contains_key("event-1"));
    }
}

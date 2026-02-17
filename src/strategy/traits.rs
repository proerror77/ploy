//! Core strategy traits and types
//!
//! Defines the common interface that all trading strategies must implement.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::domain::{OrderRequest, OrderStatus, Quote, Side};
use crate::error::Result;

// ============================================================================
// Strategy Trait
// ============================================================================

/// Core trait that all trading strategies must implement
#[async_trait]
pub trait Strategy: Send + Sync {
    /// Unique strategy identifier
    fn id(&self) -> &str;

    /// Human-readable strategy name
    fn name(&self) -> &str;

    /// Strategy description
    fn description(&self) -> &str;

    /// Data feeds required by this strategy
    fn required_feeds(&self) -> Vec<DataFeed>;

    /// Called when market data updates (quotes, prices)
    async fn on_market_update(&mut self, update: &MarketUpdate) -> Result<Vec<StrategyAction>>;

    /// Called when order status changes
    async fn on_order_update(&mut self, update: &OrderUpdate) -> Result<Vec<StrategyAction>>;

    /// Called periodically (every tick_interval_ms)
    async fn on_tick(&mut self, now: DateTime<Utc>) -> Result<Vec<StrategyAction>>;

    /// Get current strategy state info
    fn state(&self) -> StrategyStateInfo;

    /// Get current positions held by this strategy
    fn positions(&self) -> Vec<PositionInfo>;

    /// Check if strategy is active (has open positions or pending orders)
    fn is_active(&self) -> bool;

    /// Graceful shutdown - close positions, cancel orders
    async fn shutdown(&mut self) -> Result<Vec<StrategyAction>>;

    /// Reset strategy state (for new trading session)
    fn reset(&mut self);
}

// ============================================================================
// Data Feeds
// ============================================================================

/// Types of data feeds a strategy can subscribe to
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DataFeed {
    /// Polymarket WebSocket quotes for specific tokens
    PolymarketQuotes { tokens: Vec<String> },

    /// Binance spot prices for specific symbols
    BinanceSpot { symbols: Vec<String> },

    /// Binance Kline (candlestick) updates (WebSocket).
    ///
    /// Intervals use Binance strings like "1m", "5m", "15m", "1h".
    BinanceKlines {
        symbols: Vec<String>,
        intervals: Vec<String>,
        /// If true, only emit closed bars.
        closed_only: bool,
    },

    /// Polymarket event metadata (for series monitoring)
    PolymarketEvents { series_ids: Vec<String> },

    /// Periodic tick at specified interval
    Tick { interval_ms: u64 },
}

// ============================================================================
// Market Updates
// ============================================================================

/// A single closed (or in-progress) kline bar.
#[derive(Debug, Clone)]
pub struct KlineBar {
    pub open_time: DateTime<Utc>,
    pub close_time: DateTime<Utc>,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Decimal,
    /// Whether this bar is final.
    pub is_closed: bool,
}

/// Market data update event
#[derive(Debug, Clone)]
pub enum MarketUpdate {
    /// Quote update from Polymarket
    PolymarketQuote {
        token_id: String,
        side: Side,
        quote: Quote,
        timestamp: DateTime<Utc>,
    },

    /// Price update from Binance
    BinancePrice {
        symbol: String,
        price: Decimal,
        timestamp: DateTime<Utc>,
    },

    /// Closed kline bar from Binance
    BinanceKline {
        symbol: String,
        interval: String,
        kline: KlineBar,
        timestamp: DateTime<Utc>,
    },

    /// New event discovered
    EventDiscovered {
        event_id: String,
        series_id: String,
        up_token: String,
        down_token: String,
        end_time: DateTime<Utc>,
        /// Parsed from event title/question when available (Objective B).
        price_to_beat: Option<Decimal>,
        /// Optional human title for logging/debugging.
        title: Option<String>,
    },

    /// Event expired/closed
    EventExpired { event_id: String },
}

impl MarketUpdate {
    pub fn timestamp(&self) -> DateTime<Utc> {
        match self {
            MarketUpdate::PolymarketQuote { timestamp, .. } => *timestamp,
            MarketUpdate::BinancePrice { timestamp, .. } => *timestamp,
            MarketUpdate::BinanceKline { timestamp, .. } => *timestamp,
            MarketUpdate::EventDiscovered { .. } => Utc::now(),
            MarketUpdate::EventExpired { .. } => Utc::now(),
        }
    }
}

// ============================================================================
// Order Updates
// ============================================================================

/// Order status update event
#[derive(Debug, Clone)]
pub struct OrderUpdate {
    /// Order ID
    pub order_id: String,
    /// Client order ID (strategy-assigned)
    pub client_order_id: Option<String>,
    /// New status
    pub status: OrderStatus,
    /// Filled quantity
    pub filled_qty: u64,
    /// Average fill price
    pub avg_fill_price: Option<Decimal>,
    /// Update timestamp
    pub timestamp: DateTime<Utc>,
    /// Error message if failed
    pub error: Option<String>,
}

// ============================================================================
// Strategy Actions
// ============================================================================

/// Actions a strategy can request
#[derive(Debug, Clone)]
pub enum StrategyAction {
    /// Submit a new order
    SubmitOrder {
        /// Strategy-assigned ID for tracking
        client_order_id: String,
        /// Order details
        order: OrderRequest,
        /// Priority (higher = more urgent)
        priority: u8,
    },

    /// Cancel an existing order
    CancelOrder { order_id: String },

    /// Modify an existing order
    ModifyOrder {
        order_id: String,
        new_price: Option<Decimal>,
        new_size: Option<u64>,
    },

    /// Update risk state
    UpdateRisk { level: RiskLevel, reason: String },

    /// Log a strategy event
    LogEvent { event: StrategyEvent },

    /// Send an alert
    Alert { level: AlertLevel, message: String },

    /// Request data feed subscription change
    SubscribeFeed { feed: DataFeed },

    /// Request data feed unsubscription
    UnsubscribeFeed { feed: DataFeed },
}

// ============================================================================
// Strategy State
// ============================================================================

/// Strategy state information for monitoring
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyStateInfo {
    /// Strategy ID
    pub strategy_id: String,
    /// Current phase/state name
    pub phase: String,
    /// Is strategy enabled
    pub enabled: bool,
    /// Is strategy active (has positions/orders)
    pub active: bool,
    /// Number of open positions
    pub position_count: usize,
    /// Number of pending orders
    pub pending_order_count: usize,
    /// Total exposure (USD value)
    pub total_exposure: Decimal,
    /// Unrealized P&L
    pub unrealized_pnl: Decimal,
    /// Today's realized P&L
    pub realized_pnl_today: Decimal,
    /// Last update timestamp
    pub last_update: DateTime<Utc>,
    /// Strategy-specific metrics
    pub metrics: HashMap<String, String>,
}

impl Default for StrategyStateInfo {
    fn default() -> Self {
        Self {
            strategy_id: String::new(),
            phase: "idle".to_string(),
            enabled: false,
            active: false,
            position_count: 0,
            pending_order_count: 0,
            total_exposure: Decimal::ZERO,
            unrealized_pnl: Decimal::ZERO,
            realized_pnl_today: Decimal::ZERO,
            last_update: Utc::now(),
            metrics: HashMap::new(),
        }
    }
}

// ============================================================================
// Position Info
// ============================================================================

/// Position information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionInfo {
    /// Token ID
    pub token_id: String,
    /// Position side
    pub side: Side,
    /// Number of shares
    pub shares: u64,
    /// Average entry price
    pub entry_price: Decimal,
    /// Current market price
    pub current_price: Option<Decimal>,
    /// Unrealized P&L
    pub unrealized_pnl: Decimal,
    /// When position was opened
    pub opened_at: DateTime<Utc>,
    /// Associated strategy
    pub strategy_id: String,
    /// Strategy-specific metadata
    pub metadata: HashMap<String, String>,
}

impl PositionInfo {
    pub fn new(
        token_id: String,
        side: Side,
        shares: u64,
        entry_price: Decimal,
        strategy_id: String,
    ) -> Self {
        Self {
            token_id,
            side,
            shares,
            entry_price,
            current_price: None,
            unrealized_pnl: Decimal::ZERO,
            opened_at: Utc::now(),
            strategy_id,
            metadata: HashMap::new(),
        }
    }

    pub fn update_price(&mut self, price: Decimal) {
        self.current_price = Some(price);
        self.unrealized_pnl = (price - self.entry_price) * Decimal::from(self.shares);
    }
}

// ============================================================================
// Risk & Alerts
// ============================================================================

/// Risk level for strategy risk updates
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RiskLevel {
    #[default]
    Normal,
    Elevated,
    Critical,
    Halted,
}

/// Alert severity level
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlertLevel {
    Info,
    Warning,
    Error,
    Critical,
}

// ============================================================================
// Strategy Events
// ============================================================================

/// Events that strategies can log
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyEvent {
    /// Event type
    pub event_type: StrategyEventType,
    /// Event message
    pub message: String,
    /// Associated data
    pub data: HashMap<String, String>,
    /// Timestamp
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StrategyEventType {
    /// Signal detected
    SignalDetected,
    /// Entry triggered
    EntryTriggered,
    /// Exit triggered
    ExitTriggered,
    /// Order filled
    OrderFilled,
    /// Cycle completed
    CycleCompleted,
    /// Risk triggered
    RiskTriggered,
    /// State changed
    StateChanged,
    /// Error occurred
    Error,
    /// Custom event
    Custom(String),
}

impl StrategyEvent {
    pub fn new(event_type: StrategyEventType, message: impl Into<String>) -> Self {
        Self {
            event_type,
            message: message.into(),
            data: HashMap::new(),
            timestamp: Utc::now(),
        }
    }

    pub fn with_data(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.data.insert(key.into(), value.into());
        self
    }
}

// ============================================================================
// Strategy Configuration
// ============================================================================

/// Common configuration for all strategies
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyConfig {
    /// Strategy ID
    pub id: String,
    /// Is strategy enabled
    pub enabled: bool,
    /// Maximum position size (shares)
    pub max_position_size: u64,
    /// Maximum total exposure (USD)
    pub max_exposure: Decimal,
    /// Dry run mode
    pub dry_run: bool,
    /// Strategy-specific parameters
    pub params: HashMap<String, String>,
}

impl Default for StrategyConfig {
    fn default() -> Self {
        Self {
            id: "default".to_string(),
            enabled: true,
            max_position_size: 100,
            max_exposure: Decimal::from(1000),
            dry_run: true,
            params: HashMap::new(),
        }
    }
}

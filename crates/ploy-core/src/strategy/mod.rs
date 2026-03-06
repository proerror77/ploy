//! Core strategy traits and types.
//!
//! These are lightweight, dependency-free strategy abstractions that can be
//! shared across the workspace.  The main app's `strategy::traits` module
//! builds on these with async methods and heavier types (OrderRequest, Quote).

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::domain::{OrderSide, OrderStatus, OrderType, Side, TimeInForce};
use crate::error::CoreResult;

// ============================================================================
// Risk & Alert levels
// ============================================================================

/// Risk level for strategy risk updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RiskLevel {
    #[default]
    Normal,
    Elevated,
    Critical,
    Halted,
}

/// Alert severity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlertLevel {
    Info,
    Warning,
    Error,
    Critical,
}

// ============================================================================
// Strategy events
// ============================================================================

/// Events that strategies can log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyEvent {
    pub event_type: StrategyEventType,
    pub message: String,
    pub data: HashMap<String, String>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StrategyEventType {
    SignalDetected,
    EntryTriggered,
    ExitTriggered,
    OrderFilled,
    CycleCompleted,
    RiskTriggered,
    StateChanged,
    Error,
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
// Data feeds
// ============================================================================

/// Types of data feeds a strategy can subscribe to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DataFeed {
    /// Polymarket WebSocket quotes for specific tokens.
    PolymarketQuotes { tokens: Vec<String> },
    /// Binance spot prices for specific symbols.
    BinanceSpot { symbols: Vec<String> },
    /// Binance Kline (candlestick) updates.
    BinanceKlines {
        symbols: Vec<String>,
        intervals: Vec<String>,
        closed_only: bool,
    },
    /// Polymarket event metadata (for series monitoring).
    PolymarketEvents { series_ids: Vec<String> },
    /// Periodic tick at specified interval.
    Tick { interval_ms: u64 },
}

// ============================================================================
// Kline bar
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
    pub is_closed: bool,
}

// ============================================================================
// Market update
// ============================================================================

/// Market data update event (core version — no Quote dependency).
#[derive(Debug, Clone)]
pub enum MarketUpdate {
    /// Quote update from Polymarket.
    PolymarketQuote {
        token_id: String,
        side: Side,
        best_bid: Option<Decimal>,
        best_ask: Option<Decimal>,
        timestamp: DateTime<Utc>,
    },
    /// Price update from Binance.
    BinancePrice {
        symbol: String,
        price: Decimal,
        timestamp: DateTime<Utc>,
    },
    /// Closed kline bar from Binance.
    BinanceKline {
        symbol: String,
        interval: String,
        kline: KlineBar,
        timestamp: DateTime<Utc>,
    },
    /// New event discovered.
    EventDiscovered {
        event_id: String,
        series_id: String,
        up_token: String,
        down_token: String,
        end_time: DateTime<Utc>,
        price_to_beat: Option<Decimal>,
        title: Option<String>,
        condition_id: Option<String>,
    },
    /// Event expired/closed.
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
// Order update
// ============================================================================

/// Order status update event.
#[derive(Debug, Clone)]
pub struct OrderUpdate {
    pub order_id: String,
    pub client_order_id: Option<String>,
    pub status: OrderStatus,
    pub filled_qty: u64,
    pub avg_fill_price: Option<Decimal>,
    pub timestamp: DateTime<Utc>,
    pub error: Option<String>,
}

// ============================================================================
// Core order request (lightweight, no uuid)
// ============================================================================

/// Lightweight order request using only core types.
///
/// The main app's `OrderRequest` adds uuid-based client_order_id generation
/// and additional fields.  This version is suitable for trait boundaries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreOrderRequest {
    pub client_order_id: String,
    pub token_id: String,
    pub market_side: Side,
    pub order_side: OrderSide,
    pub shares: u64,
    pub limit_price: Decimal,
    pub order_type: OrderType,
    pub time_in_force: TimeInForce,
}

// ============================================================================
// Strategy actions
// ============================================================================

/// Actions a strategy can request.
#[derive(Debug, Clone)]
pub enum StrategyAction {
    /// Submit a new order.
    SubmitOrder {
        client_order_id: String,
        order: CoreOrderRequest,
        priority: u8,
    },
    /// Cancel an existing order.
    CancelOrder { order_id: String },
    /// Modify an existing order.
    ModifyOrder {
        order_id: String,
        new_price: Option<Decimal>,
        new_size: Option<u64>,
    },
    /// Update risk state.
    UpdateRisk { level: RiskLevel, reason: String },
    /// Log a strategy event.
    LogEvent { event: StrategyEvent },
    /// Send an alert.
    Alert { level: AlertLevel, message: String },
    /// Request data feed subscription change.
    SubscribeFeed { feed: DataFeed },
    /// Request data feed unsubscription.
    UnsubscribeFeed { feed: DataFeed },
}

// ============================================================================
// Strategy state info
// ============================================================================

/// Strategy state information for monitoring.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyStateInfo {
    pub strategy_id: String,
    pub phase: String,
    pub enabled: bool,
    pub active: bool,
    pub position_count: usize,
    pub pending_order_count: usize,
    pub total_exposure: Decimal,
    pub unrealized_pnl: Decimal,
    pub realized_pnl_today: Decimal,
    pub last_update: DateTime<Utc>,
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
// Position info
// ============================================================================

/// Position information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionInfo {
    pub token_id: String,
    pub side: Side,
    pub shares: u64,
    pub entry_price: Decimal,
    pub current_price: Option<Decimal>,
    pub unrealized_pnl: Decimal,
    pub opened_at: DateTime<Utc>,
    pub strategy_id: String,
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
// Strategy configuration
// ============================================================================

/// Common configuration for all strategies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyConfig {
    pub id: String,
    pub enabled: bool,
    pub max_position_size: u64,
    pub max_exposure: Decimal,
    pub dry_run: bool,
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

// ============================================================================
// Core strategy trait (sync, no heavy deps)
// ============================================================================

/// Lightweight, synchronous strategy trait.
///
/// This is the core interface that can be used across the workspace without
/// pulling in async runtimes or heavy dependencies.  The main app's
/// `Strategy` trait (async) extends this conceptually.
///
/// Implementors that need async should implement the full `Strategy` trait
/// in the main app crate instead.
pub trait CoreStrategy: Send + Sync {
    /// Unique strategy identifier.
    fn id(&self) -> &str;

    /// Human-readable strategy name.
    fn name(&self) -> &str;

    /// Strategy description.
    fn description(&self) -> &str;

    /// Data feeds required by this strategy.
    fn required_feeds(&self) -> Vec<DataFeed>;

    /// Get current strategy state info.
    fn state(&self) -> StrategyStateInfo;

    /// Get current positions held by this strategy.
    fn positions(&self) -> Vec<PositionInfo>;

    /// Check if strategy is active (has open positions or pending orders).
    fn is_active(&self) -> bool;

    /// Reset strategy state (for new trading session).
    fn reset(&mut self);

    /// Process a market update synchronously.
    ///
    /// Returns actions the engine should execute.  Strategies that need
    /// async I/O should use the full `Strategy` trait instead.
    fn on_market_update_sync(&mut self, update: &MarketUpdate) -> CoreResult<Vec<StrategyAction>>;

    /// Process an order update synchronously.
    fn on_order_update_sync(&mut self, update: &OrderUpdate) -> CoreResult<Vec<StrategyAction>>;

    /// Periodic tick handler.
    fn on_tick_sync(&mut self, now: DateTime<Utc>) -> CoreResult<Vec<StrategyAction>>;
}

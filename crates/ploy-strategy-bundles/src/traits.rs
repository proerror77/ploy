//! Core traits for the unified strategy runtime.
//!
//! These traits define the pluggable components of [`StrategyRuntime`]:
//! - [`Feed`] — market data source (historical replay or live WebSocket)
//! - [`Executor`] — order execution (simulated or real exchange)
//! - [`StrategyLogic`] — signal evaluation and decision making
//! - [`Recorder`] — signal and trade persistence

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ploy_trading::{FillRecord, OrderLedger, PositionLedger, TradingIntent};
use rust_decimal::Decimal;

// ── Market Data ──────────────────────────────────────────

/// Unified market update consumed by all strategy modes.
///
/// A single enum covers spot prices, Polymarket quotes, L2 orderbook
/// snapshots, event lifecycle, and kline updates.
#[derive(Debug, Clone)]
pub enum MarketUpdate {
    /// CEX spot price tick (e.g. Binance BTC/USDT).
    SpotPrice {
        symbol: String,
        price: Decimal,
        ts: DateTime<Utc>,
    },

    /// Polymarket token quote update.
    Quote {
        token_id: String,
        bid: Option<Decimal>,
        ask: Option<Decimal>,
        ts: DateTime<Utc>,
    },

    /// CEX L2 orderbook summary.
    L2 {
        symbol: String,
        obi: f64,
        spread_bps: u32,
        ts: DateTime<Utc>,
    },

    /// New binary-option event window discovered.
    EventDiscovered {
        event_id: String,
        symbol: String,
        up_token: String,
        down_token: String,
        end_time: DateTime<Utc>,
        window_secs: u64,
        /// Price-to-beat from the market API (window open price).
        /// When present, used directly as S0 instead of waiting for spot.
        price_to_beat: Option<Decimal>,
    },

    /// Event window expired (settlement pending or complete).
    EventExpired {
        event_id: String,
        /// The event's end time, used for timeline sorting in historical feeds.
        end_time: DateTime<Utc>,
    },

    /// CEX kline (candlestick) close.
    Kline {
        symbol: String,
        interval: String,
        open: Decimal,
        close: Decimal,
        volume: Decimal,
        ts: DateTime<Utc>,
    },
}

/// Data feed source — historical replay or live WebSocket.
///
/// Returns `None` when the feed is exhausted (end of backtest data).
/// Live feeds block until the next update arrives.
#[async_trait]
pub trait Feed: Send {
    async fn next(&mut self) -> Option<MarketUpdate>;
}

// ── Execution ────────────────────────────────────────────

/// Result of submitting a trading intent to a venue or simulator.
#[derive(Debug, Clone)]
pub struct ExecutionReport {
    /// Assigned order identifier.
    pub order_id: String,
    /// Fill record if the order was (partially) filled.
    pub fill: Option<FillRecord>,
    /// Whether the order was rejected by the venue.
    pub rejected: bool,
    /// Reason for rejection, if any.
    pub rejection_reason: Option<String>,
    /// Simulated slippage (simulator only).
    pub slippage: Option<Decimal>,
    /// Simulated market impact (simulator only).
    pub market_impact: Option<Decimal>,
}

/// Order executor — real exchange or execution simulator.
#[async_trait]
pub trait Executor: Send {
    /// Submit a trading intent and return the execution report.
    async fn submit(&mut self, intent: &TradingIntent) -> ExecutionReport;

    /// Cancel an active order. Returns true if cancellation succeeded.
    async fn cancel(&mut self, order_id: &str) -> bool;
}

// ── Strategy Logic ───────────────────────────────────────

/// Decision produced by strategy evaluation.
#[derive(Debug, Clone)]
pub enum StrategyDecision {
    /// Open a new position.
    Enter(TradingIntent),
    /// Close or reduce an existing position.
    Exit(TradingIntent),
    /// No action.
    Hold,
}

/// Strategy signal evaluator — the pluggable "brain".
///
/// Implementors process market updates and produce trading decisions.
/// The runtime provides read access to positions and orders so the
/// strategy can check exposure, cooldowns, and duplicate prevention.
pub trait StrategyLogic: Send {
    /// Process a market update. May produce zero or more decisions.
    fn on_update(
        &mut self,
        update: &MarketUpdate,
        positions: &PositionLedger,
        orders: &OrderLedger,
    ) -> Vec<StrategyDecision>;

    /// Called after a fill is recorded so the strategy can update
    /// internal state (cooldowns, daily counters, etc.).
    fn on_fill(&mut self, fill: &FillRecord);

    /// Strategy name for logging and metrics.
    fn name(&self) -> &str;
}

// ── Recording ────────────────────────────────────────────

/// A signal evaluation record for persistence and analysis.
#[derive(Debug, Clone)]
pub struct SignalRecord {
    pub strategy: String,
    pub symbol: String,
    pub direction: String,
    pub p_hat: f64,
    pub edge: f64,
    pub entry_price: Decimal,
    /// "enter", "reject:insufficient_edge", "reject:cooldown", etc.
    pub decision: String,
    pub ts: DateTime<Utc>,
}

/// Signal and trade recorder for observability.
#[async_trait]
pub trait Recorder: Send {
    /// Record a signal evaluation (both entries and rejections).
    async fn record_signal(&mut self, signal: &SignalRecord);

    /// Flush buffered records to storage.
    async fn flush(&mut self);
}

/// No-op recorder for tests.
pub struct NullRecorder;

#[async_trait]
impl Recorder for NullRecorder {
    async fn record_signal(&mut self, _signal: &SignalRecord) {}
    async fn flush(&mut self) {}
}

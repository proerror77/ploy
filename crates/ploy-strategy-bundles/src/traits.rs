//! Core traits for the unified strategy runtime.
//!
//! These traits define the pluggable components of [`StrategyRuntime`]:
//! - [`Feed`] — market data source (historical replay or live WebSocket)
//! - [`Executor`] — order execution (simulated or real exchange)
//! - [`StrategyLogic`] — signal evaluation and decision making
//! - [`Recorder`] — signal and trade persistence

use async_trait::async_trait;
use chrono::{DateTime, Utc};
pub use ploy_market_contracts::{Feed, MarketUpdate};
use ploy_trading::{FillRecord, OrderLedger, PositionLedger, TradingIntent};
use rust_decimal::Decimal;

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
    /// `order_id` is the caller-assigned ID that must be used in the returned fill.
    async fn submit(&mut self, intent: &TradingIntent, order_id: &str) -> ExecutionReport;

    /// Cancel an active order. Returns true if cancellation succeeded.
    async fn cancel(&mut self, order_id: &str) -> bool;

    /// Reconcile fills for active venue orders. Default executors can ignore this.
    async fn reconcile_fills(&mut self, _orders: &OrderLedger) -> Result<Vec<FillRecord>, String> {
        Ok(Vec::new())
    }
}

// ── Strategy Logic ───────────────────────────────────────

/// Decision produced by strategy evaluation.
#[derive(Debug, Clone)]
pub enum StrategyDecision {
    /// Open a new position.
    Enter {
        intent: TradingIntent,
        signal: Option<SignalRecord>,
    },
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

    /// Called when an order is rejected by the venue so the strategy can
    /// arm cooldowns and avoid hammering the same signal repeatedly.
    ///
    /// `reason` is the raw rejection string from the venue.
    /// Default: no-op (strategies that don't need rejection handling can ignore it).
    fn on_reject(&mut self, _intent: &ploy_trading::TradingIntent, _reason: &str) {}

    /// Strategy name for logging and metrics.
    fn name(&self) -> &str;
}

/// Blanket impl so `Box<dyn StrategyLogic>` can be used as a generic `S: StrategyLogic`.
impl StrategyLogic for Box<dyn StrategyLogic> {
    fn on_update(
        &mut self,
        update: &MarketUpdate,
        positions: &PositionLedger,
        orders: &OrderLedger,
    ) -> Vec<StrategyDecision> {
        (**self).on_update(update, positions, orders)
    }

    fn on_fill(&mut self, fill: &FillRecord) {
        (**self).on_fill(fill);
    }

    fn on_reject(&mut self, intent: &ploy_trading::TradingIntent, reason: &str) {
        (**self).on_reject(intent, reason);
    }

    fn name(&self) -> &str {
        (**self).name()
    }
}

// ── Recording ────────────────────────────────────────────

/// A signal evaluation record for persistence and analysis.
#[derive(Debug, Clone)]
pub struct SignalRecord {
    pub strategy: String,
    pub event_id: Option<String>,
    pub token_id: Option<String>,
    pub intent_id: Option<String>,
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

    /// Record order submission or rejection for execution audit.
    async fn record_order(
        &mut self,
        _strategy: &str,
        _intent: &TradingIntent,
        _signal: Option<&SignalRecord>,
        _report: &ExecutionReport,
        _order_id: &str,
    ) {
    }

    /// Record a fill for execution audit.
    async fn record_fill(
        &mut self,
        _strategy: &str,
        _intent: &TradingIntent,
        _signal: Option<&SignalRecord>,
        _fill: &FillRecord,
        _report: &ExecutionReport,
    ) {
    }

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

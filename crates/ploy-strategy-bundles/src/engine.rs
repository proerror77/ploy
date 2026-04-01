//! Unified strategy runtime engine.
//!
//! [`StrategyRuntime`] is the core loop that connects a [`Feed`], a
//! [`StrategyLogic`], and an [`Executor`] through the canonical
//! [`ploy_trading::TradingRuntime`] lifecycle.
//!
//! The same `StrategyRuntime` drives backtest, dry-run, and live trading —
//! only the trait implementations differ.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use ploy_trading::{TradingRuntime, PnlSnapshot, RiskSnapshot};
use rust_decimal::Decimal;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::traits::{Executor, Feed, MarketUpdate, Recorder, StrategyDecision, StrategyLogic};

/// Operating mode for the runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeMode {
    /// Replay historical data with simulated execution.
    Backtest,
    /// Live market data with simulated execution.
    DryRun,
    /// Live market data with real exchange execution.
    Live,
}

/// Configuration for the runtime loop.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub mode: RuntimeMode,
    /// Maximum signal evaluations per second. `None` = unlimited.
    pub throttle_hz: Option<u32>,
    /// Stop after N updates (backtest bound). `None` = run forever.
    pub max_updates: Option<u64>,
}

/// Summary produced at the end of a runtime session.
#[derive(Debug, Clone)]
pub struct RuntimeResult {
    pub mode: RuntimeMode,
    pub updates_processed: u64,
    pub intents_submitted: u64,
    pub fills_recorded: u64,
    pub pnl: PnlSnapshot,
    pub risk: RiskSnapshot,
    pub elapsed_secs: f64,
}

/// Unified strategy runtime.
///
/// Generic over:
/// - `S`: strategy logic (signal evaluation)
/// - `F`: data feed (historical or live)
/// - `E`: order executor (simulated or real)
pub struct StrategyRuntime<S, F, E>
where
    S: StrategyLogic,
    F: Feed,
    E: Executor,
{
    strategy: S,
    feed: F,
    executor: E,
    recorder: Box<dyn Recorder>,
    trading: TradingRuntime,
    config: RuntimeConfig,
}

impl<S, F, E> StrategyRuntime<S, F, E>
where
    S: StrategyLogic,
    F: Feed,
    E: Executor,
{
    /// Create a new runtime with the given components.
    pub fn new(
        strategy: S,
        feed: F,
        executor: E,
        recorder: Box<dyn Recorder>,
        config: RuntimeConfig,
    ) -> Self {
        Self {
            strategy,
            feed,
            executor,
            recorder,
            trading: TradingRuntime::default(),
            config,
        }
    }

    /// Run the strategy loop until the feed is exhausted or max_updates reached.
    pub async fn run(&mut self) -> RuntimeResult {
        let start = std::time::Instant::now();
        let mut updates_processed: u64 = 0;
        let mut intents_submitted: u64 = 0;
        let mut fills_recorded: u64 = 0;
        let mut last_eval_ts: Option<DateTime<Utc>> = None;

        info!(
            mode = ?self.config.mode,
            strategy = self.strategy.name(),
            "StrategyRuntime started",
        );

        while let Some(update) = self.feed.next().await {
            updates_processed += 1;

            // Throttle: skip high-frequency price/quote updates if within the same time slot.
            // Event lifecycle updates (discovered/expired) always pass through.
            if let Some(hz) = self.config.throttle_hz {
                let is_lifecycle = matches!(
                    update,
                    MarketUpdate::EventDiscovered { .. } | MarketUpdate::EventExpired { .. }
                );
                if !is_lifecycle {
                    if let Some(ts) = Self::update_ts(&update) {
                        if let Some(last) = last_eval_ts {
                            let min_gap_ms = 1000 / hz as i64;
                            if (ts - last).num_milliseconds() < min_gap_ms {
                                continue;
                            }
                        }
                        last_eval_ts = Some(ts);
                    }
                }
            }

            // 1. Strategy evaluates the update.
            let decisions = self.strategy.on_update(
                &update,
                self.trading.positions(),
                self.trading.orders(),
            );

            // 2. Execute each decision.
            for decision in decisions {
                match decision {
                    StrategyDecision::Enter(intent) | StrategyDecision::Exit(intent) => {
                        let order_id = Uuid::new_v4().to_string();

                        let report = self.executor.submit(&intent).await;

                        if report.rejected && report.fill.is_none() {
                            // Pure rejection — don't record the intent at all
                            let reason = report.rejection_reason.as_deref().unwrap_or("unknown");
                            warn!(order_id = %order_id, reason = %reason, "Order rejected");
                            continue;
                        }

                        // Intent accepted (possibly with fill)
                        self.trading.submit_intent(intent.clone(), order_id.clone());
                        intents_submitted += 1;

                        if let Some(fill) = report.fill {
                            self.trading.record_fill(fill.clone());
                            self.strategy.on_fill(&fill);
                            fills_recorded += 1;
                            debug!(
                                order_id = %order_id,
                                token = %fill.token_id,
                                qty = %fill.quantity,
                                price = %fill.price,
                                "Fill recorded",
                            );
                        }
                    }
                    StrategyDecision::Hold => {}
                }
            }

            // 3. Check update limit (backtest bound).
            if let Some(max) = self.config.max_updates {
                if updates_processed >= max {
                    info!(updates = updates_processed, "Max updates reached, stopping");
                    break;
                }
            }
        }

        self.recorder.flush().await;

        let elapsed = start.elapsed().as_secs_f64();
        let mark_prices: BTreeMap<String, Decimal> = BTreeMap::new();
        let snapshot = self.trading.snapshot(&mark_prices);

        let result = RuntimeResult {
            mode: self.config.mode,
            updates_processed,
            intents_submitted,
            fills_recorded,
            pnl: snapshot.pnl,
            risk: snapshot.risk,
            elapsed_secs: elapsed,
        };

        info!(
            mode = ?result.mode,
            updates = result.updates_processed,
            intents = result.intents_submitted,
            fills = result.fills_recorded,
            elapsed = format!("{:.1}s", result.elapsed_secs),
            net_pnl = %result.pnl.net_pnl(),
            "StrategyRuntime finished",
        );

        result
    }

    /// Read-only access to the trading runtime state.
    pub fn trading(&self) -> &TradingRuntime {
        &self.trading
    }

    /// Extract timestamp from a market update (for throttling).
    fn update_ts(update: &MarketUpdate) -> Option<DateTime<Utc>> {
        match update {
            MarketUpdate::SpotPrice { ts, .. }
            | MarketUpdate::Quote { ts, .. }
            | MarketUpdate::L2 { ts, .. }
            | MarketUpdate::Kline { ts, .. } => Some(*ts),
            MarketUpdate::EventDiscovered { end_time, .. } => Some(*end_time),
            MarketUpdate::EventExpired { end_time, .. } => Some(*end_time),
        }
    }
}

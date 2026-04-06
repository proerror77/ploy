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
use ploy_trading::{PnlSnapshot, RiskSnapshot, TradingRuntime};
use rust_decimal::Decimal;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::traits::{Executor, Feed, MarketUpdate, Recorder, StrategyDecision, StrategyLogic};

/// Operating mode for the runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeMode {
    /// Replay historical data with simulated execution.
    Backtest,
    /// Replay a previously recorded canonical market-update log.
    Replay,
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
            let decisions =
                self.strategy
                    .on_update(&update, self.trading.positions(), self.trading.orders());

            // 2. Execute each decision.
            for decision in decisions {
                let (intent, signal) = match decision {
                    StrategyDecision::Enter { intent, signal } => (intent, signal),
                    StrategyDecision::Exit(intent) => (intent, None),
                    StrategyDecision::Hold => continue,
                };

                if let Some(signal) = signal.as_ref() {
                    self.recorder.record_signal(signal).await;
                }

                let order_id = Uuid::new_v4().to_string();

                let report = self.executor.submit(&intent, &order_id).await;

                if report.rejected && report.fill.is_none() {
                    // Pure rejection — keep the signal audit trail, but don't record an intent.
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
            | MarketUpdate::SportsState { ts, .. }
            | MarketUpdate::ReferencePrice { ts, .. }
            | MarketUpdate::Kline { ts, .. } => Some(*ts),
            MarketUpdate::EventDiscovered { end_time, .. } => Some(*end_time),
            MarketUpdate::EventExpired { end_time, .. } => Some(*end_time),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use chrono::Utc;
    use ploy_trading::{
        FillRecord, IntentPurpose, OrderLedger, PositionLedger, TradeSide, TradingIntent,
    };
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;

    use super::{RuntimeConfig, RuntimeMode, StrategyRuntime};
    use crate::traits::{
        ExecutionReport, Executor, Feed, MarketUpdate, Recorder, SignalRecord, StrategyDecision,
        StrategyLogic,
    };

    struct SingleUpdateFeed {
        next: Option<MarketUpdate>,
    }

    #[async_trait]
    impl Feed for SingleUpdateFeed {
        async fn next(&mut self) -> Option<MarketUpdate> {
            self.next.take()
        }
    }

    struct RejectingExecutor;

    #[async_trait]
    impl Executor for RejectingExecutor {
        async fn submit(&mut self, _intent: &TradingIntent, order_id: &str) -> ExecutionReport {
            ExecutionReport {
                order_id: order_id.to_string(),
                fill: None,
                rejected: true,
                rejection_reason: Some("simulated rejection".into()),
                slippage: None,
                market_impact: None,
            }
        }

        async fn cancel(&mut self, _order_id: &str) -> bool {
            true
        }
    }

    struct RecordingStrategy {
        emitted: bool,
        signal: SignalRecord,
        intent: TradingIntent,
    }

    impl StrategyLogic for RecordingStrategy {
        fn on_update(
            &mut self,
            _update: &MarketUpdate,
            _positions: &PositionLedger,
            _orders: &OrderLedger,
        ) -> Vec<StrategyDecision> {
            if self.emitted {
                return vec![];
            }
            self.emitted = true;
            vec![StrategyDecision::Enter {
                intent: self.intent.clone(),
                signal: Some(self.signal.clone()),
            }]
        }

        fn on_fill(&mut self, _fill: &FillRecord) {}

        fn name(&self) -> &str {
            "recording_strategy"
        }
    }

    struct CollectingRecorder {
        signals: Arc<Mutex<Vec<SignalRecord>>>,
    }

    #[async_trait]
    impl Recorder for CollectingRecorder {
        async fn record_signal(&mut self, signal: &SignalRecord) {
            self.signals.lock().unwrap().push(signal.clone());
        }

        async fn flush(&mut self) {}
    }

    #[tokio::test]
    async fn records_entry_signal_even_when_execution_is_rejected() {
        let now = Utc::now();
        let signal = SignalRecord {
            strategy: "pm_5m_directional".into(),
            event_id: Some("evt1".into()),
            token_id: Some("token-up".into()),
            intent_id: Some("pm5d_BTCUSDT_UP_test".into()),
            symbol: "BTCUSDT".into(),
            direction: "UP".into(),
            p_hat: 0.71,
            edge: 0.08,
            entry_price: dec!(0.30),
            decision: "enter".into(),
            ts: now,
        };
        let intent = TradingIntent {
            intent_id: "pm5d_BTCUSDT_UP_test".into(),
            deployment_id: String::new(),
            market_id: "evt1".into(),
            token_id: "token-up".into(),
            side: TradeSide::Buy,
            quantity: dec!(10),
            limit_price: Some(dec!(0.30)),
            purpose: IntentPurpose::Entry,
            created_at: now,
        };
        let recorder_store = Arc::new(Mutex::new(Vec::new()));
        let recorder = Box::new(CollectingRecorder {
            signals: recorder_store.clone(),
        });
        let strategy = RecordingStrategy {
            emitted: false,
            signal,
            intent,
        };
        let feed = SingleUpdateFeed {
            next: Some(MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100000),
                ts: now,
            }),
        };
        let executor = RejectingExecutor;
        let config = RuntimeConfig {
            mode: RuntimeMode::DryRun,
            throttle_hz: None,
            max_updates: None,
        };

        let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, config);
        let result = runtime.run().await;

        assert_eq!(result.intents_submitted, 0);
        let signals = recorder_store.lock().unwrap();
        assert_eq!(signals.len(), 1);
        assert_eq!(
            signals[0].intent_id.as_deref(),
            Some("pm5d_BTCUSDT_UP_test")
        );
        assert_eq!(signals[0].decision, "enter");
        assert_eq!(signals[0].entry_price, Decimal::new(30, 2));
    }
}

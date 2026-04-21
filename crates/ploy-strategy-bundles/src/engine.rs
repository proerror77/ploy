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

use ploy_trading::{FillRecord, IntentPurpose};
use rust_decimal_macros::dec;

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
    /// Skip settlement exit orders (live mode: Polymarket auto-settles on-chain).
    pub skip_settlement_exits: bool,
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

            // 1b. In live mode, filter out settlement exits (Polymarket auto-settles on-chain).
            let decisions: Vec<StrategyDecision> = if self.config.skip_settlement_exits {
                decisions
                    .into_iter()
                    .filter(|d| {
                        !matches!(d, StrategyDecision::Exit(intent)
                            if intent.purpose == IntentPurpose::Exit
                            && intent.limit_price.map_or(false, |p| p == dec!(0) || p == dec!(1)))
                    })
                    .collect()
            } else {
                decisions
            };

            // 2. Execute each decision.
            for decision in decisions {
                let (intent, signal) = match decision {
                    StrategyDecision::Enter { intent, signal } => (intent, signal),
                    StrategyDecision::Exit(intent) => (intent, None),
                    StrategyDecision::Hold => continue,
                };
                let strategy_name = self.strategy.name().to_string();
                let signal_ref = signal.as_ref();

                if let Some(signal) = signal_ref {
                    self.recorder.record_signal(signal).await;
                }

                let order_id = Uuid::new_v4().to_string();

                let report = self.executor.submit(&intent, &order_id).await;
                self.recorder
                    .record_order(&strategy_name, &intent, signal_ref, &report, &order_id)
                    .await;

                if report.rejected && report.fill.is_none() {
                    // Pure rejection — keep the signal audit trail, but don't record an intent.
                    // Notify the strategy so it can arm cooldowns and avoid hammering the same
                    // signal on every tick (e.g. balance exhausted, FAK no match).
                    let reason = report.rejection_reason.as_deref().unwrap_or("unknown");
                    warn!(order_id = %order_id, reason = %reason, "Order rejected");
                    self.strategy.on_reject(&intent, reason);
                    continue;
                }

                // Intent accepted (possibly with fill)
                self.trading.submit_intent(intent.clone(), order_id.clone());
                if !report.order_id.is_empty() && report.order_id != order_id {
                    self.trading
                        .acknowledge_order(&order_id, report.order_id.clone());
                }
                intents_submitted += 1;

                if let Some(fill) = report.fill.as_ref() {
                    self.trading.record_fill(fill.clone());
                    self.recorder
                        .record_fill(&strategy_name, &intent, signal_ref, fill, &report)
                        .await;
                    self.strategy.on_fill(&fill);
                    fills_recorded += 1;
                    debug!(
                        order_id = %order_id,
                        token = %fill.token_id,
                        qty = %fill.quantity,
                        price = %fill.price,
                        "Fill recorded",
                    );
                } else if !report.rejected && intent.purpose == IntentPurpose::Entry {
                    // Live mode: executor acknowledged an entry but no immediate fill.
                    // Arm cooldown/daily counter with a synthetic fill so the
                    // strategy doesn't re-signal for the same symbol immediately.
                    //
                    // Do not synthesize fills for exits: a live exit acknowledge
                    // without a real fill must not make the strategy believe the
                    // position is closed.
                    let synthetic = FillRecord {
                        fill_id: format!("synthetic_{order_id}"),
                        order_id: order_id.clone(),
                        token_id: intent.token_id.clone(),
                        side: intent.side.clone(),
                        quantity: intent.quantity,
                        price: intent.limit_price.unwrap_or_default(),
                        fee: Decimal::ZERO,
                        timestamp: intent.created_at,
                    };
                    self.strategy.on_fill(&synthetic);
                    debug!(
                        order_id = %order_id,
                        token = %intent.token_id,
                        "Synthetic fill for cooldown (live acknowledged)",
                    );
                }
            }

            match self.executor.reconcile_fills(self.trading.orders()).await {
                Ok(fills) => {
                    let strategy_name = self.strategy.name().to_string();
                    for fill in fills {
                        if !self.trading.record_fill(fill.clone()) {
                            continue;
                        }

                        let Some(order) = self.trading.order(&fill.order_id).cloned() else {
                            continue;
                        };
                        let Some(intent) = self.trading.intent(&order.intent_id).cloned() else {
                            continue;
                        };

                        let report = crate::traits::ExecutionReport {
                            order_id: order
                                .venue_order_id
                                .clone()
                                .unwrap_or_else(|| fill.order_id.clone()),
                            fill: Some(fill.clone()),
                            rejected: false,
                            rejection_reason: None,
                            slippage: None,
                            market_impact: None,
                        };

                        self.recorder
                            .record_fill(&strategy_name, &intent, None, &fill, &report)
                            .await;
                        self.strategy.on_fill(&fill);
                        fills_recorded += 1;
                        debug!(
                            order_id = %fill.order_id,
                            token = %fill.token_id,
                            qty = %fill.quantity,
                            price = %fill.price,
                            "Reconciled fill recorded",
                        );
                    }
                }
                Err(error) => {
                    warn!(error = %error, "Fill reconciliation failed");
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
            | MarketUpdate::AggTrade { ts, .. }
            | MarketUpdate::Quote { ts, .. }
            | MarketUpdate::L2 { ts, .. }
            | MarketUpdate::L2Depth { ts, .. }
            | MarketUpdate::SportsState { ts, .. }
            | MarketUpdate::SportsPregame { ts, .. }
            | MarketUpdate::SportsLive { ts, .. }
            | MarketUpdate::ReferencePrice { ts, .. }
            | MarketUpdate::Kline { ts, .. } => Some(*ts),
            MarketUpdate::EventDiscovered { end_time, .. } => Some(*end_time),
            MarketUpdate::EventExpired { end_time, .. } => Some(*end_time),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
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

    struct NoopStrategy;

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

    impl StrategyLogic for NoopStrategy {
        fn on_update(
            &mut self,
            _update: &MarketUpdate,
            _positions: &PositionLedger,
            _orders: &OrderLedger,
        ) -> Vec<StrategyDecision> {
            vec![]
        }

        fn on_fill(&mut self, _fill: &FillRecord) {}

        fn name(&self) -> &str {
            "noop_strategy"
        }
    }

    struct FillCountingStrategy {
        emitted: bool,
        intent: TradingIntent,
        fill_count: Arc<Mutex<u32>>,
    }

    impl StrategyLogic for FillCountingStrategy {
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
            vec![StrategyDecision::Exit(self.intent.clone())]
        }

        fn on_fill(&mut self, _fill: &FillRecord) {
            *self.fill_count.lock().unwrap() += 1;
        }

        fn name(&self) -> &str {
            "fill_counting_strategy"
        }
    }

    struct CollectingRecorder {
        signals: Arc<Mutex<Vec<SignalRecord>>>,
        orders: Arc<Mutex<Vec<(String, String)>>>,
        fills: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Recorder for CollectingRecorder {
        async fn record_signal(&mut self, signal: &SignalRecord) {
            self.signals.lock().unwrap().push(signal.clone());
        }

        async fn record_order(
            &mut self,
            strategy: &str,
            intent: &TradingIntent,
            _signal: Option<&SignalRecord>,
            _report: &ExecutionReport,
            _order_id: &str,
        ) {
            self.orders
                .lock()
                .unwrap()
                .push((strategy.to_string(), intent.intent_id.clone()));
        }

        async fn record_fill(
            &mut self,
            _strategy: &str,
            _intent: &TradingIntent,
            _signal: Option<&SignalRecord>,
            fill: &FillRecord,
            _report: &ExecutionReport,
        ) {
            self.fills.lock().unwrap().push(fill.fill_id.clone());
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
        let order_store = Arc::new(Mutex::new(Vec::new()));
        let fill_store = Arc::new(Mutex::new(Vec::new()));
        let recorder = Box::new(CollectingRecorder {
            signals: recorder_store.clone(),
            orders: order_store.clone(),
            fills: fill_store.clone(),
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
            skip_settlement_exits: false,
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
        let orders = order_store.lock().unwrap();
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].0, "recording_strategy");
        assert_eq!(orders[0].1, "pm5d_BTCUSDT_UP_test");
        assert!(fill_store.lock().unwrap().is_empty());
    }

    struct FillingExecutor;

    #[async_trait]
    impl Executor for FillingExecutor {
        async fn submit(&mut self, intent: &TradingIntent, order_id: &str) -> ExecutionReport {
            ExecutionReport {
                order_id: order_id.to_string(),
                fill: Some(FillRecord {
                    fill_id: "fill-1".into(),
                    order_id: order_id.to_string(),
                    token_id: intent.token_id.clone(),
                    side: intent.side,
                    quantity: intent.quantity,
                    price: intent.limit_price.expect("limit price"),
                    fee: dec!(0.01),
                    timestamp: intent.created_at,
                }),
                rejected: false,
                rejection_reason: None,
                slippage: Some(Decimal::ZERO),
                market_impact: Some(Decimal::ZERO),
            }
        }

        async fn cancel(&mut self, _order_id: &str) -> bool {
            true
        }
    }

    struct AcknowledgingExecutor;

    #[async_trait]
    impl Executor for AcknowledgingExecutor {
        async fn submit(&mut self, _intent: &TradingIntent, _order_id: &str) -> ExecutionReport {
            ExecutionReport {
                order_id: "venue-order-1".into(),
                fill: None,
                rejected: false,
                rejection_reason: None,
                slippage: None,
                market_impact: None,
            }
        }

        async fn cancel(&mut self, _order_id: &str) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn records_order_and_fill_audit_for_successful_execution() {
        let now = Utc::now();
        let signal = SignalRecord {
            strategy: "pm_5m_directional".into(),
            event_id: Some("evt1".into()),
            token_id: Some("token-up".into()),
            intent_id: Some("pm5d_BTCUSDT_UP_fill".into()),
            symbol: "BTCUSDT".into(),
            direction: "UP".into(),
            p_hat: 0.71,
            edge: 0.08,
            entry_price: dec!(0.30),
            decision: "enter".into(),
            ts: now,
        };
        let intent = TradingIntent {
            intent_id: "pm5d_BTCUSDT_UP_fill".into(),
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
        let order_store = Arc::new(Mutex::new(Vec::new()));
        let fill_store = Arc::new(Mutex::new(Vec::new()));
        let recorder = Box::new(CollectingRecorder {
            signals: recorder_store.clone(),
            orders: order_store.clone(),
            fills: fill_store.clone(),
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
        let executor = FillingExecutor;
        let config = RuntimeConfig {
            mode: RuntimeMode::DryRun,
            throttle_hz: None,
            max_updates: None,
            skip_settlement_exits: false,
        };

        let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, config);
        let result = runtime.run().await;

        assert_eq!(result.intents_submitted, 1);
        assert_eq!(result.fills_recorded, 1);
        assert_eq!(order_store.lock().unwrap().len(), 1);
        assert_eq!(fill_store.lock().unwrap().as_slice(), ["fill-1"]);
    }

    #[tokio::test]
    async fn acknowledged_orders_update_runtime_order_state_with_venue_id() {
        let now = Utc::now();
        let signal = SignalRecord {
            strategy: "pm_5m_directional".into(),
            event_id: Some("evt1".into()),
            token_id: Some("token-up".into()),
            intent_id: Some("pm5d_BTCUSDT_UP_ack".into()),
            symbol: "BTCUSDT".into(),
            direction: "UP".into(),
            p_hat: 0.71,
            edge: 0.08,
            entry_price: dec!(0.30),
            decision: "enter".into(),
            ts: now,
        };
        let intent = TradingIntent {
            intent_id: "pm5d_BTCUSDT_UP_ack".into(),
            deployment_id: String::new(),
            market_id: "evt1".into(),
            token_id: "token-up".into(),
            side: TradeSide::Buy,
            quantity: dec!(10),
            limit_price: Some(dec!(0.30)),
            purpose: IntentPurpose::Entry,
            created_at: now,
        };
        let recorder = Box::new(CollectingRecorder {
            signals: Arc::new(Mutex::new(Vec::new())),
            orders: Arc::new(Mutex::new(Vec::new())),
            fills: Arc::new(Mutex::new(Vec::new())),
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
        let executor = AcknowledgingExecutor;
        let config = RuntimeConfig {
            mode: RuntimeMode::Live,
            throttle_hz: None,
            max_updates: None,
            skip_settlement_exits: true,
        };

        let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, config);
        let _ = runtime.run().await;
        let snapshot = runtime.trading().snapshot(&BTreeMap::new());

        assert_eq!(snapshot.orders.len(), 1);
        assert_eq!(
            snapshot.orders[0].state,
            ploy_trading::OrderState::Acknowledged
        );
        assert_eq!(
            snapshot.orders[0].venue_order_id.as_deref(),
            Some("venue-order-1")
        );
    }

    #[tokio::test]
    async fn live_exit_ack_without_fill_does_not_trigger_synthetic_fill() {
        let now = Utc::now();
        let fill_count = Arc::new(Mutex::new(0));
        let intent = TradingIntent {
            intent_id: "pm5d_BTCUSDT_UP_exit_ack".into(),
            deployment_id: String::new(),
            market_id: "evt1".into(),
            token_id: "token-up".into(),
            side: TradeSide::Sell,
            quantity: dec!(10),
            limit_price: Some(dec!(0.42)),
            purpose: IntentPurpose::Exit,
            created_at: now,
        };
        let strategy = FillCountingStrategy {
            emitted: false,
            intent,
            fill_count: fill_count.clone(),
        };
        let feed = SingleUpdateFeed {
            next: Some(MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100000),
                ts: now,
            }),
        };
        let recorder = Box::new(CollectingRecorder {
            signals: Arc::new(Mutex::new(Vec::new())),
            orders: Arc::new(Mutex::new(Vec::new())),
            fills: Arc::new(Mutex::new(Vec::new())),
        });
        let executor = AcknowledgingExecutor;
        let config = RuntimeConfig {
            mode: RuntimeMode::Live,
            throttle_hz: None,
            max_updates: None,
            skip_settlement_exits: true,
        };

        let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, config);
        let _ = runtime.run().await;

        assert_eq!(*fill_count.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn agg_trade_updates_are_accepted_by_runtime() {
        let now = Utc::now();
        let feed = SingleUpdateFeed {
            next: Some(MarketUpdate::AggTrade {
                symbol: "BTCUSDT".into(),
                agg_trade_id: 42,
                price: dec!(100000),
                quantity: dec!(0.25),
                is_buyer_maker: false,
                ts: now,
            }),
        };
        let recorder = Box::new(CollectingRecorder {
            signals: Arc::new(Mutex::new(Vec::new())),
            orders: Arc::new(Mutex::new(Vec::new())),
            fills: Arc::new(Mutex::new(Vec::new())),
        });
        let executor = RejectingExecutor;
        let config = RuntimeConfig {
            mode: RuntimeMode::DryRun,
            throttle_hz: None,
            max_updates: None,
            skip_settlement_exits: true,
        };

        let mut runtime = StrategyRuntime::new(NoopStrategy, feed, executor, recorder, config);
        let result = runtime.run().await;

        assert_eq!(result.updates_processed, 1);
        assert_eq!(result.intents_submitted, 0);
        assert_eq!(result.fills_recorded, 0);
    }
}

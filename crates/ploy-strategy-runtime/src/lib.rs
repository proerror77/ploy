use async_trait::async_trait;
use ploy_claimer::ensure_account_claimer_daemon;
use ploy_market_data::feeds::{
    spawn_chainlink_feed, spawn_db_aggtrade_feed, spawn_db_l2_feed,
    spawn_db_spot_feed, spawn_pyth_reference_feed, spawn_spot_feed,
};
use ploy_market_data::reference_prices::new_reference_price_registry;
use ploy_market_data::scanner::spawn_market_scanner;
use ploy_market_data::sports_feed::spawn_sports_feed;
use ploy_strategy_bundles::feed::{load_from_database_with_options, HistoricalLoadOptions};
use ploy_strategy_bundles::{
    BayesianDirectionalStrategy, ExecutionReport, Feed, FullConfig, HistoricalFeed, LiveFeed,
    MeanReversionStrategy, NullRecorder, RecordedFeed, Recorder, RecordingFeed, RuntimeMode,
    SignalRecord, SimulatedExecutor, StrategyLogic, StrategyRuntime,
};
use ploy_trading::{FillRecord, TradeSide, TradingIntent};
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use std::collections::{BTreeMap, HashMap};
use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use tracing::{error, info, warn};

pub use ploy_strategy_bundles::RuntimeMode as StrategyRuntimeMode;

pub async fn run_strategy(config: FullConfig, config_path: &str, force_dry_run: bool) {
    let mut runtime_config = config.runtime_config();
    if force_dry_run {
        runtime_config.mode = RuntimeMode::DryRun;
    }

    info!(
        mode = ?runtime_config.mode,
        config = %config_path,
        symbols = ?config.strategy.symbols,
        "ploy-runner starting",
    );

    let symbols = prepare_feed_symbols(runtime_config.mode, &config.strategy.symbols);
    let strategy = build_strategy(&config);

    let (result, snapshot) = match runtime_config.mode {
        RuntimeMode::Backtest => run_backtest(&config, &symbols, strategy, runtime_config.clone()).await,
        RuntimeMode::Replay => run_replay(&config, strategy, runtime_config.clone()).await,
        RuntimeMode::Live | RuntimeMode::DryRun => {
            run_live_or_dry_run(&config, &symbols, strategy, runtime_config.clone()).await
        }
    };

    info!(
        updates = result.updates_processed,
        intents = result.intents_submitted,
        fills = result.fills_recorded,
        net_pnl = %result.pnl.net_pnl(),
        elapsed = format!("{:.1}s", result.elapsed_secs),
        "ploy-runner finished",
    );

    if matches!(
        runtime_config.mode,
        RuntimeMode::Backtest | RuntimeMode::Replay
    ) {
        let cashflow = snapshot.fill_cashflow_summary();
        let roi_on_deployed_capital = cashflow
            .roi_on_deployed_capital()
            .map(|roi| format!("{}%", (roi * Decimal::from(100)).round_dp(2)))
            .unwrap_or_else(|| "n/a".to_string());

        info!(
            buy_shares = %cashflow.buy_shares,
            sell_shares = %cashflow.sell_shares,
            deployed_capital = %cashflow.deployed_capital(),
            gross_sell_proceeds = %cashflow.gross_sell_proceeds,
            fees = %cashflow.total_fees,
            roi_on_deployed_capital = %roi_on_deployed_capital,
            "Replay/backtest cashflow summary",
        );
    }
}

async fn run_backtest(
    config: &FullConfig,
    symbols: &[String],
    strategy: Box<dyn StrategyLogic>,
    runtime_config: RuntimeModeConfig,
) -> (ploy_strategy_bundles::RuntimeResult, ploy_trading::TradingRuntimeSnapshot) {
    let db_url = env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgresql://postgres:postgres@localhost:5432/ploy".to_string());

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("Failed to connect to database");

    let (from, to) = config.backtest_time_range().unwrap_or_else(|| {
        let from = chrono::DateTime::parse_from_rfc3339("2026-04-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let to = chrono::DateTime::parse_from_rfc3339("2026-04-01T13:30:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        (from, to)
    });

    info!(
        from = %from,
        to = %to,
        symbols = ?symbols,
        "Loading historical data from database",
    );

    let backtest_options = HistoricalLoadOptions {
        include_reference_prices: config.backtest_data.include_reference_prices,
        reference_symbols: config
            .backtest_data
            .reference_symbols(&config.reference_data),
        include_sports_state: config.backtest_data.include_sports_state,
        require_official_settlement: config.backtest_data.require_official_settlement,
    };

    let updates = load_from_database_with_options(&pool, symbols, from, to, &backtest_options)
        .await
        .expect("Failed to load historical data");

    info!(updates = updates.len(), "Historical data loaded");

    let feed = HistoricalFeed::new(updates);
    let executor = SimulatedExecutor::new(config.sim_executor_config());
    let recorder: Box<dyn Recorder> = Box::new(NullRecorder);
    let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, runtime_config);
    let result = runtime.run().await;
    let snapshot = runtime.trading().snapshot(&BTreeMap::new());
    (result, snapshot)
}

async fn run_replay(
    config: &FullConfig,
    strategy: Box<dyn StrategyLogic>,
    runtime_config: RuntimeModeConfig,
) -> (ploy_strategy_bundles::RuntimeResult, ploy_trading::TradingRuntimeSnapshot) {
    let replay_path = config.replay_market_updates_path().unwrap_or_else(|| {
        eprintln!("Replay mode requires [runtime].replay_market_updates_from in the config");
        std::process::exit(1);
    });

    info!(
        path = %replay_path.display(),
        "Loading recorded market-update log for replay",
    );

    let feed = RecordedFeed::from_path(replay_path).unwrap_or_else(|error| {
        eprintln!(
            "Failed to load replay market updates from {}: {error}",
            replay_path.display()
        );
        std::process::exit(1);
    });
    let executor = SimulatedExecutor::new(config.sim_executor_config());
    let recorder: Box<dyn Recorder> = Box::new(NullRecorder);
    let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, runtime_config);
    let result = runtime.run().await;
    let snapshot = runtime.trading().snapshot(&BTreeMap::new());
    (result, snapshot)
}

async fn run_live_or_dry_run(
    config: &FullConfig,
    symbols: &[String],
    strategy: Box<dyn StrategyLogic>,
    runtime_config: RuntimeModeConfig,
) -> (ploy_strategy_bundles::RuntimeResult, ploy_trading::TradingRuntimeSnapshot) {
    let db_url = env::var("DATABASE_URL").ok();
    let db_pool: Option<sqlx::PgPool> = match db_url.as_deref() {
        Some(url) => match PgPoolOptions::new().max_connections(5).connect(url).await {
            Ok(pool) => {
                info!("DB connected — market metadata and quotes will be persisted");
                Some(pool)
            }
            Err(error) => {
                if database_unavailable_is_fatal(runtime_config.mode, true) {
                    error!(
                        error = %error,
                        "DB connection failed for configured runtime; refusing to start without persistence"
                    );
                    std::process::exit(1);
                }

                warn!(error = %error, "DB connection failed; running without persistence");
                None
            }
        },
        None => {
            info!("DATABASE_URL not set — running without DB persistence");
            None
        }
    };

    let (tx, rx) = broadcast::channel(8192);
    let tx = Arc::new(tx);
    let reference_prices = new_reference_price_registry();

    let spot_handle = spawn_spot_feed(
        tx.clone(),
        reference_prices.clone(),
        symbols.to_vec(),
        db_pool.clone(),
    );
    let _db_spot_handle = if let Some(ref db) = db_pool {
        Some(spawn_db_spot_feed(tx.clone(), symbols.to_vec(), db.clone()))
    } else {
        None
    };
    let _db_aggtrade_handle = if let Some(ref db) = db_pool {
        Some(spawn_db_aggtrade_feed(tx.clone(), symbols.to_vec(), db.clone()))
    } else {
        None
    };
    let _db_l2_handle = if let Some(ref db) = db_pool {
        Some(spawn_db_l2_feed(tx.clone(), symbols.to_vec(), db.clone()))
    } else {
        None
    };
    let chainlink_handle = spawn_chainlink_feed(
        tx.clone(),
        reference_prices.clone(),
        symbols.to_vec(),
        db_pool.clone(),
    );
    let pyth_handle = spawn_pyth_reference_feed(
        tx.clone(),
        reference_prices.clone(),
        config.reference_data.pyth_symbols.clone(),
        db_pool.clone(),
    );
    let scanner_handle = spawn_market_scanner(
        tx.clone(),
        reference_prices.clone(),
        symbols.to_vec(),
        db_pool.clone(),
    );

    let sports_handle = if config.reference_data.capture_sports_state {
        Some(spawn_sports_feed(tx.clone(), db_pool.clone()))
    } else {
        None
    };

    let feed: Box<dyn Feed> = if let Some(record_path) = config.record_market_updates_path() {
        Box::new(
            RecordingFeed::new(LiveFeed::new(rx), record_path).unwrap_or_else(|error| {
                eprintln!(
                    "Failed to open market-update log {}: {error}",
                    record_path.display()
                );
                std::process::exit(1);
            }),
        )
    } else {
        Box::new(LiveFeed::new(rx))
    };

    let recorder = build_signal_recorder(db_pool.clone(), runtime_config.mode);
    let result = if runtime_config.mode == RuntimeMode::Live {
        if let Err(error) = ensure_account_claimer_daemon().await {
            warn!("Auto-claimer daemon failed to start: {error}");
        }
        let executor = build_live_executor();
        let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, runtime_config);
        let result = runtime.run().await;
        let snapshot = runtime.trading().snapshot(&BTreeMap::new());
        (result, snapshot)
    } else {
        let executor = SimulatedExecutor::new(config.sim_executor_config());
        let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, runtime_config);
        let result = runtime.run().await;
        let snapshot = runtime.trading().snapshot(&BTreeMap::new());
        (result, snapshot)
    };

    spot_handle.abort();
    chainlink_handle.abort();
    pyth_handle.abort();
    scanner_handle.abort();
    if let Some(handle) = sports_handle {
        handle.abort();
    }

    result
}

type RuntimeModeConfig = ploy_strategy_bundles::RuntimeConfig;

#[derive(Clone, Default)]
struct TokenExecutionContext {
    event_id: Option<String>,
    symbol: Option<String>,
    market_side: Option<String>,
}

struct RuntimeDbRecorder {
    pool: sqlx::PgPool,
    mode_label: String,
    token_context: HashMap<String, TokenExecutionContext>,
}

impl RuntimeDbRecorder {
    fn new(pool: sqlx::PgPool, mode_label: String) -> Self {
        Self {
            pool,
            mode_label,
            token_context: HashMap::new(),
        }
    }

    fn merge_context(
        &self,
        intent: &TradingIntent,
        signal: Option<&SignalRecord>,
    ) -> TokenExecutionContext {
        let mut context = self
            .token_context
            .get(&intent.token_id)
            .cloned()
            .unwrap_or_default();

        if context.event_id.is_none() && !intent.market_id.is_empty() {
            context.event_id = Some(intent.market_id.clone());
        }

        if let Some(signal) = signal {
            if context.event_id.is_none() {
                context.event_id = signal.event_id.clone();
            }
            if context.symbol.is_none() {
                context.symbol = Some(signal.symbol.clone());
            }
            if context.market_side.is_none() {
                context.market_side = Some(signal.direction.clone());
            }
        }

        context
    }

    fn remember_context(&mut self, token_id: &str, context: &TokenExecutionContext) {
        if context.event_id.is_none() && context.symbol.is_none() && context.market_side.is_none() {
            return;
        }
        self.token_context
            .insert(token_id.to_string(), context.clone());
    }

    fn remember_signal_context(&mut self, signal: &SignalRecord) {
        let Some(token_id) = signal.token_id.as_deref() else {
            return;
        };

        self.token_context.insert(
            token_id.to_string(),
            TokenExecutionContext {
                event_id: signal.event_id.clone(),
                symbol: Some(signal.symbol.clone()),
                market_side: Some(signal.direction.clone()),
            },
        );
    }

    async fn persist_signal(&self, signal: &SignalRecord) {
        let confidence = Decimal::from_f64(signal.p_hat);
        let edge = Decimal::from_f64(signal.edge);
        let context = json!({
            "runtime_mode": self.mode_label,
            "event_id": signal.event_id,
            "intent_id": signal.intent_id,
        });

        if let Err(error) = sqlx::query(
            r#"
            INSERT INTO signal_history (
                recorded_at,
                intent_id,
                agent_id,
                strategy_id,
                domain,
                signal_type,
                token_id,
                symbol,
                side,
                confidence,
                market_price,
                edge,
                context
            )
            VALUES (
                $1,
                NULL,
                'ploy-runner',
                $2,
                'polymarket',
                $3,
                $4,
                $5,
                $6,
                $7,
                $8,
                $9,
                $10
            )
            "#,
        )
        .bind(signal.ts)
        .bind(signal.strategy.as_str())
        .bind(signal.decision.as_str())
        .bind(signal.token_id.as_deref())
        .bind(signal.symbol.as_str())
        .bind(signal.direction.as_str())
        .bind(confidence)
        .bind(signal.entry_price)
        .bind(edge)
        .bind(context)
        .execute(&self.pool)
        .await
        {
            warn!(error = %error, "Failed to persist signal record");
        }
    }

    async fn persist_order(
        &self,
        strategy: &str,
        intent: &TradingIntent,
        context: &TokenExecutionContext,
        signal: Option<&SignalRecord>,
        report: &ExecutionReport,
        order_id: &str,
    ) {
        let fill = report.fill.as_ref();
        let status = if report.rejected {
            "REJECTED"
        } else if let Some(fill) = fill {
            if fill.quantity >= intent.quantity {
                "FILLED"
            } else {
                "PARTIALLY_FILLED"
            }
        } else {
            "ACKNOWLEDGED"
        };
        let exchange_order_id = if report.order_id.is_empty() || report.order_id == order_id {
            None
        } else {
            Some(report.order_id.as_str())
        };
        let filled_quantity = fill.map(|record| record.quantity).unwrap_or(Decimal::ZERO);
        let avg_fill_price = fill.map(|record| record.price);
        let context_json = json!({
            "runtime_mode": self.mode_label,
            "signal_decision": signal.map(|record| record.decision.as_str()),
            "slippage": report.slippage.map(|value| value.to_string()),
            "market_impact": report.market_impact.map(|value| value.to_string()),
        });

        if let Err(error) = sqlx::query(
            r#"
            INSERT INTO strategy_runtime_orders (
                recorded_at,
                runtime_mode,
                strategy_id,
                deployment_id,
                intent_id,
                order_id,
                venue_order_id,
                event_id,
                symbol,
                token_id,
                market_side,
                order_side,
                quantity,
                limit_price,
                filled_quantity,
                avg_fill_price,
                status,
                rejection_reason,
                slippage,
                market_impact,
                context
            )
            VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21
            )
            ON CONFLICT (order_id) DO UPDATE
            SET venue_order_id = COALESCE(EXCLUDED.venue_order_id, strategy_runtime_orders.venue_order_id),
                event_id = COALESCE(EXCLUDED.event_id, strategy_runtime_orders.event_id),
                symbol = COALESCE(EXCLUDED.symbol, strategy_runtime_orders.symbol),
                market_side = COALESCE(EXCLUDED.market_side, strategy_runtime_orders.market_side),
                filled_quantity = EXCLUDED.filled_quantity,
                avg_fill_price = COALESCE(EXCLUDED.avg_fill_price, strategy_runtime_orders.avg_fill_price),
                status = EXCLUDED.status,
                rejection_reason = COALESCE(EXCLUDED.rejection_reason, strategy_runtime_orders.rejection_reason),
                slippage = COALESCE(EXCLUDED.slippage, strategy_runtime_orders.slippage),
                market_impact = COALESCE(EXCLUDED.market_impact, strategy_runtime_orders.market_impact),
                context = EXCLUDED.context
            "#,
        )
        .bind(intent.created_at)
        .bind(self.mode_label.as_str())
        .bind(strategy)
        .bind(intent.deployment_id.as_str())
        .bind(intent.intent_id.as_str())
        .bind(order_id)
        .bind(exchange_order_id)
        .bind(context.event_id.as_deref())
        .bind(context.symbol.as_deref())
        .bind(intent.token_id.as_str())
        .bind(context.market_side.as_deref())
        .bind(match intent.side {
            TradeSide::Buy => "BUY",
            TradeSide::Sell => "SELL",
        })
        .bind(intent.quantity)
        .bind(intent.limit_price)
        .bind(filled_quantity)
        .bind(avg_fill_price)
        .bind(status)
        .bind(report.rejection_reason.as_deref())
        .bind(report.slippage)
        .bind(report.market_impact)
        .bind(context_json)
        .execute(&self.pool)
        .await
        {
            warn!(error = %error, order_id, "Failed to persist execution order");
        }
    }

    async fn persist_fill(
        &self,
        strategy: &str,
        intent: &TradingIntent,
        context: &TokenExecutionContext,
        fill: &FillRecord,
        report: &ExecutionReport,
    ) {
        let context_json = json!({
            "runtime_mode": self.mode_label,
            "slippage": report.slippage.map(|value| value.to_string()),
            "market_impact": report.market_impact.map(|value| value.to_string()),
        });

        if let Err(error) = sqlx::query(
            r#"
            INSERT INTO strategy_runtime_fills (
                recorded_at,
                runtime_mode,
                strategy_id,
                deployment_id,
                intent_id,
                order_id,
                fill_id,
                event_id,
                symbol,
                token_id,
                market_side,
                fill_side,
                quantity,
                price,
                fee,
                slippage,
                market_impact,
                fill_timestamp,
                context
            )
            VALUES (
                NOW(), $1, $2, $3, $4, $5, $6, $7, $8, $9,
                $10, $11, $12, $13, $14, $15, $16, $17, $18
            )
            ON CONFLICT (fill_id) DO NOTHING
            "#,
        )
        .bind(self.mode_label.as_str())
        .bind(strategy)
        .bind(intent.deployment_id.as_str())
        .bind(intent.intent_id.as_str())
        .bind(fill.order_id.as_str())
        .bind(fill.fill_id.as_str())
        .bind(context.event_id.as_deref())
        .bind(context.symbol.as_deref())
        .bind(fill.token_id.as_str())
        .bind(context.market_side.as_deref())
        .bind(match fill.side {
            TradeSide::Buy => "BUY",
            TradeSide::Sell => "SELL",
        })
        .bind(fill.quantity)
        .bind(fill.price)
        .bind(fill.fee)
        .bind(report.slippage)
        .bind(report.market_impact)
        .bind(fill.timestamp)
        .bind(context_json)
        .execute(&self.pool)
        .await
        {
            warn!(error = %error, fill_id = %fill.fill_id, "Failed to persist execution fill");
        }
    }
}

#[async_trait]
impl Recorder for RuntimeDbRecorder {
    async fn record_signal(&mut self, signal: &SignalRecord) {
        self.remember_signal_context(signal);
        self.persist_signal(signal).await;
    }

    async fn record_order(
        &mut self,
        strategy: &str,
        intent: &TradingIntent,
        signal: Option<&SignalRecord>,
        report: &ExecutionReport,
        order_id: &str,
    ) {
        let context = self.merge_context(intent, signal);
        self.remember_context(&intent.token_id, &context);
        self.persist_order(strategy, intent, &context, signal, report, order_id)
            .await;
    }

    async fn record_fill(
        &mut self,
        strategy: &str,
        intent: &TradingIntent,
        signal: Option<&SignalRecord>,
        fill: &FillRecord,
        report: &ExecutionReport,
    ) {
        let context = self.merge_context(intent, signal);
        self.remember_context(&intent.token_id, &context);
        self.persist_fill(strategy, intent, &context, fill, report)
            .await;
    }

    async fn flush(&mut self) {}
}

fn build_signal_recorder(db_pool: Option<sqlx::PgPool>, mode: RuntimeMode) -> Box<dyn Recorder> {
    let Some(pool) = db_pool else {
        info!("Signal recorder disabled — DATABASE_URL unavailable");
        return Box::new(NullRecorder);
    };

    let mode_label = match mode {
        RuntimeMode::Backtest => "backtest",
        RuntimeMode::Replay => "replay",
        RuntimeMode::DryRun => "dry_run",
        RuntimeMode::Live => "live",
    }
    .to_string();

    Box::new(RuntimeDbRecorder::new(pool, mode_label))
}

#[derive(Clone)]
struct LiveExecutor {
    gateway: Arc<ploy_connectivity::PolymarketExecutionGateway>,
    next_reconcile_at: Option<Instant>,
}

impl LiveExecutor {
    fn new(gateway: Arc<ploy_connectivity::PolymarketExecutionGateway>) -> Self {
        Self {
            gateway,
            next_reconcile_at: None,
        }
    }

    fn build_request(&self, intent: &TradingIntent) -> ploy_connectivity::ExecutionRequest {
        let limit_price = intent.limit_price.map(|price| price.round_dp(2));
        let quantity = intent.quantity.trunc_with_scale(2);
        ploy_connectivity::ExecutionRequest {
            order_id: intent.intent_id.clone(),
            token_id: intent.token_id.clone(),
            side: intent.side,
            quantity,
            limit_price,
            order_type: ploy_connectivity::OrderExecutionType::FAK,
            aggressive_ticks: 0,
        }
    }
}

#[async_trait]
impl ploy_strategy_bundles::Executor for LiveExecutor {
    async fn submit(&mut self, intent: &TradingIntent, _order_id: &str) -> ExecutionReport {
        use ploy_connectivity::{ExecutionOutcome, LiveExecutionGateway};

        let gateway = self.gateway.clone();
        let request = self.build_request(intent);

        match tokio::task::spawn_blocking(move || gateway.submit(&request)).await {
            Ok(Ok(outcome)) => match outcome {
                ExecutionOutcome::Acknowledged { venue_order_id } => {
                    info!(venue_order_id = %venue_order_id, "Order acknowledged");
                    ExecutionReport {
                        order_id: venue_order_id,
                        fill: None,
                        rejected: false,
                        rejection_reason: None,
                        slippage: None,
                        market_impact: None,
                    }
                }
                ExecutionOutcome::Rejected { reason } => {
                    error!(reason = %reason, "Order rejected by venue");
                    ExecutionReport {
                        order_id: String::new(),
                        fill: None,
                        rejected: true,
                        rejection_reason: Some(reason),
                        slippage: None,
                        market_impact: None,
                    }
                }
            },
            Ok(Err(error)) => {
                error!(error = %error, "Execution gateway error");
                ExecutionReport {
                    order_id: String::new(),
                    fill: None,
                    rejected: true,
                    rejection_reason: Some(error.to_string()),
                    slippage: None,
                    market_impact: None,
                }
            }
            Err(error) => {
                error!(error = %error, "Spawn blocking failed");
                ExecutionReport {
                    order_id: String::new(),
                    fill: None,
                    rejected: true,
                    rejection_reason: Some(format!("internal: {error}")),
                    slippage: None,
                    market_impact: None,
                }
            }
        }
    }

    async fn cancel(&mut self, _order_id: &str) -> bool {
        false
    }

    async fn reconcile_fills(
        &mut self,
        orders: &ploy_trading::OrderLedger,
    ) -> Result<Vec<FillRecord>, String> {
        use ploy_connectivity::{LiveExecutionGateway, TrackedOrder};

        let now = Instant::now();
        if let Some(next_reconcile_at) = self.next_reconcile_at {
            if now < next_reconcile_at {
                return Ok(Vec::new());
            }
        }

        let tracked_orders: Vec<TrackedOrder> = orders
            .orders()
            .filter(|order| {
                order.venue_order_id.is_some()
                    && matches!(
                        order.state,
                        ploy_trading::OrderState::Acknowledged
                            | ploy_trading::OrderState::PartiallyFilled
                    )
            })
            .filter_map(|order| {
                Some(TrackedOrder {
                    order_id: order.order_id.clone(),
                    venue_order_id: order.venue_order_id.clone()?,
                    token_id: order.token_id.clone(),
                })
            })
            .collect();

        if tracked_orders.is_empty() {
            self.next_reconcile_at = None;
            return Ok(Vec::new());
        }

        let gateway = self.gateway.clone();
        match tokio::task::spawn_blocking(move || gateway.reconcile_fills(&tracked_orders)).await {
            Ok(Ok(fills)) => {
                self.next_reconcile_at = Some(now + Duration::from_secs(3));
                Ok(fills)
            }
            Ok(Err(error)) => {
                self.next_reconcile_at = Some(now + Duration::from_secs(10));
                Err(error.to_string())
            }
            Err(error) => {
                self.next_reconcile_at = Some(now + Duration::from_secs(10));
                Err(format!("reconcile task failed: {error}"))
            }
        }
    }
}

fn build_live_executor() -> LiveExecutor {
    let gateway = Arc::new(ploy_connectivity::PolymarketExecutionGateway::from_env());
    LiveExecutor::new(gateway)
}

fn build_strategy(config: &FullConfig) -> Box<dyn StrategyLogic> {
    let configured_variant = config.runtime.strategy_variant.trim();
    let canonical_variant = config.runtime.canonical_strategy_variant();

    match canonical_variant.as_str() {
        "directional" => {
            if configured_variant != "directional" {
                info!(
                    configured_variant = configured_variant,
                    canonical_variant = canonical_variant.as_str(),
                    "Using roadmap alias for directional strategy variant",
                );
            }

            Box::new(ploy_strategy_bundles::DirectionalStrategy::new(
                config.strategy.clone(),
            ))
        }
        "directional_bayes" => {
            info!(
                configured_variant = configured_variant,
                canonical_variant = canonical_variant.as_str(),
                "Using Bayesian directional strategy variant",
            );
            let json = serde_json::to_value(&config.strategy).expect("serialize DirectionalConfig");
            let bayes_config: ploy_strategy_bundles::strategies::directional_bayes::BayesianDirectionalConfig =
                serde_json::from_value(json).expect("deserialize BayesianDirectionalConfig");
            Box::new(BayesianDirectionalStrategy::new(bayes_config))
        }
        "mean_reversion" => {
            info!(
                configured_variant = configured_variant,
                canonical_variant = canonical_variant.as_str(),
                "Using mean-reversion strategy variant",
            );
            Box::new(MeanReversionStrategy::new(config.strategy.clone()))
        }
        _ => {
            eprintln!(
                "Unsupported strategy_variant `{configured_variant}` in config. \
                 Supported runtime variants: directional, directional_bayes, mean_reversion, v1, v2, v3, v4."
            );
            std::process::exit(1);
        }
    }
}

fn prepare_feed_symbols(mode: RuntimeMode, strategy_symbols: &[String]) -> Vec<String> {
    match mode {
        RuntimeMode::Backtest | RuntimeMode::Replay => strategy_symbols.to_vec(),
        RuntimeMode::Live | RuntimeMode::DryRun => strategy_symbols.to_vec(),
    }
}

fn database_unavailable_is_fatal(mode: RuntimeMode, database_url_present: bool) -> bool {
    database_url_present && matches!(mode, RuntimeMode::Live | RuntimeMode::DryRun)
}

#[cfg(test)]
mod tests {
    use super::{build_strategy, database_unavailable_is_fatal, prepare_feed_symbols};
    use ploy_strategy_bundles::{FullConfig, RuntimeMode};

    #[test]
    fn keeps_strategy_symbols_canonical_for_live_feeds() {
        let symbols = vec!["BTCUSDT".to_string(), "ethusdt".to_string()];
        let prepared = prepare_feed_symbols(RuntimeMode::DryRun, &symbols);
        assert_eq!(prepared, vec!["BTCUSDT".to_string(), "ethusdt".to_string()]);
    }

    #[test]
    fn treats_live_and_dry_run_db_connection_failures_as_fatal_when_configured() {
        assert!(database_unavailable_is_fatal(RuntimeMode::Live, true));
        assert!(database_unavailable_is_fatal(RuntimeMode::DryRun, true));
        assert!(!database_unavailable_is_fatal(RuntimeMode::Backtest, true));
        assert!(!database_unavailable_is_fatal(RuntimeMode::Replay, true));
        assert!(!database_unavailable_is_fatal(RuntimeMode::DryRun, false));
    }

    #[test]
    fn roadmap_aliases_build_expected_strategy_variants() {
        for (variant, expected_name) in [
            ("v1", "pm_5m_directional"),
            ("v2", "pm_5m_directional"),
            ("v3", "pm_5m_directional"),
            ("v4", "pm_5m_mean_reversion"),
        ] {
            let config = FullConfig::from_toml(&format!(
                r#"
[runtime]
mode = "dryrun"
strategy_variant = "{variant}"

[strategy]
"#
            ))
            .unwrap();

            let strategy = build_strategy(&config);
            assert_eq!(strategy.name(), expected_name);
        }
    }
}

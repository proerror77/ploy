use chrono::{DateTime, Utc};
use ploy_market_data::feeds::{
    spawn_chainlink_feed, spawn_db_aggtrade_feed, spawn_db_l2_feed, spawn_db_spot_feed,
    spawn_pyth_reference_feed, spawn_spot_feed,
};
use ploy_market_data::reference_prices::new_reference_price_registry;
use ploy_market_data::scanner::spawn_market_scanner;
use ploy_market_data::sports_feed::spawn_sports_feed;
use ploy_strategy_bundles::{
    Feed, FullConfig, LiveFeed, RecordingFeed, RuntimeMode, StrategyLogic,
};
use ploy_trading::{
    FillRecord, IntentPurpose, OrderRecord, OrderState, PnlSnapshot, TradeSide, TradingIntent,
    TradingRuntime, TradingRuntimeSnapshot,
};
use rust_decimal::Decimal;
use sqlx::{postgres::PgPoolOptions, FromRow, PgPool};
use std::env;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

#[cfg(all(feature = "live", feature = "live-execution"))]
mod execution {
    use async_trait::async_trait;
    use ploy_strategy_bundles::{ExecutionPolicy, ExecutionReport};
    use ploy_trading::{FillRecord, TradeSide, TradingIntent};
    use rust_decimal::Decimal;
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use tracing::{debug, error, info};

    #[derive(Clone)]
    pub(super) struct LiveExecutor {
        gateway: Arc<ploy_connectivity::PolymarketExecutionGateway>,
        next_reconcile_at: Option<Instant>,
        policy: ExecutionPolicy,
        last_reconcile_attempted: bool,
    }

    impl LiveExecutor {
        pub(super) fn new(
            gateway: Arc<ploy_connectivity::PolymarketExecutionGateway>,
            policy: ExecutionPolicy,
        ) -> Self {
            Self {
                gateway,
                next_reconcile_at: None,
                policy,
                last_reconcile_attempted: false,
            }
        }

        fn build_request(&self, intent: &TradingIntent) -> ploy_connectivity::ExecutionRequest {
            let limit_price = intent
                .limit_price
                .map(|price| self.slippage_bounded_price(price, intent.side));
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

        fn prepared_quantity(&self, intent: &TradingIntent) -> Decimal {
            let normalized_quantity = intent.quantity.trunc_with_scale(2);
            if intent.side != TradeSide::Buy {
                return normalized_quantity;
            }

            let Some(limit_price) = intent.limit_price else {
                return normalized_quantity;
            };
            if limit_price <= Decimal::ZERO || normalized_quantity <= Decimal::ZERO {
                return normalized_quantity;
            }

            let target_notional = (intent.quantity * limit_price).trunc_with_scale(6);
            if target_notional <= Decimal::ZERO {
                return normalized_quantity;
            }

            let execution_price = self.slippage_bounded_price(limit_price, intent.side);
            if execution_price <= Decimal::ZERO {
                return normalized_quantity;
            }

            let capped_quantity = (target_notional / execution_price).trunc_with_scale(2);
            let prepared_quantity = normalized_quantity.min(capped_quantity);
            if prepared_quantity < normalized_quantity {
                debug!(
                    intent_id = %intent.intent_id,
                    original_quantity = %intent.quantity,
                    prepared_quantity = %prepared_quantity,
                    limit_price = %limit_price,
                    execution_price = %execution_price,
                    target_notional = %target_notional,
                    "Capped live BUY quantity to strategy notional"
                );
            }

            prepared_quantity
        }

        fn slippage_bounded_price(&self, limit_price: Decimal, side: TradeSide) -> Decimal {
            let tolerance = self.policy.max_slippage_bps / Decimal::from(10_000_u32);
            let adjusted = match side {
                TradeSide::Buy => limit_price * (Decimal::ONE + tolerance),
                TradeSide::Sell => limit_price * (Decimal::ONE - tolerance),
            };
            adjusted
                .clamp(Decimal::new(1, 2), Decimal::new(99, 2))
                .round_dp(2)
        }
    }

    #[async_trait]
    impl ploy_strategy_bundles::Executor for LiveExecutor {
        fn execution_policy(&self) -> ExecutionPolicy {
            self.policy
        }

        fn last_reconcile_attempted(&self) -> bool {
            self.last_reconcile_attempted
        }

        fn prepare_intent(&self, intent: &TradingIntent) -> TradingIntent {
            let mut prepared = intent.clone();
            prepared.quantity = self.prepared_quantity(intent);
            prepared
        }

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

            self.last_reconcile_attempted = false;
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
            match tokio::task::spawn_blocking(move || gateway.reconcile_fills(&tracked_orders))
                .await
            {
                Ok(Ok(fills)) => {
                    self.last_reconcile_attempted = true;
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

    #[cfg(test)]
    mod tests {
        use super::*;
        use chrono::Utc;
        use ploy_strategy_bundles::Executor;
        use ploy_trading::IntentPurpose;

        fn test_executor(max_slippage_bps: Decimal) -> LiveExecutor {
            LiveExecutor::new(
                Arc::new(ploy_connectivity::PolymarketExecutionGateway::from_env()),
                ExecutionPolicy {
                    max_slippage_bps,
                    max_attempts: 2,
                    reconcile_cycles_before_retry: 2,
                },
            )
        }

        fn buy_intent(quantity: Decimal, limit_price: Decimal) -> TradingIntent {
            TradingIntent {
                intent_id: "intent-buy".into(),
                deployment_id: "deployment".into(),
                market_id: "event".into(),
                token_id: "token".into(),
                side: TradeSide::Buy,
                quantity,
                limit_price: Some(limit_price),
                purpose: IntentPurpose::Entry,
                created_at: Utc::now(),
            }
        }

        #[test]
        fn prepare_intent_caps_buy_quantity_to_slippage_bounded_notional() {
            let executor = test_executor(Decimal::new(150, 0));
            let intent = buy_intent(Decimal::new(142_857_143, 6), Decimal::new(105, 3));

            let prepared = executor.prepare_intent(&intent);
            let request = executor.build_request(&prepared);
            let execution_price = request.limit_price.expect("bounded live price");

            assert_eq!(execution_price, Decimal::new(11, 2));
            assert_eq!(prepared.quantity, Decimal::new(13_636, 2));
            assert_eq!(request.quantity, Decimal::new(13_636, 2));
            assert!(prepared.quantity * execution_price <= Decimal::new(1_500, 2));
        }

        #[test]
        fn prepare_intent_keeps_buy_quantity_when_slippage_price_does_not_raise_notional() {
            let executor = test_executor(Decimal::new(150, 0));
            let intent = buy_intent(Decimal::new(150, 0), Decimal::new(10, 2));

            let prepared = executor.prepare_intent(&intent);
            let request = executor.build_request(&prepared);

            assert_eq!(request.limit_price, Some(Decimal::new(10, 2)));
            assert_eq!(prepared.quantity, Decimal::new(15_000, 2));
            assert_eq!(request.quantity, Decimal::new(15_000, 2));
        }
    }
}

use crate::recording::build_signal_recorder;
use crate::{database_unavailable_is_fatal, RuntimeModeConfig};

#[derive(Debug, FromRow)]
struct LiveOrderRestoreRow {
    intent_id: String,
    order_id: String,
    deployment_id: String,
    event_id: Option<String>,
    token_id: String,
    order_side: String,
    quantity: Decimal,
    limit_price: Option<Decimal>,
    venue_order_id: Option<String>,
    filled_quantity: Decimal,
    status: String,
    created_at: DateTime<Utc>,
}

#[derive(Debug, FromRow)]
struct LiveFillRestoreRow {
    order_id: String,
    fill_id: String,
    token_id: String,
    fill_side: String,
    quantity: Decimal,
    price: Decimal,
    fee: Decimal,
    fill_timestamp: DateTime<Utc>,
}

pub(crate) async fn run_live_or_dry_run_entry(
    config: &FullConfig,
    symbols: &[String],
    strategy: Box<dyn StrategyLogic>,
    runtime_config: RuntimeModeConfig,
) -> (
    ploy_strategy_bundles::RuntimeResult,
    ploy_trading::TradingRuntimeSnapshot,
) {
    run_live_or_dry_run(config, symbols, strategy, runtime_config).await
}

async fn restore_active_live_trading_runtime(pool: &PgPool) -> Option<TradingRuntime> {
    let orders = match sqlx::query_as::<_, LiveOrderRestoreRow>(
        r#"
        SELECT
            intent_id,
            order_id,
            deployment_id,
            event_id,
            token_id,
            order_side,
            quantity,
            limit_price,
            venue_order_id,
            filled_quantity,
            status,
            created_at
        FROM strategy_runtime_orders
        WHERE runtime_mode = 'live'
          AND venue_order_id IS NOT NULL
          AND status IN ('ACKNOWLEDGED', 'PARTIALLY_FILLED', 'acknowledged', 'partially_filled')
        ORDER BY created_at DESC
        LIMIT 500
        "#,
    )
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(error) => {
            warn!(error = %error, "Failed to restore active live orders from DB");
            return None;
        }
    };

    if orders.is_empty() {
        return None;
    }

    let order_ids = orders
        .iter()
        .map(|order| order.order_id.clone())
        .collect::<Vec<_>>();
    let fills = match sqlx::query_as::<_, LiveFillRestoreRow>(
        r#"
        SELECT order_id, fill_id, token_id, fill_side, quantity, price, fee, fill_timestamp
        FROM strategy_runtime_fills
        WHERE runtime_mode = 'live'
          AND order_id = ANY($1)
        ORDER BY fill_timestamp ASC
        "#,
    )
    .bind(&order_ids)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(error) => {
            warn!(error = %error, "Failed to restore live fills from DB");
            Vec::new()
        }
    };

    let intents = orders
        .iter()
        .map(|order| TradingIntent {
            intent_id: order.intent_id.clone(),
            deployment_id: order.deployment_id.clone(),
            market_id: order.event_id.clone().unwrap_or_default(),
            token_id: order.token_id.clone(),
            side: trade_side_from_db(&order.order_side),
            quantity: order.quantity,
            limit_price: order.limit_price,
            purpose: intent_purpose_from_side(&order.order_side),
            created_at: order.created_at,
        })
        .collect::<Vec<_>>();
    let order_records = orders
        .into_iter()
        .map(|order| OrderRecord {
            order_id: order.order_id,
            intent_id: order.intent_id,
            deployment_id: order.deployment_id,
            token_id: order.token_id,
            requested_qty: order.quantity,
            limit_price: order.limit_price,
            venue_order_id: order.venue_order_id,
            venue_order_history: Vec::new(),
            revision: 0,
            state: order_state_from_db(&order.status),
            filled_qty: order.filled_quantity,
            rejection_reason: None,
            last_error: None,
        })
        .collect::<Vec<_>>();
    let fill_records = fills
        .into_iter()
        .map(|fill| FillRecord {
            fill_id: fill.fill_id,
            order_id: fill.order_id,
            token_id: fill.token_id,
            side: trade_side_from_db(&fill.fill_side),
            quantity: fill.quantity,
            price: fill.price,
            fee: fill.fee,
            timestamp: fill.fill_timestamp,
        })
        .collect::<Vec<_>>();

    info!(
        orders = order_records.len(),
        fills = fill_records.len(),
        "Restored active live trading runtime from DB"
    );

    Some(TradingRuntime::restore(TradingRuntimeSnapshot {
        intents,
        orders: order_records,
        fills: fill_records,
        positions: Vec::new(),
        pnl: PnlSnapshot::default(),
        risk: Default::default(),
    }))
}

fn trade_side_from_db(side: &str) -> TradeSide {
    if side.eq_ignore_ascii_case("SELL") {
        TradeSide::Sell
    } else {
        TradeSide::Buy
    }
}

fn intent_purpose_from_side(side: &str) -> IntentPurpose {
    if side.eq_ignore_ascii_case("SELL") {
        IntentPurpose::Exit
    } else {
        IntentPurpose::Entry
    }
}

fn order_state_from_db(status: &str) -> OrderState {
    match status.to_ascii_uppercase().as_str() {
        "PARTIALLY_FILLED" => OrderState::PartiallyFilled,
        "FILLED" => OrderState::Filled,
        "CANCELED" | "CANCELLED" => OrderState::Canceled,
        "REJECTED" => OrderState::Rejected,
        _ => OrderState::Acknowledged,
    }
}

async fn run_live_or_dry_run(
    config: &FullConfig,
    symbols: &[String],
    strategy: Box<dyn StrategyLogic>,
    runtime_config: RuntimeModeConfig,
) -> (
    ploy_strategy_bundles::RuntimeResult,
    ploy_trading::TradingRuntimeSnapshot,
) {
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
    let mut db_feed_handles = Vec::new();
    if let Some(ref db) = db_pool {
        db_feed_handles.push(spawn_db_spot_feed(tx.clone(), symbols.to_vec(), db.clone()));
        db_feed_handles.push(spawn_db_aggtrade_feed(
            tx.clone(),
            symbols.to_vec(),
            db.clone(),
        ));
        db_feed_handles.push(spawn_db_l2_feed(tx.clone(), symbols.to_vec(), db.clone()));
    }
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
        #[cfg(not(feature = "live-execution"))]
        {
            eprintln!("Live execution requires the `live-execution` feature");
            std::process::exit(1);
        }

        #[cfg(feature = "live-execution")]
        {
            let executor = build_live_executor(config.live_execution_policy());
            let trading = match db_pool.as_ref() {
                Some(pool) => restore_active_live_trading_runtime(pool)
                    .await
                    .unwrap_or_default(),
                None => TradingRuntime::default(),
            };
            let mut runtime = ploy_strategy_bundles::StrategyRuntime::new_with_trading(
                strategy,
                feed,
                executor,
                recorder,
                runtime_config,
                trading,
            );
            let result = runtime.run().await;
            let snapshot = runtime
                .trading()
                .snapshot(&std::collections::BTreeMap::new());
            (result, snapshot)
        }
    } else {
        let executor = ploy_strategy_bundles::SimulatedExecutor::new(config.sim_executor_config());
        let mut runtime = ploy_strategy_bundles::StrategyRuntime::new(
            strategy,
            feed,
            executor,
            recorder,
            runtime_config,
        );
        let result = runtime.run().await;
        let snapshot = runtime
            .trading()
            .snapshot(&std::collections::BTreeMap::new());
        (result, snapshot)
    };

    spot_handle.abort();
    for handle in db_feed_handles {
        handle.abort();
    }
    chainlink_handle.abort();
    pyth_handle.abort();
    scanner_handle.abort();
    if let Some(handle) = sports_handle {
        handle.abort();
    }

    result
}

#[cfg(all(feature = "live", feature = "live-execution"))]
fn build_live_executor(policy: ploy_strategy_bundles::ExecutionPolicy) -> execution::LiveExecutor {
    let gateway = Arc::new(ploy_connectivity::PolymarketExecutionGateway::from_env());
    execution::LiveExecutor::new(gateway, policy)
}

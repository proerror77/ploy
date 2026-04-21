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
use sqlx::postgres::PgPoolOptions;
use std::env;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

#[cfg(all(feature = "live", feature = "live-execution"))]
mod execution {
    use async_trait::async_trait;
    use ploy_strategy_bundles::ExecutionReport;
    use ploy_trading::{FillRecord, TradingIntent};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use tracing::{error, info};

    #[derive(Clone)]
    pub(super) struct LiveExecutor {
        gateway: Arc<ploy_connectivity::PolymarketExecutionGateway>,
        next_reconcile_at: Option<Instant>,
    }

    impl LiveExecutor {
        pub(super) fn new(gateway: Arc<ploy_connectivity::PolymarketExecutionGateway>) -> Self {
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
            match tokio::task::spawn_blocking(move || gateway.reconcile_fills(&tracked_orders))
                .await
            {
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
}

use crate::recording::build_signal_recorder;
use crate::{database_unavailable_is_fatal, RuntimeModeConfig};

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
    let _db_spot_handle = if let Some(ref db) = db_pool {
        Some(spawn_db_spot_feed(tx.clone(), symbols.to_vec(), db.clone()))
    } else {
        None
    };
    let _db_aggtrade_handle = if let Some(ref db) = db_pool {
        Some(spawn_db_aggtrade_feed(
            tx.clone(),
            symbols.to_vec(),
            db.clone(),
        ))
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
        #[cfg(feature = "auto-claimer")]
        if let Err(error) = ploy_claimer::ensure_account_claimer_daemon().await {
            warn!("Auto-claimer daemon failed to start: {error}");
        }

        #[cfg(not(feature = "live-execution"))]
        {
            eprintln!("Live execution requires the `live-execution` feature");
            std::process::exit(1);
        }

        #[cfg(feature = "live-execution")]
        {
            let executor = build_live_executor();
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
    chainlink_handle.abort();
    pyth_handle.abort();
    scanner_handle.abort();
    if let Some(handle) = sports_handle {
        handle.abort();
    }

    result
}

#[cfg(all(feature = "live", feature = "live-execution"))]
fn build_live_executor() -> execution::LiveExecutor {
    let gateway = Arc::new(ploy_connectivity::PolymarketExecutionGateway::from_env());
    execution::LiveExecutor::new(gateway)
}

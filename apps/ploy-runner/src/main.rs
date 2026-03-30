mod feeds;
mod scanner;

use std::env;
use std::sync::Arc;

use ploy_strategy_bundles::{
    CallbackExecutor, DirectionalStrategy, ExecutionReport, FullConfig, LiveFeed, NullRecorder,
    RuntimeMode, SimulatedExecutor, StrategyRuntime,
};
use ploy_trading::TradingIntent;
use tokio::sync::broadcast;
use tracing::{error, info};

use feeds::spawn_spot_feed;
use scanner::spawn_market_scanner;

fn print_usage() {
    eprintln!("Usage: ploy-runner --config <path.toml> [--dry-run]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --config <path>   Unified TOML config file (required)");
    eprintln!("  --dry-run         Force dry-run mode (simulated execution)");
    eprintln!("  --foreground      Run in foreground (default, kept for compat)");
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,hyper_util=off,hyper=off,reqwest=off,h2=off,rustls=off".parse().unwrap()),
        )
        .init();

    let args: Vec<String> = env::args().collect();

    let mut config_path: Option<String> = None;
    let mut force_dry_run = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--config" => {
                i += 1;
                config_path = args.get(i).cloned();
            }
            "--dry-run" => force_dry_run = true,
            "--foreground" => {} // compat, always foreground
            "--help" | "-h" => {
                print_usage();
                return;
            }
            other => {
                eprintln!("Unknown argument: {other}");
                print_usage();
                std::process::exit(1);
            }
        }
        i += 1;
    }

    let config_path = config_path.unwrap_or_else(|| {
        eprintln!("Error: --config is required");
        print_usage();
        std::process::exit(1);
    });

    let config = match FullConfig::from_file(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to load config {config_path}: {e}");
            std::process::exit(1);
        }
    };

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

    // Broadcast channel for market updates
    let (tx, rx) = broadcast::channel(8192);
    let tx = Arc::new(tx);

    // Spawn feed producers
    let symbols: Vec<String> = config
        .strategy
        .symbols
        .iter()
        .map(|s| s.to_lowercase())
        .collect();

    // 1. Spot feed — always available (Binance via RTDS)
    let spot_handle = spawn_spot_feed(tx.clone(), symbols.clone());

    // 2. Market scanner — discovers events and injects EventDiscovered/EventExpired
    //    Also spawns quote feeds dynamically as new tokens are discovered.
    let scanner_handle = spawn_market_scanner(tx.clone(), symbols.clone());

    // Build strategy
    let strategy = DirectionalStrategy::new(config.strategy.clone());

    // Build executor based on mode
    let feed = LiveFeed::new(rx);
    let recorder: Box<dyn ploy_strategy_bundles::Recorder> = Box::new(NullRecorder);

    let result = match runtime_config.mode {
        RuntimeMode::Live => {
            let executor = build_live_executor();
            let mut runtime = StrategyRuntime::new(
                strategy,
                feed,
                executor,
                recorder,
                runtime_config,
            );
            runtime.run().await
        }
        RuntimeMode::DryRun | RuntimeMode::Backtest => {
            let sim_config = config.sim_executor_config();
            let executor = SimulatedExecutor::new(sim_config);
            let mut runtime = StrategyRuntime::new(
                strategy,
                feed,
                executor,
                recorder,
                runtime_config,
            );
            runtime.run().await
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

    // Clean up feed tasks
    spot_handle.abort();
    scanner_handle.abort();
}

/// Build a CallbackExecutor that routes orders through PolymarketExecutionGateway.
fn build_live_executor() -> CallbackExecutor {
    use ploy_connectivity::{ExecutionRequest, LiveExecutionGateway, PolymarketExecutionGateway};

    let gateway = Arc::new(PolymarketExecutionGateway::from_env());

    CallbackExecutor::new(Box::new(move |intent: TradingIntent| {
        let gw = gateway.clone();
        Box::pin(async move {
            let request = ExecutionRequest {
                order_id: intent.intent_id.clone(),
                token_id: intent.token_id.clone(),
                side: intent.side,
                quantity: intent.quantity,
                limit_price: intent.limit_price,
            };

            match tokio::task::spawn_blocking(move || gw.submit(&request)).await {
                Ok(Ok(outcome)) => {
                    use ploy_connectivity::ExecutionOutcome;
                    match outcome {
                        ExecutionOutcome::Acknowledged { venue_order_id } => {
                            info!(venue_order_id = %venue_order_id, "Order acknowledged");
                            ExecutionReport {
                                order_id: venue_order_id,
                                fill: None, // fills come from reconciliation
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
                    }
                }
                Ok(Err(e)) => {
                    error!(error = %e, "Execution gateway error");
                    ExecutionReport {
                        order_id: String::new(),
                        fill: None,
                        rejected: true,
                        rejection_reason: Some(e.to_string()),
                        slippage: None,
                        market_impact: None,
                    }
                }
                Err(e) => {
                    error!(error = %e, "Spawn blocking failed");
                    ExecutionReport {
                        order_id: String::new(),
                        fill: None,
                        rejected: true,
                        rejection_reason: Some(format!("internal: {e}")),
                        slippage: None,
                        market_impact: None,
                    }
                }
            }
        })
    }))
}

#[cfg(feature = "rl")]
use ploy::error::Result;
#[cfg(feature = "rl")]
use tokio::signal;
#[cfg(feature = "rl")]
use tracing::{debug, info, warn};

#[cfg(feature = "rl")]
fn intent_to_request(intent: &ploy::platform::OrderIntent) -> ploy::domain::OrderRequest {
    let mut request = if intent.is_buy {
        ploy::domain::OrderRequest::buy_limit(
            intent.token_id.clone(),
            intent.side,
            intent.shares,
            intent.limit_price,
        )
    } else {
        ploy::domain::OrderRequest::sell_limit(
            intent.token_id.clone(),
            intent.side,
            intent.shares,
            intent.limit_price,
        )
    };
    let client_order_id = format!("intent:{}", intent.intent_id);
    request.client_order_id = client_order_id.clone();
    request.idempotency_key = Some(client_order_id);
    request
}

#[cfg(feature = "rl")]
async fn execute_intent(
    executor: &ploy::strategy::OrderExecutor,
    intent: &ploy::platform::OrderIntent,
) -> Result<ploy::platform::ExecutionReport> {
    let request = intent_to_request(intent);
    match executor.execute(&request).await {
        Ok(result) => {
            use ploy::domain::OrderStatus;
            use ploy::platform::ExecutionStatus;

            let status = match result.status {
                OrderStatus::Pending => ExecutionStatus::Pending,
                OrderStatus::Submitted => ExecutionStatus::Submitted,
                OrderStatus::PartiallyFilled => ExecutionStatus::PartiallyFilled,
                OrderStatus::Filled => ExecutionStatus::Filled,
                OrderStatus::Cancelled => ExecutionStatus::Cancelled,
                OrderStatus::Rejected | OrderStatus::Failed => ExecutionStatus::Rejected,
                OrderStatus::Expired => ExecutionStatus::Expired,
            };

            Ok(ploy::platform::ExecutionReport {
                intent_id: intent.intent_id,
                agent_id: intent.agent_id.clone(),
                order_id: Some(result.order_id),
                status,
                filled_shares: result.filled_shares,
                avg_fill_price: result.avg_fill_price,
                fees: rust_decimal::Decimal::ZERO,
                error_message: None,
                executed_at: chrono::Utc::now(),
                latency_ms: result.elapsed_ms,
            })
        }
        Err(err) => Ok(ploy::platform::ExecutionReport::rejected(
            intent,
            err.to_string(),
        )),
    }
}

#[cfg(feature = "rl")]
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_agent(
    symbol: &str,
    market: &str,
    up_token: &str,
    down_token: &str,
    shares: u64,
    max_exposure: f64,
    exploration: f32,
    online_learning: bool,
    dry_run: bool,
    tick_interval: u64,
    policy_onnx: &Option<String>,
    policy_output: &str,
    policy_version: &Option<String>,
) -> Result<()> {
    use ploy::adapters::{polymarket_clob::POLYGON_CHAIN_ID, PolymarketClient};
    use ploy::error::PloyError;
    use ploy::platform::{
        AgentRiskParams, CryptoEvent, DataPlaneConfig, DataPlaneFreshness, PlatformDataPlane,
        QuoteData,
    };
    use ploy::rl::{RLCryptoRuntime, RLCryptoRuntimeConfig, RLConfig};
    use ploy::signing::Wallet;
    use ploy::AgentRiskParams;
    use rust_decimal::prelude::ToPrimitive;
    use rust_decimal::Decimal;
    use std::sync::Arc;

    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║         Ploy RL Agent - Canonical Runtime Helper             ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!(
        "║  Symbol:         {:>10}                                    ║",
        symbol
    );
    println!(
        "║  Market:         {:>10}                                    ║",
        market
    );
    println!(
        "║  UP Token:       {}...                                    ",
        &up_token[..up_token.len().min(20)]
    );
    println!(
        "║  DOWN Token:     {}...                                    ",
        &down_token[..down_token.len().min(20)]
    );
    println!(
        "║  Shares:         {:>10}                                    ║",
        shares
    );
    println!(
        "║  Max Exposure:   ${:>9.2}                                   ║",
        max_exposure
    );
    println!(
        "║  Exploration:    {:>10.2}                                   ║",
        exploration
    );
    println!(
        "║  Online Learn:   {:>10}                                    ║",
        online_learning
    );
    println!(
        "║  Dry Run:        {:>10}                                    ║",
        dry_run
    );
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    let mut rl_config = RLConfig::default();
    rl_config.training.online_learning = online_learning;
    rl_config.training.exploration_rate = exploration;

    let mut runtime = RLCryptoRuntime::new(RLCryptoRuntimeConfig {
        id: "rl-crypto-agent".to_string(),
        name: format!("RL Agent - {}", symbol),
        coins: vec![symbol.replace("USDT", "")],
        up_token_id: up_token.to_string(),
        down_token_id: down_token.to_string(),
        binance_symbol: symbol.to_string(),
        market_slug: market.to_string(),
        default_shares: shares,
        risk_params: AgentRiskParams {
            max_order_value: Decimal::try_from(max_exposure / 2.0).unwrap_or(Decimal::new(50, 0)),
            max_total_exposure: Decimal::try_from(max_exposure).unwrap_or(Decimal::new(100, 0)),
            ..Default::default()
        },
        rl_config,
        online_learning,
        exploration_rate: exploration,
        policy_model_path: policy_onnx.clone(),
        policy_output: policy_output.to_string(),
        policy_model_version: policy_version.clone(),
    });
    runtime.start().await?;

    let symbol_upper = symbol.to_uppercase();
    let data_plane = Arc::new(PlatformDataPlane::new(
        DataPlaneConfig {
            polymarket_ws_url: "wss://ws-subscriptions-clob.polymarket.com/ws/market".to_string(),
            binance_spot_symbols: vec![symbol_upper.clone()],
            ..Default::default()
        },
        Arc::new(DataPlaneFreshness::new()),
    ));
    let bn_ws = data_plane.binance_ws().ok_or_else(|| {
        PloyError::Validation("rl agent data plane missing binance websocket".to_string())
    })?;
    let pm_ws = data_plane.polymarket_ws().ok_or_else(|| {
        PloyError::Validation("rl agent data plane missing polymarket websocket".to_string())
    })?;

    let price_cache = bn_ws.price_cache().clone();
    pm_ws.register_token(up_token, ploy::domain::Side::Up).await;
    pm_ws.register_token(down_token, ploy::domain::Side::Down)
        .await;
    data_plane.start(Vec::new()).await?;
    let quote_cache = pm_ws.quote_cache().clone();

    println!("🚀 Runtime started. Listening for market data...\n");
    println!("📡 Binance: {} | Polymarket: UP/DOWN tokens", symbol_upper);

    let executor = if dry_run {
        None
    } else {
        info!("Setting up live order execution...");
        let wallet = Wallet::from_env(POLYGON_CHAIN_ID)?;
        info!("Wallet loaded: {:?}", wallet.address());
        let client = PolymarketClient::new_authenticated(
            "https://clob.polymarket.com",
            wallet,
            true,
        )
        .await?;
        info!("✅ Authenticated with Polymarket CLOB");
        Some(ploy::strategy::OrderExecutor::new(client, Default::default()))
    };

    let tick_duration = std::time::Duration::from_millis(tick_interval);
    let mut interval = tokio::time::interval(tick_duration);
    let mut step_count = 0u64;
    let mut events_received = 0u64;
    let mut intents_generated = 0u64;
    let mut quotes_received = false;
    let mut intents_generated = 0u64;
    let mut executions_success = 0u64;
    let mut executions_failed = 0u64;

    loop {
        tokio::select! {
            _ = interval.tick() => {
                step_count += 1;

                let spot_price = match price_cache.get(&symbol_upper).await {
                    Some(sp) => sp.price,
                    None => continue,
                };

                let momentum = {
                    let m1 = price_cache.momentum(&symbol_upper, 1).await;
                    let m5 = price_cache.momentum(&symbol_upper, 5).await;
                    let m15 = price_cache.momentum(&symbol_upper, 15).await;
                    let m60 = price_cache.momentum(&symbol_upper, 60).await;
                    match (m1, m5, m15, m60) {
                        (Some(a), Some(b), Some(c), Some(d)) => Some([
                            a.to_f64().unwrap_or(0.0),
                            b.to_f64().unwrap_or(0.0),
                            c.to_f64().unwrap_or(0.0),
                            d.to_f64().unwrap_or(0.0),
                        ]),
                        _ => None,
                    }
                };

                let up_quote = quote_cache.get(up_token);
                let down_quote = quote_cache.get(down_token);

                let quotes = match (&up_quote, &down_quote) {
                    (Some(uq), Some(dq)) => {
                        if !quotes_received {
                            println!("✅ Receiving live Polymarket quotes!");
                            quotes_received = true;
                        }
                        Some(QuoteData {
                            up_bid: uq.best_bid.unwrap_or(Decimal::ZERO),
                            up_ask: uq.best_ask.unwrap_or(Decimal::ONE),
                            down_bid: dq.best_bid.unwrap_or(Decimal::ZERO),
                            down_ask: dq.best_ask.unwrap_or(Decimal::ONE),
                            timestamp: chrono::Utc::now(),
                        })
                    }
                    (Some(uq), None) => Some(QuoteData {
                        up_bid: uq.best_bid.unwrap_or(Decimal::ZERO),
                        up_ask: uq.best_ask.unwrap_or(Decimal::ONE),
                        down_bid: Decimal::ZERO,
                        down_ask: Decimal::ONE,
                        timestamp: chrono::Utc::now(),
                    }),
                    (None, Some(dq)) => Some(QuoteData {
                        up_bid: Decimal::ZERO,
                        up_ask: Decimal::ONE,
                        down_bid: dq.best_bid.unwrap_or(Decimal::ZERO),
                        down_ask: dq.best_ask.unwrap_or(Decimal::ONE),
                        timestamp: chrono::Utc::now(),
                    }),
                    (None, None) => {
                        if step_count % 30 == 0 {
                            debug!("Waiting for Polymarket quotes...");
                        }
                        continue;
                    }
                };

                let intents = runtime.on_crypto_event(&CryptoEvent {
                    symbol: symbol.to_string(),
                    spot_price,
                    round_slug: Some(market.to_string()),
                    quotes,
                    momentum,
                });
                runtime.on_tick(chrono::Utc::now());

                if !intents.is_empty() {
                    intents_generated += intents.len() as u64;
                    for intent in intents {
                        if dry_run {
                            println!(
                                "📝 [DRY] Intent: {} {} {} @ {} ({})",
                                if intent.is_buy { "BUY" } else { "SELL" },
                                intent.shares,
                                intent.side,
                                intent.limit_price,
                                intent.market_slug,
                            );
                            continue;
                        }

                        println!(
                            "🔴 [LIVE] Executing: {} {} {} @ {} ({})",
                            if intent.is_buy { "BUY" } else { "SELL" },
                            intent.shares,
                            intent.side,
                            intent.limit_price,
                            intent.market_slug,
                        );

                        if let Some(executor) = executor.as_ref() {
                            let report = execute_intent(executor, &intent).await?;
                            if report.is_success() {
                                executions_success += 1;
                            } else {
                                executions_failed += 1;
                            }
                            runtime.on_execution(&report);
                        } else {
                            warn!("Live mode requested but no executor initialized");
                        }
                    }
                }

                if step_count % 30 == 0 {
                    let up_ask = up_quote.as_ref().and_then(|q| q.best_ask).unwrap_or(Decimal::ZERO);
                    let down_ask = down_quote.as_ref().and_then(|q| q.best_ask).unwrap_or(Decimal::ZERO);
                    let sum_asks = up_ask + down_ask;
                    println!(
                        "📊 Step {}: spot={} | UP={}/{} DOWN={}/{} | sum_asks={}",
                        step_count,
                        spot_price,
                        up_quote.as_ref().and_then(|q| q.best_bid).unwrap_or(Decimal::ZERO),
                        up_ask,
                        down_quote.as_ref().and_then(|q| q.best_bid).unwrap_or(Decimal::ZERO),
                        down_ask,
                        sum_asks,
                    );
                }
            }
            _ = signal::ctrl_c() => {
                println!("\n\n╔══════════════════════════════════════════════════════════════╗");
                println!("║               Session Summary                                ║");
                println!("╠══════════════════════════════════════════════════════════════╣");
                println!("║  Total Steps:     {:>10}                                   ║", step_count);
                println!("║  Intents Gen:     {:>10}                                   ║", intents_generated);
                println!("║  Exec Success:    {:>10}                                   ║", executions_success);
                println!("║  Exec Failed:     {:>10}                                   ║", executions_failed);
                println!("╚══════════════════════════════════════════════════════════════╝");
                runtime.stop().await?;
                break;
            }
        }
    }

    Ok(())
}

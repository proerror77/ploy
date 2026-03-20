#[cfg(feature = "rl")]
use ploy::error::Result;
#[cfg(feature = "rl")]
use ploy::{ExecutionReport, OrderIntent};
#[cfg(feature = "rl")]
use rust_decimal::prelude::ToPrimitive;
#[cfg(feature = "rl")]
use serde::Deserialize;
#[cfg(feature = "rl")]
use serde_json::{json, Value};
#[cfg(feature = "rl")]
use std::sync::OnceLock;
#[cfg(feature = "rl")]
use tokio::signal;
#[cfg(feature = "rl")]
use tracing::{debug, error, warn};

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
    use ploy::data_plane::{DataPlaneConfig, DataPlaneFreshness, PlatformDataPlane};
    use ploy::error::PloyError;
    use ploy::rl::cli_agent::RLCryptoAgentConfig;
    use ploy::rl::config::RLConfig;
    use ploy::rl::integration::{RLCryptoRuntime, RLCryptoRuntimeConfig};
    use ploy::rl::{CryptoEvent, QuoteData};
    use ploy::AgentRiskParams;
    use rust_decimal::prelude::ToPrimitive;
    use rust_decimal::Decimal;
    use std::sync::Arc;

    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║         Ploy RL Agent - Coordinator Ingress                  ║");
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
        agent_config: RLCryptoAgentConfig {
            id: "rl-crypto-agent".to_string(),
            name: format!("RL Agent - {}", symbol),
            coins: vec![symbol.replace("USDT", "")],
            up_token_id: up_token.to_string(),
            down_token_id: down_token.to_string(),
            binance_symbol: symbol.to_string(),
            market_slug: market.to_string(),
            default_shares: shares,
            risk_params: AgentRiskParams {
                max_order_value: Decimal::try_from(max_exposure / 2.0)
                    .unwrap_or(Decimal::new(50, 0)),
                max_total_exposure: Decimal::try_from(max_exposure).unwrap_or(Decimal::new(100, 0)),
                ..Default::default()
            },
            rl_config,
            online_learning,
            exploration_rate: exploration,
            policy_model_path: policy_onnx.clone(),
            policy_output: policy_output.to_string(),
            policy_model_version: policy_version.clone(),
        },
        ..Default::default()
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
    pm_ws
        .register_token(down_token, ploy::domain::Side::Down)
        .await;
    data_plane.start(Vec::new()).await?;
    let quote_cache = pm_ws.quote_cache().clone();

    println!("🚀 Runtime started. Listening for market data...\n");
    println!("📡 Binance: {} | Polymarket: UP/DOWN tokens", symbol_upper);

    let tick_duration = std::time::Duration::from_millis(tick_interval);
    let mut interval = tokio::time::interval(tick_duration);
    let mut step_count = 0u64;
    let mut events_received = 0u64;
    let mut intents_generated = 0u64;
    let mut quotes_received = false;
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

                let event = CryptoEvent {
                    symbol: symbol.to_string(),
                    spot_price,
                    round_slug: Some(market.to_string()),
                    quotes,
                    momentum,
                };
                events_received += 1;

                match runtime.on_crypto_event(&event).await {
                    Ok(intents) => {
                        intents_generated += intents.len() as u64;
                        for intent in intents {
                            if dry_run {
                                println!("📝 [DRY] Intent: {} {} {} @ {} ({})",
                                    if intent.is_buy { "BUY" } else { "SELL" },
                                    intent.shares,
                                    intent.side,
                                    intent.limit_price,
                                    intent.market_slug,
                                );
                            } else {
                                println!("🔴 [LIVE] Routing via coordinator: {} {} {} @ {} ({})",
                                    if intent.is_buy { "BUY" } else { "SELL" },
                                    intent.shares,
                                    intent.side,
                                    intent.limit_price,
                                    intent.market_slug,
                                );
                                match submit_intent_via_coordinator(&intent).await {
                                    Ok(report) => {
                                        executions_success += 1;
                                        runtime.on_execution(report).await;
                                    }
                                    Err(e) => {
                                        executions_failed += 1;
                                        error!("Failed to submit RL intent via coordinator: {}", e);
                                        runtime
                                            .on_execution(ExecutionReport::rejected(
                                                &intent,
                                                e.to_string(),
                                            ))
                                            .await;
                                    }
                                }
                            }
                        }
                    }
                    Err(error) => {
                        warn!("RL runtime failed to process crypto event: {}", error);
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
                println!("║  Events Received: {:>10}                                   ║", events_received);
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

#[cfg(feature = "rl")]
#[derive(Debug, Deserialize)]
struct CoordinatorIntentResponse {
    intent_id: String,
}

#[cfg(feature = "rl")]
fn coordinator_intent_ingress_url() -> String {
    std::env::var("PLOY_RPC_COORDINATOR_INTENT_URL")
        .or_else(|_| std::env::var("PLOY_COORDINATOR_INTENT_URL"))
        .unwrap_or_else(|_| "http://127.0.0.1:8081/api/sidecar/intents".to_string())
}

#[cfg(feature = "rl")]
fn coordinator_intent_ingress_token() -> Option<String> {
    std::env::var("PLOY_RPC_SIDECAR_AUTH_TOKEN")
        .or_else(|_| std::env::var("PLOY_SIDECAR_AUTH_TOKEN"))
        .or_else(|_| std::env::var("PLOY_API_SIDECAR_AUTH_TOKEN"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(feature = "rl")]
fn rl_http_client() -> Result<&'static reqwest::Client> {
    static CLIENT: OnceLock<std::result::Result<reqwest::Client, String>> = OnceLock::new();
    let client = CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(8))
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .pool_max_idle_per_host(16)
            .tcp_nodelay(true)
            .build()
            .map_err(|error| format!("failed to build RL coordinator client: {}", error))
    });

    client
        .as_ref()
        .map_err(|msg| ploy::error::PloyError::Internal(msg.clone()))
}

#[cfg(feature = "rl")]
fn build_coordinator_payload(intent: &OrderIntent) -> Result<Value> {
    let deployment_id = intent.deployment_id().ok_or_else(|| {
        ploy::error::PloyError::Validation(format!(
            "RL intent {} is missing deployment_id metadata required for coordinator ingress",
            intent.intent_id
        ))
    })?;
    let price_limit = intent.limit_price.to_f64().ok_or_else(|| {
        ploy::error::PloyError::Validation(format!(
            "RL intent {} has price that cannot be represented as f64",
            intent.intent_id
        ))
    })?;

    let mut metadata = intent.metadata.clone();
    metadata
        .entry("source".to_string())
        .or_insert_with(|| "cli.rl.agent".to_string());
    metadata
        .entry("agent_id".to_string())
        .or_insert_with(|| intent.agent_id.clone());
    metadata
        .entry("runtime".to_string())
        .or_insert_with(|| "rl_cli".to_string());
    metadata
        .entry("client_order_id".to_string())
        .or_insert_with(|| intent.client_order_id.clone());

    Ok(json!({
        "deployment_id": deployment_id,
        "domain": intent.domain.to_string().to_ascii_lowercase(),
        "market_slug": intent.market_slug.clone(),
        "token_id": intent.token_id.clone(),
        "side": intent.side.as_str(),
        "order_side": if intent.is_buy { "BUY" } else { "SELL" },
        "is_buy": intent.is_buy,
        "size": intent.shares,
        "price_limit": price_limit,
        "idempotency_key": intent.client_order_id.clone(),
        "reason": format!("rl cli submit: {}", intent.agent_id),
        "priority": priority_label(intent.priority),
        "metadata": metadata,
        "dry_run": false,
    }))
}

#[cfg(feature = "rl")]
fn priority_label(priority: ploy::coordinator::OrderPriority) -> &'static str {
    match priority {
        ploy::coordinator::OrderPriority::Critical => "critical",
        ploy::coordinator::OrderPriority::High => "high",
        ploy::coordinator::OrderPriority::Normal => "normal",
        ploy::coordinator::OrderPriority::Low => "low",
    }
}

#[cfg(feature = "rl")]
async fn submit_intent_via_coordinator(intent: &OrderIntent) -> Result<ExecutionReport> {
    let payload = build_coordinator_payload(intent)?;
    let url = coordinator_intent_ingress_url();
    let client = rl_http_client()?;

    let mut request = client.post(&url).json(&payload);
    if let Some(token) = coordinator_intent_ingress_token() {
        request = request.header("x-ploy-sidecar-token", token);
    }

    let response = request.send().await.map_err(|error| {
        ploy::error::PloyError::Internal(format!(
            "failed to reach coordinator intent ingress {}: {}",
            url, error
        ))
    })?;
    let status = response.status();
    let text = response
        .text()
        .await
        .unwrap_or_else(|_| "<empty>".to_string());

    if !status.is_success() {
        let message = format!(
            "coordinator intent ingress rejected RL intent (status={}): {}",
            status, text
        );
        return Err(if status.is_client_error() {
            ploy::error::PloyError::Validation(message)
        } else {
            ploy::error::PloyError::Internal(message)
        });
    }

    let response: CoordinatorIntentResponse = serde_json::from_str(&text).map_err(|error| {
        ploy::error::PloyError::Internal(format!(
            "invalid ingress JSON from coordinator intent ingress: {}",
            error
        ))
    })?;

    Ok(ExecutionReport::submitted(intent, Some(response.intent_id)))
}

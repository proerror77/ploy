//! Crypto split arbitrage runner
//!
//! Main entry point for running split arbitrage on crypto markets.

use super::CryptoMarketDiscovery;
use crate::adapters::{PolymarketClient, PolymarketWebSocket};
use crate::error::Result;
use crate::strategy::core::{MarketDiscovery, SplitArbConfig, SplitArbEngine};
use crate::strategy::OrderExecutor;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

/// Configuration specific to crypto split arbitrage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CryptoSplitArbConfig {
    /// Base split arb config
    #[serde(flatten)]
    pub base: SplitArbConfig,
    
    /// Series IDs to monitor
    pub series_ids: Vec<String>,
}

impl Default for CryptoSplitArbConfig {
    fn default() -> Self {
        Self {
            base: SplitArbConfig::default(),
            series_ids: vec![
                "10423".into(), // SOL 15m
                "10191".into(), // ETH 15m
                "41".into(),    // BTC daily
            ],
        }
    }
}

/// Run crypto split arbitrage strategy
pub async fn run_crypto_split_arb(
    client: PolymarketClient,
    executor: OrderExecutor,
    config: CryptoSplitArbConfig,
    dry_run: bool,
) -> Result<()> {
    info!("Starting crypto split arbitrage strategy");
    info!("Monitoring series: {:?}", config.series_ids);
    
    // Print config banner
    println!("\n\x1b[35m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[35m║         CRYPTO SPLIT ARBITRAGE (gabagool22 分时套利)         ║\x1b[0m");
    println!("\x1b[35m╠══════════════════════════════════════════════════════════════╣\x1b[0m");
    println!("\x1b[35m║\x1b[0m  Max Entry Price:    {}¢                                     \x1b[35m║\x1b[0m", 
             config.base.max_entry_price * dec!(100));
    println!("\x1b[35m║\x1b[0m  Target Total Cost:  {}¢ (profit: {}¢)                       \x1b[35m║\x1b[0m",
             config.base.target_total_cost * dec!(100),
             (Decimal::ONE - config.base.target_total_cost) * dec!(100));
    println!("\x1b[35m║\x1b[0m  Min Profit Margin:  {}¢                                      \x1b[35m║\x1b[0m",
             config.base.min_profit_margin * dec!(100));
    println!("\x1b[35m║\x1b[0m  Mode:               {}                                \x1b[35m║\x1b[0m",
             if dry_run { "DRY RUN" } else { "LIVE" });
    println!("\x1b[35m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");
    
    // Create discovery
    let discovery = CryptoMarketDiscovery::with_series(client.clone(), config.series_ids.clone());
    
    // Discover markets
    let markets = discovery.discover_markets().await?;
    info!("Monitoring {} markets, {} tokens", markets.len(), markets.len() * 2);
    
    if markets.is_empty() {
        warn!("No markets found to monitor!");
        return Ok(());
    }
    
    // Create engine
    let engine = Arc::new(SplitArbEngine::new(config.base, client, executor, dry_run));
    
    // Add markets to engine
    let token_ids: Vec<String> = markets.iter()
        .flat_map(|m| vec![m.yes_token_id.clone(), m.no_token_id.clone()])
        .collect();
    
    engine.add_markets(markets).await;
    
    info!("Found {} tokens to monitor", token_ids.len());
    
    // Connect to WebSocket
    info!("Split Arbitrage Engine started");
    info!("Config: max_entry={}¢, target_total={}¢, min_profit={}¢",
          engine.config().max_entry_price * dec!(100),
          engine.config().target_total_cost * dec!(100),
          engine.config().min_profit_margin * dec!(100));
    
    let ws_url = "wss://ws-subscriptions-clob.polymarket.com/ws/market";
    let ws = PolymarketWebSocket::new(ws_url);
    
    // Get update receiver
    let mut update_rx = ws.subscribe_updates();
    
    // Stats timer
    let engine_clone = Arc::clone(&engine);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            engine_clone.print_stats().await;
        }
    });
    
    // Spawn WebSocket runner
    let token_ids_clone = token_ids.clone();
    tokio::spawn(async move {
        if let Err(e) = ws.run(token_ids_clone).await {
            warn!("WebSocket error: {}", e);
        }
    });
    
    // Main loop - process updates
    loop {
        match update_rx.recv().await {
            Ok(quote_update) => {
                engine.on_price_update(
                    &quote_update.token_id,
                    quote_update.quote.best_bid,
                    quote_update.quote.best_ask,
                ).await;
            }
            Err(e) => {
                warn!("Update channel error: {}", e);
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

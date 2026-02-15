//! Sports split arbitrage runner
//!
//! Main entry point for running split arbitrage on sports markets.

use super::{SportsLeague, SportsMarketDiscovery};
use crate::adapters::{PolymarketClient, PolymarketWebSocket};
use crate::error::Result;
use crate::strategy::core::{MarketDiscovery, SplitArbConfig, SplitArbEngine};
use crate::strategy::OrderExecutor;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

/// Configuration specific to sports split arbitrage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SportsSplitArbConfig {
    /// Base split arb config
    #[serde(flatten)]
    pub base: SplitArbConfig,

    /// Leagues to monitor
    pub leagues: Vec<SportsLeague>,
}

impl Default for SportsSplitArbConfig {
    fn default() -> Self {
        Self {
            base: SplitArbConfig {
                max_entry_price: dec!(0.45),   // Sports markets often tighter
                target_total_cost: dec!(0.92), // Less profit margin
                min_profit_margin: dec!(0.03), // 3¢ minimum
                max_hedge_wait_secs: 3600,     // 1 hour (games are longer)
                shares_per_trade: 100,
                max_unhedged_positions: 5,
                unhedged_stop_loss: dec!(0.20),
            },
            leagues: vec![SportsLeague::NBA, SportsLeague::NFL],
        }
    }
}

/// Run sports split arbitrage strategy
pub async fn run_sports_split_arb(
    client: PolymarketClient,
    executor: OrderExecutor,
    config: SportsSplitArbConfig,
    dry_run: bool,
) -> Result<()> {
    info!("Starting sports split arbitrage strategy");
    info!("Monitoring leagues: {:?}", config.leagues);

    // Print config banner
    println!("\n\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║         SPORTS SPLIT ARBITRAGE (運動市場套利)                 ║\x1b[0m");
    println!("\x1b[36m╠══════════════════════════════════════════════════════════════╣\x1b[0m");
    println!(
        "\x1b[36m║\x1b[0m  Leagues:           {:?}                            \x1b[36m║\x1b[0m",
        config.leagues
    );
    println!("\x1b[36m║\x1b[0m  Max Entry Price:   {}¢                                     \x1b[36m║\x1b[0m",
             config.base.max_entry_price * dec!(100));
    println!("\x1b[36m║\x1b[0m  Target Total Cost: {}¢                                    \x1b[36m║\x1b[0m",
             config.base.target_total_cost * dec!(100));
    println!(
        "\x1b[36m║\x1b[0m  Mode:              {}                                \x1b[36m║\x1b[0m",
        if dry_run { "DRY RUN" } else { "LIVE" }
    );
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

    // Create discovery
    let discovery = SportsMarketDiscovery::with_leagues(client.clone(), config.leagues.clone());

    // Discover markets
    let markets = discovery.discover_markets().await?;
    info!("Monitoring {} sports markets", markets.len());

    if markets.is_empty() {
        warn!("No sports markets found to monitor!");
        warn!("Sports market discovery is not fully implemented yet.");
        warn!("This is a placeholder for future development.");
        return Ok(());
    }

    // Create engine
    let engine = Arc::new(SplitArbEngine::new(config.base, client, executor, dry_run));

    // Add markets to engine
    let token_ids: Vec<String> = markets
        .iter()
        .flat_map(|m| vec![m.yes_token_id.clone(), m.no_token_id.clone()])
        .collect();

    engine.add_markets(markets).await;

    info!("Found {} tokens to monitor", token_ids.len());

    // Connect to WebSocket
    info!("Sports Split Arbitrage Engine started");

    let ws_url = "wss://ws-subscriptions-clob.polymarket.com/ws/market";
    let ws = PolymarketWebSocket::new(ws_url);

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

    // Main loop
    loop {
        match update_rx.recv().await {
            Ok(quote_update) => {
                engine
                    .on_price_update(
                        &quote_update.token_id,
                        quote_update.quote.best_bid,
                        quote_update.quote.best_ask,
                    )
                    .await;
            }
            Err(e) => {
                warn!("Update channel error: {}", e);
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

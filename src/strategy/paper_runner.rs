//! Paper Trading Runner for Volatility Arbitrage Strategy
//!
//! Connects to real Polymarket WebSocket and Binance for live data,
//! but only records signals without executing trades.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use chrono::Utc;
use rust_decimal::prelude::*;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use crate::adapters::{BinanceWebSocket, PolymarketClient, PolymarketWebSocket};
use crate::collector::BinanceKlineClient;
use crate::strategy::core::{BinaryMarket, MarketDiscovery};
use crate::strategy::{CryptoMarketDiscovery, PaperTrader, PaperTradingStats, VolatilityArbConfig};

/// Configuration for paper trading runner
#[derive(Debug, Clone)]
pub struct PaperTradingConfig {
    /// Volatility arbitrage config
    pub vol_arb_config: VolatilityArbConfig,
    /// Symbols to monitor (e.g., ["BTCUSDT", "ETHUSDT"])
    pub symbols: Vec<String>,
    /// Series IDs to monitor on Polymarket
    pub series_ids: Vec<String>,
    /// How often to update K-line volatility (seconds)
    pub kline_update_interval_secs: u64,
    /// How often to print stats (seconds)
    pub stats_interval_secs: u64,
    /// Log file path for signals
    pub log_file: Option<String>,
}

impl Default for PaperTradingConfig {
    fn default() -> Self {
        Self {
            vol_arb_config: VolatilityArbConfig::default(),
            symbols: vec!["BTCUSDT".into(), "ETHUSDT".into(), "SOLUSDT".into()],
            series_ids: vec![
                "btc-price-series-15m".into(),
                "eth-price-series-15m".into(),
                "sol-price-series-15m".into(),
            ],
            kline_update_interval_secs: 60, // Update volatility every minute
            stats_interval_secs: 300,       // Print stats every 5 minutes
            log_file: Some("./data/paper_signals.json".into()),
        }
    }
}

/// Tracked market info for paper trading
#[derive(Debug, Clone)]
pub struct TrackedMarket {
    pub market: BinaryMarket,
    pub symbol: String,
    pub threshold_price: Decimal,
    pub yes_token_id: String,
    pub no_token_id: String,
}

/// Paper trading runner state
pub struct PaperTradingRunner {
    config: PaperTradingConfig,
    paper_trader: Arc<RwLock<PaperTrader>>,
    tracked_markets: Arc<RwLock<HashMap<String, TrackedMarket>>>,
}

impl PaperTradingRunner {
    pub fn new(config: PaperTradingConfig) -> Self {
        let paper_trader = PaperTrader::new(config.vol_arb_config.clone(), config.log_file.clone());

        Self {
            config,
            paper_trader: Arc::new(RwLock::new(paper_trader)),
            tracked_markets: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Run the paper trading system
    pub async fn run(self, pm_client: PolymarketClient) -> Result<()> {
        info!("Starting paper trading runner");

        // Print banner
        println!(
            "\n\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m"
        );
        println!("\x1b[36m║         VOLATILITY ARBITRAGE - PAPER TRADING                 ║\x1b[0m");
        println!("\x1b[36m╠══════════════════════════════════════════════════════════════╣\x1b[0m");
        println!(
            "\x1b[36m║\x1b[0m  Symbols: {:49}\x1b[36m║\x1b[0m",
            self.config.symbols.join(", ")
        );
        println!("\x1b[36m║\x1b[0m  Min Vol Edge: {:.1}%                                        \x1b[36m║\x1b[0m",
                 self.config.vol_arb_config.min_vol_edge_pct * 100.0);
        println!("\x1b[36m║\x1b[0m  Min Price Edge: {}¢                                        \x1b[36m║\x1b[0m",
                 self.config.vol_arb_config.min_price_edge * dec!(100));
        println!("\x1b[36m║\x1b[0m  Mode: PAPER TRADING (signals only)                         \x1b[36m║\x1b[0m");
        println!(
            "\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n"
        );

        // Discover markets
        let discovery =
            CryptoMarketDiscovery::with_series(pm_client.clone(), self.config.series_ids.clone());

        let markets = discovery.discover_markets().await?;
        info!("Discovered {} markets to monitor", markets.len());

        if markets.is_empty() {
            warn!("No markets found to monitor!");
            return Ok(());
        }

        // Build token -> market mapping and collect token IDs
        let mut token_ids: Vec<String> = Vec::new();
        {
            let mut tracked = self.tracked_markets.write().await;
            for market in &markets {
                // Parse threshold price from market metadata or labels
                let threshold = self.parse_threshold_price(market);
                let symbol = self.extract_symbol(market);

                let tracked_market = TrackedMarket {
                    market: market.clone(),
                    symbol: symbol.clone(),
                    threshold_price: threshold,
                    yes_token_id: market.yes_token_id.clone(),
                    no_token_id: market.no_token_id.clone(),
                };

                // Map both token IDs to this market
                tracked.insert(market.yes_token_id.clone(), tracked_market.clone());
                tracked.insert(market.no_token_id.clone(), tracked_market);

                token_ids.push(market.yes_token_id.clone());
                token_ids.push(market.no_token_id.clone());
            }
        }

        info!(
            "Monitoring {} tokens across {} markets",
            token_ids.len(),
            markets.len()
        );

        // Initialize K-line client for volatility
        let kline_client = BinanceKlineClient::new();
        info!("Initializing K-line volatility data...");
        let _ = kline_client.initialize_symbols(&self.config.symbols).await;

        // Update paper trader with initial volatility
        {
            let mut trader = self.paper_trader.write().await;
            for symbol in &self.config.symbols {
                if let Some(vol) = kline_client.get_15m_volatility(symbol).await {
                    let vol_f64 = vol.to_f64().unwrap_or(0.003);
                    trader.update_volatility(symbol, vol_f64);
                    info!("{} initial volatility: {:.4}%", symbol, vol_f64 * 100.0);
                }
            }
        }

        // Create WebSocket connections
        let pm_ws =
            PolymarketWebSocket::new("wss://ws-subscriptions-clob.polymarket.com/ws/market");

        // Binance WS needs lowercase symbols
        let binance_symbols: Vec<String> = self
            .config
            .symbols
            .iter()
            .map(|s| s.to_lowercase())
            .collect();
        let binance_ws = Arc::new(BinanceWebSocket::new(binance_symbols.clone()));

        // Get update channels
        let mut pm_update_rx = pm_ws.subscribe_updates();
        let mut binance_update_rx = binance_ws.subscribe();
        let _pm_quote_cache = pm_ws.quote_cache();
        let binance_price_cache = binance_ws.price_cache().clone();

        // Clone for background tasks
        let paper_trader = Arc::clone(&self.paper_trader);
        let tracked_markets = Arc::clone(&self.tracked_markets);
        let symbols = self.config.symbols.clone();
        let kline_interval = self.config.kline_update_interval_secs;

        // Spawn K-line volatility updater
        let paper_trader_kline = Arc::clone(&paper_trader);
        let symbols_kline = symbols.clone();
        tokio::spawn(async move {
            let kline_client = BinanceKlineClient::new();
            let mut interval = tokio::time::interval(Duration::from_secs(kline_interval));
            loop {
                interval.tick().await;
                debug!("Updating K-line volatility...");

                for symbol in &symbols_kline {
                    if let Err(e) = kline_client.update_volatility_stats(symbol).await {
                        warn!("Failed to update volatility for {}: {}", symbol, e);
                        continue;
                    }

                    if let Some(vol) = kline_client.get_15m_volatility(symbol).await {
                        let vol_f64 = vol.to_f64().unwrap_or(0.003);
                        let mut trader = paper_trader_kline.write().await;
                        trader.update_volatility(symbol, vol_f64);
                        debug!("{} volatility updated: {:.4}%", symbol, vol_f64 * 100.0);
                    }
                }
            }
        });

        // Spawn stats printer
        let paper_trader_stats = Arc::clone(&paper_trader);
        let stats_interval = self.config.stats_interval_secs;
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(stats_interval));
            loop {
                interval.tick().await;
                let trader = paper_trader_stats.read().await;
                let stats = trader.statistics();
                Self::print_stats(&stats);
            }
        });

        // Spawn PM WebSocket runner
        let token_ids_clone = token_ids.clone();
        tokio::spawn(async move {
            if let Err(e) = pm_ws.run(token_ids_clone).await {
                error!("Polymarket WebSocket error: {}", e);
            }
        });

        // Spawn Binance WebSocket runner
        let binance_ws_runner = Arc::clone(&binance_ws);
        tokio::spawn(async move {
            if let Err(e) = binance_ws_runner.run().await {
                error!("Binance WebSocket error: {}", e);
            }
        });

        info!("Paper trading runner started, waiting for quotes...");

        // Main loop - process PM quote updates
        loop {
            tokio::select! {
                // Process PM quote updates
                pm_result = pm_update_rx.recv() => {
                    match pm_result {
                        Ok(quote_update) => {
                            // Get tracked market for this token
                            let tracked = tracked_markets.read().await;
                            if let Some(market_info) = tracked.get(&quote_update.token_id) {
                                // Get current spot price from Binance
                                let spot_symbol = market_info.symbol.to_lowercase();

                                let spot_price = binance_price_cache.get(&spot_symbol).await
                                    .map(|sp| sp.price)
                                    .unwrap_or(Decimal::ZERO);

                                if spot_price > Decimal::ZERO {
                                    // Calculate time remaining
                                    let now = Utc::now();
                                    let time_remaining = if market_info.market.end_time > now {
                                        (market_info.market.end_time - now).num_seconds() as u64
                                    } else {
                                        0
                                    };

                                    // Skip expired markets
                                    if time_remaining == 0 {
                                        continue;
                                    }

                                    // Get tick volatility from Binance (use 60s lookback)
                                    let tick_vol = binance_price_cache.volatility(&spot_symbol, 60).await
                                        .map(|d| d.to_f64().unwrap_or(0.0));

                                    // Get bid/ask prices, skip if not available
                                    let yes_bid = match quote_update.quote.best_bid {
                                        Some(b) => b,
                                        None => continue,
                                    };
                                    let yes_ask = match quote_update.quote.best_ask {
                                        Some(a) => a,
                                        None => continue,
                                    };

                                    // Check for signal
                                    let mut trader = paper_trader.write().await;
                                    if let Some(signal) = trader.check_and_record(
                                        &market_info.symbol,
                                        &market_info.market.event_id,
                                        &market_info.market.condition_id,
                                        spot_price,
                                        market_info.threshold_price,
                                        yes_bid,
                                        yes_ask,
                                        time_remaining,
                                        tick_vol,
                                    ) {
                                        info!(
                                            "📝 Signal: {} {} @ {} (edge: {:.2}%, vol_edge: {:.2}%)",
                                            signal.symbol,
                                            signal.direction,
                                            signal.entry_price,
                                            signal.price_edge.to_f64().unwrap_or(0.0) * 100.0,
                                            signal.vol_edge_pct * 100.0,
                                        );
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            warn!("PM update channel error: {}", e);
                            tokio::time::sleep(Duration::from_secs(1)).await;
                        }
                    }
                }

                // Process Binance price updates (just for logging)
                binance_result = binance_update_rx.recv() => {
                    match binance_result {
                        Ok(price_update) => {
                            debug!("{} spot: ${}", price_update.symbol, price_update.price);
                        }
                        Err(e) => {
                            warn!("Binance update channel error: {}", e);
                        }
                    }
                }
            }
        }
    }

    /// Parse threshold price from market
    fn parse_threshold_price(&self, market: &BinaryMarket) -> Decimal {
        // Try to parse from yes_label (e.g., "BTC above $94,000")
        let label = &market.yes_label;

        // Look for dollar amount pattern
        if let Some(idx) = label.find('$') {
            let price_str: String = label[idx + 1..]
                .chars()
                .filter(|c| c.is_numeric() || *c == '.' || *c == ',')
                .collect();

            let clean_price: String = price_str.replace(',', "");
            if let Ok(price) = Decimal::from_str(&clean_price) {
                return price;
            }
        }

        // Default based on symbol (rough estimates)
        match market.yes_label.to_uppercase() {
            l if l.contains("BTC") => dec!(95000),
            l if l.contains("ETH") => dec!(3400),
            l if l.contains("SOL") => dec!(200),
            _ => dec!(0),
        }
    }

    /// Extract symbol from market
    fn extract_symbol(&self, market: &BinaryMarket) -> String {
        let label = market.yes_label.to_uppercase();

        if label.contains("BTC") || label.contains("BITCOIN") {
            "BTCUSDT".into()
        } else if label.contains("ETH") || label.contains("ETHEREUM") {
            "ETHUSDT".into()
        } else if label.contains("SOL") || label.contains("SOLANA") {
            "SOLUSDT".into()
        } else if label.contains("XRP") {
            "XRPUSDT".into()
        } else {
            "UNKNOWN".into()
        }
    }

    /// Print trading statistics
    fn print_stats(stats: &PaperTradingStats) {
        println!(
            "\n\x1b[33m╔══════════════════════════════════════════════════════════════╗\x1b[0m"
        );
        println!(
            "\x1b[33m║               PAPER TRADING STATISTICS                        ║\x1b[0m"
        );
        println!("\x1b[33m╠══════════════════════════════════════════════════════════════╣\x1b[0m");
        println!("\x1b[33m║\x1b[0m  Total Signals:     {:>10}                              \x1b[33m║\x1b[0m", 
                 stats.total_signals);
        println!("\x1b[33m║\x1b[0m  Winning Signals:   {:>10}                              \x1b[33m║\x1b[0m",
                 stats.winning_signals);
        println!("\x1b[33m║\x1b[0m  Win Rate:          {:>9.1}%                              \x1b[33m║\x1b[0m",
                 stats.win_rate * 100.0);
        println!("\x1b[33m║\x1b[0m  Theoretical PnL:   ${:>9}                              \x1b[33m║\x1b[0m",
                 stats.theoretical_pnl);
        println!("\x1b[33m║\x1b[0m  Avg Vol Edge:      {:>9.2}%                              \x1b[33m║\x1b[0m",
                 stats.avg_vol_edge * 100.0);
        println!("\x1b[33m║\x1b[0m  Avg Confidence:    {:>9.2}%                              \x1b[33m║\x1b[0m",
                 stats.avg_confidence * 100.0);
        println!("\x1b[33m║\x1b[0m  Pending Signals:   {:>10}                              \x1b[33m║\x1b[0m",
                 stats.pending_signals);
        println!(
            "\x1b[33m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n"
        );
    }

    /// Get current statistics
    pub async fn statistics(&self) -> PaperTradingStats {
        self.paper_trader.read().await.statistics()
    }
}

/// Run paper trading with default config
pub async fn run_paper_trading(
    pm_client: PolymarketClient,
    config: Option<PaperTradingConfig>,
) -> Result<()> {
    let config = config.unwrap_or_default();
    let runner = PaperTradingRunner::new(config);
    runner.run(pm_client).await
}

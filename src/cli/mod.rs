//! Ploy CLI - Unified trading system management
//!
//! Commands:
//! - `ploy strategy` - Manage trading strategies
//! - `ploy service` - Manage core services
//! - `ploy config` - Configuration management
//! - `ploy infra` - Infrastructure management

pub mod strategy;
pub mod service;
pub mod config;
pub mod infra;
pub mod legacy;

use clap::{Parser, Subcommand};
use anyhow::Result;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// Ploy Trading System CLI
#[derive(Parser, Debug)]
#[command(name = "ploy")]
#[command(author, version, about = "Professional trading system for prediction markets")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Manage trading strategies
    #[command(subcommand)]
    Strategy(strategy::StrategyCommands),

    /// Manage core services (market data, executor, risk)
    #[command(subcommand)]
    Service(service::ServiceCommands),

    /// Configuration management
    #[command(subcommand)]
    Config(config::ConfigCommands),

    /// Infrastructure management (deploy, status, ssh)
    #[command(subcommand)]
    Infra(infra::InfraCommands),

    // === Legacy commands for backward compatibility ===

    /// [Legacy] Run momentum strategy directly
    Momentum {
        #[arg(short, long, default_value = "BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT")]
        symbols: String,
        #[arg(long, default_value = "0.5")]
        min_move: f64,
        #[arg(long, default_value = "45")]
        max_entry: f64,
        #[arg(long, default_value = "5")]
        min_edge: f64,
        #[arg(long, default_value = "100")]
        shares: u64,
        #[arg(long, default_value = "5")]
        max_positions: usize,
        #[arg(long, default_value = "20")]
        take_profit: f64,
        #[arg(long, default_value = "12")]
        stop_loss: f64,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        predictive: bool,
        #[arg(long, default_value = "300")]
        min_time: u64,
        #[arg(long, default_value = "900")]
        max_time: u64,
    },

    /// [Legacy] Test API connectivity
    Test,

    /// [Legacy] Show account info
    Account {
        #[arg(long)]
        orders: bool,
        #[arg(long)]
        positions: bool,
    },

    /// Run volatility arbitrage in paper trading mode
    Paper {
        /// Symbols to monitor (comma-separated)
        #[arg(short, long, default_value = "BTCUSDT,ETHUSDT,SOLUSDT")]
        symbols: String,

        /// Minimum volatility edge percentage
        #[arg(long, default_value = "5.0")]
        min_vol_edge: f64,

        /// Minimum price edge in cents
        #[arg(long, default_value = "2.0")]
        min_price_edge: f64,

        /// Log file path
        #[arg(long, default_value = "./data/paper_signals.json")]
        log_file: String,

        /// Stats print interval (seconds)
        #[arg(long, default_value = "300")]
        stats_interval: u64,
    },
}

impl Cli {
    pub async fn run(self) -> Result<()> {
        match self.command {
            Commands::Strategy(cmd) => cmd.run().await,
            Commands::Service(cmd) => cmd.run().await,
            Commands::Config(cmd) => cmd.run().await,
            Commands::Infra(cmd) => cmd.run().await,
            // Legacy commands delegate to old implementation
            Commands::Momentum { .. } => {
                println!("Use 'ploy strategy start momentum' instead");
                println!("Or run with legacy mode: ploy --legacy momentum ...");
                Ok(())
            }
            Commands::Test => {
                println!("Use 'ploy service status' instead");
                Ok(())
            }
            Commands::Account { .. } => {
                println!("Use 'ploy strategy status' or 'ploy service status' instead");
                Ok(())
            }
            Commands::Paper {
                symbols,
                min_vol_edge,
                min_price_edge,
                log_file,
                stats_interval,
            } => {
                use crate::adapters::PolymarketClient;
                use crate::strategy::{PaperTradingConfig, run_paper_trading, VolatilityArbConfig};

                // Parse symbols
                let symbols: Vec<String> = symbols
                    .split(',')
                    .map(|s| s.trim().to_uppercase())
                    .collect();

                // Build series IDs from symbols
                let series_ids: Vec<String> = symbols.iter()
                    .filter_map(|s| {
                        match s.trim_end_matches("USDT") {
                            "BTC" => Some("btc-price-series-15m".into()),
                            "ETH" => Some("eth-price-series-15m".into()),
                            "SOL" => Some("sol-price-series-15m".into()),
                            _ => None,
                        }
                    })
                    .collect();

                let mut vol_arb_config = VolatilityArbConfig::default();
                vol_arb_config.min_vol_edge_pct = min_vol_edge / 100.0;
                vol_arb_config.min_price_edge = Decimal::from_f64_retain(min_price_edge / 100.0)
                    .unwrap_or(dec!(0.02));
                vol_arb_config.symbols = symbols.clone();

                let config = PaperTradingConfig {
                    vol_arb_config,
                    symbols,
                    series_ids,
                    kline_update_interval_secs: 60,
                    stats_interval_secs: stats_interval,
                    log_file: Some(log_file),
                };

                let pm_client = PolymarketClient::new("https://clob.polymarket.com", true)?;
                run_paper_trading(pm_client, Some(config)).await
            }
        }
    }
}

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
        }
    }
}

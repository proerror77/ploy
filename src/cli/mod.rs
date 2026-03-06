//! Ploy management CLI (non-runtime entrypoints).

pub mod config;
pub mod infra;
pub mod pm;
pub mod rpc;
pub mod runtime;
pub mod service;
pub mod strategy;

use anyhow::Result;
use clap::{Parser, Subcommand};

/// Ploy management CLI
#[derive(Parser, Debug)]
#[command(name = "ploy")]
#[command(
    author,
    version,
    about = "Professional trading system for prediction markets"
)]
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
}

impl Cli {
    pub async fn run(self) -> Result<()> {
        match self.command {
            Commands::Strategy(cmd) => cmd.run().await,
            Commands::Service(cmd) => cmd.run().await,
            Commands::Config(cmd) => cmd.run().await,
            Commands::Infra(cmd) => cmd.run().await,
        }
    }
}

use clap::Parser;
use ploy::cli::runtime::Cli;
use ploy::error::Result;

mod main_agent_mode;
mod main_commands;
mod main_dispatch;
mod main_modes;
mod main_runtime;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    main_dispatch::run(&cli).await
}

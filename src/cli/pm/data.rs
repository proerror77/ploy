//! `ploy pm data` — Data API (positions, trades, analytics, leaderboard).

use clap::Subcommand;

use super::auth::PmAuth;
use super::output::{self, OutputMode};

#[derive(Subcommand, Debug, Clone)]
pub enum DataCommands {
    /// Get positions for an address.
    Positions {
        #[arg(long)]
        address: Option<String>,
        #[arg(long, default_value = "100")]
        limit: i32,
    },
    /// Get closed positions for an address.
    ClosedPositions {
        #[arg(long)]
        address: Option<String>,
        #[arg(long, default_value = "100")]
        limit: i32,
    },
    /// Get trades for an address.
    Trades {
        #[arg(long)]
        address: Option<String>,
        #[arg(long, default_value = "50")]
        limit: i32,
    },
    /// Get activity for an address.
    Activity {
        #[arg(long)]
        address: Option<String>,
        #[arg(long, default_value = "50")]
        limit: i32,
    },
    /// Get token holders for a market.
    Holders { condition_id: String },
    /// Get portfolio value over time.
    Value {
        #[arg(long)]
        address: Option<String>,
    },
    /// Get open interest for a market.
    OpenInterest { condition_id: String },
    /// Get leaderboard.
    Leaderboard {
        #[arg(long, default_value = "25")]
        limit: i32,
    },
}

pub async fn run(cmd: DataCommands, auth: &PmAuth, mode: OutputMode) -> anyhow::Result<()> {
    use polymarket_client_sdk::data::Client as DataClient;
    use polymarket_client_sdk::data::types::request::*;

    let config = super::config_file::PmConfig::load().unwrap_or_default();
    let data = DataClient::new(config.clob_base_url())?;

    let resolve_addr = |explicit: Option<String>| -> anyhow::Result<alloy::primitives::Address> {
        if let Some(addr) = explicit {
            addr.parse().map_err(|e| anyhow::anyhow!("invalid address: {e}"))
        } else {
            auth.address()
        }
    };

    match cmd {
        DataCommands::Positions { address, limit } => {
            let addr = resolve_addr(address)?;
            let req = PositionsRequest::builder()
                .user(addr)
                .limit(limit)?
                .build();
            let positions = data.positions(&req).await?;
            output::print_debug_items(&positions, mode)?;
        }
        DataCommands::ClosedPositions { address, limit } => {
            let addr = resolve_addr(address)?;
            let req = ClosedPositionsRequest::builder()
                .user(addr)
                .limit(limit)?
                .build();
            let positions = data.closed_positions(&req).await?;
            output::print_debug_items(&positions, mode)?;
        }
        DataCommands::Trades { address, limit } => {
            let addr = resolve_addr(address)?;
            let req = TradesRequest::builder()
                .user(addr)
                .limit(limit)?
                .build();
            let trades = data.trades(&req).await?;
            output::print_debug_items(&trades, mode)?;
        }
        DataCommands::Activity { address, limit } => {
            let addr = resolve_addr(address)?;
            let req = ActivityRequest::builder()
                .user(addr)
                .limit(limit)?
                .build();
            let activity = data.activity(&req).await?;
            output::print_debug_items(&activity, mode)?;
        }
        DataCommands::Holders { condition_id } => {
            let cid: alloy::primitives::B256 = condition_id.parse()
                .map_err(|e| anyhow::anyhow!("invalid condition_id: {e}"))?;
            let req = HoldersRequest::builder().markets(vec![cid]).build();
            let holders = data.holders(&req).await?;
            output::print_debug_items(&holders, mode)?;
        }
        DataCommands::Value { address } => {
            let addr = resolve_addr(address)?;
            let req = ValueRequest::builder().user(addr).build();
            let value = data.value(&req).await?;
            output::print_debug_items(&value, mode)?;
        }
        DataCommands::OpenInterest { condition_id } => {
            let cid: alloy::primitives::B256 = condition_id.parse()
                .map_err(|e| anyhow::anyhow!("invalid condition_id: {e}"))?;
            let req = OpenInterestRequest::builder()
                .markets(vec![cid])
                .build();
            let oi = data.open_interest(&req).await?;
            output::print_debug_items(&oi, mode)?;
        }
        DataCommands::Leaderboard { limit } => {
            let req = TraderLeaderboardRequest::builder().limit(limit)?.build();
            let lb = data.leaderboard(&req).await?;
            output::print_debug_items(&lb, mode)?;
        }
    }
    Ok(())
}

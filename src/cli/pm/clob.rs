//! `ploy pm clob` — CLOB (Central Limit Order Book) API commands.

use clap::Subcommand;

use super::auth::PmAuth;
use super::output::{self, OutputMode};

#[derive(Subcommand, Debug, Clone)]
pub enum ClobCommands {
    /// Check CLOB API health.
    Health,
    /// Get server time.
    Time,
    /// Get midpoint price for a token.
    Midpoint { token_id: String },
    /// Get price for a token on a given side.
    Price {
        token_id: String,
        #[arg(long, default_value = "BUY")]
        side: String,
    },
    /// Get spread for a token.
    Spread { token_id: String },
    /// Get order book for a token.
    Book { token_id: String },
    /// Get last trade price for a token.
    LastTrade { token_id: String },
    /// Get market info from CLOB.
    Market { condition_id: String },
    /// Check if a market uses negative risk.
    NegRisk { token_id: String },
    /// Get tick size for a token.
    TickSize { token_id: String },
    /// Get price history for a token.
    PriceHistory {
        token_id: String,
        /// Interval: 1m, 1h, 6h, 1d, 1w, max
        #[arg(long, default_value = "1d")]
        interval: String,
        #[arg(long)]
        fidelity: Option<u32>,
    },
}

pub async fn run(cmd: ClobCommands, _auth: &PmAuth, mode: OutputMode) -> anyhow::Result<()> {
    use alloy::primitives::U256;
    use polymarket_client_sdk::clob::Client as ClobClient;
    use polymarket_client_sdk::clob::types::request::*;
    use polymarket_client_sdk::clob::types::{Interval, Side, TimeRange};
    use std::str::FromStr;

    let config = super::config_file::PmConfig::load().unwrap_or_default();
    let clob = ClobClient::new(
        config.clob_base_url(),
        polymarket_client_sdk::clob::Config::default(),
    )?;

    match cmd {
        ClobCommands::Health => {
            let health = clob.ok().await?;
            output::print_kv("health", &health);
        }
        ClobCommands::Time => {
            let time = clob.server_time().await?;
            output::print_kv("server_time", &time.to_string());
        }
        ClobCommands::Midpoint { token_id } => {
            let tid = U256::from_str(&token_id)
                .map_err(|e| anyhow::anyhow!("invalid token_id: {e}"))?;
            let req = MidpointRequest::builder().token_id(tid).build();
            let mid = clob.midpoint(&req).await?;
            output::print_debug(&mid, mode)?;
        }
        ClobCommands::Price { token_id, side } => {
            let tid = U256::from_str(&token_id)
                .map_err(|e| anyhow::anyhow!("invalid token_id: {e}"))?;
            let sdk_side = match side.to_uppercase().as_str() {
                "SELL" => Side::Sell,
                _ => Side::Buy,
            };
            let req = PriceRequest::builder().token_id(tid).side(sdk_side).build();
            let price = clob.price(&req).await?;
            output::print_debug(&price, mode)?;
        }
        ClobCommands::Spread { token_id } => {
            let tid = U256::from_str(&token_id)
                .map_err(|e| anyhow::anyhow!("invalid token_id: {e}"))?;
            let req = SpreadRequest::builder().token_id(tid).build();
            let spread = clob.spread(&req).await?;
            output::print_debug(&spread, mode)?;
        }
        ClobCommands::Book { token_id } => {
            let tid = U256::from_str(&token_id)
                .map_err(|e| anyhow::anyhow!("invalid token_id: {e}"))?;
            let req = OrderBookSummaryRequest::builder().token_id(tid).build();
            let book = clob.order_book(&req).await?;
            output::print_debug(&book, mode)?;
        }
        ClobCommands::LastTrade { token_id } => {
            let tid = U256::from_str(&token_id)
                .map_err(|e| anyhow::anyhow!("invalid token_id: {e}"))?;
            let req = LastTradePriceRequest::builder().token_id(tid).build();
            let trade = clob.last_trade_price(&req).await?;
            output::print_debug(&trade, mode)?;
        }
        ClobCommands::Market { condition_id } => {
            let market = clob.market(&condition_id).await?;
            output::print_debug(&market, mode)?;
        }
        ClobCommands::NegRisk { token_id } => {
            let tid = U256::from_str(&token_id)
                .map_err(|e| anyhow::anyhow!("invalid token_id: {e}"))?;
            let neg_risk = clob.neg_risk(tid).await?;
            output::print_debug(&neg_risk, mode)?;
        }
        ClobCommands::TickSize { token_id } => {
            let tid = U256::from_str(&token_id)
                .map_err(|e| anyhow::anyhow!("invalid token_id: {e}"))?;
            let tick = clob.tick_size(tid).await?;
            output::print_debug(&tick, mode)?;
        }
        ClobCommands::PriceHistory { token_id, interval, fidelity } => {
            let tid = U256::from_str(&token_id)
                .map_err(|e| anyhow::anyhow!("invalid token_id: {e}"))?;
            let iv = match interval.as_str() {
                "1m" => Interval::OneMinute,
                "1h" => Interval::OneHour,
                "6h" => Interval::SixHours,
                "1d" => Interval::OneDay,
                "1w" => Interval::OneWeek,
                "max" => Interval::Max,
                other => anyhow::bail!("invalid interval '{other}': expected 1m, 1h, 6h, 1d, 1w, or max"),
            };
            let time_range = TimeRange::from_interval(iv);
            let req = PriceHistoryRequest::builder()
                .market(tid)
                .time_range(time_range)
                .maybe_fidelity(fidelity)
                .build();
            let history = clob.price_history(&req).await?;
            output::print_debug(&history, mode)?;
        }
    }
    Ok(())
}

use crate::main_runtime::enforce_coordinator_only_live;
use ploy::adapters::PolymarketClient;
use ploy::cli::runtime::SportsCommands;
use ploy::error::Result;
use ploy::strategy::OrderExecutor;
use tracing::info;

pub(crate) async fn run_sports_command(cmd: &SportsCommands) -> Result<()> {
    use ploy::adapters::polymarket_clob::POLYGON_CHAIN_ID;
    use ploy::signing::Wallet;
    use ploy::strategy::{
        core::SplitArbConfig, run_sports_split_arb, SportsLeague, SportsSplitArbConfig,
    };
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use std::str::FromStr;

    match cmd {
        SportsCommands::SplitArb {
            max_entry,
            target_cost,
            min_profit,
            max_wait,
            shares,
            max_unhedged,
            stop_loss,
            leagues,
            dry_run,
        } => {
            info!("Starting sports split-arb strategy");
            if !*dry_run {
                enforce_coordinator_only_live("ploy sports split-arb")?;
            }

            let league_list: Vec<SportsLeague> = leagues
                .split(',')
                .filter_map(|l| match l.trim().to_uppercase().as_str() {
                    "NBA" => Some(SportsLeague::NBA),
                    "NFL" => Some(SportsLeague::NFL),
                    "MLB" => Some(SportsLeague::MLB),
                    "NHL" => Some(SportsLeague::NHL),
                    "SOCCER" => Some(SportsLeague::Soccer),
                    "UFC" => Some(SportsLeague::UFC),
                    _ => None,
                })
                .collect();

            let config = SportsSplitArbConfig {
                base: SplitArbConfig {
                    max_entry_price: Decimal::from_str(&format!("{:.6}", max_entry / 100.0))
                        .unwrap_or(dec!(0.45)),
                    target_total_cost: Decimal::from_str(&format!("{:.6}", target_cost / 100.0))
                        .unwrap_or(dec!(0.92)),
                    min_profit_margin: Decimal::from_str(&format!("{:.6}", min_profit / 100.0))
                        .unwrap_or(dec!(0.03)),
                    max_hedge_wait_secs: *max_wait,
                    shares_per_trade: *shares,
                    max_unhedged_positions: *max_unhedged,
                    unhedged_stop_loss: Decimal::from_str(&format!("{:.6}", stop_loss / 100.0))
                        .unwrap_or(dec!(0.20)),
                },
                leagues: league_list,
            };

            let client = if *dry_run {
                PolymarketClient::new("https://clob.polymarket.com", true)?
            } else {
                let wallet = Wallet::from_env(POLYGON_CHAIN_ID)?;
                PolymarketClient::new_authenticated(
                    "https://clob.polymarket.com",
                    wallet,
                    true, // neg_risk
                )
                .await?
            };

            let executor = OrderExecutor::new(client.clone(), Default::default());
            run_sports_split_arb(client, executor, config, *dry_run).await?;
        }
    }

    Ok(())
}

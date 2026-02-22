use crate::enforce_coordinator_only_live;
use crate::OrderExecutor;
use crate::PolymarketClient;
use ploy::cli::legacy::CryptoCommands;
use ploy::error::Result;
use tracing::info;

pub(crate) fn map_crypto_coin_to_series_ids(coin_or_series: &str) -> Vec<String> {
    match coin_or_series.trim().to_uppercase().as_str() {
        // Prefer 5m; include 15m as fallback (e.g. ETH 5m can be absent).
        "BTC" => vec!["10684".to_string(), "10192".to_string()], // btc-up-or-down-5m, btc-up-or-down-15m
        "ETH" => vec!["10683".to_string(), "10191".to_string()], // eth-up-or-down-5m, eth-up-or-down-15m
        "SOL" => vec!["10686".to_string(), "10423".to_string()], // sol-up-or-down-5m, sol-up-or-down-15m
        "XRP" => vec!["10685".to_string(), "10422".to_string()], // xrp-up-or-down-5m, xrp-up-or-down-15m
        _ => vec![coin_or_series.trim().to_string()],            // Allow raw series IDs
    }
}

/// Handle crypto subcommands
pub(crate) async fn run_crypto_command(cmd: &CryptoCommands) -> Result<()> {
    use ploy::adapters::polymarket_clob::POLYGON_CHAIN_ID;
    use ploy::signing::Wallet;
    use ploy::strategy::{core::SplitArbConfig, run_crypto_split_arb, CryptoSplitArbConfig};
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use std::str::FromStr;

    match cmd {
        CryptoCommands::SplitArb {
            max_entry,
            target_cost,
            min_profit,
            max_wait,
            shares,
            max_unhedged,
            stop_loss,
            coins,
            dry_run,
        } => {
            info!("Starting crypto split-arb strategy");
            if !*dry_run {
                enforce_coordinator_only_live("ploy crypto split-arb")?;
            }

            // Map coins to series IDs
            let series_ids: Vec<String> = coins
                .split(',')
                .flat_map(map_crypto_coin_to_series_ids)
                .collect();

            // Create config
            let config = CryptoSplitArbConfig {
                base: SplitArbConfig {
                    max_entry_price: Decimal::from_str(&format!("{:.6}", max_entry / 100.0))
                        .unwrap_or(dec!(0.35)),
                    target_total_cost: Decimal::from_str(&format!("{:.6}", target_cost / 100.0))
                        .unwrap_or(dec!(0.95)),
                    min_profit_margin: Decimal::from_str(&format!("{:.6}", min_profit / 100.0))
                        .unwrap_or(dec!(0.05)),
                    max_hedge_wait_secs: *max_wait,
                    shares_per_trade: *shares,
                    max_unhedged_positions: *max_unhedged,
                    unhedged_stop_loss: Decimal::from_str(&format!("{:.6}", stop_loss / 100.0))
                        .unwrap_or(dec!(0.15)),
                },
                series_ids,
            };

            // Initialize client
            let client = if *dry_run {
                PolymarketClient::new("https://clob.polymarket.com", true)?
            } else {
                let wallet = Wallet::from_env(POLYGON_CHAIN_ID)?;
                PolymarketClient::new_authenticated(
                    "https://clob.polymarket.com",
                    wallet,
                    true, // neg_risk for UP/DOWN markets
                )
                .await?
            };

            // Initialize executor with default config
            let executor = OrderExecutor::new(client.clone(), Default::default());

            // Run strategy
            run_crypto_split_arb(client, executor, config, *dry_run).await?;
        }
        CryptoCommands::Monitor { coins } => {
            info!("Monitoring crypto markets: {}", coins);
            // TODO: Implement monitoring mode
            println!("Crypto monitoring mode not yet implemented");
        }
        CryptoCommands::BacktestUpDown {
            symbols,
            days,
            max_events_per_series,
            entry_remaining_secs,
            min_window_move_pcts,
            binance_interval,
            vol_lookback_minutes,
            use_db_prices,
            db_url,
            max_snapshot_age_secs,
        } => {
            use ploy::analysis::updown_backtest::{run_updown_backtest, UpDownBacktestConfig};

            let parse_u64_list = |raw: &str| -> Vec<u64> {
                raw.split(',')
                    .filter_map(|s| s.trim().parse::<u64>().ok())
                    .collect()
            };
            let parse_decimal_list = |raw: &str| -> Vec<Decimal> {
                raw.split(',')
                    .filter_map(|s| s.trim().parse::<Decimal>().ok())
                    .collect()
            };

            let symbols: Vec<String> = symbols
                .split(',')
                .map(|s| s.trim().to_ascii_uppercase())
                .filter(|s| !s.is_empty())
                .collect();

            let cfg = UpDownBacktestConfig {
                symbols,
                lookback_days: *days,
                max_events_per_series: *max_events_per_series,
                entry_remaining_secs: parse_u64_list(entry_remaining_secs),
                min_window_move_pcts: parse_decimal_list(min_window_move_pcts),
                binance_interval: binance_interval.clone(),
                vol_lookback_minutes: *vol_lookback_minutes,
                use_db_prices: *use_db_prices,
                db_url: db_url.clone(),
                max_snapshot_age_secs: *max_snapshot_age_secs,
                ..Default::default()
            };

            run_updown_backtest(cfg).await?;
        }
    }

    Ok(())
}

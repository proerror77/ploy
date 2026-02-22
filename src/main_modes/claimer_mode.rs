use ploy::adapters::PolymarketClient;
use ploy::error::Result;
use tracing::{error, info, warn};

pub async fn run_claimer(check_only: bool, min_size: f64, interval: u64) -> Result<()> {
    use ploy::adapters::polymarket_clob::POLYGON_CHAIN_ID;
    use ploy::signing::Wallet;
    use ploy::strategy::{AutoClaimer, ClaimerConfig};
    use rust_decimal::Decimal;
    use std::str::FromStr;

    info!(
        "Starting auto-claimer (check_only={}, min_size={}, interval={}s)",
        check_only, min_size, interval
    );

    let private_key = std::env::var("POLYMARKET_PRIVATE_KEY")
        .or_else(|_| std::env::var("PRIVATE_KEY"))
        .ok();

    if private_key.is_none() && !check_only {
        warn!("No POLYMARKET_PRIVATE_KEY found - running in check-only mode");
    }

    let wallet = Wallet::from_env(POLYGON_CHAIN_ID)?;

    let funder = std::env::var("POLYMARKET_FUNDER").ok();
    let client = if let Some(ref funder_addr) = funder {
        info!("Using proxy wallet, funder: {}", funder_addr);
        PolymarketClient::new_authenticated_proxy(
            "https://clob.polymarket.com",
            wallet,
            funder_addr,
            false,
        )
        .await?
    } else {
        PolymarketClient::new_authenticated("https://clob.polymarket.com", wallet, false).await?
    };

    let config = ClaimerConfig {
        check_interval_secs: if interval > 0 { interval } else { 60 },
        min_claim_size: Decimal::from_str(&min_size.to_string()).unwrap_or(Decimal::ONE),
        auto_claim: !check_only && private_key.is_some(),
        private_key,
    };

    let claimer = AutoClaimer::new(client, config);

    if interval == 0 {
        info!("One-shot mode: checking for redeemable positions...");
        let positions = claimer.check_once().await?;

        if positions.is_empty() {
            info!("No redeemable positions found");
        } else {
            info!("Found {} redeemable positions:", positions.len());
            for pos in &positions {
                info!(
                    "  • {} {} shares = ${:.2} | condition={}",
                    pos.outcome,
                    pos.size,
                    pos.payout,
                    &pos.condition_id[..16.min(pos.condition_id.len())]
                );
            }

            if !check_only {
                info!("Claiming positions...");
                let results = claimer.check_and_claim().await?;
                for result in results {
                    if result.success {
                        info!(
                            "✅ Claimed ${:.2} from {} | tx: {}",
                            result.amount_claimed, result.condition_id, result.tx_hash
                        );
                    } else {
                        error!(
                            "❌ Failed to claim {}: {:?}",
                            result.condition_id, result.error
                        );
                    }
                }
            }
        }
    } else {
        info!(
            "Starting continuous claiming service (interval: {}s)...",
            interval
        );
        claimer.start().await?;
    }

    Ok(())
}

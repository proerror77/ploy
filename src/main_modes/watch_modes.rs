use ploy::adapters::PolymarketClient;
use ploy::error::Result;
use std::io::{stdout, Write};

pub async fn run_account_mode(show_orders: bool, show_positions: bool) -> Result<()> {
    use ploy::adapters::polymarket_clob::POLYGON_CHAIN_ID;
    use ploy::signing::Wallet;

    let client = match std::env::var("POLYMARKET_PRIVATE_KEY") {
        Ok(_) => {
            println!("  Loading wallet from environment...");
            let wallet = Wallet::from_env(POLYGON_CHAIN_ID)?;
            println!("  Wallet loaded: {:?}", wallet.address());

            let funder = std::env::var("POLYMARKET_FUNDER").ok();
            if let Some(ref funder_addr) = funder {
                println!("  Funder (proxy wallet): {}", funder_addr);
            }

            println!("  Authenticating with Polymarket CLOB...");
            let auth_result = if let Some(funder_addr) = funder {
                PolymarketClient::new_authenticated_proxy(
                    "https://clob.polymarket.com",
                    wallet,
                    &funder_addr,
                    true,
                )
                .await
            } else {
                PolymarketClient::new_authenticated("https://clob.polymarket.com", wallet, true)
                    .await
            };

            match auth_result {
                Ok(client) => {
                    println!("  \x1b[32m✓ Authentication successful\x1b[0m");
                    println!("  Has HMAC auth: {}\n", client.has_hmac_auth());
                    client
                }
                Err(e) => {
                    println!("  \x1b[31m✗ Authentication failed: {}\x1b[0m", e);
                    println!("  Falling back to unauthenticated client...\n");
                    PolymarketClient::new("https://clob.polymarket.com", true)?
                }
            }
        }
        Err(_) => {
            println!("  No POLYMARKET_PRIVATE_KEY found, using unauthenticated client");
            PolymarketClient::new("https://clob.polymarket.com", true)?
        }
    };

    show_account(&client, show_orders, show_positions).await
}

async fn show_account(
    client: &PolymarketClient,
    show_orders: bool,
    show_positions: bool,
) -> Result<()> {
    println!("\x1b[36m");
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║                    POLYMARKET ACCOUNT                        ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!("\x1b[0m");

    print!("  Fetching balance... ");
    stdout().flush()?;

    match client.get_usdc_balance().await {
        Ok(balance) => {
            println!("\x1b[32mOK\x1b[0m");
            println!("\n  \x1b[33m💰 USDC Balance: ${:.2}\x1b[0m\n", balance);
        }
        Err(e) => {
            println!("\x1b[31mFAILED\x1b[0m");
            println!("    Error: {}", e);
            println!("\n  \x1b[31mNote: Balance API requires authentication.\x1b[0m");
            println!("  Make sure POLYMARKET_PRIVATE_KEY is set in environment.\n");
        }
    }

    if show_positions {
        print!("  Fetching positions... ");
        stdout().flush()?;

        match client.get_positions().await {
            Ok(positions) => {
                println!("\x1b[32mOK\x1b[0m");
                if positions.is_empty() {
                    println!("\n  \x1b[33m📊 Positions: None\x1b[0m\n");
                } else {
                    println!("\n  \x1b[33m📊 Positions ({}):\x1b[0m", positions.len());
                    for (i, pos) in positions.iter().enumerate() {
                        let size: f64 = pos.size.parse().unwrap_or(0.0);
                        if size.abs() > 0.0001 {
                            println!(
                                "    {}. Token: {}",
                                i + 1,
                                pos.token_id.as_ref().unwrap_or(&pos.asset_id)
                            );
                            println!("       Size: {} shares", pos.size);
                            if let Some(avg) = &pos.avg_price {
                                println!("       Avg Price: ${}", avg);
                            }
                            if let Some(cur) = &pos.cur_price {
                                println!("       Current Price: ${}", cur);
                            }
                            if let Some(val) = pos.market_value() {
                                println!("       Market Value: \x1b[32m${:.2}\x1b[0m", val);
                            }
                            println!();
                        }
                    }
                }
            }
            Err(e) => {
                println!("\x1b[31mFAILED\x1b[0m");
                println!("    Error: {}", e);
            }
        }
    }

    if show_orders {
        print!("  Fetching open orders... ");
        stdout().flush()?;

        match client.get_open_orders().await {
            Ok(orders) => {
                println!("\x1b[32mOK\x1b[0m");
                if orders.is_empty() {
                    println!("\n  \x1b[33m📋 Open Orders: None\x1b[0m\n");
                } else {
                    println!("\n  \x1b[33m📋 Open Orders ({}):\x1b[0m", orders.len());
                    for (i, order) in orders.iter().enumerate() {
                        println!("    {}. Order ID: {}", i + 1, order.id);
                        println!(
                            "       Token: {}",
                            order.asset_id.as_deref().unwrap_or("N/A")
                        );
                        println!(
                            "       Side: {} @ ${}",
                            order.side.as_deref().unwrap_or("N/A"),
                            order.price.as_deref().unwrap_or("N/A")
                        );
                        println!(
                            "       Size: {} (filled: {})",
                            order.original_size.as_deref().unwrap_or("0"),
                            order.size_matched.as_deref().unwrap_or("0")
                        );
                        println!("       Status: {}", order.status);
                        println!();
                    }
                }
            }
            Err(e) => {
                println!("\x1b[31mFAILED\x1b[0m");
                println!("    Error: {}", e);
            }
        }
    }

    if !show_orders && !show_positions {
        print!("  Fetching account summary... ");
        stdout().flush()?;

        match client.get_account_summary().await {
            Ok(summary) => {
                println!("\x1b[32mOK\x1b[0m\n");
                println!("  \x1b[36m─────────────────────────────────────\x1b[0m");
                println!(
                    "  Total Equity:     \x1b[32m${:.2}\x1b[0m",
                    summary.total_equity
                );
                println!("  USDC Balance:     ${:.2}", summary.usdc_balance);
                println!(
                    "  Position Value:   ${:.2} ({} positions)",
                    summary.position_value, summary.position_count
                );
                println!(
                    "  Open Orders:      ${:.2} ({} orders)",
                    summary.open_order_value, summary.open_order_count
                );
                println!("  \x1b[36m─────────────────────────────────────\x1b[0m\n");
            }
            Err(e) => {
                println!("\x1b[31mFAILED\x1b[0m");
                println!("    Error: {}", e);
            }
        }
    }

    println!("\x1b[90mTip: Use --orders or --positions for detailed views\x1b[0m\n");
    Ok(())
}

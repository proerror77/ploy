//! `ploy pm setup` — Interactive setup wizard.

use super::config_file::PmConfig;
use super::output;

pub async fn run() -> anyhow::Result<()> {
    println!("\x1b[36m");
    println!("╔══════════════════════════════════════════════════════╗");
    println!("║        Polymarket CLI Setup Wizard                   ║");
    println!("╚══════════════════════════════════════════════════════╝");
    println!("\x1b[0m");

    let mut config = PmConfig::load().unwrap_or_default();

    // Step 1: Private key
    println!("Step 1: Configure private key");
    println!("  This key is used to sign orders and transactions.");
    println!("  It will be stored at ~/.config/polymarket/config.json");
    println!();

    if config.private_key.is_some() {
        println!("  A private key is already configured.");
        if !output::confirm("  Replace it?") {
            println!("  Keeping existing key.");
        } else {
            prompt_private_key(&mut config)?;
        }
    } else {
        prompt_private_key(&mut config)?;
    }

    // Step 2: Funder address (optional)
    println!();
    println!("Step 2: Proxy wallet / funder address (optional)");
    println!("  If you use a Polymarket proxy wallet, enter the funder address.");
    println!("  Leave blank to skip.");
    print!("  Funder address: ");
    std::io::Write::flush(&mut std::io::stdout())?;
    let mut funder = String::new();
    std::io::stdin().read_line(&mut funder)?;
    let funder = funder.trim();
    if !funder.is_empty() {
        // Validate
        funder
            .parse::<alloy::primitives::Address>()
            .map_err(|e| anyhow::anyhow!("invalid funder address: {e}"))?;
        config.funder_address = Some(funder.to_string());
    }

    // Step 3: Save
    config.save()?;
    let path = PmConfig::config_path()?;
    output::print_success(&format!("config saved to {}", path.display()));

    // Step 4: Test auth
    println!();
    println!("Step 3: Testing authentication...");

    if let Some(ref key) = config.private_key {
        use alloy::signers::local::PrivateKeySigner;
        use alloy::signers::Signer;
        use std::str::FromStr;

        let hex = key.trim_start_matches("0x");
        match PrivateKeySigner::from_str(hex) {
            Ok(signer) => {
                let signer = signer.with_chain_id(Some(config.chain()));
                let addr = signer.address();
                output::print_success(&format!("wallet address: {addr}"));

                // Try CLOB health check
                match polymarket_client_sdk::clob::Client::new(
                    config.clob_base_url(),
                    polymarket_client_sdk::clob::Config::default(),
                ) {
                    Ok(client) => match client.ok().await {
                        Ok(health) => output::print_success(&format!("CLOB API: {health}")),
                        Err(e) => output::print_warn(&format!("CLOB health check failed: {e}")),
                    },
                    Err(e) => output::print_warn(&format!("failed to create CLOB client: {e}")),
                }
            }
            Err(e) => {
                output::print_error(&format!("invalid private key: {e}"));
            }
        }
    }

    println!();
    output::print_success("setup complete! Try: ploy pm wallet balance");

    Ok(())
}

fn prompt_private_key(config: &mut PmConfig) -> anyhow::Result<()> {
    // Use rpassword to hide input from terminal echo and shoulder surfing
    let key = rpassword::prompt_password("  Private key (hex): ")?;
    let key = key.trim().to_string();

    if key.is_empty() {
        println!("  No key provided, skipping.");
        return Ok(());
    }

    // Validate
    let hex = key.trim_start_matches("0x");
    use alloy::signers::local::PrivateKeySigner;
    use std::str::FromStr;
    PrivateKeySigner::from_str(hex).map_err(|e| anyhow::anyhow!("invalid private key: {e}"))?;

    config.private_key = Some(key);
    Ok(())
}

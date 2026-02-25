//! `ploy pm bridge` — Bridge operations (deposit USDC to Polygon).
//!
//! Uses the Polymarket Bridge API to generate deposit addresses and query
//! supported assets for bridging funds from other chains.

use clap::Subcommand;

use super::auth::PmAuth;
use super::output::{self, OutputMode};

#[derive(Subcommand, Debug, Clone)]
pub enum BridgeCommands {
    /// Get deposit addresses for bridging assets to Polymarket.
    Deposit {
        /// Override wallet address (defaults to signer address).
        #[arg(long)]
        address: Option<String>,
    },
    /// List supported bridge assets and chains.
    SupportedAssets,
    /// Check deposit transaction status.
    Status {
        /// Deposit address to check status for.
        address: String,
    },
}

pub async fn run(cmd: BridgeCommands, auth: &PmAuth, _mode: OutputMode) -> anyhow::Result<()> {
    use polymarket_client_sdk::bridge::Client as BridgeClient;

    let client = BridgeClient::default();

    match cmd {
        BridgeCommands::Deposit { address } => {
            let addr = if let Some(a) = address {
                a.parse::<alloy::primitives::Address>()
                    .map_err(|e| anyhow::anyhow!("invalid address: {e}"))?
            } else {
                auth.address()?
            };

            let req = polymarket_client_sdk::bridge::types::DepositRequest::builder()
                .address(addr)
                .build();

            let resp = client.deposit(&req).await?;
            output::print_kv("evm", &format!("{}", resp.address.evm));
            output::print_kv("svm", &resp.address.svm);
            output::print_kv("btc", &resp.address.btc);
            if let Some(note) = &resp.note {
                output::print_kv("note", note);
            }
        }
        BridgeCommands::SupportedAssets => {
            let resp = client.supported_assets().await?;
            if resp.supported_assets.is_empty() {
                println!("(no supported assets)");
            } else {
                for asset in &resp.supported_assets {
                    println!(
                        "{} ({}) on {} [chain {}] — min ${:.2}",
                        asset.token.name,
                        asset.token.symbol,
                        asset.chain_name,
                        asset.chain_id,
                        asset.min_checkout_usd,
                    );
                }
            }
            if let Some(note) = &resp.note {
                output::print_kv("note", note);
            }
        }
        BridgeCommands::Status { address } => {
            let req = polymarket_client_sdk::bridge::types::StatusRequest::builder()
                .address(address)
                .build();

            let resp = client.status(&req).await?;
            if resp.transactions.is_empty() {
                println!("(no deposit transactions found)");
            } else {
                for tx in &resp.transactions {
                    println!(
                        "chain {} → {} | {} base units | status: {:?}{}",
                        tx.from_chain_id,
                        tx.to_chain_id,
                        tx.from_amount_base_unit,
                        tx.status,
                        tx.tx_hash.as_deref().map(|h| format!(" | tx: {h}")).unwrap_or_default(),
                    );
                }
            }
        }
    }
    Ok(())
}

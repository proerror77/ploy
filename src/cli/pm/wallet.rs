//! `ploy pm wallet` — Wallet and account operations (authenticated).

use clap::Subcommand;

use super::auth::PmAuth;
use super::output::{self, OutputMode};

#[derive(Subcommand, Debug, Clone)]
pub enum WalletCommands {
    /// Show wallet address.
    Address,
    /// Show USDC balance and allowances.
    Balance,
    /// List API keys.
    ApiKeys,
    /// Create a new API key.
    CreateApiKey,
    /// Show notifications.
    Notifications,
}

pub async fn run(cmd: WalletCommands, auth: &PmAuth, mode: OutputMode) -> anyhow::Result<()> {
    use polymarket_client_sdk::clob::Client as ClobClient;
    use polymarket_client_sdk::clob::types::request::*;
    use polymarket_client_sdk::clob::types::AssetType;

    let signer = auth.require_signer()?;
    let config = super::config_file::PmConfig::load().unwrap_or_default();

    match cmd {
        WalletCommands::Address => {
            let addr = signer.address();
            output::print_kv("address", &format!("{addr}"));
            if let Some(funder) = auth.funder {
                output::print_kv("funder", &format!("{funder}"));
            }
        }
        WalletCommands::Balance => {
            let client = ClobClient::new(
                config.clob_base_url(),
                polymarket_client_sdk::clob::Config::default(),
            )?
            .authentication_builder(signer)
            .authenticate()
            .await?;

            let req = BalanceAllowanceRequest::builder()
                .asset_type(AssetType::Collateral)
                .build();
            let bal = client.balance_allowance(req).await?;
            output::print_debug(&bal, mode)?;
        }
        WalletCommands::ApiKeys => {
            let client = ClobClient::new(
                config.clob_base_url(),
                polymarket_client_sdk::clob::Config::default(),
            )?
            .authentication_builder(signer)
            .authenticate()
            .await?;

            let keys = client.api_keys().await?;
            output::print_debug(&keys, mode)?;
        }
        WalletCommands::CreateApiKey => {
            let unauth_client = ClobClient::new(
                config.clob_base_url(),
                polymarket_client_sdk::clob::Config::default(),
            )?;

            let creds = unauth_client.create_or_derive_api_key(signer, None).await?;
            output::print_debug(&creds, mode)?;
        }
        WalletCommands::Notifications => {
            let client = ClobClient::new(
                config.clob_base_url(),
                polymarket_client_sdk::clob::Config::default(),
            )?
            .authentication_builder(signer)
            .authenticate()
            .await?;

            let notifs = client.notifications().await?;
            output::print_debug_items(&notifs, mode)?;
        }
    }
    Ok(())
}

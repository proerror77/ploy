//! `ploy pm approve` — Token approval management.

use clap::Subcommand;

use super::auth::PmAuth;
use super::output::{self, OutputMode};
use super::GlobalPmArgs;

#[derive(Subcommand, Debug, Clone)]
pub enum ApproveCommands {
    /// Check current approval status.
    Check {
        #[arg(long, default_value = "collateral")]
        token_type: String,
    },
    /// Set approval for the exchange.
    Set {
        #[arg(long, default_value = "collateral")]
        token_type: String,
    },
}

pub async fn run(
    cmd: ApproveCommands,
    auth: &PmAuth,
    mode: OutputMode,
    args: &GlobalPmArgs,
) -> anyhow::Result<()> {
    use polymarket_client_sdk::clob::Client as ClobClient;
    use polymarket_client_sdk::clob::types::request::*;
    use polymarket_client_sdk::clob::types::AssetType;

    let signer = auth.require_signer()?;
    let config = super::config_file::PmConfig::load().unwrap_or_default();

    let client = ClobClient::new(
        config.clob_base_url(),
        polymarket_client_sdk::clob::Config::default(),
    )?
    .authentication_builder(signer)
    .authenticate()
    .await?;

    let asset_type = match &cmd {
        ApproveCommands::Check { token_type } | ApproveCommands::Set { token_type } => {
            match token_type.to_lowercase().as_str() {
                "conditional" => AssetType::Conditional,
                _ => AssetType::Collateral,
            }
        }
    };

    match cmd {
        ApproveCommands::Check { .. } => {
            let req = BalanceAllowanceRequest::builder()
                .asset_type(asset_type)
                .build();
            let bal = client.balance_allowance(req).await?;
            output::print_debug(&bal, mode)?;
        }
        ApproveCommands::Set { .. } => {
            if args.dry_run {
                output::print_warn("[DRY RUN] Would set approval");
                return Ok(());
            }
            let req = BalanceAllowanceRequest::builder()
                .asset_type(asset_type)
                .build();
            client.update_balance_allowance(req).await?;
            output::print_success("approval set");
        }
    }
    Ok(())
}

//! `ploy pm profiles` — View Polymarket user profiles.

use clap::Subcommand;

use super::auth::PmAuth;
use super::output::{self, OutputMode};

#[derive(Subcommand, Debug, Clone)]
pub enum ProfilesCommands {
    /// Get a public profile by address.
    Get { address: String },
}

pub async fn run(cmd: ProfilesCommands, _auth: &PmAuth, mode: OutputMode) -> anyhow::Result<()> {
    use polymarket_client_sdk::gamma::types::request::*;
    use polymarket_client_sdk::gamma::Client as GammaClient;

    let config = super::config_file::PmConfig::load().unwrap_or_default();
    let gamma = GammaClient::new(config.gamma_base_url())?;

    match cmd {
        ProfilesCommands::Get { address } => {
            let addr: alloy::primitives::Address = address
                .parse()
                .map_err(|e| anyhow::anyhow!("invalid address: {e}"))?;
            let req = PublicProfileRequest::builder().address(addr).build();
            let profile = gamma.public_profile(&req).await?;
            output::print_item(&profile, mode)?;
        }
    }
    Ok(())
}

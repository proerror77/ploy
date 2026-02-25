//! `ploy pm sports` — Sports market metadata from Gamma API.

use clap::Subcommand;

use super::auth::PmAuth;
use super::output::{self, OutputMode};

#[derive(Subcommand, Debug, Clone)]
pub enum SportsCommands {
    /// Get sports metadata.
    Metadata,
    /// Get available sports market types.
    MarketTypes,
    /// List teams.
    Teams {
        /// Filter by league (e.g., NBA, NFL).
        #[arg(long)]
        league: Option<String>,
        #[arg(long, default_value = "50")]
        limit: i32,
    },
}

pub async fn run(cmd: SportsCommands, _auth: &PmAuth, mode: OutputMode) -> anyhow::Result<()> {
    use polymarket_client_sdk::gamma::Client as GammaClient;
    use polymarket_client_sdk::gamma::types::request::*;

    let config = super::config_file::PmConfig::load().unwrap_or_default();
    let gamma = GammaClient::new(config.gamma_base_url())?;

    match cmd {
        SportsCommands::Metadata => {
            let metadata = gamma.sports().await?;
            output::print_item(&metadata, mode)?;
        }
        SportsCommands::MarketTypes => {
            let types = gamma.sports_market_types().await?;
            output::print_item(&types, mode)?;
        }
        SportsCommands::Teams { league, limit } => {
            let league_vec = league.map(|lg| vec![lg]).unwrap_or_default();
            let req = TeamsRequest::builder()
                .limit(limit)
                .league(league_vec)
                .build();
            let teams = gamma.teams(&req).await?;
            output::print_item(&teams, mode)?;
        }
    }
    Ok(())
}

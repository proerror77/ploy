//! `ploy pm series` — Browse Polymarket market series.

use clap::Subcommand;

use super::auth::PmAuth;
use super::output::{self, OutputMode};

#[derive(Subcommand, Debug, Clone)]
pub enum SeriesCommands {
    /// List series.
    List {
        #[arg(long, default_value = "25")]
        limit: i32,
        #[arg(long, default_value = "0")]
        offset: i32,
        #[arg(long)]
        closed: Option<bool>,
    },
    /// Get a series by ID.
    Get { id: String },
}

pub async fn run(cmd: SeriesCommands, _auth: &PmAuth, mode: OutputMode) -> anyhow::Result<()> {
    use polymarket_client_sdk::gamma::Client as GammaClient;
    use polymarket_client_sdk::gamma::types::request::*;

    let config = super::config_file::PmConfig::load().unwrap_or_default();
    let gamma = GammaClient::new(config.gamma_base_url())?;

    match cmd {
        SeriesCommands::List { limit, offset, closed } => {
            let req = SeriesListRequest::builder()
                .limit(limit)
                .offset(offset)
                .maybe_closed(closed)
                .build();
            let series = gamma.series(&req).await?;
            output::print_item(&series, mode)?;
        }
        SeriesCommands::Get { id } => {
            let req = SeriesByIdRequest::builder().id(id).build();
            let series = gamma.series_by_id(&req).await?;
            output::print_item(&series, mode)?;
        }
    }
    Ok(())
}

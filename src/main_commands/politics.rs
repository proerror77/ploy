use crate::cli;
use ploy::cli::legacy::PoliticsCommands;
use ploy::error::Result;

/// Handle politics subcommands
pub(crate) async fn run_politics_command(cmd: &PoliticsCommands) -> Result<()> {
    match cmd {
        PoliticsCommands::Markets {
            category,
            search,
            high_volume,
        } => {
            cli::show_polymarket_politics(category, search.as_deref(), *high_volume).await?;
        }
        PoliticsCommands::Search { query } => {
            cli::search_politics_markets(query).await?;
        }
        PoliticsCommands::Analyze { event, candidate } => {
            cli::analyze_politics_market(event.as_deref(), candidate.as_deref()).await?;
        }
        PoliticsCommands::Trump { market_type } => {
            cli::show_trump_markets(market_type).await?;
        }
        PoliticsCommands::Elections { year } => {
            cli::show_election_markets(year.as_deref()).await?;
        }
    }

    Ok(())
}

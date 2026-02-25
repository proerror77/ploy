//! `ploy pm markets` — Browse and search Polymarket markets.

use clap::Subcommand;
use serde::Serialize;
use tabled::Tabled;

use super::auth::PmAuth;
use super::output::{self, OutputMode};

#[derive(Subcommand, Debug, Clone)]
pub enum MarketsCommands {
    /// List markets with optional filters.
    List {
        #[arg(long, default_value = "25")]
        limit: i32,
        #[arg(long, default_value = "0")]
        offset: i32,
        #[arg(long)]
        tag_id: Option<String>,
    },
    /// Get a market by ID.
    Get { id: String },
    /// Get a market by slug.
    GetBySlug { slug: String },
    /// Search markets by keyword.
    Search {
        query: String,
        #[arg(long, default_value = "10")]
        limit: i32,
    },
}

#[derive(Debug, Serialize, Tabled)]
pub struct MarketRow {
    pub id: String,
    pub question: String,
    pub active: String,
    pub volume: String,
    pub liquidity: String,
}

pub async fn run(cmd: MarketsCommands, _auth: &PmAuth, mode: OutputMode) -> anyhow::Result<()> {
    use polymarket_client_sdk::gamma::types::request::*;
    use polymarket_client_sdk::gamma::Client as GammaClient;

    let config = super::config_file::PmConfig::load().unwrap_or_default();
    let gamma = GammaClient::new(config.gamma_base_url())?;

    match cmd {
        MarketsCommands::List {
            limit,
            offset,
            tag_id,
        } => {
            let req = MarketsRequest::builder()
                .limit(limit)
                .offset(offset)
                .maybe_tag_id(tag_id)
                .build();
            let markets = gamma.markets(&req).await?;
            let rows: Vec<MarketRow> = markets
                .iter()
                .map(|m| MarketRow {
                    id: m.id.clone(),
                    question: m.question.clone().unwrap_or_default(),
                    active: m.active.map(|a| a.to_string()).unwrap_or_default(),
                    volume: m.volume.map(|v| v.to_string()).unwrap_or_default(),
                    liquidity: m.liquidity.map(|l| l.to_string()).unwrap_or_default(),
                })
                .collect();
            output::print_items(&rows, mode)?;
        }
        MarketsCommands::Get { id } => {
            let req = MarketByIdRequest::builder().id(id).build();
            let market = gamma.market_by_id(&req).await?;
            output::print_item(&market, mode)?;
        }
        MarketsCommands::GetBySlug { slug } => {
            let req = MarketBySlugRequest::builder().slug(slug).build();
            let market = gamma.market_by_slug(&req).await?;
            output::print_item(&market, mode)?;
        }
        MarketsCommands::Search { query, limit } => {
            let req = SearchRequest::builder()
                .q(query)
                .limit_per_type(limit)
                .build();
            let results = gamma.search(&req).await?;
            output::print_item(&results, mode)?;
        }
    }
    Ok(())
}

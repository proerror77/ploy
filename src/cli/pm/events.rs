//! `ploy pm events` — Browse Polymarket events.

use clap::Subcommand;
use serde::Serialize;
use tabled::Tabled;

use super::auth::PmAuth;
use super::output::{self, OutputMode};

#[derive(Subcommand, Debug, Clone)]
pub enum EventsCommands {
    /// List events with optional filters.
    List {
        #[arg(long, default_value = "25")]
        limit: i32,
        #[arg(long, default_value = "0")]
        offset: i32,
        #[arg(long)]
        active: Option<bool>,
        #[arg(long)]
        closed: Option<bool>,
        #[arg(long)]
        tag_slug: Option<String>,
    },
    /// Get an event by ID.
    Get { id: String },
    /// Get an event by slug.
    GetBySlug { slug: String },
}

#[derive(Debug, Serialize, Tabled)]
pub struct EventRow {
    pub id: String,
    pub title: String,
    pub active: String,
    pub volume: String,
}

pub async fn run(cmd: EventsCommands, _auth: &PmAuth, mode: OutputMode) -> anyhow::Result<()> {
    use polymarket_client_sdk::gamma::Client as GammaClient;
    use polymarket_client_sdk::gamma::types::request::*;

    let config = super::config_file::PmConfig::load().unwrap_or_default();
    let gamma = GammaClient::new(config.gamma_base_url())?;

    match cmd {
        EventsCommands::List { limit, offset, active, closed, tag_slug } => {
            let req = EventsRequest::builder()
                .limit(limit)
                .offset(offset)
                .maybe_active(active)
                .maybe_closed(closed)
                .maybe_tag_slug(tag_slug)
                .build();
            let events = gamma.events(&req).await?;
            let rows: Vec<EventRow> = events
                .iter()
                .map(|e| EventRow {
                    id: e.id.clone(),
                    title: e.title.clone().unwrap_or_default(),
                    active: e.active.map(|a| a.to_string()).unwrap_or_default(),
                    volume: e.volume.map(|v| v.to_string()).unwrap_or_default(),
                })
                .collect();
            output::print_items(&rows, mode)?;
        }
        EventsCommands::Get { id } => {
            let req = EventByIdRequest::builder().id(id).build();
            let event = gamma.event_by_id(&req).await?;
            output::print_item(&event, mode)?;
        }
        EventsCommands::GetBySlug { slug } => {
            let req = EventBySlugRequest::builder().slug(slug).build();
            let event = gamma.event_by_slug(&req).await?;
            output::print_item(&event, mode)?;
        }
    }
    Ok(())
}

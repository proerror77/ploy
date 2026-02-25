//! `ploy pm comments` — View Polymarket comments.

use clap::Subcommand;

use super::auth::PmAuth;
use super::output::{self, OutputMode};

#[derive(Subcommand, Debug, Clone)]
pub enum CommentsCommands {
    /// List comments on a market or event.
    List {
        /// Entity type: "market" or "event".
        #[arg(long, default_value = "market")]
        entity_type: String,
        /// Entity ID.
        entity_id: String,
        #[arg(long, default_value = "25")]
        limit: i32,
        #[arg(long, default_value = "0")]
        offset: i32,
    },
    /// Get a comment by ID.
    Get { id: String },
    /// Get comments by user address.
    ByUser {
        /// Ethereum address.
        address: String,
        #[arg(long, default_value = "25")]
        limit: i32,
    },
}

pub async fn run(cmd: CommentsCommands, _auth: &PmAuth, mode: OutputMode) -> anyhow::Result<()> {
    use polymarket_client_sdk::gamma::Client as GammaClient;
    use polymarket_client_sdk::gamma::types::request::*;
    use polymarket_client_sdk::gamma::types::ParentEntityType;

    let config = super::config_file::PmConfig::load().unwrap_or_default();
    let gamma = GammaClient::new(config.gamma_base_url())?;

    match cmd {
        CommentsCommands::List { entity_type, entity_id, limit, offset } => {
            let parent_type = match entity_type.to_lowercase().as_str() {
                "event" => ParentEntityType::Event,
                _ => ParentEntityType::Market,
            };
            let req = CommentsRequest::builder()
                .parent_entity_type(parent_type)
                .parent_entity_id(entity_id)
                .limit(limit)
                .offset(offset)
                .build();
            let comments = gamma.comments(&req).await?;
            output::print_item(&comments, mode)?;
        }
        CommentsCommands::Get { id } => {
            let req = CommentsByIdRequest::builder().id(id).build();
            let comment = gamma.comments_by_id(&req).await?;
            output::print_item(&comment, mode)?;
        }
        CommentsCommands::ByUser { address, limit } => {
            let addr: alloy::primitives::Address = address.parse()
                .map_err(|e| anyhow::anyhow!("invalid address: {e}"))?;
            let req = CommentsByUserAddressRequest::builder()
                .user_address(addr)
                .limit(limit)
                .build();
            let comments = gamma.comments_by_user_address(&req).await?;
            output::print_item(&comments, mode)?;
        }
    }
    Ok(())
}

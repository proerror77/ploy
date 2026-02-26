//! `ploy pm tags` — Browse Polymarket tags and categories.

use clap::Subcommand;

use super::auth::PmAuth;
use super::output::{self, OutputMode};

#[derive(Subcommand, Debug, Clone)]
pub enum TagsCommands {
    /// List all tags.
    List {
        #[arg(long, default_value = "50")]
        limit: i32,
        #[arg(long, default_value = "0")]
        offset: i32,
    },
    /// Get a tag by ID.
    Get { id: String },
    /// Get a tag by slug.
    GetBySlug { slug: String },
    /// Get related tags for a tag.
    Related { id: String },
}

pub async fn run(cmd: TagsCommands, _auth: &PmAuth, mode: OutputMode) -> anyhow::Result<()> {
    use polymarket_client_sdk::gamma::types::request::*;
    use polymarket_client_sdk::gamma::Client as GammaClient;

    let config = super::config_file::PmConfig::load().unwrap_or_default();
    let gamma = GammaClient::new(config.gamma_base_url())?;

    match cmd {
        TagsCommands::List { limit, offset } => {
            let req = TagsRequest::builder().limit(limit).offset(offset).build();
            let tags = gamma.tags(&req).await?;
            output::print_item(&tags, mode)?;
        }
        TagsCommands::Get { id } => {
            let req = TagByIdRequest::builder().id(id).build();
            let tag = gamma.tag_by_id(&req).await?;
            output::print_item(&tag, mode)?;
        }
        TagsCommands::GetBySlug { slug } => {
            let req = TagBySlugRequest::builder().slug(slug).build();
            let tag = gamma.tag_by_slug(&req).await?;
            output::print_item(&tag, mode)?;
        }
        TagsCommands::Related { id } => {
            let req = RelatedTagsByIdRequest::builder().id(id).build();
            let tags = gamma.related_tags_by_id(&req).await?;
            output::print_item(&tags, mode)?;
        }
    }
    Ok(())
}

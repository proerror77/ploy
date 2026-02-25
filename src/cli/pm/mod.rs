//! `ploy pm` — Polymarket CLI commands.
//!
//! Provides direct access to all Polymarket APIs: Gamma (market discovery),
//! CLOB (trading), Data API (analytics), CTF (conditional tokens), and more.

pub mod auth;
pub mod config_file;
pub mod output;

// Command modules (Phase 2-5)
pub mod approve;
pub mod bridge;
pub mod clob;
pub mod comments;
pub mod ctf;
pub mod data;
pub mod events;
pub mod markets;
pub mod orders;
pub mod profiles;
pub mod series;
pub mod setup;
pub mod shell;
pub mod sports;
pub mod tags;
pub mod wallet;

use clap::{Args, Subcommand};

/// Global arguments available to all `ploy pm` subcommands.
#[derive(Args, Debug, Clone)]
pub struct GlobalPmArgs {
    /// Output as JSON instead of human-readable tables.
    #[arg(long, global = true)]
    pub json: bool,

    /// Private key for authenticated operations (overrides env/config).
    /// WARNING: Prefer POLYMARKET_PRIVATE_KEY env var or `ploy pm setup` instead.
    /// CLI args are visible in `ps` output and shell history.
    #[arg(long, global = true, env = "POLYMARKET_PRIVATE_KEY")]
    pub private_key: Option<String>,

    /// Dry-run mode: print what would happen without executing.
    #[arg(long, global = true)]
    pub dry_run: bool,

    /// Skip confirmation prompts.
    #[arg(long, short = 'y', global = true)]
    pub yes: bool,
}

/// Top-level container for `ploy pm` — wraps GlobalPmArgs + PmCommands so
/// clap can parse both global flags and the subcommand together.
#[derive(Args, Debug, Clone)]
pub struct PmCli {
    #[command(flatten)]
    pub args: GlobalPmArgs,
    #[command(subcommand)]
    pub command: PmCommands,
}

/// Polymarket CLI commands.
#[derive(Subcommand, Debug, Clone)]
pub enum PmCommands {
    /// Browse and search markets.
    #[command(subcommand)]
    Markets(markets::MarketsCommands),

    /// Browse events (groups of related markets).
    #[command(subcommand)]
    Events(events::EventsCommands),

    /// Browse tags and categories.
    #[command(subcommand)]
    Tags(tags::TagsCommands),

    /// Browse market series.
    #[command(subcommand)]
    Series(series::SeriesCommands),

    /// View comments on markets and events.
    #[command(subcommand)]
    Comments(comments::CommentsCommands),

    /// View user profiles.
    #[command(subcommand)]
    Profiles(profiles::ProfilesCommands),

    /// Sports market metadata.
    #[command(subcommand)]
    Sports(sports::SportsCommands),

    /// CLOB (Central Limit Order Book) API.
    #[command(subcommand)]
    Clob(clob::ClobCommands),

    /// Data API (positions, trades, analytics).
    #[command(subcommand)]
    Data(data::DataCommands),

    /// Order management (create, cancel, list).
    #[command(subcommand)]
    Orders(orders::OrdersCommands),

    /// Wallet and account operations.
    #[command(subcommand)]
    Wallet(wallet::WalletCommands),

    /// CTF (Conditional Token Framework) on-chain operations.
    #[command(subcommand)]
    Ctf(ctf::CtfCommands),

    /// Token approval management.
    #[command(subcommand)]
    Approve(approve::ApproveCommands),

    /// Bridge operations (deposit USDC).
    #[command(subcommand)]
    Bridge(bridge::BridgeCommands),

    /// Interactive setup wizard.
    Setup,

    /// Interactive shell (REPL).
    Shell,
}

/// Main dispatch for `ploy pm <subcommand>`.
pub async fn run(cmd: PmCommands, args: &GlobalPmArgs) -> anyhow::Result<()> {
    let out_mode = output::OutputMode::from_json_flag(args.json);
    let auth = auth::resolve_auth(args.private_key.as_deref())?;

    match cmd {
        PmCommands::Markets(sub) => markets::run(sub, &auth, out_mode).await,
        PmCommands::Events(sub) => events::run(sub, &auth, out_mode).await,
        PmCommands::Tags(sub) => tags::run(sub, &auth, out_mode).await,
        PmCommands::Series(sub) => series::run(sub, &auth, out_mode).await,
        PmCommands::Comments(sub) => comments::run(sub, &auth, out_mode).await,
        PmCommands::Profiles(sub) => profiles::run(sub, &auth, out_mode).await,
        PmCommands::Sports(sub) => sports::run(sub, &auth, out_mode).await,
        PmCommands::Clob(sub) => clob::run(sub, &auth, out_mode).await,
        PmCommands::Data(sub) => data::run(sub, &auth, out_mode).await,
        PmCommands::Orders(sub) => orders::run(sub, &auth, out_mode, args).await,
        PmCommands::Wallet(sub) => wallet::run(sub, &auth, out_mode).await,
        PmCommands::Ctf(sub) => ctf::run(sub, &auth, out_mode, args).await,
        PmCommands::Approve(sub) => approve::run(sub, &auth, out_mode, args).await,
        PmCommands::Bridge(sub) => bridge::run(sub, &auth, out_mode).await,
        PmCommands::Setup => setup::run().await,
        PmCommands::Shell => shell::run(args).await,
    }
}

//! `ploy pm` — Polymarket CLI commands.
//!
//! Provides direct access to all Polymarket APIs: Gamma (market discovery),
//! CLOB (trading), Data API (analytics), CTF (conditional tokens), and more.

pub mod auth;
pub mod config_file;
pub mod output;

// Command modules (Phase 2-5)
pub mod approve;
pub mod clob;
pub mod comments;
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

#[cfg(feature = "pm_bridge")]
pub mod bridge;
#[cfg(not(feature = "pm_bridge"))]
mod bridge {
    use clap::Subcommand;

    use super::auth::PmAuth;
    use super::output::OutputMode;

    #[derive(Subcommand, Debug, Clone)]
    pub enum BridgeCommands {
        /// Get deposit addresses for bridging assets to Polymarket.
        Deposit {
            /// Override wallet address (defaults to signer address).
            #[arg(long)]
            address: Option<String>,
        },
        /// List supported bridge assets and chains.
        SupportedAssets,
        /// Check deposit transaction status.
        Status {
            /// Deposit address to check status for.
            address: String,
        },
    }

    pub async fn run(
        _cmd: BridgeCommands,
        _auth: &PmAuth,
        _mode: OutputMode,
    ) -> anyhow::Result<()> {
        anyhow::bail!("`pm bridge` is disabled in this build. Rebuild with `--features pm_bridge`")
    }
}

#[cfg(feature = "pm_ctf")]
pub mod ctf;
#[cfg(not(feature = "pm_ctf"))]
mod ctf {
    use clap::Subcommand;

    use super::auth::PmAuth;
    use super::output::OutputMode;
    use super::GlobalPmArgs;

    #[derive(Subcommand, Debug, Clone)]
    pub enum CtfCommands {
        /// Split collateral into conditional tokens.
        Split {
            /// Condition ID (bytes32 hex).
            condition_id: String,
            /// Amount of collateral to split (USDC, e.g., "10.0").
            amount: String,
        },
        /// Merge conditional tokens back into collateral.
        Merge {
            /// Condition ID (bytes32 hex).
            condition_id: String,
            /// Amount to merge.
            amount: String,
        },
        /// Redeem resolved conditional tokens.
        Redeem {
            /// Condition ID (bytes32 hex).
            condition_id: String,
            /// Use NegRisk adapter for negative-risk markets.
            #[arg(long)]
            neg_risk: bool,
        },
        /// Compute a condition ID from oracle + question ID + outcome count.
        ConditionId {
            /// Oracle address.
            oracle: String,
            /// Question ID (bytes32 hex).
            question_id: String,
            /// Number of outcomes (usually 2).
            #[arg(long, default_value = "2")]
            outcome_count: u32,
        },
    }

    pub async fn run(
        _cmd: CtfCommands,
        _auth: &PmAuth,
        _mode: OutputMode,
        _args: &GlobalPmArgs,
    ) -> anyhow::Result<()> {
        anyhow::bail!("`pm ctf` is disabled in this build. Rebuild with `--features pm_ctf`")
    }
}

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

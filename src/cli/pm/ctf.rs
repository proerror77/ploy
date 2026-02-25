//! `ploy pm ctf` — CTF (Conditional Token Framework) on-chain operations.
//!
//! Split USDC into conditional tokens, merge them back, or redeem after resolution.
//! These are on-chain transactions that require a private key and Polygon RPC.

use clap::Subcommand;

use super::auth::PmAuth;
use super::output::{self, OutputMode};
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
    cmd: CtfCommands,
    auth: &PmAuth,
    mode: OutputMode,
    args: &GlobalPmArgs,
) -> anyhow::Result<()> {
    match cmd {
        CtfCommands::ConditionId {
            oracle,
            question_id,
            outcome_count,
        } => {
            // Pure computation — no RPC needed. Use SDK's contract call if RPC
            // available, otherwise compute locally with keccak256.
            run_condition_id(&oracle, &question_id, outcome_count, auth, mode).await
        }
        CtfCommands::Split {
            condition_id,
            amount,
        } => run_split(&condition_id, &amount, auth, args).await,
        CtfCommands::Merge {
            condition_id,
            amount,
        } => run_merge(&condition_id, &amount, auth, args).await,
        CtfCommands::Redeem {
            condition_id,
            neg_risk,
        } => run_redeem(&condition_id, neg_risk, auth, args).await,
    }
}

/// Compute a condition ID. If RPC is available, uses the on-chain view function;
/// otherwise falls back to local keccak256 computation.
async fn run_condition_id(
    oracle: &str,
    question_id: &str,
    outcome_count: u32,
    auth: &PmAuth,
    _mode: OutputMode,
) -> anyhow::Result<()> {
    use alloy::primitives::{B256, U256};
    use std::str::FromStr;

    let oracle_addr: alloy::primitives::Address = oracle
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid oracle address: {e}"))?;
    let q_id =
        B256::from_str(question_id).map_err(|e| anyhow::anyhow!("invalid question_id: {e}"))?;

    // Try on-chain computation via CTF contract
    let config = super::config_file::PmConfig::load().unwrap_or_default();
    let rpc_url = config.rpc_url();

    match try_onchain_condition_id(rpc_url, auth.chain_id, oracle_addr, q_id, outcome_count).await {
        Ok(cond_id) => {
            output::print_kv("condition_id", &format!("{cond_id}"));
        }
        Err(_) => {
            // Fallback: local keccak256(abi.encodePacked(oracle, questionId, outcomeSlotCount))
            use alloy::primitives::keccak256;
            let mut data = Vec::new();
            data.extend_from_slice(oracle_addr.as_slice());
            data.extend_from_slice(q_id.as_slice());
            // outcomeSlotCount is uint256 in Solidity — pad to 32 bytes
            let count_u256 = U256::from(outcome_count);
            data.extend_from_slice(&count_u256.to_be_bytes::<32>());
            let cond_id = keccak256(&data);
            output::print_kv("condition_id", &format!("{cond_id}"));
            output::print_warn("(computed locally — RPC unavailable)");
        }
    }
    Ok(())
}

async fn try_onchain_condition_id(
    rpc_url: &str,
    chain_id: u64,
    oracle: alloy::primitives::Address,
    question_id: alloy::primitives::B256,
    outcome_count: u32,
) -> anyhow::Result<alloy::primitives::B256> {
    use alloy::primitives::U256;
    use alloy::providers::ProviderBuilder;
    use polymarket_client_sdk::ctf::types::ConditionIdRequest;
    use polymarket_client_sdk::ctf::Client as CtfClient;

    let provider = ProviderBuilder::new()
        .connect(rpc_url)
        .await
        .map_err(|e| anyhow::anyhow!("failed to connect to RPC: {e}"))?;

    let ctf = CtfClient::new(provider, chain_id)?;

    let req = ConditionIdRequest::builder()
        .oracle(oracle)
        .question_id(question_id)
        .outcome_slot_count(U256::from(outcome_count))
        .build();

    let resp = ctf.condition_id(&req).await?;
    Ok(resp.condition_id)
}

/// Split USDC into conditional tokens for a binary market.
async fn run_split(
    condition_id: &str,
    amount: &str,
    auth: &PmAuth,
    args: &GlobalPmArgs,
) -> anyhow::Result<()> {
    use alloy::primitives::B256;
    use std::str::FromStr;

    let cond_id =
        B256::from_str(condition_id).map_err(|e| anyhow::anyhow!("invalid condition_id: {e}"))?;

    // Parse amount as USDC (6 decimals)
    let usdc_amount = parse_usdc_amount(amount)?;

    if args.dry_run {
        output::print_warn(&format!(
            "[DRY RUN] Would split {amount} USDC into conditional tokens for {cond_id}"
        ));
        output::print_kv("usdc_raw", &usdc_amount.to_string());
        return Ok(());
    }

    if !args.yes && !output::confirm(&format!("Split {amount} USDC for condition {cond_id}?")) {
        output::print_warn("cancelled");
        return Ok(());
    }

    let signer = auth.require_signer()?;
    let config = super::config_file::PmConfig::load().unwrap_or_default();

    let provider = build_signer_provider(config.rpc_url(), signer, auth.chain_id).await?;
    let ctf = polymarket_client_sdk::ctf::Client::new(provider, auth.chain_id)?;

    // Use the convenience method for binary markets
    let collateral = polymarket_usdc_address(auth.chain_id);
    let req = polymarket_client_sdk::ctf::types::SplitPositionRequest::for_binary_market(
        collateral,
        cond_id,
        usdc_amount,
    );

    output::print_warn("Submitting split transaction...");
    let resp = ctf.split_position(&req).await?;
    output::print_success(&format!("Split successful!"));
    output::print_kv("tx_hash", &format!("{}", resp.transaction_hash));
    output::print_kv("block", &resp.block_number.to_string());
    Ok(())
}

/// Merge conditional tokens back into USDC.
async fn run_merge(
    condition_id: &str,
    amount: &str,
    auth: &PmAuth,
    args: &GlobalPmArgs,
) -> anyhow::Result<()> {
    use alloy::primitives::B256;
    use std::str::FromStr;

    let cond_id =
        B256::from_str(condition_id).map_err(|e| anyhow::anyhow!("invalid condition_id: {e}"))?;
    let usdc_amount = parse_usdc_amount(amount)?;

    if args.dry_run {
        output::print_warn(&format!(
            "[DRY RUN] Would merge {amount} conditional tokens for {cond_id}"
        ));
        output::print_kv("usdc_raw", &usdc_amount.to_string());
        return Ok(());
    }

    if !args.yes && !output::confirm(&format!("Merge {amount} tokens for condition {cond_id}?")) {
        output::print_warn("cancelled");
        return Ok(());
    }

    let signer = auth.require_signer()?;
    let config = super::config_file::PmConfig::load().unwrap_or_default();

    let provider = build_signer_provider(config.rpc_url(), signer, auth.chain_id).await?;
    let ctf = polymarket_client_sdk::ctf::Client::new(provider, auth.chain_id)?;

    let collateral = polymarket_usdc_address(auth.chain_id);
    let req = polymarket_client_sdk::ctf::types::MergePositionsRequest::for_binary_market(
        collateral,
        cond_id,
        usdc_amount,
    );

    output::print_warn("Submitting merge transaction...");
    let resp = ctf.merge_positions(&req).await?;
    output::print_success(&format!("Merge successful!"));
    output::print_kv("tx_hash", &format!("{}", resp.transaction_hash));
    output::print_kv("block", &resp.block_number.to_string());
    Ok(())
}

/// Redeem resolved conditional tokens for USDC.
async fn run_redeem(
    condition_id: &str,
    neg_risk: bool,
    auth: &PmAuth,
    args: &GlobalPmArgs,
) -> anyhow::Result<()> {
    use alloy::primitives::{B256, U256};
    use std::str::FromStr;

    let cond_id =
        B256::from_str(condition_id).map_err(|e| anyhow::anyhow!("invalid condition_id: {e}"))?;

    if args.dry_run {
        let mode = if neg_risk { "NegRisk" } else { "standard" };
        output::print_warn(&format!(
            "[DRY RUN] Would redeem resolved tokens for {cond_id} ({mode} mode)"
        ));
        return Ok(());
    }

    if !args.yes && !output::confirm(&format!("Redeem tokens for condition {cond_id}?")) {
        output::print_warn("cancelled");
        return Ok(());
    }

    let signer = auth.require_signer()?;
    let config = super::config_file::PmConfig::load().unwrap_or_default();

    if neg_risk {
        // NegRisk redemption uses the NegRisk adapter
        let provider = build_signer_provider(config.rpc_url(), signer, auth.chain_id).await?;
        let ctf = polymarket_client_sdk::ctf::Client::with_neg_risk(provider, auth.chain_id)?;

        let req = polymarket_client_sdk::ctf::types::RedeemNegRiskRequest::builder()
            .condition_id(cond_id)
            .amounts(vec![U256::MAX, U256::MAX]) // Redeem all
            .build();

        output::print_warn("Submitting NegRisk redeem transaction...");
        let resp = ctf.redeem_neg_risk(&req).await?;
        output::print_success(&format!("NegRisk redeem successful!"));
        output::print_kv("tx_hash", &format!("{}", resp.transaction_hash));
        output::print_kv("block", &resp.block_number.to_string());
    } else {
        // Standard CTF redemption
        let provider = build_signer_provider(config.rpc_url(), signer, auth.chain_id).await?;
        let ctf = polymarket_client_sdk::ctf::Client::new(provider, auth.chain_id)?;

        let collateral = polymarket_usdc_address(auth.chain_id);
        let req = polymarket_client_sdk::ctf::types::RedeemPositionsRequest::for_binary_market(
            collateral, cond_id,
        );

        output::print_warn("Submitting redeem transaction...");
        let resp = ctf.redeem_positions(&req).await?;
        output::print_success(&format!("Redeem successful!"));
        output::print_kv("tx_hash", &format!("{}", resp.transaction_hash));
        output::print_kv("block", &resp.block_number.to_string());
    }
    Ok(())
}

// ─── Helpers ─────────────────────────────────────────────────

/// Build an alloy provider with a signer for sending transactions.
async fn build_signer_provider(
    rpc_url: &str,
    signer: &alloy::signers::local::PrivateKeySigner,
    _chain_id: u64,
) -> anyhow::Result<impl alloy::providers::Provider + Clone> {
    use alloy::network::EthereumWallet;
    use alloy::providers::ProviderBuilder;

    let wallet = EthereumWallet::from(signer.clone());

    let provider = ProviderBuilder::new()
        .wallet(wallet)
        .connect(rpc_url)
        .await
        .map_err(|e| anyhow::anyhow!("failed to connect to RPC: {e}"))?;

    Ok(provider)
}

/// Parse a USDC amount string (e.g., "10.5") into raw U256 (6 decimals).
fn parse_usdc_amount(amount: &str) -> anyhow::Result<alloy::primitives::U256> {
    use alloy::primitives::U256;

    let parts: Vec<&str> = amount.split('.').collect();
    let raw = match parts.len() {
        1 => {
            let whole: u64 = parts[0]
                .parse()
                .map_err(|e| anyhow::anyhow!("invalid amount: {e}"))?;
            whole * 1_000_000
        }
        2 => {
            let whole: u64 = parts[0]
                .parse()
                .map_err(|e| anyhow::anyhow!("invalid amount: {e}"))?;
            let frac_str = format!("{:0<6}", parts[1]); // Pad to 6 decimals
            let frac: u64 = frac_str[..6]
                .parse()
                .map_err(|e| anyhow::anyhow!("invalid fractional amount: {e}"))?;
            whole * 1_000_000 + frac
        }
        _ => anyhow::bail!("invalid amount format: expected 'X' or 'X.Y'"),
    };
    Ok(U256::from(raw))
}

/// Returns the USDC.e contract address on Polygon.
fn polymarket_usdc_address(chain_id: u64) -> alloy::primitives::Address {
    use alloy::primitives::address;
    match chain_id {
        // Polygon mainnet: USDC.e (bridged USDC)
        137 => address!("2791Bca1f2de4661ED88A30C99A7a9449Aa84174"),
        // Amoy testnet: test USDC
        80002 => address!("9c4e1703476e875070ee25b56a58b008cfb8fa78"),
        // Fallback to mainnet
        _ => address!("2791Bca1f2de4661ED88A30C99A7a9449Aa84174"),
    }
}

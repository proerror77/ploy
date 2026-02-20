//! Auto-claimer for resolved Polymarket positions
//!
//! Monitors for positions that can be redeemed (winning positions after market resolution)
//! and automatically claims them by calling the ConditionalTokens contract.

use alloy::network::EthereumWallet;
use alloy::primitives::{Address, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use alloy::sol;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use chrono::{NaiveDate, Utc};
use ethers::abi::{encode as abi_encode, AbiParser, Token};
use ethers::middleware::SignerMiddleware;
use ethers::providers::{
    Http as EthersHttp, Middleware as EthersMiddleware, Provider as EthersProvider,
};
use ethers::signers::{LocalWallet, Signer as _};
use ethers::types::{
    Bytes as EthersBytes, H256 as EthersH256,
    Address as EthersAddress, TransactionRequest as EthersTransactionRequest, U256 as EthersU256,
    transaction::eip2718::TypedTransaction as EthersTypedTransaction,
    U64 as EthersU64,
};
use ethers::utils::{get_create2_address_from_hash as ethers_get_create2_address_from_hash, keccak256};
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

use crate::adapters::PolymarketClient;
use crate::error::Result;

// CTF contracts on Polygon
const CONDITIONAL_TOKENS_POLYGON: &str = "0x4D97DCd97eC945f40cF65F87097ACe5EA0476045";
const USDC_E_POLYGON: &str = "0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174";
const POLYGON_RPC_DEFAULT: &str = "https://polygon-bor-rpc.publicnode.com";
const POLYGON_CHAIN_ID: u64 = 137;
const DEFAULT_MIN_NATIVE_GAS_WEI: u64 = 5_000_000_000_000_000; // 0.005 MATIC buffer
const DEFAULT_AUTO_TOPUP_TARGET_WEI: u128 = 20_000_000_000_000_000; // 0.02 MATIC
const DEFAULT_AUTO_TOPUP_MAX_PER_TX_WEI: u128 = 20_000_000_000_000_000; // 0.02 MATIC
const DEFAULT_AUTO_TOPUP_DAILY_CAP_WEI: u128 = 100_000_000_000_000_000; // 0.1 MATIC
const DEFAULT_AUTO_TOPUP_RESERVE_WEI: u128 = 5_000_000_000_000_000; // keep 0.005 MATIC on top-up wallet
const RELAYER_URL_DEFAULT: &str = "https://relayer-v2.polymarket.com";
const RELAYER_PROXY_FACTORY_POLYGON: &str = "0xaB45c5A4B0c941a2F231C04C3f49182e1A254052";
const RELAYER_RELAY_HUB_POLYGON: &str = "0xD216153c06E857cD7f72665E0aF1d7D82172F494";
const RELAYER_PROXY_INIT_CODE_HASH: &str =
    "0xd21df8dc65880a8606f09fe0ce3df9b8869287ab0b058be05aa9e8af6330a00b";
const RELAYER_DEFAULT_GAS_LIMIT: u64 = 10_000_000;
const RELAYER_DEFAULT_MAX_POLLS: u64 = 100;
const RELAYER_DEFAULT_POLL_INTERVAL_MS: u64 = 2_000;

// Generate contract bindings for ConditionalTokens
sol! {
    #[allow(missing_docs)]
    #[sol(rpc)]
    interface IConditionalTokens {
        /// Redeem positions for a resolved condition
        function redeemPositions(
            address collateralToken,
            bytes32 parentCollectionId,
            bytes32 conditionId,
            uint256[] calldata indexSets
        ) external;

        /// Get balance of a token for an account
        function balanceOf(address account, uint256 id) external view returns (uint256);
    }
}

fn json_value_to_boolish(value: &serde_json::Value) -> Option<bool> {
    match value {
        serde_json::Value::Bool(v) => Some(*v),
        serde_json::Value::Number(n) => {
            if n.as_i64() == Some(0) {
                Some(false)
            } else if n.as_i64() == Some(1) {
                Some(true)
            } else {
                None
            }
        }
        serde_json::Value::String(s) => {
            let s = s.trim().to_ascii_lowercase();
            match s.as_str() {
                "true" | "1" | "yes" | "y" => Some(true),
                "false" | "0" | "no" | "n" => Some(false),
                _ => None,
            }
        }
        _ => None,
    }
}

fn env_flag(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "y" | "on"
            )
        })
        .unwrap_or(default)
}

fn env_string_any(keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Ok(v) = std::env::var(key) {
            let trimmed = v.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn env_u128_any(keys: &[&str]) -> Option<u128> {
    for key in keys {
        if let Ok(v) = std::env::var(key) {
            if let Ok(parsed) = v.trim().parse::<u128>() {
                return Some(parsed);
            }
        }
    }
    None
}

fn env_u64_any(keys: &[&str]) -> Option<u64> {
    for key in keys {
        if let Ok(v) = std::env::var(key) {
            if let Ok(parsed) = v.trim().parse::<u64>() {
                return Some(parsed);
            }
        }
    }
    None
}

fn min_native_gas_wei() -> U256 {
    std::env::var("CLAIMER_MIN_NATIVE_GAS_WEI")
        .ok()
        .and_then(|v| v.trim().parse::<u128>().ok())
        .map(U256::from)
        .unwrap_or_else(|| U256::from(DEFAULT_MIN_NATIVE_GAS_WEI))
}

fn auto_topup_enabled() -> bool {
    env_flag(
        "CLAIMER_AUTO_TOPUP_ENABLED",
        env_flag("CLAIMER_GAS_TOPUP_ENABLED", false),
    )
}

fn relayer_claim_enabled() -> bool {
    env_flag(
        "CLAIMER_RELAYER_ENABLED",
        env_flag("CLAIMER_GASLESS_REDEEM_ENABLED", true),
    )
}

fn relayer_builder_credentials() -> Option<RelayerBuilderCredentials> {
    let api_key = env_string_any(&[
        "CLAIMER_BUILDER_API_KEY",
        "POLY_BUILDER_API_KEY",
        "BUILDER_API_KEY",
    ])?;
    let secret = env_string_any(&[
        "CLAIMER_BUILDER_SECRET",
        "POLY_BUILDER_SECRET",
        "BUILDER_SECRET",
    ])?;
    let passphrase = env_string_any(&[
        "CLAIMER_BUILDER_PASSPHRASE",
        "POLY_BUILDER_PASSPHRASE",
        "BUILDER_PASS_PHRASE",
        "BUILDER_PASSPHRASE",
    ])?;
    Some(RelayerBuilderCredentials {
        api_key,
        secret,
        passphrase,
    })
}

fn relayer_base_url() -> String {
    env_string_any(&["CLAIMER_RELAYER_URL", "POLYMARKET_RELAYER_URL", "RELAYER_URL"])
        .unwrap_or_else(|| RELAYER_URL_DEFAULT.to_string())
}

fn relayer_fallback_onchain_enabled() -> bool {
    env_flag("CLAIMER_RELAYER_FALLBACK_ONCHAIN", false)
}

fn relayer_poll_max() -> u64 {
    env_u64_any(&["CLAIMER_RELAYER_MAX_POLLS"])
        .unwrap_or(RELAYER_DEFAULT_MAX_POLLS)
        .max(1)
}

fn relayer_poll_interval_ms() -> u64 {
    env_u64_any(&["CLAIMER_RELAYER_POLL_INTERVAL_MS"])
        .unwrap_or(RELAYER_DEFAULT_POLL_INTERVAL_MS)
        .max(250)
}

fn relayer_hmac_signature(secret_base64: &str, message: &str) -> Result<String> {
    let trimmed = secret_base64.trim();
    let secret = BASE64
        .decode(trimmed)
        .or_else(|_| {
            // Some builder secrets are url-safe base64 (with '-'/'_').
            let mut normalized = trimmed.replace('-', "+").replace('_', "/");
            while normalized.len() % 4 != 0 {
                normalized.push('=');
            }
            BASE64.decode(normalized.as_bytes())
        })
        .map_err(|e| {
            crate::error::PloyError::Signature(format!("Invalid builder secret encoding: {}", e))
        })?;
    let mut mac: Hmac<Sha256> = Hmac::new_from_slice(&secret).map_err(|e| {
        crate::error::PloyError::Signature(format!("Builder HMAC init failed: {}", e))
    })?;
    mac.update(message.as_bytes());
    let sig = BASE64.encode(mac.finalize().into_bytes());
    Ok(sig.replace('+', "-").replace('/', "_"))
}

fn u256_to_u128_saturating(value: U256) -> u128 {
    value.to_string().parse::<u128>().unwrap_or(u128::MAX)
}

#[derive(Debug, Clone)]
struct GasTopupState {
    day: NaiveDate,
    spent_wei: u128,
}

#[derive(Debug, Clone)]
struct RelayerBuilderCredentials {
    api_key: String,
    secret: String,
    passphrase: String,
}

#[derive(Debug, Deserialize)]
struct RelayerPayloadResponse {
    address: String,
    nonce: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RelayerSignatureParams {
    #[serde(rename = "gasPrice")]
    gas_price: String,
    #[serde(rename = "gasLimit")]
    gas_limit: String,
    #[serde(rename = "relayerFee")]
    relayer_fee: String,
    #[serde(rename = "relayHub")]
    relay_hub: String,
    relay: String,
}

#[derive(Debug, Serialize)]
struct RelayerSubmitRequest {
    #[serde(rename = "type")]
    tx_type: String,
    from: String,
    to: String,
    #[serde(rename = "proxyWallet")]
    proxy_wallet: String,
    data: String,
    nonce: String,
    signature: String,
    #[serde(rename = "signatureParams")]
    signature_params: RelayerSignatureParams,
    metadata: String,
}

#[derive(Debug, Deserialize)]
struct RelayerSubmitResponse {
    #[serde(rename = "transactionID")]
    transaction_id: String,
    state: String,
    #[serde(rename = "transactionHash")]
    transaction_hash: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RelayerTransactionStatus {
    state: String,
    #[serde(rename = "transactionHash")]
    transaction_hash: Option<String>,
}

/// Position that can be redeemed
#[derive(Debug, Clone)]
pub struct RedeemablePosition {
    pub condition_id: String,
    pub token_id: String,
    pub outcome: String,
    pub size: Decimal,
    pub payout: Decimal,
    pub neg_risk: bool,
}

/// Result of a claim operation
#[derive(Debug, Clone)]
pub struct ClaimResult {
    pub condition_id: String,
    pub amount_claimed: Decimal,
    pub tx_hash: String,
    pub success: bool,
    pub error: Option<String>,
}

/// Auto-claimer configuration
#[derive(Debug, Clone)]
pub struct ClaimerConfig {
    /// How often to check for redeemable positions (seconds)
    pub check_interval_secs: u64,
    /// Minimum position size to claim (avoid dust)
    pub min_claim_size: Decimal,
    /// Whether to claim automatically or just report
    pub auto_claim: bool,
    /// Private key for signing transactions
    pub private_key: Option<String>,
}

impl Default for ClaimerConfig {
    fn default() -> Self {
        Self {
            check_interval_secs: 60,
            min_claim_size: Decimal::ONE, // At least $1 to claim
            auto_claim: true,
            private_key: None,
        }
    }
}

/// Auto-claimer for Polymarket positions
pub struct AutoClaimer {
    client: PolymarketClient,
    config: ClaimerConfig,
    claimed_conditions: Arc<RwLock<std::collections::HashSet<String>>>,
    gas_topup_state: Arc<RwLock<GasTopupState>>,
    running: Arc<RwLock<bool>>,
}

impl AutoClaimer {
    /// Create a new auto-claimer
    pub fn new(client: PolymarketClient, config: ClaimerConfig) -> Self {
        Self {
            client,
            config,
            claimed_conditions: Arc::new(RwLock::new(std::collections::HashSet::new())),
            gas_topup_state: Arc::new(RwLock::new(GasTopupState {
                day: Utc::now().date_naive(),
                spent_wei: 0,
            })),
            running: Arc::new(RwLock::new(false)),
        }
    }

    /// Start the auto-claimer background task
    pub async fn start(&self) -> Result<()> {
        let mut running = self.running.write().await;
        if *running {
            info!("AutoClaimer already running");
            return Ok(());
        }
        *running = true;
        drop(running);

        info!(
            "Starting AutoClaimer (check interval: {}s, auto_claim: {})",
            self.config.check_interval_secs, self.config.auto_claim
        );

        loop {
            // Check if we should stop
            if !*self.running.read().await {
                break;
            }

            // Check for redeemable positions
            match self.check_and_claim().await {
                Ok(results) => {
                    for result in results {
                        if result.success {
                            info!(
                                "Claimed ${:.2} from condition {}",
                                result.amount_claimed, result.condition_id
                            );
                        } else {
                            warn!(
                                "Failed to claim condition {}: {:?}",
                                result.condition_id, result.error
                            );
                        }
                    }
                }
                Err(e) => {
                    error!("Error checking redeemable positions: {}", e);
                }
            }

            // Wait before next check
            tokio::time::sleep(Duration::from_secs(self.config.check_interval_secs)).await;
        }

        info!("AutoClaimer stopped");
        Ok(())
    }

    /// Stop the auto-claimer
    pub async fn stop(&self) {
        let mut running = self.running.write().await;
        *running = false;
    }

    /// Check for redeemable positions and optionally claim them
    pub async fn check_and_claim(&self) -> Result<Vec<ClaimResult>> {
        let positions =
            Self::collapse_positions_by_condition(self.get_redeemable_positions().await?);

        if positions.is_empty() {
            debug!("No redeemable positions found");
            return Ok(vec![]);
        }

        let relayer_ready = relayer_claim_enabled() && relayer_builder_credentials().is_some();
        if self.config.auto_claim && !relayer_ready && !self.preflight_wallet_can_claim().await? {
            return Ok(vec![]);
        }
        if self.config.auto_claim && relayer_ready {
            debug!("Relayer redeem is enabled; skipping native gas preflight");
        }

        info!("Found {} redeemable condition(s)", positions.len());

        let mut results = Vec::new();

        for pos in positions {
            // Skip if already claimed
            {
                let claimed = self.claimed_conditions.read().await;
                if claimed.contains(&pos.condition_id) {
                    debug!("Already claimed condition {}", pos.condition_id);
                    continue;
                }
            }

            // Skip if below minimum size
            if pos.payout < self.config.min_claim_size {
                debug!("Skipping small position: ${:.2}", pos.payout);
                continue;
            }

            // Log the opportunity
            info!(
                "Redeemable: {} - {} shares = ${:.2}",
                pos.outcome, pos.size, pos.payout
            );

            if self.config.auto_claim {
                // Attempt to claim
                match self.claim_position(&pos).await {
                    Ok(tx_hash) => {
                        let mut claimed = self.claimed_conditions.write().await;
                        claimed.insert(pos.condition_id.clone());

                        results.push(ClaimResult {
                            condition_id: pos.condition_id,
                            amount_claimed: pos.payout,
                            tx_hash,
                            success: true,
                            error: None,
                        });
                    }
                    Err(e) => {
                        results.push(ClaimResult {
                            condition_id: pos.condition_id,
                            amount_claimed: Decimal::ZERO,
                            tx_hash: String::new(),
                            success: false,
                            error: Some(e.to_string()),
                        });
                    }
                }
            } else {
                info!(
                    "[DRY RUN] Would claim ${:.2} from {}",
                    pos.payout, pos.condition_id
                );
            }
        }

        Ok(results)
    }

    /// Merge multiple redeemable rows for the same condition into one claim attempt.
    ///
    /// A condition-level redeem burns all balances for the provided index sets, so sending
    /// one transaction per condition avoids duplicate claims for split rows in Data API output.
    fn collapse_positions_by_condition(
        positions: Vec<RedeemablePosition>,
    ) -> Vec<RedeemablePosition> {
        let mut merged: std::collections::BTreeMap<String, RedeemablePosition> =
            std::collections::BTreeMap::new();

        for pos in positions {
            if let Some(existing) = merged.get_mut(&pos.condition_id) {
                existing.size += pos.size;
                existing.payout += pos.payout;
                existing.neg_risk = existing.neg_risk || pos.neg_risk;
                if existing.outcome.is_empty() && !pos.outcome.is_empty() {
                    existing.outcome = pos.outcome;
                }
                continue;
            }
            merged.insert(pos.condition_id.clone(), pos);
        }

        merged.into_values().collect()
    }

    /// Get list of redeemable positions from Polymarket
    async fn get_redeemable_positions(&self) -> Result<Vec<RedeemablePosition>> {
        // Use the Data API to get positions
        let positions = self.client.get_positions().await?;
        let allow_price_fallback = env_flag("CLAIMER_ALLOW_PRICE_FALLBACK", false);

        let mut redeemable = Vec::new();

        for p in positions {
            // Check if position has shares
            let size: Decimal = match p.size.parse() {
                Ok(s) if s > Decimal::ZERO => s,
                _ => continue,
            };

            // Check if position is redeemable (API may provide this flag)
            // Or check if current price = 1.0 (winning side)
            let is_winner = p
                .cur_price
                .as_ref()
                .and_then(|price_str| price_str.parse::<f64>().ok())
                .map(|price| price > 0.99) // Winner = price ~1.0
                .unwrap_or(false);

            // Also check the redeemable flag if available
            let api_says_redeemable = p.is_redeemable();

            if !api_says_redeemable && !(allow_price_fallback && is_winner) {
                continue;
            }
            if !api_says_redeemable && allow_price_fallback && is_winner {
                debug!(
                    "Using price-based fallback for condition {:?} (cur_price={:?})",
                    p.condition_id, p.cur_price
                );
            }

            let condition_id = p
                .condition_id
                .clone()
                .or_else(|| {
                    p.extra
                        .get("conditionId")
                        .and_then(|v| v.as_str().map(ToString::to_string))
                })
                .or_else(|| {
                    p.extra
                        .get("condition_id")
                        .and_then(|v| v.as_str().map(ToString::to_string))
                })
                .unwrap_or_default();
            if condition_id.trim().is_empty() {
                warn!(
                    "Skipping redeemable position with missing condition_id (outcome={}, size={})",
                    p.outcome.clone().unwrap_or_default(),
                    size
                );
                continue;
            }

            let token_id = p
                .token_id
                .clone()
                .or_else(|| {
                    p.extra
                        .get("tokenId")
                        .and_then(|v| v.as_str().map(ToString::to_string))
                })
                .or_else(|| {
                    p.extra
                        .get("token_id")
                        .and_then(|v| v.as_str().map(ToString::to_string))
                })
                .unwrap_or_else(|| p.asset_id.clone());

            let outcome = p
                .outcome
                .clone()
                .or_else(|| {
                    p.extra
                        .get("outcome")
                        .and_then(|v| v.as_str().map(ToString::to_string))
                })
                .unwrap_or_default();

            let payout = size; // Each winning share = $1

            redeemable.push(RedeemablePosition {
                condition_id: condition_id.clone(),
                token_id,
                outcome: outcome.clone(),
                size,
                payout,
                neg_risk: p
                    .negative_risk
                    .or_else(|| {
                        p.extra
                            .get("neg_risk")
                            .or_else(|| p.extra.get("negRisk"))
                            .and_then(json_value_to_boolish)
                    })
                    .unwrap_or(false),
            });

            info!(
                "Found redeemable position: {} {} shares, condition={}",
                outcome,
                size,
                condition_id.chars().take(16).collect::<String>()
            );
        }

        Ok(redeemable)
    }

    /// Preflight signer wallet native balance to avoid spamming failed redeem txs.
    async fn preflight_wallet_can_claim(&self) -> Result<bool> {
        let private_key =
            self.config.private_key.as_ref().ok_or_else(|| {
                crate::error::PloyError::Wallet("No private key for claiming".into())
            })?;

        let signer: PrivateKeySigner = private_key
            .parse()
            .map_err(|e| crate::error::PloyError::Wallet(format!("Invalid private key: {}", e)))?;

        let polygon_rpc = std::env::var("POLYGON_RPC_URL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| POLYGON_RPC_DEFAULT.to_string());
        let rpc_url = polygon_rpc.parse().map_err(|e| {
            crate::error::PloyError::AddressParsing(format!("Invalid RPC URL: {}", e))
        })?;
        let provider = ProviderBuilder::new().connect_http(rpc_url);

        let wallet_addr = signer.address();
        let balance = provider.get_balance(wallet_addr).await.map_err(|e| {
            crate::error::PloyError::OrderSubmission(format!(
                "Failed to read claimer wallet balance: {}",
                e
            ))
        })?;
        let min_balance = min_native_gas_wei();

        let mut effective_balance = balance;
        if effective_balance < min_balance {
            if let Some(updated) = self
                .maybe_auto_topup_wallet(wallet_addr, effective_balance, min_balance)
                .await?
            {
                effective_balance = updated;
            }
        }

        if effective_balance < min_balance {
            warn!(
                "Auto-claim paused: wallet {} has {} wei, need at least {} wei for gas. Top up MATIC and claimer will resume automatically.",
                wallet_addr,
                effective_balance,
                min_balance
            );
            return Ok(false);
        }

        debug!(
            "Claimer wallet {} gas check passed: {} wei",
            wallet_addr, effective_balance
        );
        Ok(true)
    }

    /// Optionally tops up signer native gas from a dedicated top-up wallet.
    ///
    /// This is intentionally conservative and disabled by default. Enable with:
    /// - CLAIMER_AUTO_TOPUP_ENABLED=true
    /// - CLAIMER_AUTO_TOPUP_PRIVATE_KEY=0x... (wallet with POL/MATIC)
    async fn maybe_auto_topup_wallet(
        &self,
        target_wallet: Address,
        current_balance: U256,
        min_balance: U256,
    ) -> Result<Option<U256>> {
        if !auto_topup_enabled() {
            return Ok(None);
        }

        let Some(topup_private_key) = env_string_any(&[
            "CLAIMER_AUTO_TOPUP_PRIVATE_KEY",
            "CLAIMER_GAS_TOPUP_PRIVATE_KEY",
        ]) else {
            warn!(
                "Auto top-up enabled but missing CLAIMER_AUTO_TOPUP_PRIVATE_KEY/CLAIMER_GAS_TOPUP_PRIVATE_KEY"
            );
            return Ok(None);
        };

        let threshold_wei = env_u128_any(&[
            "CLAIMER_AUTO_TOPUP_THRESHOLD_WEI",
            "CLAIMER_GAS_TOPUP_THRESHOLD_WEI",
        ])
        .unwrap_or_else(|| u256_to_u128_saturating(min_balance));

        let current_wei = u256_to_u128_saturating(current_balance);
        if current_wei >= threshold_wei {
            return Ok(Some(current_balance));
        }

        let target_wei = env_u128_any(&[
            "CLAIMER_AUTO_TOPUP_TARGET_WEI",
            "CLAIMER_GAS_TOPUP_TARGET_WEI",
        ])
        .unwrap_or(DEFAULT_AUTO_TOPUP_TARGET_WEI)
        .max(threshold_wei);

        if target_wei <= current_wei {
            return Ok(Some(current_balance));
        }

        let max_per_tx_wei = env_u128_any(&[
            "CLAIMER_AUTO_TOPUP_MAX_PER_TX_WEI",
            "CLAIMER_GAS_TOPUP_MAX_PER_TX_WEI",
        ])
        .unwrap_or(DEFAULT_AUTO_TOPUP_MAX_PER_TX_WEI)
        .max(1);

        let daily_cap_wei = env_u128_any(&[
            "CLAIMER_AUTO_TOPUP_DAILY_CAP_WEI",
            "CLAIMER_GAS_TOPUP_DAILY_CAP_WEI",
        ])
        .unwrap_or(DEFAULT_AUTO_TOPUP_DAILY_CAP_WEI);

        let reserve_wei = env_u128_any(&[
            "CLAIMER_AUTO_TOPUP_RESERVE_WEI",
            "CLAIMER_GAS_TOPUP_RESERVE_WEI",
        ])
        .unwrap_or(DEFAULT_AUTO_TOPUP_RESERVE_WEI);

        let desired_wei = target_wei.saturating_sub(current_wei);
        let mut topup_wei = desired_wei.min(max_per_tx_wei);

        {
            let today = Utc::now().date_naive();
            let mut state = self.gas_topup_state.write().await;
            if state.day != today {
                state.day = today;
                state.spent_wei = 0;
            }

            if state.spent_wei >= daily_cap_wei {
                warn!(
                    "Auto top-up skipped: daily cap reached (spent={} wei, cap={} wei)",
                    state.spent_wei, daily_cap_wei
                );
                return Ok(None);
            }

            let remaining_today = daily_cap_wei.saturating_sub(state.spent_wei);
            topup_wei = topup_wei.min(remaining_today);
        }

        if topup_wei == 0 {
            debug!("Auto top-up skipped: computed top-up amount is 0 wei");
            return Ok(None);
        }

        let polygon_rpc = std::env::var("POLYGON_RPC_URL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| POLYGON_RPC_DEFAULT.to_string());

        let provider = match EthersProvider::<EthersHttp>::try_from(polygon_rpc.as_str()) {
            Ok(p) => p,
            Err(e) => {
                warn!("Auto top-up skipped: invalid POLYGON_RPC_URL: {}", e);
                return Ok(None);
            }
        };

        let topup_wallet = match topup_private_key.parse::<LocalWallet>() {
            Ok(w) => w.with_chain_id(POLYGON_CHAIN_ID),
            Err(e) => {
                warn!("Auto top-up skipped: invalid top-up private key: {}", e);
                return Ok(None);
            }
        };
        let topup_addr = topup_wallet.address();
        let client = SignerMiddleware::new(provider.clone(), topup_wallet);

        let topup_balance_wei = match provider.get_balance(topup_addr, None).await {
            Ok(v) => v.to_string().parse::<u128>().unwrap_or(u128::MAX),
            Err(e) => {
                warn!(
                    "Auto top-up skipped: failed reading top-up wallet balance: {}",
                    e
                );
                return Ok(None);
            }
        };

        let required_wei = topup_wei.saturating_add(reserve_wei);
        if topup_balance_wei < required_wei {
            warn!(
                "Auto top-up skipped: top-up wallet {} has {} wei, needs at least {} wei (topup={} + reserve={})",
                topup_addr, topup_balance_wei, required_wei, topup_wei, reserve_wei
            );
            return Ok(None);
        }

        let target_str = format!("{:#x}", target_wallet);
        let target_addr: EthersAddress = match target_str.parse() {
            Ok(addr) => addr,
            Err(e) => {
                warn!(
                    "Auto top-up skipped: invalid target wallet address {}: {}",
                    target_str, e
                );
                return Ok(None);
            }
        };

        info!(
            "Auto top-up triggered: sending {} wei to claimer wallet {} (current={} threshold={} target={})",
            topup_wei, target_addr, current_wei, threshold_wei, target_wei
        );

        let tx = EthersTransactionRequest::new()
            .to(target_addr)
            .value(EthersU256::from(topup_wei));

        let pending_tx = match client.send_transaction(tx, None).await {
            Ok(p) => p,
            Err(e) => {
                warn!("Auto top-up tx submission failed: {}", e);
                return Ok(None);
            }
        };

        let receipt = match pending_tx.await {
            Ok(Some(r)) => r,
            Ok(None) => {
                warn!("Auto top-up tx dropped before receipt");
                return Ok(None);
            }
            Err(e) => {
                warn!("Auto top-up tx confirmation failed: {}", e);
                return Ok(None);
            }
        };

        if receipt.status != Some(EthersU64::from(1u64)) {
            warn!(
                "Auto top-up tx reverted: hash={:?}, status={:?}",
                receipt.transaction_hash, receipt.status
            );
            return Ok(None);
        }

        {
            let today = Utc::now().date_naive();
            let mut state = self.gas_topup_state.write().await;
            if state.day != today {
                state.day = today;
                state.spent_wei = 0;
            }
            state.spent_wei = state.spent_wei.saturating_add(topup_wei);
        }

        let refreshed = match provider.get_balance(target_addr, None).await {
            Ok(v) => v.to_string().parse::<u128>().unwrap_or(u128::MAX),
            Err(e) => {
                warn!(
                    "Auto top-up sent but failed to read refreshed balance: {}",
                    e
                );
                return Ok(None);
            }
        };

        let refreshed_alloy = refreshed
            .to_string()
            .parse::<U256>()
            .unwrap_or(current_balance);

        info!(
            "Auto top-up success: tx={:?}, new claimer wallet balance={} wei",
            receipt.transaction_hash, refreshed
        );
        Ok(Some(refreshed_alloy))
    }

    fn encode_ctf_redeem_calldata(condition_id: [u8; 32]) -> Result<Vec<u8>> {
        let function = AbiParser::default()
            .parse_function(
                "function redeemPositions(address collateralToken, bytes32 parentCollectionId, bytes32 conditionId, uint256[] indexSets)",
            )
            .map_err(|e| {
                crate::error::PloyError::Internal(format!("Failed to parse redeem ABI: {}", e))
            })?;

        let usdc_addr: EthersAddress = USDC_E_POLYGON.parse().map_err(|e| {
            crate::error::PloyError::AddressParsing(format!("Invalid USDC.e address: {}", e))
        })?;

        function
            .encode_input(&[
                Token::Address(usdc_addr),
                Token::FixedBytes(vec![0u8; 32]),
                Token::FixedBytes(condition_id.to_vec()),
                Token::Array(vec![
                    Token::Uint(EthersU256::from(1u8)),
                    Token::Uint(EthersU256::from(2u8)),
                ]),
            ])
            .map_err(|e| {
                crate::error::PloyError::Internal(format!(
                    "Failed to encode redeem calldata: {}",
                    e
                ))
            })
    }

    fn encode_proxy_transaction_data(call_to: EthersAddress, call_data: Vec<u8>) -> Result<Vec<u8>> {
        let calls = Token::Array(vec![Token::Tuple(vec![
            Token::Uint(EthersU256::from(1u8)), // CallType.Call
            Token::Address(call_to),
            Token::Uint(EthersU256::zero()),
            Token::Bytes(call_data),
        ])]);
        let encoded_args = abi_encode(&[calls]);
        let selector = &keccak256("proxy((uint8,address,uint256,bytes)[])")[0..4];
        let mut payload = Vec::with_capacity(4 + encoded_args.len());
        payload.extend_from_slice(selector);
        payload.extend_from_slice(&encoded_args);
        Ok(payload)
    }

    fn derive_proxy_wallet_address(signer: EthersAddress) -> Result<EthersAddress> {
        let proxy_factory: EthersAddress = RELAYER_PROXY_FACTORY_POLYGON.parse().map_err(|e| {
            crate::error::PloyError::AddressParsing(format!("Invalid relayer proxy factory: {}", e))
        })?;
        let init_hash: EthersH256 = RELAYER_PROXY_INIT_CODE_HASH.parse().map_err(|e| {
            crate::error::PloyError::AddressParsing(format!(
                "Invalid relayer proxy init code hash: {}",
                e
            ))
        })?;
        let salt = keccak256(signer.as_bytes());
        Ok(ethers_get_create2_address_from_hash(
            proxy_factory,
            salt,
            init_hash.to_fixed_bytes(),
        ))
    }

    fn create_proxy_struct_hash(
        from: EthersAddress,
        to: EthersAddress,
        data: &[u8],
        tx_fee: EthersU256,
        gas_price: EthersU256,
        gas_limit: EthersU256,
        nonce: EthersU256,
        relay_hub: EthersAddress,
        relay: EthersAddress,
    ) -> EthersH256 {
        fn append_u256(out: &mut Vec<u8>, value: EthersU256) {
            let mut buf = [0u8; 32];
            value.to_big_endian(&mut buf);
            out.extend_from_slice(&buf);
        }

        let mut payload = Vec::with_capacity(4 + 20 + 20 + data.len() + 32 * 4 + 20 + 20);
        payload.extend_from_slice(b"rlx:");
        payload.extend_from_slice(from.as_bytes());
        payload.extend_from_slice(to.as_bytes());
        payload.extend_from_slice(data);
        append_u256(&mut payload, tx_fee);
        append_u256(&mut payload, gas_price);
        append_u256(&mut payload, gas_limit);
        append_u256(&mut payload, nonce);
        payload.extend_from_slice(relay_hub.as_bytes());
        payload.extend_from_slice(relay.as_bytes());

        EthersH256::from(keccak256(payload))
    }

    fn build_relayer_builder_headers(
        creds: &RelayerBuilderCredentials,
        timestamp: i64,
        body: &str,
    ) -> Result<HeaderMap> {
        let message = format!("{}POST/submit{}", timestamp, body);
        let signature = relayer_hmac_signature(&creds.secret, &message)?;

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            HeaderName::from_static("poly_builder_api_key"),
            HeaderValue::from_str(&creds.api_key).map_err(|e| {
                crate::error::PloyError::Internal(format!("Invalid builder API key header: {}", e))
            })?,
        );
        headers.insert(
            HeaderName::from_static("poly_builder_passphrase"),
            HeaderValue::from_str(&creds.passphrase).map_err(|e| {
                crate::error::PloyError::Internal(format!(
                    "Invalid builder passphrase header: {}",
                    e
                ))
            })?,
        );
        headers.insert(
            HeaderName::from_static("poly_builder_signature"),
            HeaderValue::from_str(&signature).map_err(|e| {
                crate::error::PloyError::Internal(format!(
                    "Invalid builder signature header: {}",
                    e
                ))
            })?,
        );
        headers.insert(
            HeaderName::from_static("poly_builder_timestamp"),
            HeaderValue::from_str(&timestamp.to_string()).map_err(|e| {
                crate::error::PloyError::Internal(format!(
                    "Invalid builder timestamp header: {}",
                    e
                ))
            })?,
        );
        Ok(headers)
    }

    async fn claim_position_via_relayer_proxy(
        &self,
        pos: &RedeemablePosition,
    ) -> Result<Option<String>> {
        if !relayer_claim_enabled() {
            return Ok(None);
        }

        let Some(builder_creds) = relayer_builder_credentials() else {
            return Ok(None);
        };

        let private_key = self.config.private_key.as_ref().ok_or_else(|| {
            crate::error::PloyError::Wallet("No private key for relayer redeem".into())
        })?;

        let signer_wallet = private_key.parse::<LocalWallet>().map_err(|e| {
            crate::error::PloyError::Wallet(format!("Invalid private key for relayer: {}", e))
        })?
        .with_chain_id(POLYGON_CHAIN_ID);
        let signer_addr = signer_wallet.address();

        let condition_hex = pos
            .condition_id
            .trim()
            .trim_start_matches("0x")
            .trim_start_matches("0X");
        let condition_bytes: [u8; 32] = hex::decode(condition_hex)
            .map_err(|e| crate::error::PloyError::Internal(format!("Invalid condition ID: {}", e)))?
            .try_into()
            .map_err(|_| crate::error::PloyError::Internal("Condition ID wrong length".into()))?;

        let redeem_call_data = Self::encode_ctf_redeem_calldata(condition_bytes)?;
        let ctf_addr: EthersAddress = CONDITIONAL_TOKENS_POLYGON.parse().map_err(|e| {
            crate::error::PloyError::AddressParsing(format!(
                "Invalid ConditionalTokens address: {}",
                e
            ))
        })?;
        let proxy_factory_addr: EthersAddress =
            RELAYER_PROXY_FACTORY_POLYGON.parse().map_err(|e| {
                crate::error::PloyError::AddressParsing(format!(
                    "Invalid relayer proxy factory: {}",
                    e
                ))
            })?;
        let relay_hub_addr: EthersAddress = RELAYER_RELAY_HUB_POLYGON.parse().map_err(|e| {
            crate::error::PloyError::AddressParsing(format!("Invalid relayer hub address: {}", e))
        })?;
        let proxy_wallet = Self::derive_proxy_wallet_address(signer_addr)?;
        let proxy_call_data = Self::encode_proxy_transaction_data(ctf_addr, redeem_call_data)?;

        let polygon_rpc = std::env::var("POLYGON_RPC_URL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| POLYGON_RPC_DEFAULT.to_string());
        let provider = EthersProvider::<EthersHttp>::try_from(polygon_rpc.as_str()).map_err(|e| {
            crate::error::PloyError::AddressParsing(format!("Invalid RPC URL: {}", e))
        })?;

        let gas_estimate_tx: EthersTypedTransaction = EthersTransactionRequest::new()
            .from(signer_addr)
            .to(proxy_factory_addr)
            .data(EthersBytes::from(proxy_call_data.clone()))
            .into();
        let gas_limit = match provider.estimate_gas(&gas_estimate_tx, None).await {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    "Relayer redeem gas estimation failed, using default {}: {}",
                    RELAYER_DEFAULT_GAS_LIMIT, e
                );
                EthersU256::from(RELAYER_DEFAULT_GAS_LIMIT)
            }
        };

        let relayer_url = relayer_base_url();
        let relayer_base = relayer_url.trim_end_matches('/').to_string();
        let http = reqwest::Client::new();

        let relay_payload_resp = http
            .get(format!("{}/relay-payload", relayer_base))
            .query(&[
                ("address", format!("{:#x}", signer_addr)),
                ("type", "PROXY".to_string()),
            ])
            .send()
            .await
            .map_err(crate::error::PloyError::Http)?
            .error_for_status()
            .map_err(crate::error::PloyError::Http)?;
        let relay_payload: RelayerPayloadResponse = relay_payload_resp
            .json()
            .await
            .map_err(crate::error::PloyError::Http)?;

        let relay_addr: EthersAddress = relay_payload.address.parse().map_err(|e| {
            crate::error::PloyError::AddressParsing(format!(
                "Invalid relayer payload address {}: {}",
                relay_payload.address, e
            ))
        })?;
        let nonce = EthersU256::from_dec_str(relay_payload.nonce.trim()).map_err(|e| {
            crate::error::PloyError::Internal(format!(
                "Invalid relayer payload nonce {}: {}",
                relay_payload.nonce, e
            ))
        })?;

        let struct_hash = Self::create_proxy_struct_hash(
            signer_addr,
            proxy_factory_addr,
            &proxy_call_data,
            EthersU256::zero(),
            EthersU256::zero(),
            gas_limit,
            nonce,
            relay_hub_addr,
            relay_addr,
        );
        let signature = signer_wallet
            .sign_message(struct_hash.as_bytes())
            .await
            .map_err(|e| {
                crate::error::PloyError::Signature(format!("Relayer proxy signature failed: {}", e))
            })?
            .to_string();

        let submit_req = RelayerSubmitRequest {
            tx_type: "PROXY".to_string(),
            from: format!("{:#x}", signer_addr),
            to: format!("{:#x}", proxy_factory_addr),
            proxy_wallet: format!("{:#x}", proxy_wallet),
            data: format!("0x{}", hex::encode(proxy_call_data)),
            nonce: relay_payload.nonce,
            signature,
            signature_params: RelayerSignatureParams {
                gas_price: "0".to_string(),
                gas_limit: gas_limit.to_string(),
                relayer_fee: "0".to_string(),
                relay_hub: format!("{:#x}", relay_hub_addr),
                relay: format!("{:#x}", relay_addr),
            },
            metadata: format!("redeem {}", &condition_hex.chars().take(16).collect::<String>()),
        };
        let submit_body = serde_json::to_string(&submit_req)?;
        let ts = Utc::now().timestamp();
        let headers = Self::build_relayer_builder_headers(&builder_creds, ts, &submit_body)?;

        let submit_resp = http
            .post(format!("{}/submit", relayer_base))
            .headers(headers)
            .body(submit_body)
            .send()
            .await
            .map_err(crate::error::PloyError::Http)?
            .error_for_status()
            .map_err(crate::error::PloyError::Http)?;
        let submitted: RelayerSubmitResponse = submit_resp
            .json()
            .await
            .map_err(crate::error::PloyError::Http)?;

        info!(
            "Relayer redeem submitted: id={}, state={}, condition={}",
            submitted.transaction_id,
            submitted.state,
            &condition_hex.chars().take(16).collect::<String>()
        );

        for _ in 0..relayer_poll_max() {
            let status_resp = http
                .get(format!("{}/transaction", relayer_base))
                .query(&[("id", submitted.transaction_id.as_str())])
                .send()
                .await
                .map_err(crate::error::PloyError::Http)?
                .error_for_status()
                .map_err(crate::error::PloyError::Http)?;
            let transactions: Vec<RelayerTransactionStatus> = status_resp
                .json()
                .await
                .map_err(crate::error::PloyError::Http)?;

            if let Some(txn) = transactions.first() {
                match txn.state.as_str() {
                    "STATE_MINED" | "STATE_CONFIRMED" => {
                        let tx_hash = txn
                            .transaction_hash
                            .clone()
                            .or_else(|| submitted.transaction_hash.clone())
                            .unwrap_or_default();
                        info!("Relayer redeem confirmed: state={}, tx={}", txn.state, tx_hash);
                        return Ok(Some(tx_hash));
                    }
                    "STATE_FAILED" | "STATE_INVALID" => {
                        return Err(crate::error::PloyError::OrderSubmission(format!(
                            "Relayer redeem failed: id={}, state={}",
                            submitted.transaction_id, txn.state
                        )));
                    }
                    _ => {}
                }
            }

            sleep(Duration::from_millis(relayer_poll_interval_ms())).await;
        }

        Err(crate::error::PloyError::OrderTimeout(format!(
            "Relayer redeem polling timed out: id={}",
            submitted.transaction_id
        )))
    }

    /// Claim a specific condition by calling the ConditionalTokens redeem function
    async fn claim_position(&self, pos: &RedeemablePosition) -> Result<String> {
        if relayer_claim_enabled() && relayer_builder_credentials().is_some() {
            match self.claim_position_via_relayer_proxy(pos).await {
                Ok(Some(tx_hash)) => return Ok(tx_hash),
                Ok(None) => {}
                Err(e) => {
                    if !relayer_fallback_onchain_enabled() {
                        return Err(e);
                    }
                    warn!(
                        "Relayer redeem failed, falling back to direct on-chain redeem: {}",
                        e
                    );
                }
            }
        }

        let private_key =
            self.config.private_key.as_ref().ok_or_else(|| {
                crate::error::PloyError::Wallet("No private key for claiming".into())
            })?;

        // Parse private key
        let signer: PrivateKeySigner = private_key
            .parse()
            .map_err(|e| crate::error::PloyError::Wallet(format!("Invalid private key: {}", e)))?;

        let wallet = EthereumWallet::from(signer);

        // Connect to Polygon (allow env override for infra-level failover)
        let polygon_rpc = std::env::var("POLYGON_RPC_URL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| POLYGON_RPC_DEFAULT.to_string());
        let rpc_url = polygon_rpc.parse().map_err(|e| {
            crate::error::PloyError::AddressParsing(format!("Invalid RPC URL: {}", e))
        })?;
        let provider = ProviderBuilder::new().wallet(wallet).connect_http(rpc_url);

        let conditional_tokens_addr: Address = CONDITIONAL_TOKENS_POLYGON.parse().map_err(|e| {
            crate::error::PloyError::AddressParsing(format!(
                "Invalid ConditionalTokens address: {}",
                e
            ))
        })?;
        let collateral_addr: Address = USDC_E_POLYGON.parse().map_err(|e| {
            crate::error::PloyError::AddressParsing(format!("Invalid USDC.e address: {}", e))
        })?;

        let contract = IConditionalTokens::new(conditional_tokens_addr, provider);

        // Parse condition ID to bytes32 (accept both raw hex and 0x-prefixed values)
        let condition_hex = pos
            .condition_id
            .trim()
            .trim_start_matches("0x")
            .trim_start_matches("0X");
        let condition_id: [u8; 32] = hex::decode(condition_hex)
            .map_err(|e| crate::error::PloyError::Internal(format!("Invalid condition ID: {}", e)))?
            .try_into()
            .map_err(|_| crate::error::PloyError::Internal("Condition ID wrong length".into()))?;

        // Parent collection ID (usually zero for standard markets)
        let parent_collection_id = [0u8; 32];

        // Index sets for redeeming (1 = first outcome, 2 = second outcome)
        // For binary markets: [1, 2] redeems both outcomes
        let index_sets = vec![U256::from(1), U256::from(2)];

        info!(
            "Calling ConditionalTokens.redeemPositions for condition {} (neg_risk={})...",
            &condition_hex.chars().take(16).collect::<String>(),
            pos.neg_risk
        );

        // Polymarket docs: redeem against ConditionalTokens with collateral token +
        // zero parent collection and indexSets [1,2] for binary outcomes.
        let tx = contract.redeemPositions(
            collateral_addr,
            parent_collection_id.into(),
            condition_id.into(),
            index_sets,
        );

        let pending = tx.send().await.map_err(|e| {
            crate::error::PloyError::OrderSubmission(format!("Redeem tx failed: {}", e))
        })?;

        let receipt = pending.get_receipt().await.map_err(|e| {
            crate::error::PloyError::OrderSubmission(format!("Tx confirmation failed: {}", e))
        })?;

        let tx_hash = format!("{:?}", receipt.transaction_hash);
        info!("Redeem successful! Tx: {}", tx_hash);

        Ok(tx_hash)
    }

    /// Check redeemable positions once (for manual check)
    pub async fn check_once(&self) -> Result<Vec<RedeemablePosition>> {
        self.get_redeemable_positions().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_claimer_config_default() {
        let config = ClaimerConfig::default();
        assert_eq!(config.check_interval_secs, 60);
        assert_eq!(config.min_claim_size, Decimal::ONE);
        assert!(config.auto_claim);
    }

    #[test]
    fn test_redeemable_position() {
        let pos = RedeemablePosition {
            condition_id: "abc123".to_string(),
            token_id: "token456".to_string(),
            outcome: "Yes".to_string(),
            size: dec!(100),
            payout: dec!(100),
            neg_risk: false,
        };

        assert_eq!(pos.size, dec!(100));
        assert!(!pos.neg_risk);
    }

    #[test]
    fn test_collapse_positions_by_condition_merges_duplicate_rows() {
        let positions = vec![
            RedeemablePosition {
                condition_id: "cond-1".to_string(),
                token_id: "tok-a".to_string(),
                outcome: "Yes".to_string(),
                size: dec!(10),
                payout: dec!(10),
                neg_risk: false,
            },
            RedeemablePosition {
                condition_id: "cond-1".to_string(),
                token_id: "tok-b".to_string(),
                outcome: "No".to_string(),
                size: dec!(5),
                payout: dec!(5),
                neg_risk: true,
            },
            RedeemablePosition {
                condition_id: "cond-2".to_string(),
                token_id: "tok-c".to_string(),
                outcome: "Yes".to_string(),
                size: dec!(7),
                payout: dec!(7),
                neg_risk: false,
            },
        ];

        let merged = AutoClaimer::collapse_positions_by_condition(positions);
        assert_eq!(merged.len(), 2);

        let cond1 = merged
            .iter()
            .find(|p| p.condition_id == "cond-1")
            .expect("cond-1 should exist");
        assert_eq!(cond1.size, dec!(15));
        assert_eq!(cond1.payout, dec!(15));
        assert!(cond1.neg_risk);
    }

    #[test]
    fn test_u256_to_u128_saturating() {
        assert_eq!(u256_to_u128_saturating(U256::from(123u64)), 123u128);
    }

    #[test]
    fn test_relayer_hmac_signature_urlsafe_base64() {
        let sig = relayer_hmac_signature("dGVzdHNlY3JldA==", "123POST/submit{\"a\":1}")
            .expect("signature should be created");
        assert_eq!(sig, "5UKMaApqgL6X7RdBVDJLKCU_aDY7kSpONfbGIEZAX0s=");
    }

    #[test]
    fn test_relayer_hmac_signature_accepts_urlsafe_secret_variant() {
        let sig = relayer_hmac_signature(
            "Ndt7ZPLgVWpSzXHGFMohLB33x_Z4qCfqjiMYBwmxamE=",
            "1700000000POST/submit{}",
        )
        .expect("url-safe builder secret should decode");
        assert!(!sig.is_empty());
    }

    #[test]
    fn test_derive_proxy_wallet_address_matches_known_vector() {
        let signer: EthersAddress = "0x9d699747148fd637a7d2514f9b3e3028bf59195c"
            .parse()
            .expect("valid signer");
        let proxy = AutoClaimer::derive_proxy_wallet_address(signer)
            .expect("proxy address should derive correctly");
        assert_eq!(
            format!("{:#x}", proxy),
            "0xcbaaa60c5dec85eac2a2c424bdcd7258ab67eee2"
        );
    }

    #[test]
    fn test_encode_proxy_transaction_data_accepts_tuple_calls() {
        let call_to: EthersAddress = "0x4D97DCd97eC945f40cF65F87097ACe5EA0476045"
            .parse()
            .expect("valid call target");
        let encoded = AutoClaimer::encode_proxy_transaction_data(call_to, vec![0x12, 0x34])
            .expect("proxy calldata should encode");
        assert!(!encoded.is_empty());
    }
}

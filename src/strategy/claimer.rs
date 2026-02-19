//! Auto-claimer for resolved Polymarket positions
//!
//! Monitors for positions that can be redeemed (winning positions after market resolution)
//! and automatically claims them by calling the ConditionalTokens contract.

use alloy::network::EthereumWallet;
use alloy::primitives::{Address, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use alloy::sol;
use rust_decimal::Decimal;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use crate::adapters::PolymarketClient;
use crate::error::Result;

// CTF contracts on Polygon
const CONDITIONAL_TOKENS_POLYGON: &str = "0x4D97DCd97eC945f40cF65F87097ACe5EA0476045";
const USDC_E_POLYGON: &str = "0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174";
const POLYGON_RPC_DEFAULT: &str = "https://polygon-bor-rpc.publicnode.com";
const DEFAULT_MIN_NATIVE_GAS_WEI: u64 = 5_000_000_000_000_000; // 0.005 MATIC buffer

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

fn min_native_gas_wei() -> U256 {
    std::env::var("CLAIMER_MIN_NATIVE_GAS_WEI")
        .ok()
        .and_then(|v| v.trim().parse::<u128>().ok())
        .map(U256::from)
        .unwrap_or_else(|| U256::from(DEFAULT_MIN_NATIVE_GAS_WEI))
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
    running: Arc<RwLock<bool>>,
}

impl AutoClaimer {
    /// Create a new auto-claimer
    pub fn new(client: PolymarketClient, config: ClaimerConfig) -> Self {
        Self {
            client,
            config,
            claimed_conditions: Arc::new(RwLock::new(std::collections::HashSet::new())),
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

        if self.config.auto_claim && !self.preflight_wallet_can_claim().await? {
            return Ok(vec![]);
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

        if balance < min_balance {
            warn!(
                "Auto-claim paused: wallet {} has {} wei, need at least {} wei for gas. Top up MATIC and claimer will resume automatically.",
                wallet_addr,
                balance,
                min_balance
            );
            return Ok(false);
        }

        debug!(
            "Claimer wallet {} gas check passed: {} wei",
            wallet_addr, balance
        );
        Ok(true)
    }

    /// Claim a specific condition by calling the ConditionalTokens redeem function
    async fn claim_position(&self, pos: &RedeemablePosition) -> Result<String> {
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
}

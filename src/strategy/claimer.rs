//! Auto-claimer for resolved Polymarket positions
//!
//! Monitors for positions that can be redeemed (winning positions after market resolution)
//! and automatically claims them by calling the CTFExchange contract.

use std::sync::Arc;
use std::time::Duration;
use alloy::primitives::{Address, U256};
use alloy::providers::ProviderBuilder;
use alloy::network::EthereumWallet;
use alloy::signers::local::PrivateKeySigner;
use alloy::sol;
use rust_decimal::Decimal;
use tokio::sync::RwLock;
use tracing::{info, warn, error, debug};

use crate::adapters::PolymarketClient;
use crate::error::Result;

// CTF Exchange contract addresses on Polygon
const CTF_EXCHANGE_POLYGON: &str = "0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E";
const NEG_RISK_CTF_EXCHANGE_POLYGON: &str = "0xC5d563A36AE78145C45a50134d48A1215220f80a";
const POLYGON_RPC: &str = "https://polygon-rpc.com";

// Generate contract bindings for CTFExchange
sol! {
    #[allow(missing_docs)]
    #[sol(rpc)]
    interface ICTFExchange {
        /// Redeem positions for a resolved condition
        function redeemPositions(
            bytes32 parentCollectionId,
            bytes32 conditionId,
            uint256[] calldata indexSets
        ) external;

        /// Get balance of a token for an account
        function balanceOf(address account, uint256 id) external view returns (uint256);
    }
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

        info!("Starting AutoClaimer (check interval: {}s, auto_claim: {})",
            self.config.check_interval_secs,
            self.config.auto_claim);

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
                            info!("Claimed ${:.2} from condition {}",
                                result.amount_claimed, result.condition_id);
                        } else {
                            warn!("Failed to claim condition {}: {:?}",
                                result.condition_id, result.error);
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
        let positions = self.get_redeemable_positions().await?;

        if positions.is_empty() {
            debug!("No redeemable positions found");
            return Ok(vec![]);
        }

        info!("Found {} redeemable positions", positions.len());

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
            info!("Redeemable: {} - {} shares = ${:.2}",
                pos.outcome, pos.size, pos.payout);

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
                info!("[DRY RUN] Would claim ${:.2} from {}", pos.payout, pos.condition_id);
            }
        }

        Ok(results)
    }

    /// Get list of redeemable positions from Polymarket
    async fn get_redeemable_positions(&self) -> Result<Vec<RedeemablePosition>> {
        // Use the Data API to get positions
        let positions = self.client.get_positions().await?;

        let mut redeemable = Vec::new();

        for p in positions {
            // Check if position has shares
            let size: Decimal = match p.size.parse() {
                Ok(s) if s > Decimal::ZERO => s,
                _ => continue,
            };

            // Check if position is redeemable (API may provide this flag)
            // Or check if current price = 1.0 (winning side)
            let is_winner = p.cur_price.as_ref()
                .and_then(|price_str| price_str.parse::<f64>().ok())
                .map(|price| price > 0.99) // Winner = price ~1.0
                .unwrap_or(false);

            // Also check the redeemable flag if available
            let api_says_redeemable = p.is_redeemable();

            if !is_winner && !api_says_redeemable {
                continue;
            }

            let payout = size; // Each winning share = $1

            redeemable.push(RedeemablePosition {
                condition_id: p.condition_id.clone().unwrap_or_default(),
                token_id: p.token_id.clone().unwrap_or_else(|| p.asset_id.clone()),
                outcome: p.outcome.clone().unwrap_or_default(),
                size,
                payout,
                neg_risk: false, // TODO: detect from market type
            });

            info!(
                "Found redeemable position: {} {} shares, condition={}",
                p.outcome.clone().unwrap_or_default(),
                size,
                p.condition_id.clone().unwrap_or_default().chars().take(16).collect::<String>()
            );
        }

        Ok(redeemable)
    }

    /// Claim a specific position by calling the CTFExchange contract
    async fn claim_position(&self, pos: &RedeemablePosition) -> Result<String> {
        let private_key = self.config.private_key.as_ref()
            .ok_or_else(|| crate::error::PloyError::Wallet("No private key for claiming".into()))?;

        // Parse private key
        let signer: PrivateKeySigner = private_key.parse()
            .map_err(|e| crate::error::PloyError::Wallet(format!("Invalid private key: {}", e)))?;

        let wallet = EthereumWallet::from(signer);

        // Connect to Polygon
        let provider = ProviderBuilder::new()
            .wallet(wallet)
            .connect_http(POLYGON_RPC.parse().unwrap());

        // Select appropriate exchange contract
        let exchange_addr: Address = if pos.neg_risk {
            NEG_RISK_CTF_EXCHANGE_POLYGON.parse().unwrap()
        } else {
            CTF_EXCHANGE_POLYGON.parse().unwrap()
        };

        let contract = ICTFExchange::new(exchange_addr, provider);

        // Parse condition ID to bytes32
        let condition_id: [u8; 32] = hex::decode(&pos.condition_id)
            .map_err(|e| crate::error::PloyError::Internal(format!("Invalid condition ID: {}", e)))?
            .try_into()
            .map_err(|_| crate::error::PloyError::Internal("Condition ID wrong length".into()))?;

        // Parent collection ID (usually zero for standard markets)
        let parent_collection_id = [0u8; 32];

        // Index sets for redeeming (1 = first outcome, 2 = second outcome)
        // For binary markets: [1, 2] redeems both outcomes
        let index_sets = vec![U256::from(1), U256::from(2)];

        info!("Calling redeemPositions on {} for condition {}...",
            if pos.neg_risk { "NegRisk CTF Exchange" } else { "CTF Exchange" },
            &pos.condition_id[..16]);

        // Call redeem
        let tx = contract.redeemPositions(
            parent_collection_id.into(),
            condition_id.into(),
            index_sets,
        );

        let pending = tx.send().await
            .map_err(|e| crate::error::PloyError::OrderSubmission(format!("Redeem tx failed: {}", e)))?;

        let receipt = pending.get_receipt().await
            .map_err(|e| crate::error::PloyError::OrderSubmission(format!("Tx confirmation failed: {}", e)))?;

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
}

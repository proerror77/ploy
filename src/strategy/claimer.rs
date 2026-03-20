//! Auto-claimer for resolved Polymarket positions
//!
//! Monitors for positions that can be redeemed (winning positions after market resolution)
//! and automatically claims them by calling the ConditionalTokens contract.

use alloy::network::EthereumWallet;
use alloy::primitives::{Address, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::TransactionRequest as AlloyTransactionRequest;
use alloy::signers::{local::PrivateKeySigner, Signer as _};
use alloy::sol;
use chrono::{NaiveDate, Utc};
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use crate::adapters::PolymarketClient;
use crate::error::Result;
use crate::signing::Wallet;

mod claim_flow;
mod discovery;
mod relayer;

use self::relayer::{
    missing_relayer_builder_credential_groups, relayer_base_url,
    relayer_builder_credentials_available, relayer_claim_enabled, relayer_fallback_onchain_enabled,
};

mod claim_flow;
mod daemon;
mod discovery;
mod relayer;

pub use self::daemon::ensure_account_claimer_daemon;
pub(crate) use self::daemon::{
    auto_topup_enabled, env_flag, env_string_any, env_u128_any, env_u64_any, min_native_gas_wei,
    needs_native_gas_preflight, u256_to_u128_saturating,
};
use self::relayer::{
    missing_relayer_builder_credential_groups, relayer_base_url,
    relayer_builder_credentials_available, relayer_claim_enabled, relayer_fallback_onchain_enabled,
};

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

static ACCOUNT_CLAIMER_DAEMON_STARTED: OnceLock<AtomicBool> = OnceLock::new();

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

/// Start one global account-level auto-claimer daemon (process-wide).
///
/// This is intentionally strategy-agnostic: it scans the account for redeemable
/// positions every minute (configurable), independent of any strategy lifecycle.
pub async fn ensure_account_claimer_daemon() -> Result<()> {
    if !env_flag("CLAIMER_DAEMON_ENABLED", true) {
        return Ok(());
    }

    let gate = ACCOUNT_CLAIMER_DAEMON_STARTED.get_or_init(|| AtomicBool::new(false));
    if gate
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Ok(());
    }

    let private_key = std::env::var("POLYMARKET_PRIVATE_KEY")
        .or_else(|_| std::env::var("PRIVATE_KEY"))
        .ok();
    let Some(private_key) = private_key else {
        warn!("Auto-claimer daemon disabled: no POLYMARKET_PRIVATE_KEY/PRIVATE_KEY");
        gate.store(false, Ordering::SeqCst);
        return Ok(());
    };

    let wallet = match Wallet::from_env(POLYGON_CHAIN_ID) {
        Ok(w) => w,
        Err(e) => {
            warn!("Auto-claimer daemon wallet init failed: {}", e);
            gate.store(false, Ordering::SeqCst);
            return Ok(());
        }
    };

    let funder = std::env::var("POLYMARKET_FUNDER")
        .or_else(|_| std::env::var("POLYMARKET_FUNDER_ADDRESS"))
        .ok();
    let client = if let Some(ref funder_addr) = funder {
        match PolymarketClient::new_authenticated_proxy(
            "https://clob.polymarket.com",
            wallet,
            funder_addr,
            true,
        )
        .await
        {
            Ok(c) => c,
            Err(e) => {
                gate.store(false, Ordering::SeqCst);
                return Err(e);
            }
        }
    } else {
        match PolymarketClient::new_authenticated("https://clob.polymarket.com", wallet, true).await
        {
            Ok(c) => c,
            Err(e) => {
                gate.store(false, Ordering::SeqCst);
                return Err(e);
            }
        }
    };

    let interval_secs = env_u64_any(&["CLAIMER_CHECK_INTERVAL_SECS", "CLAIMER_INTERVAL_SECS"])
        .unwrap_or(60)
        .max(10);
    let min_claim_size = env_string_any(&["CLAIMER_MIN_CLAIM_SIZE", "CLAIMER_MIN_SIZE_USDC"])
        .and_then(|v| Decimal::from_str(v.trim()).ok())
        .unwrap_or(Decimal::ONE);

    let claimer = AutoClaimer::new(
        client,
        ClaimerConfig {
            check_interval_secs: interval_secs,
            min_claim_size,
            auto_claim: true,
            private_key: Some(private_key),
        },
    );

    let gate_ref = ACCOUNT_CLAIMER_DAEMON_STARTED
        .get()
        .expect("claimer gate should be initialized");
    tokio::spawn(async move {
        if let Err(e) = claimer.start().await {
            error!("Auto-claimer daemon stopped with error: {}", e);
            gate_ref.store(false, Ordering::SeqCst);
        }
    });

    info!(
        interval_secs,
        min_claim_size = %min_claim_size,
        "Auto-claimer daemon started (account-level)"
    );

    Ok(())
}

fn u256_to_u128_saturating(value: U256) -> u128 {
    value.to_string().parse::<u128>().unwrap_or(u128::MAX)
}

fn needs_native_gas_preflight(auto_claim: bool, relayer_ready: bool) -> bool {
    auto_claim && !relayer_ready
}

#[derive(Debug, Clone)]
struct GasTopupState {
    day: NaiveDate,
    spent_wei: u128,
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
        let discovery::EligiblePositions {
            positions: eligible,
            skipped_small,
        } = discovery::discover_eligible_positions(&self.client, self.config.min_claim_size)
            .await?;

        if eligible.is_empty() {
            return Ok(vec![]);
        }

        let relayer_enabled = relayer_claim_enabled();
        let relayer_ready = relayer_enabled && relayer_builder_credentials_available();
        if self.config.auto_claim {
            if relayer_ready {
                info!(
                    relayer_url = %relayer_base_url(),
                    onchain_fallback = relayer_fallback_onchain_enabled(),
                    "Gasless redeem path enabled (Builder relayer)"
                );
            } else if relayer_enabled {
                let missing = missing_relayer_builder_credential_groups().join(",");
                warn!(
                    missing = %missing,
                    "Relayer redeem is enabled but builder credentials are incomplete; claimer will require native MATIC for on-chain redeem"
                );
            } else {
                warn!(
                    "Relayer redeem is disabled; claimer will use direct on-chain redeem and require native MATIC gas"
                );
            }
        }

        let preflight_required = needs_native_gas_preflight(self.config.auto_claim, relayer_ready);
        if preflight_required && !self.preflight_wallet_can_claim().await? {
            return Ok(vec![]);
        }
        if self.config.auto_claim && relayer_ready {
            debug!(
                "Relayer redeem is enabled; deferring native gas preflight unless on-chain fallback is needed"
            );
        }

        info!(
            eligible = eligible.len(),
            min_claim_size = %self.config.min_claim_size,
            skipped_small,
            "Found redeemable conditions"
        );

        let mut results = Vec::new();
        let mut abort_remaining_claims = false;

        for pos in eligible {
            if abort_remaining_claims {
                debug!(
                    "Skipping remaining redeemable conditions in this cycle after terminal claim error"
                );
                break;
            }
            // Skip if already claimed
            {
                let claimed = self.claimed_conditions.read().await;
                if claimed.contains(&pos.condition_id) {
                    debug!("Already claimed condition {}", pos.condition_id);
                    continue;
                }
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
                        let err_text = e.to_string();
                        let terminal_cycle_error = err_text
                            .contains("Relayer submit failed: status=429")
                            || err_text
                                .contains("Insufficient native gas for on-chain fallback redeem");
                        results.push(ClaimResult {
                            condition_id: pos.condition_id,
                            amount_claimed: Decimal::ZERO,
                            tx_hash: String::new(),
                            success: false,
                            error: Some(err_text.clone()),
                        });
                        if terminal_cycle_error {
                            warn!(
                                "Stopping further claim attempts in this cycle after terminal error: {}",
                                err_text
                            );
                            abort_remaining_claims = true;
                        }
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

    /// Preflight signer wallet native balance to avoid spamming failed redeem txs.
    async fn preflight_wallet_can_claim(&self) -> Result<bool> {
        claim_flow::preflight_wallet_can_claim(self).await
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
        claim_flow::maybe_auto_topup_wallet(self, target_wallet, current_balance, min_balance).await
    }

    /// Claim a specific condition by calling the ConditionalTokens redeem function
    async fn claim_position(&self, pos: &RedeemablePosition) -> Result<String> {
        claim_flow::claim_position(self, pos).await
    }

    /// Check redeemable positions once (for manual check)
    pub async fn check_once(&self) -> Result<Vec<RedeemablePosition>> {
        discovery::get_redeemable_positions(&self.client).await
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

        let merged = discovery::collapse_positions_by_condition(positions);
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
    fn test_needs_native_gas_preflight() {
        assert!(!needs_native_gas_preflight(false, false));
        assert!(needs_native_gas_preflight(true, false));
        assert!(!needs_native_gas_preflight(true, true));
    }

    #[test]
    fn test_condition_ignore_prefix_matching() {
        let patterns = vec!["04e8fab12c2e30".to_string()];
        assert!(discovery::condition_is_ignored(
            "0x04e8fab12c2e30d06292db90b9c16f5526deac27b96345f15ccc7ba0bdb16c17",
            &patterns
        ));
        assert!(!discovery::condition_is_ignored(
            "0x0ab116e9d0401a0000000000000000000000000000000000000000000000000",
            &patterns
        ));
    }
}

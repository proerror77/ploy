//! Auto-claimer for resolved Polymarket positions.
//!
//! Monitors for positions that can be redeemed (winning positions after market
//! resolution) and automatically claims them by calling the ConditionalTokens
//! contract on Polygon.
//!
//! # Quick start
//!
//! Call [`ensure_account_claimer_daemon`] once at startup (gated by
//! `CLAIMER_DAEMON_ENABLED`, default `false`). It spawns a background tokio
//! task that scans for redeemable positions every `CLAIMER_CHECK_INTERVAL_SECS`
//! seconds (default 60).

use alloy::network::EthereumWallet;
use alloy::primitives::{Address, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::TransactionRequest as AlloyTransactionRequest;
use alloy::signers::{Signer as _, local::PrivateKeySigner};
use alloy::sol;
use chrono::{NaiveDate, Utc};
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

mod claim_flow;
mod discovery;
mod relayer;

use self::relayer::{
    missing_relayer_builder_credential_groups, relayer_base_url,
    relayer_builder_credentials_available, relayer_claim_enabled, relayer_fallback_onchain_enabled,
};

// CTF contracts on Polygon
pub(crate) const CONDITIONAL_TOKENS_POLYGON: &str = "0x4D97DCd97eC945f40cF65F87097ACe5EA0476045";
pub(crate) const NEG_RISK_ADAPTER_POLYGON: &str = "0xd91E80cF2E7be2e162c6513ceD06f1dD0dA35296";
pub(crate) const PUSD_POLYGON: &str = "0xC011a7E12a19f7B1f670d46F03B03f3342E82DFB";
/// Collateral onramp contract — call wrap() to convert USDC.e → pUSD (not yet wired)
pub(crate) const COLLATERAL_ONRAMP: &str = "0x93070a847efEf7F70739046A929D47a521F5B8ee";
pub(crate) const POLYGON_RPC_DEFAULT: &str = "https://polygon-bor-rpc.publicnode.com";
pub(crate) const POLYGON_CHAIN_ID: u64 = 137;
const DEFAULT_MIN_NATIVE_GAS_WEI: u64 = 5_000_000_000_000_000; // 0.005 MATIC
const DEFAULT_AUTO_TOPUP_TARGET_WEI: u128 = 20_000_000_000_000_000; // 0.02 MATIC
const DEFAULT_AUTO_TOPUP_MAX_PER_TX_WEI: u128 = 20_000_000_000_000_000; // 0.02 MATIC
const DEFAULT_AUTO_TOPUP_DAILY_CAP_WEI: u128 = 100_000_000_000_000_000; // 0.1 MATIC
const DEFAULT_AUTO_TOPUP_RESERVE_WEI: u128 = 5_000_000_000_000_000; // 0.005 MATIC reserve

static ACCOUNT_CLAIMER_DAEMON_STARTED: OnceLock<AtomicBool> = OnceLock::new();

// Generate contract bindings for ConditionalTokens
sol! {
    #[allow(missing_docs)]
    #[sol(rpc)]
    interface IConditionalTokens {
        function redeemPositions(
            address collateralToken,
            bytes32 parentCollectionId,
            bytes32 conditionId,
            uint256[] calldata indexSets
        ) external;

        function balanceOf(address account, uint256 id) external view returns (uint256);
    }

    #[allow(missing_docs)]
    #[sol(rpc)]
    interface INegRiskAdapter {
        function redeemPositions(
            bytes32 conditionId,
            uint256[] calldata amounts
        ) external;
    }
}

pub(crate) fn env_flag(name: &str, default: bool) -> bool {
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

pub(crate) fn env_string_any(keys: &[&str]) -> Option<String> {
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

pub(crate) fn env_u128_any(keys: &[&str]) -> Option<u128> {
    for key in keys {
        if let Ok(v) = std::env::var(key) {
            if let Ok(parsed) = v.trim().parse::<u128>() {
                return Some(parsed);
            }
        }
    }
    None
}

pub(crate) fn env_u64_any(keys: &[&str]) -> Option<u64> {
    for key in keys {
        if let Ok(v) = std::env::var(key) {
            if let Ok(parsed) = v.trim().parse::<u64>() {
                return Some(parsed);
            }
        }
    }
    None
}

pub(crate) fn claim_allow_price_fallback() -> bool {
    env_flag("CLAIMER_ALLOW_PRICE_FALLBACK", false)
}

pub(crate) fn max_claims_per_cycle() -> Option<usize> {
    env_u64_any(&["CLAIMER_MAX_CLAIMS_PER_CYCLE"])
        .map(|value| value as usize)
        .filter(|value| *value > 0)
}

pub(crate) fn max_claim_payout_per_cycle() -> Option<Decimal> {
    env_string_any(&[
        "CLAIMER_MAX_PAYOUT_PER_CYCLE_USDC",
        "CLAIMER_MAX_CLAIM_PAYOUT_USDC",
    ])
    .and_then(|value| Decimal::from_str(value.trim()).ok())
    .filter(|value| *value > Decimal::ZERO)
}

pub(crate) fn min_native_gas_wei() -> U256 {
    std::env::var("CLAIMER_MIN_NATIVE_GAS_WEI")
        .ok()
        .and_then(|v| v.trim().parse::<u128>().ok())
        .map(U256::from)
        .unwrap_or_else(|| U256::from(DEFAULT_MIN_NATIVE_GAS_WEI))
}

pub(crate) fn auto_topup_enabled() -> bool {
    env_flag(
        "CLAIMER_AUTO_TOPUP_ENABLED",
        env_flag("CLAIMER_GAS_TOPUP_ENABLED", false),
    )
}

/// Start one global account-level auto-claimer daemon (process-wide singleton).
///
/// Safe to call multiple times — only the first call spawns the daemon.
/// Reads configuration from environment variables:
/// - `CLAIMER_DAEMON_ENABLED` (default: false)
/// - `POLYMARKET_PRIVATE_KEY` or `PRIVATE_KEY`
/// - `POLYMARKET_FUNDER` or `POLYMARKET_FUNDER_ADDRESS` (optional proxy wallet)
/// - `CLAIMER_CHECK_INTERVAL_SECS` (default: 300, min: 60)
/// - `CLAIMER_MIN_CLAIM_SIZE` (default: 1 USDC)
/// - `CLAIMER_MAX_CLAIMS_PER_CYCLE` (optional hard cap)
/// - `CLAIMER_MAX_PAYOUT_PER_CYCLE_USDC` (optional hard cap)
pub async fn ensure_account_claimer_daemon() -> Result<(), ClaimerError> {
    if !env_flag("CLAIMER_DAEMON_ENABLED", false) {
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

    // Derive wallet address from private key for the Data API user filter
    let signer = PrivateKeySigner::from_str(&private_key)
        .map_err(|e| ClaimerError::Wallet(format!("Invalid private key: {e}")))?;
    let wallet_address = signer.address();

    let funder_str = std::env::var("POLYMARKET_FUNDER")
        .or_else(|_| std::env::var("POLYMARKET_FUNDER_ADDRESS"))
        .ok();

    // Use funder address for position lookup if proxy wallet is configured
    let lookup_address = if let Some(ref funder) = funder_str {
        Address::from_str(funder)
            .map_err(|e| ClaimerError::Wallet(format!("Invalid POLYMARKET_FUNDER address: {e}")))?
    } else {
        wallet_address
    };

    let interval_secs = env_u64_any(&["CLAIMER_CHECK_INTERVAL_SECS", "CLAIMER_INTERVAL_SECS"])
        .unwrap_or(300) // Default: check every 5 minutes (matches 5-min market settlement cycle)
        .max(60); // Minimum 60s to avoid accidental hammering
    let min_claim_size = env_string_any(&["CLAIMER_MIN_CLAIM_SIZE", "CLAIMER_MIN_SIZE_USDC"])
        .and_then(|v| Decimal::from_str(v.trim()).ok())
        .unwrap_or(Decimal::ONE);
    let max_claims_per_cycle = max_claims_per_cycle();
    let max_payout_per_cycle_usdc = max_claim_payout_per_cycle();

    let claimer = AutoClaimer::new(
        lookup_address,
        ClaimerConfig {
            check_interval_secs: interval_secs,
            min_claim_size,
            max_claims_per_cycle,
            max_payout_per_cycle_usdc,
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
        max_claims_per_cycle = ?max_claims_per_cycle,
        max_payout_per_cycle_usdc = ?max_payout_per_cycle_usdc,
        %wallet_address,
        %lookup_address,
        "Auto-claimer daemon started (account-level)"
    );

    Ok(())
}

pub(crate) fn u256_to_u128_saturating(value: U256) -> u128 {
    value.to_string().parse::<u128>().unwrap_or(u128::MAX)
}

pub(crate) fn decimal_to_token_units(amount: Decimal) -> Result<u128, ClaimerError> {
    if amount < Decimal::ZERO {
        return Err(ClaimerError::Internal(format!(
            "Cannot encode negative claim amount: {}",
            amount
        )));
    }

    let scaled = (amount * Decimal::from(1_000_000u64)).round_dp(0);
    let raw = scaled.to_string();
    let parsed = raw.parse::<u128>().map_err(|e| {
        ClaimerError::Internal(format!("Invalid scaled claim amount {}: {}", raw, e))
    })?;
    Ok(parsed)
}

pub(crate) fn needs_native_gas_preflight(auto_claim: bool, relayer_ready: bool) -> bool {
    auto_claim && !relayer_ready
}

#[derive(Debug, Clone)]
pub(crate) struct GasTopupState {
    pub day: NaiveDate,
    pub spent_wei: u128,
}

/// Position that can be redeemed.
#[derive(Debug, Clone)]
pub struct RedeemablePosition {
    pub condition_id: String,
    pub token_id: String,
    pub outcome: String,
    pub outcome_index: usize,
    pub size: Decimal,
    pub payout: Decimal,
    pub claim_amounts: Vec<Decimal>,
    pub neg_risk: bool,
}

/// Result of a single claim operation.
#[derive(Debug, Clone)]
pub struct ClaimResult {
    pub condition_id: String,
    pub amount_claimed: Decimal,
    pub tx_hash: String,
    pub success: bool,
    pub error: Option<String>,
}

/// Errors produced by the claimer.
#[derive(Debug, thiserror::Error)]
pub enum ClaimerError {
    #[error("wallet error: {0}")]
    Wallet(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("contract error: {0}")]
    Contract(String),
    #[error("internal error: {0}")]
    Internal(String),
}

/// Auto-claimer configuration.
#[derive(Clone)]
pub struct ClaimerConfig {
    /// How often to check for redeemable positions (seconds, min 10).
    pub check_interval_secs: u64,
    /// Minimum position size to claim (avoid dust).
    pub min_claim_size: Decimal,
    /// Maximum claim attempts allowed per scan cycle.
    pub max_claims_per_cycle: Option<usize>,
    /// Maximum total payout allowed per scan cycle.
    pub max_payout_per_cycle_usdc: Option<Decimal>,
    /// Whether to claim automatically or just report.
    pub auto_claim: bool,
    /// Private key for signing transactions.
    pub private_key: Option<String>,
}

impl std::fmt::Debug for ClaimerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaimerConfig")
            .field("check_interval_secs", &self.check_interval_secs)
            .field("min_claim_size", &self.min_claim_size)
            .field("max_claims_per_cycle", &self.max_claims_per_cycle)
            .field("max_payout_per_cycle_usdc", &self.max_payout_per_cycle_usdc)
            .field("auto_claim", &self.auto_claim)
            .field(
                "private_key",
                &self.private_key.as_ref().map(|_| "[redacted]"),
            )
            .finish()
    }
}

impl Default for ClaimerConfig {
    fn default() -> Self {
        Self {
            check_interval_secs: 60,
            min_claim_size: Decimal::ONE,
            max_claims_per_cycle: None,
            max_payout_per_cycle_usdc: None,
            auto_claim: true,
            private_key: None,
        }
    }
}

/// Auto-claimer for Polymarket positions.
pub struct AutoClaimer {
    /// Polygon address used to query positions (proxy wallet or EOA).
    lookup_address: Address,
    config: ClaimerConfig,
    claimed_conditions: Arc<RwLock<std::collections::HashSet<String>>>,
    pub(crate) gas_topup_state: Arc<RwLock<GasTopupState>>,
    running: Arc<RwLock<bool>>,
}

impl AutoClaimer {
    /// Create a new auto-claimer.
    pub fn new(lookup_address: Address, config: ClaimerConfig) -> Self {
        Self {
            lookup_address,
            config,
            claimed_conditions: Arc::new(RwLock::new(std::collections::HashSet::new())),
            gas_topup_state: Arc::new(RwLock::new(GasTopupState {
                day: Utc::now().date_naive(),
                spent_wei: 0,
            })),
            running: Arc::new(RwLock::new(false)),
        }
    }

    /// Start the auto-claimer background loop (runs until stopped).
    pub async fn start(&self) -> Result<(), ClaimerError> {
        {
            let mut running = self.running.write().await;
            if *running {
                info!("AutoClaimer already running");
                return Ok(());
            }
            *running = true;
        }

        info!(
            "Starting AutoClaimer (check interval: {}s, auto_claim: {})",
            self.config.check_interval_secs, self.config.auto_claim
        );

        loop {
            if !*self.running.read().await {
                break;
            }

            let sleep_secs = match self.check_and_claim().await {
                Ok(results) => {
                    let mut had_relayer_limit = false;
                    let mut had_no_gas = false;
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
                            if let Some(ref err) = result.error {
                                if err.contains("status=429") || err.contains("quota exceeded") {
                                    had_relayer_limit = true;
                                } else if err.contains("Insufficient native gas") {
                                    had_no_gas = true;
                                }
                            }
                        }
                    }
                    // Back off based on failure type to avoid burning relayer quota.
                    // 5-minute markets settle every 5 minutes, so normal interval = 5 min.
                    if had_relayer_limit {
                        let backoff = env_u64_any(&["CLAIMER_RATE_LIMIT_BACKOFF_SECS"])
                            .unwrap_or(600) // default 10 min — enough to clear quota without missing many settlement windows
                            .max(60);
                        warn!(
                            "Relayer quota exhausted — backing off {}s before next claim attempt",
                            backoff
                        );
                        backoff
                    } else if had_no_gas {
                        warn!("No MATIC gas — backing off 10 minutes before next claim attempt");
                        600 // 10 minutes
                    } else {
                        self.config.check_interval_secs
                    }
                }
                Err(e) => {
                    error!("Error checking redeemable positions: {}", e);
                    self.config.check_interval_secs
                }
            };

            tokio::time::sleep(Duration::from_secs(sleep_secs)).await;
        }

        info!("AutoClaimer stopped");
        Ok(())
    }

    /// Stop the auto-claimer loop.
    pub async fn stop(&self) {
        *self.running.write().await = false;
    }

    /// Check for redeemable positions and optionally claim them.
    pub async fn check_and_claim(&self) -> Result<Vec<ClaimResult>, ClaimerError> {
        let discovery::EligiblePositions {
            positions: eligible,
            skipped_small,
            skipped_cycle_limited,
        } = discovery::discover_eligible_positions(self.lookup_address, self.config.min_claim_size)
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
                    "Relayer redeem enabled but builder credentials incomplete; will use on-chain redeem (needs MATIC)"
                );
            } else {
                warn!("Relayer redeem disabled; using direct on-chain redeem (needs MATIC gas)");
            }
        }

        let preflight_required = needs_native_gas_preflight(self.config.auto_claim, relayer_ready);
        if preflight_required && !self.preflight_wallet_can_claim().await? {
            return Ok(vec![]);
        }

        info!(
            eligible = eligible.len(),
            min_claim_size = %self.config.min_claim_size,
            skipped_small,
            skipped_cycle_limited,
            max_claims_per_cycle = ?self.config.max_claims_per_cycle,
            max_payout_per_cycle_usdc = ?self.config.max_payout_per_cycle_usdc,
            "Found redeemable conditions"
        );

        let mut results = Vec::new();
        let mut abort_remaining = false;

        for pos in eligible {
            if abort_remaining {
                debug!("Skipping remaining conditions after terminal claim error");
                break;
            }
            {
                let claimed = self.claimed_conditions.read().await;
                if claimed.contains(&pos.condition_id) {
                    debug!("Already claimed condition {}", pos.condition_id);
                    continue;
                }
            }

            info!(
                "Redeemable: {} - {} shares = ${:.2}",
                pos.outcome, pos.size, pos.payout
            );

            if self.config.auto_claim {
                match self.claim_position(&pos).await {
                    Ok(tx_hash) => {
                        self.claimed_conditions
                            .write()
                            .await
                            .insert(pos.condition_id.clone());
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
                        let terminal = err_text.contains("Relayer submit failed: status=429")
                            || err_text
                                .contains("Insufficient native gas for on-chain fallback redeem");
                        results.push(ClaimResult {
                            condition_id: pos.condition_id,
                            amount_claimed: Decimal::ZERO,
                            tx_hash: String::new(),
                            success: false,
                            error: Some(err_text.clone()),
                        });
                        if terminal {
                            warn!(
                                "Stopping further claims this cycle after terminal error: {}",
                                err_text
                            );
                            abort_remaining = true;
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

    async fn preflight_wallet_can_claim(&self) -> Result<bool, ClaimerError> {
        claim_flow::preflight_wallet_can_claim(self).await
    }

    pub(crate) async fn maybe_auto_topup_wallet(
        &self,
        target_wallet: Address,
        current_balance: U256,
        min_balance: U256,
    ) -> Result<Option<U256>, ClaimerError> {
        claim_flow::maybe_auto_topup_wallet(self, target_wallet, current_balance, min_balance).await
    }

    async fn claim_position(&self, pos: &RedeemablePosition) -> Result<String, ClaimerError> {
        claim_flow::claim_position(self, pos).await
    }

    /// One-shot check for redeemable positions (no claiming).
    pub async fn check_once(&self) -> Result<Vec<RedeemablePosition>, ClaimerError> {
        discovery::get_redeemable_positions(self.lookup_address).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn claimer_config_default() {
        let config = ClaimerConfig::default();
        assert_eq!(config.check_interval_secs, 60);
        assert_eq!(config.min_claim_size, Decimal::ONE);
        assert_eq!(config.max_claims_per_cycle, None);
        assert_eq!(config.max_payout_per_cycle_usdc, None);
        assert!(config.auto_claim);
    }

    #[test]
    fn needs_native_gas_preflight_logic() {
        assert!(!needs_native_gas_preflight(false, false));
        assert!(needs_native_gas_preflight(true, false));
        assert!(!needs_native_gas_preflight(true, true));
    }

    #[test]
    fn collapse_positions_merges_duplicate_conditions() {
        let positions = vec![
            RedeemablePosition {
                condition_id: "cond-1".to_string(),
                token_id: "tok-a".to_string(),
                outcome: "Yes".to_string(),
                outcome_index: 0,
                size: dec!(10),
                payout: dec!(10),
                claim_amounts: vec![dec!(10)],
                neg_risk: false,
            },
            RedeemablePosition {
                condition_id: "cond-1".to_string(),
                token_id: "tok-b".to_string(),
                outcome: "No".to_string(),
                outcome_index: 1,
                size: dec!(5),
                payout: dec!(5),
                claim_amounts: vec![Decimal::ZERO, dec!(5)],
                neg_risk: true,
            },
        ];
        let merged = discovery::collapse_positions_by_condition(positions);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].size, dec!(15));
        assert!(merged[0].neg_risk);
        assert_eq!(merged[0].claim_amounts, vec![dec!(10), dec!(5)]);
    }

    #[test]
    fn claimer_config_debug_redacts_private_key() {
        let config = ClaimerConfig {
            check_interval_secs: 60,
            min_claim_size: Decimal::ONE,
            max_claims_per_cycle: Some(2),
            max_payout_per_cycle_usdc: Some(dec!(10)),
            auto_claim: true,
            private_key: Some("0xdeadbeef".to_string()),
        };
        let debug = format!("{config:?}");
        assert!(!debug.contains("0xdeadbeef"));
        assert!(debug.contains("[redacted]"));
        assert!(debug.contains("max_claims_per_cycle"));
    }
}

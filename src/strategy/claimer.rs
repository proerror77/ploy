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

mod discovery;
mod relayer;

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
        let rpc_url = match polygon_rpc.parse() {
            Ok(url) => url,
            Err(e) => {
                warn!("Auto top-up skipped: invalid POLYGON_RPC_URL: {}", e);
                return Ok(None);
            }
        };

        let topup_signer = match topup_private_key.parse::<PrivateKeySigner>() {
            Ok(signer) => signer.with_chain_id(Some(POLYGON_CHAIN_ID)),
            Err(e) => {
                warn!("Auto top-up skipped: invalid top-up private key: {}", e);
                return Ok(None);
            }
        };
        let topup_addr = topup_signer.address();
        let wallet_provider = ProviderBuilder::new()
            .wallet(EthereumWallet::from(topup_signer))
            .connect_http(rpc_url);

        let topup_balance_wei = match wallet_provider.get_balance(topup_addr).await {
            Ok(v) => u256_to_u128_saturating(v),
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

        info!(
            "Auto top-up triggered: sending {} wei to claimer wallet {} (current={} threshold={} target={})",
            topup_wei, target_wallet, current_wei, threshold_wei, target_wei
        );

        let tx = AlloyTransactionRequest::default()
            .to(target_wallet)
            .value(U256::from(topup_wei));

        let pending_tx = match wallet_provider.send_transaction(tx).await {
            Ok(p) => p,
            Err(e) => {
                warn!("Auto top-up tx submission failed: {}", e);
                return Ok(None);
            }
        };

        let receipt = match pending_tx.get_receipt().await {
            Ok(r) => r,
            Err(e) => {
                warn!("Auto top-up tx confirmation failed: {}", e);
                return Ok(None);
            }
        };

        if !receipt.status() {
            warn!(
                "Auto top-up tx reverted: hash={:?}, status={}",
                receipt.transaction_hash,
                receipt.status()
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

        let refreshed_alloy = match wallet_provider.get_balance(target_wallet).await {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    "Auto top-up sent but failed to read refreshed balance: {}",
                    e
                );
                return Ok(None);
            }
        };
        let refreshed = u256_to_u128_saturating(refreshed_alloy);

        info!(
            "Auto top-up success: tx={:?}, new claimer wallet balance={} wei",
            receipt.transaction_hash, refreshed
        );
        Ok(Some(refreshed_alloy))
    }

    /// Claim a specific condition by calling the ConditionalTokens redeem function
    async fn claim_position(&self, pos: &RedeemablePosition) -> Result<String> {
        let mut attempted_relayer = false;
        if relayer_claim_enabled() && relayer_builder_credentials_available() {
            attempted_relayer = true;
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
        if attempted_relayer
            && relayer_fallback_onchain_enabled()
            && !self.preflight_wallet_can_claim().await?
        {
            return Err(crate::error::PloyError::Wallet(
                "Insufficient native gas for on-chain fallback redeem".into(),
            ));
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

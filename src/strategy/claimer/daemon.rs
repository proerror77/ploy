//! Environment helpers and process-wide claimer daemon bootstrap.

use super::{
    AutoClaimer, ClaimerConfig, ACCOUNT_CLAIMER_DAEMON_STARTED, DEFAULT_MIN_NATIVE_GAS_WEI,
    POLYGON_CHAIN_ID,
};
use crate::adapters::PolymarketClient;
use crate::error::Result;
use crate::signing::Wallet;
use alloy::primitives::U256;
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{error, info, warn};

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

pub(crate) fn u256_to_u128_saturating(value: U256) -> u128 {
    value.to_string().parse::<u128>().unwrap_or(u128::MAX)
}

pub(crate) fn needs_native_gas_preflight(auto_claim: bool, relayer_ready: bool) -> bool {
    auto_claim && !relayer_ready
}

#[cfg(feature = "builder_relayer_sdk")]
use tracing::warn;

use crate::error::Result;

use super::{
    env_flag, env_string_any, env_u64_any, AutoClaimer, RedeemablePosition,
    CONDITIONAL_TOKENS_POLYGON, POLYGON_CHAIN_ID, POLYGON_RPC_DEFAULT, USDC_E_POLYGON,
};

mod legacy_flow;
mod proxy_support;
mod sdk_flow;
#[cfg(test)]
mod tests;

use proxy_support::relayer_builder_credentials;

const RELAYER_URL_DEFAULT: &str = "https://relayer-v2.polymarket.com";
const RELAYER_PROXY_FACTORY_POLYGON: &str = "0xaB45c5A4B0c941a2F231C04C3f49182e1A254052";
const RELAYER_RELAY_HUB_POLYGON: &str = "0xD216153c06E857cD7f72665E0aF1d7D82172F494";
const RELAYER_PROXY_INIT_CODE_HASH: &str =
    "0xd21df8dc65880a8606f09fe0ce3df9b8869287ab0b058be05aa9e8af6330a00b";
const RELAYER_DEFAULT_GAS_LIMIT: u64 = 10_000_000;
const RELAYER_DEFAULT_MAX_POLLS: u64 = 100;
const RELAYER_DEFAULT_POLL_INTERVAL_MS: u64 = 2_000;

const RELAYER_BUILDER_API_KEY_ENV_KEYS: [&str; 3] = [
    "CLAIMER_BUILDER_API_KEY",
    "POLY_BUILDER_API_KEY",
    "BUILDER_API_KEY",
];
const RELAYER_BUILDER_SECRET_ENV_KEYS: [&str; 3] = [
    "CLAIMER_BUILDER_SECRET",
    "POLY_BUILDER_SECRET",
    "BUILDER_SECRET",
];
const RELAYER_BUILDER_PASSPHRASE_ENV_KEYS: [&str; 4] = [
    "CLAIMER_BUILDER_PASSPHRASE",
    "POLY_BUILDER_PASSPHRASE",
    "BUILDER_PASS_PHRASE",
    "BUILDER_PASSPHRASE",
];

pub(super) fn relayer_claim_enabled() -> bool {
    env_flag(
        "CLAIMER_RELAYER_ENABLED",
        env_flag("CLAIMER_GASLESS_REDEEM_ENABLED", true),
    )
}

pub(super) fn relayer_builder_credentials_available() -> bool {
    relayer_builder_credentials().is_some()
}

pub(super) fn missing_relayer_builder_credential_groups() -> Vec<&'static str> {
    let mut missing = Vec::new();
    if first_present_env_key(&RELAYER_BUILDER_API_KEY_ENV_KEYS).is_none() {
        missing.push("api_key");
    }
    if first_present_env_key(&RELAYER_BUILDER_SECRET_ENV_KEYS).is_none() {
        missing.push("secret");
    }
    if first_present_env_key(&RELAYER_BUILDER_PASSPHRASE_ENV_KEYS).is_none() {
        missing.push("passphrase");
    }
    missing
}

pub(super) fn relayer_fallback_onchain_enabled() -> bool {
    env_flag("CLAIMER_RELAYER_FALLBACK_ONCHAIN", false)
}

pub(super) fn relayer_base_url() -> String {
    env_string_any(&[
        "CLAIMER_RELAYER_URL",
        "POLYMARKET_RELAYER_URL",
        "RELAYER_URL",
    ])
    .unwrap_or_else(|| RELAYER_URL_DEFAULT.to_string())
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

fn first_present_env_key(keys: &[&'static str]) -> Option<&'static str> {
    keys.iter().copied().find(|key| {
        std::env::var(key)
            .ok()
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
    })
}

impl AutoClaimer {
    pub(super) async fn claim_position_via_relayer_proxy(
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

        #[cfg(feature = "builder_relayer_sdk")]
        match self
            .claim_position_via_relayer_proxy_sdk(pos, &builder_creds, private_key)
            .await
        {
            Ok(tx_hash) => return Ok(tx_hash),
            Err(e) => {
                warn!(
                    "Relayer SDK path failed, falling back to legacy relayer flow: {}",
                    e
                );
            }
        }
        self.claim_position_via_relayer_proxy_legacy(pos, &builder_creds, private_key)
            .await
    }
}

//! Authentication resolution for `ploy pm` commands.
//!
//! Key resolution order:
//! 1. `--private-key <KEY>` CLI flag
//! 2. `POLYMARKET_PRIVATE_KEY` env var
//! 3. `PRIVATE_KEY` env var
//! 4. `~/.config/polymarket/config.json` → `private_key`
//! 5. No key → read-only mode (returns None)

use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer;
use anyhow::{Context, Result};
use std::str::FromStr;
use tracing::debug;

use super::config_file::PmConfig;

/// Resolved authentication context for pm commands.
pub struct PmAuth {
    /// Signer (if private key available).
    pub signer: Option<PrivateKeySigner>,
    /// Funder address for proxy wallets.
    pub funder: Option<alloy::primitives::Address>,
    /// Chain ID.
    pub chain_id: u64,
}

impl PmAuth {
    /// True if we have signing capability.
    pub fn is_authenticated(&self) -> bool {
        self.signer.is_some()
    }

    /// Returns the signer or an error explaining how to authenticate.
    pub fn require_signer(&self) -> Result<&PrivateKeySigner> {
        self.signer.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "authentication required. Provide a private key via:\n  \
                 --private-key <KEY>\n  \
                 POLYMARKET_PRIVATE_KEY env var\n  \
                 PRIVATE_KEY env var\n  \
                 ploy pm setup"
            )
        })
    }

    /// Returns the wallet address (requires signer).
    pub fn address(&self) -> Result<alloy::primitives::Address> {
        Ok(self.require_signer()?.address())
    }
}

/// Resolve authentication from CLI flag, env vars, and config file.
///
/// Returns `PmAuth` with signer set to `None` if no key found (read-only mode).
pub fn resolve_auth(cli_private_key: Option<&str>) -> Result<PmAuth> {
    let config = PmConfig::load().unwrap_or_default();
    let chain_id = config.chain();

    // 1. CLI flag
    let raw_key = if let Some(key) = cli_private_key {
        debug!("using private key from --private-key flag");
        Some(key.to_string())
    }
    // 2. POLYMARKET_PRIVATE_KEY env
    else if let Ok(key) = std::env::var("POLYMARKET_PRIVATE_KEY") {
        debug!("using private key from POLYMARKET_PRIVATE_KEY env");
        Some(key)
    }
    // 3. PRIVATE_KEY env
    else if let Ok(key) = std::env::var("PRIVATE_KEY") {
        debug!("using private key from PRIVATE_KEY env");
        Some(key)
    }
    // 4. Config file
    else if let Some(key) = config.private_key.as_deref() {
        debug!("using private key from config file");
        Some(key.to_string())
    }
    // 5. No key → read-only
    else {
        debug!("no private key found, running in read-only mode");
        None
    };

    let signer = raw_key
        .map(|key| {
            let hex = key.trim_start_matches("0x");
            PrivateKeySigner::from_str(hex)
                .context("invalid private key")
                .map(|s| s.with_chain_id(Some(chain_id)))
        })
        .transpose()?;

    let funder = config
        .funder_address
        .as_deref()
        .map(|addr| addr.parse::<alloy::primitives::Address>())
        .transpose()
        .context("invalid funder address in config")?;

    Ok(PmAuth {
        signer,
        funder,
        chain_id,
    })
}

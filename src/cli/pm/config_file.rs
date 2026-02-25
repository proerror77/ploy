//! Persistent config file for `ploy pm` commands.
//!
//! Stored at `~/.config/polymarket/config.json` with 0o600 permissions.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Configuration stored in `~/.config/polymarket/config.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PmConfig {
    /// Private key (hex, with or without 0x prefix).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private_key: Option<String>,

    /// Funder / proxy wallet address.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub funder_address: Option<String>,

    /// Default CLOB API base URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clob_url: Option<String>,

    /// Default Gamma API base URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gamma_url: Option<String>,

    /// Chain ID (137 = Polygon mainnet, 80002 = Amoy testnet).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain_id: Option<u64>,

    /// Polygon RPC URL (required for CTF on-chain operations).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpc_url: Option<String>,
}

impl PmConfig {
    /// Returns the config directory path: `~/.config/polymarket/`.
    pub fn config_dir() -> Result<PathBuf> {
        let base = dirs::config_dir().context("cannot determine config directory")?;
        Ok(base.join("polymarket"))
    }

    /// Returns the config file path: `~/.config/polymarket/config.json`.
    pub fn config_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("config.json"))
    }

    /// Load config from disk. Returns default if file doesn't exist.
    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let config: PmConfig = serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        Ok(config)
    }

    /// Save config to disk with restrictive permissions (0o600).
    pub fn save(&self) -> Result<()> {
        let dir = Self::config_dir()?;
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;

        let path = Self::config_path()?;
        let contents = serde_json::to_string_pretty(self)?;

        // Write with restrictive permissions (private key inside)
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true).mode(0o600);
            use std::io::Write;
            let mut f = opts
                .open(&path)
                .with_context(|| format!("failed to write {}", path.display()))?;
            f.write_all(contents.as_bytes())?;
        }

        #[cfg(not(unix))]
        {
            std::fs::write(&path, &contents)
                .with_context(|| format!("failed to write {}", path.display()))?;
        }

        Ok(())
    }

    /// Returns the CLOB base URL (default: mainnet).
    pub fn clob_base_url(&self) -> &str {
        self.clob_url
            .as_deref()
            .unwrap_or("https://clob.polymarket.com")
    }

    /// Returns the Gamma base URL (default: mainnet).
    pub fn gamma_base_url(&self) -> &str {
        self.gamma_url
            .as_deref()
            .unwrap_or("https://gamma-api.polymarket.com")
    }

    /// Returns the chain ID (default: 137 Polygon mainnet).
    pub fn chain(&self) -> u64 {
        self.chain_id.unwrap_or(137)
    }

    /// Returns the Polygon RPC URL (default: public Polygon RPC).
    pub fn rpc_url(&self) -> &str {
        self.rpc_url
            .as_deref()
            .unwrap_or("https://polygon-rpc.com")
    }
}

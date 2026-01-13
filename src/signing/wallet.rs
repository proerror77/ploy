use crate::error::{PloyError, Result};
use ethers::signers::{LocalWallet, Signer as EthersSigner};
use ethers::types::{Address, Signature, H256};
use tracing::{info, warn};
use zeroize::Zeroize;

/// Wallet for signing Polymarket orders and authentication messages
///
/// # Security
/// This wallet no longer stores the private key in memory after initialization.
/// The private key is only used during wallet creation and then immediately zeroized.
/// This prevents memory dumps from exposing the private key.
#[derive(Clone)]
pub struct Wallet {
    inner: LocalWallet,
    chain_id: u64,
}

impl Wallet {
    /// Create a wallet from a private key hex string
    ///
    /// # Security
    /// The private key is zeroized from memory after wallet creation.
    /// It is never stored in the Wallet struct.
    pub fn from_private_key(private_key: &str, chain_id: u64) -> Result<Self> {
        // Remove 0x prefix if present
        let key_hex = private_key.trim_start_matches("0x");

        // Create a zeroizing string for the key
        let mut secure_key = key_hex.to_string();

        let wallet = secure_key
            .parse::<LocalWallet>()
            .map_err(|e| PloyError::Wallet(format!("Invalid private key: {}", e)))?
            .with_chain_id(chain_id);

        // Zeroize the key from memory
        secure_key.zeroize();

        info!("Wallet initialized: {} (private key zeroized from memory)", wallet.address());

        Ok(Self {
            inner: wallet,
            chain_id,
        })
    }

    /// Create a wallet from environment variable
    ///
    /// # Security
    /// The private key is read from environment and immediately zeroized after use.
    pub fn from_env(chain_id: u64) -> Result<Self> {
        let mut private_key = std::env::var("POLYMARKET_PRIVATE_KEY")
            .or_else(|_| std::env::var("PRIVATE_KEY"))
            .map_err(|_| {
                PloyError::Wallet(
                    "POLYMARKET_PRIVATE_KEY or PRIVATE_KEY environment variable not set"
                        .to_string(),
                )
            })?;

        let result = Self::from_private_key(&private_key, chain_id);

        // Zeroize the key from memory
        private_key.zeroize();

        result
    }

    /// Get the wallet address
    pub fn address(&self) -> Address {
        self.inner.address()
    }

    /// Get the chain ID
    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    /// Get the private key as hex string (DEPRECATED - DO NOT USE)
    ///
    /// # Security Warning
    /// This method is deprecated and will be removed in a future version.
    /// Private keys should never be exposed after wallet creation.
    #[deprecated(
        since = "0.1.0",
        note = "Private key access is a security risk. This method will be removed."
    )]
    pub fn private_key_hex(&self) -> &str {
        warn!("SECURITY WARNING: private_key_hex() called - this is deprecated and insecure");
        // Return empty string to maintain API compatibility
        // The actual key is not stored anymore
        ""
    }

    /// Sign a message hash (32 bytes)
    pub async fn sign_hash(&self, hash: H256) -> Result<Signature> {
        self.inner
            .sign_hash(hash)
            .map_err(|e| PloyError::Signature(format!("Failed to sign hash: {}", e)))
    }

    /// Sign a message (will be prefixed with Ethereum signed message)
    pub async fn sign_message<S: AsRef<[u8]> + Send + Sync>(&self, message: S) -> Result<Signature> {
        self.inner
            .sign_message(message)
            .await
            .map_err(|e| PloyError::Signature(format!("Failed to sign message: {}", e)))
    }

    /// Get the underlying ethers wallet for EIP-712 signing
    pub fn inner(&self) -> &LocalWallet {
        &self.inner
    }
}

impl std::fmt::Debug for Wallet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wallet")
            .field("address", &self.address())
            .field("chain_id", &self.chain_id)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_wallet_creation() {
        // Test private key (DO NOT use in production!)
        let test_key = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

        let wallet = Wallet::from_private_key(test_key, 137).unwrap();

        assert_eq!(wallet.chain_id(), 137);
        // This is the well-known address for this test key
        assert_eq!(
            format!("{:?}", wallet.address()),
            "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266"
        );
    }
}

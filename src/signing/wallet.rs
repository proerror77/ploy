use crate::error::{PloyError, Result};
use ethers::signers::{LocalWallet, Signer as EthersSigner};
use ethers::types::{Address, Signature, H256};
use tracing::info;

/// Wallet for signing Polymarket orders and authentication messages
#[derive(Clone)]
pub struct Wallet {
    inner: LocalWallet,
    chain_id: u64,
    private_key_hex: String,
}

impl Wallet {
    /// Create a wallet from a private key hex string
    pub fn from_private_key(private_key: &str, chain_id: u64) -> Result<Self> {
        // Remove 0x prefix if present
        let key_hex = private_key.trim_start_matches("0x");

        let wallet = key_hex
            .parse::<LocalWallet>()
            .map_err(|e| PloyError::Wallet(format!("Invalid private key: {}", e)))?
            .with_chain_id(chain_id);

        info!("Wallet initialized: {}", wallet.address());

        Ok(Self {
            inner: wallet,
            chain_id,
            private_key_hex: format!("0x{}", key_hex),
        })
    }

    /// Create a wallet from environment variable
    pub fn from_env(chain_id: u64) -> Result<Self> {
        let private_key = std::env::var("POLYMARKET_PRIVATE_KEY")
            .or_else(|_| std::env::var("PRIVATE_KEY"))
            .map_err(|_| {
                PloyError::Wallet(
                    "POLYMARKET_PRIVATE_KEY or PRIVATE_KEY environment variable not set"
                        .to_string(),
                )
            })?;

        Self::from_private_key(&private_key, chain_id)
    }

    /// Get the wallet address
    pub fn address(&self) -> Address {
        self.inner.address()
    }

    /// Get the chain ID
    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    /// Get the private key as hex string (with 0x prefix)
    pub fn private_key_hex(&self) -> &str {
        &self.private_key_hex
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

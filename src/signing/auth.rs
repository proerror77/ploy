use crate::error::Result;
use crate::signing::Wallet;
use ethers::types::{Address, U256};
use ethers::utils::keccak256;
use serde::{Deserialize, Serialize};

/// CLOB authentication domain name
pub const CLOB_DOMAIN_NAME: &str = "ClobAuthDomain";
/// CLOB authentication domain version
pub const CLOB_DOMAIN_VERSION: &str = "1";
/// Message that attests wallet control
pub const CLOB_AUTH_MESSAGE: &str = "This message attests that I control the given wallet";

/// EIP-712 domain for CLOB authentication
#[derive(Debug, Clone)]
pub struct ClobAuthDomain {
    pub name: String,
    pub version: String,
    pub chain_id: u64,
}

impl ClobAuthDomain {
    pub fn new(chain_id: u64) -> Self {
        Self {
            name: CLOB_DOMAIN_NAME.to_string(),
            version: CLOB_DOMAIN_VERSION.to_string(),
            chain_id,
        }
    }

    /// Compute the EIP-712 domain separator hash
    pub fn separator_hash(&self) -> [u8; 32] {
        let type_hash = keccak256(b"EIP712Domain(string name,string version,uint256 chainId)");

        let name_hash = keccak256(self.name.as_bytes());
        let version_hash = keccak256(self.version.as_bytes());

        let mut encoded = Vec::with_capacity(128);
        encoded.extend_from_slice(&type_hash);
        encoded.extend_from_slice(&name_hash);
        encoded.extend_from_slice(&version_hash);
        encoded.extend_from_slice(&ethers::abi::encode(&[ethers::abi::Token::Uint(
            U256::from(self.chain_id),
        )]));

        keccak256(&encoded)
    }
}

/// CLOB authentication message structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClobAuthMessage {
    pub address: Address,
    pub timestamp: String,
    pub nonce: U256,
    pub message: String,
}

impl ClobAuthMessage {
    /// Create a new auth message
    pub fn new(address: Address, timestamp: i64, nonce: u64) -> Self {
        Self {
            address,
            timestamp: timestamp.to_string(),
            nonce: U256::from(nonce),
            message: CLOB_AUTH_MESSAGE.to_string(),
        }
    }

    /// Compute the EIP-712 struct hash
    pub fn struct_hash(&self) -> [u8; 32] {
        let type_hash =
            keccak256(b"ClobAuth(address address,string timestamp,uint256 nonce,string message)");

        let timestamp_hash = keccak256(self.timestamp.as_bytes());
        let message_hash = keccak256(self.message.as_bytes());

        let mut encoded = Vec::with_capacity(160);
        encoded.extend_from_slice(&type_hash);
        // address is left-padded to 32 bytes
        encoded.extend_from_slice(&[0u8; 12]);
        encoded.extend_from_slice(self.address.as_bytes());
        encoded.extend_from_slice(&timestamp_hash);
        encoded.extend_from_slice(&ethers::abi::encode(&[ethers::abi::Token::Uint(
            self.nonce,
        )]));
        encoded.extend_from_slice(&message_hash);

        keccak256(&encoded)
    }

    /// Compute the full EIP-712 hash to sign
    pub fn signing_hash(&self, domain: &ClobAuthDomain) -> [u8; 32] {
        let domain_separator = domain.separator_hash();
        let struct_hash = self.struct_hash();

        let mut encoded = Vec::with_capacity(66);
        encoded.extend_from_slice(b"\x19\x01");
        encoded.extend_from_slice(&domain_separator);
        encoded.extend_from_slice(&struct_hash);

        keccak256(&encoded)
    }
}

/// Build a CLOB authentication EIP-712 signature
pub async fn build_clob_auth_signature(
    wallet: &Wallet,
    timestamp: i64,
    nonce: u64,
) -> Result<(ClobAuthMessage, String)> {
    let domain = ClobAuthDomain::new(wallet.chain_id());
    let message = ClobAuthMessage::new(wallet.address(), timestamp, nonce);

    let hash = message.signing_hash(&domain);
    let signature = wallet.sign_hash(hash.into()).await?;

    // Format signature as hex string
    let sig_hex = format!("0x{}", hex::encode(signature.to_vec()));

    Ok((message, sig_hex))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_domain_separator() {
        let domain = ClobAuthDomain::new(137);
        let separator = domain.separator_hash();

        // Should produce a valid 32-byte hash
        assert_eq!(separator.len(), 32);
    }

    #[test]
    fn test_auth_message_hash() {
        let address = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266"
            .parse()
            .unwrap();
        let message = ClobAuthMessage::new(address, 1704067200, 0);

        let struct_hash = message.struct_hash();
        assert_eq!(struct_hash.len(), 32);

        let domain = ClobAuthDomain::new(137);
        let signing_hash = message.signing_hash(&domain);
        assert_eq!(signing_hash.len(), 32);
    }
}

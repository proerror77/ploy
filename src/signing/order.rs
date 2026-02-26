use crate::error::{PloyError, Result};
use crate::signing::Wallet;
use ethers::types::{Address, U256};
use ethers::utils::keccak256;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Default order expiration: 30 minutes from submission.
///
/// This is the on-chain signature validity window, NOT the CLOB matching behavior
/// (which is controlled by TimeInForce: IOC/FOK/GTC). For IOC/FOK orders the
/// expiration is irrelevant (they resolve in <1s). For GTC orders this acts as a
/// safety net: if the system crashes and can't cancel a stale order, the exchange
/// contract will reject it after this window.
///
/// 30 min is chosen to be:
/// - Long enough for any strategy round (crypto ~5min, sports ~hours)
/// - Short enough that a crashed system's ghost orders don't linger indefinitely
///
/// Override via PLOY_ORDER_EXPIRY_SECS env var.
fn order_expiry_secs() -> u64 {
    std::env::var("PLOY_ORDER_EXPIRY_SECS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(1800)
}

/// Exchange contract addresses for Polygon Mainnet
pub mod contracts {
    /// CTF Exchange contract on Polygon Mainnet
    pub const CTF_EXCHANGE: &str = "0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E";
    /// Neg Risk CTF Exchange contract on Polygon Mainnet
    pub const NEG_RISK_CTF_EXCHANGE: &str = "0xC5d563A36AE78145C45a50134d48A1215220f80a";
    /// USDC contract on Polygon
    pub const USDC: &str = "0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174";
    /// Conditional Tokens contract
    pub const CONDITIONAL_TOKENS: &str = "0x4D97DCd97eC945f40cF65F87097ACe5EA0476045";
}

/// EIP-712 domain for order signing
pub const ORDER_DOMAIN_NAME: &str = "Polymarket CTF Exchange";
pub const ORDER_DOMAIN_VERSION: &str = "1";

/// Signature types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum SignatureType {
    /// EOA wallet (direct private key)
    EOA = 0,
    /// Polymarket proxy wallet
    PolyProxy = 1,
    /// Gnosis Safe wallet
    PolyGnosisSafe = 2,
}

impl Default for SignatureType {
    fn default() -> Self {
        Self::EOA
    }
}

/// Order side
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum OrderSide {
    Buy = 0,
    Sell = 1,
}

fn scale_price_to_u128(price: Decimal) -> Result<u128> {
    if price.is_sign_negative() {
        return Err(PloyError::OrderSubmission(format!(
            "Invalid price: {}",
            price
        )));
    }

    let scaled = (price * Decimal::from(1_000_000u64))
        .round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero);

    scaled
        .to_u128()
        .ok_or_else(|| PloyError::OrderSubmission(format!("Invalid price: {}", price)))
}

/// Order data structure for EIP-712 signing
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderData {
    /// Random salt for uniqueness
    pub salt: U256,
    /// Order creator address
    pub maker: Address,
    /// Order signer (may differ from maker for proxy wallets)
    pub signer: Address,
    /// Counterparty address (usually zero address for open orders)
    pub taker: Address,
    /// Token ID to trade
    pub token_id: U256,
    /// Amount maker will provide (USDC for buys, tokens for sells)
    pub maker_amount: U256,
    /// Amount maker will receive (tokens for buys, USDC for sells)
    pub taker_amount: U256,
    /// Order expiration timestamp (0 for no expiration)
    pub expiration: U256,
    /// Unique nonce
    pub nonce: U256,
    /// Fee rate in basis points
    pub fee_rate_bps: U256,
    /// Order side (0 = buy, 1 = sell)
    pub side: u8,
    /// Signature type
    pub signature_type: u8,
}

impl OrderData {
    /// Create a new buy order
    pub fn new_buy(
        maker: Address,
        signer: Address,
        token_id: &str,
        price: Decimal,
        size: u64,
        nonce: u64,
    ) -> Result<Self> {
        let token_id = U256::from_dec_str(token_id)
            .map_err(|e| PloyError::OrderSubmission(format!("Invalid token ID: {}", e)))?;

        // For buys: maker provides USDC, receives tokens
        // maker_amount = price * size (in USDC with 6 decimals)
        // taker_amount = size (in tokens with 6 decimals)
        let price_scaled = scale_price_to_u128(price)?;
        let size_scaled = u128::from(size) * 1_000_000;

        let maker_amount = U256::from(price_scaled) * U256::from(size);
        let taker_amount = U256::from(size_scaled);

        Ok(Self {
            salt: Self::generate_salt(),
            maker,
            signer,
            taker: Address::zero(),
            token_id,
            maker_amount,
            taker_amount,
            expiration: Self::default_expiration(),
            nonce: U256::from(nonce),
            fee_rate_bps: U256::zero(), // No fee
            side: OrderSide::Buy as u8,
            signature_type: SignatureType::EOA as u8,
        })
    }

    /// Create a new sell order
    pub fn new_sell(
        maker: Address,
        signer: Address,
        token_id: &str,
        price: Decimal,
        size: u64,
        nonce: u64,
    ) -> Result<Self> {
        let token_id = U256::from_dec_str(token_id)
            .map_err(|e| PloyError::OrderSubmission(format!("Invalid token ID: {}", e)))?;

        // For sells: maker provides tokens, receives USDC
        // maker_amount = size (in tokens with 6 decimals)
        // taker_amount = price * size (in USDC with 6 decimals)
        let price_scaled = scale_price_to_u128(price)?;
        let size_scaled = u128::from(size) * 1_000_000;

        let maker_amount = U256::from(size_scaled);
        let taker_amount = U256::from(price_scaled) * U256::from(size);

        Ok(Self {
            salt: Self::generate_salt(),
            maker,
            signer,
            taker: Address::zero(),
            token_id,
            maker_amount,
            taker_amount,
            expiration: Self::default_expiration(),
            nonce: U256::from(nonce),
            fee_rate_bps: U256::zero(),
            side: OrderSide::Sell as u8,
            signature_type: SignatureType::EOA as u8,
        })
    }

    /// Generate a random salt
    fn generate_salt() -> U256 {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let bytes: [u8; 32] = rng.gen();
        U256::from_big_endian(&bytes)
    }

    /// Compute default order expiration (current time + configurable window).
    fn default_expiration() -> U256 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        U256::from(now + order_expiry_secs())
    }

    /// Compute the EIP-712 struct hash
    pub fn struct_hash(&self) -> [u8; 32] {
        let type_hash = keccak256(
            b"Order(uint256 salt,address maker,address signer,address taker,uint256 tokenId,uint256 makerAmount,uint256 takerAmount,uint256 expiration,uint256 nonce,uint256 feeRateBps,uint8 side,uint8 signatureType)",
        );

        let tokens = vec![
            ethers::abi::Token::FixedBytes(type_hash.to_vec()),
            ethers::abi::Token::Uint(self.salt),
            ethers::abi::Token::Address(self.maker),
            ethers::abi::Token::Address(self.signer),
            ethers::abi::Token::Address(self.taker),
            ethers::abi::Token::Uint(self.token_id),
            ethers::abi::Token::Uint(self.maker_amount),
            ethers::abi::Token::Uint(self.taker_amount),
            ethers::abi::Token::Uint(self.expiration),
            ethers::abi::Token::Uint(self.nonce),
            ethers::abi::Token::Uint(self.fee_rate_bps),
            ethers::abi::Token::Uint(U256::from(self.side)),
            ethers::abi::Token::Uint(U256::from(self.signature_type)),
        ];

        let encoded = ethers::abi::encode(&tokens);
        keccak256(&encoded)
    }
}

/// EIP-712 domain for order signing
#[derive(Debug, Clone)]
pub struct OrderDomain {
    pub name: String,
    pub version: String,
    pub chain_id: u64,
    pub verifying_contract: Address,
}

impl OrderDomain {
    pub fn new(chain_id: u64, neg_risk: bool) -> Self {
        let contract = if neg_risk {
            contracts::NEG_RISK_CTF_EXCHANGE
        } else {
            contracts::CTF_EXCHANGE
        };

        Self {
            name: ORDER_DOMAIN_NAME.to_string(),
            version: ORDER_DOMAIN_VERSION.to_string(),
            chain_id,
            verifying_contract: contract.parse().unwrap_or_default(),
        }
    }

    /// Compute the EIP-712 domain separator hash
    pub fn separator_hash(&self) -> [u8; 32] {
        let type_hash = keccak256(
            b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
        );

        let name_hash = keccak256(self.name.as_bytes());
        let version_hash = keccak256(self.version.as_bytes());

        let tokens = vec![
            ethers::abi::Token::FixedBytes(type_hash.to_vec()),
            ethers::abi::Token::FixedBytes(name_hash.to_vec()),
            ethers::abi::Token::FixedBytes(version_hash.to_vec()),
            ethers::abi::Token::Uint(U256::from(self.chain_id)),
            ethers::abi::Token::Address(self.verifying_contract),
        ];

        let encoded = ethers::abi::encode(&tokens);
        keccak256(&encoded)
    }
}

/// Signed order ready for submission
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedOrder {
    #[serde(flatten)]
    pub order: OrderData,
    pub signature: String,
}

impl SignedOrder {
    /// Convert to JSON for API submission
    pub fn to_json(&self) -> Result<String> {
        // Convert to the format expected by the API
        let json = serde_json::json!({
            "order": {
                "salt": self.order.salt.to_string(),
                "maker": format!("{:?}", self.order.maker),
                "signer": format!("{:?}", self.order.signer),
                "taker": format!("{:?}", self.order.taker),
                "tokenId": self.order.token_id.to_string(),
                "makerAmount": self.order.maker_amount.to_string(),
                "takerAmount": self.order.taker_amount.to_string(),
                "expiration": self.order.expiration.to_string(),
                "nonce": self.order.nonce.to_string(),
                "feeRateBps": self.order.fee_rate_bps.to_string(),
                "side": self.order.side.to_string(),
                "signatureType": self.order.signature_type,
                "signature": &self.signature
            }
        });

        serde_json::to_string(&json).map_err(|e| PloyError::Json(e))
    }
}

/// Build a signed order
pub async fn build_signed_order(
    wallet: &Wallet,
    order: OrderData,
    neg_risk: bool,
) -> Result<SignedOrder> {
    let domain = OrderDomain::new(wallet.chain_id(), neg_risk);

    // Compute EIP-712 hash
    let domain_separator = domain.separator_hash();
    let struct_hash = order.struct_hash();

    let mut encoded = Vec::with_capacity(66);
    encoded.extend_from_slice(b"\x19\x01");
    encoded.extend_from_slice(&domain_separator);
    encoded.extend_from_slice(&struct_hash);

    let signing_hash = keccak256(&encoded);

    // Sign the hash
    let signature = wallet.sign_hash(signing_hash.into()).await?;

    // Format signature as hex string
    let sig_hex = format!("0x{}", hex::encode(signature.to_vec()));

    Ok(SignedOrder {
        order,
        signature: sig_hex,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_order_data_buy() {
        let maker = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266"
            .parse()
            .unwrap();
        let order = OrderData::new_buy(maker, maker, "12345", dec!(0.50), 100, 1).unwrap();

        assert_eq!(order.side, OrderSide::Buy as u8);
        assert_eq!(order.nonce, U256::from(1));
        assert_eq!(order.token_id, U256::from(12345));
    }

    #[test]
    fn test_order_price_scaling_rounds() {
        let maker = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266"
            .parse()
            .unwrap();
        let order = OrderData::new_buy(maker, maker, "12345", dec!(0.1234567), 1, 1).unwrap();

        // 0.1234567 * 1_000_000 = 123456.7 -> rounds to 123457
        assert_eq!(order.maker_amount, U256::from(123457u128));
    }

    #[test]
    fn test_order_price_scaling_rejects_negative() {
        let maker = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266"
            .parse()
            .unwrap();
        let result = OrderData::new_sell(maker, maker, "12345", dec!(-0.01), 1, 1);

        assert!(result.is_err());
    }

    #[test]
    fn test_order_domain() {
        let domain = OrderDomain::new(137, false);
        let separator = domain.separator_hash();
        assert_eq!(separator.len(), 32);
    }
}

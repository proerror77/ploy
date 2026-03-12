use super::*;
use alloy::primitives::{keccak256, Address, B256, U256};
use alloy::sol;
use alloy::sol_types::SolCall;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

sol! {
    struct RelayerProxyCall {
        uint8 operation;
        address to;
        uint256 value;
        bytes data;
    }

    function redeemPositions(
        address collateralToken,
        bytes32 parentCollectionId,
        bytes32 conditionId,
        uint256[] indexSets
    ) external;

    function proxy(RelayerProxyCall[] calls) external;
}

#[derive(Debug, Clone)]
pub(super) struct RelayerBuilderCredentials {
    pub(super) api_key: String,
    pub(super) secret: String,
    pub(super) passphrase: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct RelayerPayloadResponse {
    pub(super) address: String,
    pub(super) nonce: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RelayerSignatureParams {
    #[serde(rename = "gasPrice")]
    pub(super) gas_price: String,
    #[serde(rename = "gasLimit")]
    pub(super) gas_limit: String,
    #[serde(rename = "relayerFee")]
    pub(super) relayer_fee: String,
    #[serde(rename = "relayHub")]
    pub(super) relay_hub: String,
    pub(super) relay: String,
}

#[derive(Debug, Serialize)]
pub(super) struct RelayerSubmitRequest {
    #[serde(rename = "type")]
    pub(super) tx_type: String,
    pub(super) from: String,
    pub(super) to: String,
    #[serde(rename = "proxyWallet")]
    pub(super) proxy_wallet: String,
    pub(super) data: String,
    pub(super) nonce: String,
    pub(super) signature: String,
    #[serde(rename = "signatureParams")]
    pub(super) signature_params: RelayerSignatureParams,
    pub(super) metadata: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct RelayerSubmitResponse {
    #[serde(rename = "transactionID")]
    pub(super) transaction_id: String,
    pub(super) state: String,
    #[serde(rename = "transactionHash")]
    pub(super) transaction_hash: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RelayerTransactionStatus {
    pub(super) state: String,
    #[serde(rename = "transactionHash")]
    pub(super) transaction_hash: Option<String>,
}

pub(super) fn relayer_builder_credentials() -> Option<RelayerBuilderCredentials> {
    let api_key = env_string_any(&RELAYER_BUILDER_API_KEY_ENV_KEYS)?;
    let secret = env_string_any(&RELAYER_BUILDER_SECRET_ENV_KEYS)?;
    let passphrase = env_string_any(&RELAYER_BUILDER_PASSPHRASE_ENV_KEYS)?;
    Some(RelayerBuilderCredentials {
        api_key,
        secret,
        passphrase,
    })
}

pub(super) fn relayer_hmac_signature(secret_base64: &str, message: &str) -> Result<String> {
    let trimmed = secret_base64.trim();
    let secret = BASE64
        .decode(trimmed)
        .or_else(|_| {
            let mut normalized = trimmed.replace('-', "+").replace('_', "/");
            while normalized.len() % 4 != 0 {
                normalized.push('=');
            }
            BASE64.decode(normalized.as_bytes())
        })
        .map_err(|e| {
            crate::error::PloyError::Signature(format!("Invalid builder secret encoding: {}", e))
        })?;
    let mut mac: Hmac<Sha256> = Hmac::new_from_slice(&secret).map_err(|e| {
        crate::error::PloyError::Signature(format!("Builder HMAC init failed: {}", e))
    })?;
    mac.update(message.as_bytes());
    let sig = BASE64.encode(mac.finalize().into_bytes());
    Ok(sig.replace('+', "-").replace('/', "_"))
}

pub(super) fn compact_http_body(raw: &str, max_chars: usize) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "<empty>".to_string();
    }
    let mut out = trimmed.to_string();
    if out.len() > max_chars {
        out.truncate(max_chars);
        out.push_str("...");
    }
    out
}

pub(super) fn ensure_0x_prefix(hex: &str) -> String {
    if hex.starts_with("0x") || hex.starts_with("0X") {
        return hex.to_string();
    }
    format!("0x{}", hex)
}

impl AutoClaimer {
    pub(super) fn encode_ctf_redeem_calldata(condition_id: [u8; 32]) -> Result<Vec<u8>> {
        let usdc_addr: Address = USDC_E_POLYGON.parse().map_err(|e| {
            crate::error::PloyError::AddressParsing(format!("Invalid USDC.e address: {}", e))
        })?;

        Ok(redeemPositionsCall {
            collateralToken: usdc_addr,
            parentCollectionId: B256::ZERO,
            conditionId: B256::from(condition_id),
            indexSets: vec![U256::from(1u8), U256::from(2u8)],
        }
        .abi_encode())
    }

    pub(super) fn encode_proxy_transaction_data(
        call_to: Address,
        call_data: Vec<u8>,
    ) -> Result<Vec<u8>> {
        Ok(proxyCall {
            calls: vec![RelayerProxyCall {
                operation: 1u8,
                to: call_to,
                value: U256::ZERO,
                data: call_data.into(),
            }],
        }
        .abi_encode())
    }

    pub(super) fn derive_proxy_wallet_address(signer: Address) -> Result<Address> {
        let proxy_factory: Address = RELAYER_PROXY_FACTORY_POLYGON.parse().map_err(|e| {
            crate::error::PloyError::AddressParsing(format!("Invalid relayer proxy factory: {}", e))
        })?;
        let init_hash: B256 = RELAYER_PROXY_INIT_CODE_HASH.parse().map_err(|e| {
            crate::error::PloyError::AddressParsing(format!(
                "Invalid relayer proxy init code hash: {}",
                e
            ))
        })?;
        let salt = keccak256(signer.as_slice());
        Ok(proxy_factory.create2(salt, init_hash))
    }

    pub(super) fn create_proxy_struct_hash(
        from: Address,
        to: Address,
        data: &[u8],
        tx_fee: U256,
        gas_price: U256,
        gas_limit: U256,
        nonce: U256,
        relay_hub: Address,
        relay: Address,
    ) -> B256 {
        fn append_u256(out: &mut Vec<u8>, value: U256) {
            out.extend_from_slice(&value.to_be_bytes::<32>());
        }

        let mut payload = Vec::with_capacity(4 + 20 + 20 + data.len() + 32 * 4 + 20 + 20);
        payload.extend_from_slice(b"rlx:");
        payload.extend_from_slice(from.as_slice());
        payload.extend_from_slice(to.as_slice());
        payload.extend_from_slice(data);
        append_u256(&mut payload, tx_fee);
        append_u256(&mut payload, gas_price);
        append_u256(&mut payload, gas_limit);
        append_u256(&mut payload, nonce);
        payload.extend_from_slice(relay_hub.as_slice());
        payload.extend_from_slice(relay.as_slice());

        keccak256(payload)
    }

    pub(super) fn build_relayer_builder_headers(
        creds: &RelayerBuilderCredentials,
        timestamp: i64,
        body: &str,
    ) -> Result<HeaderMap> {
        let message = format!("{}POST/submit{}", timestamp, body);
        let signature = relayer_hmac_signature(&creds.secret, &message)?;

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            HeaderName::from_static("poly_builder_api_key"),
            HeaderValue::from_str(&creds.api_key).map_err(|e| {
                crate::error::PloyError::Internal(format!("Invalid builder API key header: {}", e))
            })?,
        );
        headers.insert(
            HeaderName::from_static("poly_builder_passphrase"),
            HeaderValue::from_str(&creds.passphrase).map_err(|e| {
                crate::error::PloyError::Internal(format!(
                    "Invalid builder passphrase header: {}",
                    e
                ))
            })?,
        );
        headers.insert(
            HeaderName::from_static("poly_builder_signature"),
            HeaderValue::from_str(&signature).map_err(|e| {
                crate::error::PloyError::Internal(format!(
                    "Invalid builder signature header: {}",
                    e
                ))
            })?,
        );
        headers.insert(
            HeaderName::from_static("poly_builder_timestamp"),
            HeaderValue::from_str(&timestamp.to_string()).map_err(|e| {
                crate::error::PloyError::Internal(format!(
                    "Invalid builder timestamp header: {}",
                    e
                ))
            })?,
        );
        Ok(headers)
    }
}

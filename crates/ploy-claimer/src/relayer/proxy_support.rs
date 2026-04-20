use super::*;
use alloy::primitives::Address;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use ethers_core::abi::{AbiParser, Token, encode as abi_encode};
use ethers_core::types::{Address as EthersAddress, H256 as EthersH256, U256 as EthersU256};
use ethers_core::utils::{
    get_create2_address_from_hash as ethers_get_create2_address_from_hash, keccak256,
};
use hmac::{Hmac, Mac};
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

#[derive(Clone)]
pub(super) struct RelayerBuilderCredentials {
    pub(super) api_key: String,
    pub(super) secret: String,
    pub(super) passphrase: String,
}

impl std::fmt::Debug for RelayerBuilderCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayerBuilderCredentials")
            .field("api_key", &"[redacted]")
            .field("secret", &"[redacted]")
            .field("passphrase", &"[redacted]")
            .finish()
    }
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

pub(super) fn relayer_hmac_signature(
    secret_base64: &str,
    message: &str,
) -> Result<String, crate::ClaimerError> {
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
            crate::ClaimerError::Internal(format!("Invalid builder secret encoding: {}", e))
        })?;
    let mut mac: Hmac<Sha256> = Hmac::new_from_slice(&secret)
        .map_err(|e| crate::ClaimerError::Internal(format!("Builder HMAC init failed: {}", e)))?;
    mac.update(message.as_bytes());
    let sig = BASE64.encode(mac.finalize().into_bytes());
    Ok(sig.replace('+', "-").replace('/', "_"))
}

pub(super) fn ethers_to_alloy_address(value: EthersAddress) -> Address {
    Address::from_slice(value.as_bytes())
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
    pub(super) fn encode_redeem_calldata(
        condition_id: [u8; 32],
        neg_risk: bool,
        amounts: &[EthersU256],
    ) -> Result<Vec<u8>, crate::ClaimerError> {
        let (signature, tokens) = if neg_risk {
            (
                "function redeemPositions(bytes32 conditionId, uint256[] amounts)",
                vec![
                    Token::FixedBytes(condition_id.to_vec()),
                    Token::Array(amounts.iter().copied().map(Token::Uint).collect()),
                ],
            )
        } else {
            let pusd_addr: EthersAddress = PUSD_POLYGON.parse().map_err(|e| {
                crate::ClaimerError::Network(format!("Invalid pUSD address: {}", e))
            })?;

            (
                "function redeemPositions(address collateralToken, bytes32 parentCollectionId, bytes32 conditionId, uint256[] indexSets)",
                vec![
                    Token::Address(pusd_addr),
                    Token::FixedBytes(vec![0u8; 32]),
                    Token::FixedBytes(condition_id.to_vec()),
                    Token::Array(vec![
                        Token::Uint(EthersU256::from(1u8)),
                        Token::Uint(EthersU256::from(2u8)),
                    ]),
                ],
            )
        };

        AbiParser::default()
            .parse_function(signature)
            .map_err(|e| {
                crate::ClaimerError::Internal(format!("Failed to parse redeem ABI: {}", e))
            })?
            .encode_input(&tokens)
            .map_err(|e| {
                crate::ClaimerError::Internal(format!("Failed to encode redeem calldata: {}", e))
            })
    }

    pub(super) fn encode_proxy_transaction_data(
        call_to: EthersAddress,
        call_data: Vec<u8>,
    ) -> Result<Vec<u8>, crate::ClaimerError> {
        let calls = Token::Array(vec![Token::Tuple(vec![
            Token::Uint(EthersU256::from(1u8)),
            Token::Address(call_to),
            Token::Uint(EthersU256::zero()),
            Token::Bytes(call_data),
        ])]);
        let encoded_args = abi_encode(&[calls]);
        let selector = &keccak256("proxy((uint8,address,uint256,bytes)[])")[0..4];
        let mut payload = Vec::with_capacity(4 + encoded_args.len());
        payload.extend_from_slice(selector);
        payload.extend_from_slice(&encoded_args);
        Ok(payload)
    }

    pub(super) fn derive_proxy_wallet_address(
        signer: EthersAddress,
    ) -> Result<EthersAddress, crate::ClaimerError> {
        let proxy_factory: EthersAddress = RELAYER_PROXY_FACTORY_POLYGON.parse().map_err(|e| {
            crate::ClaimerError::Network(format!("Invalid relayer proxy factory: {}", e))
        })?;
        let init_hash: EthersH256 = RELAYER_PROXY_INIT_CODE_HASH.parse().map_err(|e| {
            crate::ClaimerError::Network(format!("Invalid relayer proxy init code hash: {}", e))
        })?;
        let salt = keccak256(signer.as_bytes());
        Ok(ethers_get_create2_address_from_hash(
            proxy_factory,
            salt,
            init_hash.to_fixed_bytes(),
        ))
    }

    pub(super) fn create_proxy_struct_hash(
        from: EthersAddress,
        to: EthersAddress,
        data: &[u8],
        tx_fee: EthersU256,
        gas_price: EthersU256,
        gas_limit: EthersU256,
        nonce: EthersU256,
        relay_hub: EthersAddress,
        relay: EthersAddress,
    ) -> EthersH256 {
        fn append_u256(out: &mut Vec<u8>, value: EthersU256) {
            let mut buf = [0u8; 32];
            value.to_big_endian(&mut buf);
            out.extend_from_slice(&buf);
        }

        let mut payload = Vec::with_capacity(4 + 20 + 20 + data.len() + 32 * 4 + 20 + 20);
        payload.extend_from_slice(b"rlx:");
        payload.extend_from_slice(from.as_bytes());
        payload.extend_from_slice(to.as_bytes());
        payload.extend_from_slice(data);
        append_u256(&mut payload, tx_fee);
        append_u256(&mut payload, gas_price);
        append_u256(&mut payload, gas_limit);
        append_u256(&mut payload, nonce);
        payload.extend_from_slice(relay_hub.as_bytes());
        payload.extend_from_slice(relay.as_bytes());

        EthersH256::from(keccak256(payload))
    }

    pub(super) fn build_relayer_builder_headers(
        creds: &RelayerBuilderCredentials,
        timestamp: i64,
        body: &str,
    ) -> Result<HeaderMap, crate::ClaimerError> {
        let message = format!("{}POST/submit{}", timestamp, body);
        let signature = relayer_hmac_signature(&creds.secret, &message)?;

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            HeaderName::from_static("poly_builder_api_key"),
            HeaderValue::from_str(&creds.api_key).map_err(|e| {
                crate::ClaimerError::Internal(format!("Invalid builder API key header: {}", e))
            })?,
        );
        headers.insert(
            HeaderName::from_static("poly_builder_passphrase"),
            HeaderValue::from_str(&creds.passphrase).map_err(|e| {
                crate::ClaimerError::Internal(format!("Invalid builder passphrase header: {}", e))
            })?,
        );
        headers.insert(
            HeaderName::from_static("poly_builder_signature"),
            HeaderValue::from_str(&signature).map_err(|e| {
                crate::ClaimerError::Internal(format!("Invalid builder signature header: {}", e))
            })?,
        );
        headers.insert(
            HeaderName::from_static("poly_builder_timestamp"),
            HeaderValue::from_str(&timestamp.to_string()).map_err(|e| {
                crate::ClaimerError::Internal(format!("Invalid builder timestamp header: {}", e))
            })?,
        );
        Ok(headers)
    }
}

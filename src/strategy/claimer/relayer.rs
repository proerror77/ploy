use alloy::primitives::Address;
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::TransactionRequest as AlloyTransactionRequest;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
#[cfg(feature = "builder_relayer_sdk")]
use builder_relayer_client_rust::signer::DummySigner;
#[cfg(feature = "builder_relayer_sdk")]
use builder_relayer_client_rust::{
    CallType as BuilderCallType, ProxyTransaction as BuilderProxyTransaction, RelayClient,
    RelayerTxType as BuilderRelayerTxType,
};
#[cfg(feature = "builder_relayer_sdk")]
use builder_signing_sdk_rs::BuilderApiKeyCreds;
use chrono::Utc;
use ethers_core::abi::{encode as abi_encode, AbiParser, Token};
use ethers_core::types::{Address as EthersAddress, H256 as EthersH256, U256 as EthersU256};
use ethers_core::utils::{
    get_create2_address_from_hash as ethers_get_create2_address_from_hash, keccak256,
};
use ethers_signers::{LocalWallet, Signer as _};
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{info, warn};

use crate::error::Result;

use super::{
    env_flag, env_string_any, env_u64_any, AutoClaimer, RedeemablePosition,
    CONDITIONAL_TOKENS_POLYGON, POLYGON_CHAIN_ID, POLYGON_RPC_DEFAULT, USDC_E_POLYGON,
};

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

#[derive(Debug, Clone)]
struct RelayerBuilderCredentials {
    api_key: String,
    secret: String,
    passphrase: String,
}

#[derive(Debug, Deserialize)]
struct RelayerPayloadResponse {
    address: String,
    nonce: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RelayerSignatureParams {
    #[serde(rename = "gasPrice")]
    gas_price: String,
    #[serde(rename = "gasLimit")]
    gas_limit: String,
    #[serde(rename = "relayerFee")]
    relayer_fee: String,
    #[serde(rename = "relayHub")]
    relay_hub: String,
    relay: String,
}

#[derive(Debug, Serialize)]
struct RelayerSubmitRequest {
    #[serde(rename = "type")]
    tx_type: String,
    from: String,
    to: String,
    #[serde(rename = "proxyWallet")]
    proxy_wallet: String,
    data: String,
    nonce: String,
    signature: String,
    #[serde(rename = "signatureParams")]
    signature_params: RelayerSignatureParams,
    metadata: String,
}

#[derive(Debug, Deserialize)]
struct RelayerSubmitResponse {
    #[serde(rename = "transactionID")]
    transaction_id: String,
    state: String,
    #[serde(rename = "transactionHash")]
    transaction_hash: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RelayerTransactionStatus {
    state: String,
    #[serde(rename = "transactionHash")]
    transaction_hash: Option<String>,
}

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

fn relayer_builder_credentials() -> Option<RelayerBuilderCredentials> {
    let api_key = env_string_any(&RELAYER_BUILDER_API_KEY_ENV_KEYS)?;
    let secret = env_string_any(&RELAYER_BUILDER_SECRET_ENV_KEYS)?;
    let passphrase = env_string_any(&RELAYER_BUILDER_PASSPHRASE_ENV_KEYS)?;
    Some(RelayerBuilderCredentials {
        api_key,
        secret,
        passphrase,
    })
}

fn relayer_hmac_signature(secret_base64: &str, message: &str) -> Result<String> {
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

fn ethers_to_alloy_address(value: EthersAddress) -> Address {
    Address::from_slice(value.as_bytes())
}

fn compact_http_body(raw: &str, max_chars: usize) -> String {
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

fn ensure_0x_prefix(hex: &str) -> String {
    if hex.starts_with("0x") || hex.starts_with("0X") {
        return hex.to_string();
    }
    format!("0x{}", hex)
}

impl AutoClaimer {
    fn encode_ctf_redeem_calldata(condition_id: [u8; 32]) -> Result<Vec<u8>> {
        let function = AbiParser::default()
            .parse_function(
                "function redeemPositions(address collateralToken, bytes32 parentCollectionId, bytes32 conditionId, uint256[] indexSets)",
            )
            .map_err(|e| {
                crate::error::PloyError::Internal(format!("Failed to parse redeem ABI: {}", e))
            })?;

        let usdc_addr: EthersAddress = USDC_E_POLYGON.parse().map_err(|e| {
            crate::error::PloyError::AddressParsing(format!("Invalid USDC.e address: {}", e))
        })?;

        function
            .encode_input(&[
                Token::Address(usdc_addr),
                Token::FixedBytes(vec![0u8; 32]),
                Token::FixedBytes(condition_id.to_vec()),
                Token::Array(vec![
                    Token::Uint(EthersU256::from(1u8)),
                    Token::Uint(EthersU256::from(2u8)),
                ]),
            ])
            .map_err(|e| {
                crate::error::PloyError::Internal(format!(
                    "Failed to encode redeem calldata: {}",
                    e
                ))
            })
    }

    fn encode_proxy_transaction_data(
        call_to: EthersAddress,
        call_data: Vec<u8>,
    ) -> Result<Vec<u8>> {
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

    fn derive_proxy_wallet_address(signer: EthersAddress) -> Result<EthersAddress> {
        let proxy_factory: EthersAddress = RELAYER_PROXY_FACTORY_POLYGON.parse().map_err(|e| {
            crate::error::PloyError::AddressParsing(format!("Invalid relayer proxy factory: {}", e))
        })?;
        let init_hash: EthersH256 = RELAYER_PROXY_INIT_CODE_HASH.parse().map_err(|e| {
            crate::error::PloyError::AddressParsing(format!(
                "Invalid relayer proxy init code hash: {}",
                e
            ))
        })?;
        let salt = keccak256(signer.as_bytes());
        Ok(ethers_get_create2_address_from_hash(
            proxy_factory,
            salt,
            init_hash.to_fixed_bytes(),
        ))
    }

    fn create_proxy_struct_hash(
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

    fn build_relayer_builder_headers(
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

    #[cfg(feature = "builder_relayer_sdk")]
    async fn claim_position_via_relayer_proxy_sdk(
        &self,
        pos: &RedeemablePosition,
        builder_creds: &RelayerBuilderCredentials,
        private_key: &str,
    ) -> Result<Option<String>> {
        let signer = DummySigner::new(private_key).map_err(|e| {
            crate::error::PloyError::Wallet(format!("Invalid private key for relayer SDK: {}", e))
        })?;
        let condition_hex = pos
            .condition_id
            .trim()
            .trim_start_matches("0x")
            .trim_start_matches("0X");
        let condition_bytes: [u8; 32] = hex::decode(condition_hex)
            .map_err(|e| crate::error::PloyError::Internal(format!("Invalid condition ID: {}", e)))?
            .try_into()
            .map_err(|_| crate::error::PloyError::Internal("Condition ID wrong length".into()))?;
        let redeem_call_data = Self::encode_ctf_redeem_calldata(condition_bytes)?;
        let metadata = format!(
            "redeem {}",
            &condition_hex.chars().take(16).collect::<String>()
        );
        let polygon_rpc = std::env::var("POLYGON_RPC_URL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| POLYGON_RPC_DEFAULT.to_string());

        let relayer_client = RelayClient::new_with_type(
            relayer_base_url(),
            POLYGON_CHAIN_ID,
            BuilderRelayerTxType::Proxy,
        )
        .with_signer(Box::new(signer.clone()), Box::new(signer))
        .with_builder_api_key(BuilderApiKeyCreds {
            key: builder_creds.api_key.clone(),
            secret: builder_creds.secret.clone(),
            passphrase: builder_creds.passphrase.clone(),
        })
        .with_gas_estimate_rpc(polygon_rpc);

        let submitted = relayer_client
            .execute_proxy_transactions(
                vec![BuilderProxyTransaction {
                    to: CONDITIONAL_TOKENS_POLYGON.to_string(),
                    type_code: BuilderCallType::Call,
                    data: format!("0x{}", hex::encode(redeem_call_data)),
                    value: "0".to_string(),
                }],
                Some(metadata.clone()),
            )
            .await
            .map_err(|e| {
                crate::error::PloyError::OrderSubmission(format!(
                    "Relayer SDK submit failed: {}",
                    e
                ))
            })?;

        info!(
            "Relayer SDK redeem submitted: id={}, state={}, condition={}",
            submitted.transaction_id,
            submitted.state,
            &condition_hex.chars().take(16).collect::<String>()
        );

        for _ in 0..relayer_poll_max() {
            let transactions = relayer_client
                .get_transaction(&submitted.transaction_id)
                .await
                .map_err(|e| {
                    crate::error::PloyError::OrderSubmission(format!(
                        "Relayer SDK polling failed: {}",
                        e
                    ))
                })?;

            if let Some(txn) = transactions.first() {
                match txn.state.as_str() {
                    "STATE_MINED" | "STATE_CONFIRMED" => {
                        let tx_hash = if !txn.transaction_hash.trim().is_empty() {
                            txn.transaction_hash.clone()
                        } else if !submitted.transaction_hash.trim().is_empty() {
                            submitted.transaction_hash.clone()
                        } else {
                            submitted.hash.clone()
                        };
                        info!(
                            "Relayer SDK redeem confirmed: state={}, tx={}",
                            txn.state, tx_hash
                        );
                        return Ok(Some(tx_hash));
                    }
                    "STATE_FAILED" | "STATE_INVALID" => {
                        return Err(crate::error::PloyError::OrderSubmission(format!(
                            "Relayer redeem failed: id={}, state={}",
                            submitted.transaction_id, txn.state
                        )));
                    }
                    _ => {}
                }
            }

            sleep(Duration::from_millis(relayer_poll_interval_ms())).await;
        }

        Err(crate::error::PloyError::OrderTimeout(format!(
            "Relayer redeem polling timed out: id={}",
            submitted.transaction_id
        )))
    }

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

        let signer_wallet = private_key
            .parse::<LocalWallet>()
            .map_err(|e| {
                crate::error::PloyError::Wallet(format!("Invalid private key for relayer: {}", e))
            })?
            .with_chain_id(POLYGON_CHAIN_ID);
        let signer_addr = signer_wallet.address();

        let condition_hex = pos
            .condition_id
            .trim()
            .trim_start_matches("0x")
            .trim_start_matches("0X");
        let condition_bytes: [u8; 32] = hex::decode(condition_hex)
            .map_err(|e| crate::error::PloyError::Internal(format!("Invalid condition ID: {}", e)))?
            .try_into()
            .map_err(|_| crate::error::PloyError::Internal("Condition ID wrong length".into()))?;

        let redeem_call_data = Self::encode_ctf_redeem_calldata(condition_bytes)?;
        let ctf_addr: EthersAddress = CONDITIONAL_TOKENS_POLYGON.parse().map_err(|e| {
            crate::error::PloyError::AddressParsing(format!(
                "Invalid ConditionalTokens address: {}",
                e
            ))
        })?;
        let proxy_factory_addr: EthersAddress =
            RELAYER_PROXY_FACTORY_POLYGON.parse().map_err(|e| {
                crate::error::PloyError::AddressParsing(format!(
                    "Invalid relayer proxy factory: {}",
                    e
                ))
            })?;
        let relay_hub_addr: EthersAddress = RELAYER_RELAY_HUB_POLYGON.parse().map_err(|e| {
            crate::error::PloyError::AddressParsing(format!("Invalid relayer hub address: {}", e))
        })?;
        let proxy_wallet = Self::derive_proxy_wallet_address(signer_addr)?;
        let proxy_call_data = Self::encode_proxy_transaction_data(ctf_addr, redeem_call_data)?;

        let polygon_rpc = std::env::var("POLYGON_RPC_URL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| POLYGON_RPC_DEFAULT.to_string());
        let rpc_url = polygon_rpc.parse().map_err(|e| {
            crate::error::PloyError::AddressParsing(format!("Invalid RPC URL: {}", e))
        })?;
        let provider = ProviderBuilder::new().connect_http(rpc_url);

        let gas_estimate_tx = AlloyTransactionRequest::default()
            .from(ethers_to_alloy_address(signer_addr))
            .to(ethers_to_alloy_address(proxy_factory_addr))
            .input(proxy_call_data.clone().into());
        let gas_limit = match provider.estimate_gas(gas_estimate_tx).await {
            Ok(v) => EthersU256::from(v),
            Err(e) => {
                warn!(
                    "Relayer redeem gas estimation failed, using default {}: {}",
                    RELAYER_DEFAULT_GAS_LIMIT, e
                );
                EthersU256::from(RELAYER_DEFAULT_GAS_LIMIT)
            }
        };

        let relayer_url = relayer_base_url();
        let relayer_base = relayer_url.trim_end_matches('/').to_string();
        let http = reqwest::Client::new();

        let relay_payload_resp = http
            .get(format!("{}/relay-payload", relayer_base))
            .query(&[
                ("address", format!("{:#x}", signer_addr)),
                ("type", "PROXY".to_string()),
            ])
            .send()
            .await
            .map_err(crate::error::PloyError::Http)?
            .error_for_status()
            .map_err(crate::error::PloyError::Http)?;
        let relay_payload: RelayerPayloadResponse = relay_payload_resp
            .json()
            .await
            .map_err(crate::error::PloyError::Http)?;

        let relay_addr: EthersAddress = relay_payload.address.parse().map_err(|e| {
            crate::error::PloyError::AddressParsing(format!(
                "Invalid relayer payload address {}: {}",
                relay_payload.address, e
            ))
        })?;
        let nonce = EthersU256::from_dec_str(relay_payload.nonce.trim()).map_err(|e| {
            crate::error::PloyError::Internal(format!(
                "Invalid relayer payload nonce {}: {}",
                relay_payload.nonce, e
            ))
        })?;

        let struct_hash = Self::create_proxy_struct_hash(
            signer_addr,
            proxy_factory_addr,
            &proxy_call_data,
            EthersU256::zero(),
            EthersU256::zero(),
            gas_limit,
            nonce,
            relay_hub_addr,
            relay_addr,
        );
        let signature = ensure_0x_prefix(
            &signer_wallet
                .sign_message(struct_hash.as_bytes())
                .await
                .map_err(|e| {
                    crate::error::PloyError::Signature(format!(
                        "Relayer proxy signature failed: {}",
                        e
                    ))
                })?
                .to_string(),
        );

        let submit_req = RelayerSubmitRequest {
            tx_type: "PROXY".to_string(),
            from: format!("{:#x}", signer_addr),
            to: format!("{:#x}", proxy_factory_addr),
            proxy_wallet: format!("{:#x}", proxy_wallet),
            data: format!("0x{}", hex::encode(proxy_call_data)),
            nonce: relay_payload.nonce,
            signature,
            signature_params: RelayerSignatureParams {
                gas_price: "0".to_string(),
                gas_limit: gas_limit.to_string(),
                relayer_fee: "0".to_string(),
                relay_hub: format!("{:#x}", relay_hub_addr),
                relay: format!("{:#x}", relay_addr),
            },
            metadata: format!(
                "redeem {}",
                &condition_hex.chars().take(16).collect::<String>()
            ),
        };
        let submit_body = serde_json::to_string(&submit_req)?;
        let ts = Utc::now().timestamp();
        let headers = Self::build_relayer_builder_headers(&builder_creds, ts, &submit_body)?;

        let submit_resp = http
            .post(format!("{}/submit", relayer_base))
            .headers(headers)
            .body(submit_body)
            .send()
            .await
            .map_err(crate::error::PloyError::Http)?;
        let submit_status = submit_resp.status();
        let submit_text = submit_resp
            .text()
            .await
            .map_err(crate::error::PloyError::Http)?;
        if !submit_status.is_success() {
            return Err(crate::error::PloyError::OrderSubmission(format!(
                "Relayer submit failed: status={}, body={}",
                submit_status,
                compact_http_body(&submit_text, 4096)
            )));
        }
        let submitted: RelayerSubmitResponse = serde_json::from_str(&submit_text).map_err(|e| {
            crate::error::PloyError::Internal(format!(
                "Invalid relayer submit response JSON: {}, body={}",
                e,
                compact_http_body(&submit_text, 4096)
            ))
        })?;

        info!(
            "Relayer redeem submitted: id={}, state={}, condition={}",
            submitted.transaction_id,
            submitted.state,
            &condition_hex.chars().take(16).collect::<String>()
        );

        for _ in 0..relayer_poll_max() {
            let status_resp = http
                .get(format!("{}/transaction", relayer_base))
                .query(&[("id", submitted.transaction_id.as_str())])
                .send()
                .await
                .map_err(crate::error::PloyError::Http)?
                .error_for_status()
                .map_err(crate::error::PloyError::Http)?;
            let transactions: Vec<RelayerTransactionStatus> = status_resp
                .json()
                .await
                .map_err(crate::error::PloyError::Http)?;

            if let Some(txn) = transactions.first() {
                match txn.state.as_str() {
                    "STATE_MINED" | "STATE_CONFIRMED" => {
                        let tx_hash = txn
                            .transaction_hash
                            .clone()
                            .or_else(|| submitted.transaction_hash.clone())
                            .unwrap_or_default();
                        info!(
                            "Relayer redeem confirmed: state={}, tx={}",
                            txn.state, tx_hash
                        );
                        return Ok(Some(tx_hash));
                    }
                    "STATE_FAILED" | "STATE_INVALID" => {
                        return Err(crate::error::PloyError::OrderSubmission(format!(
                            "Relayer redeem failed: id={}, state={}",
                            submitted.transaction_id, txn.state
                        )));
                    }
                    _ => {}
                }
            }

            sleep(Duration::from_millis(relayer_poll_interval_ms())).await;
        }

        Err(crate::error::PloyError::OrderTimeout(format!(
            "Relayer redeem polling timed out: id={}",
            submitted.transaction_id
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use super::super::{AutoClaimer, POLYGON_CHAIN_ID, USDC_E_POLYGON};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn set_env(key: &str, value: Option<&str>) {
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn test_relayer_hmac_signature_urlsafe_base64() {
        let sig = relayer_hmac_signature("dGVzdHNlY3JldA==", "123POST/submit{\"a\":1}")
            .expect("signature should be created");
        assert_eq!(sig, "5UKMaApqgL6X7RdBVDJLKCU_aDY7kSpONfbGIEZAX0s=");
    }

    #[test]
    fn test_relayer_hmac_signature_accepts_urlsafe_secret_variant() {
        let sig = relayer_hmac_signature(
            "Ndt7ZPLgVWpSzXHGFMohLB33x_Z4qCfqjiMYBwmxamE=",
            "1700000000POST/submit{}",
        )
        .expect("url-safe builder secret should decode");
        assert!(!sig.is_empty());
    }

    #[test]
    fn test_ensure_0x_prefix() {
        assert_eq!(ensure_0x_prefix("abcd"), "0xabcd");
        assert_eq!(ensure_0x_prefix("0xabcd"), "0xabcd");
    }

    #[test]
    fn test_missing_relayer_builder_credential_groups() {
        let _guard = ENV_LOCK.lock().expect("env lock");

        let all_keys: Vec<&str> = RELAYER_BUILDER_API_KEY_ENV_KEYS
            .iter()
            .chain(RELAYER_BUILDER_SECRET_ENV_KEYS.iter())
            .chain(RELAYER_BUILDER_PASSPHRASE_ENV_KEYS.iter())
            .copied()
            .collect();
        let prev: Vec<(&str, Option<String>)> = all_keys
            .iter()
            .map(|k| (*k, std::env::var(k).ok()))
            .collect();

        for key in &all_keys {
            set_env(key, None);
        }
        set_env("POLY_BUILDER_API_KEY", Some("k"));
        set_env("POLY_BUILDER_PASSPHRASE", Some("p"));
        assert_eq!(missing_relayer_builder_credential_groups(), vec!["secret"]);

        set_env("BUILDER_SECRET", Some("s"));
        assert!(missing_relayer_builder_credential_groups().is_empty());

        for (key, val) in prev {
            set_env(key, val.as_deref());
        }
    }

    #[test]
    fn test_derive_proxy_wallet_address_matches_known_vector() {
        let signer: EthersAddress = "0x9d699747148fd637a7d2514f9b3e3028bf59195c"
            .parse()
            .expect("valid signer");
        let proxy = AutoClaimer::derive_proxy_wallet_address(signer)
            .expect("proxy address should derive correctly");
        assert_eq!(
            format!("{:#x}", proxy),
            "0xcbaaa60c5dec85eac2a2c424bdcd7258ab67eee2"
        );
    }

    #[test]
    fn test_encode_proxy_transaction_data_accepts_tuple_calls() {
        let call_to: EthersAddress = "0x4D97DCd97eC945f40cF65F87097ACe5EA0476045"
            .parse()
            .expect("valid call target");
        let encoded = AutoClaimer::encode_proxy_transaction_data(call_to, vec![0x12, 0x34])
            .expect("proxy calldata should encode");
        assert!(!encoded.is_empty());
    }

    #[tokio::test]
    async fn test_proxy_signature_matches_builder_relayer_client_vector() {
        let private_key = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
        let signer_wallet: LocalWallet = private_key.parse().expect("known private key");
        let signer_addr = signer_wallet.address();

        let proxy_factory: EthersAddress = RELAYER_PROXY_FACTORY_POLYGON
            .parse()
            .expect("proxy factory");
        let relay_hub: EthersAddress = RELAYER_RELAY_HUB_POLYGON.parse().expect("relay hub");
        let relay_addr: EthersAddress = "0xae700edfd9ab986395f3999fe11177b9903a52f1"
            .parse()
            .expect("relay address");
        let usdc: EthersAddress = USDC_E_POLYGON.parse().expect("usdc");
        let approve_calldata = hex::decode(
            "095ea7b30000000000000000000000004d97dcd97ec945f40cf65f87097ace5ea0476045ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        )
        .expect("approve calldata");
        let proxy_call_data =
            AutoClaimer::encode_proxy_transaction_data(usdc, approve_calldata).expect("proxy data");

        let struct_hash = AutoClaimer::create_proxy_struct_hash(
            signer_addr,
            proxy_factory,
            &proxy_call_data,
            EthersU256::zero(),
            EthersU256::zero(),
            EthersU256::from(85_338u64),
            EthersU256::zero(),
            relay_hub,
            relay_addr,
        );
        let sig = ensure_0x_prefix(
            &signer_wallet
                .with_chain_id(POLYGON_CHAIN_ID)
                .sign_message(struct_hash.as_bytes())
                .await
                .expect("signature")
                .to_string(),
        );

        assert_eq!(
            sig,
            "0x4c18e2d2294a00d686714aff8e7936ab657cb4655dfccb2b556efadcb7e835f800dc2fecec69c501e29bb36ecb54b4da6b7c410c4dc740a33af2afde2b77297e1b"
        );
    }
}

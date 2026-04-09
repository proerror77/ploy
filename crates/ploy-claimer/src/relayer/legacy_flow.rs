use super::proxy_support::{
    RelayerBuilderCredentials, RelayerPayloadResponse, RelayerSignatureParams,
    RelayerSubmitRequest, RelayerSubmitResponse, RelayerTransactionStatus, compact_http_body,
    ensure_0x_prefix, ethers_to_alloy_address,
};
use super::*;
use serde_json;

use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::TransactionRequest as AlloyTransactionRequest;
use chrono::Utc;
use ethers_core::types::{Address as EthersAddress, U256 as EthersU256};
use ethers_signers::{LocalWallet, Signer as _};
use std::time::Duration;
use tokio::time::sleep;
use tracing::{info, warn};

impl AutoClaimer {
    pub(super) async fn claim_position_via_relayer_proxy_legacy(
        &self,
        pos: &RedeemablePosition,
        builder_creds: &RelayerBuilderCredentials,
        private_key: &str,
    ) -> Result<Option<String>, crate::ClaimerError> {
        let signer_wallet = private_key
            .parse::<LocalWallet>()
            .map_err(|e| {
                crate::ClaimerError::Wallet(format!("Invalid private key for relayer: {}", e))
            })?
            .with_chain_id(POLYGON_CHAIN_ID);
        let signer_addr = signer_wallet.address();

        let condition_hex = pos
            .condition_id
            .trim()
            .trim_start_matches("0x")
            .trim_start_matches("0X");
        let condition_bytes: [u8; 32] = hex::decode(condition_hex)
            .map_err(|e| crate::ClaimerError::Internal(format!("Invalid condition ID: {}", e)))?
            .try_into()
            .map_err(|_| crate::ClaimerError::Internal("Condition ID wrong length".into()))?;

        let redeem_amounts = pos
            .claim_amounts
            .iter()
            .map(|amount| crate::decimal_to_token_units(*amount).map(EthersU256::from))
            .collect::<Result<Vec<_>, _>>()?;
        let redeem_call_data =
            Self::encode_redeem_calldata(condition_bytes, pos.neg_risk, &redeem_amounts)?;
        let call_target: EthersAddress = if pos.neg_risk {
            NEG_RISK_ADAPTER_POLYGON.parse().map_err(|e| {
                crate::ClaimerError::Network(format!(
                    "Invalid NegRisk adapter address: {}",
                    e
                ))
            })?
        } else {
            CONDITIONAL_TOKENS_POLYGON.parse().map_err(|e| {
                crate::ClaimerError::Network(format!(
                    "Invalid ConditionalTokens address: {}",
                    e
                ))
            })?
        };
        let proxy_factory_addr: EthersAddress =
            RELAYER_PROXY_FACTORY_POLYGON.parse().map_err(|e| {
                crate::ClaimerError::Network(format!(
                    "Invalid relayer proxy factory: {}",
                    e
                ))
            })?;
        let relay_hub_addr: EthersAddress = RELAYER_RELAY_HUB_POLYGON.parse().map_err(|e| {
            crate::ClaimerError::Network(format!("Invalid relayer hub address: {}", e))
        })?;
        let proxy_wallet = Self::derive_proxy_wallet_address(signer_addr)?;
        let proxy_call_data =
            Self::encode_proxy_transaction_data(call_target, redeem_call_data)?;

        let polygon_rpc = std::env::var("POLYGON_RPC_URL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| POLYGON_RPC_DEFAULT.to_string());
        let rpc_url = polygon_rpc.parse().map_err(|e| {
            crate::ClaimerError::Network(format!("Invalid RPC URL: {}", e))
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

        let relayer_base = relayer_base_url().trim_end_matches('/').to_string();
        let http = reqwest::Client::new();

        let relay_payload = fetch_relay_payload(&http, &relayer_base, signer_addr).await?;
        let relay_addr: EthersAddress = relay_payload.address.parse().map_err(|e| {
            crate::ClaimerError::Network(format!(
                "Invalid relayer payload address {}: {}",
                relay_payload.address, e
            ))
        })?;
        let nonce = EthersU256::from_dec_str(relay_payload.nonce.trim()).map_err(|e| {
            crate::ClaimerError::Internal(format!(
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
                    crate::ClaimerError::Internal(format!(
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
        let submitted =
            submit_relayer_request(&http, &relayer_base, builder_creds, &submit_req).await?;

        info!(
            "Relayer redeem submitted: id={}, state={}, condition={}",
            submitted.transaction_id,
            submitted.state,
            &condition_hex.chars().take(16).collect::<String>()
        );

        poll_submitted_relayer_transaction(&http, &relayer_base, &submitted).await
    }
}

async fn fetch_relay_payload(
    http: &reqwest::Client,
    relayer_base: &str,
    signer_addr: EthersAddress,
) -> Result<RelayerPayloadResponse, crate::ClaimerError> {
    http.get(format!("{}/relay-payload", relayer_base))
        .query(&[
            ("address", format!("{:#x}", signer_addr)),
            ("type", "PROXY".to_string()),
        ])
        .send()
        .await
        .map_err(|e| crate::ClaimerError::Network(e.to_string()))?
        .error_for_status()
        .map_err(|e| crate::ClaimerError::Network(e.to_string()))?
        .json()
        .await
        .map_err(|e| crate::ClaimerError::Network(e.to_string()))
}

async fn submit_relayer_request(
    http: &reqwest::Client,
    relayer_base: &str,
    builder_creds: &RelayerBuilderCredentials,
    submit_req: &RelayerSubmitRequest,
) -> Result<RelayerSubmitResponse, crate::ClaimerError> {
    let submit_body = serde_json::to_string(submit_req)
        .map_err(|e| crate::ClaimerError::Internal(format!("JSON serialize failed: {e}")))?;
    let ts = Utc::now().timestamp();
    let headers = AutoClaimer::build_relayer_builder_headers(builder_creds, ts, &submit_body)?;

    let submit_resp = http
        .post(format!("{}/submit", relayer_base))
        .headers(headers)
        .body(submit_body)
        .send()
        .await
        .map_err(|e| crate::ClaimerError::Network(e.to_string()))?;
    let submit_status = submit_resp.status();
    let submit_text = submit_resp
        .text()
        .await
        .map_err(|e| crate::ClaimerError::Network(e.to_string()))?;
    if !submit_status.is_success() {
        return Err(crate::ClaimerError::Contract(format!(
            "Relayer submit failed: status={}, body={}",
            submit_status,
            compact_http_body(&submit_text, 4096)
        )));
    }

    serde_json::from_str(&submit_text).map_err(|e| {
        crate::ClaimerError::Internal(format!(
            "Invalid relayer submit response JSON: {}, body={}",
            e,
            compact_http_body(&submit_text, 4096)
        ))
    })
}

async fn poll_submitted_relayer_transaction(
    http: &reqwest::Client,
    relayer_base: &str,
    submitted: &RelayerSubmitResponse,
) -> Result<Option<String>, crate::ClaimerError> {
    for _ in 0..relayer_poll_max() {
        let status_resp = http
            .get(format!("{}/transaction", relayer_base))
            .query(&[("id", submitted.transaction_id.as_str())])
            .send()
            .await
            .map_err(|e| crate::ClaimerError::Network(e.to_string()))?
            .error_for_status()
            .map_err(|e| crate::ClaimerError::Network(e.to_string()))?;
        let transactions: Vec<RelayerTransactionStatus> = status_resp
            .json()
            .await
            .map_err(|e| crate::ClaimerError::Network(e.to_string()))?;

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
                    return Err(crate::ClaimerError::Contract(format!(
                        "Relayer redeem failed: id={}, state={}",
                        submitted.transaction_id, txn.state
                    )));
                }
                _ => {}
            }
        }

        sleep(Duration::from_millis(relayer_poll_interval_ms())).await;
    }

    Err(crate::ClaimerError::Contract(format!(
        "Relayer redeem polling timed out: id={}",
        submitted.transaction_id
    )))
}

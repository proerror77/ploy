#[cfg(feature = "builder_relayer_sdk")]
use super::AutoClaimer;
#[cfg(feature = "builder_relayer_sdk")]
use super::proxy_support::RelayerBuilderCredentials;
#[cfg(feature = "builder_relayer_sdk")]
use super::{
    CONDITIONAL_TOKENS_POLYGON, POLYGON_CHAIN_ID, POLYGON_RPC_DEFAULT, RedeemablePosition,
    relayer_base_url, relayer_poll_interval_ms, relayer_poll_max,
};
#[cfg(feature = "builder_relayer_sdk")]

#[cfg(feature = "builder_relayer_sdk")]
use builder_relayer_client_rust::signer::DummySigner;
#[cfg(feature = "builder_relayer_sdk")]
use builder_relayer_client_rust::{
    CallType as BuilderCallType, ProxyTransaction as BuilderProxyTransaction, RelayClient,
    RelayerTxType as BuilderRelayerTxType,
};
#[cfg(feature = "builder_relayer_sdk")]
use builder_signing_sdk_rs::BuilderApiKeyCreds;
#[cfg(feature = "builder_relayer_sdk")]
use std::time::Duration;
#[cfg(feature = "builder_relayer_sdk")]
use tokio::time::sleep;
#[cfg(feature = "builder_relayer_sdk")]
use tracing::info;

#[cfg(feature = "builder_relayer_sdk")]
impl AutoClaimer {
    pub(super) async fn claim_position_via_relayer_proxy_sdk(
        &self,
        pos: &RedeemablePosition,
        builder_creds: &RelayerBuilderCredentials,
        private_key: &str,
    ) -> Result<Option<String>, crate::ClaimerError> {
        let signer = DummySigner::new(private_key).map_err(|e| {
            crate::ClaimerError::Wallet(format!("Invalid private key for relayer SDK: {}", e))
        })?;
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
            .map(|amount| crate::decimal_to_token_units(*amount).map(ethers_core::types::U256::from))
            .collect::<Result<Vec<_>, _>>()?;
        let redeem_call_data =
            Self::encode_redeem_calldata(condition_bytes, pos.neg_risk, &redeem_amounts)?;
        let metadata = format!(
            "redeem {}",
            &condition_hex.chars().take(16).collect::<String>()
        );
        let polygon_rpc = std::env::var("POLYGON_RPC_URL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| POLYGON_RPC_DEFAULT.to_string());

        let call_target = if pos.neg_risk {
            NEG_RISK_ADAPTER_POLYGON.to_string()
        } else {
            CONDITIONAL_TOKENS_POLYGON.to_string()
        };

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
                    to: call_target,
                    type_code: BuilderCallType::Call,
                    data: format!("0x{}", hex::encode(redeem_call_data)),
                    value: "0".to_string(),
                }],
                Some(metadata.clone()),
            )
            .await
            .map_err(|e| {
                crate::ClaimerError::Contract(format!(
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
                    crate::ClaimerError::Contract(format!(
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
}

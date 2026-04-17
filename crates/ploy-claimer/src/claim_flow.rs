use super::*;
use crate::ClaimerError;

pub(super) async fn preflight_wallet_can_claim(
    claimer: &AutoClaimer,
) -> Result<bool, ClaimerError> {
    let private_key = claimer
        .config
        .private_key
        .as_ref()
        .ok_or_else(|| ClaimerError::Wallet("No private key for claiming".into()))?;

    let signer: PrivateKeySigner = private_key
        .parse()
        .map_err(|e| ClaimerError::Wallet(format!("Invalid private key: {}", e)))?;

    let polygon_rpc = std::env::var("POLYGON_RPC_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| POLYGON_RPC_DEFAULT.to_string());
    let rpc_url = polygon_rpc
        .parse()
        .map_err(|e| ClaimerError::Network(format!("Invalid RPC URL: {}", e)))?;
    let provider = ProviderBuilder::new().connect_http(rpc_url);

    let wallet_addr = signer.address();
    let balance = provider.get_balance(wallet_addr).await.map_err(|e| {
        ClaimerError::Contract(format!("Failed to read claimer wallet balance: {}", e))
    })?;
    let min_balance = min_native_gas_wei();

    let mut effective_balance = balance;
    if effective_balance < min_balance {
        if let Some(updated) = claimer
            .maybe_auto_topup_wallet(wallet_addr, effective_balance, min_balance)
            .await?
        {
            effective_balance = updated;
        }
    }

    if effective_balance < min_balance {
        warn!(
            "Auto-claim paused: wallet {} has {} wei, need at least {} wei for gas. Top up MATIC and claimer will resume automatically.",
            wallet_addr, effective_balance, min_balance
        );
        return Ok(false);
    }

    debug!(
        "Claimer wallet {} gas check passed: {} wei",
        wallet_addr, effective_balance
    );
    Ok(true)
}

pub(super) async fn maybe_auto_topup_wallet(
    claimer: &AutoClaimer,
    target_wallet: Address,
    current_balance: U256,
    min_balance: U256,
) -> Result<Option<U256>, ClaimerError> {
    if !auto_topup_enabled() {
        return Ok(None);
    }

    let Some(topup_private_key) = env_string_any(&[
        "CLAIMER_AUTO_TOPUP_PRIVATE_KEY",
        "CLAIMER_GAS_TOPUP_PRIVATE_KEY",
    ]) else {
        warn!(
            "Auto top-up enabled but missing CLAIMER_AUTO_TOPUP_PRIVATE_KEY/CLAIMER_GAS_TOPUP_PRIVATE_KEY"
        );
        return Ok(None);
    };

    let threshold_wei = env_u128_any(&[
        "CLAIMER_AUTO_TOPUP_THRESHOLD_WEI",
        "CLAIMER_GAS_TOPUP_THRESHOLD_WEI",
    ])
    .unwrap_or_else(|| u256_to_u128_saturating(min_balance));

    let current_wei = u256_to_u128_saturating(current_balance);
    if current_wei >= threshold_wei {
        return Ok(Some(current_balance));
    }

    let target_wei = env_u128_any(&[
        "CLAIMER_AUTO_TOPUP_TARGET_WEI",
        "CLAIMER_GAS_TOPUP_TARGET_WEI",
    ])
    .unwrap_or(DEFAULT_AUTO_TOPUP_TARGET_WEI)
    .max(threshold_wei);

    if target_wei <= current_wei {
        return Ok(Some(current_balance));
    }

    let max_per_tx_wei = env_u128_any(&[
        "CLAIMER_AUTO_TOPUP_MAX_PER_TX_WEI",
        "CLAIMER_GAS_TOPUP_MAX_PER_TX_WEI",
    ])
    .unwrap_or(DEFAULT_AUTO_TOPUP_MAX_PER_TX_WEI)
    .max(1);

    let daily_cap_wei = env_u128_any(&[
        "CLAIMER_AUTO_TOPUP_DAILY_CAP_WEI",
        "CLAIMER_GAS_TOPUP_DAILY_CAP_WEI",
    ])
    .unwrap_or(DEFAULT_AUTO_TOPUP_DAILY_CAP_WEI);

    let reserve_wei = env_u128_any(&[
        "CLAIMER_AUTO_TOPUP_RESERVE_WEI",
        "CLAIMER_GAS_TOPUP_RESERVE_WEI",
    ])
    .unwrap_or(DEFAULT_AUTO_TOPUP_RESERVE_WEI);

    let desired_wei = target_wei.saturating_sub(current_wei);
    let mut topup_wei = desired_wei.min(max_per_tx_wei);

    {
        let today = Utc::now().date_naive();
        let mut state = claimer.gas_topup_state.write().await;
        if state.day != today {
            state.day = today;
            state.spent_wei = 0;
        }

        if state.spent_wei >= daily_cap_wei {
            warn!(
                "Auto top-up skipped: daily cap reached (spent={} wei, cap={} wei)",
                state.spent_wei, daily_cap_wei
            );
            return Ok(None);
        }

        let remaining_today = daily_cap_wei.saturating_sub(state.spent_wei);
        topup_wei = topup_wei.min(remaining_today);
    }

    if topup_wei == 0 {
        debug!("Auto top-up skipped: computed top-up amount is 0 wei");
        return Ok(None);
    }

    let polygon_rpc = std::env::var("POLYGON_RPC_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| POLYGON_RPC_DEFAULT.to_string());
    let rpc_url = match polygon_rpc.parse() {
        Ok(url) => url,
        Err(e) => {
            warn!("Auto top-up skipped: invalid POLYGON_RPC_URL: {}", e);
            return Ok(None);
        }
    };

    let topup_signer = match topup_private_key.parse::<PrivateKeySigner>() {
        Ok(signer) => signer.with_chain_id(Some(POLYGON_CHAIN_ID)),
        Err(e) => {
            warn!("Auto top-up skipped: invalid top-up private key: {}", e);
            return Ok(None);
        }
    };
    let topup_addr = topup_signer.address();
    let wallet_provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(topup_signer))
        .connect_http(rpc_url);

    let topup_balance_wei = match wallet_provider.get_balance(topup_addr).await {
        Ok(v) => u256_to_u128_saturating(v),
        Err(e) => {
            warn!(
                "Auto top-up skipped: failed reading top-up wallet balance: {}",
                e
            );
            return Ok(None);
        }
    };

    let required_wei = topup_wei.saturating_add(reserve_wei);
    if topup_balance_wei < required_wei {
        warn!(
            "Auto top-up skipped: top-up wallet {} has {} wei, needs at least {} wei (topup={} + reserve={})",
            topup_addr, topup_balance_wei, required_wei, topup_wei, reserve_wei
        );
        return Ok(None);
    }

    info!(
        "Auto top-up triggered: sending {} wei to claimer wallet {} (current={} threshold={} target={})",
        topup_wei, target_wallet, current_wei, threshold_wei, target_wei
    );

    let tx = AlloyTransactionRequest::default()
        .to(target_wallet)
        .value(U256::from(topup_wei));

    let pending_tx = match wallet_provider.send_transaction(tx).await {
        Ok(p) => p,
        Err(e) => {
            warn!("Auto top-up tx submission failed: {}", e);
            return Ok(None);
        }
    };

    let receipt = match pending_tx.get_receipt().await {
        Ok(r) => r,
        Err(e) => {
            warn!("Auto top-up tx confirmation failed: {}", e);
            return Ok(None);
        }
    };

    if !receipt.status() {
        warn!(
            "Auto top-up tx reverted: hash={:?}, status={}",
            receipt.transaction_hash,
            receipt.status()
        );
        return Ok(None);
    }

    {
        let today = Utc::now().date_naive();
        let mut state = claimer.gas_topup_state.write().await;
        if state.day != today {
            state.day = today;
            state.spent_wei = 0;
        }
        state.spent_wei = state.spent_wei.saturating_add(topup_wei);
    }

    let refreshed_alloy = match wallet_provider.get_balance(target_wallet).await {
        Ok(v) => v,
        Err(e) => {
            warn!(
                "Auto top-up sent but failed to read refreshed balance: {}",
                e
            );
            return Ok(None);
        }
    };
    let refreshed = u256_to_u128_saturating(refreshed_alloy);

    info!(
        "Auto top-up success: tx={:?}, new claimer wallet balance={} wei",
        receipt.transaction_hash, refreshed
    );
    Ok(Some(refreshed_alloy))
}

pub(super) async fn claim_position(
    claimer: &AutoClaimer,
    pos: &RedeemablePosition,
) -> Result<String, ClaimerError> {
    let mut attempted_relayer = false;
    if relayer_claim_enabled() && relayer_builder_credentials_available() {
        attempted_relayer = true;
        match claimer.claim_position_via_relayer_proxy(pos).await {
            Ok(Some(tx_hash)) => return Ok(tx_hash),
            Ok(None) => {}
            Err(e) => {
                if !relayer_fallback_onchain_enabled() {
                    return Err(e);
                }
                warn!(
                    "Relayer redeem failed, falling back to direct on-chain redeem: {}",
                    e
                );
            }
        }
    }
    if attempted_relayer
        && relayer_fallback_onchain_enabled()
        && !claimer.preflight_wallet_can_claim().await?
    {
        return Err(ClaimerError::Wallet(
            "Insufficient native gas for on-chain fallback redeem".into(),
        ));
    }

    let private_key = claimer
        .config
        .private_key
        .as_ref()
        .ok_or_else(|| ClaimerError::Wallet("No private key for claiming".into()))?;

    let signer: PrivateKeySigner = private_key
        .parse()
        .map_err(|e| ClaimerError::Wallet(format!("Invalid private key: {}", e)))?;

    let wallet = EthereumWallet::from(signer);

    let polygon_rpc = std::env::var("POLYGON_RPC_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| POLYGON_RPC_DEFAULT.to_string());
    let rpc_url = polygon_rpc
        .parse()
        .map_err(|e| ClaimerError::Network(format!("Invalid RPC URL: {}", e)))?;
    let provider = ProviderBuilder::new().wallet(wallet).connect_http(rpc_url);

    let conditional_tokens_addr: Address = CONDITIONAL_TOKENS_POLYGON
        .parse()
        .map_err(|e| ClaimerError::Network(format!("Invalid ConditionalTokens address: {}", e)))?;
    let neg_risk_adapter_addr: Address = NEG_RISK_ADAPTER_POLYGON
        .parse()
        .map_err(|e| ClaimerError::Network(format!("Invalid NegRisk adapter address: {}", e)))?;
    let collateral_addr: Address = USDC_E_POLYGON
        .parse()
        .map_err(|e| ClaimerError::Network(format!("Invalid USDC.e address: {}", e)))?;

    let contract = IConditionalTokens::new(conditional_tokens_addr, provider.clone());

    let condition_hex = pos
        .condition_id
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X");
    let condition_id: [u8; 32] = hex::decode(condition_hex)
        .map_err(|e| ClaimerError::Internal(format!("Invalid condition ID: {}", e)))?
        .try_into()
        .map_err(|_| ClaimerError::Internal("Condition ID wrong length".into()))?;

    let pending = if pos.neg_risk {
        let contract = INegRiskAdapter::new(neg_risk_adapter_addr, provider.clone());
        let amounts = pos
            .claim_amounts
            .iter()
            .map(|amount| crate::decimal_to_token_units(*amount).map(U256::from))
            .collect::<Result<Vec<_>, _>>()?;
        info!(
            "Calling NegRiskAdapter.redeemPositions for condition {} (amounts={:?})...",
            &condition_hex.chars().take(16).collect::<String>(),
            amounts
        );
        contract
            .redeemPositions(condition_id.into(), amounts)
            .send()
            .await
            .map_err(|e| ClaimerError::Contract(format!("NegRisk redeem tx failed: {}", e)))?
    } else {
        let parent_collection_id = [0u8; 32];
        let index_sets = vec![U256::from(1), U256::from(2)];

        info!(
            "Calling ConditionalTokens.redeemPositions for condition {} (neg_risk={})...",
            &condition_hex.chars().take(16).collect::<String>(),
            pos.neg_risk
        );

        contract
            .redeemPositions(
                collateral_addr,
                parent_collection_id.into(),
                condition_id.into(),
                index_sets,
            )
            .send()
            .await
            .map_err(|e| ClaimerError::Contract(format!("Redeem tx failed: {}", e)))?
    };

    let receipt = pending
        .get_receipt()
        .await
        .map_err(|e| ClaimerError::Contract(format!("Tx confirmation failed: {}", e)))?;

    let tx_hash = format!("{:?}", receipt.transaction_hash);
    info!("Redeem successful! Tx: {}", tx_hash);

    Ok(tx_hash)
}

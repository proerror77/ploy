use std::sync::Arc;

use crate::adapters::{KalshiClient, PolymarketClient};
use crate::config::AppConfig;
use crate::error::{PloyError, Result};
use crate::signing::Wallet;

use super::{parse_exchange_kind, ExchangeClient, ExchangeKind};

fn kalshi_experimental_enabled() -> bool {
    std::env::var("PLOY_ENABLE_KALSHI_EXPERIMENTAL")
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

/// Create the runtime exchange client from `AppConfig`.
///
/// Defaults to Polymarket if config value is invalid/missing upstream validation.
pub async fn build_exchange_client(
    app_config: &AppConfig,
    dry_run: bool,
) -> Result<Arc<dyn ExchangeClient>> {
    let exchange =
        parse_exchange_kind(&app_config.execution.exchange).unwrap_or(ExchangeKind::Polymarket);

    build_exchange_client_for(exchange, app_config, dry_run).await
}

/// Create exchange client for an explicit exchange kind.
pub async fn build_exchange_client_for(
    exchange: ExchangeKind,
    app_config: &AppConfig,
    dry_run: bool,
) -> Result<Arc<dyn ExchangeClient>> {
    match exchange {
        ExchangeKind::Polymarket => {
            let rest_url = app_config
                .market
                .exchange_rest_url
                .as_deref()
                .unwrap_or(&app_config.market.rest_url);

            if dry_run {
                let client = PolymarketClient::new(rest_url, true)?;
                Ok(Arc::new(client))
            } else {
                let wallet = Wallet::from_env(crate::adapters::polymarket_clob::POLYGON_CHAIN_ID)?;
                let funder = std::env::var("POLYMARKET_FUNDER").ok();
                if let Some(funder_addr) = funder {
                    let client = PolymarketClient::new_authenticated_proxy(
                        rest_url,
                        wallet,
                        &funder_addr,
                        true,
                    )
                    .await?;
                    Ok(Arc::new(client))
                } else {
                    let client =
                        PolymarketClient::new_authenticated(rest_url, wallet, true).await?;
                    Ok(Arc::new(client))
                }
            }
        }
        ExchangeKind::Kalshi => {
            if !kalshi_experimental_enabled() {
                return Err(PloyError::Validation(
                    "Kalshi exchange is temporarily disabled. Set PLOY_ENABLE_KALSHI_EXPERIMENTAL=true to enable."
                        .to_string(),
                ));
            }

            let base_url = app_config
                .market
                .exchange_rest_url
                .as_deref()
                .unwrap_or(&app_config.kalshi.base_url);

            let mut api_key = app_config.kalshi.api_key.clone();
            let mut api_secret = app_config.kalshi.api_secret.clone();
            if api_key.is_none() {
                api_key = std::env::var("KALSHI_API_KEY")
                    .ok()
                    .or_else(|| std::env::var("KALSHI_ACCESS_KEY").ok());
            }
            if api_secret.is_none() {
                api_secret = std::env::var("KALSHI_API_SECRET")
                    .ok()
                    .or_else(|| std::env::var("KALSHI_ACCESS_SECRET").ok());
            }

            let client = KalshiClient::new(Some(base_url), api_key, api_secret, dry_run)?;
            Ok(Arc::new(client))
        }
    }
}

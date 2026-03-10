use super::*;

#[path = "crypto_runtime_support/market_data_runtime.rs"]
mod market_data_runtime;
#[path = "crypto_runtime_support/market_discovery.rs"]
mod market_discovery;
#[path = "crypto_runtime_support/preflight.rs"]
mod preflight;

use self::market_data_runtime::initialize_crypto_market_data_runtime;
use self::market_discovery::initialize_crypto_market_discovery;
use self::preflight::{initialize_crypto_runtime_preflight, CryptoRuntimePreflight};

#[derive(Default)]
pub(super) struct CryptoRuntimeSupport {
    pub(super) managed_runtime_data_plane: Option<Arc<PlatformDataPlane>>,
    pub(super) shared_crypto_data_plane: Option<Arc<PlatformDataPlane>>,
}

pub(super) async fn initialize_crypto_runtime_support(
    config: &PlatformBootstrapConfig,
    app_config: &AppConfig,
    runtime_crypto_targets: &strategy_deployments::RuntimeCryptoStrategyTargets,
    shared_pool: Option<&PgPool>,
    pm_client: Option<&PolymarketClient>,
    freshness: &Arc<crate::platform::DataPlaneFreshness>,
) -> Result<CryptoRuntimeSupport> {
    let pm_client_ref = pm_client.ok_or_else(|| {
        crate::error::PloyError::Validation(
            "crypto domain requires a Polymarket client, but none was initialized".to_string(),
        )
    })?;
    let CryptoRuntimePreflight {
        use_data_plane,
        crypto_cfg,
        all_coins,
        data_plane,
        binance_ws,
        pm_ws,
        lob_agent_enabled,
        rl_agent_enabled,
    } = initialize_crypto_runtime_preflight(config, app_config, runtime_crypto_targets, freshness)
        .await?;

    let event_matcher = initialize_crypto_market_discovery(
        shared_pool,
        pm_client_ref,
        &crypto_cfg,
        &all_coins,
        use_data_plane,
        pm_ws.clone(),
    )
    .await;

    initialize_crypto_market_data_runtime(
        shared_pool,
        freshness,
        use_data_plane,
        data_plane.as_ref(),
        binance_ws.clone(),
        pm_ws.clone(),
        event_matcher.clone(),
        &crypto_cfg,
        &all_coins,
        lob_agent_enabled,
        rl_agent_enabled,
    )
    .await;

    Ok(CryptoRuntimeSupport {
        managed_runtime_data_plane: data_plane.clone(),
        shared_crypto_data_plane: data_plane,
    })
}

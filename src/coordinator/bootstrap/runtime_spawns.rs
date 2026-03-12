use super::*;
use crate::agent_runtime::AgentRiskParams;
use crate::agents::openclaw::OpenClawAgent;
use crate::agents::{GovernanceAgent, GovernanceContext};

#[derive(Debug, Clone)]
pub(super) struct ManagedStrategyRuntimeSpawn {
    pub(super) strategy_label: &'static str,
    pub(super) agent_id: String,
    pub(super) domain: Domain,
    pub(super) risk_params: AgentRiskParams,
    pub(super) strategy_config_toml: String,
}

pub(super) async fn spawn_managed_strategy_runtime_task(
    spec: ManagedStrategyRuntimeSpawn,
    coordinator: &mut Coordinator,
    handle: &CoordinatorHandle,
    shutdown_tx: &broadcast::Sender<()>,
    agent_handles: &mut Vec<tokio::task::JoinHandle<()>>,
    dry_run: bool,
    pm_client: Option<&PolymarketClient>,
    pm_ws_url: &str,
    data_plane: Option<Arc<PlatformDataPlane>>,
    observability_pool: Option<PgPool>,
    observability_account_id: &str,
) -> bool {
    let Some(strategy_pm_client) = pm_client.cloned() else {
        warn!(
            agent = %spec.agent_id,
            strategy = spec.strategy_label,
            "managed strategy runtime requested but pm client not configured; skipping"
        );
        return false;
    };

    let strategy_label = spec.strategy_label;
    let agent_id = spec.agent_id;
    let strategy_cmd_rx = coordinator
        .register_agent(agent_id.clone(), spec.domain, spec.risk_params)
        .await;
    let strategy_order_updates_rx = coordinator.register_order_updates(agent_id.clone()).await;
    let strategy_shutdown_rx = shutdown_tx.subscribe();
    let strategy_ws_url = pm_ws_url.to_string();
    let strategy_data_plane = data_plane;
    let strategy_observability_pool = observability_pool;
    let strategy_account_id = observability_account_id.to_string();
    let strategy_config_toml = spec.strategy_config_toml;
    let runtime_agent_id = agent_id.clone();
    let strategy_handle = handle.clone();

    let jh = tokio::spawn(async move {
        if let Err(e) = run_managed_strategy_runtime(
            strategy_label,
            &runtime_agent_id,
            spec.domain,
            strategy_config_toml,
            dry_run,
            strategy_pm_client,
            strategy_ws_url,
            strategy_data_plane,
            strategy_observability_pool,
            strategy_account_id,
            strategy_handle,
            strategy_cmd_rx,
            strategy_order_updates_rx,
            strategy_shutdown_rx,
        )
        .await
        {
            error!(
                agent = strategy_label,
                runtime_agent_id = %runtime_agent_id,
                error = %e,
                "managed strategy runtime exited with error"
            );
        }
    });
    agent_handles.push(jh);
    info!(
        agent = %agent_id,
        strategy = strategy_label,
        "managed strategy runtime spawned"
    );
    true
}

pub(super) async fn spawn_openclaw_governance_agent(
    config: &PlatformBootstrapConfig,
    freshness: &Arc<crate::platform::DataPlaneFreshness>,
    coordinator: &mut Coordinator,
    handle: &CoordinatorHandle,
    agent_handles: &mut Vec<tokio::task::JoinHandle<()>>,
) {
    let openclaw_enabled = env_bool(
        "PLOY_OPENCLAW__ENABLED",
        config.enable_openclaw || config.openclaw.enabled,
    );
    if !openclaw_enabled {
        return;
    }

    let oc_symbols = vec![config.openclaw.btc_symbol.clone()];
    let oc_binance_ws = Arc::new(BinanceWebSocket::new(oc_symbols));
    oc_binance_ws.set_freshness(Arc::clone(freshness));

    let oc_ws = oc_binance_ws.clone();
    tokio::spawn(async move {
        if let Err(e) = oc_ws.run().await {
            tracing::error!(error = %e, "openclaw binance ws exited");
        }
    });

    let oc_risk_params = AgentRiskParams {
        max_order_value: Decimal::ZERO,
        max_total_exposure: Decimal::ZERO,
        max_unhedged_positions: 0,
        max_daily_loss: Decimal::ZERO,
        allow_overnight: false,
        allowed_markets: vec![],
    };
    let oc_agent_id = config.openclaw.agent_id.clone();
    let cmd_rx = coordinator
        .register_agent(oc_agent_id.clone(), Domain::Custom(0), oc_risk_params)
        .await;

    let oc_market_data = BinanceDataPlaneHandle::new(oc_binance_ws);
    let agent = OpenClawAgent::new(config.openclaw.clone(), oc_market_data);
    let ctx = GovernanceContext::new(
        oc_agent_id.clone(),
        Domain::Custom(0),
        handle.clone(),
        cmd_rx,
    );

    let jh = tokio::spawn(async move {
        if let Err(e) = agent.run(ctx).await {
            tracing::error!(agent = "openclaw", error = %e, "openclaw meta-agent exited with error");
        }
    });
    agent_handles.push(jh);
    info!(
        agent_id = %oc_agent_id,
        regime_tick = config.openclaw.regime_tick_secs,
        "openclaw meta-agent spawned"
    );
}

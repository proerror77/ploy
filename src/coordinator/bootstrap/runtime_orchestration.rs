use super::*;

fn build_runtime_health_state(
    app_config: &AppConfig,
    freshness: &Arc<crate::data_plane::DataPlaneFreshness>,
    db_connected: bool,
    polymarket_ws: Option<&Arc<PolymarketWebSocket>>,
) -> Option<Arc<crate::services::HealthState>> {
    let _port = app_config.health_port?;
    let state = Arc::new(
        crate::services::HealthState::new()
            .with_metrics(Arc::new(crate::services::Metrics::new()))
            .with_freshness(Arc::clone(freshness)),
    );
    state.set_db_connected(db_connected);
    if let Some(ws) = polymarket_ws {
        ws.set_health_state(Arc::clone(&state));
    }
    Some(state)
}

fn spawn_runtime_health_server(port: u16, state: Arc<crate::services::HealthState>) {
    tokio::spawn(async move {
        if let Err(error) = crate::services::HealthServer::new(state, port).run().await {
            warn!(port, error = %error, "runtime health server exited");
        }
    });
}

pub(super) async fn run_platform_runtime(
    config: &PlatformBootstrapConfig,
    app_config: &AppConfig,
    control: &PlatformStartControl,
    mut coordinator: Coordinator,
    handle: CoordinatorHandle,
    pm_client: Option<PolymarketClient>,
    shared_pool: Option<PgPool>,
    account_id: String,
    runtime_crypto_targets: &strategy_deployments::RuntimeCryptoStrategyTargets,
) -> Result<()> {
    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    if let Some(pool) = shared_pool.as_ref() {
        let mut collector_domains: Vec<&'static str> = Vec::new();
        if config.enable_crypto {
            collector_domains.push("CRYPTO");
        }
        if config.enable_sports {
            collector_domains.push("SPORTS_NBA");
        }

        if !collector_domains.is_empty() {
            if let Some(client) = pm_client.clone() {
                spawn_pm_token_settlement_persistence(
                    client,
                    pool.clone(),
                    format!("settlements:{}", account_id),
                    collector_domains,
                );
            } else {
                warn!(
                    account_id = %account_id,
                    "pm client not configured; skipping token settlement persistence task"
                );
            }
        }
    }

    let mut agent_handles = Vec::new();
    let freshness = Arc::new(crate::data_plane::DataPlaneFreshness::new());
    let mut managed_runtime_data_plane: Option<Arc<PlatformDataPlane>> = None;
    let mut shared_crypto_data_plane: Option<Arc<PlatformDataPlane>> = None;
    let mut runtime_polymarket_ws: Option<Arc<PolymarketWebSocket>> = None;

    if config.enable_crypto {
        let runtime_support = initialize_crypto_runtime_support(
            config,
            app_config,
            runtime_crypto_targets,
            shared_pool.as_ref(),
            pm_client.as_ref(),
            &freshness,
        )
        .await?;
        managed_runtime_data_plane = runtime_support.managed_runtime_data_plane;
        shared_crypto_data_plane = runtime_support.shared_crypto_data_plane;
        runtime_polymarket_ws = runtime_support.polymarket_ws;
    }

    if let Some(state) = build_runtime_health_state(
        app_config,
        &freshness,
        shared_pool.is_some(),
        runtime_polymarket_ws.as_ref(),
    ) {
        if let Some(port) = app_config.health_port {
            spawn_runtime_health_server(port, state);
        }
    }

    if config.enable_sports {
        prepare_sports_runtime_support(config, app_config, shared_pool.as_ref(), &freshness)
            .await?;
    }

    let managed_runtime_plans =
        collect_managed_strategy_runtime_plans(config, app_config, runtime_crypto_targets);
    for plan in managed_runtime_plans {
        if matches!(
            plan.bootstrap_step,
            ManagedRuntimeBootstrapStep::EnsurePatternMemoryTable
        ) {
            if let Some(ref pool) = shared_pool {
                if let Err(e) =
                    crate::strategy::pattern_memory::persistence::ensure_table(pool).await
                {
                    warn!(error = %e, "failed to create pattern_memory_samples table");
                }
            }
        }

        let runtime_data_plane = match plan.data_plane {
            ManagedRuntimeDataPlaneKind::ManagedCrypto => managed_runtime_data_plane.clone(),
            ManagedRuntimeDataPlaneKind::SharedCrypto => shared_crypto_data_plane.clone(),
            ManagedRuntimeDataPlaneKind::None => None,
        };
        let _ = spawn_managed_strategy_runtime_task(
            plan.spawn,
            &mut coordinator,
            &handle,
            &shutdown_tx,
            &mut agent_handles,
            config.dry_run,
            pm_client.as_ref(),
            &app_config.market.ws_url,
            runtime_data_plane,
            shared_pool.clone(),
            &account_id,
        )
        .await;
    }

    spawn_openclaw_governance_agent(
        config,
        &freshness,
        &mut coordinator,
        &handle,
        &mut agent_handles,
    )
    .await;

    #[cfg(feature = "claimer_daemon")]
    if !config.dry_run && pm_client.is_some() {
        if let Err(e) = crate::strategy::ensure_account_claimer_daemon().await {
            warn!(error = %e, "failed to ensure account-level auto-claimer daemon");
        } else {
            info!("auto-claimer background task ensured (account-level)");
        }
    }

    #[cfg(not(feature = "claimer_daemon"))]
    if pm_client.is_some() {
        info!("claimer feature disabled; skipping auto-claimer background task");
    }

    info!(
        agents = agent_handles.len(),
        "all agents spawned, starting coordinator"
    );

    if let Some(agent_id) = control.pause.as_deref() {
        if agent_id == "all" {
            coordinator.pause_all().await;
        } else if let Err(e) = coordinator
            .send_command(agent_id, crate::coordinator::CoordinatorCommand::Pause)
            .await
        {
            warn!(agent_id, error = %e, "failed to pause agent at startup");
        }
    } else if let Some(agent_id) = control.resume.as_deref() {
        if agent_id == "all" {
            coordinator.resume_all().await;
        } else if let Err(e) = coordinator
            .send_command(agent_id, crate::coordinator::CoordinatorCommand::Resume)
            .await
        {
            warn!(agent_id, error = %e, "failed to resume agent at startup");
        }
    }

    let shutdown_rx = shutdown_tx.subscribe();
    let stx = shutdown_tx.clone();
    tokio::spawn(async move {
        if let Ok(()) = tokio::signal::ctrl_c().await {
            info!("Ctrl+C received, initiating shutdown");
            let _ = stx.send(());
        }
    });

    coordinator.run(shutdown_rx).await;

    info!("waiting for agents to finish...");
    let timeout = tokio::time::Duration::from_secs(10);
    for jh in agent_handles {
        let _ = tokio::time::timeout(timeout, jh).await;
    }

    info!("platform shutdown complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn build_runtime_health_state_attaches_freshness_and_ws() {
        let app_config = AppConfig::default_config(true, "test-market");
        let freshness = Arc::new(crate::data_plane::DataPlaneFreshness::new());
        let pm_ws = Arc::new(PolymarketWebSocket::new("wss://example.invalid"));

        let state = build_runtime_health_state(&app_config, &freshness, true, Some(&pm_ws))
            .expect("health state should be enabled when health_port is set");

        assert!(
            state.freshness.is_some(),
            "freshness tracker should be attached"
        );
        assert!(
            state.metrics.is_some(),
            "metrics collector should be attached"
        );
        assert!(
            state.db_connected.load(Ordering::SeqCst),
            "shared db should mark db connectivity"
        );
        assert!(
            pm_ws.health_state().is_some(),
            "polymarket ws should be wired to health state"
        );
    }

    #[test]
    fn build_runtime_health_state_respects_disabled_port() {
        let mut app_config = AppConfig::default_config(true, "test-market");
        app_config.health_port = None;
        let freshness = Arc::new(crate::data_plane::DataPlaneFreshness::new());
        let pm_ws = Arc::new(PolymarketWebSocket::new("wss://example.invalid"));

        let state = build_runtime_health_state(&app_config, &freshness, false, Some(&pm_ws));

        assert!(
            state.is_none(),
            "health state should be disabled without a port"
        );
        assert!(
            pm_ws.health_state().is_none(),
            "ws should not receive health wiring when health server is disabled"
        );
    }
}

use super::*;

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
    let freshness = Arc::new(crate::platform::DataPlaneFreshness::new());
    let mut managed_runtime_data_plane: Option<Arc<PlatformDataPlane>> = None;
    let mut shared_crypto_data_plane: Option<Arc<PlatformDataPlane>> = None;

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
